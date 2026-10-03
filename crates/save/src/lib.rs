//! The `<Map>_pa.usa` files of a save slot: the actors a level keeps between visits.
//!
//! The layout is written down in `docs/savegame.md`. Despite the extension it is not a package:
//! it is a stream of tagged properties per actor, with its own string encodings, and the size a
//! tag declares is not the size of what follows it, so every value is decoded by its type.

use grim_object::types::PropType;
use grim_object::World;

/// The magic, as the file spells it: UTF-16 with its NUL and no count in front.
const MAGIC: &str = "PA0";

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    /// Absolute offset in the file.
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{:#x}: {}", self.offset, self.message)
    }
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    /// The map file name as the stream spells it, which is not always how the file does.
    pub name: String,
    /// The three header words between the name and the actor count. `-1, 0, 0` in every original
    /// save; what they mean is not known, so they are carried as they came.
    pub unknown: [i32; 3],
    pub actors: Vec<Actor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// Object name of the actor in its level, which is what ties the record to it.
    pub name: String,
    pub class: String,
    pub props: Vec<Prop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    /// Element of a static array; 0 for a plain property.
    pub index: u32,
    pub value: Saved,
}

/// What a property tag carried.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    /// An object or a class reference: the stream keeps the property but not what it pointed at,
    /// so there is nothing to put back.
    Object,
    Class,
    Name(String),
    /// A string and which of the two encodings the stream used, so a file read and written again
    /// comes out the same. The game writes either for the same kind of text.
    Str { text: String, unicode: bool },
    /// A struct, by type name, with one entry per slot of its declaration (a field with an array
    /// dimension takes one entry per element), as `StructDef::fields` orders them.
    Struct { ty: String, fields: Vec<Saved> },
}

