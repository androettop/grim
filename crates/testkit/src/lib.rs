//! Test and diagnostics helpers; the engine never depends on this crate.
//!
//! Validation runs against the real game files, unpacked by `grim extract` into
//! `game/HP2` (gitignored) or pointed to by `GRIM_GAME_DIR`.

use std::path::{Path, PathBuf};

pub const GAME_DIR_ENV: &str = "GRIM_GAME_DIR";
const PACKAGE_MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];

/// Game install root (the directory containing `System/`, `Maps/`, ...).
pub fn game_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(GAME_DIR_ENV) {
        return Some(PathBuf::from(p));
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game/HP2");
    workspace.is_dir().then(|| workspace.canonicalize().unwrap_or(workspace))
}

/// Panics when the assets are missing: validation tests never skip silently.
pub fn require_game_dir() -> PathBuf {
    game_dir().unwrap_or_else(|| {
        panic!(
            "game assets not found: extract them with \
             `cargo run --release -p grim-cli -- extract <disc image>` or set {GAME_DIR_ENV}"
        )
    })
}

/// Every Unreal package under `root`, detected by magic (not extension), sorted.
pub fn package_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.retain(|p| is_package(p));
    out.sort();
    out
}

/// Names of the map packages under `root` (the `.unr` files), sorted.
pub fn map_names(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = package_files(root)
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("unr")))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    out.sort();
    out
}

pub fn is_package(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|_| magic == PACKAGE_MAGIC)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

pub fn display_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}
