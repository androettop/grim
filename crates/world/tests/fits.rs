//! Box queries against the BSP: a teleport asks whether an actor has room where it lands, and
//! walking asks how far down the floor is. Both have to agree about where the floor is.

use grim_assets::{load_export, Asset};
use grim_package::{Library, ObjectRef};
use grim_world::Bsp;

fn bsp_of(map: &str) -> Bsp {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let pkg = lib.load_path(&root.join("Maps").join(map)).unwrap();
    let level = (0..pkg.exports.len() as u32).find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level").unwrap();
    let Asset::Level(l) = load_export(&pkg, level).unwrap().asset else { panic!("no level") };
    let ObjectRef::Export(m) = l.model else { panic!("no model") };
    let Asset::Model(model) = load_export(&pkg, m).unwrap().asset else { panic!("no model") };
    Bsp::new(&model)
}

#[test]
fn the_floor_is_where_both_box_queries_say_it_is() {
    let bsp = bsp_of("PrivetDr.unr");
    // Harry's bedroom, over a cut mark the intro walks him to.
    let (x, y) = (-882.0, 711.0);
    let extent = [15.0, 15.0, 42.0];
    let resting = bsp
        .box_check([x, y, 340.0], [x, y, 140.0], extent)
        .expect("a trace down the room finds its floor")
        .location[2];
    assert!((resting - 234.0).abs() < 2.0, "landed at {resting}");
    for z in (150..=330).step_by(4).map(|z| z as f32) {
        let solid = bsp.box_solid([x, y, z], extent);
        assert_eq!(solid, z < resting - 1.0, "a box at {z} reported solid {solid}");
        if z > resting {
            let landed = bsp.box_check([x, y, z], [x, y, z - 200.0], extent).map(|h| h.location[2]);
            assert!(landed.is_some_and(|l| (l - resting).abs() < 2.0), "from {z} it landed at {landed:?}");
        }
    }
}
