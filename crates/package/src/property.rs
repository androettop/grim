//! UE1 tagged properties.
//!
//! ```text
//! name      compact name index ("None" ends the list)
//! info      u8: bits 0..3 type, bits 4..6 size code, bit 7 array flag
//! [struct]  compact name index (Struct only)
//! [size]    u8/u16/i32 when the size code is 5/6/7
//! [index]   array index (array flag set and type != Bool)
//! value     `size` bytes
//! ```
//!
//! For Bool the array flag is the value and there is no payload.

use crate::error::{Error, Result};
use crate::package::Package;
use crate::reader::{latin1, Reader};
use crate::tables::{NameIndex, ObjectRef};
use crate::unknown::Unknown;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyType {
    Byte = 1,
    Int = 2,
    Bool = 3,
    Float = 4,
    Object = 5,
    Name = 6,
    String = 7,
    Class = 8,
    Array = 9,
    Struct = 10,
    Vector = 11,
    Rotator = 12,
    Str = 13,
    Map = 14,
    FixedArray = 15,
}

impl PropertyType {
    fn from_u8(v: u8) -> Option<Self> {
        use PropertyType::*;
        Some(match v {
            1 => Byte,
            2 => Int,
            3 => Bool,
            4 => Float,
            5 => Object,
            6 => Name,
            7 => String,
            8 => Class,
            9 => Array,
            10 => Struct,
            11 => Vector,
            12 => Rotator,
            13 => Str,
            14 => Map,
            15 => FixedArray,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rotator {
    pub pitch: i32,
    pub yaw: i32,
    pub roll: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Plane {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scale {
    pub scale: Vector,
    pub sheer_rate: f32,
    pub sheer_axis: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointRegion {
    pub zone: ObjectRef,
    pub leaf: i32,
    pub zone_number: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StructValue {
    Vector(Vector),
    Rotator(Rotator),
    Color(Color),
    Plane(Plane),
    Scale(Scale),
    PointRegion(PointRegion),
    /// Layout not modeled yet; needs the UStruct definition (class linker).
    Unparsed(Unknown),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Object(ObjectRef),
    Name(NameIndex),
    Class(ObjectRef),
    Str(String),
    Struct { name: NameIndex, value: StructValue },
    /// Element type is only known from the class's ArrayProperty definition (class linker).
    Array(Unknown),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: NameIndex,
    pub array_index: u32,
    pub value: PropertyValue,
    /// Absolute file offset of the tag.
    pub offset: usize,
    /// Absolute file range of the value payload (empty for Bool).
    pub payload: std::ops::Range<usize>,
}

pub fn read_vector(r: &mut Reader) -> Result<Vector> {
    Ok(Vector { x: r.f32()?, y: r.f32()?, z: r.f32()? })
}

pub fn read_rotator(r: &mut Reader) -> Result<Rotator> {
    Ok(Rotator { pitch: r.i32()?, yaw: r.i32()?, roll: r.i32()? })
}

pub fn read_plane(r: &mut Reader) -> Result<Plane> {
    Ok(Plane { x: r.f32()?, y: r.f32()?, z: r.f32()?, w: r.f32()? })
}

pub fn read_object_ref(pkg: &Package, r: &mut Reader) -> Result<ObjectRef> {
    let at = r.offset();
    let raw = r.compact()?;
    let o = ObjectRef::from_raw(raw);
    let ok = match o {
        ObjectRef::Null => true,
        ObjectRef::Import(i) => (i as usize) < pkg.imports.len(),
        ObjectRef::Export(i) => (i as usize) < pkg.exports.len(),
    };
    if ok {
        Ok(o)
    } else {
        Err(Error::new(at, crate::ErrorKind::ObjectRefOutOfRange(raw)))
    }
}

pub fn read_name(pkg: &Package, r: &mut Reader) -> Result<NameIndex> {
    let at = r.offset();
    let v = r.compact()?;
    if v < 0 || v as usize >= pkg.names.len() {
        return Err(Error::new(at, crate::ErrorKind::NameIndexOutOfRange(v as i64)));
    }
    Ok(NameIndex(v as u32))
}

/// UE1 FString: compact length including the trailing NUL.
/// Positive: ANSI bytes (decoded as Latin-1). Negative: `-len` UTF-16LE units, seen in
/// `CutScene` scripts (e.g. `Grandstaircase_hub.CutScene3.aThreadScripts`), where the tag size
/// confirms `2 * len + compact size`.
pub fn read_string(r: &mut Reader) -> Result<String> {
    let at = r.offset();
    let len = r.compact()?;
    if len == 0 {
        return Ok(String::new());
    }
    if len > 0 {
        let s = r.bytes(len as usize)?;
        if s[s.len() - 1] != 0 {
            return Err(Error::invalid(at, "string without trailing NUL"));
        }
        return Ok(latin1(&s[..s.len() - 1]));
    }
    let units = len.unsigned_abs() as usize;
    let raw = r.bytes(units * 2)?;
    let mut chars: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    if chars.pop() != Some(0) {
        return Err(Error::invalid(at, "UTF-16 string without trailing NUL"));
    }
    String::from_utf16(&chars).map_err(|_| Error::invalid(at, "invalid UTF-16 string"))
}

fn read_array_index(r: &mut Reader) -> Result<u32> {
    let b0 = r.u8()? as u32;
    Ok(if b0 & 0x80 == 0 {
        b0
    } else if b0 & 0x40 == 0 {
        ((b0 & 0x7f) << 8) | r.u8()? as u32
    } else {
        let b = r.array::<3>()?;
        ((b0 & 0x3f) << 24) | (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32
    })
}

fn read_struct(pkg: &Package, name: &str, r: &mut Reader) -> Result<StructValue> {
    let at = r.offset();
    let expect = |len: usize, r: &Reader| -> Result<()> {
        if r.len() == len {
            Ok(())
        } else {
            Err(Error::invalid(at, format!("struct {name} has size {} (expected {len})", r.len())))
        }
    };
    Ok(match name {
        "Vector" => {
            expect(12, r)?;
            StructValue::Vector(read_vector(r)?)
        }
        "Rotator" => {
            expect(12, r)?;
            StructValue::Rotator(read_rotator(r)?)
        }
        "Color" => {
            expect(4, r)?;
            let [red, g, b, a] = r.array()?;
            StructValue::Color(Color { r: red, g, b, a })
        }
        "Plane" => {
            expect(16, r)?;
            StructValue::Plane(read_plane(r)?)
        }
        "Scale" => {
            expect(17, r)?;
            StructValue::Scale(Scale { scale: read_vector(r)?, sheer_rate: r.f32()?, sheer_axis: r.u8()? })
        }
        "PointRegion" => StructValue::PointRegion(PointRegion {
            zone: read_object_ref(pkg, r)?,
            leaf: r.i32()?,
            zone_number: r.u8()?,
        }),
        _ => StructValue::Unparsed(Unknown::new(r.offset(), r.rest())),
    })
}

pub fn read_properties(pkg: &Package, r: &mut Reader) -> Result<Vec<Property>> {
    let mut props = Vec::new();
    loop {
        let offset = r.offset();
        let name = read_name(pkg, r)?;
        if pkg.name(name).eq_ignore_ascii_case("None") {
            return Ok(props);
        }
        props.push(read_property_body(pkg, r, name, offset).map_err(|e| {
            e.context(format!("property '{}' (tag @{offset:#x})", pkg.name(name)))
        })?);
    }
}

fn read_property_body(pkg: &Package, r: &mut Reader, name: NameIndex, offset: usize) -> Result<Property> {
    let info_at = r.offset();
    let info = r.u8()?;
    let ty = PropertyType::from_u8(info & 0x0f)
        .ok_or_else(|| Error::invalid(info_at, format!("unknown property type {}", info & 0x0f)))?;
    let array_flag = info & 0x80 != 0;

    let struct_name = if ty == PropertyType::Struct { Some(read_name(pkg, r)?) } else { None };

    let size = match (info >> 4) & 0x07 {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 12,
        4 => 16,
        5 => r.u8()? as usize,
        6 => r.u16()? as usize,
        _ => {
            let at = r.offset();
            let s = r.i32()?;
            usize::try_from(s).map_err(|_| Error::invalid(at, format!("negative size {s}")))?
        }
    };

    let array_index = if array_flag && ty != PropertyType::Bool { read_array_index(r)? } else { 0 };

    if ty == PropertyType::Bool {
        // Bool has size code 0 (1 byte) but no payload.
        let at = r.offset();
        return Ok(Property { name, array_index, value: PropertyValue::Bool(array_flag), offset, payload: at..at });
    }

    let mut v = r.sub(size)?;
    let payload = v.offset()..v.offset() + size;
    let fixed = |want: usize| -> Result<()> {
        if size == want {
            Ok(())
        } else {
            Err(Error::invalid(info_at, format!("{ty:?} with size {size} (expected {want})")))
        }
    };
    let value = match ty {
        PropertyType::Byte => {
            fixed(1)?;
            PropertyValue::Byte(v.u8()?)
        }
        PropertyType::Int => {
            fixed(4)?;
            PropertyValue::Int(v.i32()?)
        }
        PropertyType::Float => {
            fixed(4)?;
            PropertyValue::Float(v.f32()?)
        }
        PropertyType::Object => PropertyValue::Object(read_object_ref(pkg, &mut v)?),
        PropertyType::Class => PropertyValue::Class(read_object_ref(pkg, &mut v)?),
        PropertyType::Name => PropertyValue::Name(read_name(pkg, &mut v)?),
        PropertyType::Str => PropertyValue::Str(read_string(&mut v)?),
        PropertyType::Struct => {
            let sname = struct_name.unwrap();
            PropertyValue::Struct { name: sname, value: read_struct(pkg, pkg.name(sname), &mut v)? }
        }
        PropertyType::Array => PropertyValue::Array(Unknown::new(v.offset(), v.rest())),
        PropertyType::Bool => unreachable!(),
        other => {
            return Err(Error::invalid(info_at, format!("property type {other:?} not seen in HP2 yet")));
        }
    };
    v.expect_end(&format!("{ty:?} value"))?;
    Ok(Property { name, array_index, value, offset, payload })
}
