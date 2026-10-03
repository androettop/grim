//! State flag semantics verified on the game's classes.

use grim_object::{ObjectKey, World};
use grim_package::{Library, ObjectRef};

/// `auto state` flag: no class declares more than one (0x1 is `state()`, editable).
const STATE_AUTO: u32 = 0x2;

#[test]
fn at_most_one_auto_state_per_class() {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    for f in grim_testkit::package_files(&root) {
        let pkg = world.lib.load_path(&f).unwrap();
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).unwrap();
            }
        }
    }
    let mut with_auto = 0;
    for c in &world.classes {
        let n = c.states.values().filter(|&&s| world.state(s).flags & STATE_AUTO != 0).count();
        assert!(n <= 1, "{} has {n} auto states", world.names.str(c.name));
        with_auto += (n == 1) as usize;
    }
    assert!(with_auto > 100);
}
