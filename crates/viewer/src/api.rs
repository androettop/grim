use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grim_assets::texture::TextureFormat;
use grim_assets::{load_export, Asset, LoadedObject};
use grim_package::hexdump::hexdump_around;
use grim_package::property::{PropertyValue, StructValue};
use grim_package::{Library, ObjectRef, Package};
use serde_json::{json, Value};

use crate::decode;
use crate::Reply;

pub struct State {
    root: PathBuf,
    lib: Library,
}

type Query = HashMap<String, String>;
type ApiResult = Result<Reply, String>;

const PF_INVISIBLE: u32 = 0x0000_0001;

impl State {
    pub fn new(root: PathBuf) -> Self {
        let lib = Library::open(&root);
        Self { root, lib }
    }

    pub fn handle(&self, route: &str, q: &Query) -> Reply {
        let r = match route {
            "packages" => self.packages(),
            "exports" => self.exports(q),
            "object" => self.object(q),
            "texture" => self.texture(q),
            "sound" => self.sound(q),
            "mesh" => self.mesh(q),
            "anim" => self.anim(q),
            "model" => self.model(q),
            _ => Err(format!("unknown route {route}")),
        };
        r.unwrap_or_else(|e| Reply::error(400, e))
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().replace('\\', "/")
    }

    fn package(&self, q: &Query) -> Result<(String, Arc<Package>), String> {
        let rel = q.get("p").ok_or("missing p")?;
        if rel.contains("..") {
            return Err("invalid path".into());
        }
        Ok((rel.clone(), self.lib.load_path(&self.root.join(rel))?))
    }

    fn export_index(q: &Query, pkg: &Package) -> Result<u32, String> {
        let i: u32 = q.get("i").ok_or("missing i")?.parse().map_err(|_| "invalid i")?;
        if (i as usize) < pkg.exports.len() { Ok(i) } else { Err("export out of range".into()) }
    }

    fn load(&self, q: &Query) -> Result<(String, Arc<Package>, u32, LoadedObject), String> {
        let (rel, pkg) = self.package(q)?;
        let i = Self::export_index(q, &pkg)?;
        let obj = load_export(&pkg, i).map_err(|e| e.to_string())?;
        Ok((rel, pkg, i, obj))
    }

    /// Resolves `r` (as seen from `pkg`) to `{p, i, name, class}` or `null`.
    fn ref_json(&self, pkg: &Arc<Package>, rel: &str, r: ObjectRef) -> Value {
        if r.is_null() {
            return Value::Null;
        }
        match self.lib.resolve(pkg, r) {
            Ok(h) => {
                let p = if Arc::ptr_eq(&h.package, pkg) {
                    rel.to_string()
                } else {
                    self.lib.path_of(&h.package.name).map(|p| self.rel(p)).unwrap_or_default()
                };
                let er = ObjectRef::Export(h.export);
                json!({"p": p, "i": h.export, "name": h.package.path_name(er), "class": h.package.class_name(er)})
            }
            Err(e) => json!({"unresolved": pkg.path_name(r), "error": e}),
        }
    }

    fn resolve_loaded(&self, pkg: &Arc<Package>, r: ObjectRef) -> Result<LoadedObject, String> {
        let h = self.lib.resolve(pkg, r)?;
        load_export(&h.package, h.export).map_err(|e| e.to_string())
    }

    fn packages(&self) -> ApiResult {
        let list: Vec<Value> = grim_testkit::package_files(&self.root)
            .iter()
            .map(|f| json!({"p": self.rel(f), "size": f.metadata().map(|m| m.len()).unwrap_or(0)}))
            .collect();
        Ok(Reply::json(json!(list)))
    }

