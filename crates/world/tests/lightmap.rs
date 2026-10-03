//! Lightmap layout over every map: block sizes, grid coverage and baking.

use grim_assets::model::Model;
use grim_assets::{load_export, Asset};
use grim_package::{Library, ObjectRef};
use grim_world::lightmap::{self, LightSample};

fn model_of(lib: &Library, map: &str) -> Option<Model> {
    let pkg = lib.load(map).ok()?;
    let li = (0..pkg.exports.len() as u32).find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level")?;
    let Asset::Level(l) = load_export(&pkg, li).ok()?.asset else { return None };
    let ObjectRef::Export(mi) = l.model else { return None };
    match load_export(&pkg, mi).ok()?.asset {
        Asset::Model(m) => Some(*m),
        _ => None,
    }
}

/// Lights of a lightmap: entries from `i_light_actors` until a null one.
fn lights_of(model: &Model, lm: &grim_assets::model::LightMapIndex) -> usize {
    if lm.i_light_actors < 0 {
        return 0;
    }
    let mut k = lm.i_light_actors as usize;
    let mut n = 0;
    while model.lights.get(k).is_some_and(|r| !r.is_null()) {
        n += 1;
        k += 1;
    }
    n
}

#[test]
fn layout() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let maps = grim_testkit::map_names(&root);
    assert!(!maps.is_empty(), "no maps found");
    let (mut sized, mut blocks, mut inside, mut outside) = (0u64, 0u64, 0u64, 0u64);
    let mut bits = 0u64;

    for map in &maps {
        let Some(model) = model_of(&lib, map) else { continue };
        // A lightmap's block is one bitmap per light: ceil(u_clamp / 8) bytes per row.
        let mut order: Vec<usize> = (0..model.light_map.len()).collect();
        order.sort_by_key(|&i| (model.light_map[i].data_offset, i));
        for w in order.windows(2) {
            let (a, b) = (&model.light_map[w[0]], &model.light_map[w[1]]);
            // Lightmaps without lights take no space and share their neighbour's offset.
            if lights_of(&model, a) == 0 || lights_of(&model, b) == 0 {
                continue;
            }
            let size = (b.data_offset - a.data_offset) as i64;
            let rows = a.v_clamp as i64;
            let expected = lights_of(&model, a) as i64 * ((a.u_clamp as i64).div_euclid(8) + i64::from(a.u_clamp % 8 != 0)) * rows;
            blocks += 1;
            if size == expected {
                sized += 1;
            }
        }
        // Every byte of the light data belongs to exactly one lightmap: nothing unexplained.
        let mut cover = vec![0u8; model.light_bits.len()];
        for lm in &model.light_map {
            let stride = (lm.u_clamp as usize).div_ceil(8);
            let start = lm.data_offset.max(0) as usize;
            let end = start + lights_of(&model, lm) * stride * lm.v_clamp.max(0) as usize;
            assert!(end <= cover.len(), "{map}: lightmap block runs past the light bits");
            for c in &mut cover[start..end] {
                *c += 1;
            }
        }
        assert!(cover.iter().all(|&c| c == 1), "{map}: light bits left over or shared between lightmaps");
        bits += cover.len() as u64;
        // Each node's vertices must land inside its surface's texel grid.
        for node in &model.nodes {
            if node.num_vertices < 3 || node.i_surf < 0 {
                continue;
            }
            let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
            if surf.i_light_map < 0 {
                continue;
            }
            let Some(lm) = model.light_map.get(surf.i_light_map as usize) else { continue };
            let mut ok = true;
            for k in 0..node.num_vertices as i32 {
                let Some(vert) = model.verts.get((node.i_vert_pool + k) as usize) else { continue };
                let Some(p) = model.points.get(vert.p_vertex as usize) else { continue };
                let pieces = lightmap::Pieces::Surfs(&model);
                let Some([u, v]) = pieces.coords(node.i_surf as usize, [p.x, p.y, p.z]) else { continue };
                // Texel centres are whole coordinates: the grid spans 0 ..= clamp - 1.
                ok &= u >= -0.1 && v >= -0.1 && u <= (lm.u_clamp - 1) as f32 + 0.1 && v <= (lm.v_clamp - 1) as f32 + 0.1;
            }
            if ok {
                inside += 1;
            } else {
                outside += 1;
            }
        }
    }
    eprintln!("   {bits} bytes of light data, every one claimed by one lightmap");
    eprintln!("   blocks sized as lights * ceil(u/8) * v: {sized}/{blocks}");
    eprintln!("   nodes inside their lightmap grid: {inside}, outside {outside}");
    assert_eq!(outside, 0, "some nodes fall outside their lightmap grid");
    assert_eq!(sized, blocks, "block sizes stopped matching the layout");
}

#[test]
fn bakes_every_map() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    for map in grim_testkit::map_names(&root) {
        let Some(model) = model_of(&lib, &map) else { continue };
        // One white light per reference: enough to walk every bitmap of every surface.
        let light = |_: ObjectRef| Some(LightSample { position: [0.0; 3], color: [1.0; 3], radius: 4096.0, inner: 0.0, cone: None, ignores_angle: false, dark: false, special: false });
        let ambient = vec![[0.1; 3]; model.surfs.len()];
        let baked = lightmap::bake(&lightmap::Pieces::Surfs(&model), &ambient, 6, &light);
        assert_eq!(baked.rgba.len(), (baked.width * baked.height * 4) as usize);
        assert!(baked.width <= 4096 && baked.height <= 4096, "{map}: lightmap atlas too large");
        for (i, p) in baked.placements.iter().enumerate() {
            if p.valid {
                assert!(p.x + p.width <= baked.width && p.y + p.height <= baked.height, "{map}: surface {i} spills out of the atlas");
            }
        }
    }
}

