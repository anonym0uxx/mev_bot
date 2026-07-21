//! # scalp_position
//!
//! Per-swap, event-driven scalp position management for the pump-quant strategy.
//!
//! Position state is advanced by every decoded swap event through a pure §22 reducer
//! ([`apply_swap`]) — never by a poll loop. All money/percent quantities are integer
//! fixed-point (lamports as `u64`/`u128`, ratios as `bps`/`fp` in `u32`/`i64`). There is
//! **no** `f32`/`f64` in any outcome-controlling path (constitution §22), overflow is always
//! explicit (checked / saturating), and every function here is deterministic: identical
//! inputs and identical event sequences yield identical outputs, with time sourced only from
//! event timestamps.
//!
//! The module bundles the small set of pure decision leaves used by the scalp lane:
//! * state transition ([`apply_swap`])
//! * lane-parametric minimum-hold gate ([`min_hold_blocks_exit`])
//! * expected-landing-state price adjustment ([`expected_landing`])
//! * hazard-model covariate assembly ([`hazard_inputs`])
//! * optimal-stopping redeploy comparator ([`should_exit_on_rate`])
//! * accelerating-favorable-flow no-cut guard with anti-pin void ([`time_stop_binds`])

// ===========================================================================================
// Enumerations
// ===========================================================================================

/// Execution lane. Minimum-hold and other timing parameters are lane-parametric; the scalp
/// lane in particular may carry a near-zero (or exactly zero) minimum hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Fast in/out scalp lane (near-zero minimum hold).
    Scalp,
    /// Longer-horizon swing lane.
    Swing,
    /// Core / conviction lane.
    Core,
}

/// Trade side of a swap or of an intended exit leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Acquiring the base asset (adverse drift/impact pushes the price *up*).
    Buy,
    /// Disposing of the base asset (adverse drift/impact pushes the proceeds price *down*).
    Sell,
}

/// Classification of the exit being evaluated.
///
/// The four hard-risk classes ([`ExitClass::Emergency`], [`ExitClass::SellabilityFailure`],
/// [`ExitClass::RiskLimit`], [`ExitClass::CircuitBreaker`]) bypass the minimum-hold gate and
/// every discretionary no-cut exception. Everything else is [`ExitClass::Normal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitClass {
    /// Ordinary, discretionary exit — subject to min-hold and no-cut exceptions.
    Normal,
    /// Hard emergency exit (e.g. rug / drain detection).
    Emergency,
    /// The position can no longer be sold — must leave immediately.
    SellabilityFailure,
    /// A hard risk limit (exposure / loss) has been breached.
    RiskLimit,
    /// A global circuit breaker has tripped.
    CircuitBreaker,
}

impl ExitClass {
    /// `true` for the hard-risk classes that unconditionally bypass every hold/no-cut guard.
    #[inline]
    pub fn is_emergency(self) -> bool {
        matches!(
            self,
            ExitClass::Emergency
                | ExitClass::SellabilityFailure
                | ExitClass::RiskLimit
                | ExitClass::CircuitBreaker
        )
    }
}

/// Market-structure phase of the token at the time of the position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Still on the bonding curve.
    Curve,
    /// Migrated to an AMM pool.
    Pool,
}

// ===========================================================================================
// Flow window — fixed-size, integer-decayed summary of recent swap flow
// ===========================================================================================

/// Fixed-size, integer-decayed summary of recent swap flow.
///
/// Every field is integer fixed-point and the struct is `Copy` with a fixed footprint — there
/// is **no** per-event `Vec` growth. Decay is applied multiplicatively with an integer
/// `15/16` factor (`x - (x >> 4)`), which can never overflow and is fully deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowState {
    /// Decayed cumulative volume delta (buys positive, sells negative), lamports fp.
    pub cvd_fp: i64,
    /// Decayed buy-side base volume, lamports fp.
    pub buy_volume_fp: u64,
    /// Decayed sell-side base volume, lamports fp.
    pub sell_volume_fp: u64,
    /// Number of swaps folded in over the lifetime of this window (saturating).
    pub arrival_count: u32,
    /// Timestamp (ns) of the most recently folded swap.
    pub last_ts_ns: u64,
    /// Inter-arrival gap (ns) between the last two folded swaps.
    pub last_interval_ns: u64,
    /// Arrival acceleration fp: positive when inter-arrival gaps are shrinking (flow speeding up).
    pub arrival_accel_fp: i64,
    /// Authenticity score in fp (0..=10_000): balance of two-sided flow; one-sided (potentially
    /// fabricated) streams score low.
    pub authenticity_fp: u32,
}

