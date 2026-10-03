//! Turning level geometry into renderable meshes.

use std::collections::HashMap;
use std::sync::Arc;

use grim_assets::model::Model;
use grim_assets::texture::{decode_mip, Texture};
use grim_assets::{load_export, Asset};
use grim_render::{Blend, MeshData, Renderer, Section, TextureData, TextureId, Vertex};
use grim_package::{Library, ObjectRef, Package};

use crate::{dot, sub as sub3, Vec3};

/// `PF_Invisible`: surfaces the engine never draws.
pub const PF_INVISIBLE: u32 = 0x0000_0001;
/// `PF_Masked`: palette index 0 is transparent.
pub const PF_MASKED: u32 = 0x0000_0002;
/// `PF_TwoSided`: the surface is drawn from both sides.
pub const PF_TWO_SIDED: u32 = 0x0000_0100;
/// `PF_Translucent`: the surface adds its colour.
pub const PF_TRANSLUCENT: u32 = 0x0000_0004;
/// `PF_Modulated`: the surface multiplies what is behind it.
pub const PF_MODULATED: u32 = 0x0000_0040;
/// `PF_FakeBackdrop`: the surface is a window onto the sky rather than a wall.
pub const PF_FAKE_BACKDROP: u32 = 0x0000_0080;
/// `PF_Mirrored`: the surface shows the level reflected in it.
pub const PF_MIRRORED: u32 = 0x0800_0000;
/// `PF_Unlit`: drawn at face value, with no light of its own. The sky is made of these.
pub const PF_UNLIT: u32 = 0x0040_0000;
/// `PF_AutoUPan`: the texture slides along U on its own, at the zone's `TexUPanSpeed`.
pub const PF_AUTO_U_PAN: u32 = 0x0000_0200;
/// `PF_AutoVPan`: the same along V, at the zone's `TexVPanSpeed`.
pub const PF_AUTO_V_PAN: u32 = 0x0000_0400;
/// Unreal's `PF_HighShadowDetail`, which this game uses, together with `PF_MODULATED`, to mark
/// the surfaces Lumos brings out. Hypothesis, from the maps: in all eighteen that have Lumos
/// the surfaces with both bits are exactly the modulated ones, and they stand where a Lumos
/// mover does (the wall `LumosDoor01` of `Adv1Willow` opens has two, in its own opening);
/// the only other surfaces carrying the bit, a hatch in the Quidditch maps, are not modulated.
/// `PlayerPawn` says the engine "will look at bLumosOn and fLumosRadius to determine if it
/// should calculate the lumos surface alpha values".
pub const PF_LUMOS: u32 = 0x0080_0000;

/// Whether a surface is one Lumos brings out.
pub fn is_lumos(flags: u32) -> bool {
    flags & (PF_LUMOS | PF_MODULATED) == PF_LUMOS | PF_MODULATED
}

/// Blend mode of a surface from its poly flags.
pub fn blend_of(poly_flags: u32) -> Blend {
    if poly_flags & PF_TRANSLUCENT != 0 {
        Blend::Translucent
    } else if poly_flags & PF_MODULATED != 0 {
        Blend::Modulated
    } else {
        Blend::Opaque
    }
}

