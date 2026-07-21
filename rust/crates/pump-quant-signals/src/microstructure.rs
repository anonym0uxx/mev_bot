//! AMM order-flow / microstructure feature catalog (constitution §21.7, criterion 95).
//!
//! Memecoin venues are constant-product AMMs with **no central limit order
//! book**, so classical LOB microstructure does not transfer. What transfers is
//! computed from decoded swap flow and reserve state. This module provides the
//! deterministic, pure-function `TimedFeature` computations over
//! entity-deduplicated swap flow (Section 28 dedup is the caller's
//! responsibility; every input swap carries an `entity_id`):
//!
//! - CVD (cumulative volume delta) + delta velocity and CVD-vs-price divergence
//! - Order-flow imbalance (OFI) over rolling windows, breadth-decomposed into
//!   net-new-buyer vs repeat/bot cohorts
//! - Trade-size distribution + large-print detection
//! - AMM absorption / exhaustion (quote inflow vs price response)
//! - Anchored-VWAP location + reclaim/rejection states
//! - Reserve-depth dynamics + executable constant-product price-impact curve
//! - Swap-arrival intensity + burst onset/climax/exhaustion signatures
//!
//! # Constitution constraints (§22)
//!
//! Pure, stateless, deterministic and integer-only. Quote volume is in lamports
//! (`u64`), prices are fixed-point integers, rates are basis points (bps).
//! Signed accumulation uses `i128`; multiplications widen to `u128`/`i128`;
//! results saturate explicitly. This module is a *feature catalog*: per §21.7
//! "none authorizes alone" — admission is the supervisor's economic gate, not
//! any single feature here.
//!
//! Note: live swap/reserve I/O is OUT OF SCOPE; callers feed already-decoded,
//! entity-deduplicated fixtures. Nothing here performs I/O.

/// Direction of an AMM swap, from the taker's perspective.
///
/// Responsibility: encode aggressor side so buy-side and sell-side quote
/// volume can be netted. Constitution §21.7 (order-flow intent proxy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDir {
    /// Quote (SOL) in, base (token) out — buy-side aggression.
    Buy,
    /// Base (token) in, quote (SOL) out — sell-side aggression.
    Sell,
}

/// A single decoded, entity-deduplicated swap.
///
/// Responsibility: the atomic unit every §21.7 feature is computed over.
/// `entity_id` is the Section 28 deduplicated cluster id (NOT the raw wallet);
/// `is_new_buyer` marks Section 28 breadth-decomposed net-new-buyer flow.
/// Constitution §22: all quantities integer / fixed-point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swap {
    /// Swap landing time in milliseconds (monotonic within a sequence).
    pub ts_ms: u64,
    /// Aggressor side.
    pub dir: SwapDir,
    /// Quote (SOL) notional moved, in lamports.
    pub quote_lamports: u64,
    /// Executed price in fixed-point units (quote-per-base).
    pub price_fp: u64,
    /// Section 28 entity-deduplicated cluster id (adversary-resistant identity).
    pub entity_id: u64,
    /// `true` iff this swap's entity is a net-new buyer (breadth), not a
    /// repeat/bot participant.
    pub is_new_buyer: bool,
}

// -- CVD ----------------------------------------------------------------------

/// Cumulative volume delta: running net of buy-side minus sell-side quote
/// volume, in lamports. The primary order-flow-intent proxy (§21.7).
///
/// Buys add `+quote_lamports`, sells subtract. `i128` accumulation cannot
/// overflow for any realistic swap count.
///
/// Responsibility: net directional quote pressure over a swap sequence.
/// Constitution §22: signed integer, `i128` accumulator.
#[inline]
pub fn cumulative_volume_delta(swaps: &[Swap]) -> i128 {
    let mut cvd: i128 = 0;
    for s in swaps {
        match s.dir {
            SwapDir::Buy => cvd += s.quote_lamports as i128,
            SwapDir::Sell => cvd -= s.quote_lamports as i128,
        }
    }
    cvd
}

/// CVD delta velocity in lamports per second over an explicit window.
///
/// `velocity = cvd_delta * 1_000 / dt_ms`. Returns `0` when `dt_ms == 0`.
/// Positive = accelerating net buying, negative = accelerating net selling.
///
/// Responsibility: rate of change of order-flow intent (§21.7 delta velocity).
/// Constitution §22: integer, `i128` intermediates, `dt_ms == 0` guard.
#[inline]
pub fn cvd_velocity_lamports_per_s(cvd_delta: i128, dt_ms: u64) -> i128 {
    if dt_ms == 0 {
        return 0;
    }
    cvd_delta * 1_000 / dt_ms as i128
}

