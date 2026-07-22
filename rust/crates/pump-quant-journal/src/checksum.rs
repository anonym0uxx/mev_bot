//! Integrity and content-hash primitives for the hot journal (§12).
//!
//! Two distinct integer algorithms are provided, matching §12's two distinct needs:
//!
//! * [`crc32`] — a per-frame integrity check ("frame CRC"). CRC-32/ISO-HDLC
//!   (reflected, polynomial `0xEDB88320`, init/xor-out `0xFFFFFFFF`), the same
//!   ubiquitous CRC used by zlib/gzip/PNG. Its check value over the ASCII bytes
//!   `"123456789"` is `0xCBF43926`, which the tests assert as an independent vector.
//! * [`fnv1a64`] / [`Fnv1a64`] — a 64-bit content hash used for segment checksums
//!   and the manifest content hash. FNV-1a is order-sensitive and streamable, which
//!   is exactly what a rolling append-only checksum needs.
//!
//! Neither uses floating point (§22). CRC and FNV are defined modulo a power of two,
//! so the multiply/shift steps use `wrapping_*` *by contract*, not by accident.

/// Compute the CRC-32/ISO-HDLC checksum of `data`.
///
/// Responsibility (§12 "frame CRC"): detect truncation and single/multi-bit
/// corruption of an encoded frame. Reflected form, polynomial `0xEDB88320`,
/// initial value `0xFFFFFFFF`, final XOR `0xFFFFFFFF`.
///
/// Deterministic and float-free. The bit-loop uses shifts and XOR only.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            // Branchless reflected step: subtract-1 of (crc & 1) yields an all-ones
            // mask when the low bit is set, all-zeros otherwise. wrapping_neg is the
            // integer contract for "-x mod 2^32".
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// FNV-1a-64 offset basis (the seed for an empty input).
pub const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a-64 multiplication prime.
pub const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

/// A streaming FNV-1a-64 hasher.
///
/// Responsibility (§12 "segment checksum"): accumulate a content hash over an
/// append-only byte stream incrementally, so a segment need not re-hash all of its
/// bytes on every append. Feeding the same bytes in the same order always yields the
/// same [`Fnv1a64::finish`] value (order-sensitive by design).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fnv1a64 {
    state: u64,
}

impl Fnv1a64 {
    /// Create a hasher seeded with the FNV offset basis.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: FNV_OFFSET_BASIS_64,
        }
    }

    /// Fold `data` into the running hash (FNV-1a: XOR the byte, then multiply).
    ///
    /// The multiply is defined modulo 2^64, so `wrapping_mul` is the algorithm's
    /// contract, not an overflow bug (§22).
    pub fn update(&mut self, data: &[u8]) {
        let mut hash = self.state;
        for &byte in data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME_64);
        }
        self.state = hash;
    }

    /// Return the current 64-bit hash value.
    #[must_use]
    pub const fn finish(&self) -> u64 {
        self.state
    }
}

impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot FNV-1a-64 content hash of `data`.
///
/// Convenience over [`Fnv1a64`]; `fnv1a64(x)` equals `new().update(x).finish()`.
#[must_use]
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.update(data);
    h.finish()
}
