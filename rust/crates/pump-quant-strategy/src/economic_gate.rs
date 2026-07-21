//! economic_gate — implemented leaf-by-leaf against the dossier property tests.
//! Functions are added here by the build; this skeleton only establishes the module.

/// Basis-point denominator. Every ratio in this module is an integer bps fraction of
/// this scale (§22: no floats in outcome-controlling paths).
const BPS_SCALE: u128 = 10_000;

/// eg_effective_fixed — inflate the size-invariant fixed cost (priority + tip + gas) by the
/// failure-rate attempt multiplier `1 / (1 - p)`, `p = fail_rate_bps / 10_000`.
///
/// A failed transaction still pays its fixed cost and lands nothing, so the fixed cost the
/// gate must amortize is the *expected* cost per landed fill, not the per-attempt cost.
/// Returns `None` for `fail_rate_bps >= 10_000` (certain failure — no finite attempt count)
/// and on overflow of the u64 lamport domain.
pub fn effective_fixed_lamports(base_fixed_lamports: u64, fail_rate_bps: u32) -> Option<u64> {
    let p = fail_rate_bps as u128;
    if p >= BPS_SCALE {
        return None;
    }
    let inflated = (base_fixed_lamports as u128).checked_mul(BPS_SCALE)? / (BPS_SCALE - p);
    u64::try_from(inflated).ok()
}
