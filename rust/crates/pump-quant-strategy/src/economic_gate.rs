//! # economic_gate
//!
//! The arithmetic core of the `MinimumEconomicTradeGate`: the U-shaped round-trip
//! cost model and the size-viability band it produces.
//!
//! Round-trip cost as a fraction of the position (in bps) is
//!
//! ```text
//!     cost(x) = fixed / x  +  protocol  +  impact(x)
//! ```
//!
//! where `fixed` is the size-invariant cost (priority + tip + gas) *inflated* by the
//! failure-rate attempt multiplier `1 / (1 - p_fail)` — failed transactions still pay
//! the fixed cost and land nothing — `protocol` is a size-invariant rate, and `impact(x)`
//! rises with size per the decoded curve / reserve model.
//!
//! From this model the gate derives, per candidate and per decoded market state, a
//! viability band:
//!
//! * `x_min`  — the smallest size whose expected executable move still clears the full
//!              cost floor *with configured margin*. A risk-permitted size below `x_min`
//!              yields `Refuse` — we never shrink into a guaranteed loss.
//! * `x_cost` — the cost-minimizing reference size (bottom of the U), reported as
//!              context only; it is **never** used as the trade size.
//! * `x_max`  — the largest viable size (impact + sellability bound).
//!
//! These are *inputs* to Section-49 sizing and are explicitly distinct from the
//! far-larger unconstrained profit-maximizing size `R*(edge - protocol)/4`.
//!
//! All math is integer / fixed-point (lamports as `u64`/`u128`, ratios in bps). There is
//! no `f32`/`f64` anywhere in an outcome-controlling path, and every operation that could
//! overflow is explicit (checked / saturating). Identical inputs always produce identical
//! output.

/// Basis-points scale (`10_000 bps == 100%`).
pub const BPS_SCALE: u32 = 10_000;

// ---------------------------------------------------------------------------
// Impact curve
// ---------------------------------------------------------------------------

/// The size-dependent price-impact component of round-trip cost, in bps.
///
/// Modelled as a linear (reserve-style) curve: `impact_bps(size) = size * num / den`.
/// A larger `den` (deeper effective liquidity) means less impact per lamport. The curve
/// is monotonically non-decreasing in size, which is what makes the total cost U-shaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImpactCurve {
    /// Numerator of the per-lamport impact slope (bps per lamport = `num / den`).
    slope_num: u128,
    /// Denominator of the per-lamport impact slope. Never zero.
    slope_den: u128,
}

impl ImpactCurve {
    /// Construct a linear impact curve with slope `1 / den` bps per lamport.
    ///
    /// `den` is clamped to a minimum of 1 to keep the curve well-defined (a `den` of 0
    /// would mean infinite impact and is meaningless).
    pub fn linear_test(den: u64) -> Self {
        ImpactCurve {
            slope_num: 1,
            slope_den: (den as u128).max(1),
        }
    }

    /// General linear constructor: `impact_bps(size) = size * num / den`.
    ///
    /// Both `num` and `den` are clamped to a minimum of 1.
    pub fn linear(num: u64, den: u64) -> Self {
        ImpactCurve {
            slope_num: (num as u128).max(1),
            slope_den: (den as u128).max(1),
        }
    }

    /// Impact cost at `size_lamports`, in bps.
    ///
    /// Rises monotonically with size. Saturates at `u32::MAX` rather than overflowing.
    pub fn impact_bps(&self, size_lamports: u64) -> u32 {
        let raw = (size_lamports as u128)
            .saturating_mul(self.slope_num)
            / self.slope_den;
        raw.min(u32::MAX as u128) as u32
    }

    /// Largest size whose impact cost does not exceed `budget_bps`.
    ///
    /// This is the inverse of [`impact_bps`](Self::impact_bps): the impact-bounded upper
    /// size. Saturates at `u64::MAX`.
    pub fn max_size_at(&self, budget_bps: u32) -> u64 {
        let raw = (budget_bps as u128).saturating_mul(self.slope_den) / self.slope_num;
        raw.min(u64::MAX as u128) as u64
    }
}

// ---------------------------------------------------------------------------
// Leaf: eg_effective_fixed
// ---------------------------------------------------------------------------