impl FlowState {
    /// A fresh, empty flow window.
    #[inline]
    pub fn new() -> Self {
        FlowState {
            cvd_fp: 0,
            buy_volume_fp: 0,
            sell_volume_fp: 0,
            arrival_count: 0,
            last_ts_ns: 0,
            last_interval_ns: 0,
            arrival_accel_fp: 0,
            authenticity_fp: 0,
        }
    }

    /// Fold one swap into the window: decay the running aggregates, then add the new event.
    ///
    /// Integer-only and overflow-safe (`15/16` decay via shift; saturating adds).
    pub fn push_decayed(&mut self, ev: &SwapEvent) {
        // Decay existing aggregates by a factor of 15/16 (no overflow: `x >> 4 <= x`).
        self.buy_volume_fp -= self.buy_volume_fp >> 4;
        self.sell_volume_fp -= self.sell_volume_fp >> 4;
        self.cvd_fp -= self.cvd_fp / 16; // truncates toward zero; magnitude only shrinks.

        let amt = ev.base_amount_fp;
        match ev.side {
            Side::Buy => {
                self.buy_volume_fp = self.buy_volume_fp.saturating_add(amt);
                self.cvd_fp = self.cvd_fp.saturating_add(amt.min(i64::MAX as u64) as i64);
            }
            Side::Sell => {
                self.sell_volume_fp = self.sell_volume_fp.saturating_add(amt);
                self.cvd_fp = self.cvd_fp.saturating_sub(amt.min(i64::MAX as u64) as i64);
            }
        }

        // Arrival timing / acceleration.
        let interval = ev.ts_ns.saturating_sub(self.last_ts_ns);
        if self.arrival_count > 0 {
            // Positive => gaps shrinking => flow accelerating.
            let prev = self.last_interval_ns.min(i64::MAX as u64) as i64;
            let now = interval.min(i64::MAX as u64) as i64;
            self.arrival_accel_fp = prev.saturating_sub(now);
        }
        self.last_interval_ns = interval;
        self.last_ts_ns = ev.ts_ns;
        self.arrival_count = self.arrival_count.saturating_add(1);

        // Authenticity: two-sided balance in bps (0..=10_000). One-sided flow scores near zero.
        let total = self.buy_volume_fp.saturating_add(self.sell_volume_fp);
        if total > 0 {
            let minv = self.buy_volume_fp.min(self.sell_volume_fp) as u128;
            let bal = (2u128 * minv * 10_000u128) / total as u128;
            self.authenticity_fp = bal.min(10_000) as u32;
        }
    }

    /// Deterministic non-trivial fixture used by property tests.
    pub fn test() -> Self {
        FlowState {
            cvd_fp: 4_200,
            buy_volume_fp: 12_000,
            sell_volume_fp: 8_000,
            arrival_count: 20,
            last_ts_ns: 5_000,
            last_interval_ns: 250,
            arrival_accel_fp: 30,
            authenticity_fp: 8_000,
        }
    }
}

impl Default for FlowState {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================================
// Swap event
// ===========================================================================================

/// One decoded swap event driving the reducer. `Copy`, fixed-size, integer-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapEvent {
    /// Observed execution price, fixed-point.
    pub price_fp: u64,
    /// Event timestamp in nanoseconds (from the event stream, never a syscall clock).
    pub ts_ns: u64,
    /// Trade side.
    pub side: Side,
    /// Base-asset amount of the swap, lamports fp.
    pub base_amount_fp: u64,
    /// Quote-asset amount of the swap, lamports fp.
    pub quote_amount_fp: u64,
}

