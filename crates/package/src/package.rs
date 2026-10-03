use std::path::Path;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::header::{Header, Lineage};
use crate::reader::Reader;
use crate::tables::{
    read_export, read_import, read_name_entry, Counts, Export, Import, NameEntry, NameIndex,
    ObjectRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
    pub what: RegionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Header,
    Heritage,
    Names,
    Imports,
    Exports,
    ExportData(u32),
}

/// Which region covers each byte of the file, and which bytes nothing explains.
#[derive(Debug, Clone)]
pub struct Layout {
    pub regions: Vec<Region>,
    pub gaps: Vec<(usize, usize)>,
    pub overlaps: Vec<(Region, Region)>,
}

/// An indexed package (header + tables); export data is interpreted on demand.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub header: Header,
    pub names: Vec<NameEntry>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    table_ends: TableEnds,
    data: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy)]
struct TableEnds {
    names: usize,
    imports: usize,
    exports: usize,
}

impl Package {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Result<Self>> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        Ok(Self::parse(Self::name_of(path), data.into()))
    }

    /// The package's name, which is its file's name up to the first dot.
    pub fn name_of(path: &Path) -> String {
        path.file_name().and_then(|s| s.to_str()).and_then(|s| s.split('.').next()).unwrap_or_default().to_string()
    }

    pub fn parse(name: String, data: Arc<[u8]>) -> Result<Self> {
        let header = Header::parse(&data)?;
        let mut r = Reader::new(&data);

        let counts = Counts {
            names: header.name_count,
            imports: header.import_count,
            exports: header.export_count,
        };

        r.seek(header.name_offset as usize)?;
        let names = (0..header.name_count)
            .map(|_| read_name_entry(&mut r, header.version))
            .collect::<Result<Vec<_>>>()?;
        let names_end = r.pos();

        r.seek(header.import_offset as usize)?;
        let imports = (0..header.import_count)
            .map(|_| read_import(&mut r, &counts))
            .collect::<Result<Vec<_>>>()?;
        let imports_end = r.pos();

        r.seek(header.export_offset as usize)?;
        let exports = (0..header.export_count)
            .map(|_| read_export(&mut r, &counts))
            .collect::<Result<Vec<_>>>()?;
        let exports_end = r.pos();

        for (i, e) in exports.iter().enumerate() {
            let end = e.serial_offset as u64 + e.serial_size as u64;
            if end > data.len() as u64 {
                return Err(Error::invalid(
                    header.export_offset as usize,
                    format!("export {i}: data [{:#x}+{:#x}] outside the file", e.serial_offset, e.serial_size),
                ));
            }
        }

        Ok(Self {
            name,
            header,
            names,
            imports,
            exports,
            table_ends: TableEnds { names: names_end, imports: imports_end, exports: exports_end },
            data,
        })
    }

    pub fn version(&self) -> u16 {
        self.header.version
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn shared_bytes(&self) -> Arc<[u8]> {
        self.data.clone()
    }

    pub fn name(&self, i: NameIndex) -> &str {
        &self.names[i.0 as usize].name
    }

    /// Case-insensitive, like Unreal.
    pub fn find_name(&self, s: &str) -> Option<NameIndex> {
        self.names
            .iter()
            .position(|n| n.name.eq_ignore_ascii_case(s))
            .map(|i| NameIndex(i as u32))
    }

    pub fn import(&self, i: u32) -> &Import {
        &self.imports[i as usize]
    }

    pub fn export(&self, i: u32) -> &Export {
        &self.exports[i as usize]
    }

    pub fn object_name(&self, r: ObjectRef) -> &str {
        match r {
            ObjectRef::Null => "None",
            ObjectRef::Import(i) => self.name(self.import(i).object_name),
            ObjectRef::Export(i) => self.name(self.export(i).object_name),
        }
    }

    pub fn outer(&self, r: ObjectRef) -> ObjectRef {
        match r {
            ObjectRef::Null => ObjectRef::Null,
            ObjectRef::Import(i) => self.import(i).outer,
            ObjectRef::Export(i) => self.export(i).outer,
        }
    }

    /// An export with a null class is a `Class`.
    pub fn class_name(&self, r: ObjectRef) -> &str {
        match r {
            ObjectRef::Null => "None",
            ObjectRef::Import(i) => self.name(self.import(i).class_name),
            ObjectRef::Export(i) => match self.export(i).class {
                ObjectRef::Null => "Class",
                c => self.object_name(c),
            },
        }
    }

    /// `Outer.Outer.Name`; exports are prefixed with the package name.
    pub fn path_name(&self, r: ObjectRef) -> String {
        let mut parts = vec![self.object_name(r).to_string()];
        let mut cur = self.outer(r);
        let mut guard = 0;
        while !cur.is_null() {
            parts.push(self.object_name(cur).to_string());
            cur = self.outer(cur);
            guard += 1;
            if guard > 64 {
                parts.push("<cycle>".into());
                break;
            }
        }
        if matches!(r, ObjectRef::Export(_)) {
            parts.push(self.name.clone());
        }
        parts.reverse();
        parts.join(".")
    }

    /// Reader over an export's serialized data, with absolute file offsets.
    pub fn export_reader(&self, i: u32) -> Reader<'_> {
        let e = self.export(i);
        let start = e.serial_offset as usize;
        let end = start + e.serial_size as usize;
        Reader::with_base(&self.data[start..end], start)
    }

    pub fn layout(&self) -> Layout {
        let h = &self.header;
        let mut regions = vec![Region { start: 0, end: h.size, what: RegionKind::Header }];
        if let Lineage::Heritage { offset, guids } = &h.lineage {
            let start = *offset as usize;
            regions.push(Region { start, end: start + guids.len() * 16, what: RegionKind::Heritage });
        }
        regions.push(Region { start: h.name_offset as usize, end: self.table_ends.names, what: RegionKind::Names });
        regions.push(Region { start: h.import_offset as usize, end: self.table_ends.imports, what: RegionKind::Imports });
        regions.push(Region { start: h.export_offset as usize, end: self.table_ends.exports, what: RegionKind::Exports });
        for (i, e) in self.exports.iter().enumerate() {
            if e.serial_size > 0 {
                let start = e.serial_offset as usize;
                regions.push(Region {
                    start,
                    end: start + e.serial_size as usize,
                    what: RegionKind::ExportData(i as u32),
                });
            }
        }
        regions.retain(|r| r.end > r.start);
        regions.sort_by_key(|r| (r.start, r.end));

        let mut gaps = Vec::new();
        let mut overlaps = Vec::new();
        let mut cursor = 0usize;
        let mut last: Option<&Region> = None;
        for r in &regions {
            if r.start > cursor {
                gaps.push((cursor, r.start));
            } else if r.start < cursor {
                overlaps.push((last.unwrap().clone(), r.clone()));
            }
            if r.end > cursor {
                cursor = r.end;
                last = Some(r);
            }
        }
        if cursor < self.data.len() {
            gaps.push((cursor, self.data.len()));
        }
        Layout { regions, gaps, overlaps }
    }
}
