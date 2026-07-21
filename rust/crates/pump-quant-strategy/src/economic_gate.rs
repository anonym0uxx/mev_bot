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

/// The size-conditioned executable price-impact term of the round-trip cost model.
///
/// Impact is held as an integer slope — how many lamports of position size one bps of
/// executable impact costs — so the whole cost model stays in the integer domain (§22).
/// A steeper (thinner) market has a *smaller* `lamports_per_bps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpactCurve {
    lamports_per_bps: u64,
}

impl ImpactCurve {
    /// Linear impact model: `lamports_per_bps` lamports of size per 1 bps of impact.
    pub const fn linear_test(lamports_per_bps: u64) -> Self {
        Self { lamports_per_bps }
    }

    /// Executable price impact, in bps, of trading `size_lamports` against this curve.
    ///
    /// A zero slope is a market with no depth at all: every non-zero size is unboundedly
    /// expensive, reported as the saturated maximum so the gate refuses rather than
    /// dividing by zero.
    pub fn impact_bps(&self, size_lamports: u64) -> u32 {
        if self.lamports_per_bps == 0 {
            return if size_lamports == 0 { 0 } else { u32::MAX };
        }
        u32::try_from(size_lamports / self.lamports_per_bps).unwrap_or(u32::MAX)
    }
}

/// eg_cost_pct — total round-trip cost at `size_lamports`, in bps of the position:
/// `eff_fixed/size + protocol + impact(size)`.
///
/// This is the U-shaped curve the viability band is cut from: the fixed component falls
/// as size rises (a size-invariant lamport cost amortized over more position), the
/// protocol component is flat, and the impact component rises. `eff_fixed_lamports` is
/// expected to already carry the attempt multiplier from [`effective_fixed_lamports`].
///
/// Returns `None` for `size_lamports == 0` (the fixed component is undefined there — an
/// unsized trade has no cost fraction, not a zero one). Component sums saturate rather
/// than wrap: a cost past `u32::MAX` bps is refused either way, and wrapping it would
/// present a ruinous trade as a cheap one.
pub fn round_trip_cost_bps(
    size_lamports: u64,
    eff_fixed_lamports: u64,
    protocol_bps: u32,
    impact: &ImpactCurve,
) -> Option<u32> {
    if size_lamports == 0 {
        return None;
    }
    let fixed = (eff_fixed_lamports as u128 * BPS_SCALE) / size_lamports as u128;
    let fixed_bps = u32::try_from(fixed).unwrap_or(u32::MAX);
    Some(
        fixed_bps
            .saturating_add(protocol_bps)
            .saturating_add(impact.impact_bps(size_lamports)),
    )
}
