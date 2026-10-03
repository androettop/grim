//! UE1 object flags (EObjectFlags) relevant to the on-disk format.

pub const RF_TRANSACTIONAL: u32 = 0x0000_0001;
pub const RF_PUBLIC: u32 = 0x0000_0004;
pub const RF_STANDALONE: u32 = 0x0008_0000;
pub const RF_NATIVE: u32 = 0x0400_0000;
/// A `StateFrame` is serialized before the properties.
pub const RF_HAS_STACK: u32 = 0x0200_0000;
