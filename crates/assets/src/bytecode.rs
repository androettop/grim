//! UnrealScript bytecode, decoded into an expression tree.
//!
//! `ScriptSize` counts *in-memory* bytes, where object references and names take 4 bytes but
//! are stored as compact indices on disk. Decoding therefore walks every token, tracking the
//! in-memory offset (`mem`), until it reaches `ScriptSize` exactly.

use grim_package::property::{read_name, read_object_ref, read_rotator, read_vector, Rotator, Vector};
use grim_package::reader::latin1;
use grim_package::{Error, NameIndex, ObjectRef, Reader, Result};

use crate::Ctx;

/// First token that is a native function call with a one-byte index.
pub const FIRST_NATIVE: u8 = 0x70;
/// Tokens `0x60..0x70` are native calls with a two-byte index.
pub const EXTENDED_NATIVE: u8 = 0x60;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LocalVariable(ObjectRef),
    InstanceVariable(ObjectRef),
    DefaultVariable(ObjectRef),
    Return(Box<Expr>),
    Switch { size: u8, value: Box<Expr> },
    Jump(u16),
    JumpIfNot { target: u16, cond: Box<Expr> },
    Stop,
    Assert { line: u16, cond: Box<Expr> },
    /// `target == 0xFFFF` is `default:` and carries no value.
    Case { target: u16, value: Option<Box<Expr>> },
    Nothing,
    LabelTable(Vec<(NameIndex, u32)>),
    GotoLabel(Box<Expr>),
    EatString(Box<Expr>),
    Let(Box<Expr>, Box<Expr>),
    DynArrayElement { index: Box<Expr>, array: Box<Expr> },
    New { outer: Box<Expr>, name: Box<Expr>, flags: Box<Expr>, class: Box<Expr> },
    ClassContext { object: Box<Expr>, skip: u16, size: u8, expr: Box<Expr> },
    MetaCast { class: ObjectRef, expr: Box<Expr> },
    LetBool(Box<Expr>, Box<Expr>),
    EndFunctionParms,
    SelfRef,
    Skip { skip: u16, expr: Box<Expr> },
    Context { object: Box<Expr>, skip: u16, size: u8, expr: Box<Expr> },
    ArrayElement { index: Box<Expr>, array: Box<Expr> },
    VirtualFunction { name: NameIndex, args: Vec<Expr> },
    FinalFunction { function: ObjectRef, args: Vec<Expr> },
    IntConst(i32),
    FloatConst(f32),
    StringConst(String),
    ObjectConst(ObjectRef),
    NameConst(NameIndex),
    RotationConst(Rotator),
    VectorConst(Vector),
    ByteConst(u8),
    IntZero,
    IntOne,
    True,
    False,
    NativeParm(ObjectRef),
    NoObject,
    IntConstByte(u8),
    BoolVariable(Box<Expr>),
    DynamicCast { class: ObjectRef, expr: Box<Expr> },
    Iterator { expr: Box<Expr>, skip: u16 },
    IteratorPop,
    IteratorNext,
    StructCmpEq { struct_: ObjectRef, a: Box<Expr>, b: Box<Expr> },
    StructCmpNe { struct_: ObjectRef, a: Box<Expr>, b: Box<Expr> },
    UnicodeStringConst(String),
    StructMember { member: ObjectRef, expr: Box<Expr> },
    /// Token 0x37, one operand, int result (seen as `X.Arr.? == 2` in
    /// HPParticle.SpellLearnFX.DrawSpell). Hypothesis: dynamic array length.
    DynArrayLength(Box<Expr>),
    GlobalFunction { name: NameIndex, args: Vec<Expr> },
    Cast { cast: Cast, expr: Box<Expr> },
    Native { index: u16, args: Vec<Expr> },
}

/// Primitive conversions. Derived from the game's own bytecode by type inference (see
/// grim-vm tests/conversions.rs): each token has a single dominant source → target pair. Tokens
/// in 0x3A..0x60 that never occur in the game are rejected rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cast {
    RotatorToVector,
    ByteToInt,
    ByteToFloat,
    IntToByte,
    IntToFloat,
    BoolToInt,
    FloatToByte,
    FloatToInt,
    StringToInt,
    StringToBool,
    StringToFloat,
    VectorToRotator,
    ByteToString,
    IntToString,
    BoolToString,
    FloatToString,
    ObjectToString,
    NameToString,
    VectorToString,
    RotatorToString,
    StringToName,
}

