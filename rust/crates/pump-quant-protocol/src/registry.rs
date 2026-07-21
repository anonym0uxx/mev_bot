//! Versioned protocol-registry identifiers.
//!
//! # Responsibility
//! Map a trading [`Venue`] to a `(version, hash)` pair used by the supervisor
//! to pin which decoder/builder revision a strategy was compiled against. The
//! hash is a **deterministic placeholder** derived purely from the venue's
//! canonical program-id string — it is not a cryptographic digest, but it is
//! stable across processes and machines so that a version mismatch is
//! detectable without any network call.
//!
//! # Constitution
//! * Deterministic — identical `venue` always yields identical output; no RNG,
//!   clock, or network involved.
//! * §22 — integer-only.

/// A supported trading venue whose data layout this crate can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    /// pump.fun bonding-curve venue.
    PumpFun,
    /// PumpSwap constant-product AMM venue.
    PumpSwap,
}

impl Venue {
    /// Canonical on-chain program id for this venue (base58 string).
    pub const fn program_id(self) -> &'static str {
        match self {
            Venue::PumpFun => "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
            Venue::PumpSwap => "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        }
    }

    /// Registry schema version for this venue's decoders/builders.
    pub const fn version(self) -> u16 {
        match self {
            Venue::PumpFun => 1,
            Venue::PumpSwap => 1,
        }
    }
}

/// Return the versioned protocol-registry id and hash placeholder for `venue`.
///
/// The 32-byte hash is produced by a small deterministic FNV-1a-style fold over
/// the venue's program-id bytes, expanded to fill the array. It is a stable
/// placeholder — swap for a real digest when the registry is finalized.
///
/// # Constitution
/// Deterministic and integer-only; no floats, RNG, clock, or I/O.
pub fn registry_version(venue: Venue) -> (u16, [u8; 32]) {
    (
        venue.version(),
        placeholder_hash(venue.program_id().as_bytes()),
    )
}

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Deterministically expand `seed` bytes into a stable 32-byte placeholder.
///
/// Each output byte is drawn from a running FNV-1a hash that is re-mixed with
/// the byte index, giving good diffusion while remaining fully reproducible.
fn placeholder_hash(seed: &[u8]) -> [u8; 32] {
    let mut base = FNV_OFFSET;
    for &b in seed {
        base ^= b as u64;
        base = base.wrapping_mul(FNV_PRIME);
    }

    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut h = base ^ (i as u64).wrapping_mul(FNV_PRIME);
        h = h.wrapping_mul(FNV_PRIME);
        *slot = (h >> ((i % 8) * 8)) as u8;
    }
    out
}
