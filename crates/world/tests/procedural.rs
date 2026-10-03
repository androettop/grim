//! The textures the engine draws itself are stored with no pixels: a fire keeps only its
//! palette, its emitters and its settings. Running one has to give a picture that both changes
//! over time and uses the colours the texture was given, or nothing of it shows in the game.

use grim_assets::procedural::ProceduralKind;
use grim_assets::{load_export, Asset};
use grim_package::{Library, ObjectRef};
use grim_world::procedural::{Drop, Procedural, Settings};

/// Every procedural texture of the game, built and ready to run.
fn running(lib: &Library, root: &std::path::Path) -> Vec<(String, Procedural)> {
    let mut out = Vec::new();
    for file in grim_testkit::package_files(root) {
        let Ok(pkg) = lib.load_path(&file) else { continue };
        for i in 0..pkg.exports.len() as u32 {
            let Ok(obj) = load_export(&pkg, i) else { continue };
            let Asset::ProceduralTexture(p) = &obj.asset else { continue };
            let palette = match obj.base.property(&pkg, "Palette", 0) {
                Some(grim_package::property::PropertyValue::Object(r)) => match lib.resolve(&pkg, *r) {
                    Ok(h) => match load_export(&h.package, h.export).map(|l| l.asset) {
                        Ok(Asset::Palette(pal)) => pal.colors.iter().map(|c| [c.r, c.g, c.b, c.a]).collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            let byte = |name: &str| match obj.base.property(&pkg, name, 0) {
                Some(grim_package::property::PropertyValue::Byte(b)) => *b,
                _ => 0,
            };
            let settings = Settings {
                render_heat: byte("RenderHeat"),
                rising: obj.base.bool_property(&pkg, "bRising").unwrap_or(false),
                parametric: obj.base.bool_property(&pkg, "bParametric").unwrap_or(false),
                fx_heat: byte("FX_Heat"),
                fx_size: byte("FX_Size"),
                fx_area: byte("FX_Area"),
                fx_frequency: byte("FX_Frequency"),
                fx_phase: byte("FX_Phase"),
                fx_horiz_speed: byte("FX_HorizSpeed"),
                fx_vert_speed: byte("FX_VertSpeed"),
                fx_amplitude: byte("FX_Amplitude"),
                fx_speed: byte("FX_Speed"),
                fx_depth: byte("FX_Depth"),
                wave_amp: byte("WaveAmp"),
            };
            let count = obj.base.int_property(&pkg, "NumDrops").unwrap_or(0).max(0) as u32;
            let drops: Vec<Drop> = (0..count)
                .filter_map(|d| {
                    let grim_package::property::PropertyValue::Struct { value, .. } = obj.base.property(&pkg, "Drops", d)? else {
                        return None;
                    };
                    let grim_package::property::StructValue::Unparsed(bytes) = value else { return None };
                    let b = &bytes.bytes;
                    (b.len() >= 8).then(|| Drop { kind: b[0], depth: b[1], x: b[2], y: b[3], rate: b[7] })
                })
                .collect();
            let name = format!("{}.{}", pkg.name, pkg.object_name(ObjectRef::Export(i)));
            if let Some(p) = Procedural::new(p, settings, palette, drops, None) {
                out.push((name, p));
            }
        }
    }
    out
}

#[test]
fn procedural_textures_draw_themselves() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let mut all = running(&lib, &root);
    assert!(!all.is_empty(), "the game has procedural textures to run");

    let (mut fires, mut moved, mut coloured) = (0, 0, 0);
    for (name, p) in &mut all {
        if p.kind != ProceduralKind::Fire {
            continue;
        }
        if p.spark_count() == 0 {
            // A picture with no emitters has nothing to draw, and two of the game's are empty.
            continue;
        }
        fires += 1;
        // A second of it: long enough for the emitters to fill the picture.
        for _ in 0..35 {
            p.tick(1.0 / 35.0);
        }
        let first = p.rgba();
        for _ in 0..35 {
            p.tick(1.0 / 35.0);
        }
        let second = p.rgba();
        let lit = first.chunks(4).filter(|c| c[0] > 8 || c[1] > 8 || c[2] > 8).count();
        if lit > 0 {
            coloured += 1;
        } else {
            println!("{name}: nothing lit ({} sparks, palette {})", p.spark_count(), p.palette_len());
        }
        if first != second {
            moved += 1;
        }
    }
    println!("{fires} fires with emitters, {coloured} with something lit, {moved} that keep changing");
    assert!(fires > 100, "the game has fire textures");
    assert_eq!(coloured, fires, "every fire with an emitter puts colour on its picture");
    // Most of them move; a few settle into a steady glow, which is a picture of its own.
    assert!(moved * 10 >= fires * 9, "the fires keep moving");
}