    fn exports(&self, q: &Query) -> ApiResult {
        let (_, pkg) = self.package(q)?;
        let list: Vec<Value> = (0..pkg.exports.len() as u32)
            .map(|i| {
                let r = ObjectRef::Export(i);
                let (status, kind) = match load_export(&pkg, i) {
                    Ok(o) => match &o.asset {
                        Asset::Unparsed(_) => ("unparsed", "other"),
                        a => ("ok", kind_of(a)),
                    },
                    Err(_) => ("error", "other"),
                };
                json!({
                    "i": i,
                    "name": pkg.path_name(r),
                    "class": pkg.class_name(r),
                    "size": pkg.export(i).serial_size,
                    "status": status,
                    "kind": kind,
                })
            })
            .collect();
        Ok(Reply::json(json!(list)))
    }

    fn object(&self, q: &Query) -> ApiResult {
        let (rel, pkg) = self.package(q)?;
        let i = Self::export_index(q, &pkg)?;
        let r = ObjectRef::Export(i);
        let e = pkg.export(i);
        let mut out = json!({
            "name": pkg.path_name(r),
            "class": grim_assets::class_path(&pkg, i),
            "flags": format!("{:#010x}", e.flags),
            "offset": e.serial_offset,
            "size": e.serial_size,
        });
        match load_export(&pkg, i) {
            Ok(o) => {
                let props: Vec<Value> = o
                    .base
                    .properties
                    .iter()
                    .flatten()
                    .map(|p| {
                        json!({
                            "name": pkg.name(p.name),
                            "index": p.array_index,
                            "value": self.prop_value(&pkg, &rel, &p.value),
                        })
                    })
                    .collect();
                out["kind"] = json!(kind_of(&o.asset));
                out["status"] = json!(if matches!(o.asset, Asset::Unparsed(_)) { "unparsed" } else { "ok" });
                out["properties"] = json!(props);
                out["state_frame"] = json!(o.base.state_frame.as_ref().map(|s| format!("{s:?}")));
                out["debug"] = json!(limited_debug(&o.asset, 40_000));
                match &o.asset {
                    Asset::Texture(t) => out["texture"] = texture_info(t),
                    Asset::ProceduralTexture(p) => out["texture"] = texture_info(&p.texture),
                    Asset::Palette(p) => {
                        out["palette"] = json!(p.colors.iter().map(|c| [c.r, c.g, c.b, c.a]).collect::<Vec<_>>())
                    }
                    Asset::Font(f) => {
                        out["font"] = json!(f.pages.iter().map(|pg| json!({
                            "texture": self.ref_json(&pkg, &rel, pg.texture),
                            "chars": pg.characters.iter().map(|c| [c.start_u, c.start_v, c.u_size, c.v_size]).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>())
                    }
                    Asset::Sound(s) => {
                        out["sound"] = json!({
                            "format": pkg.name(s.format), "flags": format!("{:#x}", s.flags),
                            "duration": s.duration, "sample_rate": s.sample_rate,
                            "has_trailer": s.trailer.is_some(),
                        })
                    }
                    _ => {}
                }
            }
            Err(err) => {
                out["status"] = json!("error");
                out["kind"] = json!("other");
                out["error"] = json!(err.to_string());
                out["hexdump"] = json!(hexdump_around(pkg.bytes(), err.offset, 128, 128));
            }
        }
        Ok(Reply::json(out))
    }

    fn prop_value(&self, pkg: &Arc<Package>, rel: &str, v: &PropertyValue) -> Value {
        match v {
            PropertyValue::Object(r) | PropertyValue::Class(r) => self.ref_json(pkg, rel, *r),
            PropertyValue::Name(n) => json!(pkg.name(*n)),
            PropertyValue::Struct { name, value: StructValue::Unparsed(u) } => {
                json!(format!("{} {:02x?}", pkg.name(*name), u.bytes))
            }
            PropertyValue::Struct { name, value } => json!(format!("{} {value:?}", pkg.name(*name))),
            other => json!(format!("{other:?}")),
        }
    }

    fn texture(&self, q: &Query) -> ApiResult {
        let (_, pkg, _, obj) = self.load(q)?;
        let tex = match &obj.asset {
            Asset::Texture(t) => t,
            Asset::ProceduralTexture(p) => &p.texture,
            _ => return Err(format!("{} is not a texture", obj.class)),
        };
        let mip: usize = q.get("mip").and_then(|m| m.parse().ok()).unwrap_or(0);
        let comp = q.get("comp").is_some_and(|c| c == "1");
        let (mips, format) = if comp {
            (&tex.comp_mips, tex.comp_format.ok_or("no compressed mips")?)
        } else {
            (&tex.mips, tex.format)
        };
        let m = mips.get(mip).ok_or("mip out of range")?;
        if m.data.is_empty() {
            return Err("empty mip (procedural texture, generated at runtime)".into());
        }
        let palette = if format == TextureFormat::P8 {
            let r = match obj.base.property(&pkg, "Palette", 0) {
                Some(PropertyValue::Object(r)) => *r,
                _ => return Err("P8 texture without Palette property".into()),
            };
            match self.resolve_loaded(&pkg, r)?.asset {
                Asset::Palette(p) => Some(p),
                _ => return Err("Palette property is not a Palette".into()),
            }
        } else {
            None
        };
        let masked = obj.base.bool_property(&pkg, "bMasked").unwrap_or(false);
        let img = grim_assets::texture::decode_mip(m, format, palette.as_ref(), masked)?;
        Ok(Reply::bytes("image/png", decode::encode_png(&img)?))
    }

    fn sound(&self, q: &Query) -> ApiResult {
        let (_, pkg, _, obj) = self.load(q)?;
        let Asset::Sound(s) = &obj.asset else { return Err(format!("{} is not a sound", obj.class)) };
        match pkg.name(s.format).to_ascii_lowercase().as_str() {
            "wav" => Ok(Reply::bytes("audio/wav", s.data.clone())),
            "xa" => {
                let pcm = grim_assets::sound::decode_xa(s).map_err(|e| e.to_string())?;
                Ok(Reply::bytes("audio/wav", decode::wav_from_pcm16(&pcm, s.sample_rate, s.channels as u16)))
            }
            f => Err(format!("unknown sound format {f}")),
        }
    }

    fn mesh(&self, q: &Query) -> ApiResult {
        let (rel, pkg, _, obj) = self.load(q)?;
        let (lod, skel) = match &obj.asset {
            Asset::SkeletalMesh(s) => (&s.lod, Some(s)),
            Asset::LodMesh(l) => (l.as_ref(), None),
            _ => return Err(format!("{} is not a mesh", obj.class)),
        };
        let m = &lod.mesh;
        let mut out = json!({
            "textures": m.textures.iter().map(|&t| self.ref_json(&pkg, &rel, t)).collect::<Vec<_>>(),
            "materials": lod.materials.iter().map(|x| json!([x.texture_index, x.poly_flags])).collect::<Vec<_>>(),
            "faces": lod.faces.iter().flat_map(|f| [f.i_wedge[0], f.i_wedge[1], f.i_wedge[2], f.material_index]).collect::<Vec<_>>(),
            "special_faces": lod.special_faces.len(),
            "wedges": lod.wedges.iter().flat_map(|w| [w.i_vertex as u32, w.tex_uv.u as u32, w.tex_uv.v as u32]).collect::<Vec<_>>(),
            "scale": [m.scale.x, m.scale.y, m.scale.z],
            "origin": [m.origin.x, m.origin.y, m.origin.z],
            "rot_origin": [m.rot_origin.pitch, m.rot_origin.yaw, m.rot_origin.roll],
            "frame_verts": m.frame_verts,
            "anim_frames": m.anim_frames,
        });
        if let Some(s) = skel {
            out["kind"] = json!("skeletal");
            out["ext_wedges"] = json!(s.ext_wedges.iter().flat_map(|w| [w.i_vertex as f32, w.u, w.v]).collect::<Vec<_>>());
            out["points"] = json!(s.points.iter().flat_map(|p| [p.x, p.y, p.z]).collect::<Vec<_>>());
            out["local_points"] = json!(s.local_points.iter().flat_map(|p| [p.x, p.y, p.z]).collect::<Vec<_>>());
            out["bones"] = json!(s.ref_skeleton.iter().map(|b| {
                let o = b.bone_pos.orientation;
                let p = b.bone_pos.position;
                json!({"name": pkg.name(b.name), "parent": b.parent_index, "children": b.num_children,
                       "q": [o.x, o.y, o.z, o.w], "p": [p.x, p.y, p.z]})
            }).collect::<Vec<_>>());
            out["bone_weight_idx"] = json!(s.bone_weight_idx.iter().map(|w| [w.weight_index, w.number, w.detail_a, w.detail_b]).collect::<Vec<_>>());
            out["bone_weights"] = json!(s.bone_weights.iter().flat_map(|w| [w.point_index, w.bone_weight]).collect::<Vec<_>>());
            out["default_animation"] = self.ref_json(&pkg, &rel, s.default_animation);
        } else {
            out["kind"] = json!("lod");
            // Packed UE1 vertex: X 11 bits, Y 11 bits, Z 10 bits, all signed.
            out["frames"] = json!(m.verts.iter().flat_map(|&v| {
                let x = ((v << 21) as i32) >> 21;
                let y = ((v << 10) as i32) >> 21;
                let z = (v as i32) >> 22;
                [x as f32, y as f32, z as f32]
            }).collect::<Vec<_>>());
        }
        Ok(Reply::json(out))
    }

    fn anim(&self, q: &Query) -> ApiResult {
        let (_, pkg, _, obj) = self.load(q)?;
        let Asset::Animation(a) = &obj.asset else { return Err(format!("{} is not an animation", obj.class)) };
        Ok(Reply::json(json!({
            "bones": a.ref_bones.iter().map(|b| json!({"name": pkg.name(b.name), "flags": b.flags, "parent": b.parent_index})).collect::<Vec<_>>(),
            "moves": a.moves.iter().map(|m| json!({
                "root_speed": [m.root_speed_3d.x, m.root_speed_3d.y, m.root_speed_3d.z],
                "track_time": m.track_time, "start_bone": m.start_bone, "flags": m.flags,
                "bone_indices": m.bone_indices,
                "tracks": m.tracks.iter().map(|t| json!({"flags": t.flags, "a": t.num_rot_keys, "b": t.num_pos_keys, "c": t.num_time_keys, "x": t.pos_scale, "y": t.frame_time})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "seqs": a.anim_seqs.iter().map(|s| json!({
                "name": pkg.name(s.name), "groups": s.groups.iter().map(|g| pkg.name(*g)).collect::<Vec<_>>(),
                "start": s.start_frame, "frames": s.num_frames, "rate": s.rate,
                "notifys": s.notifys.iter().map(|n| json!([n.time, pkg.name(n.function)])).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "pool_a": a.rot_keys.concat(),
            "pool_b": a.pos_keys.concat(),
            "pool_c": a.key_frames,
        })))
    }

    fn model(&self, q: &Query) -> ApiResult {
        let (rel, pkg, _, obj) = self.load(q)?;
        let (model, level) = match obj.asset {
            Asset::Model(m) => (m, None),
            Asset::Level(l) => {
                let m = match self.resolve_loaded(&pkg, l.model)?.asset {
                    Asset::Model(m) => m,
                    _ => return Err("Level.Model is not a Model".into()),
                };
                (m, Some(l))
            }
            _ => return Err(format!("{} is not a Model or Level", obj.class)),
        };
        let mut tex_index: HashMap<ObjectRef, usize> = HashMap::new();
        let mut textures = Vec::new();
        let mut polys = Vec::new();
        let v3 = |v: grim_package::property::Vector| [v.x, v.y, v.z];
        for n in &model.nodes {
            if n.num_vertices == 0 || n.i_surf < 0 {
                continue;
            }
            let surf = model.surfs.get(n.i_surf as usize).ok_or("iSurf out of range")?;
            let t = *tex_index.entry(surf.texture).or_insert_with(|| {
                textures.push(self.ref_json(&pkg, &rel, surf.texture));
                textures.len() - 1
            });
            let base = v3(*model.points.get(surf.p_base as usize).ok_or("pBase out of range")?);
            let tu = v3(*model.vectors.get(surf.v_texture_u as usize).ok_or("vTextureU out of range")?);
            let tv = v3(*model.vectors.get(surf.v_texture_v as usize).ok_or("vTextureV out of range")?);
            let mut verts = Vec::new();
            let mut uvs = Vec::new();
            for k in 0..n.num_vertices as i32 {
                let vert = model.verts.get((n.i_vert_pool + k) as usize).ok_or("iVertPool out of range")?;
                let p = v3(*model.points.get(vert.p_vertex as usize).ok_or("pVertex out of range")?);
                let d = [p[0] - base[0], p[1] - base[1], p[2] - base[2]];
                let dot = |a: [f32; 3]| d[0] * a[0] + d[1] * a[1] + d[2] * a[2];
                verts.extend(p);
                uvs.extend([dot(tu) + surf.pan_u as f32, dot(tv) + surf.pan_v as f32]);
            }
            polys.push(json!({"t": t, "f": surf.poly_flags, "v": verts, "uv": uvs}));
        }
        let mut out = json!({"textures": textures, "polys": polys, "invisible_flag": PF_INVISIBLE});
        if let Some(l) = level {
            let actors: Vec<Value> = l
                .actors
                .iter()
                .filter(|a| !a.is_null())
                .filter_map(|&a| {
                    let ObjectRef::Export(e) = a else { return None };
                    let o = load_export(&pkg, e).ok()?;
                    let loc = match o.base.property(&pkg, "Location", 0) {
                        Some(PropertyValue::Struct { value: StructValue::Vector(v), .. }) => [v.x, v.y, v.z],
                        _ => [0.0, 0.0, 0.0],
                    };
                    Some(json!({"i": e, "class": pkg.class_name(a), "name": pkg.object_name(a), "loc": loc}))
                })
                .collect();
            out["actors"] = json!(actors);
        }
        Ok(Reply::json(out))
    }
}

fn texture_info(t: &grim_assets::Texture) -> Value {
    let mips = |m: &[grim_assets::texture::Mip]| m.iter().map(|m| json!([m.u_size, m.v_size, m.data.len()])).collect::<Vec<_>>();
    json!({
        "format": format!("{:?}", t.format),
        "comp_format": t.comp_format.map(|f| format!("{f:?}")),
        "mips": mips(&t.mips),
        "comp_mips": mips(&t.comp_mips),
    })
}

fn kind_of(a: &Asset) -> &'static str {
    match a {
        Asset::Texture(_) | Asset::ProceduralTexture(_) => "texture",
        Asset::Sound(_) => "sound",
        Asset::SkeletalMesh(_) | Asset::LodMesh(_) => "mesh",
        Asset::Animation(_) => "anim",
        Asset::Model(_) | Asset::Level(_) => "model",
        Asset::Palette(_) => "palette",
        Asset::Font(_) => "font",
        _ => "other",
    }
}

/// `{:#?}` capped at `limit` bytes; formatting stops as soon as the cap is hit.
fn limited_debug(v: &impl std::fmt::Debug, limit: usize) -> String {
    struct Capped {
        s: String,
        limit: usize,
    }
    impl std::fmt::Write for Capped {
        fn write_str(&mut self, x: &str) -> std::fmt::Result {
            if self.s.len() + x.len() > self.limit {
                let room = self.limit - self.s.len();
                let cut = (0..=room).rev().find(|&i| x.is_char_boundary(i)).unwrap_or(0);
                self.s.push_str(&x[..cut]);
                return Err(std::fmt::Error);
            }
            self.s.push_str(x);
            Ok(())
        }
    }
    let mut c = Capped { s: String::new(), limit };
    if write!(c, "{v:#?}").is_err() {
        c.s.push_str("\n… (truncated)");
    }
    c.s
}