/// CVD-vs-price divergence classification over a window (§21.7 exhaustion /
/// breakout-confirmation hypothesis).
///
/// Compares the sign of the price change against the sign of the CVD change:
///
/// - price higher, CVD failing to confirm (flat/down) => [`Divergence::BearishExhaustion`]
/// - price lower, CVD rising => [`Divergence::BullishExhaustion`]
/// - price and CVD both up => [`Divergence::BullishConfirm`]
/// - price and CVD both down => [`Divergence::BearishConfirm`]
/// - otherwise (either side flat) => [`Divergence::Neutral`]
///
/// Responsibility: detect order-flow/price disagreement as a
/// reversal/confirmation feature. Constitution §22: integer sign comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Divergence {
    /// Price up and CVD up: momentum confirmed.
    BullishConfirm,
    /// Price down and CVD down: decline confirmed.
    BearishConfirm,
    /// Price up but CVD failed to confirm: buy-pressure exhaustion.
    BearishExhaustion,
    /// Price down but CVD rising: sell-pressure exhaustion.
    BullishExhaustion,
    /// Either side flat: no divergence signal.
    Neutral,
}

/// Classify CVD-vs-price divergence from window endpoints.
///
/// Responsibility: pure sign-comparison of price vs CVD change (§21.7).
/// Constitution §22: integer arithmetic only.
#[inline]
pub fn cvd_price_divergence(
    price_start_fp: u64,
    price_end_fp: u64,
    cvd_start: i128,
    cvd_end: i128,
) -> Divergence {
    let price_delta = price_end_fp as i128 - price_start_fp as i128;
    let cvd_delta = cvd_end - cvd_start;
    let ps = price_delta.signum();
    let cs = cvd_delta.signum();
    match (ps, cs) {
        (1, 1) => Divergence::BullishConfirm,
        (-1, -1) => Divergence::BearishConfirm,
        (1, c) if c <= 0 => Divergence::BearishExhaustion,
        (-1, c) if c >= 0 => Divergence::BullishExhaustion,
        _ => Divergence::Neutral,
    }
}

// -- OFI ----------------------------------------------------------------------

/// Breadth-decomposed order-flow imbalance (§21.7). All figures are signed
/// basis points in `-10_000..=10_000`, computed as
/// `(buy_quote - sell_quote) * 10_000 / (buy_quote + sell_quote)`.
///
/// Responsibility: aggressor-side skew, separated (Section 28 breadth) into
/// net-new-buyer flow vs repeat/bot flow so manufactured repeat flow does not
/// masquerade as genuine breadth. Constitution §22: integer bps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OfiBreakdown {
    /// OFI over all flow.
    pub aggregate_bps: i32,
    /// OFI restricted to net-new-buyer entities (genuine breadth proxy).
    pub new_buyer_bps: i32,
    /// OFI over repeat/bot entities.
    pub repeat_bps: i32,
}

#[inline]
fn ofi_bps(buy: u128, sell: u128) -> i32 {
    let gross = buy + sell;
    if gross == 0 {
        return 0;
    }
    let net = buy as i128 - sell as i128;
    (net * 10_000 / gross as i128) as i32
}

/// Compute breadth-decomposed OFI over a swap window.
///
/// Responsibility: aggressor skew + Section 28 breadth decomposition (§21.7).
/// Constitution §22: `u128` accumulation, integer bps normalization.
#[inline]
pub fn order_flow_imbalance(swaps: &[Swap]) -> OfiBreakdown {
    let (mut agg_b, mut agg_s) = (0u128, 0u128);
    let (mut nb_b, mut nb_s) = (0u128, 0u128);
    let (mut rp_b, mut rp_s) = (0u128, 0u128);
    for s in swaps {
        let q = s.quote_lamports as u128;
        match s.dir {
            SwapDir::Buy => agg_b += q,
            SwapDir::Sell => agg_s += q,
        }
        if s.is_new_buyer {
            match s.dir {
                SwapDir::Buy => nb_b += q,
                SwapDir::Sell => nb_s += q,
            }
        } else {
            match s.dir {
                SwapDir::Buy => rp_b += q,
                SwapDir::Sell => rp_s += q,
            }
        }
    }
    OfiBreakdown {
        aggregate_bps: ofi_bps(agg_b, agg_s),
        new_buyer_bps: ofi_bps(nb_b, nb_s),
        repeat_bps: ofi_bps(rp_b, rp_s),
    }
}

// -- Trade-size distribution --------------------------------------------------

