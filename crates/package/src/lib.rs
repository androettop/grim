//! Reader for Unreal Engine 1 packages as used by Harry Potter and the Chamber of Secrets
//! (PC, 2002): header, name/import/export tables, compact indices, object references and the
//! serialization shared by every `UObject`. Per-class data lives in `grim-assets`.

pub mod error;
pub mod flags;
pub mod header;
pub mod hexdump;
pub mod library;
pub mod object;
pub mod package;
pub mod property;
pub mod reader;
pub mod tables;
pub mod unknown;

pub use error::{Error, ErrorKind, Result};
pub use header::{Guid, Header, Lineage};
pub use library::{Library, ObjectHandle};
pub use package::{Layout, Package, Region, RegionKind};
pub use reader::Reader;
pub use tables::{Export, Import, NameEntry, NameIndex, ObjectRef};
pub use unknown::Unknown;
