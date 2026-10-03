use std::collections::HashMap;

use crate::names::NameId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PkgId(pub u32);

/// Pseudo-package of intrinsic classes: their key is `{ INTRINSIC, ClassId }`, so they can be
/// passed around as class values like any serialized class.
pub const INTRINSIC: PkgId = PkgId(u32::MAX - 1);

/// A serialized object: an export of a loaded package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectKey {
    pub package: PkgId,
    pub export: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateId(pub u32);

pub mod cpf {
    pub const OPTIONAL_PARM: u32 = 0x0000_0010;
    pub const NET: u32 = 0x0000_0020;
    pub const PARM: u32 = 0x0000_0080;
    pub const OUT_PARM: u32 = 0x0000_0100;
    pub const RETURN_PARM: u32 = 0x0000_0400;
    pub const COERCE_PARM: u32 = 0x0000_0800;
    /// The engine owns the value; the serializers skip it. Verified on the save streams of the
    /// game: the properties they leave out are exactly the native and transient ones.
    pub const NATIVE: u32 = 0x0000_1000;
    pub const TRANSIENT: u32 = 0x0000_2000;
    /// The property keeps its value across a level change.
    pub const TRAVEL: u32 = 0x0001_0000;
}

pub mod func {
    pub const FINAL: u32 = 0x0000_0001;
    pub const ITERATOR: u32 = 0x0000_0004;
    pub const LATENT: u32 = 0x0000_0008;
    pub const PRE_OPERATOR: u32 = 0x0000_0010;
    pub const SINGULAR: u32 = 0x0000_0020;
    /// Reachable by name from the console (`exec function`).
    pub const EXEC: u32 = 0x0000_0200;
    pub const NATIVE: u32 = 0x0000_0400;
    pub const EVENT: u32 = 0x0000_0800;
    pub const OPERATOR: u32 = 0x0000_1000;
    pub const STATIC: u32 = 0x0000_2000;
}

/// Bytecode with a map from in-memory offsets (jump targets) to statement indices.
#[derive(Debug, Clone, Default)]
pub struct Code {
    pub statements: Vec<grim_assets::bytecode::Statement>,
    pub offsets: HashMap<u32, usize>,
    pub package: Option<PkgId>,
}

impl Code {
    pub fn new(statements: Vec<grim_assets::bytecode::Statement>, package: PkgId) -> Self {
        let offsets = statements.iter().enumerate().map(|(i, s)| (s.offset, i)).collect();
        Self { statements, offsets, package: Some(package) }
    }
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: NameId,
    pub key: ObjectKey,
    pub flags: u32,
    pub native: u16,
    pub operator_precedence: u8,
    /// Parameters, return value and locals, in declaration order; slots are frame-relative.
    pub locals: Vec<PropDef>,
    pub slots: u32,
    /// Indices into `locals` of the parameters, in call order.
    pub params: Vec<usize>,
    pub ret: Option<usize>,
    pub code: std::sync::Arc<Code>,
    /// Function this one overrides (`SuperField`).
    pub super_: Option<ObjectKey>,
}

#[derive(Debug, Clone)]
pub struct StateDef {
    pub name: NameId,
    pub key: ObjectKey,
    pub super_: Option<StateId>,
    pub flags: u32,
    pub probe_mask: u64,
    pub ignore_mask: u64,
    pub functions: crate::FastMap<NameId, FuncId>,
    pub code: std::sync::Arc<Code>,
    /// Label name to in-memory code offset.
    pub labels: HashMap<NameId, u32>,
}

/// A class referenced by a declaration: serialized somewhere, or intrinsic (native only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassRef {
    Key(ObjectKey),
    Intrinsic(NameId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropType {
    Byte(Option<EnumId>),
    Int,
    Bool,
    Float,
    /// Declared object class (not linked, to avoid cycles).
    Object(Option<ClassRef>),
    /// Declared meta class.
    Class(Option<ClassRef>),
    Name,
    Str,
    Struct(StructId),
    Array(Box<PropDef>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropDef {
    pub name: NameId,
    pub array_dim: u32,
    pub flags: u32,
    pub ty: PropType,
    /// First slot in the owner's value array; the property takes `array_dim` slots.
    pub slot: u32,
    pub key: ObjectKey,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: NameId,
    pub key: ObjectKey,
    pub super_: Option<StructId>,
    /// Inherited fields first, then own fields in declaration order.
    pub fields: Vec<PropDef>,
    pub slots: u32,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: NameId,
    pub key: ObjectKey,
    pub values: Vec<NameId>,
}

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: NameId,
    /// For intrinsic classes (no serialized definition) a key in the `INTRINSIC` package.
    pub key: Option<ObjectKey>,
    pub super_: Option<ClassId>,
    pub flags: u32,
    /// Inherited properties first.
    pub props: Vec<PropDef>,
    pub by_name: crate::FastMap<NameId, usize>,
    pub slots: u32,
    pub defaults: Vec<crate::value::Value>,
    pub functions: crate::FastMap<NameId, FuncId>,
    pub states: HashMap<NameId, StateId>,
    /// The ini a class's `config` properties are kept in, which it names itself: `User` means
    /// the player's own settings rather than the game's.
    pub config_name: NameId,
    /// Which events reach an actor of this class whatever state it is in. A `UClass` is a
    /// `UState` in UE1 and carries the same two masks; a state's own masks are combined with
    /// these to say what a frame listens for.
    pub probe_mask: u64,
    pub ignore_mask: u64,
}

impl ClassDef {
    pub fn prop(&self, name: NameId) -> Option<&PropDef> {
        self.by_name.get(&name).map(|&i| &self.props[i])
    }

    /// Every property of that name, innermost first: a class can declare one that shadows an
    /// inherited one with a type of its own. `GameReplicationInfo.Region` is an int over
    /// `Actor.Region`, which is a point region, and both are written by name.
    pub fn props_named(&self, name: NameId) -> impl Iterator<Item = &PropDef> {
        self.props.iter().rev().filter(move |p| p.name == name)
    }
}
