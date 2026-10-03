//! The InstallShield cabinet the game is installed from: `data1.hdr`, which describes every
//! file, and the volumes `data1.cab`, `data2.cab`, ... that hold their compressed bytes.
//!
//! Worked out from the Spanish and the English/European discs, which both carry version
//! `0x0100600C`; any other version is refused rather than guessed at. Offsets in the header
//! are relative to the cabinet descriptor unless said otherwise.

use crate::source::Source;

pub const SIGNATURE: [u8; 4] = *b"ISc(";
pub const VERSION: u32 = 0x0100_600C;

/// The descriptor's table of file groups: a hash table of 71 slots, each the start of a chain.
const GROUP_SLOTS_AT: usize = 0x3E;
const GROUP_SLOTS: usize = 71;
const FILE_DESCRIPTOR_LEN: usize = 0x57;

pub const FILE_SPLIT: u16 = 1;
pub const FILE_OBFUSCATED: u16 = 2;
pub const FILE_COMPRESSED: u16 = 4;
pub const FILE_INVALID: u16 = 8;

/// Bytes whose meaning is not known, kept with where they were read.
#[derive(Clone, Debug)]
pub struct Unknown {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub index: u32,
    pub name: String,
    pub directory: String,
    pub flags: u16,
    pub expanded: u64,
    pub compressed: u64,
    /// Where its data starts in its volume.
    pub data_offset: u64,
    /// MD5 of the expanded data: checked on every file of both discs.
    pub md5: [u8; 16],
    /// Hypothesis: the file's version resource (`1f000600a6046400` on the DLLs, zeros on data
    /// files); not needed to extract.
    pub version: Unknown,
    pub link_previous: u32,
    pub link_next: u32,
    pub link_flags: u8,
    /// Hypothesis: attributes and the DOS date and time of the file (`20000000 5c2a 8050`).
    pub stamp: Unknown,
    /// 1 for `data1.cab`, 2 for `data2.cab`, ...
    pub volume: u16,
}

impl FileEntry {
    /// Whether the cabinet holds this file's data. The setup files of the disc are listed with
    /// no flags and no data: they sit next to the cabinet, not in it.
    pub fn has_data(&self) -> bool {
        self.flags & FILE_COMPRESSED != 0 && self.flags & FILE_INVALID == 0
    }
}

#[derive(Clone, Debug)]
pub struct FileGroup {
    pub name: String,
    pub first: u32,
    pub last: u32,
}

pub struct Cabinet {
    pub files: Vec<FileEntry>,
    pub groups: Vec<FileGroup>,
}

fn u16_at(d: &[u8], at: usize) -> Result<u16, String> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| format!("data1.hdr: read past the end at {at:#x}"))
}

fn u32_at(d: &[u8], at: usize) -> Result<u32, String> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("data1.hdr: read past the end at {at:#x}"))
}

fn u64_at(d: &[u8], at: usize) -> Result<u64, String> {
    d.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("data1.hdr: read past the end at {at:#x}"))
}

fn cstr_at(d: &[u8], at: usize) -> Result<String, String> {
    let rest = d.get(at..).ok_or_else(|| format!("data1.hdr: string at {at:#x} past the end"))?;
    let end = rest.iter().position(|&b| b == 0).ok_or_else(|| format!("data1.hdr: unterminated string at {at:#x}"))?;
    // The names are Windows-1252; the game's are all ASCII.
    Ok(rest[..end].iter().map(|&b| b as char).collect())
}

fn check_header(d: &[u8], what: &str) -> Result<(), String> {
    if d.get(0..4) != Some(&SIGNATURE[..]) {
        return Err(format!("{what}: no InstallShield signature at 0 ({:02x?})", d.get(0..4).unwrap_or_default()));
    }
    let version = u32_at(d, 4)?;
    if version != VERSION {
        return Err(format!("{what}: cabinet version {version:#010x} at 4, only {VERSION:#010x} is known"));
    }
    Ok(())
}