/// Inflate the size-invariant fixed cost by the failure-rate attempt multiplier.
///
/// A trade that fails still pays its fixed cost (priority + tip + gas) but lands nothing,
/// so the *expected* fixed cost per successful landing is `base * 1/(1 - p)` where
/// `p = fail_rate_bps / 10_000`.
///
/// * `fail_rate_bps == 0` returns `base` unchanged (multiplier `1.0`).
/// * The multiplier is strictly increasing in `p`.
/// * `fail_rate_bps >= 10_000` (certain failure) returns `None` — no finite attempt count
///   makes a landing possible.
///
/// Integer math: `effective = base * 10_000 / (10_000 - fail_rate_bps)`, computed in
/// `u128` to avoid overflow.
pub fn effective_fixed_lamports(base_fixed_lamports: u64, fail_rate_bps: u32) -> Option<u64> {
    if fail_rate_bps >= BPS_SCALE {
        return None;
    }
    let denom = (BPS_SCALE - fail_rate_bps) as u128;
    let eff = (base_fixed_lamports as u128 * BPS_SCALE as u128) / denom;
    Some(eff.min(u64::MAX as u128) as u64)
}

// ---------------------------------------------------------------------------
// Leaf: eg_cost_pct
// ---------------------------------------------------------------------------

/// Total round-trip cost at `size_lamports`, in bps: `fixed/x + protocol + impact(x)`.
///
/// * The fixed component in bps is `eff_fixed * 10_000 / size`; it *falls* as size rises.
/// * The protocol component is size-invariant.
/// * The impact component *rises* with size per the curve.
///
/// Returns `None` when `size_lamports == 0` (division guard). The total is the exact
/// integer sum of the three bps components, saturated to `u32::MAX` rather than
/// overflowing.
pub fn round_trip_cost_bps(
    size_lamports: u64,
    eff_fixed_lamports: u64,
    protocol_bps: u32,
    impact: &ImpactCurve,
) -> Option<u32> {
    if size_lamports == 0 {
        return None;
    }
    let fixed_bps = (eff_fixed_lamports as u128 * BPS_SCALE as u128) / size_lamports as u128;
    let total = fixed_bps
        + protocol_bps as u128
        + impact.impact_bps(size_lamports) as u128;
    Some(total.min(u32::MAX as u128) as u32)
}

// ---------------------------------------------------------------------------
// Leaf: eg_min_viable_size
// ---------------------------------------------------------------------------

/// Smallest size whose expected move clears the full cost floor with margin (`x_min`).
///
/// Returns the smallest `size` in `[1, search_hi_lamports]` for which
///
/// ```text
///     expected_move_bps >= round_trip_cost_bps(size) + margin_bps
/// ```
///
/// Because the cost curve is U-shaped, the lower feasibility crossing sits on the falling
/// (fixed-cost-dominated) branch where cost decreases as size rises; the feasible region
/// is the contiguous interval containing the cost minimum `x_cost`. We bracket it with a
/// bounded geometric (doubling) scan from a small seed up to `search_hi`, then binary
/// search the bracket for the exact smallest satisfying size.
///
/// Returns `None` when no size up to `search_hi` satisfies the constraint — i.e. the
/// candidate is inadmissible at any size (for example when `protocol + margin` alone
/// already exceeds the expected move). Pure integer, deterministic.
pub fn min_viable_size(
    expected_move_bps: u32,
    eff_fixed_lamports: u64,
    protocol_bps: u32,
    margin_bps: u32,
    impact: &ImpactCurve,
    search_hi_lamports: u64,
) -> Option<u64> {
    if search_hi_lamports == 0 {
        return None;
    }

    // Feasibility predicate: does `size` clear the full floor with margin?
    let clears = |size: u64| -> bool {
        match round_trip_cost_bps(size, eff_fixed_lamports, protocol_bps, impact) {
            Some(cost) => (expected_move_bps as u64) >= cost as u64 + margin_bps as u64,
            None => false,
        }
    };

    // Geometric scan upward to find the first clearing size, keeping the last
    // non-clearing size as the lower bracket bound.
    let mut lo: u64 = 0; // last size known NOT to clear (0 = below the whole range)
    let mut probe: u64 = 1;
    loop {
        let s = probe.min(search_hi_lamports);
        if clears(s) {
            // Bracket is (lo, s]: lo does not clear, s clears. Binary search the
            // smallest clearing size. On this branch the predicate is monotone
            // (false -> true) so this yields the exact x_min.
            let mut a = lo; // does not clear (or sentinel 0)
            let mut b = s; // clears
            while b - a > 1 {
                let mid = a + (b - a) / 2;
                if clears(mid) {
                    b = mid;
                } else {
                    a = mid;
                }
            }
            return Some(b);
        }
        if s == search_hi_lamports {
            return None; // exhausted the search range without clearing
        }
        lo = s;
        probe = s.saturating_mul(2);
    }
}