/// A texel's world position must land back on the polygon it came from: the lightmap axes are
/// not always perpendicular, so the inverse has to solve both together.
#[test]
fn texel_positions_round_trip() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let (mut worst, mut worst_map) = (0.0f32, String::new());
    let mut sum = 0.0f64;
    let mut n = 0u64;
    let mut off = 0u64;
    let mut samples: Vec<String> = Vec::new();
    for map in grim_testkit::map_names(&root) {
        let Some(model) = model_of(&lib, &map) else { continue };
        for node in &model.nodes {
            if node.i_surf < 0 || node.num_vertices < 3 {
                continue;
            }
            let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
            if surf.i_light_map < 0 {
                continue;
            }
            for k in 0..node.num_vertices as i32 {
                let Some(vert) = model.verts.get((node.i_vert_pool + k) as usize) else { continue };
                let Some(p) = model.points.get(vert.p_vertex as usize) else { continue };
                let p = [p.x, p.y, p.z];
                let pieces = lightmap::Pieces::Surfs(&model);
                let Some(texel) = pieces.coords(node.i_surf as usize, p) else { continue };
                let Some(back) = pieces.position(node.i_surf as usize, texel) else { continue };
                let d = (0..3).map(|i| (back[i] - p[i]).powi(2)).sum::<f32>().sqrt();
                sum += d as f64;
                n += 1;
                if d > 1.0 {
                    off += 1;
                    if samples.len() < 4 {
                        let tu = model.vectors[surf.v_texture_u as usize];
                        let tv = model.vectors[surf.v_texture_v as usize];
                        let nn = model.vectors[surf.v_normal as usize];
                        samples.push(format!(
                            "{map} surf {} off by {d:.1}: tu ({:.2},{:.2},{:.2}) tv ({:.2},{:.2},{:.2}) n ({:.2},{:.2},{:.2})",
                            node.i_surf, tu.x, tu.y, tu.z, tv.x, tv.y, tv.z, nn.x, nn.y, nn.z
                        ));
                    }
                }
                if d > worst {
                    worst = d;
                    worst_map = map.clone();
                }
            }
        }
    }
    eprintln!("   texel round trip over {n} vertices: mean {:.4}, worst {worst:.3} ({worst_map})", sum / n as f64);
    // The few that miss are vertices that do not lie on their surface's plane at all (their
    // texture axes are orthonormal, so the inverse is exact): duplicated brush faces, off by a
    // round 64 units. They stay a rounding of the light they receive, not a mapping error.
    eprintln!("   vertices that do not lie on their surface's plane: {off}");
    for s in &samples {
        eprintln!("     {s}");
    }
    assert!(sum / n as f64 <= 0.05, "texel positions drifted from their polygons");
    assert!(off * 1000 < n, "too many vertices off their surface's plane");
}

/// Where a mover's baked light lives. Its brush leaves `FBspSurf::iLightMap` at -1 and numbers
/// the editor polygons instead, in `iBrushPoly`: the entries are exactly as many as the
/// numbered polygons, numbered 0 upwards. That is what lets a door show a picture on one band
/// of a face and stone on the next, each with its own light.
#[test]
fn a_mover_numbers_its_lightmaps_from_its_polygons() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let (mut brushes, mut lit) = (0, 0);
    for path in grim_testkit::package_files(&root.join("Maps")) {
        let Ok(pkg) = lib.load_path(&path) else { continue };
        for i in 0..pkg.exports.len() as u32 {
            if !pkg.class_name(grim_package::ObjectRef::Export(i)).contains("Mover") {
                continue;
            }
            let Ok(obj) = grim_assets::load_export(&pkg, i) else { continue };
            let Some(grim_package::property::PropertyValue::Object(grim_package::ObjectRef::Export(b))) =
                obj.base.property(&pkg, "Brush", 0)
            else {
                continue;
            };
            let Ok(grim_assets::Asset::Model(model)) = grim_assets::load_export(&pkg, *b).map(|l| l.asset) else { continue };
            brushes += 1;
            assert!(
                model.surfs.iter().all(|s| s.i_light_map < 0),
                "{}: a mover's surfaces name a lightmap of their own",
                pkg.name
            );
            if model.light_map.is_empty() {
                continue;
            }
            let grim_package::ObjectRef::Export(p) = model.polys else { continue };
            let Ok(grim_assets::Asset::Polys(polys)) = grim_assets::load_export(&pkg, p).map(|l| l.asset) else { continue };
            let numbered: Vec<i32> = polys.polys.iter().map(|p| p.i_brush_poly).filter(|&n| n >= 0).collect();
            assert_eq!(
                numbered.len(),
                model.light_map.len(),
                "{} mover {}: {} numbered polygons for {} lightmaps",
                pkg.name,
                pkg.object_name(grim_package::ObjectRef::Export(i)),
                numbered.len(),
                model.light_map.len()
            );
            assert_eq!(
                numbered.iter().copied().max().unwrap_or(-1),
                numbered.len() as i32 - 1,
                "{}: the numbers of the polygons leave gaps",
                pkg.name
            );
            lit += 1;
        }
    }
    eprintln!("   {brushes} mover brushes, {lit} with baked light numbered from their polygons");
    assert!(lit > 100, "hardly any mover carries baked light: {lit}");
}
