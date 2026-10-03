use crate::error::{Error, ErrorKind, Result};
use crate::reader::{latin1, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameIndex(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub struct NameEntry {
    /// Latin-1 decoded.
    pub name: String,
    pub flags: u32,
}

/// On disk: 0 = null, < 0 = import, > 0 = export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectRef {
    Null,
    Import(u32),
    Export(u32),
}

impl ObjectRef {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Self::Null,
            v if v < 0 => Self::Import((-(v as i64) - 1) as u32),
            v => Self::Export((v - 1) as u32),
        }
    }

    pub fn is_null(self) -> bool {
        self == Self::Null
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub class_package: NameIndex,
    pub class_name: NameIndex,
    pub outer: ObjectRef,
    pub object_name: NameIndex,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    /// `Null` means the object is a `UClass`.
    pub class: ObjectRef,
    pub super_: ObjectRef,
    pub outer: ObjectRef,
    pub object_name: NameIndex,
    pub flags: u32,
    pub serial_size: u32,
    pub serial_offset: u32,
}

pub(crate) struct Counts {
    pub names: u32,
    pub imports: u32,
    pub exports: u32,
}

impl Counts {
    pub fn name(&self, r: &mut Reader) -> Result<NameIndex> {
        let at = r.offset();
        let v = r.compact()?;
        if v < 0 || v as u32 >= self.names {
            return Err(Error::new(at, ErrorKind::NameIndexOutOfRange(v as i64)));
        }
        Ok(NameIndex(v as u32))
    }

    pub fn check_ref(&self, at: usize, raw: i32) -> Result<ObjectRef> {
        let r = ObjectRef::from_raw(raw);
        let ok = match r {
            ObjectRef::Null => true,
            ObjectRef::Import(i) => i < self.imports,
            ObjectRef::Export(i) => i < self.exports,
        };
        if ok { Ok(r) } else { Err(Error::new(at, ErrorKind::ObjectRefOutOfRange(raw))) }
    }

    pub fn compact_ref(&self, r: &mut Reader) -> Result<ObjectRef> {
        let at = r.offset();
        let raw = r.compact()?;
        self.check_ref(at, raw)
    }

    pub fn i32_ref(&self, r: &mut Reader) -> Result<ObjectRef> {
        let at = r.offset();
        let raw = r.i32()?;
        self.check_ref(at, raw)
    }
}

pub(crate) fn read_name_entry(r: &mut Reader, version: u16) -> Result<NameEntry> {
    let at = r.offset();
    let raw = if version < 64 {
        // NUL-terminated, no length prefix.
        let rest_start = r.pos();
        let mut n = 0;
        loop {
            if r.u8()? == 0 {
                break;
            }
            n += 1;
        }
        r.seek(rest_start)?;
        let s = r.bytes(n)?;
        r.u8()?;
        s
    } else {
        // Compact length including the NUL.
        let len = r.compact_len()?;
        if len == 0 {
            return Err(Error::invalid(at, "zero-length name"));
        }
        let s = r.bytes(len)?;
        if s[len - 1] != 0 {
            return Err(Error::invalid(at, "name without trailing NUL"));
        }
        &s[..len - 1]
    };
    if raw.contains(&0) {
        return Err(Error::invalid(at, "name with embedded NUL"));
    }
    Ok(NameEntry { name: latin1(raw), flags: r.u32()? })
}

pub(crate) fn read_import(r: &mut Reader, c: &Counts) -> Result<Import> {
    Ok(Import {
        class_package: c.name(r)?,
        class_name: c.name(r)?,
        outer: c.i32_ref(r)?,
        object_name: c.name(r)?,
    })
}

pub(crate) fn read_export(r: &mut Reader, c: &Counts) -> Result<Export> {
    let class = c.compact_ref(r)?;
    let super_ = c.compact_ref(r)?;
    let outer = c.i32_ref(r)?;
    let object_name = c.name(r)?;
    let flags = r.u32()?;
    let at = r.offset();
    let serial_size = r.compact()?;
    let serial_size = u32::try_from(serial_size)
        .map_err(|_| Error::invalid(at, format!("negative serial_size {serial_size}")))?;
    let serial_offset = if serial_size > 0 {
        let at = r.offset();
        let o = r.compact()?;
        u32::try_from(o).map_err(|_| Error::invalid(at, format!("negative serial_offset {o}")))?
    } else {
        0
    };
    Ok(Export { class, super_, outer, object_name, flags, serial_size, serial_offset })
}
