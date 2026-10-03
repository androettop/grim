//! The packages of an installation, loaded on demand, with cross-package import resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use grim_fs::{FileSystem, NativeFs};

use crate::package::Package;
use crate::tables::ObjectRef;

/// Extensions searched through `Paths=` in `Default.ini`. Language variants (`.spa_utx`, ...)
/// are not resolvable by name, only openable by path.
pub const SEARCH_EXTENSIONS: &[&str] = &["u", "unr", "utx", "uax", "umx"];

pub struct Library {
    fs: Arc<dyn FileSystem>,
    root: PathBuf,
    by_name: HashMap<String, PathBuf>,
    loaded: Mutex<HashMap<PathBuf, Arc<Package>>>,
}

#[derive(Clone)]
pub struct ObjectHandle {
    pub package: Arc<Package>,
    pub export: u32,
}

impl Library {
    pub fn open(root: impl AsRef<Path>) -> Self {
        Self::open_in(Arc::new(NativeFs), root)
    }

    /// The packages under `root` in a given file system.
    pub fn open_in(fs: Arc<dyn FileSystem>, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let mut by_name = HashMap::new();
        for p in fs.files_under(&root) {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase();
            if SEARCH_EXTENSIONS.contains(&ext.as_str()) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    by_name.entry(stem.to_ascii_lowercase()).or_insert(p);
                }
            }
        }
        Self { fs, root, by_name, loaded: Mutex::new(HashMap::new()) }
    }

    pub fn fs(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_path(&self, path: &Path) -> Result<Arc<Package>, String> {
        if let Some(p) = self.loaded.lock().unwrap().get(path) {
            return Ok(p.clone());
        }
        let data = self.fs.read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let pkg = Package::parse(Package::name_of(path), data).map_err(|e| format!("{}: {e}", path.display()))?;
        let pkg = Arc::new(pkg);
        self.loaded.lock().unwrap().insert(path.to_path_buf(), pkg.clone());
        Ok(pkg)
    }

    pub fn path_of(&self, name: &str) -> Option<&Path> {
        self.by_name.get(&name.to_ascii_lowercase()).map(PathBuf::as_path)
    }

    pub fn load(&self, name: &str) -> Result<Arc<Package>, String> {
        let path = self
            .by_name
            .get(&name.to_ascii_lowercase())
            .ok_or_else(|| format!("package '{name}' not found"))?
            .clone();
        self.load_path(&path)
    }

    /// Resolves a reference made from `pkg` to a concrete export, in `pkg` or another package.
    pub fn resolve(&self, pkg: &Arc<Package>, r: ObjectRef) -> Result<ObjectHandle, String> {
        match r {
            ObjectRef::Null => Err("null reference".into()),
            ObjectRef::Export(i) => Ok(ObjectHandle { package: pkg.clone(), export: i }),
            ObjectRef::Import(i) => {
                // [object, outer, ..., root package]
                let mut chain = vec![r];
                let mut cur = pkg.outer(r);
                while !cur.is_null() {
                    chain.push(cur);
                    cur = pkg.outer(cur);
                }
                let root_name = pkg.object_name(*chain.last().unwrap()).to_string();
                if chain.len() == 1 {
                    return Err(format!("import {} is a package, not an object", root_name));
                }
                let target = self.load(&root_name)?;
                let class = pkg.name(pkg.import(i).class_name).to_string();
                let names: Vec<&str> = chain[..chain.len() - 1].iter().map(|&c| pkg.object_name(c)).collect();
                for e in 0..target.exports.len() as u32 {
                    let er = ObjectRef::Export(e);
                    if !target.object_name(er).eq_ignore_ascii_case(names[0])
                        || !target.class_name(er).eq_ignore_ascii_case(&class)
                    {
                        continue;
                    }
                    let mut o = target.outer(er);
                    let mut ok = true;
                    for n in &names[1..] {
                        if o.is_null() || !target.object_name(o).eq_ignore_ascii_case(n) {
                            ok = false;
                            break;
                        }
                        o = target.outer(o);
                    }
                    if ok && o.is_null() {
                        return Ok(ObjectHandle { package: target, export: e });
                    }
                }
                Err(format!("{} not found in {root_name}", pkg.path_name(r)))
            }
        }
    }
}
