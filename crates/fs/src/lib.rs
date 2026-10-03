//! Where the engine reads the game's files from, and writes its own settings to.
//!
//! The engine only goes through [`FileSystem`]: on the desktop that is the installed files
//! ([`NativeFs`]); in a browser there is no file system to read, so the files unpacked from the
//! disc are handed over and kept in memory ([`MemoryFs`]).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
}

pub trait FileSystem: Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Arc<[u8]>>;

    /// The first `len` bytes of a file, or all of it if it is shorter.
    fn read_head(&self, path: &Path, len: usize) -> io::Result<Vec<u8>> {
        let data = self.read(path)?;
        Ok(data[..len.min(data.len())].to_vec())
    }

    /// What a directory holds, in no particular order.
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<Entry>>;

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;

    fn is_file(&self, path: &Path) -> bool;
}

impl dyn FileSystem + '_ {
    /// Every file under `root`, at any depth, sorted.
    pub fn files_under(&self, root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = self.read_dir(&dir) else { continue };
            for e in entries {
                if e.is_dir {
                    stack.push(e.path);
                } else {
                    out.push(e.path);
                }
            }
        }
        out.sort();
        out
    }

    /// Finds `dir/name` ignoring case in every component (the game was made for Windows, so
    /// `name` may also contain `\` separators).
    pub fn find(&self, dir: &Path, name: &str) -> Option<PathBuf> {
        let mut cur = dir.to_path_buf();
        for part in name.split(['\\', '/']).filter(|p| !p.is_empty()) {
            cur = self
                .read_dir(&cur)
                .ok()?
                .into_iter()
                .map(|e| e.path)
                .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(part)))?;
        }
        Some(cur)
    }
}

/// The files of an installed copy, on disk.
pub struct NativeFs;

impl FileSystem for NativeFs {
    fn read(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        std::fs::read(path).map(Into::into)
    }

    fn read_head(&self, path: &Path, len: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut out = Vec::with_capacity(len);
        std::fs::File::open(path)?.take(len as u64).read_to_end(&mut out)?;
        Ok(out)
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let path = e?.path();
            out.push(Entry { is_dir: path.is_dir(), path });
        }
        Ok(out)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        std::fs::write(path, data)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// Files held in memory, by path. Directories are implied by the paths of the files.
#[derive(Default)]
pub struct MemoryFs {
    files: RwLock<BTreeMap<PathBuf, Arc<[u8]>>>,
}

impl MemoryFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, path: impl Into<PathBuf>, data: impl Into<Arc<[u8]>>) {
        self.files.write().unwrap().insert(path.into(), data.into());
    }

    pub fn len(&self) -> usize {
        self.files.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FileSystem for MemoryFs {
    fn read(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        self.files
            .read()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{}: not found", path.display())))
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let files = self.files.read().unwrap();
        let mut out: Vec<Entry> = Vec::new();
        for path in files.keys() {
            let Ok(rest) = path.strip_prefix(dir) else { continue };
            let mut parts = rest.components();
            let Some(first) = parts.next() else { continue };
            let is_dir = parts.next().is_some();
            let child = dir.join(first);
            if !out.last().is_some_and(|e| e.path == child) {
                out.push(Entry { path: child, is_dir });
            }
        }
        if out.is_empty() && !files.keys().any(|p| p.starts_with(dir)) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: not found", dir.display())));
        }
        Ok(out)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.insert(path, data.to_vec());
        Ok(())
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.read().unwrap().contains_key(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_directories_come_from_the_paths() {
        let fs = MemoryFs::new();
        fs.insert("HP2/System/Core.u", vec![1, 2, 3]);
        fs.insert("HP2/System/CUTSCENES/a.int", vec![4]);
        fs.insert("HP2/Maps/PrivetDr.unr", vec![5]);
        let fs: &dyn FileSystem = &fs;
        let mut names: Vec<(String, bool)> = fs
            .read_dir(Path::new("HP2/System"))
            .unwrap()
            .into_iter()
            .map(|e| (e.path.file_name().unwrap().to_string_lossy().to_string(), e.is_dir))
            .collect();
        names.sort();
        assert_eq!(names, [("CUTSCENES".to_string(), true), ("Core.u".to_string(), false)]);
        assert_eq!(fs.files_under(Path::new("HP2")).len(), 3);
        assert_eq!(fs.find(Path::new("HP2"), "system\\cutscenes\\A.INT"), Some(PathBuf::from("HP2/System/CUTSCENES/a.int")));
        assert_eq!(fs.read_head(Path::new("HP2/System/Core.u"), 2).unwrap(), [1, 2]);
        assert!(fs.read_dir(Path::new("HP2/Music")).is_err());
    }
}
