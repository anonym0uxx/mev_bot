//! Deterministic integer hashing primitives (constitution 22).
//!
//! Two consumers, one implementation, zero dependencies:
//!
//! * [`fnv1a_64`] is the record checksum used by [`crate::persist`]'s append-only
//!   journal — it detects the torn tail of a crashed write.
//! * [`mix_u32`] is the avalanche mixer used by [`crate::fingerprint`] to spread
//!   sparse `meta_category_id` values across the fixed nominal slot budget of the
//!   packed signature.
//!
//! Neither is a cryptographic hash and neither is used as one: the journal defends
//! against *torn writes and bit rot*, not against an adversary, and the slot mixer
//! only needs uniformity. Both are pure integer functions with no wall clock, no
//! RNG and no I/O, so both are replay-stable: the same bytes always produce the
//! same digest on every machine, forever. That stability is load-bearing — a
//! snapshot written today must verify byte-identically after a restart tomorrow.

/// FNV-1a 64-bit offset basis (constitution 102: named const, no magic numbers).
pub const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime (constitution 102).
pub const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit digest of `bytes`.
///
/// Deterministic and endian-independent: the algorithm consumes one byte at a
/// time, so the digest of a byte slice is identical on every target. Multiplication
/// is `wrapping_mul` by construction — wrapping *is* the FNV specification here,
/// not an overflow accident (constitution 22 requires the overflow strategy be
/// explicit at the site; this is it).
#[must_use]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut acc = FNV1A_64_OFFSET_BASIS;
    for b in bytes {
        acc ^= u64::from(*b);
        acc = acc.wrapping_mul(FNV1A_64_PRIME);
    }
    acc
}

/// Multiplier for the 32-bit avalanche mixer (constitution 102). This is the
/// well-known MurmurHash3 finalizer constant `0x85eb_ca6b`.
pub const MIX_U32_M1: u32 = 0x85eb_ca6b;

/// Second multiplier for the 32-bit avalanche mixer (constitution 102):
/// MurmurHash3 finalizer constant `0xc2b2_ae35`.
pub const MIX_U32_M2: u32 = 0xc2b2_ae35;

/// Avalanche-mix a `u32` so that low-cardinality, sequentially assigned category
/// identifiers do not land in systematically adjacent slots.
///
/// Pure, deterministic, wrapping-by-specification (see [`fnv1a_64`]).
#[must_use]
pub const fn mix_u32(x: u32) -> u32 {
    let mut h = x;
    h ^= h >> 16;
    h = h.wrapping_mul(MIX_U32_M1);
    h ^= h >> 13;
    h = h.wrapping_mul(MIX_U32_M2);
    h ^= h >> 16;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_deterministic_across_calls() {
        let a = fnv1a_64(b"hermes-episodic-brain");
        let b = fnv1a_64(b"hermes-episodic-brain");
        assert_eq!(a, b);
    }

    #[test]
    fn fnv1a_empty_is_offset_basis() {
        assert_eq!(fnv1a_64(&[]), FNV1A_64_OFFSET_BASIS);
    }

    #[test]
    fn fnv1a_detects_single_bit_flip() {
        let clean = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut dirty = clean;
        dirty[4] ^= 0b0000_1000;
        assert_ne!(fnv1a_64(&clean), fnv1a_64(&dirty));
    }

    #[test]
    fn fnv1a_is_order_sensitive() {
        assert_ne!(fnv1a_64(&[1, 2]), fnv1a_64(&[2, 1]));
    }

    #[test]
    fn mix_u32_is_deterministic_and_spreads_small_ids() {
        assert_eq!(mix_u32(7), mix_u32(7));
        // Sixteen sequential ids must not collapse onto a handful of 4-bit slots.
        let mut seen = [false; 16];
        let mut distinct = 0usize;
        for id in 0u32..16 {
            let slot = (mix_u32(id) % 16) as usize;
            if !seen[slot] {
                seen[slot] = true;
                distinct += 1;
            }
        }
        assert!(
            distinct >= 8,
            "mixer collapsed sequential ids: {distinct} distinct slots"
        );
    }
}