impl SwapEvent {
    /// Deterministic test constructor: a unit-sized BUY at `(price_fp, ts_ns)`.
    pub fn test(price_fp: u64, ts_ns: u64) -> Self {
        SwapEvent {
            price_fp,
            ts_ns,
            side: Side::Buy,
            base_amount_fp: 1_000,
            quote_amount_fp: price_fp,
        }
    }
}

// ===========================================================================================
// Position state + reducer (leaf: sp_state_apply)
// ===========================================================================================

/// Immutable, `Copy`, fixed-size scalp position state.
///
/// Advanced only through [`apply_swap`]. `peak_price_fp` is monotonically non-decreasing over
/// the entire life of the position and is updated on *every* applied swap (defect #1 guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalpPositionState {
    /// Entry price, fixed-point.
    pub entry_price_fp: u64,
    /// Entry timestamp (ns).
    pub entry_ts_ns: u64,
    /// Most recent observed price, fixed-point.
    pub last_price_fp: u64,
    /// Lifetime peak price, fixed-point — monotone non-decreasing.
    pub peak_price_fp: u64,
    /// Elapsed time in trade (ns) = last event ts − entry ts.
    pub time_in_trade_ns: u64,
    /// Fixed-size decayed flow-window summary.
    pub flow: FlowState,
    /// Behavioural archetype id (part of the hazard cell key).
    pub archetype: u16,
    /// Market-structure phase (part of the hazard cell key).
    pub phase: Phase,
    /// Catalyst class id (part of the hazard cell key).
    pub catalyst: u16,
    /// Regime id (part of the hazard cell key).
    pub regime: u16,
}

impl ScalpPositionState {
    /// Open a new position at `entry_price_fp` / `entry_ts_ns`. Peak and last both start at entry.
    pub fn open(entry_price_fp: u64, entry_ts_ns: u64) -> Self {
        ScalpPositionState {
            entry_price_fp,
            entry_ts_ns,
            last_price_fp: entry_price_fp,
            peak_price_fp: entry_price_fp,
            time_in_trade_ns: 0,
            flow: FlowState::new(),
            archetype: 0,
            phase: Phase::Curve,
            catalyst: 0,
            regime: 0,
        }
    }

    /// Deterministic non-trivial fixture used by property tests.
    pub fn test() -> Self {
        let mut s = ScalpPositionState::open(1_000_000, 0);
        s.last_price_fp = 1_050_000;
        s.peak_price_fp = 1_100_000;
        s.time_in_trade_ns = 5_000;
        s.flow = FlowState::test();
        s.archetype = 7;
        s.phase = Phase::Pool;
        s.catalyst = 3;
        s.regime = 2;
        s
    }
}

/// Pure per-swap position state transition (leaf **sp_state_apply**).
///
/// Applies exactly one decoded swap to `state` and returns the new state. No IO, no clocks, no
/// allocation, no interior mutability. The peak update happens **first and unconditionally**,
/// before any flow-window bookkeeping, so a swap stream longer than any internal buffer can
/// never silently freeze the peak (defect #1 regression).
pub fn apply_swap(state: &ScalpPositionState, ev: &SwapEvent) -> ScalpPositionState {
    let mut s = *state;
    s.last_price_fp = ev.price_fp;
    s.peak_price_fp = s.peak_price_fp.max(ev.price_fp); // FIRST, unconditional.
    s.flow.push_decayed(ev); // fixed-size decayed summary; never early-returns before peak.
    s.time_in_trade_ns = ev.ts_ns.saturating_sub(s.entry_ts_ns);
    s
}

// ===========================================================================================
// Minimum-hold gate (leaf: sp_min_hold)
// ===========================================================================================

/// Lane-parametric minimum-hold check with absolute exemptions (leaf **sp_min_hold**).
///
/// Returns `true` iff a *discretionary* exit is currently blocked because the position has not
/// been held long enough. The four hard-risk exit classes are never blocked. `min_hold_ns`
/// comes from lane config and may legally be zero (no hardcoded hold anywhere).
#[inline]
pub fn min_hold_blocks_exit(
    lane: Lane,
    time_in_trade_ns: u64,
    min_hold_ns: u64,
    exit_class: ExitClass,
) -> bool {
    let _ = lane; // lane-parametric by contract; min_hold_ns already carries the lane's value.
    match exit_class {
        ExitClass::Emergency
        | ExitClass::SellabilityFailure
        | ExitClass::RiskLimit
        | ExitClass::CircuitBreaker => false,
        ExitClass::Normal => time_in_trade_ns < min_hold_ns,
    }
}

