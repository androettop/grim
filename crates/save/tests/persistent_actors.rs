//! The `_pa` stream, checked against the original saves and on its own.

use std::path::{Path, PathBuf};

use grim_object::{ObjectKey, World};
use grim_package::{Library, ObjectRef};
use grim_save::{Actor, Level, Prop, Saved};

/// Where the original save slots are: `$GRIM_SAVE_DIR`, or the `Save` directory the game keeps
/// beside its install. Every `Slot*` directory under it is looked into.
fn save_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("GRIM_SAVE_DIR") {
        roots.push(PathBuf::from(dir));
    }
    if let Some(game) = grim_testkit::game_dir() {
        roots.push(game.join("Save"));
        if let Some(parent) = game.parent() {
            roots.push(parent.join("Save"));
        }
    }
    let mut out = Vec::new();
    for root in roots {
        collect(&root, &mut out);
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().ends_with("_pa.usa")) {
            out.push(p);
        }
    }
}

fn linked_world() -> World {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    for f in grim_testkit::package_files(&root.join("System")) {
        let Ok(pkg) = world.lib.load_path(&f) else { continue };
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).unwrap();
            }
        }
    }
    world
}

/// Every original `_pa` file reads whole and writes back byte for byte, which is what our own
/// saves need if the game is to load them.
#[test]
fn the_original_streams_read_and_write_back_unchanged() {
    let files = save_files();
    if files.is_empty() {
        eprintln!("no save slots found; set GRIM_SAVE_DIR to check them");
        return;
    }
    let world = linked_world();
    let mut actors = 0;
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let level = match grim_save::read(&bytes, &world) {
            Ok(l) => l,
            Err(e) => panic!("{}: {e}", f.display()),
        };
        assert!(!level.name.is_empty(), "{}: no level name", f.display());
        assert_eq!(level.unknown, [-1, 0, 0], "{}: unexpected header words", f.display());
        let again = grim_save::write(&level);
        assert_eq!(again.len(), bytes.len(), "{}: {} bytes written for {}", f.display(), again.len(), bytes.len());
        let differs = again.iter().zip(&bytes).position(|(a, b)| a != b);
        assert_eq!(differs, None, "{}: first byte that differs at {:?}", f.display(), differs);
        actors += level.actors.len();
    }
    println!("{} files, {actors} actors", files.len());
}

/// Written and read again with no game files in play: the types that do not need a declaration.
#[test]
fn a_stream_we_write_reads_back_the_same() {
    let level = Level {
        name: "Whatever.unr".to_string(),
        unknown: [-1, 0, 0],
        actors: vec![Actor {
            name: "Thing0".to_string(),
            class: "Thing".to_string(),
            props: vec![
                Prop { name: "bDone".to_string(), index: 0, value: Saved::Bool(true) },
                Prop { name: "bLeft".to_string(), index: 0, value: Saved::Bool(false) },
                Prop { name: "Count".to_string(), index: 0, value: Saved::Int(-7) },
                Prop { name: "Rate".to_string(), index: 0, value: Saved::Float(0.25) },
                Prop { name: "Kind".to_string(), index: 0, value: Saved::Byte(3) },
                Prop { name: "Owner".to_string(), index: 0, value: Saved::Object },
                Prop { name: "Spell".to_string(), index: 2, value: Saved::Class },
                Prop { name: "Tag".to_string(), index: 0, value: Saved::Name("None".to_string()) },
                Prop { name: "Line".to_string(), index: 1, value: Saved::Str { text: "a line".to_string(), unicode: false } },
                Prop { name: "Wide".to_string(), index: 0, value: Saved::Str { text: "a wide line".to_string(), unicode: true } },
                Prop { name: "Empty".to_string(), index: 0, value: Saved::Str { text: String::new(), unicode: false } },
            ],
        }],
    };
    let bytes = grim_save::write(&level);
    let world = World::new(Library::open(Path::new(".")));
    assert_eq!(grim_save::read(&bytes, &world).unwrap(), level);
}