impl Cast {
    pub fn from_token(t: u8) -> Option<Self> {
        use Cast::*;
        Some(match t {
            0x3A => RotatorToVector,
            0x3B => ByteToInt,
            0x3D => ByteToFloat,
            0x3E => IntToByte,
            0x40 => IntToFloat,
            0x42 => BoolToInt,
            0x44 => FloatToByte,
            0x45 => FloatToInt,
            0x4A => StringToInt,
            0x4B => StringToBool,
            0x4C => StringToFloat,
            0x50 => VectorToRotator,
            0x52 => ByteToString,
            0x53 => IntToString,
            0x54 => BoolToString,
            0x55 => FloatToString,
            0x56 => ObjectToString,
            0x57 => NameToString,
            0x58 => VectorToString,
            0x59 => RotatorToString,
            0x5A => StringToName,
            _ => return None,
        })
    }
}

/// A top-level statement and its in-memory offset (jump targets refer to these).
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub offset: u32,
    pub expr: Expr,
}

struct Decoder<'a, 'r> {
    cx: &'a Ctx<'a>,
    r: &'a mut Reader<'r>,
    mem: u32,
}

impl Decoder<'_, '_> {
    fn u8(&mut self) -> Result<u8> {
        self.mem += 1;
        self.r.u8()
    }
    fn u16(&mut self) -> Result<u16> {
        self.mem += 2;
        self.r.u16()
    }
    fn i32(&mut self) -> Result<i32> {
        self.mem += 4;
        self.r.i32()
    }
    fn f32(&mut self) -> Result<f32> {
        self.mem += 4;
        self.r.f32()
    }
    fn obj(&mut self) -> Result<ObjectRef> {
        self.mem += 4;
        read_object_ref(self.cx.pkg, self.r)
    }
    fn name(&mut self) -> Result<NameIndex> {
        self.mem += 4;
        read_name(self.cx.pkg, self.r)
    }
    fn boxed(&mut self) -> Result<Box<Expr>> {
        Ok(Box::new(self.expr()?))
    }

