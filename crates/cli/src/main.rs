use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use grim_package::hexdump::{hexdump, hexdump_around};
use grim_package::object::read_object_base;
use grim_package::{ObjectRef, Package};

/// `println!` that exits quietly when stdout is closed (e.g. `| head`).
macro_rules! out {
    ($($t:tt)*) => {{
        let mut o = std::io::stdout().lock();
        if writeln!(o, $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const USAGE: &str = "\
usage:
  grim survey [dir]              parse every package and summarize by class
  grim objects                   links every class and instantiates every object
  grim skins <map>               reports actors whose mesh textures do not resolve
  grim surfs <map>               counts the BSP surfaces of a map by poly flag
  grim surfs <map> tex <name>    where the surfaces drawn with that texture are
  grim floor <map> x,y,z         how far below a point the level's floor is
  grim texflags <bool>           textures of the game that carry that boolean flag
  grim class <name>              prints a class chain and its non-empty default values
  grim func <pattern>            prints matching function signatures and class chains
  grim natives                   lists every native function declared by the scripts
  grim run [map]                 runs the startup sequence of every map (or one) in the VM
  grim save <file> [ticks]       loads a saved game and reports every property of every actor
                                that the engine does not leave as the original's save has it
  grim pa <file> [actor]         a save slot's persistent actors; with a name, its properties
  grim cut <map> [seconds]       runs a map and reports what its cutscenes do
  grim cuts [map] [seconds]      plays every cutscene on its own and reports what it lacked;
                                the seconds are how long one may stand still before giving up
  grim lightbits <map> [x,y,z r] checks the shadow bitmaps the map ships with, or those near
                                a point
  grim ls <package>              list exports
  grim imports <package>         list imports
  grim names <package>           list the name table
  grim layout <package>          file region map, gaps and overlaps
  grim hex <package> <export>    hexdump an export's data (index or name)
  grim obj <package> <export>    parse an export's UObject base
  grim props <package>           every property the package's objects carry, and how many do
  grim props <package> <prop>   every export that carries that property, and what it holds
  grim script <package> <class>  print a class's script text
  grim font <package> <font> <text>
                                where each character of the text sits in the font's pages
  grim brush <package> <model>   a brush model's box and the span of its polygons
  grim posebox <package> <mesh>  the box of a skeletal mesh in its reference pose and at the
                                first frame of every sequence
  grim code <package> <function> print a function's decoded bytecode
  grim anim <package> <export>   print an animation's sequences and its tracks' keys
  grim mesh <package> <export>   a mesh's origin, scale and box, which place it on its actor
  grim refs <package> <object>   functions of the package whose code names that object
  grim tex <package> <export> <out.ppm>
                                writes a texture's largest picture out as it is stored
  grim fire <package> <export> [out.ppm [frames]]
                                a procedural texture's settings and sparks; with a path, runs
                                it for a second and writes the picture out, or a run of them";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["survey"] => survey(&grim_testkit::require_game_dir()),
        ["survey", dir] => survey(Path::new(dir)),
        ["objects"] => objects(&grim_testkit::require_game_dir()),
        ["natives"] => natives(&grim_testkit::require_game_dir()),
        ["func", pat] => funcs(&grim_testkit::require_game_dir(), pat),
        ["class", name] => class_defaults(&grim_testkit::require_game_dir(), name),
        ["skins", map] => skins(&grim_testkit::require_game_dir(), map),
        ["surfs", map] => surfs(&grim_testkit::require_game_dir(), map, None, None),
        ["surfs", map, "at", at, reach] => {
            let v: Vec<f32> = at.split(',').filter_map(|c| c.parse().ok()).collect();
            match (v.len() == 3, reach.parse::<f32>()) {
                (true, Ok(r)) => surfs(&grim_testkit::require_game_dir(), map, None, Some([v[0], v[1], v[2], r])),
                _ => Err("usage: grim surfs <map> at x,y,z <reach>".into()),
            }
        }
        ["surfs", map, "tex", name] => surfs_named(&grim_testkit::require_game_dir(), map, name),
        ["floor", map, at] => floor(&grim_testkit::require_game_dir(), map, at),
        ["surfs", map, flag] => match u32::from_str_radix(flag.trim_start_matches("0x"), 16) {
            Ok(flag) => surfs(&grim_testkit::require_game_dir(), map, Some(flag), None),
            Err(e) => Err(e.to_string()),
        },
        ["texflags", name] => texflags(&grim_testkit::require_game_dir(), name),
        ["polynames", map] => polynames(&grim_testkit::require_game_dir(), map),
        ["movers", map] => movers(&grim_testkit::require_game_dir(), map),
        ["brushsolid", map] => brushsolid(&grim_testkit::require_game_dir(), map),
        ["moverplaces", map] => moverplaces(&grim_testkit::require_game_dir(), map),
        ["brushuv", map, name] => brushuv(&grim_testkit::require_game_dir(), map, name),
        ["fields", p, name] => with_pkg(p, |pkg| fields(pkg, name)),
        ["findtext", map, text] => findtext(&grim_testkit::require_game_dir(), map, text),
        ["lightcheck", map] => lightcheck(&grim_testkit::require_game_dir(), map),
        ["lightbits", map] => lightbits(&grim_testkit::require_game_dir(), map, None),
        ["lightbits", map, at, reach] => {
            let v: Vec<f32> = at.split(',').filter_map(|c| c.trim().parse().ok()).collect();
            match (v.len() == 3, reach.parse::<f32>()) {
                (true, Ok(r)) => lightbits(&grim_testkit::require_game_dir(), map, Some(([v[0], v[1], v[2]], r))),
                _ => Err("usage: grim lightbits <map> x,y,z <reach>".into()),
            }
        }
        ["cuts"] => cut_all(&grim_testkit::require_game_dir(), None, 30.0),
        // A number on its own is how long any cutscene may stand still, over every map.
        ["cuts", secs] if secs.parse::<f32>().is_ok() => {
            cut_all(&grim_testkit::require_game_dir(), None, secs.parse().unwrap_or(30.0))
        }
        ["cuts", map] => cut_all(&grim_testkit::require_game_dir(), Some(map), 30.0),
        ["cuts", map, secs] => match secs.parse() {
            Ok(s) => cut_all(&grim_testkit::require_game_dir(), Some(map), s),
            Err(e) => Err(format!("seconds: {e}")),
        },
        ["cut", map] => cutscenes(&grim_testkit::require_game_dir(), map, 5.0),
        ["cut", map, secs] => match secs.parse() {
            Ok(s) => cutscenes(&grim_testkit::require_game_dir(), map, s),
            Err(e) => Err(format!("seconds: {e}")),
        },
        ["pa", file] => persistent(&grim_testkit::require_game_dir(), Path::new(file), None),
        ["pa", file, actor] => persistent(&grim_testkit::require_game_dir(), Path::new(file), Some(actor)),
        ["save", file] => save_diff(&grim_testkit::require_game_dir(), Path::new(file), 2),
        ["save", file, ticks] => match ticks.parse() {
            Ok(t) => save_diff(&grim_testkit::require_game_dir(), Path::new(file), t),
            Err(e) => Err(format!("ticks: {e}")),
        },
        ["run"] => run_maps(&grim_testkit::require_game_dir(), None, 1.0),
        ["run", map] => run_maps(&grim_testkit::require_game_dir(), Some(map), 1.0),
        ["run", map, secs] => match secs.parse() {
            Ok(s) => run_maps(&grim_testkit::require_game_dir(), Some(map), s),
            Err(e) => Err(format!("seconds: {e}")),
        },
        ["ls", p] => with_pkg(p, ls),
        ["imports", p] => with_pkg(p, imports),
        ["names", p] => with_pkg(p, names),
        ["layout", p] => with_pkg(p, layout),
        ["hex", p, e] => with_pkg(p, |pkg| hex(pkg, e)),
        ["obj", p, e] => with_pkg(p, |pkg| obj(pkg, e)),
        ["props", p] => with_pkg(p, |pkg| prop_names(pkg)),
        ["props", p, e] => with_pkg(p, |pkg| props(pkg, e)),
        ["script", p, e] => with_pkg(p, |pkg| script(pkg, e)),
        ["code", p, e] => with_pkg(p, |pkg| code(pkg, e)),
        ["refs", p, e] => with_pkg(p, |pkg| refs(pkg, e)),
        ["tex", p, e, out] => with_pkg(p, |pkg| tex(pkg, e, out)),
        ["fire", p, e] => with_pkg(p, |pkg| fire(pkg, e, None, 1)),
        ["fire", p, e, out] => with_pkg(p, |pkg| fire(pkg, e, Some(out), 1)),
        ["fire", p, e, out, frames] => match frames.parse() {
            Ok(n) => with_pkg(p, |pkg| fire(pkg, e, Some(out), n)),
            Err(e) => Err(format!("frames: {e}")),
        },
        ["anim", p, e] => with_pkg(p, |pkg| anim(pkg, e)),
        ["mesh", p, e] => with_pkg(p, |pkg| mesh(pkg, e)),
        ["posebox", p, e] => with_pkg(p, |pkg| posebox(pkg, e)),
        ["brush", p, e] => with_pkg(p, |pkg| brush(pkg, e)),
        ["font", p, e, text] => with_pkg(p, |pkg| font(pkg, e, text)),
        _ => Err(USAGE.to_string()),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &Path) -> Result<Package, String> {
    Package::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map_err(|e| {
            let bytes = std::fs::read(path).unwrap_or_default();
            format!("{}: {e}\n{}", path.display(), hexdump_around(&bytes, e.offset, 64, 64))
        })
}

/// A path, or a package name looked up in the game directory.
fn resolve(p: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(p);
    if path.exists() {
        return Ok(path);
    }
    let root = grim_testkit::require_game_dir();
    grim_testkit::package_files(&root)
        .into_iter()
        .find(|f| {
            f.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.eq_ignore_ascii_case(p) || n.split('.').next().is_some_and(|s| s.eq_ignore_ascii_case(p))
            })
        })
        .ok_or_else(|| format!("package '{p}' not found"))
}

fn with_pkg(p: &str, f: impl FnOnce(&Package) -> Result<(), String>) -> Result<(), String> {
    f(&load(&resolve(p)?)?)
}

fn find_export(pkg: &Package, e: &str) -> Result<u32, String> {
    if let Ok(i) = e.parse::<u32>() {
        if (i as usize) < pkg.exports.len() {
            return Ok(i);
        }
    }
    (0..pkg.exports.len() as u32)
        .find(|&i| pkg.object_name(ObjectRef::Export(i)).eq_ignore_ascii_case(e))
        .ok_or_else(|| format!("no export '{e}'"))
}

fn ls(pkg: &Package) -> Result<(), String> {
    out!("{} v{} lic{} flags={:#x}", pkg.name, pkg.version(), pkg.header.licensee, pkg.header.flags);
    for (i, e) in pkg.exports.iter().enumerate() {
        let r = ObjectRef::Export(i as u32);
        out!(
            "{i:5} {:<20} {:<50} flags={:08x} size={:<8} @{:#x}",
            pkg.class_name(r),
            pkg.path_name(r),
            e.flags,
            e.serial_size,
            e.serial_offset
        );
    }
    Ok(())
}

fn imports(pkg: &Package) -> Result<(), String> {
    for (i, im) in pkg.imports.iter().enumerate() {
        let r = ObjectRef::Import(i as u32);
        out!("{i:5} {}.{:<20} {}", pkg.name(im.class_package), pkg.name(im.class_name), pkg.path_name(r));
    }
    Ok(())
}

fn names(pkg: &Package) -> Result<(), String> {
    for (i, n) in pkg.names.iter().enumerate() {
        out!("{i:5} {:08x} {}", n.flags, n.name);
    }
    Ok(())
}

fn layout(pkg: &Package) -> Result<(), String> {
    let l = pkg.layout();
    for r in &l.regions {
        out!("{:#010x}..{:#010x} {:?}", r.start, r.end, r.what);
    }
    for (s, e) in &l.gaps {
        out!("GAP {s:#x}..{e:#x} ({} bytes)\n{}", e - s, hexdump(&pkg.bytes()[*s..(*e).min(s + 256)], *s, None));
    }
    for (a, b) in &l.overlaps {
        out!("OVERLAP {a:?} / {b:?}");
    }
    Ok(())
}

fn hex(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let r = pkg.export_reader(i);
    let base = r.offset();
    let mut r = r;
    out!("{}", hexdump(r.rest(), base, None));
    Ok(())
}

/// Prints a function's or a state's bytecode, for the classes whose script text was stripped.
/// A state carries code of its own, the one a label jumps into, so it is printed the same way.
fn code(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let script = match grim_assets::load_export(pkg, i).map_err(|x| x.to_string())?.asset {
        grim_assets::Asset::Function(f) => f.struct_.script,
        grim_assets::Asset::State(s) => {
            out!("state: probe mask {:#x} ignore mask {:#x} flags {:#x}", s.probe_mask, s.ignore_mask, s.flags);
            s.struct_.script
        }
        other => return Err(format!("{e} is a {other:?}, which carries no code")),
    };
    for (n, st) in script.iter().enumerate() {
        out!("{n:5} @{:<5} {:?}", st.offset, st.expr);
    }
    Ok(())
}

/// A procedural texture: what it is, how big, and the sparks stored with it.
/// A texture's largest picture, written out as it is stored: which way round the artist drew
/// it is what says whether a surface's texture axes turn it or leave it upright.
fn tex(pkg: &Package, e: &str, out: &str) -> Result<(), String> {
    let export = find_export(pkg, e)?;
    let loaded = grim_assets::load_export(pkg, export).map_err(|e| e.to_string())?;
    let grim_assets::Asset::Texture(t) = &loaded.asset else { return Err("not a texture".into()) };
    let palette = match loaded.base.property(pkg, "Palette", 0) {
        Some(grim_package::property::PropertyValue::Object(grim_package::ObjectRef::Export(p))) => {
            match grim_assets::load_export(pkg, *p).map(|l| l.asset) {
                Ok(grim_assets::Asset::Palette(pal)) => Some(pal),
                _ => None,
            }
        }
        _ => None,
    };
    let mip = t.mips.first().ok_or("no picture")?;
    let img = grim_assets::texture::decode_mip(mip, t.format, palette.as_ref(), true)?;
    out!("  {}x{} {:?}", img.width, img.height, t.format);
    let mut ppm = format!("P6\n{} {}\n255\n", img.width, img.height).into_bytes();
    ppm.extend(img.pixels.chunks(4).flat_map(|c| [c[0], c[1], c[2]]));
    std::fs::write(out, ppm).map_err(|e| e.to_string())
}

fn fire(pkg: &Package, e: &str, out: Option<&str>, frames: u32) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let loaded = grim_assets::load_export(pkg, i).map_err(|x| x.to_string())?;
    let grim_assets::Asset::ProceduralTexture(p) = &loaded.asset else {
        return Err(format!("{e} is not a procedural texture"));
    };
    let mip = p.texture.mips.first();
    out!(
        "{:?} {}x{}, {} mips, first mip {} bytes",
        p.kind,
        mip.map_or(0, |m| m.u_size),
        mip.map_or(0, |m| m.v_size),
        p.texture.mips.len(),
        mip.map_or(0, |m| m.data.len())
    );
    for name in [
        "SparkType", "RenderHeat", "bRising", "DrawMode", "NumSparks", "SparksLimit", "bParametric",
        "FX_Heat", "FX_Size", "FX_AuxSize", "FX_Area", "FX_Frequency", "FX_Phase", "FX_HorizSpeed", "FX_VertSpeed",
        "UMask", "VMask", "GlobalPhase", "DrawPhase", "AuxPhase", "SourceTexture", "GlassTexture", "PanningStyle",
    ] {
        if let Some(v) = loaded.base.property(pkg, name, 0) {
            out!("  {name} = {v:?}");
        }
    }
    let drops = loaded.base.int_property(pkg, "NumDrops").unwrap_or(0).max(0) as u32;
    for d in 0..drops {
        if let Some(grim_package::property::PropertyValue::Struct { value, .. }) = loaded.base.property(pkg, "Drops", d) {
            if let grim_package::property::StructValue::Unparsed(bytes) = value {
                out!("  drop {d}: {:?}", bytes.bytes);
            }
        }
    }
    for (n, s) in p.sparks.iter().enumerate() {
        out!("  spark {n}: kind {} heat {} at ({}, {}) bytes {} {} {} {}", s.kind, s.heat, s.x, s.y, s.byte_a, s.byte_b, s.byte_c, s.byte_d);
    }
    if let Some(path) = out {
        write_procedural(pkg, &loaded, p, path, frames)?;
    }
    Ok(())
}