// ===========================================================================================
// Expected landing-state price (leaf: sp_landing_eval)
// ===========================================================================================

/// Expected-landing-state price adjustment (leaf **sp_landing_eval**).
///
/// Combines the observed price with the measured adverse latency-drift (`drift_bps_p95`) and
/// the trade's own market impact (`own_impact_bps`), applied against the trade direction:
/// * `Buy`  → `p * (10_000 + total) / 10_000` (landing price ≥ observation),
/// * `Sell` → `p * (10_000 − total) / 10_000` (proceeds price ≤ observation, saturating at ≥ 1).
///
/// All math is integer `mul_div` via `u128`; returns `None` on `u64` overflow.
pub fn expected_landing(
    obs_price_fp: u64,
    side: Side,
    drift_bps_p95: u32,
    own_impact_bps: u32,
) -> Option<u64> {
    let total = (drift_bps_p95 as u128).checked_add(own_impact_bps as u128)?;
    let p = obs_price_fp as u128;
    match side {
        Side::Buy => {
            let factor = 10_000u128.checked_add(total)?;
            let scaled = p.checked_mul(factor)? / 10_000u128;
            if scaled > u64::MAX as u128 {
                None
            } else {
                Some(scaled as u64)
            }
        }
        Side::Sell => {
            // Adverse total reduces proceeds; saturate the multiplier at zero.
            let factor = 10_000u128.saturating_sub(total);
            let mut scaled = p.checked_mul(factor)? / 10_000u128;
            // Saturate proceeds price at >= 1 whenever there is any nonzero price to sell.
            if scaled == 0 && obs_price_fp >= 1 {
                scaled = 1;
            }
            if scaled > u64::MAX as u128 {
                None
            } else {
                Some(scaled as u64)
            }
        }
    }
}

// ===========================================================================================
// Hazard covariate assembly (leaf: sp_hazard_inputs)
// ===========================================================================================

/// The conditioning-cell key for the hazard model.
///
/// Built **only** from `(archetype, phase, catalyst, regime)`. Continuous covariates such as
/// conviction and manipulation history are *never* part of this key (criterion 106) — they
/// enter the model as continuous features instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellKey {
    /// Behavioural archetype id.
    pub archetype: u16,
    /// Market-structure phase.
    pub phase: Phase,
    /// Catalyst class id.
    pub catalyst: u16,
    /// Regime id.
    pub regime: u16,
}

/// The hazard-model covariate vector for the current position (leaf **sp_hazard_inputs**).
///
/// Every field is integer / fixed-point quantized at this boundary. Conviction and
/// manipulation-history are *continuous* covariates (never used to index a conditioning cell).
/// `effective_sample` and `uncertainty_fp` are passthrough fields for the `DecisionRecord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HazardInputs {
    /// Time in trade (ns).
    pub time_in_trade_ns: u64,
    /// Market-structure phase (cell-key field).
    pub phase: Phase,
    /// Catalyst class id (cell-key field).
    pub catalyst: u16,
    /// Regime id (cell-key field).
    pub regime: u16,
    /// Behavioural archetype id (cell-key field).
    pub archetype: u16,
    /// Conviction covariate (continuous, 0..=10_000 fp) — never in the cell key.
    pub conviction_fp: u32,
    /// Manipulation-history covariate (decayed wash/LPI strength, 0..=10_000 fp) — never in the key.
    pub manip_history_fp: u32,
    /// Decayed cumulative-volume-delta slope, lamports fp.
    pub cvd_slope_fp: i64,
    /// Arrival-acceleration covariate fp.
    pub arrival_accel_fp: i64,
    /// Flow authenticity covariate (0..=10_000 fp).
    pub authenticity_fp: u32,
    /// Effective sample count backing the flow summary (passthrough for the DecisionRecord).
    pub effective_sample: u32,
    /// Uncertainty passthrough fp (higher = less certain), for the DecisionRecord.
    pub uncertainty_fp: u32,
}

