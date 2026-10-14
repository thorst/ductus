//! Shared types: interface configuration and partitioning.

pub mod about;
pub mod alerts;
pub mod config;
pub mod docs;
pub mod env;
pub mod format;
pub mod hotspots;
pub mod impact;
pub mod monitor;
pub mod owners;
pub mod password;
pub mod ports;
pub mod resources;
pub mod retention;
pub mod schedule;
pub mod secrets;
pub mod settings;

pub use config::InterfaceConfig;
pub use env::Environment;
pub use resources::Resources;
pub use retention::{Quarantined, Retention};

/// Map an ordering-key value to a virtual partition.
///
/// FNV-1a: a fixed algorithm, so the same key maps to the same partition across
/// versions and nodes. Never swap this for `std` hashing, which is randomized.
pub fn partition_of(key: &str, partitions: u32) -> i32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % u64::from(partitions.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitioning_is_stable_and_in_range() {
        assert_eq!(partition_of("123456", 64), partition_of("123456", 64));
        assert_eq!(partition_of("", 64), partition_of("", 64));
        for k in ["1", "22", "333", "MRN-0001"] {
            assert!((0..64).contains(&partition_of(k, 64)));
        }
        assert_eq!(partition_of("anything", 1), 0);
    }
}