impl Cabinet {
    /// Reads `data1.hdr`.
    pub fn parse(hdr: &[u8]) -> Result<Self, String> {
        check_header(hdr, "data1.hdr")?;
        let desc = u32_at(hdr, 0x0C)? as usize;
        let table = desc + u32_at(hdr, desc + 0x0C)? as usize;
        let (table_len, table_len2) = (u32_at(hdr, desc + 0x14)?, u32_at(hdr, desc + 0x18)?);
        if table_len != table_len2 {
            return Err(format!("data1.hdr: file table sizes {table_len} and {table_len2} at {:#x} differ", desc + 0x14));
        }
        let dir_count = u32_at(hdr, desc + 0x1C)? as usize;
        let file_count = u32_at(hdr, desc + 0x28)? as usize;
        let files_at = u32_at(hdr, desc + 0x2C)? as usize;
        // The directory offsets come first in the table, and the file descriptors right after.
        if files_at != dir_count * 4 {
            return Err(format!("data1.hdr: file descriptors at {files_at:#x} of the table, not after its {dir_count} directories"));
        }
        if files_at + file_count * FILE_DESCRIPTOR_LEN > table_len as usize {
            return Err(format!("data1.hdr: {file_count} file descriptors do not fit a {table_len}-byte file table"));
        }
        let directories = (0..dir_count).map(|i| cstr_at(hdr, table + u32_at(hdr, table + i * 4)? as usize)).collect::<Result<Vec<_>, _>>()?;

        let mut files = Vec::with_capacity(file_count);
        for index in 0..file_count {
            let at = table + files_at + index * FILE_DESCRIPTOR_LEN;
            let dir = u16_at(hdr, at + 62)? as usize;
            let name_at = u32_at(hdr, at + 58)? as usize;
            files.push(FileEntry {
                index: index as u32,
                name: cstr_at(hdr, table + name_at)?,
                directory: directories.get(dir).cloned().ok_or_else(|| format!("data1.hdr: directory {dir} of file {index} at {:#x}", at + 62))?,
                flags: u16_at(hdr, at)?,
                expanded: u64_at(hdr, at + 2)?,
                compressed: u64_at(hdr, at + 10)?,
                data_offset: u64_at(hdr, at + 18)?,
                md5: hdr[at + 26..at + 42].try_into().unwrap(),
                version: Unknown { offset: (at + 42) as u64, bytes: hdr[at + 42..at + 58].to_vec() },
                stamp: Unknown { offset: (at + 64) as u64, bytes: hdr[at + 64..at + 76].to_vec() },
                link_previous: u32_at(hdr, at + 76)?,
                link_next: u32_at(hdr, at + 80)?,
                link_flags: hdr[at + 84],
                volume: u16_at(hdr, at + 85)?,
            });
        }

        // Each slot of the group table starts a chain of (name, descriptor, next) triples.
        let mut groups = Vec::new();
        for slot in 0..GROUP_SLOTS {
            let mut link = u32_at(hdr, desc + GROUP_SLOTS_AT + slot * 4)? as usize;
            while link != 0 {
                let (name_at, group_at, next) =
                    (u32_at(hdr, desc + link)? as usize, u32_at(hdr, desc + link + 4)? as usize, u32_at(hdr, desc + link + 8)? as usize);
                let name = cstr_at(hdr, desc + name_at)?;
                let g = desc + group_at;
                // The group keeps a copy of its name apart from the one its link points at.
                let own = cstr_at(hdr, desc + u32_at(hdr, g)? as usize)?;
                if own != name {
                    return Err(format!("data1.hdr: file group at {g:#x} is named {own}, its link at {:#x} says {name}", desc + link));
                }
                let (first, last) = (u32_at(hdr, g + 0x16)?, u32_at(hdr, g + 0x1A)?);
                if first > last || last as usize >= file_count {
                    return Err(format!("data1.hdr: file group {name} at {g:#x} spans files {first}..={last} of {file_count}"));
                }
                groups.push(FileGroup { name, first, last });
                link = next;
            }
        }
        groups.sort_by(|a, b| a.first.cmp(&b.first).then(a.name.cmp(&b.name)));
        Ok(Self { files, groups })
    }

    /// Checks a volume's header against the files the cabinet places in it.
    pub fn check_volume(&self, number: u16, volume: &dyn Source) -> Result<(), String> {
        let what = format!("data{number}.cab");
        let head = volume.read_vec(0, 0x24).map_err(|e| format!("{what}: {e}"))?;
        check_header(&head, &what)?;
        let (first, last) = (u32_at(&head, 0x1C)?, u32_at(&head, 0x20)?);
        for f in self.files.iter().filter(|f| f.volume == number && f.has_data()) {
            if f.index < first || f.index > last {
                return Err(format!("{what}: holds files {first}..={last}, but file {} ({}) is said to be in it", f.index, f.name));
            }
            if f.data_offset + f.compressed > volume.len() {
                return Err(format!("{what}: {} at {} runs {} bytes past the volume's end", f.name, f.data_offset, f.data_offset + f.compressed - volume.len()));
            }
        }
        Ok(())
    }

    /// The expanded bytes of a file, checked against the size and the MD5 the header gives.
    pub fn extract(&self, file: &FileEntry, volume: &dyn Source) -> Result<Vec<u8>, String> {
        let what = format!("{} (file {}, data{}.cab at {})", file.name, file.index, file.volume, file.data_offset);
        if !file.has_data() {
            return Err(format!("{what}: not held in the cabinet (flags {:#x})", file.flags));
        }
        if file.flags & (FILE_SPLIT | FILE_OBFUSCATED) != 0 {
            return Err(format!("{what}: split or obfuscated (flags {:#x}), which no file of the known discs is", file.flags));
        }
        let packed = volume.read_vec(file.data_offset, file.compressed as usize).map_err(|e| format!("{what}: {e}"))?;
        // The data is a run of chunks, each a 16-bit length and a raw DEFLATE stream of its own.
        let mut out = Vec::with_capacity(file.expanded as usize);
        let mut at = 0;
        while at < packed.len() {
            let len = packed.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(|| format!("{what}: chunk length cut at {at}"))?;
            let chunk = packed.get(at + 2..at + 2 + len).ok_or_else(|| format!("{what}: chunk of {len} bytes at {at} runs past the file"))?;
            let expanded = miniz_oxide::inflate::decompress_to_vec(chunk).map_err(|e| format!("{what}: chunk at {at}: {e:?}"))?;
            out.extend_from_slice(&expanded);
            at += 2 + len;
        }
        if out.len() as u64 != file.expanded {
            return Err(format!("{what}: expanded to {} bytes, the header says {}", out.len(), file.expanded));
        }
        if crate::md5::digest(&out) != file.md5 {
            return Err(format!("{what}: MD5 does not match the header's"));
        }
        Ok(out)
    }
}