/// Runs a procedural texture for a second and writes what it draws, as plain PPMs: one
/// picture, or a run of them a frame apart to see how it moves.
fn write_procedural(
    pkg: &Package,
    loaded: &grim_assets::LoadedObject,
    p: &grim_assets::ProceduralTexture,
    path: &str,
    frames: u32,
) -> Result<(), String> {
    let lib = grim_package::Library::open(grim_testkit::require_game_dir());
    let palette = match loaded.base.property(pkg, "Palette", 0) {
        Some(grim_package::property::PropertyValue::Object(r)) => match lib.resolve(&std::sync::Arc::new(pkg.clone()), *r) {
            Ok(h) => match grim_assets::load_export(&h.package, h.export).map(|l| l.asset) {
                Ok(grim_assets::Asset::Palette(pal)) => pal.colors.iter().map(|c| [c.r, c.g, c.b, c.a]).collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let byte = |name: &str| match loaded.base.property(pkg, name, 0) {
        Some(grim_package::property::PropertyValue::Byte(b)) => *b,
        _ => 0,
    };
    let settings = grim_world::procedural::Settings {
        render_heat: byte("RenderHeat"),
        rising: loaded.base.bool_property(pkg, "bRising").unwrap_or(false),
        parametric: loaded.base.bool_property(pkg, "bParametric").unwrap_or(false),
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
    let count = loaded.base.int_property(pkg, "NumDrops").unwrap_or(0).max(0) as u32;
    let drops = (0..count)
        .filter_map(|d| {
            let grim_package::property::PropertyValue::Struct { value, .. } = loaded.base.property(pkg, "Drops", d)? else {
                return None;
            };
            let grim_package::property::StructValue::Unparsed(bytes) = value else { return None };
            let b = &bytes.bytes;
            (b.len() >= 8).then(|| grim_world::procedural::Drop { kind: b[0], depth: b[1], x: b[2], y: b[3], rate: b[7] })
        })
        .collect();
    // What a wet texture bends, so the pictures show the effect itself and not only the
    // surface it bends with.
    let source = match loaded.base.property(pkg, "SourceTexture", 0) {
        Some(grim_package::property::PropertyValue::Object(r)) => (|| {
            let handle = lib.resolve(&std::sync::Arc::new(pkg.clone()), *r).ok()?;
            let obj = grim_assets::load_export(&handle.package, handle.export).ok()?;
            let grim_assets::Asset::Texture(t) = &obj.asset else { return None };
            let palette = match obj.base.property(&handle.package, "Palette", 0) {
                Some(grim_package::property::PropertyValue::Object(pr)) => {
                    let h = lib.resolve(&handle.package, *pr).ok()?;
                    match grim_assets::load_export(&h.package, h.export).ok()?.asset {
                        grim_assets::Asset::Palette(pal) => Some(pal),
                        _ => None,
                    }
                }
                _ => None,
            };
            grim_assets::texture::decode_mip(t.mips.first()?, t.format, palette.as_ref(), true).ok()
        })(),
        _ => None,
    };
    let mut running = grim_world::procedural::Procedural::new(p, settings, palette, drops, source).ok_or("cannot run it")?;
    // One picture a second in, or a run of them a frame apart, to see how it moves.
    for frame in 0..frames.max(1) {
        for _ in 0..if frame == 0 { 35 } else { 1 } {
            running.tick(1.0 / 35.0);
        }
        let rgba = running.rgba();
        let mut ppm = format!("P6\n{} {}\n255\n", running.width, running.height).into_bytes();
        ppm.extend(rgba.chunks(4).flat_map(|c| [c[0], c[1], c[2]]));
        let at = if frames <= 1 { path.to_string() } else { format!("{path}-{frame:04}.ppm") };
        std::fs::write(at, ppm).map_err(|e| e.to_string())?;
    }
    out!("  after a second: {} drops, surface {:?}", running.drop_count(), running.wave_range());
    Ok(())
}

/// Where a mesh sits on the actor that carries it: `Origin` and `RotOrigin` are what the
/// artist moved it by, `Scale` what it is stored at, and the box is what it fills.
fn mesh(pkg: &Package, e: &str) -> Result<(), String> {
    let export = find_export(pkg, e)?;
    let loaded = grim_assets::load_export(pkg, export).map_err(|e| e.to_string())?;
    let lod = match &loaded.asset {
        grim_assets::Asset::LodMesh(m) => &m.mesh,
        grim_assets::Asset::SkeletalMesh(m) => &m.lod.mesh,
        _ => return Err(format!("{e} is not a mesh")),
    };
    let b = lod.bounding_box;
    out!("  origin ({:.2}, {:.2}, {:.2})", lod.origin.x, lod.origin.y, lod.origin.z);
    out!("  rot origin (pitch {}, yaw {}, roll {})", lod.rot_origin.pitch, lod.rot_origin.yaw, lod.rot_origin.roll);
    out!("  scale ({:.4}, {:.4}, {:.4})", lod.scale.x, lod.scale.y, lod.scale.z);
    out!(
        "  box x {:.1}..{:.1} y {:.1}..{:.1} z {:.1}..{:.1}, middle ({:.1}, {:.1}, {:.1})",
        b.min.x, b.max.x, b.min.y, b.max.y, b.min.z, b.max.z,
        (b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0, (b.min.z + b.max.z) / 2.0
    );
    let sp = lod.primitive.bounding_sphere;
    out!("  sphere {sp:?}");
    let p = lod.primitive.bounding_box;
    out!(
        "  primitive box x {:.1}..{:.1} y {:.1}..{:.1} z {:.1}..{:.1}, middle ({:.1}, {:.1}, {:.1})",
        p.min.x, p.max.x, p.min.y, p.max.y, p.min.z, p.max.z,
        (p.min.x + p.max.x) / 2.0, (p.min.y + p.max.y) / 2.0, (p.min.z + p.max.z) / 2.0
    );
    out!("  {} points, {} frames", lod.frame_verts, lod.anim_frames);
    // How the points are spread down the mesh: what is lowest is not always what it stands on.
    let points: Vec<f32> = match &loaded.asset {
        grim_assets::Asset::SkeletalMesh(m) => m.points.iter().map(|p| p.z).collect(),
        grim_assets::Asset::LodMesh(_) => Vec::new(),
        _ => Vec::new(),
    };
    if let grim_assets::Asset::SkeletalMesh(m) = &loaded.asset {
        for (i, b) in m.ref_skeleton.iter().take(3).enumerate() {
            out!(
                "  bone {i} {}: at ({:.2}, {:.2}, {:.2}) turned {:?} parent {}",
                pkg.name(b.name), b.bone_pos.position.x, b.bone_pos.position.y, b.bone_pos.position.z, b.bone_pos.orientation, b.parent_index
            );
        }
    }
    if !points.is_empty() {
        let mut low: Vec<f32> = points.clone();
        low.sort_by(f32::total_cmp);
        out!("  lowest points: {:?}", low.iter().take(12).map(|z| (z * 10.0).round() / 10.0).collect::<Vec<_>>());
        let (lo, hi) = (points.iter().cloned().fold(f32::MAX, f32::min), points.iter().cloned().fold(f32::MIN, f32::max));
        let step = (hi - lo) / 10.0;
        for i in 0..10 {
            let (a, b) = (lo + step * i as f32, lo + step * (i + 1) as f32);
            let n = points.iter().filter(|&&z| z >= a && z < b).count();
            out!("    z {a:6.1}..{b:6.1}: {n} points");
        }
    }
    Ok(())
}

/// Functions of a package whose bytecode names an object of it. The search is over the
/// decoded statements as they print, which is what `grim code` shows.
fn refs(pkg: &Package, name: &str) -> Result<(), String> {
    // The object may be one of the package's own, or one it imports from another; and a call
    // to it may go through its name rather than the object, which is how a virtual one does.
    let mut tokens: Vec<String> = Vec::new();
    if let Ok(i) = find_export(pkg, name) {
        tokens.push(format!("Export({i})"));
    }
    if let Some(i) =
        (0..pkg.imports.len() as u32).find(|&i| pkg.object_name(grim_package::ObjectRef::Import(i)).eq_ignore_ascii_case(name))
    {
        tokens.push(format!("Import({i})"));
    }
    if let Some(i) = (0..pkg.names.len()).find(|&i| pkg.names[i].name.eq_ignore_ascii_case(name)) {
        tokens.push(format!("NameIndex({i})"));
    }
    if tokens.is_empty() {
        return Err(format!("{name} is not in {}", pkg.name));
    }
    for i in 0..pkg.exports.len() as u32 {
        let Ok(loaded) = grim_assets::load_export(pkg, i) else { continue };
        // A function is not the only thing that carries code: a state has its own, and so
        // does a class, which is where a `Mount` called by name turned out to live.
        let script = match &loaded.asset {
            grim_assets::Asset::Function(f) => &f.struct_.script,
            grim_assets::Asset::State(st) => &st.struct_.script,
            grim_assets::Asset::Class(c) => &c.state.struct_.script,
            _ => continue,
        };
        let hit = script.iter().any(|st| {
            let text = format!("{:?}", st.expr);
            tokens.iter().any(|t| text.contains(t))
        });
        if hit {
            out!("{}", pkg.path_name(grim_package::ObjectRef::Export(i)));
        }
    }
    Ok(())
}

/// Prints a class's own script text, which the packages keep next to it.
fn script(pkg: &Package, e: &str) -> Result<(), String> {
    // Prefer the class of that name: properties and defaults share it.
    let class = (0..pkg.exports.len() as u32)
        .find(|&i| {
            pkg.class_name(grim_package::ObjectRef::Export(i)) == "Class"
                && pkg.object_name(grim_package::ObjectRef::Export(i)).eq_ignore_ascii_case(e)
        })
        .map_or_else(|| find_export(pkg, e), Ok)?;
    let text = (0..pkg.exports.len() as u32)
        .find(|&i| {
            pkg.class_name(grim_package::ObjectRef::Export(i)) == "TextBuffer"
                && pkg.outer(grim_package::ObjectRef::Export(i)) == grim_package::ObjectRef::Export(class)
        })
        .ok_or_else(|| format!("{e} has no script text"))?;
    match grim_assets::load_export(pkg, text).map_err(|x| x.to_string())?.asset {
        grim_assets::Asset::TextBuffer(t) => {
            out!("{}", t.text);
            Ok(())
        }
        _ => Err("not a text buffer".into()),
    }
}

fn obj(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let mut r = pkg.export_reader(i);
    let base = read_object_base(pkg, i, &mut r).map_err(|err| {
        format!("{err}\n{}", hexdump_around(pkg.bytes(), err.offset, 64, 64))
    })?;
    if let Some(sf) = &base.state_frame {
        out!("state frame: {sf:?}");
    }
    for p in base.properties.iter().flatten() {
        let idx = if p.array_index > 0 { format!("[{}]", p.array_index) } else { String::new() };
        out!("  {}{idx} = {:?}", pkg.name(p.name), p.value);
    }
    out!("rest: {} bytes from {:#x}", r.remaining(), r.offset());
    Ok(())
}

/// Every export of a package that carries a property of that name, and what it holds. It answers
/// whether the game ever sets something, and to what, without running it.
/// Every property the package's objects actually carry, and how many carry it. What a map
/// writes is what the engine has to answer for, so this is the list to check the engine
/// against: a name here that appears nowhere in `crates/` is something nobody reads.
fn prop_names(pkg: &Package) -> Result<(), String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for i in 0..pkg.exports.len() as u32 {
        let mut r = pkg.export_reader(i);
        let Ok(base) = read_object_base(pkg, i, &mut r) else { continue };
        for p in base.properties.iter().flatten() {
            *counts.entry(pkg.name(p.name).to_string()).or_default() += 1;
        }
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(n, c)| (c, n)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (count, name) in &rows {
        out!("{count:6} {name}");
    }
    out!("{} distinct properties", rows.len());
    Ok(())
}

fn props(pkg: &Package, want: &str) -> Result<(), String> {
    let mut found = 0;
    for i in 0..pkg.exports.len() as u32 {
        let mut r = pkg.export_reader(i);
        let Ok(base) = read_object_base(pkg, i, &mut r) else { continue };
        for p in base.properties.iter().flatten() {
            if !pkg.name(p.name).eq_ignore_ascii_case(want) {
                continue;
            }
            let idx = if p.array_index > 0 { format!("[{}]", p.array_index) } else { String::new() };
            out!("{i:6} {:<28} {}{idx} = {:?}", pkg.path_name(ObjectRef::Export(i)), pkg.name(p.name), p.value);
            found += 1;
        }
    }
    out!("{found} exports carry {want}");
    Ok(())
}

/// An animation's sequences and, per motion chunk, what each bone's track carries. Reading the
/// key counts is how a pose that comes out wrong gets traced back to the data.
/// Where the characters of a text sit in a font: page, corner and size of each.
fn font(pkg: &Package, e: &str, text: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let grim_assets::Asset::Font(f) = grim_assets::load_export(pkg, i).map_err(|e| e.to_string())?.asset else {
        return Err(format!("{e} is not a font"));
    };
    let per_page = f.characters_per_page.max(1) as usize;
    for c in text.chars() {
        let code = c as usize;
        let (page, at) = (code / per_page, code % per_page);
        match f.pages.get(page).and_then(|p| p.characters.get(at)) {
            Some(ch) => out!("{c:?} page {page} at ({}, {}) size {}x{}", ch.start_u, ch.start_v, ch.u_size, ch.v_size),
            None => out!("{c:?} not in the font"),
        }
    }
    Ok(())
}

/// What a brush model spans, in its own space: the box it was stored with and the corners of
/// the polygons the editor kept, which is what a mover is drawn and collides with.
fn brush(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let grim_assets::Asset::Model(m) = grim_assets::load_export(pkg, i).map_err(|e| e.to_string())?.asset else {
        return Err(format!("{e} is not a model"));
    };
    let b = m.primitive.bounding_box;
    out!("box {:?}..{:?} valid {}", [b.min.x, b.min.y, b.min.z], [b.max.x, b.max.y, b.max.z], b.is_valid);
    if let grim_package::ObjectRef::Export(p) = m.polys {
        if let Ok(grim_assets::Asset::Polys(polys)) = grim_assets::load_export(pkg, p).map(|l| l.asset) {
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for v in polys.polys.iter().flat_map(|p| p.vertices.iter()) {
                for (k, c) in [v.x, v.y, v.z].into_iter().enumerate() {
                    lo[k] = lo[k].min(c);
                    hi[k] = hi[k].max(c);
                }
            }
            out!("{} polygons over {lo:?}..{hi:?}", polys.polys.len());
            // The floors and ceilings: the flat polygons, by height and facing.
            let mut flats: Vec<(i32, i32)> = polys
                .polys
                .iter()
                .filter(|p| p.normal.z.abs() > 0.99 && !p.vertices.is_empty())
                .map(|p| ((p.vertices[0].z * 10.0).round() as i32, p.normal.z.signum() as i32))
                .collect();
            flats.sort();
            flats.dedup();
            for (z, up) in flats {
                out!("  flat at z {:.1} facing {}", z as f32 / 10.0, if up > 0 { "up" } else { "down" });
            }
        }
    }
    Ok(())
}

/// How big a skeletal mesh comes out posed: in its reference pose, then at the first frame of
/// each of its sequences.
fn posebox(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let grim_assets::Asset::SkeletalMesh(m) = grim_assets::load_export(pkg, i).map_err(|e| e.to_string())?.asset else {
        return Err(format!("{e} is not a skeletal mesh"));
    };
    let skin = grim_world::anim::Skin::new(pkg, &m);
    let lib = grim_package::Library::open(grim_testkit::require_game_dir());
    let handle = lib.resolve(&std::sync::Arc::new(pkg.clone()), m.default_animation).map_err(|e| e.to_string())?;
    let grim_assets::Asset::Animation(a) = grim_assets::load_export(&handle.package, handle.export).map_err(|e| e.to_string())?.asset else {
        return Err("its animation is not an Animation".into());
    };
    let tracks = grim_world::anim::Tracks::new(&handle.package, &a);
    let bones = skin.bind_local.len();
    let show = |label: &str, locals: &[Option<grim_world::anim::Joint>]| {
        let mut points = Vec::new();
        skin.deform(&skin.world(locals), &mut points);
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &points {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let size = [0, 1, 2].map(|k| (hi[k] - lo[k]) * [m.lod.mesh.scale.x, m.lod.mesh.scale.y, m.lod.mesh.scale.z][k].abs());
        out!("  {label:24} {:8.2} {:8.2} {:8.2}", size[0], size[1], size[2]);
    };
    // The packed vertices a mesh keeps from the vertex-animated meshes it derives from.
    let packed = &m.lod.mesh.verts;
    if !packed.is_empty() {
        let unpack = |v: u32| [(((v << 21) as i32) >> 21) as f32, (((v << 10) as i32) >> 21) as f32, ((v as i32) >> 22) as f32];
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in packed.iter().map(|&v| unpack(v)) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        out!("  packed: {} verts, {} a frame, {:?}..{:?}", packed.len(), m.lod.mesh.frame_verts, lo, hi);
    }
    show("reference", &vec![None; bones]);
    for (k, s) in a.anim_seqs.iter().enumerate() {
        show(handle.package.name(s.name), &tracks.locals(k, 0.0, bones, None));
    }
    Ok(())
}

fn anim(pkg: &Package, e: &str) -> Result<(), String> {
    let i = find_export(pkg, e)?;
    let a = match grim_assets::load_export(pkg, i).map_err(|e| e.to_string())?.asset {
        grim_assets::Asset::Animation(a) => a,
        // A mesh points at the animation it is posed with: show its own skeleton and follow it.
        grim_assets::Asset::SkeletalMesh(m) => {
            out!("{} bones in the mesh:", m.ref_skeleton.len());
            for (k, b) in m.ref_skeleton.iter().enumerate() {
                out!("  {k} {} parent {}", pkg.name(b.name), b.parent_index);
            }
            out!(
                "{} points, {} bone weights, {} local points, {} weight indices",
                m.points.len(),
                m.bone_weights.len(),
                m.local_points.len(),
                m.bone_weight_idx.len()
            );
            out!("animation: {}", pkg.object_name(m.default_animation));
            return Ok(());
        }
        _ => return Err(format!("{} is neither an Animation nor a SkeletalMesh", pkg.object_name(grim_package::ObjectRef::Export(i)))),
    };
    out!("{} bones in the animation:", a.ref_bones.len());
    for (k, b) in a.ref_bones.iter().enumerate() {
        out!("  {k} {} parent {}", pkg.name(b.name), b.parent_index);
    }
    out!("{} bones, {} moves, {} sequences", a.ref_bones.len(), a.moves.len(), a.anim_seqs.len());
    out!("pools: {} rotations, {} positions, {} frames", a.rot_keys.len(), a.pos_keys.len(), a.key_frames.len());
    for (i, s) in a.anim_seqs.iter().enumerate() {
        // How far the root bone travels over the sequence: an animation that carries the
        // actor itself, such as a climb, moves it there rather than in the pose.
        let root = a.moves.get(i).and_then(|m| {
            let t = m.tracks.first()?;
            let base: u32 = a.moves[..i].iter().flat_map(|m| m.tracks.iter()).map(|t| t.num_pos_keys).sum();
            let first = grim_assets::animation::decode_position(*a.pos_keys.get(base as usize)?, t.pos_scale);
            let last = grim_assets::animation::decode_position(
                *a.pos_keys.get((base + t.num_pos_keys.saturating_sub(1)) as usize)?,
                t.pos_scale,
            );
            Some([0, 1, 2].map(|k| last[k] - first[k]))
        });
        out!(
            "  sequence {i} {} frames {}..{} rate {} root move {:?}",
            pkg.name(s.name),
            s.start_frame,
            s.start_frame + s.num_frames,
            s.rate,
            root
        );
    }
    // The key pools are shared by every track of every move, one after another.
    let mut time_at = 0usize;
    let mut rot_at = 0usize;
    for (i, m) in a.moves.iter().enumerate() {
        out!("  move {i}: track time {} start bone {} flags {:#x} {} tracks", m.track_time, m.start_bone, m.flags, m.tracks.len());
        for (k, t) in m.tracks.iter().enumerate() {
            let bone = m.bone_indices.get(k).copied().unwrap_or(m.start_bone + k as i32);
            let name = a.ref_bones.get(bone.max(0) as usize).map(|b| pkg.name(b.name)).unwrap_or("?");
            // Where the keys of a track fall, in frames: the deltas added up. Which frame the
            // last key sits on is what says whether a sequence ends on its last key or runs on
            // past it, back round to the first.
            let deltas: Vec<u8> = (0..t.num_time_keys as usize).map(|j| a.key_frames.get(time_at + j).copied().unwrap_or(1)).collect();
            time_at += t.num_time_keys as usize;
            let first_rot = a.rot_keys.get(rot_at).copied();
            rot_at += t.num_rot_keys as usize;
            let mut at = 0u32;
            let times: Vec<u32> = deltas
                .iter()
                .map(|&d| {
                    at += d as u32;
                    at
                })
                .collect();
            out!(
                "    bone {bone} {name}: {} rot (first {first_rot:?}), {} pos (scale {}), {} time at {:?}{}",
                t.num_rot_keys,
                t.num_pos_keys,
                t.pos_scale,
                t.num_time_keys,
                &times[..times.len().min(4)],
                if times.len() > 4 { format!("..{:?}", times.last().unwrap()) } else { String::new() }
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct ClassStats {
    count: usize,
    bytes: u64,
    base_errors: usize,
    first_error: Option<String>,
    tail_bytes: u64,
}

fn survey(root: &Path) -> Result<(), String> {
    let files = grim_testkit::package_files(root);
    let mut classes: BTreeMap<String, ClassStats> = BTreeMap::new();
    let mut failed_pkgs = 0;
    let mut gap_bytes = 0usize;
    for f in &files {
        let rel = grim_testkit::display_path(root, f);
        let pkg = match load(f) {
            Ok(p) => p,
            Err(e) => {
                failed_pkgs += 1;
                out!("FAILED {e}");
                continue;
            }
        };
        let l = pkg.layout();
        let gaps: usize = l.gaps.iter().map(|(s, e)| e - s).sum();
        gap_bytes += gaps;
        if !l.gaps.is_empty() || !l.overlaps.is_empty() {
            out!("{rel}: {} gaps ({gaps} bytes), {} overlaps", l.gaps.len(), l.overlaps.len());
        }
        for i in 0..pkg.exports.len() as u32 {
            let class = pkg.class_name(ObjectRef::Export(i)).to_string();
            let st = classes.entry(class).or_default();
            st.count += 1;
            let r = pkg.export_reader(i);
            st.bytes += r.len() as u64;
            match grim_assets::load_export(&pkg, i) {
                Ok(o) => {
                    if let grim_assets::Asset::Unparsed(u) = &o.asset {
                        st.tail_bytes += u.len() as u64;
                    }
                }
                Err(e) => {
                    st.base_errors += 1;
                    if st.first_error.is_none() {
                        st.first_error = Some(format!("{rel} export {i} ({}): {e}", pkg.path_name(ObjectRef::Export(i))));
                    }
                }
            }
        }
    }
    out!("\n{} packages, {} failed to open; {} unexplained bytes between regions", files.len(), failed_pkgs, gap_bytes);
    out!("{:<28} {:>7} {:>12} {:>12} {:>7}", "class", "objects", "bytes", "unparsed", "errors");
    for (c, s) in &classes {
        out!("{c:<28} {:>7} {:>12} {:>12} {:>7}", s.count, s.bytes, s.tail_bytes, s.base_errors);
    }
    for (c, s) in &classes {
        if let Some(e) = &s.first_error {
            out!("[{c}] {e}");
        }
    }
    Ok(())
}

/// Classes whose instances are definitions (script metadata), not objects to instantiate.
const DEFINITION_CLASSES: &[&str] = &[
    "Class", "Function", "State", "Struct", "Enum", "Const", "TextBuffer", "ByteProperty",
    "IntProperty", "BoolProperty", "FloatProperty", "ObjectProperty", "ClassProperty",
    "NameProperty", "StrProperty", "StructProperty", "ArrayProperty",
];

fn objects(root: &Path) -> Result<(), String> {
    let lib = grim_package::Library::open(root);
    let mut world = grim_object::World::new(lib);
    let files = grim_testkit::package_files(root);
    let (mut classes, mut objects) = (0usize, 0usize);
    let mut errors: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for f in &files {
        let pkg = world.lib.load_path(f)?;
        for i in 0..pkg.exports.len() as u32 {
            let r = ObjectRef::Export(i);
            let class = pkg.class_name(r);
            let res = if class == "Class" {
                classes += 1;
                world.pkg_id(&pkg);
                let key = grim_object::ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).map(|_| ())
            } else if DEFINITION_CLASSES.contains(&class) {
                continue;
            } else {
                objects += 1;
                world.instantiate(&pkg, i).map(|_| ())
            };
            if let Err(e) = res {
                // Group by the message without offsets and object paths.
                let kind: String = e.0.rsplit(": ").next().unwrap_or(&e.0).chars().filter(|c| !c.is_ascii_digit()).collect();
                let entry = errors.entry(kind).or_insert((0, e.0.clone()));
                entry.0 += 1;
            }
        }
    }
    out!("{} packages: {classes} classes linked, {objects} objects instantiated", files.len());
    out!("{} classes, {} structs, {} enums in the world", world.classes.len(), world.structs.len(), world.enums.len());
    out!("intrinsic classes: {:?}", world.intrinsic_classes());
    out!("{} error kinds", errors.len());
    for (k, (n, example)) in &errors {
        out!("{n:6} {k}\n       e.g. {example}");
    }
    Ok(())
}

fn type_name(w: &grim_object::World, t: &grim_object::PropType) -> String {
    use grim_object::PropType::*;
    match t {
        Byte(_) => "byte".into(),
        Int => "int".into(),
        Bool => "bool".into(),
        Float => "float".into(),
        Object(_) => "object".into(),
        Class(_) => "class".into(),
        Name => "name".into(),
        Str => "string".into(),
        Struct(s) => w.names.str(w.structs[s.0 as usize].name).to_string(),
        Array(i) => format!("array<{}>", type_name(w, &i.ty)),
    }
}

fn link_all(root: &Path) -> Result<grim_object::World, String> {
    let mut world = grim_object::World::new(grim_package::Library::open(root));
    for f in grim_testkit::package_files(root) {
        let pkg = world.lib.load_path(&f)?;
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = grim_object::ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).map_err(|e| e.0)?;
            }
        }
    }
    Ok(world)
}

fn funcs(root: &Path, pattern: &str) -> Result<(), String> {
    let world = link_all(root)?;
    let pat = pattern.to_lowercase();
    for e in &world.enums {
        if world.names.str(e.name).to_lowercase().contains(&pat) {
            let values: Vec<&str> = e.values.iter().map(|&v| world.names.str(v)).collect();
            out!("enum {} = {values:?}", world.path(e.key));
        }
    }
    for f in &world.functions {
        let path = world.path(f.key);
        if !path.to_lowercase().contains(&pat) {
            continue;
        }
        let params: Vec<String> = f.params.iter().map(|&p| {
            let l = &f.locals[p];
            let out = if l.flags & grim_object::cpf::OUT_PARM != 0 { "out " } else { "" };
            let opt = if l.flags & grim_object::cpf::OPTIONAL_PARM != 0 { "optional " } else { "" };
            format!("{opt}{out}{} {}", type_name(&world, &l.ty), world.names.str(l.name))
        }).collect();
        let ret = f.ret.map(|r| type_name(&world, &f.locals[r].ty)).unwrap_or_else(|| "void".into());
        out!("{path}: {ret} ({}) flags={:#x} native={}", params.join(", "), f.flags, f.native);
    }
    // Class chains for matching class names.
    for (i, c) in world.classes.iter().enumerate() {
        if !world.names.str(c.name).to_lowercase().contains(&pat) {
            continue;
        }
        let mut chain = Vec::new();
        let mut cur = Some(grim_object::ClassId(i as u32));
        while let Some(x) = cur {
            chain.push(world.names.str(world.class(x).name).to_string());
            cur = world.class(x).super_;
        }
        out!("class {} probe mask {:#x} ignore mask {:#x}", chain.join(" <- "), c.probe_mask, c.ignore_mask);
    }
    Ok(())
}

fn value_text(world: &grim_object::World, v: &grim_object::Value) -> String {
    use grim_object::Value::*;
    match v {
        Object(Some(k)) => world.path(*k),
        Object(None) => "None".into(),
        Name(n) => world.names.str(*n).to_string(),
        Str(s) => format!("{s:?}"),
        Struct(f) => format!("({})", f.iter().map(|x| value_text(world, x)).collect::<Vec<_>>().join(", ")),
        Array(a) => format!("[{}]", a.iter().map(|x| value_text(world, x)).collect::<Vec<_>>().join(", ")),
        Byte(b) => b.to_string(),
        Int(i) => i.to_string(),
        Bool(b) => b.to_string(),
        Float(x) => format!("{x}"),
    }
}

fn class_defaults(root: &Path, name: &str) -> Result<(), String> {
    let world = link_all(root)?;
    let Some(id) = (0..world.classes.len() as u32)
        .map(grim_object::ClassId)
        .find(|&c| world.names.str(world.class(c).name).eq_ignore_ascii_case(name))
    else {
        return Err(format!("class {name} not found"));
    };
    let class = world.class(id);
    let mut chain = Vec::new();
    let mut cur = Some(id);
    while let Some(x) = cur {
        chain.push(world.names.str(world.class(x).name).to_string());
        cur = world.class(x).super_;
    }
    out!("class {}", chain.join(" <- "));
    // Which properties survive a level change is a flag on the property itself.
    let travel: Vec<&str> = class
        .props
        .iter()
        .filter(|p| p.flags & grim_object::types::cpf::TRAVEL != 0)
        .map(|p| world.names.str(p.name))
        .collect();
    if !travel.is_empty() {
        out!("  travels: {}", travel.join(", "));
    }
    for p in &class.props {
        for i in 0..p.array_dim {
            let v = &class.defaults[(p.slot + i) as usize];
            let text = value_text(&world, v);
            if matches!(text.as_str(), "0" | "None" | "false" | "\"\"" | "[]") || text.chars().all(|c| "(0, .)".contains(c)) {
                continue;
            }
            let index = if p.array_dim > 1 { format!("[{i}]") } else { String::new() };
            out!("  {}{index} = {text}", world.names.str(p.name));
        }
    }
    Ok(())
}

/// Actors whose mesh has no usable texture, grouped by class.
fn skins(root: &Path, map: &str) -> Result<(), String> {
    use grim_assets::Asset;
    let mut world = link_all(root)?;
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let pkg = world.lib.load_path(&path)?;
    let mesh_name = world.names.intern("Mesh");
    let skin_names: Vec<_> = ["Skin", "MultiSkins"].iter().map(|n| world.names.intern(n)).collect();
    let mut missing: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for i in 0..pkg.exports.len() as u32 {
        if pkg.class_name(ObjectRef::Export(i)) == "Class" {
            continue;
        }
        let Ok(obj) = world.instantiate(&pkg, i) else { continue };
        let class = world.class(obj.class).clone();
        let Some(mesh_prop) = class.prop(mesh_name) else { continue };
        let grim_object::Value::Object(Some(mesh_key)) = obj.values[mesh_prop.slot as usize] else { continue };
        let mpkg = world.package(mesh_key.package).clone();
        let Ok(loaded) = grim_assets::load_export(&mpkg, mesh_key.export) else { continue };
        let textures = match &loaded.asset {
            Asset::SkeletalMesh(m) => m.lod.mesh.textures.clone(),
            Asset::LodMesh(m) => m.mesh.textures.clone(),
            _ => continue,
        };
        let resolved = textures.iter().filter(|t| world.lib.resolve(&mpkg, **t).is_ok()).count();
        if resolved > 0 {
            continue;
        }
        // No mesh texture: look for the actor's own skins.
        let skins: Vec<String> = skin_names
            .iter()
            .filter_map(|&n| class.prop(n))
            .flat_map(|p| (0..p.array_dim).map(move |i| (p, i)))
            .filter_map(|(p, i)| match &obj.values[(p.slot + i) as usize] {
                grim_object::Value::Object(Some(k)) => Some(world.path(*k)),
                _ => None,
            })
            .collect();
        let name = world.names.str(class.name).to_string();
        let entry = missing.entry(name).or_insert((0, format!("{} skins: {skins:?}", world.path(mesh_key))));
        entry.0 += 1;
    }
    out!("actors whose mesh has no texture:");
    for (class, (n, detail)) in &missing {
        out!("{n:5} {class:24} {detail}");
    }
    Ok(())
}

type Calls<K> = BTreeMap<K, u32>;

/// Which functions a piece of code calls and how often: native tokens by index, a final call by
/// the object it names, a virtual one by name. The natives declared without an index are called
/// like any other function, so they can only be recognized through the last two.
fn native_calls(e: &grim_assets::bytecode::Expr, by_index: &mut Calls<u16>, by_name: &mut Calls<u32>, by_ref: &mut Calls<ObjectRef>) {
    use grim_assets::bytecode::Expr::*;
    match e {
        Native { index, args } => {
            *by_index.entry(*index).or_default() += 1;
            for a in args {
                native_calls(a, by_index, by_name, by_ref);
            }
        }
        VirtualFunction { name, args } | GlobalFunction { name, args } => {
            *by_name.entry(name.0).or_default() += 1;
            for a in args {
                native_calls(a, by_index, by_name, by_ref);
            }
        }
        FinalFunction { function, args } => {
            *by_ref.entry(*function).or_default() += 1;
            for a in args {
                native_calls(a, by_index, by_name, by_ref);
            }
        }
        Return(a) | GotoLabel(a) | EatString(a) | Skip { expr: a, .. } | BoolVariable(a) | DynamicCast { expr: a, .. }
        | MetaCast { expr: a, .. } | StructMember { expr: a, .. } | DynArrayLength(a) | Cast { expr: a, .. }
        | Switch { value: a, .. } | Assert { cond: a, .. } | JumpIfNot { cond: a, .. } | Iterator { expr: a, .. } => {
            native_calls(a, by_index, by_name, by_ref)
        }
        Case { value: Some(a), .. } => native_calls(a, by_index, by_name, by_ref),
        Let(a, b) | LetBool(a, b) | DynArrayElement { index: a, array: b } | ArrayElement { index: a, array: b }
        | ClassContext { object: a, expr: b, .. } | Context { object: a, expr: b, .. }
        | StructCmpEq { a, b, .. } | StructCmpNe { a, b, .. } => {
            native_calls(a, by_index, by_name, by_ref);
            native_calls(b, by_index, by_name, by_ref);
        }
        New { outer, name, flags, class } => {
            for a in [outer, name, flags, class] {
                native_calls(a, by_index, by_name, by_ref);
            }
        }
        _ => {}
    }
}

fn natives(root: &Path) -> Result<(), String> {
    let world = link_all(root)?;
    // What the scripts ask for, counted over every function and state of every package: a
    // native nothing calls is not a gap, and one called from a hundred places is where to look
    // first.
    let mut by_index: Calls<u16> = BTreeMap::new();
    let mut by_key: Calls<grim_object::ObjectKey> = BTreeMap::new();
    let mut by_name: Calls<u32> = BTreeMap::new();
    let mut index_callers: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    let mut key_callers: BTreeMap<grim_object::ObjectKey, Vec<String>> = BTreeMap::new();
    let mut name_callers: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    // A name a script also defines cannot tell a native call from an ordinary one, so only the
    // names no script function carries are counted by name.
    let mut script_names: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut code: Vec<(String, std::sync::Arc<grim_object::Code>)> = Vec::new();
    for f in &world.functions {
        code.push((world.path(f.key), f.code.clone()));
        if f.flags & grim_object::func::NATIVE == 0 {
            script_names.insert(f.name.0);
        }
    }
    for s in &world.states {
        code.push((world.path(s.key), s.code.clone()));
    }
    let mut world = world;
    for (path, c) in code {
        let (mut mine, mut named, mut refs) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        for s in &c.statements {
            native_calls(&s.expr, &mut mine, &mut named, &mut refs);
        }
        for (k, n) in mine {
            *by_index.entry(k).or_default() += n;
            index_callers.entry(k).or_default().push(path.clone());
        }
        // A name in bytecode indexes the package's own name table: it has to be interned to
        // mean the same thing as a function's name.
        if let Some(pkg) = c.package {
            for (k, n) in named {
                let pkg = world.package(pkg).clone();
                let id = world.name(&pkg, grim_package::NameIndex(k));
                *by_name.entry(id.0).or_default() += n;
                name_callers.entry(id.0).or_default().push(path.clone());
            }
            // A final call names the function it goes to, so it counts against exactly that one.
            for (r, n) in refs {
                if let Ok(Some(key)) = world.resolve_in(pkg, r) {
                    *by_key.entry(key).or_default() += n;
                    key_callers.entry(key).or_default().push(path.clone());
                }
            }
        }
    }
    let world = world;
    let engine = grim_vm::natives::Natives::standard();
    let mut rows = Vec::new();
    for f in &world.functions {
        if f.flags & grim_object::func::NATIVE == 0 {
            continue;
        }
        let params: Vec<String> = f.params.iter().map(|&p| {
            let l = &f.locals[p];
            let out = if l.flags & grim_object::cpf::OUT_PARM != 0 { "out " } else { "" };
            let opt = if l.flags & grim_object::cpf::OPTIONAL_PARM != 0 { "optional " } else { "" };
            format!("{opt}{out}{} {}", type_name(&world, &l.ty), world.names.str(l.name))
        }).collect();
        let ret = f.ret.map(|r| type_name(&world, &f.locals[r].ty)).unwrap_or_else(|| "void".into());
        let kind = if f.flags & grim_object::func::OPERATOR != 0 { "op" } else if f.flags & grim_object::func::LATENT != 0 { "latent" } else if f.flags & grim_object::func::ITERATOR != 0 { "iter" } else { "" };
        let path = world.path(f.key);
        // An indexed native is counted by its token. One called like an ordinary function is
        // counted by the final calls that name it, plus the virtual calls by a name no script
        // function shares.
        let (uses, mut callers) = if f.native != 0 {
            (by_index.get(&f.native).copied().unwrap_or(0), index_callers.get(&f.native).cloned().unwrap_or_default())
        } else {
            let mut callers = key_callers.get(&f.key).cloned().unwrap_or_default();
            let mut uses = by_key.get(&f.key).copied().unwrap_or(0);
            if !script_names.contains(&f.name.0) {
                uses += by_name.get(&f.name.0).copied().unwrap_or(0);
                callers.extend(name_callers.get(&f.name.0).cloned().unwrap_or_default());
            }
            (uses, callers)
        };
        callers.sort();
        callers.dedup();

        let have = engine.get(&path).is_some();
        rows.push((f.native, path, format!("{ret} {}({})", world.names.str(f.name), params.join(", ")), kind, uses, have, callers));
    }
    rows.sort();
    let indexed = rows.iter().filter(|r| r.0 != 0).count();
    let missing: Vec<_> = rows.iter().filter(|r| !r.5 && r.4 > 0).collect();
    out!("{} native functions ({indexed} with index, {} by name)", rows.len(), rows.len() - indexed);
    out!("{} of them are called by the scripts and the engine does not have them:", missing.len());
    for r in &missing {
        out!("  {:5} {:<46} {:3} uses  {}", r.0, r.1, r.4, r.6.join(" "));
    }
    let declared: std::collections::HashSet<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    let stale: Vec<&str> = engine.paths().filter(|p| !declared.contains(p)).collect();
    if !stale.is_empty() {
        out!("{} implementations are registered under a path no class declares native:", stale.len());
        for p in stale {
            out!("  {p}");
        }
    }
    out!("index  kind   path                                               have uses signature");
    for (n, path, sig, kind, uses, have, callers) in rows {
        out!("{n:5} {kind:6} {path:<50} {:4} {uses:4} {sig}", if have { "yes" } else { "no" });
        for caller in callers {
            out!("        called from {caller}");
        }
    }
    Ok(())
}

fn describe(vm: &mut grim_vm::Vm, v: &grim_object::Value) -> String {
    match v {
        grim_object::Value::Object(Some(k)) => {
            let name = vm.object_name(*k);
            match vm.class_of(*k) {
                Ok(c) => format!("{name} ({})", vm.world.names.str(vm.world.class(c).name)),
                Err(_) => name,
            }
        }
        other => format!("{other:?}"),
    }
}

/// Every class of every package of the game, linked: what the VM and the save streams need to
/// know what a name means.
fn linked_world(root: &Path) -> Result<grim_object::World, String> {
    let mut world = grim_object::World::new(grim_package::Library::open(root));
    for f in grim_testkit::package_files(root) {
        let pkg = world.lib.load_path(&f)?;
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = grim_object::ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).map_err(|e| e.0)?;
            }
        }
    }
    Ok(world)
}

/// A slot's `<Map>_pa.usa`: what it holds, and how it compares with what the classes declare.
fn persistent(root: &Path, file: &Path, only: Option<&str>) -> Result<(), String> {
    let world = linked_world(root)?;
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let level = grim_save::read(&bytes, &world).map_err(|e| format!("{}: {e}", file.display()))?;
    out!("{} level {:?} header {:?} {} actors", file.display(), level.name, level.unknown, level.actors.len());
    for actor in &level.actors {
        if let Some(name) = only {
            if !actor.name.eq_ignore_ascii_case(name) {
                continue;
            }
        }
        let class = world
            .classes
            .iter()
            .position(|c| world.names.str(c.name).eq_ignore_ascii_case(&actor.class))
            .map(|i| &world.classes[i]);
        // The stream holds every property but the ones the engine owns: what is missing from a
        // record, or over and above it, is worth knowing about.
        const OWNED: u32 = grim_object::types::cpf::NATIVE | grim_object::types::cpf::TRANSIENT;
        // Dynamic arrays are left out too: the only one these classes have (`Actor.AuxAnims`)
        // is in no record of the six saves. Hypothesis: the writer does not handle them.
        let expected = class.map(|c| {
            c.props
                .iter()
                .filter(|p| p.flags & OWNED == 0 && !matches!(p.ty, grim_object::PropType::Array(_)))
                .map(|p| p.array_dim as usize)
                .sum::<usize>()
        });
        match expected {
            Some(n) => out!(
                "{:28} {:24} {:4} slots, {:4} expected{}",
                actor.name,
                actor.class,
                actor.props.len(),
                n,
                if n == actor.props.len() { "" } else { "   <-- does not match" }
            ),
            None => out!("{:28} {:24} {:4} properties (class not found)", actor.name, actor.class, actor.props.len()),
        }
        if only.is_none() {
            continue;
        }
        for p in &actor.props {
            let index = if p.index == 0 { String::new() } else { format!("[{}]", p.index) };
            out!("   {}{index} = {}", p.name, describe_saved(&p.value));
        }
        if let Some(c) = class {
            let saved: std::collections::BTreeSet<String> = actor.props.iter().map(|p| p.name.to_ascii_lowercase()).collect();
            let missing: Vec<String> = c
                .props
                .iter()
                .filter(|p| !saved.contains(&world.names.str(p.name).to_ascii_lowercase()))
                .map(|p| format!("{} (flags {:#x})", world.names.str(p.name), p.flags))
                .collect();
            out!("not in the stream ({}): {}", missing.len(), missing.join(", "));
        }
    }
    Ok(())
}

fn describe_saved(v: &grim_save::Saved) -> String {
    use grim_save::Saved;
    match v {
        Saved::Byte(b) => format!("{b}"),
        Saved::Int(i) => format!("{i}"),
        Saved::Bool(b) => format!("{b}"),
        Saved::Float(f) => format!("{f}"),
        Saved::Object => "<object, not saved>".to_string(),
        Saved::Class => "<class, not saved>".to_string(),
        Saved::Name(n) => format!("'{n}'"),
        Saved::Str { text, unicode } => format!("{text:?}{}", if *unicode { " (utf-16)" } else { "" }),
        Saved::Struct { ty, fields } => {
            format!("{ty}({})", fields.iter().map(describe_saved).collect::<Vec<_>>().join(", "))
        }
    }
}

/// Checks the shadow bitmaps against the level's own geometry: for every texel of a surface,
/// whether the line to the light is blocked should agree with the bit the map stores. It is
/// also checked with the bit planes shifted by one, which is what would show if the planes
/// were being paired with the wrong lights.
fn lightcheck(root: &Path, map: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let pkg = std::sync::Arc::new(load(&path)?);
    let (_, model) = grim_world::scene::level_model(&pkg).ok_or("map has no Model")?;
    let bsp = grim_world::Bsp::new(&model);
    // Where each light of the map stands, by the reference the model keeps.
    let mut places: std::collections::HashMap<u32, [f32; 3]> = std::collections::HashMap::new();
    for i in 0..pkg.exports.len() as u32 {
        let Ok(loaded) = grim_assets::load_export(&pkg, i) else { continue };
        if let Some(grim_package::property::PropertyValue::Struct { value, .. }) = loaded.base.property(&pkg, "Location", 0) {
            if let grim_package::property::StructValue::Vector(v) = value {
                places.insert(i, [v.x, v.y, v.z]);
            }
        }
    }
    // How far each light reaches, so a texel it never touches is not counted against it.
    let mut radii: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
    for (&e, _) in places.iter() {
        let Ok(loaded) = grim_assets::load_export(&pkg, e) else { continue };
        let r = match loaded.base.property(&pkg, "LightRadius", 0) {
            Some(grim_package::property::PropertyValue::Byte(b)) => *b as f32,
            _ => 64.0,
        };
        radii.insert(e, r * 25.0);
    }
    // The middle of each surface's polygon, to keep the check to texels that are on it.
    let pieces = grim_world::lightmap::Pieces::Surfs(&model);
    let (mut same, mut total) = ([0u64; 3], 0u64);
    // The movers of the map, each as the box its brush fills where it stands: what the editor
    // may or may not have taken into account when it worked the shadows out.
    let mut mover_boxes: Vec<(u32, [f32; 3], [f32; 3], u64, u64)> = Vec::new();
    for i in 0..pkg.exports.len() as u32 {
        let Ok(loaded) = grim_assets::load_export(&pkg, i) else { continue };
        // Movers only: the level's own brushes are already part of the tree the trace uses.
        if !pkg.class_name(grim_package::ObjectRef::Export(i)).to_lowercase().contains("mover") {
            continue;
        }
        let Some(grim_package::property::PropertyValue::Object(brush)) = loaded.base.property(&pkg, "Brush", 0) else { continue };
        let grim_package::ObjectRef::Export(b) = brush else { continue };
        let Some(at) = places.get(&i) else { continue };
        let Ok(loaded_brush) = grim_assets::load_export(&pkg, *b) else { continue };
        let grim_assets::Asset::Model(m) = loaded_brush.asset else { continue };
        if !m.primitive.bounding_box.is_valid {
            continue;
        }
        let bb = m.primitive.bounding_box;
        mover_boxes.push((
            i,
            [at[0] + bb.min.x, at[1] + bb.min.y, at[2] + bb.min.z],
            [at[0] + bb.max.x, at[1] + bb.max.y, at[2] + bb.max.z],
            0,
            0,
        ));
    }
    let (mut dark_but_open, mut dark_by_mover, mut lit_but_blocked) = (0u64, 0u64, 0u64);
    for piece in 0..pieces.count() {
        let Some(lm) = pieces.map(piece) else { continue };
        let (w, h) = (lm.u_clamp.max(0) as usize, lm.v_clamp.max(0) as usize);
        if w == 0 || h == 0 || lm.i_light_actors < 0 {
            continue;
        }
        let stride = w.div_ceil(8);
        let mut lights: Vec<([f32; 3], f32)> = Vec::new();
        let mut k = lm.i_light_actors as usize;
        while let Some(r) = model.lights.get(k) {
            match r {
                grim_package::ObjectRef::Export(e) => match places.get(e) {
                    Some(p) => lights.push((*p, radii.get(e).copied().unwrap_or(1600.0))),
                    None => break,
                },
                _ => break,
            }
            k += 1;
        }
        for (l, (light, radius)) in lights.iter().enumerate() {
            for y in (0..h).step_by(3) {
                for x in (0..w).step_by(3) {
                    let Some(at) = pieces.position(piece, [x as f32 + 0.5, y as f32 + 0.5]) else { continue };
                    // A hair off the surface, so the surface itself is not what blocks it.
                    let normal = pieces.normal(piece);
                    let from = [0, 1, 2].map(|i| at[i] + normal[i] * 2.0);
                    // Beyond its reach a light is off whatever stands in the way, and that is
                    // not what this is checking.
                    let away = (0..3).map(|i| (light[i] - at[i]).powi(2)).sum::<f32>().sqrt();
                    if away > *radius {
                        continue;
                    }
                    let blocked = bsp.line_check(from, *light).is_some();
                    for (slot, shift) in [(0usize, 0i64), (1, 1), (2, -1)] {
                        let plane = l as i64 + shift;
                        if plane < 0 || plane as usize >= lights.len() {
                            continue;
                        }
                        let start = lm.data_offset.max(0) as usize + plane as usize * stride * h;
                        let bit = model.light_bits.get(start + y * stride + x / 8).is_some_and(|b| b >> (x % 8) & 1 != 0);
                        if bit != blocked {
                            same[slot] += 1;
                        } else if slot == 0 {
                            // Where they disagree: the map says shadow and the level does not
                            // block it (a mover may), or the other way round.
                            if !bit {
                                dark_but_open += 1;
                                let mut blocked_by_mover = false;
                                for m in mover_boxes.iter_mut() {
                                    let hits = (0..=16).any(|t| {
                                        let f = t as f32 / 16.0;
                                        let p = [0, 1, 2].map(|i| from[i] + (light[i] - from[i]) * f);
                                        (0..3).all(|i| p[i] >= m.1[i] && p[i] <= m.2[i])
                                    });
                                    if hits {
                                        blocked_by_mover = true;
                                        m.3 += 1;
                                    }
                                }
                                if blocked_by_mover {
                                    dark_by_mover += 1;
                                }
                            } else {
                                lit_but_blocked += 1;
                            }
                        } else if slot == 0 && bit {
                            // The map says lit: any mover standing in the way was ignored when
                            // the shadows were worked out.
                            for m in mover_boxes.iter_mut() {
                                let hits = (0..=16).any(|t| {
                                    let f = t as f32 / 16.0;
                                    let p = [0, 1, 2].map(|i| from[i] + (light[i] - from[i]) * f);
                                    (0..3).all(|i| p[i] >= m.1[i] && p[i] <= m.2[i])
                                });
                                if hits {
                                    m.4 += 1;
                                }
                            }
                        }
                    }
                    total += 1;
                }
            }
        }
    }
    out!("{total} texel-light samples, {} movers", mover_boxes.len());
    out!(
        "  disagreements: {dark_but_open} say shadow with the level open (of those, {dark_by_mover} are blocked by a mover), {lit_but_blocked} say lit with the level in the way"
    );
    for (name, n) in [("as read", same[0]), ("shifted +1", same[1]), ("shifted -1", same[2])] {
        out!("  {name}: {:.1}% agree with a trace through the level", n as f64 * 100.0 / total.max(1) as f64);
    }
    // Per mover: how many of the texels it stands in the way of are shadowed in the map, and
    // how many are lit. A mover the editor took into account shadows what it covers.
    let mut movers: Vec<_> = mover_boxes.iter().filter(|m| m.3 + m.4 > 20).collect();
    movers.sort_by_key(|m| std::cmp::Reverse(m.3 + m.4));
    out!("movers that stand in the light of something (shadowed / lit texels behind them):");
    for m in movers.iter().take(12) {
        let name = pkg.object_name(grim_package::ObjectRef::Export(m.0));
        out!("  {name}: {} shadowed, {} lit ({:.0}% taken into account)", m.3, m.4, m.3 as f64 * 100.0 / (m.3 + m.4) as f64);
    }
    Ok(())
}

/// The shadow bitmaps a map ships with: one bit per lightmap texel per light reaching the
/// surface, worked out by the editor when the map was built. What is checked here is that the
/// whole array is accounted for, which is what says the layout is read right.
fn lightbits(root: &Path, map: &str, near: Option<([f32; 3], f32)>) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let pkg = std::sync::Arc::new(load(&path)?);
    let (_, model) = grim_world::scene::level_model(&pkg).ok_or("map has no Model")?;
    // Where each surface is, to answer for the ones around a place.
    let mut centres: Vec<Option<[f32; 3]>> = vec![None; model.light_map.len()];
    for node in &model.nodes {
        if node.num_vertices < 3 || node.i_surf < 0 {
            continue;
        }
        let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
        let i = surf.i_light_map;
        if i < 0 || i as usize >= centres.len() || centres[i as usize].is_some() {
            continue;
        }
        let mut middle = [0.0f32; 3];
        for v in 0..node.num_vertices as usize {
            let Some(vert) = model.verts.get(node.i_vert_pool as usize + v) else { continue };
            let Some(p) = model.points.get(vert.p_vertex as usize) else { continue };
            middle = [middle[0] + p.x, middle[1] + p.y, middle[2] + p.z];
        }
        let n = node.num_vertices as f32;
        centres[i as usize] = Some([middle[0] / n, middle[1] / n, middle[2] / n]);
    }
    let mut used = 0usize;
    let mut surfaces = 0usize;
    let mut reach: Vec<f32> = Vec::new();
    let mut scaled: Vec<f32> = Vec::new();
    let mut inners: Vec<f32> = Vec::new();
    let mut plain_inner: Vec<f32> = Vec::new();
    let (mut lit, mut dark) = (0u64, 0u64);
    let mut highest = 0usize;
    for (index, lm) in model.light_map.iter().enumerate() {
        let centre = centres[index];
        if let Some((at, reach)) = near {
            let Some(c) = centre else { continue };
            if (0..3).map(|i| (c[i] - at[i]).powi(2)).sum::<f32>().sqrt() > reach {
                continue;
            }
        }
        surfaces += 1;
        let (w, h) = (lm.u_clamp.max(0) as usize, lm.v_clamp.max(0) as usize);
        let stride = w.div_ceil(8);
        let mut lights = 0usize;
        let mut k = lm.i_light_actors.max(0) as usize;
        if lm.i_light_actors >= 0 {
            while let Some(r) = model.lights.get(k) {
                if r.is_null() {
                    break;
                }
                lights += 1;
                k += 1;
            }
        }
        for l in 0..lights {
            let start = lm.data_offset.max(0) as usize + l * stride * h;
            highest = highest.max(start + stride * h);
            for y in 0..h {
                for x in 0..w {
                    match model.light_bits.get(start + y * stride + x / 8) {
                        Some(b) if b >> (x % 8) & 1 != 0 => lit += 1,
                        Some(_) => dark += 1,
                        None => {}
                    }
                }
            }
        }
        used += lights * stride * h;
        // How far each light the map gave this surface is, against the reach its radius says.
        // A light the editor lists but that cannot reach would mean the engine reads the
        // radius on the wrong scale.
        if near.is_none() {
            if let Some(c) = centre {
                let mut k = lm.i_light_actors.max(0) as usize;
                if lm.i_light_actors >= 0 {
                    while let Some(r) = model.lights.get(k) {
                        if r.is_null() {
                            break;
                        }
                        if let grim_package::ObjectRef::Export(e) = *r {
                            let radius = read_byte(&pkg, e, "LightRadius").unwrap_or(64) as f32 * 25.0;
                            let scale = read_float(&pkg, e, "DrawScale").unwrap_or(1.0);
                            let at = read_vector(&pkg, e, "Location").unwrap_or([0.0; 3]);
                            let away = (0..3).map(|i| (c[i] - at[i]).powi(2)).sum::<f32>().sqrt();
                            reach.push(away / radius.max(1.0));
                            scaled.push(away / (radius * scale).max(1.0));
                            let inner = read_byte(&pkg, e, "LightRadiusInner").unwrap_or(0) as f32 * 25.0;
                            if inner > 0.0 {
                                inners.push(away / (inner * scale).max(1.0));
                                plain_inner.push(away / inner.max(1.0));
                            }
                        }
                        k += 1;
                    }
                }
            }
        }
        if near.is_some() {
            // Which lights the map gave this surface, and how far away they are: what says
            // whether a light of a given radius could have reached it at all.
            let mut k = lm.i_light_actors.max(0) as usize;
            if lm.i_light_actors >= 0 {
                while let Some(r) = model.lights.get(k) {
                    if r.is_null() {
                        break;
                    }
                    if let grim_package::ObjectRef::Export(e) = *r {
                        let name = pkg.object_name(grim_package::ObjectRef::Export(e));
                        let radius = match read_byte(&pkg, e, "LightRadius") {
                            Some(b) => b as f32,
                            None => 64.0,
                        };
                        let at = read_vector(&pkg, e, "Location").unwrap_or([0.0; 3]);
                        let away = centre.map(|c| (0..3).map(|i| (c[i] - at[i]).powi(2)).sum::<f32>().sqrt());
                        out!("    light {name} radius {radius} ({} units) at {at:?}, {away:?} away", radius * 25.0);
                    }
                    k += 1;
                }
            }
            let (mut on, mut off) = (0u64, 0u64);
            for l in 0..lights {
                let start = lm.data_offset.max(0) as usize + l * stride * h;
                for y in 0..h {
                    for x in 0..w {
                        match model.light_bits.get(start + y * stride + x / 8) {
                            Some(b) if b >> (x % 8) & 1 != 0 => on += 1,
                            Some(_) => off += 1,
                            None => {}
                        }
                    }
                }
            }
            out!(
                "  surface {index} at {:?}: {w}x{h}, {lights} lights, {on} texels lit, {off} in shadow",
                centre.map(|c| c.map(|v| v.round()))
            );
        }
    }
    out!("{surfaces} lightmap entries, {} light slots, {} bytes of bits", model.lights.len(), model.light_bits.len());
    out!("bytes the entries account for: {used} (highest byte reached {highest})");
    if !reach.is_empty() {
        reach.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let over = reach.iter().filter(|&&r| r > 1.0).count();
        let pick = |q: f32| reach[((reach.len() - 1) as f32 * q) as usize];
        out!(
            "light reach (distance / radius*25): median {:.2}, 90% {:.2}, max {:.2}; {over} of {} beyond their radius",
            pick(0.5),
            pick(0.9),
            pick(1.0),
            reach.len()
        );
    }
    if !scaled.is_empty() {
        scaled.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let over = scaled.iter().filter(|&&r| r > 1.0).count();
        let pick = |q: f32| scaled[((scaled.len() - 1) as f32 * q) as usize];
        out!(
            "  with DrawScale: median {:.2}, 90% {:.2}, max {:.2}; {over} of {} beyond",
            pick(0.5),
            pick(0.9),
            pick(1.0),
            scaled.len()
        );
    }
    if !inners.is_empty() {
        inners.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let over = inners.iter().filter(|&&r| r > 1.0).count();
        let pick = |q: f32| inners[((inners.len() - 1) as f32 * q) as usize];
        out!(
            "  with LightRadiusInner: median {:.2}, 90% {:.2}, max {:.2}; {over} of {} beyond",
            pick(0.5),
            pick(0.9),
            pick(1.0),
            inners.len()
        );
    }
    if !plain_inner.is_empty() {
        plain_inner.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let over = plain_inner.iter().filter(|&&r| r > 1.0).count();
        let pick = |q: f32| plain_inner[((plain_inner.len() - 1) as f32 * q) as usize];
        out!(
            "  with LightRadiusInner and no DrawScale: median {:.2}, 90% {:.2}, max {:.2}; {over} of {} beyond",
            pick(0.5),
            pick(0.9),
            pick(1.0),
            plain_inner.len()
        );
    }
    out!("texels lit {lit}, in shadow {dark} ({:.1}% in shadow)", dark as f64 * 100.0 / (lit + dark).max(1) as f64);
    Ok(())
}

/// Runs a map and follows its cutscenes: which one plays, which is left waiting and which the
/// game state switched off, plus what the cut console printed.
fn cutscenes(root: &Path, map: &str, seconds: f32) -> Result<(), String> {
    let world = linked_world(root)?;
    let mut vm = grim_vm::Vm::new(world);
    vm.apply_config_defaults();
    let mut report = grim_vm::level::Report::default();
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let level = vm.load_level(&path)?;
    vm.begin_play(&level, &mut report);
    vm.travel_post_accept(&mut report);
    let cuts = vm.objects_of_class("CutScene");
    let state = |vm: &mut grim_vm::Vm, a: grim_object::ObjectKey| -> String {
        match vm.instance(a).ok().and_then(|i| i.state) {
            Some(s) => vm.world.names.str(vm.world.state(s).name).to_string(),
            None => "none".to_string(),
        }
    };
    let flag = |vm: &mut grim_vm::Vm, a: grim_object::ObjectKey, p: &str| match vm.get_prop(a, p) {
        Ok(grim_object::Value::Bool(b)) => b,
        _ => false,
    };
    out!("{} cutscenes in {map}", cuts.len());
    for &c in &cuts {
        let (name, st) = (vm.object_name(c), state(&mut vm, c));
        out!(
            "{name:16} state {st:12} levelLoad {} trigger {} bump {} inGameState {} playOnce {}",
            flag(&mut vm, c, "bLevelLoadStarts"),
            flag(&mut vm, c, "bTriggerStarts"),
            flag(&mut vm, c, "bBumpStarts"),
            flag(&mut vm, c, "bInCurrentGameState"),
            flag(&mut vm, c, "bPlayOnce"),
        );
    }
    let ticks = (seconds * 30.0) as u32;
    let mut last: Vec<String> = cuts.iter().map(|&c| state(&mut vm, c)).collect();
    let mut asked_for = String::new();
    for i in 0..ticks {
        vm.tick(1.0 / 30.0, &mut report);
        // The level asks to move on by leaving the next URL on its info: the engine's own loop
        // is what acts on it, so here it is only reported.
        if let Some(info) = vm.level_info {
            if let Ok(grim_object::Value::Str(url)) = vm.get_prop(info, "NextURL") {
                if url != asked_for {
                    if !url.is_empty() {
                        out!("{:6.2}s level change asked for: {url}", i as f32 / 30.0);
                    }
                    asked_for = url;
                }
            }
        }
        for (n, &c) in cuts.iter().enumerate() {
            let now = state(&mut vm, c);
            if now != last[n] {
                out!("{:6.2}s {:16} {} -> {now}", i as f32 / 30.0, vm.object_name(c), last[n]);
                last[n] = now;
            }
        }
    }
    out!("after {seconds}s:");
    for &c in &cuts {
        let played = match vm.get_prop(c, "nPlayedCount") { Ok(grim_object::Value::Int(n)) => n, _ => 0 };
        let name = vm.object_name(c);
        let st = state(&mut vm, c);
        out!("{name:16} state {st:12} played {played}");
    }
    out!("script log: {} lines", vm.log.len());
    for line in vm.log.iter().rev().take(20).rev() {
        out!("  {line}");
    }
    let mut warnings: Vec<_> = vm.warnings.iter().collect();
    warnings.sort_by_key(|x| std::cmp::Reverse(*x.1));
    for (w, n) in warnings.iter().take(10) {
        out!("{n:8} {w}");
    }
    Ok(())
}


/// Sends the player the render events the engine sends as it draws. Nothing is drawn here, but
/// they are what spawns the player's HUD, and scripts ask the HUD what the game is doing. The
/// canvas is given a size first: the HUD divides by it. What it measures text with is still
/// missing without a renderer, so the HUD's own text layout divides by zero here; that is this
/// harness, not the engine (the game itself reports none).
fn render_events(vm: &mut grim_vm::Vm, report: &mut grim_vm::level::Report) {
    let (Some(p), Some(canvas)) = (vm.player, vm.canvas) else { return };
    const SIZE: (i32, f32) = (960, 720.0);
    for (name, value) in [("SizeX", SIZE.0), ("SizeY", SIZE.1 as i32)] {
        let _ = vm.set_prop(canvas, name, grim_object::Value::Int(value));
    }
    for (name, value) in [("ClipX", SIZE.0 as f32), ("ClipY", SIZE.1)] {
        let _ = vm.set_prop(canvas, name, grim_object::Value::Float(value));
    }
    for event in ["PreRender", "PostRender"] {
        if let Err(e) = vm.call_event(p, event, vec![grim_object::Value::Object(Some(canvas))]) {
            report.record(e);
        }
    }
}

/// Compares a saved game against what the engine makes of it.
///
/// A save is a package holding the level exactly as the original engine left it, every actor
/// with every property it had. Loading it and running a couple of ticks should therefore
/// change nothing: whatever moves is the engine doing something the original did not. It is
/// the sharpest test there is of the physics and of the values the scripts read, because the
/// answers come from the game itself rather than from a guess.
///
/// Numbers are compared with a little slack, because a tick of the original went by between
/// the save being written and the state it holds.
fn save_diff(root: &Path, file: &Path, ticks: u32) -> Result<(), String> {
    /// How far a number may drift before it counts as a difference.
    const SLACK: f32 = 0.5;
    let world = linked_world(root)?;
    let mut vm = grim_vm::Vm::new(world);
    vm.apply_config_defaults();
    let mut report = grim_vm::level::Report::default();
    let level = vm.load_level(file)?;
    vm.begin_play(&level, &mut report);
    for _ in 0..ticks {
        vm.tick(1.0 / 30.0, &mut report);
    }
    let pkg = vm.world.package(level.level.package).clone();
    let mut counts: std::collections::BTreeMap<String, (usize, String)> = std::collections::BTreeMap::new();
    let (mut actors, mut differing) = (0usize, 0usize);
    for export in 0..pkg.exports.len() as u32 {
        let key = grim_object::ObjectKey { package: level.level.package, export };
        // What the save says this object was, built the same way the level loader builds it.
        let Ok(saved) = vm.world.instantiate(&pkg, export) else { continue };
        let Ok(live) = vm.instance(key) else { continue };
        if live.deleted {
            continue;
        }
        let (class, values) = (live.class, live.values.clone());
        actors += 1;
        let def = vm.world.class(class).clone();
        // GRIM_SAVE_DUMP=<name> prints everything the save holds for one object, whether it
        // differs or not: the numbers a rule is worked out from. `<prefix>*` takes every object
        // whose name starts that way.
        let dumped = std::env::var("GRIM_SAVE_DUMP").is_ok_and(|w| {
            let name = vm.object_name(key).to_lowercase();
            match w.strip_suffix('*') {
                Some(prefix) => name.starts_with(&prefix.to_lowercase()),
                None => name == w.to_lowercase(),
            }
        });
        if dumped {
            out!("  = level {} object {} export {}", vm.level_name(), vm.object_name(key), export);
            for prop in &def.props {
                for i in 0..prop.array_dim.max(1) {
                    if let Some(v) = saved.values.get((prop.slot + i) as usize) {
                        let suffix = if prop.array_dim > 1 { format!("[{i}]") } else { String::new() };
                        out!("  = {}{suffix} {v:?}", vm.world.names.str(prop.name));
                    }
                }
            }
        }
        let mut first = true;
        for prop in &def.props {
            for i in 0..prop.array_dim.max(1) {
                let slot = (prop.slot + i) as usize;
                let (Some(was), Some(now)) = (saved.values.get(slot), values.get(slot)) else { continue };
                if same_value(was, now, SLACK) {
                    continue;
                }
                let name = vm.world.names.str(prop.name).to_string();
                // What the engine fills in for itself and what it spawns fresh: every object
                // has a different `Class`, `Name` and `XLevel` than the file's, and an actor's
                // own effects are made again rather than read back, so they land in the heap
                // instead of the package. None of that is the game behaving differently.
                if matches!(name.as_str(), "Class" | "Name" | "XLevel" | "Level" | "Region" | "FootRegion" | "HeadRegion") {
                    continue;
                }
                // Network play and passwords, which the game never uses: every load logs the
                // player in again and counts it as one more.
                if matches!(name.as_str(), "bAdmin" | "PlayerID" | "NumPlayers" | "CurrentID") {
                    continue;
                }
                let spawned = |v: &grim_object::Value| matches!(v, grim_object::Value::Object(Some(k)) if k.package == grim_vm::vm::TRANSIENT);
                if spawned(was) || spawned(now) {
                    continue;
                }
                let entry = counts.entry(name.clone()).or_insert((0, String::new()));
                entry.0 += 1;
                if entry.1.is_empty() {
                    entry.1 = format!("{} {was:?} -> {now:?}", vm.object_name(key));
                }
                // GRIM_SAVE_PROP=<name> lists every object that differs in that one property,
                // which is what turning a count into a cause needs.
                if std::env::var("GRIM_SAVE_PROP").is_ok_and(|w| w.eq_ignore_ascii_case(&name)) {
                    out!("    {} {was:?} -> {now:?}", vm.object_name(key));
                }
                if first {
                    differing += 1;
                    first = false;
                }
            }
        }
    }
    out!("{} of {actors} objects differ after {ticks} ticks", differing);
    // What the engine itself complained about along the way: a save that cannot be resumed
    // says so here before it shows up as a difference.
    let mut warned: Vec<(usize, String)> = vm.warnings.iter().map(|(w, &n)| (n, w.clone())).collect();
    warned.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (n, w) in warned.iter().take(12) {
        out!("  warning x{n}: {w}");
    }
    let mut rows: Vec<_> = counts.into_iter().map(|(n, (c, e))| (c, n, e)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (count, name, example) in rows {
        out!("  {count:5} {name:28} e.g. {example}");
    }
    Ok(())
}

/// Whether two values are the same, letting numbers drift by `slack` and looking inside
/// structs and arrays.
fn same_value(a: &grim_object::Value, b: &grim_object::Value, slack: f32) -> bool {
    use grim_object::Value;
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => (x - y).abs() <= slack || (x.is_nan() && y.is_nan()),
        (Value::Struct(x), Value::Struct(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_value(p, q, slack)),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_value(p, q, slack)),
        _ => a == b,
    }
}

fn run_maps(root: &Path, only: Option<&str>, seconds: f32) -> Result<(), String> {
    let world = linked_world(root)?;
    let mut vm = grim_vm::Vm::new(world);
    vm.apply_config_defaults();
    let mut report = grim_vm::level::Report::default();
    let maps: Vec<_> = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .filter(|m| only.is_none_or(|o| m.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(o))))
        .collect();
    for m in &maps {
        vm.reset_heap();
        let level = match vm.load_level(m) {
            Ok(l) => l,
            Err(e) => {
                out!("{}: {e}", m.display());
                continue;
            }
        };
        vm.begin_play(&level, &mut report);
        let player = vm.player;
        if only.is_some() {
            match player {
                Some(p) => {
                    let class = vm.class_of(p).map_err(|e| e.to_string())?;
                    let name = vm.object_name(p);
                    out!("player: {name} ({})", vm.world.names.str(vm.world.class(class).name));
                    for prop in ["Location", "Rotation", "ViewTarget", "Physics", "bCollideWorld"] {
                        if let Ok(v) = vm.get_prop(p, prop) {
                            out!("   {prop} = {}", describe(&mut vm, &v));
                        }
                    }
                }
                None => out!("player: none"),
            }
        }
        for _ in 0..(seconds * 30.0).max(1.0) as u32 {
            vm.tick(1.0 / 30.0, &mut report);
            render_events(&mut vm, &mut report);
        }
        // GRIM_DEBUG_PHYSICS counts what the actors of every map are doing, which says which
        // physics modes the game actually uses.
        if std::env::var_os("GRIM_DEBUG_PHYSICS").is_some() {
            for a in vm.actors.clone() {
                if vm.is_deleted(a) {
                    continue;
                }
                if let Ok(grim_object::Value::Byte(mode)) = vm.get_prop(a, "Physics") {
                    let class = vm.class_of(a).map(|c| vm.world.names.str(vm.world.class(c).name).to_string()).unwrap_or_default();
                    out!("PHYSICS {mode} {class}");
                }
            }
        }
        if only.is_some() {
            if let Some(p) = vm.player {
                out!("after 1 second:");
                for prop in ["Location", "Rotation", "Target", "ViewTarget"] {
                    if let Ok(v) = vm.get_prop(p, prop) {
                        out!("   {prop} = {}", describe(&mut vm, &v));
                    }
                }
            }
            let upright = vm.decals.values().filter(|d| d.normal[2] < 0.7).count();
            out!("decals on the world: {} ({upright} of them on something other than the floor)", vm.decals.len());
            if let Some(p) = vm.player {
                let st = vm.instance(p).map_err(|e| e.to_string())?.state;
                let state = st.map(|s| vm.world.names.str(vm.world.state(s).name).to_string()).unwrap_or("none".into());
                out!("player state: {state}");
            }
        }
    }
    out!("{} maps, {} event calls", maps.len(), report.calls);
    let mut natives: Vec<_> = report.missing_natives.iter().collect();
    natives.sort_by_key(|x| std::cmp::Reverse(*x.1));
    out!("{} missing natives:", natives.len());
    for (n, c) in natives {
        out!("{c:8} {n}");
    }
    let mut errors: Vec<_> = report.errors.iter().collect();
    errors.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    out!("{} script error kinds:", errors.len());
    for (_, (c, example)) in errors.iter().take(40) {
        out!("{c:8} {}", example.replace('\n', "\n          "));
    }
    let mut warnings: Vec<_> = vm.warnings.iter().collect();
    warnings.sort_by_key(|x| std::cmp::Reverse(*x.1));
    out!("warnings: {warnings:?}");
    out!("script log: {} lines, e.g. {:?}", vm.log.len(), vm.log.iter().take(5).collect::<Vec<_>>());
    Ok(())
}


/// The fields of a struct, in the order the package chains them, which is the order its
/// default values are written in.
fn fields(pkg: &Package, name: &str) -> Result<(), String> {
    let i = (0..pkg.exports.len() as u32)
        .find(|&i| pkg.class_name(grim_package::ObjectRef::Export(i)) == "Struct" && pkg.object_name(grim_package::ObjectRef::Export(i)).eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no struct {name} in {}", pkg.name))?;
    let grim_assets::Asset::Struct(st) = grim_assets::load_export(pkg, i).map_err(|e| e.to_string())?.asset else {
        return Err("not a struct".into());
    };
    let mut child = st.children;
    while let grim_package::ObjectRef::Export(e) = child {
        out!("  {} {}", pkg.class_name(grim_package::ObjectRef::Export(e)), pkg.object_name(grim_package::ObjectRef::Export(e)));
        let Ok(loaded) = grim_assets::load_export(pkg, e) else { break };
        child = match &loaded.asset {
            grim_assets::Asset::Property(p) => p.field.next,
            _ => break,
        };
    }
    Ok(())
}

/// Where each mover of a map stands and what starts it, with the box its brush takes up in
/// the world: a mover often sits at the origin and carries its place in the brush itself.
fn moverplaces(root: &Path, map: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    for i in 0..pkg.exports.len() as u32 {
        if !pkg.class_name(grim_package::ObjectRef::Export(i)).contains("Mover") {
            continue;
        }
        let Ok(obj) = grim_assets::load_export(&pkg, i) else { continue };
        let state = match obj.base.property(&pkg, "InitialState", 0) {
            Some(grim_package::property::PropertyValue::Name(n)) => pkg.name(*n).to_string(),
            _ => String::new(),
        };
        let at = match obj.base.property(&pkg, "Location", 0) {
            Some(grim_package::property::PropertyValue::Struct { value: grim_package::property::StructValue::Vector(v), .. }) => {
                [v.x, v.y, v.z]
            }
            _ => [0.0; 3],
        };
        let Some(grim_package::property::PropertyValue::Object(grim_package::ObjectRef::Export(b))) =
            obj.base.property(&pkg, "Brush", 0)
        else {
            continue;
        };
        let Ok(grim_assets::Asset::Model(model)) = grim_assets::load_export(&pkg, *b).map(|l| l.asset) else { continue };
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &model.points {
            for (k, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[k] = lo[k].min(v);
                hi[k] = hi[k].max(v);
            }
        }
        out!(
            "{} {state} at {:?} brush {:?}..{:?}",
            pkg.path_name(grim_package::ObjectRef::Export(i)),
            at.map(|v| v.round()),
            lo.map(|v| v.round()),
            hi.map(|v| v.round())
        );
    }
    Ok(())
}

/// The texture mapping a brush's surfaces carry: which vectors they point at and how many
/// there are. A surface whose axes are not its own comes out skewed.
fn brushuv(root: &Path, map: &str, name: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let i = (0..pkg.exports.len() as u32)
        .find(|&i| pkg.object_name(grim_package::ObjectRef::Export(i)).eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no {name}"))?;
    let Ok(obj) = grim_assets::load_export(&pkg, i) else { return Err("cannot load".into()) };
    let Some(grim_package::property::PropertyValue::Object(grim_package::ObjectRef::Export(b))) =
        obj.base.property(&pkg, "Brush", 0)
    else {
        return Err("no brush".into());
    };
    let Ok(grim_assets::Asset::Model(model)) = grim_assets::load_export(&pkg, *b).map(|l| l.asset) else {
        return Err("no model".into());
    };
    // The editor polygons the surfaces were cut from: they carry the mapping the map was
    // painted with, which is what a surface's own vectors have to agree with.
    let polys = match model.polys {
        grim_package::ObjectRef::Export(e) => match grim_assets::load_export(&pkg, e).map(|l| l.asset) {
            Ok(grim_assets::Asset::Polys(p)) => p.polys,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    out!(
        "{} vectors, {} points, {} surfaces, {} editor polygons",
        model.vectors.len(),
        model.points.len(),
        model.surfs.len(),
        polys.len()
    );
    for (k, surf) in model.surfs.iter().enumerate() {
        // Where the surface is: the corners of every node cut out of it, which says which face
        // of the brush each one is and how big a piece of it shows.
        let mut low = [f32::MAX; 3];
        let mut high = [f32::MIN; 3];
        for node in model.nodes.iter().filter(|n| n.i_surf == k as i32) {
            for j in 0..node.num_vertices as i32 {
                let Some(vert) = model.verts.get((node.i_vert_pool + j) as usize) else { continue };
                let Some(p) = model.points.get(vert.p_vertex.max(0) as usize) else { continue };
                for (i, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                    low[i] = low[i].min(c);
                    high[i] = high[i].max(c);
                }
            }
        }
        let where_ = if low[0] > high[0] {
            "  (no node)".to_string()
        } else {
            format!("  x {:.0}..{:.0} y {:.0}..{:.0} z {:.0}..{:.0}", low[0], high[0], low[1], high[1], low[2], high[2])
        };
        let v = |i: i32| model.vectors.get(i.max(0) as usize).map(|v| [v.x.round(), v.y.round(), v.z.round()]);
        let r = |v: grim_package::property::Vector| [v.x.round(), v.y.round(), v.z.round()];
        out!(
            "  surf {k}: flags {:#x} base {} normal {:?} u {} {:?} v {} {:?} pan {},{} texture {}",
            surf.poly_flags,
            surf.p_base,
            v(surf.v_normal),
            surf.v_texture_u,
            v(surf.v_texture_u),
            surf.v_texture_v,
            v(surf.v_texture_v),
            surf.pan_u,
            surf.pan_v,
            format!("{}{where_}", pkg.path_name(surf.texture))
        );
        if let Some(poly) = (surf.i_brush_poly >= 0).then(|| polys.get(surf.i_brush_poly as usize)).flatten() {
            let same = r(poly.texture_u) == v(surf.v_texture_u).unwrap_or_default()
                && r(poly.texture_v) == v(surf.v_texture_v).unwrap_or_default();
            out!(
                "      poly {}: u {:?} v {:?} pan {},{} texture {}{}",
                surf.i_brush_poly,
                r(poly.texture_u),
                r(poly.texture_v),
                poly.pan_u,
                poly.pan_v,
                pkg.object_name(poly.texture),
                if same { "" } else { "   <- different mapping" }
            );
        }
    }
    for (i, node) in model.nodes.iter().enumerate() {
        out!(
            "  node {i}: surf {} verts {} pool {} collision {} render {} zone {:?} flags {:#x} leaf {:?}",
            node.i_surf,
            node.num_vertices,
            node.i_vert_pool,
            node.i_collision_bound,
            node.i_render_bound,
            node.i_zone,
            node.node_flags,
            node.i_leaf
        );
    }
    for (i, lm) in model.light_map.iter().enumerate() {
        out!(
            "  lightmap {i}: offset {} actors {} pan {:.0},{:.0},{:.0} scale {:.1},{:.1} clamp {}x{}",
            lm.data_offset, lm.i_light_actors, lm.pan.x, lm.pan.y, lm.pan.z, lm.u_scale, lm.v_scale, lm.u_clamp, lm.v_clamp
        );
    }
    // Cross check: a node's plane is the plane of the surface it was cut from, so the two
    // normals have to be the same vector (or its opposite, for a node on the far side).
    let mut off = 0;
    for (i, node) in model.nodes.iter().enumerate() {
        let Some(surf) = model.surfs.get(node.i_surf.max(0) as usize) else { continue };
        let Some(n) = model.vectors.get(surf.v_normal.max(0) as usize) else { continue };
        let d = node.plane.x * n.x + node.plane.y * n.y + node.plane.z * n.z;
        if (d.abs() - 1.0).abs() > 0.01 {
            off += 1;
            if off <= 5 {
                out!(
                    "  node {i} plane [{:.2}, {:.2}, {:.2}] but surf {} normal [{:.2}, {:.2}, {:.2}]",
                    node.plane.x, node.plane.y, node.plane.z, node.i_surf, n.x, n.y, n.z
                );
            }
        }
    }
    out!("  {off} of {} nodes sit on a plane other than their surface's", model.nodes.len());
    // The editor polygons themselves: a mover's surfaces often point at none of them
    // (`i_brush_poly = -1`), and then this is the only place its mapping can be checked
    // against.
    for (k, poly) in polys.iter().enumerate() {
        let r = |v: grim_package::property::Vector| [v.x.round(), v.y.round(), v.z.round()];
        let mut low = [f32::MAX; 3];
        let mut high = [f32::MIN; 3];
        for v in &poly.vertices {
            for (i, c) in [v.x, v.y, v.z].into_iter().enumerate() {
                low[i] = low[i].min(c);
                high[i] = high[i].max(c);
            }
        }
        out!(
            "  poly {k}: link {} brushpoly {} flags {:#x} normal {:?} u {:?} v {:?} pan {},{} texture {}  x {:.0}..{:.0} y {:.0}..{:.0} z {:.0}..{:.0}",
            poly.i_link,
            poly.i_brush_poly,
            poly.poly_flags,
            r(poly.normal),
            r(poly.texture_u),
            r(poly.texture_v),
            poly.pan_u,
            poly.pan_v,
            pkg.path_name(poly.texture),
            low[0],
            high[0],
            low[1],
            high[1],
            low[2],
            high[2]
        );
    }
    Ok(())
}

/// Whether a brush's own tree calls the inside or the outside solid, tested at its centre and
/// well outside it. A mover blocks the way with that tree, so which way round it is matters.
fn brushsolid(root: &Path, map: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let mut shown = 0;
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
        if model.nodes.is_empty() {
            continue;
        }
        let bsp = grim_world::Bsp::new(&model);
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &model.points {
            for (k, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[k] = lo[k].min(v);
                hi[k] = hi[k].max(v);
            }
        }
        let centre = [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0);
        let away = [hi[0] + 500.0, centre[1], centre[2]];
        out!(
            "{}: root_outside {} centre solid {} far solid {}",
            pkg.path_name(grim_package::ObjectRef::Export(i)),
            model.root_outside,
            bsp.point_solid(centre),
            bsp.point_solid(away)
        );
        shown += 1;
        if shown >= 6 {
            break;
        }
    }
    Ok(())
}

/// Movers of a map whose brush surfaces end up with no texture, which is what draws them
/// white. A mover's surfaces carry no texture of their own: it stays on the editor polygon
/// they were cut from.
fn movers(root: &Path, map: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    for i in 0..pkg.exports.len() as u32 {
        let class = pkg.class_name(grim_package::ObjectRef::Export(i)).to_string();
        if !class.contains("Mover") {
            continue;
        }
        let Ok(obj) = grim_assets::load_export(&pkg, i) else { continue };
        let Some(grim_package::property::PropertyValue::Object(brush)) = obj.base.property(&pkg, "Brush", 0) else {
            continue;
        };
        let grim_package::ObjectRef::Export(b) = *brush else { continue };
        let Ok(grim_assets::Asset::Model(model)) = grim_assets::load_export(&pkg, b).map(|l| l.asset) else { continue };
        let polys = match model.polys {
            grim_package::ObjectRef::Export(p) => match grim_assets::load_export(&pkg, p).map(|l| l.asset) {
                Ok(grim_assets::Asset::Polys(polys)) => polys.polys,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let mut blank = Vec::new();
        for (k, surf) in model.surfs.iter().enumerate() {
            if !matches!(surf.texture, grim_package::ObjectRef::Null) {
                continue;
            }
            let from_poly = polys.get(surf.i_brush_poly.max(0) as usize).map(|p| p.texture);
            if !matches!(from_poly, Some(grim_package::ObjectRef::Null) | None) {
                continue;
            }
            blank.push(format!("surf {k} poly {} of {} flags {:#x}", surf.i_brush_poly, polys.len(), surf.poly_flags));
        }
        let textured = polys.iter().filter(|p| !matches!(p.texture, grim_package::ObjectRef::Null)).count();
        if !blank.is_empty() {
            out!("  {} of {} editor polygons have a texture", textured, polys.len());
            out!(
                "{class} {} ({} surfaces, {} blank): {}",
                pkg.path_name(grim_package::ObjectRef::Export(i)),
                model.surfs.len(),
                blank.len(),
                blank.join(", ")
            );
        }
    }
    Ok(())
}

/// Exports of a map whose properties mention a text, with where they stand. A room named in
/// the game's states is reached this way: the actors of that room carry its name.
fn findtext(root: &Path, map: &str, text: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    for i in 0..pkg.exports.len() as u32 {
        let Ok(obj) = grim_assets::load_export(&pkg, i) else { continue };
        let printed = format!("{:?}", obj.base.properties);
        if !printed.to_lowercase().contains(&text.to_lowercase()) {
            continue;
        }
        let at = match obj.base.property(&pkg, "Location", 0) {
            Some(grim_package::property::PropertyValue::Struct { value, .. }) => format!("{value:?}"),
            _ => String::new(),
        };
        out!("{} {} {at}", pkg.class_name(grim_package::ObjectRef::Export(i)), pkg.path_name(grim_package::ObjectRef::Export(i)));
    }
    Ok(())
}

/// The names the editor's polygons carry in a map, counted. A designer marks things by hand
/// there, so a name that only a few polygons carry says something was singled out.
fn polynames(root: &Path, map: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let mut count: BTreeMap<String, (usize, u32)> = BTreeMap::new();
    for i in 0..pkg.exports.len() as u32 {
        if pkg.class_name(grim_package::ObjectRef::Export(i)) != "Polys" {
            continue;
        }
        let Ok(loaded) = grim_assets::load_export(&pkg, i) else { continue };
        let grim_assets::Asset::Polys(polys) = loaded.asset else { continue };
        for poly in &polys.polys {
            let entry = count.entry(pkg.name(poly.item_name).to_string()).or_default();
            entry.0 += 1;
            entry.1 |= poly.poly_flags;
        }
    }
    let mut names: Vec<_> = count.into_iter().collect();
    names.sort_by_key(|(_, (n, _))| *n);
    for (name, (n, flags)) in names {
        out!("  {n:6}  {flags:#010x}  {name}");
    }
    Ok(())
}

/// Textures of the game whose named boolean property is set. A texture carries flags that the
/// editor copies onto every surface painted with it, so this is how to find out what a flag
/// is for: see which pictures were given it.
fn texflags(root: &Path, name: &str) -> Result<(), String> {
    let lib = grim_package::Library::open(root);
    let mut found = 0;
    for file in grim_testkit::package_files(root) {
        let Ok(pkg) = lib.load_path(&file) else { continue };
        for i in 0..pkg.exports.len() as u32 {
            if !pkg.class_name(grim_package::ObjectRef::Export(i)).contains("Texture") {
                continue;
            }
            let Ok(obj) = grim_assets::load_export(&pkg, i) else { continue };
            // A flag that is set, or any other property the texture carries a value for.
            match obj.base.property(&pkg, name, 0) {
                Some(grim_package::property::PropertyValue::Bool(false)) | None => {}
                Some(value) => {
                    found += 1;
                    out!("{}.{} {value:?}", pkg.name, pkg.path_name(grim_package::ObjectRef::Export(i)));
                }
            }
        }
    }
    out!("{found} textures with {name}");
    Ok(())
}

/// Poly flags of a map's BSP surfaces, counted, and where the rarer ones sit. Surface flags
/// come from the texture they were painted with, so this is how to find, for instance, which
/// walls a texture flag marks out.
/// Where the surfaces drawn with a texture are, by the box of the nodes cut from each one:
/// what it takes to point a camera at the water of a map without walking it.
/// The first surface of the level straight below a point.
fn floor(root: &Path, map: &str, at: &str) -> Result<(), String> {
    let v: Vec<f32> = at.split(',').filter_map(|c| c.trim().parse().ok()).collect();
    let [x, y, z] = v[..] else { return Err("give the point as x,y,z".into()) };
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let (_, model) = grim_world::scene::level_model(&pkg).ok_or("the map has no level model")?;
    let bsp = grim_world::Bsp::new(&model);
    match bsp.line_check([x, y, z], [x, y, z - 4096.0]) {
        Some(hit) => out!("floor at z {:.2}, {:.2} below", hit.location[2], z - hit.location[2]),
        None => out!("no floor below"),
    }
    Ok(())
}

fn surfs_named(root: &Path, map: &str, name: &str) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let (_, model) = grim_world::scene::level_model(&pkg).ok_or("the map has no level model")?;
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3], String, u32, u8)> = Vec::new();
    for node in &model.nodes {
        if node.i_surf < 0 || node.num_vertices == 0 {
            continue;
        }
        let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
        let texture = pkg.path_name(surf.texture);
        if !texture.to_lowercase().contains(&name.to_lowercase()) {
            continue;
        }
        let points: Vec<[f32; 3]> = (0..node.num_vertices as i32)
            .filter_map(|k| model.verts.get((node.i_vert_pool + k) as usize))
            .filter_map(|v| model.points.get(v.p_vertex.max(0) as usize))
            .map(|p| [p.x, p.y, p.z])
            .collect();
        if points.is_empty() {
            continue;
        }
        let low = [0, 1, 2].map(|i| points.iter().map(|p| p[i]).fold(f32::MAX, f32::min));
        let high = [0, 1, 2].map(|i| points.iter().map(|p| p[i]).fold(f32::MIN, f32::max));
        match boxes.iter_mut().find(|(s, ..)| *s == node.i_surf as usize) {
            Some((_, l, h, ..)) => {
                for i in 0..3 {
                    l[i] = l[i].min(low[i]);
                    h[i] = h[i].max(high[i]);
                }
            }
            None => boxes.push((node.i_surf as usize, low, high, texture, surf.poly_flags, node.i_zone[grim_world::FRONT])),
        }
    }
    let pieces = grim_world::lightmap::Pieces::of(&model, None);
    for (surf, low, high, texture, flags, zone) in &boxes {
        // The lightmap grid the map ships for the surface: a surface without one is drawn at
        // full brightness, which is why its size belongs next to its flags.
        let light = match pieces.map(*surf) {
            Some(lm) => format!("light {}x{}", lm.u_clamp, lm.v_clamp),
            None => "light none".into(),
        };
        out!(
            "  surf {surf}: x {:.0}..{:.0} y {:.0}..{:.0} z {:.0}..{:.0} flags {flags:#010x} zone {zone} {light}  {texture}",
            low[0], high[0], low[1], high[1], low[2], high[2]
        );
    }
    out!("  {} surfaces drawn with a texture named like '{name}'", boxes.len());
    // The polygons the editor kept for the level, which carry the flags the surfaces were
    // built from: a surface whose flags look wrong is worth comparing against its polygon.
    if let grim_package::ObjectRef::Export(p) = model.polys {
        if let Ok(grim_assets::Asset::Polys(polys)) = grim_assets::load_export(&pkg, p).map(|l| l.asset) {
            let mut shown = 0;
            for poly in &polys.polys {
                if !pkg.path_name(poly.texture).to_lowercase().contains(&name.to_lowercase()) {
                    continue;
                }
                out!("  poly at {:?} flags {:#010x} item {}", [poly.base.x, poly.base.y, poly.base.z].map(|v| v.round()), poly.poly_flags, pkg.name(poly.item_name));
                shown += 1;
                if shown >= 12 {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn surfs(root: &Path, map: &str, wanted: Option<u32>, near: Option<[f32; 4]>) -> Result<(), String> {
    let path = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))?;
    let lib = grim_package::Library::open(root);
    let pkg = lib.load_path(&path)?;
    let (_, model) = grim_world::scene::level_model(&pkg).ok_or("the map has no level model")?;

    // With a flag given, every surface that carries it, with how tall it is and where its top
    // edge sits: what a flag marks shows in the shape of what carries it.
    // A place instead of a flag: every surface whose box reaches it, with the flags it has.
    if let Some(near) = near {
        for node in &model.nodes {
            if node.i_surf < 0 || node.num_vertices == 0 {
                continue;
            }
            let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
            let points: Vec<[f32; 3]> = (0..node.num_vertices as i32)
                .filter_map(|k| model.verts.get((node.i_vert_pool + k) as usize))
                .filter_map(|v| model.points.get(v.p_vertex.max(0) as usize))
                .map(|p| [p.x, p.y, p.z])
                .collect();
            if points.is_empty() {
                continue;
            }
            let low = [0, 1, 2].map(|i| points.iter().map(|p| p[i]).fold(f32::MAX, f32::min));
            let high = [0, 1, 2].map(|i| points.iter().map(|p| p[i]).fold(f32::MIN, f32::max));
            if (0..3).any(|i| near[i] < low[i] - near[3] || near[i] > high[i] + near[3]) {
                continue;
            }
            let v = |i: i32| {
                model.vectors.get(i.max(0) as usize).map(|v| [v.x.round(), v.y.round(), v.z.round()]).unwrap_or_default()
            };
            out!(
                "  {:#010x} z {:7.1}..{:7.1} x {:7.1}..{:7.1} y {:7.1}..{:7.1}  {} n {:?} u {:?} v {:?}",
                surf.poly_flags,
                low[2],
                high[2],
                low[0],
                high[0],
                low[1],
                high[1],
                pkg.object_name(surf.texture),
                v(surf.v_normal),
                v(surf.v_texture_u),
                v(surf.v_texture_v)
            );
        }
        return Ok(());
    }
    if let Some(wanted) = wanted {
        let mut heights: Vec<(f32, f32, String)> = Vec::new();
        for node in &model.nodes {
            if node.i_surf < 0 || node.num_vertices == 0 {
                continue;
            }
            let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
            if surf.poly_flags & wanted == 0 {
                continue;
            }
            let zs: Vec<f32> = (0..node.num_vertices as i32)
                .filter_map(|k| model.verts.get((node.i_vert_pool + k) as usize))
                .filter_map(|v| model.points.get(v.p_vertex.max(0) as usize))
                .map(|p| p.z)
                .collect();
            let (Some(&low), Some(&high)) = (
                zs.iter().min_by(|a, b| a.total_cmp(b)),
                zs.iter().max_by(|a, b| a.total_cmp(b)),
            ) else {
                continue;
            };
            let xs: Vec<f32> = (0..node.num_vertices as i32)
                .filter_map(|k| model.verts.get((node.i_vert_pool + k) as usize))
                .filter_map(|v| model.points.get(v.p_vertex.max(0) as usize))
                .map(|p| p.x)
                .collect();
            let ys: Vec<f32> = (0..node.num_vertices as i32)
                .filter_map(|k| model.verts.get((node.i_vert_pool + k) as usize))
                .filter_map(|v| model.points.get(v.p_vertex.max(0) as usize))
                .map(|p| p.y)
                .collect();
            let mid = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
            heights.push((
                high - low,
                high,
                format!("at {:.0},{:.0} {}", mid(&xs), mid(&ys), pkg.object_name(surf.texture)),
            ));
        }
        heights.sort_by(|a, b| a.0.total_cmp(&b.0));
        out!("{} faces with {wanted:#010x}", heights.len());
        for (tall, top, texture) in &heights {
            out!("  {tall:7.1} tall, top at {top:8.1}  {texture}");
        }
        return Ok(());
    }

    let mut count: BTreeMap<u32, usize> = BTreeMap::new();
    for surf in &model.surfs {
        for bit in 0..32 {
            if surf.poly_flags & (1 << bit) != 0 {
                *count.entry(1 << bit).or_default() += 1;
            }
        }
    }
    out!("{} surfaces", model.surfs.len());
    for (flag, n) in &count {
        // Which way the surfaces of a flag face, and what they are painted with: a flag that
        // marks something about the level's shape looks different from a lighting one.
        let mut up = 0;
        let mut textures: BTreeMap<String, usize> = BTreeMap::new();
        for (i, surf) in model.surfs.iter().enumerate() {
            if surf.poly_flags & flag == 0 {
                continue;
            }
            let normal = model.nodes.iter().find(|n| n.i_surf == i as i32).map(|n| n.plane.z);
            if normal.is_some_and(|z| z.abs() > 0.7) {
                up += 1;
            }
            *textures.entry(pkg.object_name(surf.texture).to_string()).or_default() += 1;
        }
        let mut names: Vec<_> = textures.into_iter().collect();
        names.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let names: Vec<String> = names.iter().take(4).map(|(t, n)| format!("{t}x{n}")).collect();
        out!("  {flag:#010x}  {n} ({up} flat) {}", names.join(" "));
    }
    Ok(())
}

/// Plays every cutscene of every map (or of one map) on its own and reports what it needed
/// that the engine does not give it: missing natives, script errors, the warnings raised while
/// it ran, and whether it ever reached its end.
fn cut_all(root: &Path, only: Option<&str>, cap: f32) -> Result<(), String> {
    // `GRIM_ONLY_CUT` narrows a run to one cutscene, for looking at a single one closely.
    let one = std::env::var("GRIM_ONLY_CUT").ok();
    let world = linked_world(root)?;
    let mut vm = grim_vm::Vm::new(world);
    vm.apply_config_defaults();
    let maps: Vec<_> = grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .filter(|m| only.is_none_or(|o| m.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(o))))
        .collect();
    let mut played = 0;
    let mut unfinished = Vec::new();
    for m in &maps {
        let map = m.file_stem().unwrap_or_default().to_string_lossy().to_string();
        // Which cutscenes the map has is asked once, on a level that is then thrown away: each
        // one is played on a level of its own, so nothing another cutscene did is in the way.
        vm.reset_heap();
        let level = match vm.load_level(m) {
            Ok(l) => l,
            Err(e) => {
                out!("{map}: {e}");
                continue;
            }
        };
        let mut report = grim_vm::level::Report::default();
        vm.begin_play(&level, &mut report);
        let names: Vec<String> = vm
            .objects_of_class("CutScene")
            .into_iter()
            .map(|c| vm.object_name(c))
            .filter(|n| one.as_deref().is_none_or(|o| n.eq_ignore_ascii_case(o)))
            .collect();
        for name in names {
            vm.reset_heap();
            let level = vm.load_level(m)?;
            let mut report = grim_vm::level::Report::default();
            vm.begin_play(&level, &mut report);
            vm.travel_post_accept(&mut report);
            let before: Vec<String> = vm.log.clone();
            let warned: std::collections::HashMap<String, usize> = vm.warnings.clone();
            let all = vm.objects_of_class("CutScene");
            let Some(cut) = all.iter().copied().find(|&c| vm.object_name(c) == name) else { continue };
            // The map's own opening cutscene runs first, as it does in the game: it is what
            // captures the camera and puts everyone in place, and a cutscene played without it
            // issues commands to actors nobody has captured, whose answers go nowhere (the
            // game hangs on that too: a thread only moves on when its cues come back). It is
            // given a few seconds; then the rest are quietened so only the one under test is
            // left running.
            let opening = all.iter().copied().any(|c| {
                c != cut && matches!(vm.get_prop(c, "bLevelLoadStarts"), Ok(grim_object::Value::Bool(true)))
            });
            if opening {
                for _ in 0..(8.0 * 30.0) as u32 {
                    let busy = all.iter().copied().any(|c| {
                        c != cut && matches!(vm.instance(c).ok().and_then(|i| i.state), Some(s) if vm.world.names.str(vm.world.state(s).name) == "Running")
                    });
                    if !busy {
                        break;
                    }
                    vm.tick(1.0 / 30.0, &mut report);
                    render_events(&mut vm, &mut report);
                }
            }
            for &other in all.iter().filter(|&&c| c != cut) {
                let _ = vm.call_event(other, "ForceFinish", vec![]);
                let _ = vm.set_prop(other, "bInCurrentGameState", grim_object::Value::Bool(false));
                let _ = vm.call_event(other, "OnResolveGameState", vec![]);
            }
            let state = |vm: &mut grim_vm::Vm, a: grim_object::ObjectKey| -> String {
                match vm.instance(a).ok().and_then(|i| i.state) {
                    Some(s) => vm.world.names.str(vm.world.state(s).name).to_string(),
                    None => "none".to_string(),
                }
            };
            // A cutscene the game state does not include sits in `disabled` and ignores `Play`.
            // Testing one means saying it belongs to the state the game is in, which is the
            // switch the game itself resolves on (`OnResolveGameState`).
            if state(&mut vm, cut) == "disabled" {
                vm.set_prop(cut, "bInCurrentGameState", grim_object::Value::Bool(true)).map_err(|e| e.to_string())?;
                if let Err(e) = vm.call_event(cut, "OnResolveGameState", vec![]) {
                    out!("{map}/{name}: enabling failed: {e}");
                    continue;
                }
            }
            let game_state = vm.enter_cutscene_state(cut, &mut report).unwrap_or(None);
            let placed = vm.place_player_at_cutscene(cut).unwrap_or(false);
            // The HUD is in place before the cutscene starts, as it would be on a screen that
            // has already drawn a frame.
            render_events(&mut vm, &mut report);
            if let Err(e) = vm.call_event(cut, "Play", vec![]) {
                out!("{map}/{name}: Play failed: {e}");
                continue;
            }
            // A cutscene ends when it says so. Until then it is given as long as it keeps
            // moving: some of the game's own run for minutes (a duel, the sorting ceremony),
            // and cutting them off at a fixed time says "stuck" about something that was only
            // long. What counts as moving is the last command its threads issued — it changes
            // every time one of them takes a step — and the cap is on standing still.
            // `cap` is how long it may stand still; nothing runs for more than ten minutes. The
            // default is 30s because the scripts themselves stand still for a while on purpose:
            // the longest `sleep` any of them takes is 18.5s, and a line of dialogue can run
            // longer than that again.
            let ticks = 600 * 30;
            let idle_cap = (cap * 30.0) as u32;
            let mut ran = 0;
            let mut ended = None;
            let mut last_step = String::new();
            let mut idle = 0;
            for i in 0..ticks {
                vm.tick(1.0 / 30.0, &mut report);
                render_events(&mut vm, &mut report);
                ran = i + 1;
                let st = state(&mut vm, cut);
                if st != "Running" && st != "FastForwarding" {
                    ended = Some(st);
                    break;
                }
                let mut step = String::new();
                for t in 0..20u32 {
                    if let Ok(grim_object::Value::Object(Some(thread))) = vm.get_prop_at(cut, "aThreads", t) {
                        if let Ok(grim_object::Value::Str(c)) = vm.get_prop(thread, "lastCommand") {
                            step.push_str(&c);
                            step.push('|');
                        }
                    }
                }
                if step == last_step {
                    idle += 1;
                    if idle >= idle_cap {
                        break;
                    }
                } else {
                    last_step = step;
                    idle = 0;
                }
            }
            played += 1;
            let seconds = ran as f32 / 30.0;
            let end = match &ended {
                Some(st) => format!("{st} after {seconds:.1}s"),
                None => {
                    unfinished.push(format!("{map}/{name}"));
                    out!("      stood still for the last {:.1}s of {seconds:.1}s", idle as f32 / 30.0);
                    // The cutscene knows what each of its threads is waiting for, and says so
                    // when asked: that is what a cutscene that never ends has to answer.
                    for i in 0..20u32 {
                        if let Ok(grim_object::Value::Object(Some(t))) = vm.get_prop_at(cut, "aThreads", i) {
                            let st = vm.state_name(t);
                            out!("      aThreads[{i}] {} state {st:?}", vm.object_name(t));
                            for i in 0..6u32 {
                                if let Ok(grim_object::Value::Str(c)) = vm.get_prop_at(t, "aPendingCues", i) {
                                    if !c.is_empty() {
                                        out!("        aPendingCues[{i}] = {c}");
                                    }
                                }
                            }
                            for prop in ["lastCommand", "nPendingCues"] {
                                if let Ok(v) = vm.get_prop(t, prop) {
                                    out!("        {prop} = {v:?}");
                                }
                            }
                            // Who owes the cue: an actor a cut command was issued to keeps the
                            // script it has to answer in `CutNotifyActor`, so the ones still
                            // holding one are what the thread is waiting for. Its state says
                            // what it is doing instead of answering.
                            for a in vm.actors.clone() {
                                if vm.is_deleted(a) {
                                    continue;
                                }
                                let owes = matches!(vm.get_prop(a, "CutNotifyActor"), Ok(grim_object::Value::Object(Some(o))) if o == t);
                                if !owes {
                                    continue;
                                }
                                let class = vm
                                    .class_of(a)
                                    .map(|c| vm.world.names.str(vm.world.class(c).name).to_string())
                                    .unwrap_or_default();
                                let anim = match vm.get_prop(a, "AnimSequence") {
                                    Ok(grim_object::Value::Name(n)) => vm.world.names.str(n).to_string(),
                                    other => format!("{other:?}"),
                                };
                                let cue = match vm.get_prop(a, "sCutNotifyCue") {
                                    Ok(grim_object::Value::Str(c)) => c,
                                    _ => String::new(),
                                };
                                let rest: Vec<String> = ["bAnimFinished", "AnimRate", "AnimFrame", "bAnimLoop"]
                                    .into_iter()
                                    .map(|p| format!("{p} {:?}", vm.get_prop(a, p).ok()))
                                    .collect();
                                out!(
                                    "        waiting on {} ({class}) owes '{cue}' state {:?} anim '{anim}' {}",
                                    vm.object_name(a),
                                    vm.state_name(a),
                                    rest.join(" ")
                                );
                            }
                        }
                    }
                    format!("still running after {seconds:.1}s")
                }
            };
            out!(
                "{map:24} {name:14} {end}{}{}",
                match &game_state {
                    Some(s) => format!(" (game state {s})"),
                    None => String::new(),
                },
                if placed { " (player put at the cutscene)" } else { "" }
            );
            let mut new: Vec<(String, usize)> = vm
                .warnings
                .iter()
                .map(|(w, n)| (w.clone(), n - warned.get(w).copied().unwrap_or(0)))
                .filter(|(_, n)| *n > 0)
                .collect();
            new.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            for (w, n) in new.iter().take(8) {
                out!("      {n:5} {w}");
            }
            for (n, e) in &report.errors {
                out!("      error x{} {n}  [{}]", e.0, e.1);
            }
            for (n, c) in &report.missing_natives {
                out!("      missing native x{c} {n}");
            }
            // What the cutscene itself said: its own complaints name what it could not do.
            for line in vm.log.iter().skip(before.len()) {
                if line.contains("Error") || line.contains("error") || line.contains("not ") || line.contains("Unknown") || line.contains("unknown") {
                    out!("      log {line}");
                }
            }
        }
    }
    out!("{played} cutscenes played, {} did not end", unfinished.len());
    for u in &unfinished {
        out!("  unfinished {u}");
    }
    Ok(())
}

/// A byte property of an export, read straight from the package for a diagnostic.
fn read_byte(pkg: &Package, export: u32, name: &str) -> Option<u8> {
    let obj = grim_assets::load_export(pkg, export).ok()?;
    let p = obj.base.properties.iter().flatten().find(|p| pkg.name(p.name).eq_ignore_ascii_case(name))?;
    match p.value {
        grim_package::property::PropertyValue::Byte(b) => Some(b),
        _ => None,
    }
}

/// A vector property of an export, read straight from the package for a diagnostic.
fn read_vector(pkg: &Package, export: u32, name: &str) -> Option<[f32; 3]> {
    let obj = grim_assets::load_export(pkg, export).ok()?;
    let p = obj.base.properties.iter().flatten().find(|p| pkg.name(p.name).eq_ignore_ascii_case(name))?;
    match &p.value {
        grim_package::property::PropertyValue::Struct { value: grim_package::property::StructValue::Vector(v), .. } => Some([v.x, v.y, v.z]),
        _ => None,
    }
}

/// A float property of an export, read straight from the package for a diagnostic.
fn read_float(pkg: &Package, export: u32, name: &str) -> Option<f32> {
    let obj = grim_assets::load_export(pkg, export).ok()?;
    let p = obj.base.properties.iter().flatten().find(|p| pkg.name(p.name).eq_ignore_ascii_case(name))?;
    match p.value {
        grim_package::property::PropertyValue::Float(f) => Some(f),
        _ => None,
    }
}
