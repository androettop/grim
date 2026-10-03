//! UnrealScript objects: fields, properties, structs, functions, states and classes.

use grim_package::property::{read_name, read_object_ref, read_properties, read_string, Property as TaggedProperty};
use grim_package::{Guid, NameIndex, ObjectRef, Reader, Result};

use crate::bytecode::{self, Statement};
use crate::model::read_array;
use crate::Ctx;

/// Function is replicated; a u16 `RepOffset` follows the flags.
pub const FUNC_NET: u32 = 0x0000_0040;

/// Replicated property; a u16 `RepOffset` follows.
pub const CPF_NET: u32 = 0x0000_0020;

#[derive(Debug, Clone)]
pub struct Field {
    pub super_field: ObjectRef,
    pub next: ObjectRef,
}

impl Field {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Ok(Self { super_field: read_object_ref(cx.pkg, r)?, next: read_object_ref(cx.pkg, r)? })
    }
}

#[derive(Debug, Clone)]
pub struct Const {
    pub field: Field,
    pub value: String,
}

impl Const {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Ok(Self { field: Field::parse(cx, r)?, value: read_string(r)? })
    }
}

#[derive(Debug, Clone)]
pub struct Enum {
    pub field: Field,
    pub names: Vec<NameIndex>,
}

impl Enum {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Ok(Self { field: Field::parse(cx, r)?, names: read_array(r, 1, |r| read_name(cx.pkg, r))? })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Byte { enum_: ObjectRef },
    Int,
    Bool,
    Float,
    Object { class: ObjectRef },
    Class { class: ObjectRef, meta_class: ObjectRef },
    Name,
    Str,
    Struct { struct_: ObjectRef },
    Array { inner: ObjectRef },
}

#[derive(Debug, Clone)]
pub struct Property {
    pub field: Field,
    pub array_dim: i32,
    pub flags: u32,
    pub category: NameIndex,
    pub rep_offset: Option<u16>,
    pub kind: PropertyKind,
}

pub fn property_class(class: &str) -> Option<&'static str> {
    Some(match class {
        "Core.ByteProperty" => "Byte",
        "Core.IntProperty" => "Int",
        "Core.BoolProperty" => "Bool",
        "Core.FloatProperty" => "Float",
        "Core.ObjectProperty" => "Object",
        "Core.ClassProperty" => "Class",
        "Core.NameProperty" => "Name",
        "Core.StrProperty" => "Str",
        "Core.StructProperty" => "Struct",
        "Core.ArrayProperty" => "Array",
        _ => return None,
    })
}

impl Property {
    pub fn parse(cx: &Ctx, class: &str, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let field = Field::parse(cx, r)?;
        let array_dim = r.i32()?;
        let flags = r.u32()?;
        let category = read_name(pkg, r)?;
        let rep_offset = if flags & CPF_NET != 0 { Some(r.u16()?) } else { None };
        let kind = match property_class(class).expect("dispatched by property_class") {
            "Byte" => PropertyKind::Byte { enum_: read_object_ref(pkg, r)? },
            "Int" => PropertyKind::Int,
            "Bool" => PropertyKind::Bool,
            "Float" => PropertyKind::Float,
            "Object" => PropertyKind::Object { class: read_object_ref(pkg, r)? },
            "Class" => PropertyKind::Class { class: read_object_ref(pkg, r)?, meta_class: read_object_ref(pkg, r)? },
            "Name" => PropertyKind::Name,
            "Str" => PropertyKind::Str,
            "Struct" => PropertyKind::Struct { struct_: read_object_ref(pkg, r)? },
            _ => PropertyKind::Array { inner: read_object_ref(pkg, r)? },
        };
        Ok(Self { field, array_dim, flags, category, rep_offset, kind })
    }
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub field: Field,
    pub script_text: ObjectRef,
    pub children: ObjectRef,
    pub friendly_name: NameIndex,
    pub line: i32,
    pub text_pos: i32,
    /// In-memory size of the bytecode.
    pub script_size: u32,
    pub script: Vec<Statement>,
}

impl Struct {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let field = Field::parse(cx, r)?;
        let script_text = read_object_ref(pkg, r)?;
        let children = read_object_ref(pkg, r)?;
        let friendly_name = read_name(pkg, r)?;
        let line = r.i32()?;
        let text_pos = r.i32()?;
        let at = r.offset();
        let script_size = r.i32()?;
        let script_size = u32::try_from(script_size)
            .map_err(|_| grim_package::Error::invalid(at, format!("negative ScriptSize {script_size}")))?;
        let script = bytecode::decode(cx, r, script_size)?;
        Ok(Self { field, script_text, children, friendly_name, line, text_pos, script_size, script })
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub struct_: Struct,
    pub native_index: u16,
    pub operator_precedence: u8,
    pub flags: u32,
    pub rep_offset: Option<u16>,
}

impl Function {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let struct_ = Struct::parse(cx, r)?;
        let native_index = r.u16()?;
        let operator_precedence = r.u8()?;
        let flags = r.u32()?;
        let rep_offset = if flags & FUNC_NET != 0 { Some(r.u16()?) } else { None };
        Ok(Self { struct_, native_index, operator_precedence, flags, rep_offset })
    }
}

#[derive(Debug, Clone)]
pub struct State {
    pub struct_: Struct,
    pub probe_mask: u64,
    pub ignore_mask: u64,
    pub label_table_offset: u16,
    pub flags: u32,
}

impl State {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Ok(Self {
            struct_: Struct::parse(cx, r)?,
            probe_mask: r.u64()?,
            ignore_mask: r.u64()?,
            label_table_offset: r.u16()?,
            flags: r.u32()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub class: ObjectRef,
    pub deep: u32,
    pub script_text_crc: u32,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub state: State,
    pub flags: u32,
    pub guid: Guid,
    pub dependencies: Vec<Dependency>,
    pub package_imports: Vec<NameIndex>,
    pub within: ObjectRef,
    pub config_name: NameIndex,
    /// Default property values.
    pub defaults: Vec<TaggedProperty>,
}

impl Class {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let state = State::parse(cx, r)?;
        let flags = r.u32()?;
        let guid = Guid(r.array()?);
        let dependencies = read_array(r, 9, |r| {
            Ok(Dependency { class: read_object_ref(pkg, r)?, deep: r.u32()?, script_text_crc: r.u32()? })
        })?;
        let package_imports = read_array(r, 1, |r| read_name(pkg, r))?;
        let within = read_object_ref(pkg, r)?;
        let config_name = read_name(pkg, r)?;
        let defaults = read_properties(pkg, r)?;
        Ok(Self { state, flags, guid, dependencies, package_imports, within, config_name, defaults })
    }
}
