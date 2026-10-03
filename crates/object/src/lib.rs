//! Runtime object model built on the parsed packages: global names, classes linked across
//! packages, property layouts, defaults and object instantiation.

pub mod names;
pub mod types;
pub mod value;
pub mod world;

pub use names::{NameId, Names};
pub use types::*;
pub use value::Value;
pub use world::{LinkError, Object, World};

/// A hasher for the keys the engine looks up thousands of times a frame: object keys, name
/// ids, class ids. They are small integers, and the standard hasher's strength is wasted on
/// them — this is the multiply-and-rotate one that compilers use for the same job.
#[derive(Default, Clone, Copy)]
pub struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.write_u64(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }

    fn write_u64(&mut self, n: u64) {
        const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(SEED);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

pub type FastBuild = std::hash::BuildHasherDefault<FastHasher>;
/// A map keyed by something small, hashed cheaply.
pub type FastMap<K, V> = std::collections::HashMap<K, V, FastBuild>;
/// The same, without values.
pub type FastSet<K> = std::collections::HashSet<K, FastBuild>;