// --------------------------------------------------------------------------- reading

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn err<T>(&self, at: usize, message: impl Into<String>) -> Result<T> {
        Err(Error { offset: at, message: message.into() })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let at = self.at;
        if at + n > self.bytes.len() {
            return self.err(at, format!("{n} bytes past the end of the file"));
        }
        self.at += n;
        Ok(&self.bytes[at..at + n])
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// A counted UTF-16 string: byte count, then that many bytes including the NUL.
    fn counted(&mut self) -> Result<String> {
        let at = self.at;
        let n = self.i32()?;
        if n < 0 || n % 2 != 0 {
            return self.err(at, format!("string of {n} bytes"));
        }
        let bytes = self.take(n as usize)?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        match String::from_utf16(&units) {
            Ok(s) => Ok(s.trim_end_matches('\0').to_string()),
            Err(e) => self.err(at, format!("string is not UTF-16: {e}")),
        }
    }

    /// UE1's compact index: sign and five value bits in the first byte, seven more per byte.
    fn compact(&mut self) -> Result<i32> {
        let first = self.u8()?;
        let mut value = (first & 0x3f) as i32;
        if first & 0x40 != 0 {
            let mut shift = 6;
            loop {
                let b = self.u8()?;
                value |= ((b & 0x7f) as i32) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
        }
        Ok(if first & 0x80 != 0 { -value } else { value })
    }

    /// A string as `FString` writes it: a compact index of bytes, positive for 8-bit text and
    /// negative for UTF-16, the NUL counted in.
    fn text(&mut self) -> Result<(String, bool)> {
        let at = self.at;
        let n = self.compact()?;
        if n == 0 {
            return Ok((String::new(), false));
        }
        if n > 0 {
            let bytes = self.take(n as usize)?;
            let s: String = bytes.iter().map(|&b| b as char).collect();
            Ok((s.trim_end_matches('\0').to_string(), false))
        } else {
            let bytes = self.take(-n as usize * 2)?;
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            match String::from_utf16(&units) {
                Ok(s) => Ok((s.trim_end_matches('\0').to_string(), true)),
                Err(e) => self.err(at, format!("string is not UTF-16: {e}")),
            }
        }
    }
}

/// Reads a `_pa` stream. `world` is only needed to know what each struct type is made of: the
/// stream writes a struct's fields one after another with no tags of their own.
pub fn read(bytes: &[u8], world: &World) -> Result<Level> {
    let mut r = Reader::new(bytes);
    let magic = r.take(8)?;
    let magic: Vec<u16> = magic.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let magic = String::from_utf16_lossy(&magic);
    if magic.trim_end_matches('\0') != MAGIC {
        return r.err(0, format!("not a persistent actor stream: magic {magic:?}"));
    }
    let name = r.counted()?;
    let unknown = [r.i32()?, r.i32()?, r.i32()?];
    let count = r.i32()?;
    if count < 0 {
        return r.err(r.at - 4, format!("{count} actors"));
    }
    let mut actors = Vec::with_capacity(count as usize);
    for _ in 0..count {
        actors.push(read_actor(&mut r, world)?);
    }
    let at = r.at;
    let end = r.counted()?;
    if end != "EOF" {
        return r.err(at, format!("expected the EOF marker, found {end:?}"));
    }
    if r.at != bytes.len() {
        return r.err(r.at, format!("{} bytes after the EOF marker", bytes.len() - r.at));
    }
    Ok(Level { name, unknown, actors })
}

fn read_actor(r: &mut Reader, world: &World) -> Result<Actor> {
    let name = r.counted()?;
    let class = r.counted()?;
    let size = r.i32()?;
    if size < 0 {
        return r.err(r.at - 4, format!("{name}: block of {size} bytes"));
    }
    let end = r.at + size as usize;
    let mut props = Vec::new();
    loop {
        if r.at >= end {
            return r.err(r.at, format!("{name}: the block ran out before its None"));
        }
        let at = r.at;
        let prop = r.counted()?;
        if prop == "None" {
            if r.at != end {
                return r.err(r.at, format!("{name}: None is {} bytes from the end of the block", end as i64 - r.at as i64));
            }
            break;
        }
        props.push(read_prop(r, world, &prop).map_err(|e| Error { offset: e.offset, message: format!("{name}.{prop} (tag @{at:#x}): {}", e.message) })?);
    }
    Ok(Actor { name, class, props })
}

fn read_prop(r: &mut Reader, world: &World, name: &str) -> Result<Prop> {
    let info = r.u8()?;
    let ty = info & 0x0f;
    let code = (info >> 4) & 7;
    let high = info & 0x80 != 0;
    // A struct names its type before the size, and a bool has no value of its own: the tag's top
    // bit is the value, where every other type puts the index of a static array element.
    let struct_ty = if ty == 10 { Some(r.counted()?) } else { None };
    match code {
        5 => {
            r.u8()?;
        }
        6 => {
            r.u16()?;
        }
        7 => {
            r.i32()?;
        }
        _ => {}
    }
    let index = if high && ty != 3 { array_index(r)? } else { 0 };
    let value = match ty {
        1 => Saved::Byte(r.u8()?),
        2 => Saved::Int(r.i32()?),
        3 => Saved::Bool(high),
        4 => Saved::Float(r.f32()?),
        5 => Saved::Object,
        6 => Saved::Name(r.counted()?),
        8 => Saved::Class,
        10 => {
            let ty = struct_ty.unwrap_or_default();
            let fields = read_struct(r, world, &ty)?;
            Saved::Struct { ty, fields }
        }
        7 | 13 => {
            let (text, unicode) = r.text()?;
            Saved::Str { text, unicode }
        }
        _ => return r.err(r.at - 1, format!("property type {ty} (tag {info:#04x})")),
    };
    Ok(Prop { name: name.to_string(), index, value })
}

/// The index of a static array element, as UE1 writes one: a byte below 128, else two or four
/// bytes. Only the one-byte form turns up in the saves we have.
fn array_index(r: &mut Reader) -> Result<u32> {
    let b = r.u8()? as u32;
    if b < 128 {
        Ok(b)
    } else if b & 0xc0 == 0x80 {
        Ok(((b & 0x3f) << 8) | r.u8()? as u32)
    } else {
        let (a, b2, c) = (r.u8()? as u32, r.u8()? as u32, r.u8()? as u32);
        Ok(((b & 0x3f) << 24) | (a << 16) | (b2 << 8) | c)
    }
}

/// A struct's fields, one after another with no tags, in the order its declaration lists them.
fn read_struct(r: &mut Reader, world: &World, ty: &str) -> Result<Vec<Saved>> {
    let Some(def) = struct_def(world, ty) else {
        return r.err(r.at, format!("struct {ty} is not declared by the game's classes"));
    };
    let mut out = Vec::new();
    for field in &def.fields {
        for _ in 0..field.array_dim {
            out.push(read_field(r, world, &field.ty)?);
        }
    }
    Ok(out)
}

fn read_field(r: &mut Reader, world: &World, ty: &PropType) -> Result<Saved> {
    Ok(match ty {
        PropType::Byte(_) => Saved::Byte(r.u8()?),
        PropType::Int => Saved::Int(r.i32()?),
        PropType::Bool => Saved::Bool(r.u8()? != 0),
        PropType::Float => Saved::Float(r.f32()?),
        PropType::Object(_) => Saved::Object,
        PropType::Class(_) => Saved::Class,
        PropType::Name => Saved::Name(r.counted()?),
        PropType::Str => {
            let (text, unicode) = r.text()?;
            Saved::Str { text, unicode }
        }
        PropType::Struct(id) => {
            let ty = world.names.str(world.structs[id.0 as usize].name).to_string();
            let fields = read_struct(r, world, &ty)?;
            Saved::Struct { ty, fields }
        }
        PropType::Array(_) => return r.err(r.at, "a dynamic array inside a struct"),
    })
}

fn struct_def<'w>(world: &'w World, ty: &str) -> Option<&'w grim_object::types::StructDef> {
    world.structs.iter().find(|s| world.names.str(s.name).eq_ignore_ascii_case(ty))
}

