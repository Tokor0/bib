//! Small helpers with no better home.

/// FNV-1a, inlined rather than pulled in as a dependency.
///
/// `DefaultHasher` would do the job but its output is explicitly not stable
/// across Rust releases, which would silently invalidate caches and indexes on
/// a toolchain upgrade.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

pub fn fnv1a_str(text: &str) -> u64 {
    fnv1a(text.as_bytes())
}