/// Decodes a texture (first mip) and uploads it, keeping one GPU texture per object.
pub struct TextureCache {
    uploaded: HashMap<(String, u32), Option<(TextureId, f32, f32, bool)>>,
    /// The textures the engine draws itself, by the id they were given.
    running: HashMap<TextureId, crate::procedural::Procedural>,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TextureCache {
    pub fn new() -> Self {
        Self { uploaded: HashMap::new(), running: HashMap::new() }
    }

    /// Texture of an already resolved object.
    pub fn get_key(&mut self, renderer: &mut dyn Renderer, lib: &Library, key: (&Arc<Package>, u32)) -> Option<(TextureId, f32, f32, bool)> {
        let cache_key = (key.0.name.clone(), key.1);
        if let Some(v) = self.uploaded.get(&cache_key) {
            return *v;
        }
        let value = self.upload(renderer, lib, key.0, key.1);
        self.uploaded.insert(cache_key, value);
        value
    }

    /// Returns the texture id, its size and whether it is masked.
    pub fn get(
        &mut self,
        renderer: &mut dyn Renderer,
        lib: &Library,
        pkg: &Arc<Package>,
        r: ObjectRef,
    ) -> Option<(TextureId, f32, f32, bool)> {
        let handle = match lib.resolve(pkg, r) {
            Ok(h) => h,
            Err(e) => {
                if std::env::var_os("GRIM_TRACE_TEXTURE").is_some() {
                    eprintln!("texture {r:?} of {} did not resolve: {e}", pkg.name);
                }
                return None;
            }
        };
        let key = (handle.package.name.clone(), handle.export);
        if let Some(v) = self.uploaded.get(&key) {
            return *v;
        }
        let value = self.upload(renderer, lib, &handle.package, handle.export);
        self.uploaded.insert(key, value);
        value
    }

    fn upload(&mut self, renderer: &mut dyn Renderer, lib: &Library, pkg: &Arc<Package>, export: u32) -> Option<(TextureId, f32, f32, bool)> {
        let obj = load_export(pkg, export).ok()?;
        let tex: &Texture = match &obj.asset {
            Asset::Texture(t) => t,
            Asset::ProceduralTexture(p) => &p.texture,
            _ => return None,
        };
        let mip = tex.mips.first()?;
        let masked = obj.base.bool_property(pkg, "bMasked").unwrap_or(false);
        let palette = match obj.base.property(pkg, "Palette", 0) {
            Some(grim_package::property::PropertyValue::Object(p)) => {
                let h = lib.resolve(pkg, *p).ok()?;
                match load_export(&h.package, h.export).ok()?.asset {
                    Asset::Palette(pal) => Some(pal),
                    _ => None,
                }
            }
            _ => None,
        };
        let (w, h) = (mip.u_size as f32, mip.v_size as f32);
        // Unreal's masked rendering treats palette index 0 as transparent; whether the alpha
        // test runs is decided per draw (surface flags or the actor's Style).
        let running = match &obj.asset {
            // A procedural texture is stored with no pixels: the engine draws it, so it starts
            // blank at its own size and is rewritten every frame from then on.
            Asset::ProceduralTexture(p) => {
                crate::procedural::Procedural::new(p, settings_of(&obj, pkg), palette_colours(palette.as_ref()), drops_of(&obj, pkg), self.source_of(lib, pkg, &obj))
            }
            _ => None,
        };
        let img = match &running {
            Some(p) => grim_assets::texture::Rgba { width: p.width as u32, height: p.height as u32, pixels: p.rgba() },
            None => decode_mip(mip, tex.format, palette.as_ref(), true).ok()?,
        };
        // The smaller copies the picture carries. A texture the engine draws itself has none:
        // it is rewritten every frame.
        let smaller: Vec<Vec<u8>> = match running {
            Some(_) => Vec::new(),
            None => tex
                .mips
                .iter()
                .skip(1)
                .map_while(|m| decode_mip(m, tex.format, palette.as_ref(), true).ok().map(|i| i.pixels))
                .collect(),
        };
        let id = renderer.create_texture(TextureData {
            width: img.width,
            height: img.height,
            rgba: &img.pixels,
            mips: &smaller,
        });
        if let Some(p) = running {
            if std::env::var_os("GRIM_TRACE_PROCEDURAL").is_some() {
                eprintln!("running {:?} {}.{}", p.kind, pkg.name, pkg.object_name(grim_package::ObjectRef::Export(export)));
            }
            self.running.insert(id, p);
        }
        Some((id, w.max(1.0), h.max(1.0), masked))
    }

    /// What a glass texture bends: its `SourceTexture`, decoded to pixels.
    fn source_of(&self, lib: &Library, pkg: &Arc<Package>, obj: &grim_assets::LoadedObject) -> Option<grim_assets::texture::Rgba> {
        let Some(grim_package::property::PropertyValue::Object(r)) = obj.base.property(pkg, "SourceTexture", 0) else {
            return None;
        };
        let handle = lib.resolve(pkg, *r).ok()?;
        let source = load_export(&handle.package, handle.export).ok()?;
        let Asset::Texture(t) = &source.asset else { return None };
        let mip = t.mips.first()?;
        let palette = match source.base.property(&handle.package, "Palette", 0) {
            Some(grim_package::property::PropertyValue::Object(p)) => {
                let h = lib.resolve(&handle.package, *p).ok()?;
                match load_export(&h.package, h.export).ok()?.asset {
                    Asset::Palette(pal) => Some(pal),
                    _ => None,
                }
            }
            _ => None,
        };
        decode_mip(mip, t.format, palette.as_ref(), true).ok()
    }

    /// Steps every texture the engine draws itself and hands the new pixels to the renderer.
    ///
    /// Only what the last frame showed moves on, which is how Unreal does it too: a procedural
    /// texture is updated when it is drawn. Each step works over every texel of it and then
    /// sends the whole picture to the renderer, and ticking all the fires and waters of
    /// `Adv1Willow` whether they were in sight or not cost over twenty milliseconds a frame.
    pub fn tick(&mut self, renderer: &mut dyn Renderer, dt: f32) {
        let shown = renderer.textures_drawn().cloned();
        let (mut n, mut px, mut ts, mut tr, mut tu) = (0, 0, 0.0f32, 0.0f32, 0.0f32);
        let trace = std::env::var_os("GRIM_TRACE_PROC").is_some();
        for (&id, p) in &mut self.running {
            if shown.as_ref().is_some_and(|s| !s.contains(&id)) {
                continue;
            }
            let t = web_time::Instant::now();
            let moved = p.tick(dt);
            ts += t.elapsed().as_secs_f32() * 1000.0;
            if moved {
                n += 1;
                px += p.width * p.height;
                let t = web_time::Instant::now();
                let pixels = p.rgba();
                tr += t.elapsed().as_secs_f32() * 1000.0;
                let t = web_time::Instant::now();
                renderer.update_texture(
                    id,
                    TextureData { width: p.width as u32, height: p.height as u32, rgba: &pixels, mips: &[] },
                );
                tu += t.elapsed().as_secs_f32() * 1000.0;
            }
        }
        if trace {
            eprintln!("PROC {n} of {} textures, {px} texels, dt {dt:.4}: step {ts:.1} rgba {tr:.1} upload {tu:.1}", self.running.len());
        }
    }
}

/// The palette as plain colours, which a fire looks its heat up in.
fn palette_colours(palette: Option<&grim_assets::palette::Palette>) -> Vec<[u8; 4]> {
    palette.map_or_else(Vec::new, |p| p.colors.iter().map(|c| [c.r, c.g, c.b, c.a]).collect())
}

/// The settings a procedural texture carries, by the names the file gives them.
fn settings_of(obj: &grim_assets::LoadedObject, pkg: &Arc<Package>) -> crate::procedural::Settings {
    let byte = |name: &str| match obj.base.property(pkg, name, 0) {
        Some(grim_package::property::PropertyValue::Byte(b)) => *b,
        _ => 0,
    };
    crate::procedural::Settings {
        render_heat: byte("RenderHeat"),
        rising: obj.base.bool_property(pkg, "bRising").unwrap_or(false),
        parametric: obj.base.bool_property(pkg, "bParametric").unwrap_or(false),
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
    }
}

/// The drops a water texture carries. They sit in a fixed length array of eight byte structs
/// the property reader keeps whole, so they are read straight out of those bytes.
fn drops_of(obj: &grim_assets::LoadedObject, pkg: &Arc<Package>) -> Vec<crate::procedural::Drop> {
    let count = obj.base.int_property(pkg, "NumDrops").unwrap_or(0).max(0) as u32;
    (0..count)
        .filter_map(|i| {
            let value = obj.base.property(pkg, "Drops", i)?;
            let grim_package::property::PropertyValue::Struct { value: inner, .. } = value else { return None };
            let grim_package::property::StructValue::Unparsed(bytes) = inner else { return None };
            let b = &bytes.bytes;
            (b.len() >= 8).then(|| crate::procedural::Drop { kind: b[0], depth: b[1], x: b[2], y: b[3], rate: b[7] })
        })
        .collect()
}

/// The editor polygon a node of a brush was cut from: the one on the node's plane that holds
/// it. A mover's tree keeps only one surface per plane, so the picture on a face and the band
/// it sits in survive nowhere else: the front of a secret door in `EntryHall_hub` is one
/// surface and three nodes (stone, portrait, stone), one polygon each. The tree still says
/// where the pieces are and carries the baked light; the polygons say what they show.
fn poly_of_node<'a>(polys: &'a [grim_assets::polys::Poly], model: &Model, node: &grim_assets::model::BspNode) -> Option<&'a grim_assets::polys::Poly> {
    let v3 = |v: grim_package::property::Vector| [v.x, v.y, v.z];
    let normal: Vec3 = [node.plane.x, node.plane.y, node.plane.z];
    let mut centre = [0.0; 3];
    let mut count = 0.0;
    for k in 0..node.num_vertices as i32 {
        let Some(vert) = model.verts.get((node.i_vert_pool + k) as usize) else { continue };
        let Some(p) = model.points.get(vert.p_vertex.max(0) as usize) else { continue };
        for (i, c) in [p.x, p.y, p.z].into_iter().enumerate() {
            centre[i] += c;
        }
        count += 1.0;
    }
    if count == 0.0 {
        return None;
    }
    centre = centre.map(|c| c / count);
    // The two axes to compare in: whichever two the plane is least steep along.
    let up = (0..3).max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs())).unwrap_or(2);
    let (ax, ay) = ([1, 0, 0][up], [2, 2, 1][up]);
    for poly in polys {
        if dot(v3(poly.normal), normal) < 0.99 || poly.vertices.len() < 3 {
            continue;
        }
        if dot(sub3(v3(poly.base), centre), normal).abs() > 0.5 {
            continue;
        }
        // Point in polygon, by the crossing count along one axis.
        let mut inside = false;
        let mut j = poly.vertices.len() - 1;
        for i in 0..poly.vertices.len() {
            let (a, b) = (v3(poly.vertices[i]), v3(poly.vertices[j]));
            if (a[ay] > centre[ay]) != (b[ay] > centre[ay]) {
                let t = (centre[ay] - a[ay]) / (b[ay] - a[ay]);
                if centre[ax] < a[ax] + t * (b[ax] - a[ax]) {
                    inside = !inside;
                }
            }
            j = i;
        }
        if inside {
            return Some(poly);
        }
    }
    None
}

