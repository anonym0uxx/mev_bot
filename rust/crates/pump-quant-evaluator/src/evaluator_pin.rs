//! `evaluator_pin` — frozen-evaluator hash-pin verification (constitution §51, §44).
//!
//! Responsibility: before any grade produced by the evaluator is accepted, the
//! evaluator artifact/config must hash to the value pinned in the release
//! manifest. A mismatch is a Tier-0 event — the evaluator has been mutated and
//! its verdicts can no longer be trusted, so results are refused rather than
//! acted upon. The write-authority / re-pinning side lives in the supervisor
//! (constitution §44 trust-on-first-use, operator-only re-pin); this module is
//! the pure, deterministic *verify* half named as the missing PARTIAL leaf.
//!
//! Everything here is integer-only (constitution §22): the digest is a 64-bit
//! FNV-1a hash computed with wrapping arithmetic *by contract* (FNV is defined
//! over the wrapping 2^64 ring). No floats, no wall-clock, no RNG.

/// 64-bit FNV-1a offset basis (the empty-input digest).
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Deterministic 64-bit FNV-1a digest of an arbitrary byte artifact.
///
/// Responsibility: reduce the evaluator artifact/config bytes to a single
/// comparable fingerprint (constitution §51). FNV-1a is defined over the
/// wrapping 2^64 ring, so the multiply/xor are wrapping *by contract*, not by
/// accident. The empty input hashes to [`FNV_OFFSET_BASIS`].
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A pinned digest recorded in the release manifest (constitution §44).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinnedDigest(pub u64);

/// Outcome of comparing a freshly-computed digest against the pinned manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinVerdict {
    /// Computed digest equals the pinned digest — results may be accepted.
    Verified,
    /// Computed digest differs from the pinned digest — Tier-0, refuse results.
    Mismatch {
        /// The digest computed from the live artifact.
        computed: u64,
        /// The digest recorded in the manifest.
        pinned: u64,
    },
}

impl PinVerdict {
    /// True iff the evaluator artifact matches its pin.
    pub fn is_verified(&self) -> bool {
        matches!(self, PinVerdict::Verified)
    }
}

/// Verify evaluator artifact bytes against a pinned manifest digest.
///
/// Responsibility (constitution §51): compute the digest of `artifact_bytes`
/// and compare it to `pinned`. Returns [`PinVerdict::Verified`] on an exact
/// match, otherwise [`PinVerdict::Mismatch`] carrying both digests so the caller
/// can escalate. Pure function of its inputs — deterministic.
pub fn verify_evaluator_pin(artifact_bytes: &[u8], pinned: PinnedDigest) -> PinVerdict {
    let computed = fnv1a_64(artifact_bytes);
    if computed == pinned.0 {
        PinVerdict::Verified
    } else {
        PinVerdict::Mismatch {
            computed,
            pinned: pinned.0,
        }
    }
}

/// Accept a result only if the evaluator artifact matches its pin.
///
/// Responsibility (constitution §51): this is the guard the whole leaf exists
/// for — a result is returned as `Ok(result)` iff the artifact is [verified],
/// and refused as `Err(PinVerdict::Mismatch { .. })` otherwise. No verdict from
/// a mutated evaluator can ever be accepted through this function.
///
/// [verified]: PinVerdict::Verified
pub fn accept_if_pinned<T>(
    artifact_bytes: &[u8],
    pinned: PinnedDigest,
    result: T,
) -> Result<T, PinVerdict> {
    match verify_evaluator_pin(artifact_bytes, pinned) {
        PinVerdict::Verified => Ok(result),
        mismatch => Err(mismatch),
    }
}