// --------------------------------------------------------------------------- writing

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn counted(&mut self, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        self.out.extend_from_slice(&((units.len() * 2) as i32).to_le_bytes());
        for u in units {
            self.out.extend_from_slice(&u.to_le_bytes());
        }
    }

    fn compact(&mut self, value: i32) {
        let negative = value < 0;
        let mut v = value.unsigned_abs();
        let mut first = (v & 0x3f) as u8;
        v >>= 6;
        if negative {
            first |= 0x80;
        }
        if v != 0 {
            first |= 0x40;
        }
        self.out.push(first);
        while v != 0 {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            self.out.push(b);
        }
    }

    fn text(&mut self, text: &str, unicode: bool) {
        if text.is_empty() {
            self.compact(0);
        } else if unicode {
            let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            self.compact(-(units.len() as i32));
            for u in units {
                self.out.extend_from_slice(&u.to_le_bytes());
            }
        } else {
            let bytes: Vec<u8> = text.chars().map(|c| c as u32 as u8).chain(std::iter::once(0)).collect();
            self.compact(bytes.len() as i32);
            self.out.extend_from_slice(&bytes);
        }
    }
}

/// How many bytes a value contributes to the size a tag declares. It is not what the value takes
/// in the file: an object or a class counts one byte and writes none, and a name counts one byte
/// and writes a whole string. Reproducing it is what makes a file we write load in the game.
fn declared_size(value: &Saved) -> u32 {
    match value {
        Saved::Byte(_) => 1,
        Saved::Int(_) | Saved::Float(_) => 4,
        Saved::Bool(_) => 0,
        Saved::Object | Saved::Class | Saved::Name(_) => 1,
        Saved::Str { text, unicode } => {
            let mut w = Writer::default();
            w.text(text, *unicode);
            w.out.len() as u32
        }
        Saved::Struct { fields, .. } => fields.iter().map(field_size).sum(),
    }
}

