use grim_package::property::{read_name, read_object_ref, read_vector, Vector};
use grim_package::{NameIndex, ObjectRef, Reader, Result};

use crate::{check, Ctx};

/// UE1 `FPoly::MAX_VERTICES`.
pub const MAX_POLY_VERTICES: usize = 16;

#[derive(Debug, Clone)]
pub struct Poly {
    pub base: Vector,
    pub normal: Vector,
    pub texture_u: Vector,
    pub texture_v: Vector,
    pub vertices: Vec<Vector>,
    pub poly_flags: u32,
    pub actor: ObjectRef,
    pub texture: ObjectRef,
    pub item_name: NameIndex,
    pub i_link: i32,
    pub i_brush_poly: i32,
    pub pan_u: i16,
    pub pan_v: i16,
}

#[derive(Debug, Clone)]
pub struct Polys {
    /// `DbMax`; informational.
    pub max: i32,
    pub polys: Vec<Poly>,
}

impl Polys {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let at = r.offset();
        let num = r.i32()?;
        let max = r.i32()?;
        if num < 0 || num > max {
            return Err(grim_package::Error::invalid(at, format!("Polys: num {num} / max {max}")));
        }
        let polys = (0..num).map(|_| read_poly(cx, r)).collect::<Result<_>>()?;
        Ok(Self { max, polys })
    }
}

fn read_poly(cx: &Ctx, r: &mut Reader) -> Result<Poly> {
    let at = r.offset();
    let n = r.compact_len()?;
    if n > MAX_POLY_VERTICES {
        check(at, "vertices per polygon <= 16", n, MAX_POLY_VERTICES)?;
    }
    let base = read_vector(r)?;
    let normal = read_vector(r)?;
    let texture_u = read_vector(r)?;
    let texture_v = read_vector(r)?;
    let vertices = (0..n).map(|_| read_vector(r)).collect::<Result<_>>()?;
    Ok(Poly {
        base,
        normal,
        texture_u,
        texture_v,
        vertices,
        poly_flags: r.u32()?,
        actor: read_object_ref(cx.pkg, r)?,
        texture: read_object_ref(cx.pkg, r)?,
        item_name: read_name(cx.pkg, r)?,
        i_link: r.compact()?,
        i_brush_poly: r.compact()?,
        pan_u: r.i16()?,
        pan_v: r.i16()?,
    })
}
