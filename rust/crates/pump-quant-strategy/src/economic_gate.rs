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

/// eg_min_viable_size — the smallest size `x_min` whose expected executable move still clears
/// the whole cost floor with `margin_bps` to spare: `expected_move_bps >= cost(x) + margin`.
///
/// Below `x_min` the un-amortized fixed cost eats the edge, so a risk-permitted size under it
/// is a *refusal*, never a shrunk position — this is the arithmetic that makes the legacy
/// 0.01-SOL structural loss impossible by construction. Returns `None` when no size up to
/// `search_hi_lamports` clears (the candidate is inadmissible at any size — e.g. a move
/// thinner than protocol + margin, which no amount of amortization can rescue).
pub fn min_viable_size(
    expected_move_bps: u32,
    eff_fixed_lamports: u64,
    protocol_bps: u32,
    margin_bps: u32,
    impact: &ImpactCurve,
    search_hi_lamports: u64,
) -> Option<u64> {
    let clears = |size: u64| {
        round_trip_cost_bps(size, eff_fixed_lamports, protocol_bps, impact)
            .is_some_and(|cost| expected_move_bps >= cost.saturating_add(margin_bps))
    };
    if search_hi_lamports == 0 {
        return None;
    }
    // Bracket the lower crossing by doubling from the smallest representable size. The
    // crossing sits on the falling (fixed-cost-dominated) branch of the U, so every probe
    // that fails proves all smaller sizes fail too: when a probe finally clears, the answer
    // is bracketed in `(lo, hi]` with `lo` known-infeasible.
    let (mut lo, mut hi) = (0u64, 1u64);
    loop {
        let probe = hi.min(search_hi_lamports);
        if clears(probe) {
            hi = probe;
            break;
        }
        if probe == search_hi_lamports {
            return None;
        }
        lo = probe;
        hi = probe.saturating_mul(2);
    }
    // Inside the bracket the feasible set is an upper interval, so bisection lands on the
    // exact smallest clearing size — deterministic, pure integer, no float anywhere.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if clears(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// Whether a candidate is economically admissible at *any* size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The band is non-empty: some size clears the whole cost floor with margin.
    Admit,
    /// The band is empty: no size clears. Refuse — never shrink into a guaranteed loss.
    Refuse,
}

/// The size-viability band cut from the U-shaped round-trip cost curve.
///
/// These are *constraints* on Section 49's sizing, not a trade size: the far larger
/// unconstrained profit-maximizing size is deliberately not computed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeBand {
    pub verdict: Verdict,
    /// Smallest size that clears the cost floor with margin.
    pub x_min: u64,
    /// Cost-minimizing size — the bottom of the U. Reported as context, never traded.
    pub x_cost: u64,
    /// Largest size the impact curve and sellability allow.
    pub x_max: u64,
}

impl SizeBand {
    /// The empty band. Every field is zero: a refusal carries no permitted size at all.
    pub const REFUSE: Self = Self {
        verdict: Verdict::Refuse,
        x_min: 0,
        x_cost: 0,
        x_max: 0,
    };
}

/// eg_size_band — assemble the full viability band `[x_min, x_cost, x_max]` and the verdict.
///
/// Admit iff a minimum viable size exists and fits under the maximum; otherwise the band is
/// empty and the candidate is refused outright.
pub fn size_band(
    expected_move_bps: u32,
    base_fixed_lamports: u64,
    fail_rate_bps: u32,
    protocol_bps: u32,
    margin_bps: u32,
    depth_lamports: u64,
    impact: &ImpactCurve,
    sellable_max_lamports: u64,
) -> SizeBand {
    // Certain failure has no finite attempt count, so no fixed cost to amortize — fail closed.
    let Some(eff) = effective_fixed_lamports(base_fixed_lamports, fail_rate_bps) else {
        return SizeBand::REFUSE;
    };
    // Upper bound: the impact budget is whatever the edge leaves after protocol and margin.
    // `impact_bps` is monotone non-decreasing in size, so bisecting over `[0, sellable_max]`
    // yields exactly `min(impact-bounded size, sellable_max_lamports)`.
    let impact_budget_bps = expected_move_bps
        .saturating_sub(protocol_bps)
        .saturating_sub(margin_bps);
    let (mut lo, mut hi) = (0u64, sellable_max_lamports);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if impact.impact_bps(mid) <= impact_budget_bps {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let x_max = lo;
    // Lower bound over the whole sellable range, then tested against the upper bound: an
    // x_min past x_max is an empty band, which is a refusal and not a clamped trade.
    let Some(x_min) = min_viable_size(
        expected_move_bps,
        eff,
        protocol_bps,
        margin_bps,
        impact,
        sellable_max_lamports,
    ) else {
        return SizeBand::REFUSE;
    };
    if x_min > x_max {
        return SizeBand::REFUSE;
    }
    // Bottom of the U: cost'(x) = 0 at x = sqrt(eff * depth / 2) under the reserve impact
    // model (a round trip moves ~2x/depth of price). The depth and the decoded impact curve
    // are independent inputs, so pin the reference inside the band it is describing.
    let x_cost_unclamped = u64::try_from((eff as u128 * depth_lamports as u128 / 2).isqrt())
        .unwrap_or(u64::MAX)
        .max(x_min);
    SizeBand {
        verdict: Verdict::Admit,
        x_min,
        x_cost: x_cost_unclamped.min(x_max),
        x_max,
    }
}
