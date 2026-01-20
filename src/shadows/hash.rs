//! FNV-1a hash functions for schema versioning
//!
//! Provides compile-time hash computation for schema change detection.
//! Used by the derive macro to compute SCHEMA_HASH constants.
//!
//! # Why FNV-1a?
//!
//! - Simple, fast, and well-understood
//! - No external dependencies
//! - Good distribution for short strings (field names)
//! - Can be computed in `const fn`

/// FNV-1a 64-bit offset basis constant
pub const FNV1A_INIT: u64 = 0xcbf29ce484222325;

/// FNV-1a 64-bit prime constant
const FNV1A_PRIME: u64 = 0x100000001b3;

/// Update FNV-1a hash state with a single byte.
///
/// # Example
///
/// ```
/// use rustot::shadows::hash::{FNV1A_INIT, fnv1a_byte};
///
/// let mut h = FNV1A_INIT;
/// h = fnv1a_byte(h, b'a');
/// h = fnv1a_byte(h, b'b');
/// // h now contains hash of "ab"
/// ```
#[inline]
pub const fn fnv1a_byte(state: u64, byte: u8) -> u64 {
    (state ^ (byte as u64)).wrapping_mul(FNV1A_PRIME)
}

/// Update FNV-1a hash state with a byte slice.
///
/// This is a `const fn` so it can be used at compile time for
/// schema hash computation.
///
/// # Example
///
/// ```
/// use rustot::shadows::hash::{FNV1A_INIT, fnv1a_bytes};
///
/// const HASH: u64 = fnv1a_bytes(FNV1A_INIT, b"hello");
/// ```
#[inline]
pub const fn fnv1a_bytes(state: u64, bytes: &[u8]) -> u64 {
    let mut h = state;
    let mut i = 0;
    while i < bytes.len() {
        h = fnv1a_byte(h, bytes[i]);
        i += 1;
    }
    h
}

/// Update FNV-1a hash state with a u64 value.
///
/// Used for composing nested type hashes.
///
/// # Example
///
/// ```
/// use rustot::shadows::hash::{FNV1A_INIT, fnv1a_bytes, fnv1a_u64};
///
/// // Hash a field name, then compose with nested type's hash
/// let mut h = FNV1A_INIT;
/// h = fnv1a_bytes(h, b"config");
/// h = fnv1a_u64(h, 0x1234567890abcdef); // nested Config::SCHEMA_HASH
/// ```
#[inline]
pub const fn fnv1a_u64(state: u64, val: u64) -> u64 {
    fnv1a_bytes(state, &val.to_le_bytes())
}

/// Compute the FNV-1a hash of a byte slice from scratch.
///
/// Convenience function that initializes with FNV1A_INIT.
///
/// # Example
///
/// ```
/// use rustot::shadows::hash::fnv1a_hash;
///
/// const HASH: u64 = fnv1a_hash(b"device");
/// ```
#[inline]
pub const fn fnv1a_hash(bytes: &[u8]) -> u64 {
    fnv1a_bytes(FNV1A_INIT, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_empty() {
        assert_eq!(fnv1a_hash(b""), FNV1A_INIT);
    }

    #[test]
    fn test_fnv1a_known_values() {
        // Test against known FNV-1a test vectors
        // From http://www.isthe.com/chongo/tech/comp/fnv/
        assert_eq!(fnv1a_hash(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_hash(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn test_fnv1a_composition() {
        // Simulates schema hash composition: field name + nested hash
        let nested_hash: u64 = 0x123456789abcdef0;

        let mut h = FNV1A_INIT;
        h = fnv1a_bytes(h, b"config");
        h = fnv1a_u64(h, nested_hash);

        // Should be deterministic
        let mut h2 = FNV1A_INIT;
        h2 = fnv1a_bytes(h2, b"config");
        h2 = fnv1a_u64(h2, nested_hash);

        assert_eq!(h, h2);
    }

    #[test]
    fn test_fnv1a_const_evaluation() {
        // Verify these can be evaluated at compile time
        const HASH1: u64 = fnv1a_hash(b"test");
        const HASH2: u64 = fnv1a_bytes(FNV1A_INIT, b"test");
        assert_eq!(HASH1, HASH2);
    }
}
