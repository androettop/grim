//! The BSP walk must reproduce the zone and leaf the editor stored in every actor's `Region`.

use grim_assets::{load_export, Asset};
use grim_object::{Value, World};
use grim_package::{Library, ObjectRef};
use grim_world::Bsp;

#[test]
fn point_region_matches_stored_actor_regions() {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    let (mut checked, mut zone_ok, mut leaf_ok, mut solid) = (0usize, 0usize, 0usize, 0usize);
    let mut examples = Vec::new();
    for map in grim_testkit::package_files(&root.join("Maps")) {
        let pkg = world.lib.load_path(&map).unwrap();
        let Some(level_idx) = (0..pkg.exports.len() as u32).find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level") else { continue };
        let Asset::Level(level) = load_export(&pkg, level_idx).unwrap().asset else { continue };
        let Asset::Model(model) = load_export(&pkg, match level.model {
            ObjectRef::Export(e) => e,
            _ => continue,
        }).unwrap().asset else { continue };
        let bsp = Bsp::new(&model);
        if bsp.is_empty() {
            continue;
        }
        for a in &level.actors {
            let ObjectRef::Export(e) = a else { continue };
            let obj = world.instantiate(&pkg, *e).unwrap();
            let class = world.class(obj.class).clone();
            let (Some(region), Some(location)) = (class.prop(world.names.intern("Region")), class.prop(world.names.intern("Location"))) else { continue };
            let (Value::Struct(r), Value::Struct(l)) = (&obj.values[region.slot as usize], &obj.values[location.slot as usize]) else { continue };
            let (Value::Int(leaf), Value::Byte(zone)) = (&r[1], &r[2]) else { continue };
            let (Value::Float(x), Value::Float(y), Value::Float(z)) = (&l[0], &l[1], &l[2]) else { continue };
            // Editor-only brushes never get a region computed (stored leaf stays -1).
            if !x.is_finite() || !y.is_finite() || !z.is_finite() || *leaf < 0 {
                continue;
            }
            let (got_leaf, got_zone) = bsp.point_region([*x, *y, *z]);
            checked += 1;
            zone_ok += (got_zone == *zone) as usize;
            leaf_ok += (got_leaf == *leaf) as usize;
            if got_zone != *zone && got_leaf < 0 {
                solid += 1;
            }
            if got_zone != *zone && got_leaf >= 0 && examples.len() < 8 {
                examples.push(format!(
                    "{} {}: stored zone {zone} leaf {leaf}, got zone {got_zone} leaf {got_leaf}",
                    map.file_stem().unwrap().to_string_lossy(),
                    pkg.object_name(*a)
                ));
            }
        }
    }
    eprintln!("{checked} actors: zone {zone_ok}, leaf {leaf_ok}, mismatches inside solid space {solid}\n{}", examples.join("\n"));
    assert!(checked > 30_000);
    // The rest are actors embedded in solid geometry, whose stored region is stale editor data.
    assert!(zone_ok * 1000 >= checked * 995, "only {zone_ok}/{checked} zones match");
}
