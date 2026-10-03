//! Random access to the bytes of a disc image, wherever they are kept.

use std::sync::{Arc, Mutex};

pub trait Source {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read_vec(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0; len];
        self.read_at(offset, &mut buf)?;
        Ok(buf)
    }
}

impl Source for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|_| format!("offset {offset} out of range"))?;
        let bytes = start.checked_add(buf.len()).and_then(|end| self.get(start..end));
        let bytes = bytes.ok_or_else(|| format!("read of {} bytes at {offset} past the end ({})", buf.len(), <[u8]>::len(self)))?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

impl Source for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.as_slice().read_at(offset, buf)
    }
}

/// A file on disk.
pub struct FileSource {
    file: Mutex<std::fs::File>,
    len: u64,
}

impl FileSource {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        Ok(Self { file: Mutex::new(file), len })
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = self.file.lock().map_err(|_| "file lock poisoned".to_string())?;
        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        file.read_exact(buf).map_err(|e| format!("read of {} bytes at {offset}: {e}", buf.len()))
    }
}

/// A stretch of another source, such as a file inside the disc image.
pub struct Slice {
    pub source: Arc<dyn Source>,
    pub offset: u64,
    pub len: u64,
}

impl Source for Slice {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if offset.checked_add(buf.len() as u64).is_none_or(|end| end > self.len) {
            return Err(format!("read of {} bytes at {offset} past the end of a {}-byte file", buf.len(), self.len));
        }
        self.source.read_at(self.offset + offset, buf)
    }
}

pub const SECTOR: u64 = 2048;
const RAW_SECTOR: u64 = 2352;
/// Every raw CD sector starts with this; an ISO starts with 32 KiB of zeros instead.
const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// The 2048-byte data sectors of a disc image: an ISO holds them back to back, a raw `.bin`
/// wraps each in a 2352-byte sector (12 bytes of sync, a 4-byte header, then the data).
pub struct Image {
    source: Arc<dyn Source>,
    raw: bool,
}

impl Image {
    pub fn open(source: Arc<dyn Source>) -> Result<Self, String> {
        let head = source.read_vec(0, 16).map_err(|e| format!("disc image too short: {e}"))?;
        let raw = head[..12] == SYNC;
        if raw {
            // Byte 15 is the sector's mode: 1 is 2048 bytes of data, which is what this disc is.
            if head[15] != 1 {
                return Err(format!("raw sector of mode {}, only mode 1 is read", head[15]));
            }
            if source.len() % RAW_SECTOR != 0 {
                return Err(format!("raw image of {} bytes is not a whole number of 2352-byte sectors", source.len()));
            }
        }
        Ok(Self { source, raw })
    }

    pub fn is_raw(&self) -> bool {
        self.raw
    }
}

impl Source for Image {
    fn len(&self) -> u64 {
        if self.raw { self.source.len() / RAW_SECTOR * SECTOR } else { self.source.len() }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if !self.raw {
            return self.source.read_at(offset, buf);
        }
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            let (sector, within) = (at / SECTOR, at % SECTOR);
            let take = ((SECTOR - within) as usize).min(buf.len() - done);
            self.source.read_at(sector * RAW_SECTOR + 16 + within, &mut buf[done..done + take])?;
            done += take;
        }
        Ok(())
    }
}