/// Number of size buckets in [`SizeDistribution`]. Bucket `i` counts swaps
/// whose quote notional falls in `[BUCKET_EDGES[i], BUCKET_EDGES[i+1])`, with
/// the final bucket unbounded above.
pub const SIZE_BUCKETS: usize = 6;

/// Inclusive lower edges (lamports) of the size-distribution buckets.
/// Edges: 0, 0.1, 0.5, 1, 5, 10 SOL.
pub const BUCKET_EDGES: [u64; SIZE_BUCKETS] = [
    0,
    100_000_000,
    500_000_000,
    1_000_000_000,
    5_000_000_000,
    10_000_000_000,
];

/// Trade-size distribution + large-print detection (§21.7).
///
/// Responsibility: histogram shape (retail vs concentrated flow), the integer
/// median print size, and large-print (whale) arrival count. Distribution
/// shifts are the accumulation/distribution signal, not raw volume.
/// Constitution §22: integer bucketing and median.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SizeDistribution {
    /// Per-bucket swap counts (see [`BUCKET_EDGES`]).
    pub buckets: [u32; SIZE_BUCKETS],
    /// Integer median quote notional (lamports); even counts average the two
    /// central order statistics via `(a + b) / 2`.
    pub median_lamports: u64,
    /// Count of swaps whose notional is `>= large_print_multiple * median`.
    pub large_prints: u32,
    /// Total swaps considered.
    pub count: u32,
}

/// Compute the trade-size distribution and large-print count.
///
/// A "large print" is any swap at least `large_print_multiple` times the
/// median print size (whale-print arrival). Empty input yields all-zero.
///
/// Responsibility: distributional microstructure of prints (§21.7).
/// Constitution §22: sorts a local copy; integer median and multiple test.
#[inline]
pub fn trade_size_distribution(swaps: &[Swap], large_print_multiple: u64) -> SizeDistribution {
    let mut out = SizeDistribution::default();
    if swaps.is_empty() {
        return out;
    }
    let mut sizes: Vec<u64> = swaps.iter().map(|s| s.quote_lamports).collect();
    for &sz in &sizes {
        let mut idx = 0usize;
        for (b, &edge) in BUCKET_EDGES.iter().enumerate() {
            if sz >= edge {
                idx = b;
            }
        }
        out.buckets[idx] += 1;
    }
    sizes.sort_unstable();
    let n = sizes.len();
    out.median_lamports = if n % 2 == 1 {
        sizes[n / 2]
    } else {
        // Average of the two central order statistics (widen to avoid overflow).
        ((sizes[n / 2 - 1] as u128 + sizes[n / 2] as u128) / 2) as u64
    };
    let threshold = (out.median_lamports as u128).saturating_mul(large_print_multiple as u128);
    out.large_prints = sizes
        .iter()
        .filter(|&&sz| sz as u128 >= threshold && threshold > 0)
        .count() as u32;
    out.count = n as u32;
    out
}

// -- Absorption / exhaustion --------------------------------------------------

/// AMM-adapted absorption / exhaustion classification (§21.7).
///
/// Responsibility: distinguish reserve-buffered absorption (large quote inflow,
/// little price response ≈ accumulation) from one-sided aggression stalling
/// near a level (exhaustion). Constitution §22: integer bps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowResponse {
    /// Large net buy inflow but price barely moved up: reserve-buffered
    /// absorption (accumulation hypothesis).
    Absorption,
    /// Large net buy inflow but price flat/falling: buy-side exhaustion.
    Exhaustion,
    /// Nothing notable (inflow below threshold, or a normal impact response).
    Normal,
}

/// Classify absorption vs exhaustion from net buy inflow and the price response.
///
/// - inflow `< min_notable_inflow_lamports` => [`FlowResponse::Normal`]
/// - notable inflow, `0 <= price_change_bps <= low_impact_bps` => [`FlowResponse::Absorption`]
/// - notable inflow, `price_change_bps < 0` => [`FlowResponse::Exhaustion`]
/// - otherwise (notable inflow with a real up-move) => [`FlowResponse::Normal`]
///
/// Responsibility: quote-inflow-vs-price-response feature (§21.7).
/// Constitution §22: integer bps computed by [`price_change_bps`].
#[inline]
pub fn absorption_exhaustion(
    net_buy_quote_lamports: u128,
    price_start_fp: u64,
    price_end_fp: u64,
    min_notable_inflow_lamports: u128,
    low_impact_bps: i64,
) -> FlowResponse {
    if net_buy_quote_lamports < min_notable_inflow_lamports {
        return FlowResponse::Normal;
    }
    let change = price_change_bps(price_start_fp, price_end_fp);
    if change < 0 {
        FlowResponse::Exhaustion
    } else if change <= low_impact_bps {
        FlowResponse::Absorption
    } else {
        FlowResponse::Normal
    }
}

