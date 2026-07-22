//! Deterministic content hashing for sealed experiments (§56.1, §56.9).
//!
//! Responsibility: turn a record into a canonical, unambiguous byte string and
//! fold it into a fixed-width fingerprint using a fully deterministic integer
//! hash (FNV-1a, 64-bit). No RNG, no wall clock, no float, no external crate —
//! the same input always yields the same [`SealHash`] on every platform, which is
//! what makes "seal → hash → reject mutation" testable and replay-stable (§22).
//!
//! The hash is used for *immutability fingerprinting*, not cryptographic
//! authentication; per §56.1/§56.9 the goal is deterministic tamper-evidence of
//! sealed research records.

/// A sealed-record content fingerprint (§56.1). Two records seal to the same
/// `SealHash` iff their canonical encodings are byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SealHash(pub u64);

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Deterministic FNV-1a 64-bit hash of a byte slice.
///
/// Overflow contract: the multiply and xor are **wrapping by contract** — FNV is
/// defined modulo 2^64, so `wrapping_mul` is the correct, intended arithmetic, not
/// an overflow bug (§22 explicit-overflow discipline).
#[must_use]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Append `v` to `buf` as 8 little-endian bytes. A fixed-width encoding so field
/// boundaries are unambiguous.
pub fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Append `v` to `buf` as 4 little-endian bytes.
pub fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Append a length-prefixed byte string: an 8-byte little-endian length followed
/// by the bytes. Length-prefixing prevents field-splicing ambiguity (so that
/// e.g. `("ab", "c")` and `("a", "bc")` cannot collide).
pub fn push_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}