    fn args(&mut self) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        loop {
            match self.expr()? {
                Expr::EndFunctionParms => return Ok(args),
                e => args.push(e),
            }
        }
    }

    fn expr(&mut self) -> Result<Expr> {
        let at = self.r.offset();
        let t = self.u8()?;
        Ok(match t {
            0x00 => Expr::LocalVariable(self.obj()?),
            0x01 => Expr::InstanceVariable(self.obj()?),
            0x02 => Expr::DefaultVariable(self.obj()?),
            0x04 => Expr::Return(self.boxed()?),
            0x05 => Expr::Switch { size: self.u8()?, value: self.boxed()? },
            0x06 => Expr::Jump(self.u16()?),
            0x07 => Expr::JumpIfNot { target: self.u16()?, cond: self.boxed()? },
            0x08 => Expr::Stop,
            0x09 => Expr::Assert { line: self.u16()?, cond: self.boxed()? },
            0x0A => {
                let target = self.u16()?;
                let value = if target == 0xFFFF { None } else { Some(self.boxed()?) };
                Expr::Case { target, value }
            }
            0x0B => Expr::Nothing,
            0x0C => {
                let mut labels = Vec::new();
                loop {
                    let name = self.name()?;
                    let offset = self.i32()? as u32;
                    if self.cx.pkg.name(name).eq_ignore_ascii_case("None") {
                        break;
                    }
                    labels.push((name, offset));
                }
                Expr::LabelTable(labels)
            }
            0x0D => Expr::GotoLabel(self.boxed()?),
            0x0E => Expr::EatString(self.boxed()?),
            0x0F => Expr::Let(self.boxed()?, self.boxed()?),
            0x10 => Expr::DynArrayElement { index: self.boxed()?, array: self.boxed()? },
            0x11 => Expr::New { outer: self.boxed()?, name: self.boxed()?, flags: self.boxed()?, class: self.boxed()? },
            0x12 => Expr::ClassContext { object: self.boxed()?, skip: self.u16()?, size: self.u8()?, expr: self.boxed()? },
            0x13 => Expr::MetaCast { class: self.obj()?, expr: self.boxed()? },
            0x14 => Expr::LetBool(self.boxed()?, self.boxed()?),
            0x16 => Expr::EndFunctionParms,
            0x17 => Expr::SelfRef,
            0x18 => Expr::Skip { skip: self.u16()?, expr: self.boxed()? },
            0x19 => Expr::Context { object: self.boxed()?, skip: self.u16()?, size: self.u8()?, expr: self.boxed()? },
            0x1A => Expr::ArrayElement { index: self.boxed()?, array: self.boxed()? },
            0x1B => Expr::VirtualFunction { name: self.name()?, args: self.args()? },
            0x1C => Expr::FinalFunction { function: self.obj()?, args: self.args()? },
            0x1D => Expr::IntConst(self.i32()?),
            0x1E => Expr::FloatConst(self.f32()?),
            0x1F => {
                let start = self.r.pos();
                while self.r.u8()? != 0 {}
                let len = self.r.pos() - start;
                self.r.seek(start)?;
                let s = latin1(&self.r.bytes(len)?[..len - 1]);
                self.mem += len as u32;
                Expr::StringConst(s)
            }
            0x20 => Expr::ObjectConst(self.obj()?),
            0x21 => Expr::NameConst(self.name()?),
            0x22 => {
                self.mem += 12;
                Expr::RotationConst(read_rotator(self.r)?)
            }
            0x23 => {
                self.mem += 12;
                Expr::VectorConst(read_vector(self.r)?)
            }
            0x24 => Expr::ByteConst(self.u8()?),
            0x25 => Expr::IntZero,
            0x26 => Expr::IntOne,
            0x27 => Expr::True,
            0x28 => Expr::False,
            0x29 => Expr::NativeParm(self.obj()?),
            0x2A => Expr::NoObject,
            0x2C => Expr::IntConstByte(self.u8()?),
            0x2D => Expr::BoolVariable(self.boxed()?),
            0x2E => Expr::DynamicCast { class: self.obj()?, expr: self.boxed()? },
            0x2F => Expr::Iterator { expr: self.boxed()?, skip: self.u16()? },
            0x30 => Expr::IteratorPop,
            0x31 => Expr::IteratorNext,
            0x32 => Expr::StructCmpEq { struct_: self.obj()?, a: self.boxed()?, b: self.boxed()? },
            0x33 => Expr::StructCmpNe { struct_: self.obj()?, a: self.boxed()?, b: self.boxed()? },
            0x34 => {
                let mut units = Vec::new();
                loop {
                    let u = self.u16()?;
                    if u == 0 {
                        break;
                    }
                    units.push(u);
                }
                let s = String::from_utf16(&units).map_err(|_| Error::invalid(at, "invalid UTF-16 constant"))?;
                Expr::UnicodeStringConst(s)
            }
            0x36 => Expr::StructMember { member: self.obj()?, expr: self.boxed()? },
            0x37 => Expr::DynArrayLength(self.boxed()?),
            // 0x39, not 0x38: e.g. `Global.Bump(Other)` in Engine.Mover.BumpOpenTimed.Bump.
            0x39 => Expr::GlobalFunction { name: self.name()?, args: self.args()? },
            0x3A..=0x5F => {
                let cast = Cast::from_token(t).ok_or_else(|| Error::invalid(at, format!("unobserved conversion token {t:#04x}")))?;
                Expr::Cast { cast, expr: self.boxed()? }
            }
            EXTENDED_NATIVE..FIRST_NATIVE => {
                let lo = self.u8()?;
                Expr::Native { index: (((t - EXTENDED_NATIVE) as u16) << 8) | lo as u16, args: self.args()? }
            }
            FIRST_NATIVE.. => Expr::Native { index: t as u16, args: self.args()? },
            other => return Err(Error::invalid(at, format!("unknown bytecode token {other:#04x}"))),
        })
    }
}

/// Decodes `script_size` in-memory bytes of bytecode.
pub fn decode(cx: &Ctx, r: &mut Reader, script_size: u32) -> Result<Vec<Statement>> {
    let start = r.offset();
    let mut d = Decoder { cx, r, mem: 0 };
    let mut out = Vec::new();
    while d.mem < script_size {
        let offset = d.mem;
        let expr = d.expr()?;
        out.push(Statement { offset, expr });
    }
    if d.mem != script_size {
        return Err(Error::invalid(
            start,
            format!("bytecode overran ScriptSize: decoded {} of {script_size} bytes", d.mem),
        ));
    }
    Ok(out)
}
