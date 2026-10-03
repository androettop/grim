//! `Engine.Model`: BSP of a level or brush.

use grim_package::property::{read_object_ref, read_plane, read_vector, Plane, Vector};
use grim_package::{Error, ObjectRef, Reader, Result};

use crate::primitive::{read_box, BoundingBox, Primitive};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct BspNode {
    pub plane: Plane,
    pub zone_mask: u64,
    pub node_flags: u8,
    pub i_vert_pool: i32,
    pub i_surf: i32,
    pub i_back: i32,
    pub i_front: i32,
    pub i_plane: i32,
    pub i_collision_bound: i32,
    pub i_render_bound: i32,
    pub i_zone: [u8; 2],
    pub num_vertices: u8,
    pub i_leaf: [i32; 2],
}

#[derive(Debug, Clone)]
pub struct BspSurf {
    pub texture: ObjectRef,
    pub poly_flags: u32,
    pub p_base: i32,
    pub v_normal: i32,
    pub v_texture_u: i32,
    pub v_texture_v: i32,
    pub i_light_map: i32,
    pub i_brush_poly: i32,
    pub pan_u: i16,
    pub pan_v: i16,
    pub actor: ObjectRef,
}

#[derive(Debug, Clone, Copy)]
pub struct Vert {
    pub p_vertex: i32,
    pub i_side: i32,
}

#[derive(Debug, Clone)]
pub struct ZoneProperties {
    pub zone_actor: ObjectRef,
    pub connectivity: u64,
    pub visibility: u64,
}

/// On-disk order verified against all 20616 Models:
/// `DataOffset, Pan, UClamp, VClamp, UScale, VScale, iLightActors`.
#[derive(Debug, Clone)]
pub struct LightMapIndex {
    pub data_offset: i32,
    pub i_light_actors: i32,
    pub pan: Vector,
    pub u_clamp: i32,
    pub v_clamp: i32,
    pub u_scale: f32,
    pub v_scale: f32,
}

#[derive(Debug, Clone)]
pub struct Leaf {
    pub i_zone: i32,
    pub i_permeating: i32,
    pub i_volumetric: i32,
    pub visible_zones: u64,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub primitive: Primitive,
    pub vectors: Vec<Vector>,
    pub points: Vec<Vector>,
    pub nodes: Vec<BspNode>,
    pub surfs: Vec<BspSurf>,
    pub verts: Vec<Vert>,
    pub num_shared_sides: i32,
    pub zones: Vec<ZoneProperties>,
    pub polys: ObjectRef,
    pub light_map: Vec<LightMapIndex>,
    pub light_bits: Vec<u8>,
    pub bounds: Vec<BoundingBox>,
    pub leaf_hulls: Vec<i32>,
    pub leaves: Vec<Leaf>,
    pub lights: Vec<ObjectRef>,
    pub root_outside: bool,
    pub linked: bool,
}

/// UE1 `FBspNode::MAX_ZONES`.
pub const MAX_ZONES: i32 = 64;

pub(crate) fn read_array<T>(r: &mut Reader, min: usize, mut f: impl FnMut(&mut Reader) -> Result<T>) -> Result<Vec<T>> {
    let n = r.count(min)?;
    (0..n).map(|_| f(r)).collect()
}

pub(crate) fn section<T>(name: &str, r: Result<T>) -> Result<T> {
    r.map_err(|e| e.context(name))
}

pub(crate) fn read_ubool(r: &mut Reader) -> Result<bool> {
    let at = r.offset();
    match r.i32()? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(Error::invalid(at, format!("UBOOL with value {v}"))),
    }
}

impl Model {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let primitive = Primitive::parse(r)?;
        let vectors = section("vectors", read_array(r, 12, read_vector))?;
        let points = section("points", read_array(r, 12, read_vector))?;
        let nodes = section("nodes", read_array(r, 16 + 8 + 1 + 7 + 3 + 8, |r| {
            Ok(BspNode {
                plane: read_plane(r)?,
                zone_mask: r.u64()?,
                node_flags: r.u8()?,
                i_vert_pool: r.compact()?,
                i_surf: r.compact()?,
                i_back: r.compact()?,
                i_front: r.compact()?,
                i_plane: r.compact()?,
                i_collision_bound: r.compact()?,
                i_render_bound: r.compact()?,
                i_zone: r.array()?,
                num_vertices: r.u8()?,
                i_leaf: [r.i32()?, r.i32()?],
            })
        }))?;
        let surfs = section("surfs", read_array(r, 1 + 4 + 6 + 4 + 1, |r| {
            Ok(BspSurf {
                texture: read_object_ref(pkg, r)?,
                poly_flags: r.u32()?,
                p_base: r.compact()?,
                v_normal: r.compact()?,
                v_texture_u: r.compact()?,
                v_texture_v: r.compact()?,
                i_light_map: r.compact()?,
                i_brush_poly: r.compact()?,
                pan_u: r.i16()?,
                pan_v: r.i16()?,
                actor: read_object_ref(pkg, r)?,
            })
        }))?;
        let verts = section("verts", read_array(r, 2, |r| Ok(Vert { p_vertex: r.compact()?, i_side: r.compact()? })))?;
        let num_shared_sides = r.i32()?;
        let at = r.offset();
        let num_zones = r.i32()?;
        if !(0..=MAX_ZONES).contains(&num_zones) {
            return Err(Error::invalid(at, format!("NumZones = {num_zones}")));
        }
        let zones = (0..num_zones)
            .map(|_| {
                Ok(ZoneProperties {
                    zone_actor: read_object_ref(pkg, r)?,
                    connectivity: r.u64()?,
                    visibility: r.u64()?,
                })
            })
            .collect::<Result<_>>()?;
        let polys = read_object_ref(pkg, r)?;
        let light_map = section("light_map", read_array(r, 4 + 4 + 12 + 2 + 8, |r| {
            Ok(LightMapIndex {
                data_offset: r.i32()?,
                pan: read_vector(r)?,
                u_clamp: r.compact()?,
                v_clamp: r.compact()?,
                u_scale: r.f32()?,
                v_scale: r.f32()?,
                i_light_actors: r.i32()?,
            })
        }))?;
        let n = r.count(1)?;
        let light_bits = r.bytes(n)?.to_vec();
        let bounds = section("bounds", read_array(r, 25, read_box))?;
        let leaf_hulls = section("leaf_hulls", read_array(r, 4, |r| r.i32()))?;
        let leaves = section("leaves", read_array(r, 3 + 8, |r| {
            Ok(Leaf { i_zone: r.compact()?, i_permeating: r.compact()?, i_volumetric: r.compact()?, visible_zones: r.u64()? })
        }))?;
        let lights = section("lights", read_array(r, 1, |r| read_object_ref(pkg, r)))?;
        let root_outside = read_ubool(r)?;
        let linked = read_ubool(r)?;
        Ok(Self {
            primitive,
            vectors,
            points,
            nodes,
            surfs,
            verts,
            num_shared_sides,
            zones,
            polys,
            light_map,
            light_bits,
            bounds,
            leaf_hulls,
            leaves,
            lights,
            root_outside,
            linked,
        })
    }
}
