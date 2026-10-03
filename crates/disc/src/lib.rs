//! The game disc: a raw `.bin` or an `.iso` of it, the ISO 9660 file system on it, and the
//! InstallShield cabinet the game is installed from.
//!
//! [`install::Disc`] lays the game's data out as an install, which is what the engine reads.

pub mod cab;
pub mod install;
pub mod iso9660;
pub mod md5;
pub mod source;

pub use install::Disc;
pub use source::{FileSource, Source};
