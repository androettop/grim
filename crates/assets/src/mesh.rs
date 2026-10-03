//! `Engine.Mesh` (per-frame vertices), `Engine.LodMesh` (+ progressive LOD) and
//! `Engine.SkeletalMesh` (used by HP2 for characters and props).

use grim_package::property::{read_name, read_object_ref, read_rotator, read_vector, Rotator, Vector};
use grim_package::{NameIndex, ObjectRef, Reader, Result};

use crate::lazy::read_lazy;
use crate::model::{read_array, section};
use crate::primitive::{read_box, read_sphere, BoundingBox, Primitive, Sphere};
use crate::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshUv {
    pub u: u8,
    pub v: u8,
}

#[derive(Debug, Clone)]
pub struct MeshTri {
    pub i_vertex: [u16; 3],
    pub tex: [MeshUv; 3],
    pub poly_flags: u32,
    pub texture_index: i32,
}

#[derive(Debug, Clone)]
pub struct AnimNotify {
    pub time: f32,
    pub function: NameIndex,
}

#[derive(Debug, Clone)]
pub struct MeshAnimSeq {
    pub name: NameIndex,
    pub group: NameIndex,
    pub start_frame: i32,
    pub num_frames: i32,
    pub notifys: Vec<AnimNotify>,
    pub rate: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct VertConnect {
    pub num_vert_triangles: i32,
    pub triangle_list_offset: i32,
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub primitive: Primitive,
    /// Packed vertices (11:11:10 bits in UE1), `frame_verts` per frame.
    pub verts: Vec<u32>,
    pub tris: Vec<MeshTri>,
    pub anim_seqs: Vec<MeshAnimSeq>,
    pub connects: Vec<VertConnect>,
    pub bounding_box: BoundingBox,
    pub bounding_sphere: Sphere,
    pub vert_links: Vec<i32>,
    pub textures: Vec<ObjectRef>,
    pub bounding_boxes: Vec<BoundingBox>,
    pub bounding_spheres: Vec<Sphere>,
    pub frame_verts: i32,
    pub anim_frames: i32,
    pub and_flags: u32,
    pub or_flags: u32,
    pub scale: Vector,
    pub origin: Vector,
    pub rot_origin: Rotator,
    pub cur_poly: i32,
    pub cur_vertex: i32,
    pub texture_lod: Vec<f32>,
}

fn read_uv(r: &mut Reader) -> Result<MeshUv> {
    Ok(MeshUv { u: r.u8()?, v: r.u8()? })
}

impl Mesh {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let v = cx.version();
        let primitive = Primitive::parse(r)?;
        let verts = section("verts", read_lazy(r, v, 4, |r| r.u32()))?;
        let tris = section(
            "tris",
            read_lazy(r, v, 20, |r| {
                Ok(MeshTri {
                    i_vertex: [r.u16()?, r.u16()?, r.u16()?],
                    tex: [read_uv(r)?, read_uv(r)?, read_uv(r)?],
                    poly_flags: r.u32()?,
                    texture_index: r.i32()?,
                })
            }),
        )?;
        let anim_seqs = section(
            "anim_seqs",
            read_array(r, 2 + 8 + 1 + 4, |r| {
                Ok(MeshAnimSeq {
                    name: read_name(pkg, r)?,
                    group: read_name(pkg, r)?,
                    start_frame: r.i32()?,
                    num_frames: r.i32()?,
                    notifys: read_array(r, 5, |r| Ok(AnimNotify { time: r.f32()?, function: read_name(pkg, r)? }))?,
                    rate: r.f32()?,
                })
            }),
        )?;
        let connects = section(
            "connects",
            read_lazy(r, v, 8, |r| Ok(VertConnect { num_vert_triangles: r.i32()?, triangle_list_offset: r.i32()? })),
        )?;
        let bounding_box = read_box(r)?;
        let bounding_sphere = read_sphere(r)?;
        let vert_links = section("vert_links", read_lazy(r, v, 4, |r| r.i32()))?;
        let textures = section("textures", read_array(r, 1, |r| read_object_ref(pkg, r)))?;
        let bounding_boxes = section("bounding_boxes", read_array(r, 25, read_box))?;
        let bounding_spheres = section("bounding_spheres", read_array(r, 16, read_sphere))?;
        Ok(Self {
            primitive,
            verts,
            tris,
            anim_seqs,
            connects,
            bounding_box,
            bounding_sphere,
            vert_links,
            textures,
            bounding_boxes,
            bounding_spheres,
            frame_verts: r.i32()?,
            anim_frames: r.i32()?,
            and_flags: r.u32()?,
            or_flags: r.u32()?,
            scale: read_vector(r)?,
            origin: read_vector(r)?,
            rot_origin: read_rotator(r)?,
            cur_poly: r.i32()?,
            cur_vertex: r.i32()?,
            texture_lod: section("texture_lod", read_array(r, 4, |r| r.f32()))?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MeshFace {
    pub i_wedge: [u16; 3],
    pub material_index: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct MeshWedge {
    pub i_vertex: u16,
    pub tex_uv: MeshUv,
}

#[derive(Debug, Clone, Copy)]
pub struct MeshMaterial {
    pub poly_flags: u32,
    pub texture_index: i32,
}

#[derive(Debug, Clone)]
pub struct LodMesh {
    pub mesh: Mesh,
    pub collapse_point_thus: Vec<u16>,
    pub face_level: Vec<u16>,
    pub faces: Vec<MeshFace>,
    pub collapse_wedge_thus: Vec<u16>,
    pub wedges: Vec<MeshWedge>,
    pub materials: Vec<MeshMaterial>,
    pub special_faces: Vec<MeshFace>,
    pub model_verts: i32,
    pub special_verts: i32,
    pub mesh_scale_max: f32,
    pub lod_hysteresis: f32,
    pub lod_strength: f32,
    pub lod_min_verts: i32,
    pub lod_morph: f32,
    pub lod_z_displace: f32,
    pub remap_anim_verts: Vec<u16>,
    pub old_frame_verts: i32,
}

fn read_face(r: &mut Reader) -> Result<MeshFace> {
    Ok(MeshFace { i_wedge: [r.u16()?, r.u16()?, r.u16()?], material_index: r.u16()? })
}

impl LodMesh {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let mesh = Mesh::parse(cx, r)?;
        Ok(Self {
            mesh,
            collapse_point_thus: section("collapse_point_thus", read_array(r, 2, |r| r.u16()))?,
            face_level: section("face_level", read_array(r, 2, |r| r.u16()))?,
            faces: section("faces", read_array(r, 8, read_face))?,
            collapse_wedge_thus: section("collapse_wedge_thus", read_array(r, 2, |r| r.u16()))?,
            wedges: section(
                "wedges",
                read_array(r, 4, |r| Ok(MeshWedge { i_vertex: r.u16()?, tex_uv: read_uv(r)? })),
            )?,
            materials: section(
                "materials",
                read_array(r, 8, |r| Ok(MeshMaterial { poly_flags: r.u32()?, texture_index: r.i32()? })),
            )?,
            special_faces: section("special_faces", read_array(r, 8, read_face))?,
            model_verts: r.i32()?,
            special_verts: r.i32()?,
            mesh_scale_max: r.f32()?,
            lod_hysteresis: r.f32()?,
            lod_strength: r.f32()?,
            lod_min_verts: r.i32()?,
            lod_morph: r.f32()?,
            lod_z_displace: r.f32()?,
            remap_anim_verts: section("remap_anim_verts", read_array(r, 2, |r| r.u16()))?,
            old_frame_verts: r.i32()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

pub fn read_quat(r: &mut Reader) -> Result<Quat> {
    Ok(Quat { x: r.f32()?, y: r.f32()?, z: r.f32()?, w: r.f32()? })
}

#[derive(Debug, Clone, Copy)]
pub struct MeshExtWedge {
    pub i_vertex: u16,
    pub flags: u16,
    pub u: f32,
    pub v: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct JointPos {
    pub orientation: Quat,
    pub position: Vector,
    pub length: f32,
    pub x_size: f32,
    pub y_size: f32,
    pub z_size: f32,
}

#[derive(Debug, Clone)]
pub struct MeshBone {
    pub name: NameIndex,
    pub flags: u32,
    pub bone_pos: JointPos,
    pub num_children: i32,
    pub parent_index: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct BoneWeightIndex {
    pub weight_index: u16,
    pub number: u16,
    pub detail_a: u16,
    pub detail_b: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct BoneWeight {
    pub point_index: u16,
    pub bone_weight: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Coords {
    pub origin: Vector,
    pub x_axis: Vector,
    pub y_axis: Vector,
    pub z_axis: Vector,
}

impl Coords {
    /// The frame that changes nothing.
    pub const IDENTITY: Self = Self {
        origin: Vector { x: 0.0, y: 0.0, z: 0.0 },
        x_axis: Vector { x: 1.0, y: 0.0, z: 0.0 },
        y_axis: Vector { x: 0.0, y: 1.0, z: 0.0 },
        z_axis: Vector { x: 0.0, y: 0.0, z: 1.0 },
    };
}

/// LodMesh + reference skeleton and vertex weights.
#[derive(Debug, Clone)]
pub struct SkeletalMesh {
    pub lod: LodMesh,
    pub ext_wedges: Vec<MeshExtWedge>,
    pub points: Vec<Vector>,
    pub ref_skeleton: Vec<MeshBone>,
    pub bone_weight_idx: Vec<BoneWeightIndex>,
    pub bone_weights: Vec<BoneWeight>,
    pub local_points: Vec<Vector>,
    pub skeletal_depth: i32,
    pub default_animation: ObjectRef,
    pub weapon_bone_index: i32,
    pub weapon_adjust: Coords,
}

impl SkeletalMesh {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let lod = LodMesh::parse(cx, r)?;
        Ok(Self {
            lod,
            ext_wedges: section(
                "ext_wedges",
                read_array(r, 12, |r| Ok(MeshExtWedge { i_vertex: r.u16()?, flags: r.u16()?, u: r.f32()?, v: r.f32()? })),
            )?,
            points: section("points", read_array(r, 12, read_vector))?,
            ref_skeleton: section(
                "ref_skeleton",
                read_array(r, 1 + 4 + 44 + 8, |r| {
                    Ok(MeshBone {
                        name: read_name(pkg, r)?,
                        flags: r.u32()?,
                        bone_pos: JointPos {
                            orientation: read_quat(r)?,
                            position: read_vector(r)?,
                            length: r.f32()?,
                            x_size: r.f32()?,
                            y_size: r.f32()?,
                            z_size: r.f32()?,
                        },
                        num_children: r.i32()?,
                        parent_index: r.i32()?,
                    })
                }),
            )?,
            bone_weight_idx: section(
                "bone_weight_idx",
                read_array(r, 8, |r| {
                    Ok(BoneWeightIndex { weight_index: r.u16()?, number: r.u16()?, detail_a: r.u16()?, detail_b: r.u16()? })
                }),
            )?,
            bone_weights: section(
                "bone_weights",
                read_array(r, 4, |r| Ok(BoneWeight { point_index: r.u16()?, bone_weight: r.u16()? })),
            )?,
            local_points: section("local_points", read_array(r, 12, read_vector))?,
            skeletal_depth: r.i32()?,
            default_animation: read_object_ref(pkg, r)?,
            weapon_bone_index: r.i32()?,
            weapon_adjust: Coords {
                origin: read_vector(r)?,
                x_axis: read_vector(r)?,
                y_axis: read_vector(r)?,
                z_axis: read_vector(r)?,
            },
        })
    }
}
