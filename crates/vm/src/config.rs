//! INI configuration, localization files and text import of property values.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use grim_object::{PropDef, PropType, Value};

use crate::vm::Vm;

/// Config property (value from `[Package.Class]` in the ini).
pub const CPF_CONFIG: u32 = 0x0000_4000;
/// Localized property (value from `[Class]` in the package's language file).
pub const CPF_LOCALIZED: u32 = 0x0000_8000;
pub const CPF_GLOBAL_CONFIG: u32 = 0x0004_0000;

/// What a key does, as the game's bindings put it.
#[derive(Debug, Clone, PartialEq)]
pub enum Binding {
    /// Moves an axis while the key is held, at that speed.
    Axis(String, f32),
    /// Holds a boolean down while the key is.
    Button(String),
    /// Calls an `exec` function of the player when the key goes down: `Jump`, `AltFire`.
    Command(String),
}

/// An INI file: case-insensitive sections and keys; a key may repeat.
#[derive(Default, Debug, Clone)]
pub struct Ini {
    sections: HashMap<String, Vec<(String, String)>>,
}

impl Ini {
    pub fn parse(bytes: &[u8]) -> Self {
        let text: String = if bytes.starts_with(&[0xFF, 0xFE]) {
            let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        } else {
            bytes.iter().map(|&b| b as char).collect()
        };
        let mut ini = Ini::default();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_ascii_lowercase();
                ini.sections.entry(section.clone()).or_default();
            } else if let Some((k, v)) = line.split_once('=') {
                ini.sections.entry(section.clone()).or_default().push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        ini
    }

    pub fn load(path: &Path) -> Option<Self> {
        std::fs::read(path).ok().map(|b| Self::parse(&b))
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let key = key.to_ascii_lowercase();
        self.sections.get(&section.to_ascii_lowercase())?.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    /// Sets a key, adding the section or the key if the file did not have them. This is what
    /// the game's own `set ini:` console command writes into.
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let entries = self.sections.entry(section.to_ascii_lowercase()).or_default();
        let key = key.to_ascii_lowercase();
        match entries.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => *v = value.to_string(),
            None => entries.push((key, value.to_string())),
        }
    }

    /// Every value stored under a key, for the ones a file repeats or numbers: `Aliases[25]`
    /// is one of the `Aliases` an ini holds.
    pub fn all(&self, section: &str, key: &str) -> Vec<&str> {
        let key = key.to_ascii_lowercase();
        let matches = |k: &String| *k == key || k.starts_with(&format!("{key}["));
        match self.sections.get(&section.to_ascii_lowercase()) {
            Some(entries) => entries.iter().filter(|(k, _)| matches(k)).map(|(_, v)| v.as_str()).collect(),
            None => Vec::new(),
        }
    }

    /// Keys of a section, in the order the file lists them.
    pub fn keys(&self, section: &str) -> Vec<(&str, &str)> {
        match self.sections.get(&section.to_ascii_lowercase()) {
            Some(entries) => entries.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
            None => Vec::new(),
        }
    }

    pub fn has_section(&self, section: &str) -> bool {
        self.sections.contains_key(&section.to_ascii_lowercase())
    }
}

/// Finds `dir/name` ignoring case in every component (the game was made for Windows, so
/// `name` may also contain `\` separators).
pub fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in name.split(['\\', '/']).filter(|p| !p.is_empty()) {
        cur = std::fs::read_dir(&cur).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(part))
        })?;
    }
    Some(cur)
}

pub struct Config {
    pub system: PathBuf,
    pub ini: Ini,
    /// The player's own settings, where the key bindings live.
    pub user: Ini,
    pub language: String,
    /// What the player has changed while playing (the options menu writes here through
    /// `set ini:`). Kept apart so it can be written out without touching the game's own files.
    pub changed: Ini,
    loc: HashMap<String, Option<Ini>>,
}

