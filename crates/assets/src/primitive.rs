use grim_package::property::{read_vector, Vector};
use grim_package::{Error, Reader, Result};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoundingBox {
    pub min: Vector,
    pub max: Vector,
    pub is_valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sphere {
    pub center: Vector,
    pub w: f32,
}

pub fn read_box(r: &mut Reader) -> Result<BoundingBox> {
    let min = read_vector(r)?;
    let max = read_vector(r)?;
    let at = r.offset();
    let is_valid = match r.u8()? {
        0 => false,
        1 => true,
        v => return Err(Error::invalid(at, format!("FBox.IsValid = {v}"))),
    };
    Ok(BoundingBox { min, max, is_valid })
}

pub fn read_sphere(r: &mut Reader) -> Result<Sphere> {
    Ok(Sphere { center: read_vector(r)?, w: r.f32()? })
}

/// `Engine.Primitive`, base of Model and meshes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Primitive {
    pub bounding_box: BoundingBox,
    pub bounding_sphere: Sphere,
}

impl Primitive {
    pub fn parse(r: &mut Reader) -> Result<Self> {
        Ok(Self { bounding_box: read_box(r)?, bounding_sphere: read_sphere(r)? })
    }
}