// ---------------------------------------------------------------------------
// Leaf: eg_size_band
// ---------------------------------------------------------------------------

/// The admit/refuse decision produced by the economic gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The candidate has a non-empty viability band.
    Admit,
    /// The candidate is inadmissible: no size clears the floor, or the band is empty.
    Refuse,
}

/// The full viability band for a candidate.
///
/// On `Refuse` all sizes are `0` and carry no meaning. On `Admit` the band satisfies
/// `x_min <= x_cost <= x_max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeBand {
    /// Admit or refuse.
    pub verdict: Verdict,
    /// Smallest viable size (lower feasibility crossing).
    pub x_min: u64,
    /// Cost-minimizing reference size (bottom of the U). Context only — never the trade size.
    pub x_cost: u64,
    /// Largest viable size (impact + sellability bound).
    pub x_max: u64,
}

impl SizeBand {
    /// A refusal band with all sizes zeroed.
    pub fn refuse() -> Self {
        SizeBand {
            verdict: Verdict::Refuse,
            x_min: 0,
            x_cost: 0,
            x_max: 0,
        }
    }

    /// An admission band. `x_cost` is clamped into `[x_min, x_max]` so the ordering
    /// invariant `x_min <= x_cost <= x_max` always holds.
    pub fn admit(x_min: u64, x_cost: u64, x_max: u64) -> Self {
        SizeBand {
            verdict: Verdict::Admit,
            x_min,
            x_cost: x_cost.clamp(x_min, x_max),
            x_max,
        }
    }
}

/// Assemble the full viability band `[x_min, x_cost, x_max]` and the admit/refuse verdict.
///
/// Pipeline:
/// 1. Inflate the fixed cost by the failure multiplier ([`effective_fixed_lamports`]);
///    certain failure -> `Refuse`.
/// 2. `x_max` = `min(impact-bounded size at the leftover edge budget, sellable_max)`.
///    The impact budget is `expected_move - protocol - margin` — the edge left over for
///    impact once protocol and margin are paid (fixed cost is negligible at large size).
///    A negative budget means even a dust-sized trade cannot clear -> `Refuse`.
/// 3. `x_cost` ~= `isqrt(eff_fixed * depth / 2)` — the analytic bottom of the U, reported
///    as context and clamped into the band.
/// 4. `x_min` via [`min_viable_size`] searched up to `x_max`; `None` -> `Refuse`.
///
/// The verdict is `Admit` iff a viable `x_min` exists and `x_min <= x_max`. The
/// profit-maximizing size `R*(move - protocol)/4` is deliberately **not** computed here —
/// that is Section 49's job; this leaf only bounds it.
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
    // 1. Effective (failure-inflated) fixed cost.
    let eff = match effective_fixed_lamports(base_fixed_lamports, fail_rate_bps) {
        Some(e) => e,
        None => return SizeBand::refuse(),
    };

    // 2. Upper bound: impact-limited size intersected with sellability.
    let move_i = expected_move_bps as i64;
    let impact_budget = move_i - protocol_bps as i64 - margin_bps as i64;
    if impact_budget < 0 {
        return SizeBand::refuse();
    }
    let x_impact = impact.max_size_at(impact_budget as u32);
    let x_max = x_impact.min(sellable_max_lamports);
    if x_max == 0 {
        return SizeBand::refuse();
    }

    // 3. Cost-minimizing reference size (bottom of the U).
    let x_cost = isqrt((eff as u128).saturating_mul(depth_lamports as u128) / 2) as u64;

    // 4. Lower feasibility crossing, searched only within the admissible upper bound.
    let x_min = match min_viable_size(
        expected_move_bps,
        eff,
        protocol_bps,
        margin_bps,
        impact,
        x_max,
    ) {
        Some(v) => v,
        None => return SizeBand::refuse(),
    };

    if x_min > x_max {
        return SizeBand::refuse();
    }

    SizeBand::admit(x_min, x_cost, x_max)
}

// ---------------------------------------------------------------------------
// Integer helpers
// ---------------------------------------------------------------------------

/// Integer square root of a `u128` (largest `r` with `r*r <= n`). Deterministic, no floats.
fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    // Newton's method with an integer seed derived from the bit length.
    let mut x = 1u128 << ((128 - n.leading_zeros() + 1) / 2);
    loop {
        let next = (x + n / x) / 2;
        if next >= x {
            break;
        }
        x = next;
    }
    // Correct any off-by-one from integer truncation.
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x
}
