//! Lays the disc's files out the way the game installs them: `System/`, `Maps/`, `Textures/`,
//! `Sounds/`, `Music/`, plus `Extra/` with the dialog of the disc's other languages.
//!
//! The cabinet keeps one file group per part of the game (`Component_2` the maps, ...) and one
//! per language besides English. Groups are told apart by a file each is known to hold rather
//! than by their number, which is not the same on every disc.

use std::path::PathBuf;
use std::sync::Arc;

use crate::cab::{Cabinet, FileEntry, FileGroup};
use crate::iso9660::Iso;
use crate::source::{Image, Slice, Source};

/// What the disc offers: `int` (English) and the extension of every other language.
pub struct Disc {
    image: Arc<Image>,
    cabinet: Cabinet,
    volumes: Vec<Slice>,
    hgame: Slice,
    pub languages: Vec<String>,
}

/// Data only: the original program files are never installed.
const SKIPPED: &[&str] = &["dll", "exe", "sys", "ex_", "scc", "dsp", "log"];

impl Disc {
    pub fn open(source: Arc<dyn Source>) -> Result<Self, String> {
        let image = Arc::new(Image::open(source)?);
        let iso = Iso::open(&*image)?;
        let slice = |path: &str| -> Result<Slice, String> {
            let e = iso.find(path)?;
            Ok(Slice { source: image.clone(), offset: e.offset, len: e.len })
        };
        let hdr = slice("setup/data1.hdr")?;
        let cabinet = Cabinet::parse(&hdr.read_vec(0, hdr.len as usize)?)?;
        let count = cabinet.files.iter().map(|f| f.volume).max().unwrap_or(0);
        let volumes = (1..=count).map(|n| slice(&format!("setup/data{n}.cab"))).collect::<Result<Vec<_>, _>>()?;
        for (i, v) in volumes.iter().enumerate() {
            cabinet.check_volume(i as u16 + 1, v)?;
        }
        let hgame = slice("setup/hgame.u")?;
        let mut languages = vec!["int".to_string()];
        for f in &cabinet.files {
            if let Some(ext) = f.name.strip_prefix("HpMenu.") {
                languages.push(ext.to_ascii_lowercase());
            }
        }
        Ok(Self { image, cabinet, volumes, hgame, languages })
    }

    pub fn is_raw(&self) -> bool {
        self.image.is_raw()
    }

    /// The language installed when none is asked for: English if the disc has its dialog.
    pub fn default_language(&self) -> Option<&str> {
        self.cabinet.files.iter().any(|f| f.has_data() && f.name.eq_ignore_ascii_case("AllDialog.uax")).then_some("int")
    }

    fn group_of(&self, file: &str) -> Result<&FileGroup, String> {
        // At the top of its group: the system files also carry a `CUTSCENES` folder with copies
        // of the cutscene scripts, whose own group holds them at its top.
        let f = self
            .cabinet
            .files
            .iter()
            .find(|f| f.has_data() && f.directory.is_empty() && f.name.eq_ignore_ascii_case(file))
            .ok_or_else(|| format!("{file} is not in the cabinet"))?;
        self.cabinet
            .groups
            .iter()
            .find(|g| g.first <= f.index && f.index <= g.last)
            .ok_or_else(|| format!("{file} is in no file group"))
    }

    fn files_of(&self, group: &FileGroup) -> impl Iterator<Item = &FileEntry> {
        self.cabinet.files[group.first as usize..=group.last as usize].iter().filter(|f| f.has_data())
    }

    fn extract(&self, f: &FileEntry) -> Result<Vec<u8>, String> {
        let volume = self.volumes.get(f.volume as usize - 1).ok_or_else(|| format!("{}: in volume {}, which the disc lacks", f.name, f.volume))?;
        self.cabinet.extract(f, volume)
    }

    /// What an install in `language` is made of: each file's path in it and the cabinet file
    /// it comes from (or `None` for `HGame.u`, which sits on the disc outside the cabinet).
    /// Later entries replace earlier ones with the same path, as copying over them would.
    pub fn plan(&self, language: &str) -> Result<Vec<(PathBuf, Option<&FileEntry>)>, String> {
        let language = language.to_ascii_lowercase();
        if !self.languages.contains(&language) {
            return Err(format!("language '{language}' is not on this disc; it has {}", self.languages.join(", ")));
        }
        let dialog = match language.as_str() {
            "int" => "AllDialog.uax".to_string(),
            l => format!("AllDialog.{}_uax", l.to_ascii_uppercase()),
        };
        let mut out: Vec<(PathBuf, Option<&FileEntry>)> = Vec::new();
        let mut copy = |group: &FileGroup, into: &str| {
            for f in self.files_of(group) {
                out.push((PathBuf::from(into).join(f.directory.replace('\\', "/")).join(&f.name), Some(f)));
            }
        };
        copy(self.group_of("Core.u")?, "System");
        copy(self.group_of("00001PrivetIntro.int")?, "System/CUTSCENES");
        if language != "int" {
            copy(self.group_of(&format!("HpMenu.{language}"))?, "System");
        }
        copy(self.group_of("Adv1Willow.unr")?, "Maps");
        copy(self.group_of("Adv4Greenhouse_Music.ogg")?, "Music");
        copy(self.group_of("HP2_Menu.utx")?, "Textures");
        let dialog_file = self.files_of(self.group_of(&dialog)?).find(|f| f.name.eq_ignore_ascii_case(&dialog)).ok_or("dialog missing")?;
        out.push((PathBuf::from("Sounds/AllDialog.uax"), Some(dialog_file)));
        // The other languages' dialog, kept to validate the parser.
        for f in self.cabinet.files.iter().filter(|f| f.has_data() && f.name.starts_with("AllDialog.") && f.name.ends_with("_uax")) {
            if f.index != dialog_file.index {
                out.push((PathBuf::from("Extra").join(&f.name), Some(f)));
            }
        }
        // The installer runs on a case-insensitive filesystem: a language's default.ini
        // (Language=<lang>) replaces Default.ini.
        for (path, _) in &mut out {
            if path.as_os_str().eq_ignore_ascii_case("System/default.ini") {
                *path = PathBuf::from("System/Default.ini");
            }
        }
        out.push((PathBuf::from("System/HGame.u"), None));
        out.retain(|(p, _)| !p.extension().is_some_and(|e| SKIPPED.iter().any(|s| e.eq_ignore_ascii_case(s))));
        let mut seen = std::collections::HashSet::new();
        let mut kept: Vec<_> = out.into_iter().rev().filter(|(p, _)| seen.insert(p.clone())).collect();
        kept.reverse();
        Ok(kept)
    }

    /// Installs `language`, handing over each file as it is unpacked. `wanted` can leave files
    /// out by their path, which is what a browser does with the other languages' dialog.
    pub fn install(
        &self,
        language: &str,
        wanted: &dyn Fn(&std::path::Path) -> bool,
        put: &mut dyn FnMut(&std::path::Path, Vec<u8>, usize, usize) -> Result<(), String>,
    ) -> Result<(), String> {
        let plan: Vec<_> = self.plan(language)?.into_iter().filter(|(p, _)| wanted(p)).collect();
        let total = plan.len();
        for (i, (path, file)) in plan.into_iter().enumerate() {
            let data = match file {
                Some(f) => self.extract(f)?,
                None => self.hgame.read_vec(0, self.hgame.len as usize)?,
            };
            put(&path, data, i + 1, total)?;
        }
        Ok(())
    }
}
