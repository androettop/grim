//! Line traces must agree with point queries: just before a hit the ray is in open space,
//! just after it is inside solid geometry.

use grim_assets::{load_export, Asset};
use grim_object::{Value, World};
use grim_package::{Library, ObjectRef};
use grim_world::{add, lerp, scale, sub, Bsp};

fn rng(state: &mut u64) -> f32 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    ((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / (1u64 << 24) as f32
}

#[test]
fn line_check_agrees_with_point_queries() {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    let mut state = 0x1234_5678_9abc_def0u64;
    let (mut traces, mut hits, mut bad_before, mut bad_after, mut open_pairs, mut false_hits) = (0, 0, 0, 0, 0, 0);
    for map in grim_testkit::package_files(&root.join("Maps")) {
        let pkg = world.lib.load_path(&map).unwrap();
        let Some(li) = (0..pkg.exports.len() as u32).find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level") else { continue };
        let Asset::Level(level) = load_export(&pkg, li).unwrap().asset else { continue };
        let ObjectRef::Export(me) = level.model else { continue };
        let Asset::Model(model) = load_export(&pkg, me).unwrap().asset else { continue };
        let bsp = Bsp::new(&model);
        if bsp.is_empty() {
            continue;
        }
        // Start from actor locations that sit in open space.
        let mut starts = Vec::new();
        for a in &level.actors {
            let ObjectRef::Export(e) = a else { continue };
            let obj = world.instantiate(&pkg, *e).unwrap();
            let class = world.class(obj.class).clone();
            let Some(loc) = class.prop(world.names.intern("Location")) else { continue };
            let Value::Struct(l) = &obj.values[loc.slot as usize] else { continue };
            let (Value::Float(x), Value::Float(y), Value::Float(z)) = (&l[0], &l[1], &l[2]) else { continue };
            let p = [*x, *y, *z];
            if p.iter().all(|c| c.is_finite()) && !bsp.point_solid(p) {
                starts.push(p);
            }
            if starts.len() >= 200 {
                break;
            }
        }
        for &p in &starts {
            for _ in 0..5 {
                let dir = [rng(&mut state) * 2.0 - 1.0, rng(&mut state) * 2.0 - 1.0, rng(&mut state) * 2.0 - 1.0];
                let end = add(p, scale(dir, 4000.0));
                traces += 1;
                match bsp.line_check(p, end) {
                    Some(h) => {
                        hits += 1;
                        let d = sub(end, p);
                        // 1 unit before and after the hit along the ray.
                        let eps = 0.25 / (grim_world::dot(d, d).sqrt().max(1.0));
                        if bsp.point_solid(lerp(p, end, (h.time - eps).max(0.0))) {
                            bad_before += 1;
                        }
                        if !bsp.point_solid(lerp(p, end, (h.time + eps).min(1.0))) {
                            bad_after += 1;
                        }
                    }
                    None => {
                        open_pairs += 1;
                        // Nothing hit: the far end must be open space too.
                        if bsp.point_solid(end) {
                            false_hits += 1;
                        }
                    }
                }
            }
        }
    }
    eprintln!("{traces} traces, {hits} hits, {bad_before} start-side solid, {bad_after} end-side open, {open_pairs} clear, {false_hits} missed solid ends");
    assert!(traces > 10_000);
    assert!(bad_before * 1000 <= hits, "{bad_before}/{hits} hits start inside solid space");
    // A few hits graze edges and thin wedges, where a point past the surface is open again.
    assert!(bad_after * 100 <= hits, "{bad_after}/{hits} hits do not enter solid space");
    assert!(false_hits * 1000 <= open_pairs, "{false_hits}/{open_pairs} clear traces end inside solid space");
}