impl HazardInputs {
    /// The conditioning-cell key — derived from structural fields only, never the covariates.
    #[inline]
    pub fn cell_key(&self) -> CellKey {
        CellKey {
            archetype: self.archetype,
            phase: self.phase,
            catalyst: self.catalyst,
            regime: self.regime,
        }
    }
}

/// Assemble the hazard covariate vector for the current position (leaf **sp_hazard_inputs**).
///
/// Only causally-available, integer/fixed-point quantized inputs are used. Conviction and
/// manipulation-history are passed straight through as continuous covariates (clamped to the
/// `0..=10_000` fp band) and are deliberately kept out of the cell key.
pub fn hazard_inputs(
    s: &ScalpPositionState,
    flow: &FlowState,
    conviction_score_fp: u32,
    manip_history_fp: u32,
) -> HazardInputs {
    // Uncertainty shrinks with more samples: fewer arrivals => higher uncertainty. Integer only.
    let uncertainty_fp = 10_000u32.saturating_sub(flow.arrival_count.saturating_mul(100).min(10_000));
    HazardInputs {
        time_in_trade_ns: s.time_in_trade_ns,
        phase: s.phase,
        catalyst: s.catalyst,
        regime: s.regime,
        archetype: s.archetype,
        conviction_fp: conviction_score_fp.min(10_000),
        manip_history_fp: manip_history_fp.min(10_000),
        cvd_slope_fp: flow.cvd_fp,
        arrival_accel_fp: flow.arrival_accel_fp,
        authenticity_fp: flow.authenticity_fp.min(10_000),
        effective_sample: flow.arrival_count,
        uncertainty_fp,
    }
}

// ===========================================================================================
// Optimal-stopping redeploy comparator (leaf: sp_redeploy_stop)
// ===========================================================================================

/// Optimal-stopping comparator against redeployment value (leaf **sp_redeploy_stop**).
///
/// Hold only while the marginal hold rate exceeds the redeployment rate net of switching
/// costs; otherwise exit. When inputs are not fresh (stale arrival-rate / productivity
/// estimates) the decision falls back to the fixed-constant `baseline_says_exit` — never a
/// guess. `redeploy_rate_fp` is zero when no qualified candidate exists (caller contract).
/// All rates are signed lamports-per-second fixed-point.
#[inline]
pub fn should_exit_on_rate(
    hold_rate_fp: i64,
    redeploy_rate_fp: i64,
    switch_cost_rate_fp: i64,
    inputs_fresh: bool,
    baseline_says_exit: bool,
) -> bool {
    if !inputs_fresh {
        return baseline_says_exit;
    }
    hold_rate_fp < redeploy_rate_fp.saturating_sub(switch_cost_rate_fp)
}

// ===========================================================================================
// No-cut guard / anti-pin (leaf: sp_no_cut_guard)
// ===========================================================================================

/// Accelerating-favorable-flow no-cut exception with the anti-pin fabrication void
/// (leaf **sp_no_cut_guard**).
///
/// Returns `true` when the time-stop *binds* (the position must exit on the clock).
/// * Emergency-class exits always bind, regardless of every other argument.
/// * Otherwise, if the elapsed condition is not met, the stop does not bind.
/// * The no-cut exception (fresh, accelerating, favorable flow) suppresses the stop **only**
///   when authenticity is at or above the fabrication threshold. Below threshold the exception
///   is void and the stop binds normally (anti-pin). Stale flow never suppresses the stop.
#[inline]
pub fn time_stop_binds(
    elapsed_ok: bool,
    flow_accel_favorable: bool,
    flow_fresh: bool,
    authenticity_fp: u32,
    fabrication_threshold_fp: u32,
    exit_class: ExitClass,
) -> bool {
    if exit_class.is_emergency() {
        return true;
    }
    if !elapsed_ok {
        return false;
    }
    let exception =
        flow_accel_favorable && flow_fresh && authenticity_fp >= fabrication_threshold_fp;
    !exception
}