impl Config {
    pub fn load(game_root: &Path) -> Self {
        let system = game_root.join("System");
        let ini = find_file(&system, "Default.ini").and_then(|p| Ini::load(&p)).unwrap_or_default();
        // What the player changed wins over what the game ships with.
        let mut user = find_file(&system, "DefUser.ini").and_then(|p| Ini::load(&p)).unwrap_or_default();
        if let Some(mine) = find_file(&system, "User.ini").and_then(|p| Ini::load(&p)) {
            for (section, entries) in mine.sections.clone() {
                user.sections.insert(section, entries);
            }
        }
        // Settings the player changed in an earlier run. They are kept in a file of this
        // engine's own rather than in the game's, which is read-only as far as we care.
        let mut ini = ini;
        let changed = find_file(&system, "grim.ini").and_then(|p| Ini::load(&p)).unwrap_or_default();
        for (section, entries) in changed.sections.clone() {
            for (key, value) in entries {
                ini.set(&section, &key, &value);
            }
        }
        let language = ini.get("Engine.Engine", "Language").unwrap_or("int").to_ascii_lowercase();
        Self { system, ini, user, language, changed, loc: HashMap::new() }
    }

    /// What a key is bound to, following the aliases the bindings go through. Unreal keeps
    /// them in `[Engine.Input]`: a key names a command or an alias, commands are separated by
    /// `|`, and an alias is `Aliases[n]=(Command="...",Alias=Name)`.
    pub fn bindings(&self, key: &str) -> Vec<Binding> {
        let Some(command) = self.user.get("Engine.Input", key) else { return Vec::new() };
        let mut out = Vec::new();
        for part in command.split('|') {
            self.binding_of(part.trim(), &mut out, &mut Vec::new());
        }
        out
    }

    fn binding_of(&self, command: &str, out: &mut Vec<Binding>, chain: &mut Vec<String>) {
        if chain.len() > 4 || command.is_empty() {
            return;
        }
        let mut words = command.split_whitespace();
        match words.next() {
            Some(word) if word.eq_ignore_ascii_case("Axis") => {
                let Some(name) = words.next() else { return };
                let speed = words
                    .find_map(|w| w.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("Speed")).map(|(_, v)| v))
                    .and_then(|v| v.trim_start_matches('+').parse().ok())
                    .unwrap_or(1.0);
                out.push(Binding::Axis(name.to_string(), speed));
            }
            Some(word) if word.eq_ignore_ascii_case("Button") => {
                if let Some(name) = words.next() {
                    out.push(Binding::Button(name.to_string()));
                }
            }
            // Anything else names an alias, or an `exec` function of the player. An alias that
            // names itself means the function: `Jump` expands to `Jump | Axis aUp | Button ...`,
            // where the inner `Jump` is `PlayerPawn.Jump`, not the alias again.
            Some(word) => {
                if !chain.iter().any(|a| a.eq_ignore_ascii_case(word)) {
                    for entry in self.user.all("Engine.Input", "Aliases") {
                        if !field_of(entry, "Alias").is_some_and(|a| a.eq_ignore_ascii_case(word)) {
                            continue;
                        }
                        let Some(inner) = field_of(entry, "Command") else { continue };
                        chain.push(word.to_string());
                        for part in inner.split('|') {
                            self.binding_of(part.trim(), out, chain);
                        }
                        chain.pop();
                        return;
                    }
                }
                out.push(Binding::Command(word.to_string()));
            }
            None => {}
        }
    }

    /// Language file of a package: `<Package>.<lang>`, falling back to `.int`.
    fn loc_file(&mut self, package: &str) -> Option<&Ini> {
        let key = package.to_ascii_lowercase();
        if !self.loc.contains_key(&key) {
            let file = self
                .find_loc(&format!("{package}.{}", self.language))
                .or_else(|| self.find_loc(&format!("{package}.int")))
                .and_then(|p| Ini::load(&p));
            self.loc.insert(key.clone(), file);
        }
        self.loc.get(&key).and_then(|f| f.as_ref())
    }

    /// A localization file in the system directory, or in one of its folders. The cutscene
    /// scripts are `.int` files of their own kept in `System/CUTSCENES`, and nothing in the ini
    /// names that directory. Hypothesis: the engine looks inside the system directory's folders;
    /// one level down is as deep as this game goes.
    fn find_loc(&self, name: &str) -> Option<PathBuf> {
        if let Some(p) = find_file(&self.system, name) {
            return Some(p);
        }
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&self.system).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        dirs.iter().find_map(|d| find_file(d, name))
    }

    pub fn localize(&mut self, section: &str, key: &str, package: &str) -> Option<String> {
        self.loc_file(package)?.get(section, key).map(|v| unquote(v).to_string())
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s)
}