/// Signed price change in basis points: `(end - start) * 10_000 / start`.
/// Returns `0` when `start == 0`.
///
/// Responsibility: shared fixed-point price-change helper (§21.7).
/// Constitution §22: `i128` intermediates, saturating into `i64`.
#[inline]
pub fn price_change_bps(price_start_fp: u64, price_end_fp: u64) -> i64 {
    if price_start_fp == 0 {
        return 0;
    }
    let delta = price_end_fp as i128 - price_start_fp as i128;
    let v = delta * 10_000 / price_start_fp as i128;
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

// -- Anchored VWAP ------------------------------------------------------------

/// Anchored volume-weighted average price over a swap slice, in fixed-point.
///
/// `vwap = sum(price_fp * quote) / sum(quote)`, anchored simply by which slice
/// the caller passes (launch/migration/session anchoring is a slice choice).
/// Returns `0` when total quote volume is zero.
///
/// Responsibility: mean-reversion location reference (§21.7 anchored VWAP).
/// Constitution §22: `u128` numerator accumulation, integer division.
#[inline]
pub fn anchored_vwap_fp(swaps: &[Swap]) -> u64 {
    let mut num: u128 = 0;
    let mut den: u128 = 0;
    for s in swaps {
        num += s.price_fp as u128 * s.quote_lamports as u128;
        den += s.quote_lamports as u128;
    }
    if den == 0 {
        return 0;
    }
    (num / den).min(u64::MAX as u128) as u64
}

/// VWAP reclaim/rejection state (§21.7). Used only with CVD as intent
/// confirmation by the supervisor — this function reports location only.
///
/// Responsibility: transition of price across the anchored VWAP.
/// Constitution §22: integer comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VwapState {
    /// Crossed from at/below VWAP to above: reclaim.
    ReclaimAbove,
    /// Crossed from at/above VWAP to below: rejection.
    RejectBelow,
    /// Stayed above VWAP.
    HoldAbove,
    /// Stayed at or below VWAP.
    HoldBelow,
}

/// Classify the VWAP location transition from previous/current price.
///
/// Responsibility: reclaim/rejection/hold state around anchored VWAP (§21.7).
/// Constitution §22: pure integer comparison.
#[inline]
pub fn vwap_state(prev_price_fp: u64, cur_price_fp: u64, vwap_fp: u64) -> VwapState {
    let prev_above = prev_price_fp > vwap_fp;
    let cur_above = cur_price_fp > vwap_fp;
    match (prev_above, cur_above) {
        (false, true) => VwapState::ReclaimAbove,
        (true, false) => VwapState::RejectBelow,
        (true, true) => VwapState::HoldAbove,
        (false, false) => VwapState::HoldBelow,
    }
}

// -- Reserve depth + executable price impact ----------------------------------

/// Base tokens received for a constant-product (`x*y=k`) buy of `quote_in`
/// lamports, given current reserves. No fee is modelled here (callers apply the
/// decoded venue fee separately).
///
/// `base_out = base_reserve - k / (quote_reserve + quote_in)` where
/// `k = base_reserve * quote_reserve`. Returns `0` on degenerate reserves.
///
/// Responsibility: executable fill size at current depth (§21.7 / §55 capacity).
/// Constitution §22: `u128` product for `k`, integer division.
#[inline]
pub fn constant_product_base_out(base_reserve: u128, quote_reserve: u128, quote_in: u128) -> u128 {
    if base_reserve == 0 || quote_reserve == 0 || quote_in == 0 {
        return 0;
    }
    let k = base_reserve * quote_reserve;
    let new_quote = quote_reserve + quote_in;
    let new_base = k / new_quote;
    base_reserve.saturating_sub(new_base)
}