/// Builds one mesh for the level's BSP surfaces, one section per texture.
pub fn level_mesh(
    renderer: &mut dyn Renderer,
    lib: &Library,
    pkg: &Arc<Package>,
    model: &Model,
    textures: &mut TextureCache,
    lights: Option<&crate::lightmap::Lightmaps>,
    part: Part,
    zone_pan: &[[f32; 2]],
) -> MeshData {
    let mut mesh = MeshData::default();
    // Group triangles by texture so each section binds once. A surface whose texture slides
    // has its own sections: the offset is one number for the whole section.
    let mut by_texture: HashMap<(Option<TextureId>, bool, Blend, [u32; 2]), (Vec<u32>, bool, [f32; 2])> = HashMap::new();
    let mut vertices: Vec<Vertex> = Vec::new();
    // The level's own surfaces have no material slots to override.
    // A brush is drawn from the polygons the editor kept: its tree holds one surface per
    // plane, and the texture and mapping of each piece live only here.
    let polys_of_brush: Vec<grim_assets::polys::Poly> = match (part, model.polys) {
        (Part::Brush | Part::Mirror(_), ObjectRef::Export(p)) => match load_export(pkg, p).map(|l| l.asset) {
            Ok(Asset::Polys(polys)) => polys.polys,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let polys = &polys_of_brush[..];
    let pieces = crate::lightmap::Pieces::of(model, (!polys.is_empty()).then_some(polys));

    for node in &model.nodes {
        if node.num_vertices < 3 || node.i_surf < 0 {
            continue;
        }
        let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
        // On a brush the polygon the node was cut from is what it shows, so its flags are the
        // ones that count: a mover keeps invisible polygons on faces whose surface says
        // nothing (`Mover20` of `Adv11aCorridor` has four of them).
        let cut_from = poly_of_node(polys, model, node);
        let flags = cut_from.map_or(surf.poly_flags, |p| p.poly_flags);
        if flags & PF_INVISIBLE != 0 {
            continue;
        }
        match part {
            // The sky is built from its own zone's surfaces.
            Part::Sky(z) => {
                if node.i_zone[crate::FRONT] != z {
                    continue;
                }
            }
            // The world leaves the sky's own surfaces and its windows open, for it to show.
            Part::World { sky } => {
                if surf.poly_flags & (PF_FAKE_BACKDROP | PF_MIRRORED) != 0 || Some(node.i_zone[crate::FRONT]) == sky || is_lumos(flags) {
                    continue;
                }
            }
            Part::WorldZone { zone, sky } => {
                if surf.poly_flags & (PF_FAKE_BACKDROP | PF_MIRRORED) != 0 || Some(node.i_zone[crate::FRONT]) == sky || is_lumos(flags) {
                    continue;
                }
                if node.i_zone[crate::FRONT] != zone {
                    continue;
                }
            }
            // A brush shows everything it has but its mirrors, which are drawn reflecting.
            Part::Brush => {
                if flags & PF_MIRRORED != 0 {
                    continue;
                }
            }
            Part::Mirror(surf) => {
                if node.i_surf != surf {
                    continue;
                }
            }
            Part::Lumos { sky } => {
                if !is_lumos(flags) || Some(node.i_zone[crate::FRONT]) == sky {
                    continue;
                }
            }
            // The windows onto the sky, kept apart so they can still stop what is behind them.
            // The sky's own surfaces carry the flag too, and those are not windows.
            Part::Backdrop { sky } => {
                if surf.poly_flags & PF_FAKE_BACKDROP == 0 || Some(node.i_zone[crate::FRONT]) == sky {
                    continue;
                }
            }
        }
        // A mover's surfaces come out of the editor with no texture of their own: it stays on
        // the brush polygon the surface was cut from, which `i_brush_poly` names when the
        // polygon the node sits in cannot be found.
        let from_poly = || {
            if let Some(poly) = cut_from {
                return poly.texture;
            }
            let ObjectRef::Export(p) = model.polys else { return ObjectRef::Null };
            let Ok(Asset::Polys(polys)) = load_export(pkg, p).map(|l| l.asset) else { return ObjectRef::Null };
            polys.polys.get(surf.i_brush_poly.max(0) as usize).map_or(ObjectRef::Null, |poly| poly.texture)
        };
        let reference = match cut_from.map(|p| p.texture).unwrap_or(surf.texture) {
            ObjectRef::Null => from_poly(),
            other => other,
        };
        // A surface with no texture at all is drawn with the level's own, which `LevelInfo`
        // names: the doors of `EntryHall_hub` are cut from brushes nobody ever painted.
        let tex = match textures.get(renderer, lib, pkg, reference) {
            Some(t) => Some(t),
            None => default_texture(lib).and_then(|(p, e)| textures.get_key(renderer, lib, (&p, e))),
        };
        let (id, tw, th, masked) = match tex {
            Some((id, w, h, m)) => (Some(id), w, h, m || flags & PF_MASKED != 0),
            None => (None, 256.0, 256.0, false),
        };
        let v3 = |v: grim_package::property::Vector| [v.x, v.y, v.z];
        // The mapping goes with the picture: from the polygon the node was cut from when there
        // is one, and from the surface for the level's own geometry.
        let (base, tu, tv, pan_u, pan_v) = match cut_from {
            Some(poly) => (v3(poly.base), v3(poly.texture_u), v3(poly.texture_v), poly.pan_u, poly.pan_v),
            None => (
                v3(*model.points.get(surf.p_base.max(0) as usize).unwrap_or(&Default::default())),
                v3(*model.vectors.get(surf.v_texture_u.max(0) as usize).unwrap_or(&Default::default())),
                v3(*model.vectors.get(surf.v_texture_v.max(0) as usize).unwrap_or(&Default::default())),
                surf.pan_u,
                surf.pan_v,
            ),
        };
        let normal: Vec3 = [node.plane.x, node.plane.y, node.plane.z];

        let first = vertices.len() as u32;
        for k in 0..node.num_vertices as i32 {
            let Some(vert) = model.verts.get((node.i_vert_pool + k) as usize) else { continue };
            let Some(p) = model.points.get(vert.p_vertex as usize) else { continue };
            let p = v3(*p);
            let d = sub3(p, base);
            vertices.push(Vertex {
                position: p,
                uv: [(dot(d, tu) + pan_u as f32) / tw, (dot(d, tv) + pan_v as f32) / th],
                normal,
                color: [1.0; 3],
                uv2: lights.map_or([0.0; 2], |l| {
                    if flags & PF_UNLIT != 0 {
                        l.white_uv()
                    } else {
                        match pieces.of_node(node, cut_from) {
                            Some(piece) => l.atlas_uv(piece, pieces.coords(piece, p).unwrap_or([0.0; 2])),
                            None => l.white_uv(),
                        }
                    }
                }),
            });
        }
        let added = vertices.len() as u32 - first;
        let two_sided = flags & PF_TWO_SIDED != 0;
        // How fast the texture slides over this surface, in uv per second. The speed belongs to
        // the zone the surface is in and is counted in texels, as the surface's own pan is.
        let speed = zone_pan.get(node.i_zone[crate::FRONT] as usize).copied().unwrap_or([0.0; 2]);
        let pan = [
            if flags & PF_AUTO_U_PAN != 0 { speed[0] / tw } else { 0.0 },
            if flags & PF_AUTO_V_PAN != 0 { speed[1] / th } else { 0.0 },
        ];
        let entry = by_texture
            .entry((id, two_sided, blend_of(flags), pan.map(f32::to_bits)))
            .or_insert_with(|| (Vec::new(), masked, pan));
        for k in 1..added.saturating_sub(1) {
            entry.0.extend([first, first + k, first + k + 1]);
        }
    }

    mesh.vertices = vertices;
    for ((texture, two_sided, blend, _), (indices, masked, pan)) in by_texture {
        mesh.sections.push(Section {
            first_index: mesh.indices.len() as u32,
            index_count: indices.len() as u32,
            texture,
            masked,
            material: u32::MAX,
            two_sided,
            blend,
            pan,
        });
        mesh.indices.extend(indices);
    }
    mesh
}

/// Mesh of a brush actor (a `Mover`'s `Brush`), in the brush's own space minus its pivot.
/// Brush polygons carry their own texture axes, so they are built from the `Polys` the brush
/// keeps rather than from its BSP.
/// A brush's editor polygons, if it has them.
pub fn brush_polys(pkg: &Arc<Package>, model: &Model) -> Vec<grim_assets::polys::Poly> {
    let ObjectRef::Export(p) = model.polys else { return Vec::new() };
    match load_export(pkg, p).map(|l| l.asset) {
        Ok(Asset::Polys(polys)) => polys.polys,
        _ => Vec::new(),
    }
}

/// A brush's polygons as the editor kept them, which is what a mover shows: its tree holds one
/// surface per plane, so the picture on a door and where it sits only survive here.
pub fn brush_mesh(
    renderer: &mut dyn Renderer,
    lib: &Library,
    pkg: &Arc<Package>,
    model: &Model,
    pivot: Vec3,
    textures: &mut TextureCache,
    lights: Option<&crate::lightmap::Lightmaps>,
    part: Part,
) -> Option<MeshData> {
    if std::env::var_os("GRIM_TRACE_BRUSH").is_some() {
        eprintln!(
            "brush nodes {} surfs {} verts {} lightmaps {} lightbits {} points {}",
            model.nodes.len(), model.surfs.len(), model.verts.len(), model.light_map.len(), model.light_bits.len(), model.points.len()
        );
    }
    let ObjectRef::Export(polys) = model.polys else { return None };
    let Asset::Polys(polys) = load_export(pkg, polys).ok()?.asset else { return None };
    let mut data = MeshData::default();
    let mut by_texture: HashMap<(Option<TextureId>, bool, Blend), (Vec<u32>, bool)> = HashMap::new();

    for (index, poly) in polys.polys.iter().enumerate() {
        if poly.vertices.len() < 3 || poly.poly_flags & PF_INVISIBLE != 0 {
            continue;
        }
        match part {
            // A brush shows everything it has but its mirrors, which are drawn reflecting.
            Part::Brush => {
                if poly.poly_flags & PF_MIRRORED != 0 {
                    continue;
                }
            }
            Part::Mirror(only) => {
                if index as i32 != only {
                    continue;
                }
            }
            _ => {}
        }
        let (id, tw, th, masked) = match textures.get(renderer, lib, pkg, poly.texture) {
            Some((id, w, h, m)) => (Some(id), w, h, m || poly.poly_flags & PF_MASKED != 0),
            None => (None, 256.0, 256.0, false),
        };
        let v3 = |v: grim_package::property::Vector| [v.x, v.y, v.z];
        let (base, tu, tv) = (v3(poly.base), v3(poly.texture_u), v3(poly.texture_v));
        let normal = v3(poly.normal);
        let lit = lights.and_then(|l| Some((l, usize::try_from(poly.i_brush_poly).ok()?)));
        let first = data.vertices.len() as u32;
        for p in &poly.vertices {
            let p = v3(*p);
            let d = sub3(p, base);
            data.vertices.push(Vertex {
                position: [p[0] - pivot[0], p[1] - pivot[1], p[2] - pivot[2]],
                uv: [(dot(d, tu) + poly.pan_u as f32) / tw, (dot(d, tv) + poly.pan_v as f32) / th],
                normal,
                color: [1.0; 3],
                uv2: match lit {
                    Some((l, piece)) if poly.poly_flags & PF_UNLIT == 0 => {
                        let pieces = crate::lightmap::Pieces::of(model, Some(&polys.polys));
                        l.atlas_uv(piece, pieces.coords(piece, p).unwrap_or([0.0; 2]))
                    }
                    Some((l, _)) => l.white_uv(),
                    None => [0.0; 2],
                },
            });
        }
        let added = data.vertices.len() as u32 - first;
        let two_sided = poly.poly_flags & PF_TWO_SIDED != 0;
        let entry = by_texture.entry((id, two_sided, blend_of(poly.poly_flags))).or_insert_with(|| (Vec::new(), masked));
        for k in 1..added.saturating_sub(1) {
            entry.0.extend([first, first + k, first + k + 1]);
        }
    }

    for ((texture, two_sided, blend), (indices, masked)) in by_texture {
        data.sections.push(Section {
            first_index: data.indices.len() as u32,
            index_count: indices.len() as u32,
            texture,
            masked,
            material: u32::MAX,
            two_sided,
            blend,
            pan: [0.0; 2],
        });
        data.indices.extend(indices);
    }
    (!data.vertices.is_empty()).then_some(data)
}

/// Which part of the level a mesh is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Everything but the sky, which is left open where it shows through.
    World { sky: Option<u8> },
    /// The same, but only the surfaces of one zone: the map's own zones are what the engine
    /// draws by, and what a zone cannot see is never handed to the renderer.
    WorldZone { zone: u8, sky: Option<u8> },
    /// Only that zone, drawn as the backdrop.
    Sky(u8),
    /// Only the surfaces that open onto the sky. They are drawn into the depth buffer alone,
    /// so the sky shows through them and the rest of the level does not.
    Backdrop { sky: Option<u8> },
    /// Only that surface, which is a mirror: it is drawn with the level reflected in its plane.
    Mirror(i32),
    /// The surfaces Lumos brings out, which are only there while it is lit.
    Lumos { sky: Option<u8> },
    /// A brush of its own: everything it has, mirrors included. A mover carries its own piece
    /// of geometry and nothing else shows through it, so nothing is left open.
    Brush,
}

/// Surfaces of the level that are mirrors, with the plane each one reflects about.
pub fn mirror_surfaces(model: &Model) -> Vec<(i32, [f32; 4])> {
    let mut out: Vec<(i32, [f32; 4])> = Vec::new();
    for node in &model.nodes {
        if node.i_surf < 0 || out.iter().any(|(s, _)| *s == node.i_surf) {
            continue;
        }
        let Some(surf) = model.surfs.get(node.i_surf as usize) else { continue };
        if surf.poly_flags & PF_MIRRORED == 0 {
            continue;
        }
        out.push((node.i_surf, [node.plane.x, node.plane.y, node.plane.z, node.plane.w]));
    }
    out
}

/// The zone the level's sky lives in, if it has one: the zone whose info actor is a
/// `SkyZoneInfo`. Its surfaces are drawn as the backdrop instead of as part of the world.
pub fn sky_zone_of(pkg: &Arc<Package>, model: &Model) -> Option<u8> {
    model.zones.iter().position(|z| {
        matches!(z.zone_actor, ObjectRef::Export(e) if pkg.class_name(ObjectRef::Export(e)).contains("SkyZoneInfo"))
    }).map(|i| i as u8)
}

/// What the engine draws a surface with when it has no texture of its own:
/// `LevelInfo.DefaultTexture`, which every level of the game leaves at `Engine.DefaultTexture`.
fn default_texture(lib: &Library) -> Option<(Arc<Package>, u32)> {
    let pkg = lib.load("Engine").ok()?;
    let export = (0..pkg.exports.len() as u32)
        .find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Texture" && pkg.object_name(ObjectRef::Export(i)) == "DefaultTexture")?;
    Some((pkg, export))
}

/// The level's `Model`, if the package has one.
pub fn level_model(pkg: &Arc<Package>) -> Option<(u32, Model)> {
    let level = (0..pkg.exports.len() as u32).find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level")?;
    let Asset::Level(l) = load_export(pkg, level).ok()?.asset else { return None };
    let ObjectRef::Export(m) = l.model else { return None };
    match load_export(pkg, m).ok()?.asset {
        Asset::Model(model) => Some((m, *model)),
        _ => None,
    }
}

/// An actor's mesh with what a pose needs to move it: the mesh point each vertex came from,
/// and how the vertices were placed.
pub struct PosedMesh {
    pub data: MeshData,
    /// Mesh point index per vertex.
    pub sources: Vec<u32>,
    pub centre: Vec3,
    pub scale: Vec3,
    /// How far the bottom of the mesh's stored box is below the middle it was centred on,
    /// already scaled: what an actor that stands on its feet is lowered by.
    pub low: f32,
}

impl PosedMesh {
    /// Rewrites the vertex positions from posed points, keeping everything else.
    pub fn pose(&mut self, points: &[Vec3]) {
        for (v, &source) in self.data.vertices.iter_mut().zip(&self.sources) {
            let Some(p) = points.get(source as usize) else { continue };
            v.position = [0, 1, 2].map(|i| (p[i] - self.centre[i]) * self.scale[i]);
        }
    }
}

/// Mesh of an actor in its reference pose, one section per material.
pub fn actor_mesh(
    renderer: &mut dyn Renderer,
    lib: &Library,
    pkg: &Arc<Package>,
    export: u32,
    textures: &mut TextureCache,
) -> Option<PosedMesh> {
    let asset = load_export(pkg, export).ok()?.asset;
    let (lod, points, ext_wedges) = match &asset {
        Asset::SkeletalMesh(m) => (
            &m.lod,
            m.points.iter().map(|p| [p.x, p.y, p.z]).collect::<Vec<_>>(),
            m.ext_wedges.iter().map(|w| (w.i_vertex as usize, [w.u, w.v])).collect::<Vec<_>>(),
        ),
        Asset::LodMesh(m) => {
            // Frame 0 of the vertex animation, unpacked from 11:11:10 bits.
            let frame: Vec<[f32; 3]> = m
                .mesh
                .verts
                .iter()
                .take(m.mesh.frame_verts.max(0) as usize)
                .map(|&v| [(((v << 21) as i32) >> 21) as f32, (((v << 10) as i32) >> 21) as f32, ((v as i32) >> 22) as f32])
                .collect();
            (m.as_ref(), frame, Vec::new())
        }
        _ => return None,
    };
    let mesh = &lod.mesh;
    let scale = [mesh.scale.x, mesh.scale.y, mesh.scale.z];
    // Meshes are drawn with the centre of their stored bounding box at the actor's location.
    // Checked on 392 props resting on the floor: 0.6 units of median error, against 9.8 when
    // drawing the vertices as they come.
    let b = mesh.primitive.bounding_box;
    let centre = [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0, (b.min.z + b.max.z) / 2.0];
    // UV scale: byte wedges address a 256-texel space, ext wedges are already normalized.
    let mut data = MeshData::default();
    let mut sources: Vec<u32> = Vec::new();
    // One section per material, so an actor's skins can replace each one.
    let mut by_material: HashMap<u32, (Vec<u32>, Option<TextureId>, bool, bool, Blend)> = HashMap::new();

    // Normals are averaged per mesh vertex, not per face corner, so the shading stays smooth;
    // they are negated because the model matrix mirrors the mesh along Y.
    let mut normals = vec![[0.0f32; 3]; points.len()];
    for face in &lod.faces {
        let corner = |w: u16| -> Option<usize> {
            if ext_wedges.is_empty() {
                lod.wedges.get(w as usize).map(|x| x.i_vertex as usize)
            } else {
                ext_wedges.get(w as usize).map(|x| x.0)
            }
        };
        let Some(v) = face.i_wedge.iter().map(|&w| corner(w)).collect::<Option<Vec<_>>>() else { continue };
        let p = [0, 1, 2].map(|k| points.get(v[k]).copied().unwrap_or_default());
        let (a, b) = (sub3(p[1], p[0]), sub3(p[2], p[0]));
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        for &i in &v {
            if let Some(slot) = normals.get_mut(i) {
                for (s, c) in slot.iter_mut().zip(n) {
                    *s -= c;
                }
            }
        }
    }
    for n in &mut normals {
        let len = dot(*n, *n).sqrt();
        *n = if len > 0.0 { [n[0] / len, n[1] / len, n[2] / len] } else { [0.0, 0.0, 1.0] };
    }

    for face in &lod.faces {
        let material = lod.materials.get(face.material_index as usize);
        let tex_ref = material
            .and_then(|m| mesh.textures.get(m.texture_index.max(0) as usize))
            .or_else(|| mesh.textures.get(face.material_index as usize));
        // Each material carries its own poly flags, masking included.
        let material_masked = material.is_some_and(|m| m.poly_flags & PF_MASKED != 0);
        let (id, masked) = match tex_ref.and_then(|&t| textures.get(renderer, lib, pkg, t)) {
            Some((id, _, _, m)) => (Some(id), m || material_masked),
            None => (None, material_masked),
        };
        let first = data.vertices.len() as u32;
        for &w in &face.i_wedge {
            let (vertex, uv) = if !ext_wedges.is_empty() {
                let (v, uv) = *ext_wedges.get(w as usize)?;
                (v, uv)
            } else {
                // Byte wedge UVs address the whole texture in 0..255, whatever its size.
                let wedge = lod.wedges.get(w as usize)?;
                (wedge.i_vertex as usize, [wedge.tex_uv.u as f32 / 256.0, wedge.tex_uv.v as f32 / 256.0])
            };
            let p = *points.get(vertex)?;
            sources.push(vertex as u32);
            data.vertices.push(Vertex {
                position: [(p[0] - centre[0]) * scale[0], (p[1] - centre[1]) * scale[1], (p[2] - centre[2]) * scale[2]],
                uv,
                normal: normals.get(vertex).copied().unwrap_or([0.0, 0.0, 1.0]),
                color: [1.0; 3],
                uv2: [0.0; 2],
            });
        }
        let two_sided = material.is_some_and(|m| m.poly_flags & PF_TWO_SIDED != 0);
        let blend = material.map(|m| blend_of(m.poly_flags)).unwrap_or_default();
        let entry = by_material.entry(face.material_index as u32).or_insert_with(|| (Vec::new(), id, masked, two_sided, blend));
        entry.0.extend([first, first + 1, first + 2]);
    }
    let mut materials: Vec<_> = by_material.into_iter().collect();
    materials.sort_by_key(|(m, _)| *m);
    for (material, (indices, texture, masked, two_sided, blend)) in materials {
        data.sections.push(Section {
            first_index: data.indices.len() as u32,
            index_count: indices.len() as u32,
            texture,
            masked,
            material,
            two_sided,
            blend,
            pan: [0.0; 2],
        });
        data.indices.extend(indices);
    }
    let low = (b.min.z - centre[2]) * scale[2];
    Some(PosedMesh { data, sources, centre, scale, low })
}