/// Splits `(A=1,B=(C=2,D=3))` into top-level `key=value` pairs.
fn struct_fields(s: &str) -> Option<Vec<(String, String)>> {
    let inner = s.trim().strip_prefix('(')?.strip_suffix(')')?;
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0, false, 0);
    let b = inner.as_bytes();
    for i in 0..=b.len() {
        let c = if i < b.len() { b[i] } else { b',' };
        match c {
            b'"' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth -= 1,
            b',' if !quoted && depth == 0 => {
                let part = inner[start..i].trim();
                if !part.is_empty() {
                    let (k, v) = part.split_once('=')?;
                    out.push((k.trim().to_string(), v.trim().to_string()));
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    Some(out)
}

impl Vm {
    /// Parses a property value written as text (ini / localization syntax).
    pub fn import_text(&mut self, ty: &PropType, text: &str) -> Option<Value> {
        let t = text.trim();
        Some(match ty {
            PropType::Int => Value::Int(crate::text::parse_int(t)),
            PropType::Float => Value::Float(crate::text::parse_float(t)),
            PropType::Bool => Value::Bool(crate::text::parse_bool(t)),
            PropType::Byte(e) => match t.parse::<u8>() {
                Ok(v) => Value::Byte(v),
                Err(_) => {
                    let e = (*e)?;
                    let n = self.world.names.get(t)?;
                    Value::Byte(self.world.enums[e.0 as usize].values.iter().position(|&v| v == n)? as u8)
                }
            },
            PropType::Name => Value::Name(self.world.names.intern(unquote(t))),
            PropType::Str => Value::Str(unquote(t).to_string()),
            PropType::Object(_) | PropType::Class(_) => {
                if t.eq_ignore_ascii_case("None") {
                    return Some(Value::Object(None));
                }
                // `Class'Package.Name'` or plain `Package.Name`.
                let path = t.split_once('\'').map(|(_, r)| r.trim_end_matches('\'')).unwrap_or(t);
                let (pkg_name, _) = path.split_once('.')?;
                let pkg = self.world.lib.load(pkg_name).ok()?;
                let i = (0..pkg.exports.len() as u32)
                    .find(|&i| pkg.path_name(grim_package::ObjectRef::Export(i)).eq_ignore_ascii_case(path))?;
                Value::Object(Some(grim_object::ObjectKey { package: self.world.pkg_id(&pkg), export: i }))
            }
            PropType::Struct(s) => {
                let def = self.world.structs[s.0 as usize].clone();
                let Value::Struct(mut fields) = self.world.zero(ty) else { return None };
                for (k, v) in struct_fields(t)? {
                    let f = def.fields.iter().find(|f| self.world.names.str(f.name).eq_ignore_ascii_case(&k))?;
                    fields[f.slot as usize] = self.import_text(&f.ty, &v)?;
                }
                Value::Struct(fields)
            }
            PropType::Array(_) => return None,
        })
    }

    /// Overrides class defaults of `config` properties from the ini and of `localized`
    /// properties from the language files, like UE1 does when a class loads.
    pub fn apply_config_defaults(&mut self) {
        for ci in 0..self.world.classes.len() {
            let class = self.world.classes[ci].clone();
            let Some(key) = class.key else { continue };
            if key.package == grim_object::INTRINSIC {
                continue;
            }
            let package = self.world.package(key.package).name.clone();
            let class_name = self.world.names.str(class.name).to_string();
            let mut defaults = class.defaults.clone();
            let mut changed = false;
            let user_settings = self.world.names.str(class.config_name).eq_ignore_ascii_case("User");
            for p in &class.props {
                let (ini_section, from_loc) = if p.flags & (CPF_CONFIG | CPF_GLOBAL_CONFIG) != 0 {
                    (format!("{package}.{class_name}"), false)
                } else if p.flags & CPF_LOCALIZED != 0 {
                    (class_name.clone(), true)
                } else {
                    continue;
                };
                // Inherited props are looked up in the section of the class that declares them.
                let (section, owner_pkg) = self.declaring_section(p, &ini_section, &package, from_loc);
                for i in 0..p.array_dim {
                    let name = self.world.names.str(p.name).to_string();
                    let key = if p.array_dim > 1 { format!("{name}[{i}]") } else { name };
                    let text = if from_loc {
                        self.config.localize(&section, &key, &owner_pkg)
                    } else if user_settings {
                        // A class says which ini its settings live in. `config(User)` means the
                        // player's own, which is where `bMoveWhileCasting` and the rest of the
                        // options the game lets people change are kept.
                        self.config.user.get(&section, &key).map(str::to_string)
                    } else {
                        self.config.ini.get(&section, &key).map(str::to_string)
                    };
                    let Some(text) = text else { continue };
                    match self.import_text(&p.ty, &text) {
                        Some(v) => {
                            defaults[(p.slot + i) as usize] = v;
                            changed = true;
                        }
                        None => self.warn(format!("cannot import '{text}' for {section}.{key}")),
                    }
                }
            }
            if changed {
                self.world.classes[ci].defaults = defaults;
            }
        }
    }

    /// Section (and package, for language files) of a property's declaring class; the value
    /// of a subclass section, when present, takes precedence.
    fn declaring_section(&mut self, p: &PropDef, own_section: &str, own_pkg: &str, from_loc: bool) -> (String, String) {
        let name = self.world.names.str(p.name).to_string();
        let has_own = if from_loc {
            self.config.localize(own_section, &name, own_pkg).is_some()
        } else {
            self.config.ini.get(own_section, &name).is_some()
        };
        if has_own {
            return (own_section.to_string(), own_pkg.to_string());
        }
        // The property object's outer is the declaring class.
        let pkg = self.world.package(p.key.package).clone();
        let owner = pkg.outer(grim_package::ObjectRef::Export(p.key.export));
        let owner_name = pkg.object_name(owner).to_string();
        if from_loc {
            (owner_name, pkg.name.clone())
        } else {
            (format!("{}.{owner_name}", pkg.name), pkg.name.clone())
        }
    }
}

/// One field of an ini struct value, as `(Command="...",Alias=Name)` writes them.
fn field_of(text: &str, name: &str) -> Option<String> {
    let at = text.to_ascii_lowercase().find(&format!("{}=", name.to_ascii_lowercase()))?;
    let rest = text[at + name.len() + 1..].trim_start();
    Some(match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?.to_string(),
        None => rest.split([',', ')']).next()?.trim().to_string(),
    })
}

impl Config {
    /// Remembers a setting the player changed, in memory and on the next `SaveConfig`.
    pub fn change(&mut self, section: &str, key: &str, value: &str) {
        self.ini.set(section, key, value);
        self.changed.set(section, key, value);
    }

    /// Writes what the player changed to `System/grim.ini`, which is read back the next time
    /// the game starts. The game's own `.ini` files are left alone.
    pub fn save_changes(&self) -> std::io::Result<()> {
        if self.changed.sections.is_empty() {
            return Ok(());
        }
        let mut text = String::from("; Settings changed while playing. Written by grim-engine.\n");
        let mut sections: Vec<_> = self.changed.sections.iter().collect();
        sections.sort_by(|a, b| a.0.cmp(b.0));
        for (section, entries) in sections {
            text.push_str(&format!("\n[{section}]\n"));
            for (key, value) in entries {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
        std::fs::write(self.system.join("grim.ini"), text)
    }
}
