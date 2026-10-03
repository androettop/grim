use crate::error::{Error, ErrorKind, Result};
use crate::reader::Reader;

pub const PACKAGE_MAGIC: u32 = 0x9E2A_83C1;

/// Package versions present on the HP2 disc; anything else is rejected until verified.
pub const KNOWN_VERSIONS: &[u16] = &[61, 68, 69, 76, 79];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid(pub [u8; 16]);

impl std::fmt::Display for Guid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let w = |i: usize| u32::from_le_bytes(self.0[i..i + 4].try_into().unwrap());
        write!(f, "{:08X}{:08X}{:08X}{:08X}", w(0), w(4), w(8), w(12))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub export_count: i32,
    pub name_count: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lineage {
    /// v < 68: "heritage" GUID table stored elsewhere in the file.
    Heritage { offset: u32, guids: Vec<Guid> },
    /// v >= 68: package GUID + inline generations.
    Generations { guid: Guid, generations: Vec<Generation> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub version: u16,
    pub licensee: u16,
    pub flags: u32,
    pub name_count: u32,
    pub name_offset: u32,
    pub export_count: u32,
    pub export_offset: u32,
    pub import_count: u32,
    pub import_offset: u32,
    pub lineage: Lineage,
    /// Size of the header itself, starting at offset 0.
    pub size: usize,
}

impl Header {
    pub fn parse(file: &[u8]) -> Result<Self> {
        let mut r = Reader::new(file);
        let magic = r.u32()?;
        if magic != PACKAGE_MAGIC {
            return Err(Error::new(0, ErrorKind::BadMagic(magic)));
        }
        let version = r.u16()?;
        if !KNOWN_VERSIONS.contains(&version) {
            return Err(Error::new(4, ErrorKind::UnsupportedVersion(version)));
        }
        let licensee = r.u16()?;
        let flags = r.u32()?;
        let count = |r: &mut Reader| -> Result<u32> {
            let at = r.offset();
            let v = r.i32()?;
            u32::try_from(v).map_err(|_| Error::invalid(at, format!("negative header value {v}")))
        };
        let name_count = count(&mut r)?;
        let name_offset = count(&mut r)?;
        let export_count = count(&mut r)?;
        let export_offset = count(&mut r)?;
        let import_count = count(&mut r)?;
        let import_offset = count(&mut r)?;

        let (lineage, size) = if version < 68 {
            let n = count(&mut r)?;
            let offset = count(&mut r)?;
            let size = r.pos();
            let mut hr = Reader::new(file);
            hr.seek(offset as usize)?;
            let guids = (0..n).map(|_| hr.array().map(Guid)).collect::<Result<_>>()?;
            (Lineage::Heritage { offset, guids }, size)
        } else {
            let guid = Guid(r.array()?);
            let n = count(&mut r)?;
            let generations = (0..n)
                .map(|_| {
                    Ok(Generation { export_count: r.i32()?, name_count: r.i32()? })
                })
                .collect::<Result<_>>()?;
            (Lineage::Generations { guid, generations }, r.pos())
        };

        Ok(Self {
            version,
            licensee,
            flags,
            name_count,
            name_offset,
            export_count,
            export_offset,
            import_count,
            import_offset,
            lineage,
            size,
        })
    }
}
