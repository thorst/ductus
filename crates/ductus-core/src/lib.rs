//! Shared types: interface configuration and partitioning.

pub mod about;
pub mod alerts;
pub mod allow;
pub mod config;
pub mod deprecated;
pub mod docs;
pub mod env;
pub mod format;
pub mod hotspots;
pub mod impact;
pub mod ldap;
pub mod monitor;
pub mod owners;
pub mod password;
pub mod pgtls;
pub mod ports;
pub mod resources;
pub mod retention;
pub mod schedule;
pub mod schema;
pub mod secrets;
pub mod service_account;
pub mod settings;
pub mod tls;

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

/// Matching with `*` (any run of characters) and `?` (any one character): file names, and
/// certificate subjects.
pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    // Where the last `*` was, and where in the name it started matching: on a mismatch, let
    // that `*` swallow one more character and try again.
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("*", "anything.csv"));
        assert!(glob("*.csv", "labs.csv") && !glob("*.csv", "labs.csv.done"));
        assert!(glob("ADT_??.hl7", "ADT_01.hl7") && !glob("ADT_??.hl7", "ADT_1.hl7"));
        assert!(glob("a*b*c", "aXXbYYc") && !glob("a*b*c", "aXXbYY"));
        assert!(glob("*_*.txt", "x_y_z.txt"));
        assert!(!glob("labs", "labs2"));
    }

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
