//! The ISO 9660 file system of the disc (ECMA-119): enough to find a file by its path.

use crate::source::{SECTOR, Source};

/// Where a file's bytes are in the image.
#[derive(Clone, Copy, Debug)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
}

struct Record {
    name: String,
    extent: Extent,
    is_dir: bool,
}

pub struct Iso<'a> {
    image: &'a dyn Source,
    root: Extent,
}

impl<'a> Iso<'a> {
    pub fn open(image: &'a dyn Source) -> Result<Self, String> {
        // The primary volume descriptor is the first of the descriptors, at sector 16.
        let pvd = image.read_vec(16 * SECTOR, SECTOR as usize)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return Err(format!("no ISO 9660 primary volume descriptor at {} (type {}, {:02x?})", 16 * SECTOR, pvd[0], &pvd[1..6]));
        }
        let root = parse_record(&pvd[156..190]).ok_or("bad root directory record")?;
        Ok(Self { image, root: root.extent })
    }

    /// A file by its path, `/` separated, ignoring case and the `;1` version suffix.
    pub fn find(&self, path: &str) -> Result<Extent, String> {
        let mut at = self.root;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let want_dir = i + 1 < parts.len();
            let found = self
                .read_dir(at)?
                .into_iter()
                .find(|r| r.is_dir == want_dir && r.name.eq_ignore_ascii_case(part))
                .ok_or_else(|| format!("{path}: not on the disc"))?;
            at = found.extent;
        }
        Ok(at)
    }

    fn read_dir(&self, dir: Extent) -> Result<Vec<Record>, String> {
        let data = self.image.read_vec(dir.offset, dir.len as usize)?;
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < data.len() {
            let len = data[at] as usize;
            // Records never cross a sector: a zero length means the rest of the sector is padding.
            if len == 0 {
                at = (at / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            let bytes = data.get(at..at + len).ok_or_else(|| format!("directory record at {} runs past its directory", dir.offset + at as u64))?;
            if let Some(r) = parse_record(bytes) {
                out.push(r);
            }
            at += len;
        }
        Ok(out)
    }
}

fn parse_record(r: &[u8]) -> Option<Record> {
    if r.len() < 34 {
        return None;
    }
    let lba = u32::from_le_bytes(r[2..6].try_into().ok()?) as u64;
    let len = u32::from_le_bytes(r[10..14].try_into().ok()?) as u64;
    let is_dir = r[25] & 2 != 0;
    let name_len = r[32] as usize;
    let raw = r.get(33..33 + name_len)?;
    // 0 and 1 are the directory's own and its parent's entries.
    let name = match raw {
        [0] => ".".to_string(),
        [1] => "..".to_string(),
        _ => {
            let s = String::from_utf8_lossy(raw);
            let s = s.split(';').next().unwrap_or_default();
            s.strip_suffix('.').unwrap_or(s).to_string()
        }
    };
    Some(Record { name, extent: Extent { offset: lba * SECTOR, len }, is_dir })
}