/// Executable price-impact in basis points for a constant-product buy.
///
/// Spot price `= quote_reserve * SCALE / base_reserve`; effective fill price
/// `= quote_in * SCALE / base_out`. Impact `= (effective - spot) * 10_000 /
/// spot`. `SCALE` (fixed-point) cancels in the ratio, so it is applied
/// consistently to both. Returns `0` when the fill is empty.
///
/// Responsibility: size-conditioned impact function determining fillable size
/// (§21.7 executable price-impact curve). Constitution §22: `u128` throughout.
#[inline]
pub fn price_impact_bps(base_reserve: u128, quote_reserve: u128, quote_in: u128) -> u64 {
    let base_out = constant_product_base_out(base_reserve, quote_reserve, quote_in);
    if base_out == 0 || base_reserve == 0 {
        return 0;
    }
    // spot = quote/base ; effective = quote_in/base_out.
    // impact_bps = (effective/spot - 1) * 10_000
    //            = (quote_in * base_reserve - quote_reserve * base_out)
    //              * 10_000 / (quote_reserve * base_out)
    let effective_num = quote_in * base_reserve; // effective/spot numerator part
    let spot_num = quote_reserve * base_out;
    if effective_num <= spot_num {
        return 0;
    }
    ((effective_num - spot_num) * 10_000 / spot_num).min(u64::MAX as u128) as u64
}

/// Reserve-depth (liquidity) velocity in lamports per second: signed change in
/// quote reserve over `dt_ms`. Positive = liquidity add, negative = removal.
///
/// Returns `0` when `dt_ms == 0`.
///
/// Responsibility: liquidity-add/-remove velocity (§21.7 reserve-depth
/// dynamics). Constitution §22: `i128` intermediates, `dt_ms == 0` guard.
#[inline]
pub fn reserve_velocity_lamports_per_s(
    quote_reserve_start: u128,
    quote_reserve_end: u128,
    dt_ms: u64,
) -> i128 {
    if dt_ms == 0 {
        return 0;
    }
    let delta = quote_reserve_end as i128 - quote_reserve_start as i128;
    delta * 1_000 / dt_ms as i128
}

// -- Swap-arrival intensity + burst -------------------------------------------

/// Swap-arrival intensity in swaps per 1000 seconds (milli-Hz), an integer
/// rate: `count * 1_000_000 / window_ms`. Returns `0` when `window_ms == 0`.
///
/// Responsibility: per-window arrival-rate estimate (§21.7 swap-arrival
/// intensity). Constitution §22: integer rate, no float, `window_ms == 0` guard.
#[inline]
pub fn arrival_rate_millihz(count: u32, window_ms: u64) -> u64 {
    if window_ms == 0 {
        return 0;
    }
    (count as u128 * 1_000_000 / window_ms as u128).min(u64::MAX as u128) as u64
}

/// Self-exciting burst phase (§21.7). Compares a recent-window arrival rate
/// against the immediately prior window and a longer baseline (all in the same
/// milli-Hz unit from [`arrival_rate_millihz`]).
///
/// Responsibility: burst onset/climax/exhaustion signature — the microstructure
/// of "candles that peak within seconds." Constitution §22: integer comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstPhase {
    /// Recent rate at or below baseline: no burst.
    Quiet,
    /// Rate elevated above baseline and still accelerating: burst onset.
    Onset,
    /// Rate strongly elevated but acceleration has stalled/plateaued: climax.
    Climax,
    /// Rate was elevated and is now decelerating: exhaustion.
    Exhaustion,
}

/// Classify the burst phase from recent, prior, and baseline arrival rates.
///
/// `elevated` means `recent >= baseline * elevation_multiple`. Given elevation:
/// accelerating (`recent > prior`) => [`BurstPhase::Onset`]; decelerating
/// (`recent < prior`) => [`BurstPhase::Exhaustion`]; flat (`recent == prior`)
/// => [`BurstPhase::Climax`]. Not elevated (`recent <= baseline`) =>
/// [`BurstPhase::Quiet`]; elevated-but-below-multiple with deceleration is
/// [`BurstPhase::Exhaustion`], otherwise [`BurstPhase::Onset`].
///
/// Responsibility: deterministic burst-state machine over rates (§21.7).
/// Constitution §22: pure integer comparisons.
#[inline]
pub fn burst_phase(
    recent_millihz: u64,
    prior_millihz: u64,
    baseline_millihz: u64,
    elevation_multiple: u64,
) -> BurstPhase {
    if recent_millihz <= baseline_millihz {
        return BurstPhase::Quiet;
    }
    let strongly_elevated =
        recent_millihz >= baseline_millihz.saturating_mul(elevation_multiple.max(1));
    if strongly_elevated {
        match recent_millihz.cmp(&prior_millihz) {
            std::cmp::Ordering::Greater => BurstPhase::Onset,
            std::cmp::Ordering::Equal => BurstPhase::Climax,
            std::cmp::Ordering::Less => BurstPhase::Exhaustion,
        }
    } else if recent_millihz < prior_millihz {
        BurstPhase::Exhaustion
    } else {
        BurstPhase::Onset
    }
}