/// The same count for a field inside a struct, where a bool does take its byte: only at the top
/// level does the tag itself carry the value.
fn field_size(value: &Saved) -> u32 {
    match value {
        Saved::Bool(_) => 1,
        other => declared_size(other),
    }
}

fn write_value(w: &mut Writer, value: &Saved) {
    match value {
        Saved::Byte(b) => w.out.push(*b),
        Saved::Int(i) => w.out.extend_from_slice(&i.to_le_bytes()),
        Saved::Float(f) => w.out.extend_from_slice(&f.to_le_bytes()),
        // A bool at the top of a tag lives in the tag; inside a struct it is a byte.
        Saved::Bool(_) | Saved::Object | Saved::Class => {}
        Saved::Name(n) => w.counted(n),
        Saved::Str { text, unicode } => w.text(text, *unicode),
        Saved::Struct { fields, .. } => {
            for f in fields {
                match f {
                    Saved::Bool(b) => w.out.push(*b as u8),
                    other => write_value(w, other),
                }
            }
        }
    }
}

fn type_code(value: &Saved) -> u8 {
    match value {
        Saved::Byte(_) => 1,
        Saved::Int(_) => 2,
        Saved::Bool(_) => 3,
        Saved::Float(_) => 4,
        Saved::Object => 5,
        Saved::Name(_) => 6,
        Saved::Class => 8,
        Saved::Struct { .. } => 10,
        Saved::Str { .. } => 13,
    }
}

/// The size code UE1 uses for a size: the exact ones first, then a byte, a word or an int.
fn size_code(size: u32) -> u8 {
    match size {
        1 => 0,
        2 => 1,
        4 => 2,
        12 => 3,
        16 => 4,
        s if s < 256 => 5,
        s if s < 65536 => 6,
        _ => 7,
    }
}

/// Writes a `_pa` stream, byte for byte as the game writes it.
pub fn write(level: &Level) -> Vec<u8> {
    let mut w = Writer::default();
    for u in MAGIC.encode_utf16().chain(std::iter::once(0)) {
        w.out.extend_from_slice(&u.to_le_bytes());
    }
    w.counted(&level.name);
    for u in level.unknown {
        w.out.extend_from_slice(&u.to_le_bytes());
    }
    w.out.extend_from_slice(&(level.actors.len() as i32).to_le_bytes());
    for actor in &level.actors {
        w.counted(&actor.name);
        w.counted(&actor.class);
        let mut block = Writer::default();
        for prop in &actor.props {
            write_prop(&mut block, prop);
        }
        block.counted("None");
        w.out.extend_from_slice(&(block.out.len() as i32).to_le_bytes());
        w.out.extend_from_slice(&block.out);
    }
    w.counted("EOF");
    w.out
}

fn write_prop(w: &mut Writer, prop: &Prop) {
    w.counted(&prop.name);
    let size = declared_size(&prop.value);
    let code = size_code(size);
    let mut info = type_code(&prop.value) | (code << 4);
    match &prop.value {
        Saved::Bool(true) => info |= 0x80,
        _ if prop.index != 0 => info |= 0x80,
        _ => {}
    }
    w.out.push(info);
    if let Saved::Struct { ty, .. } = &prop.value {
        w.counted(ty);
    }
    match code {
        5 => w.out.push(size as u8),
        6 => w.out.extend_from_slice(&(size as u16).to_le_bytes()),
        7 => w.out.extend_from_slice(&size.to_le_bytes()),
        _ => {}
    }
    if prop.index != 0 && !matches!(prop.value, Saved::Bool(_)) {
        write_array_index(w, prop.index);
    }
    write_value(w, &prop.value);
}

fn write_array_index(w: &mut Writer, index: u32) {
    if index < 128 {
        w.out.push(index as u8);
    } else if index < 16384 {
        w.out.push(0x80 | (index >> 8) as u8);
        w.out.push((index & 0xff) as u8);
    } else {
        w.out.push(0xc0 | (index >> 24) as u8);
        w.out.push((index >> 16) as u8);
        w.out.push((index >> 8) as u8);
        w.out.push(index as u8);
    }
}
