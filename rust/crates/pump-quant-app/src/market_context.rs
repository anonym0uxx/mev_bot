//! # market_context — regime, breadth, phase, and exit-cost context (§21.3/§28/§21.7)
//!
//! The engine-facing fold that keeps three market-state views current from the
//! decoded event stream, each wired verbatim from its owning crate:
//!
//! * **Market regime** (§21.3) — [`pump_quant_market_state::regime`]'s
//!   multi-dimensional, never-composited regime state. This module folds
//!   launches / graduations / buys / sells / rugs into the
//!   [`MarketRegimeReducer`] and exposes a compact [`RegimeSummary`] at
//!   reflection cadence (the full state stays inspectable upstream; the
//!   summary is a *view*, not a composite score).
//! * **Cluster-adjusted breadth** (§28/§21.7) — per-mint
//!   [`pump_quant_market_state::breadth`] reducers. "Raw wallet count is not
//!   organic breadth" (§28): the corroboration input exposed here is
//!   `genuine_net_exposure_breadth` — positive-net-inventory,
//!   meaningful-net-SOL, unflagged clusters — never the raw bitset count.
//! * **Phase + executable exit cost** (§21.7 phase asymmetry) — tracks
//!   curve→pool migration per mint and prices the *phase-correct* executable
//!   exit cost through `pump_quant_strategy::exit_cost_model`, which
//!   mechanically forbids applying either phase's model to the other.
//!
//! ## Determinism & bounds
//! Integer-only, no wall-clock, no RNG (§22): time is the event ordinal.
//! Per-event folds are O(1)/O(log n) amortized; the per-mint table is bounded
//! at [`MAX_TRACKED_MINTS`] with deterministic oldest-activity eviction via a
//! `(last_touch, mint)` index (§99). Regime scalars with no feed (SOL shock,
//! fees, congestion) stay `None` and classify as UNKNOWN (§6.4), never as a
//! fabricated default.

use std::collections::{BTreeMap, BTreeSet};

use pump_quant_market_state::breadth::{
    BreadthConfig, BreadthReducer, BuyerFlags, FlowEvent, Side,
};
use pump_quant_market_state::regime::{
    MarketEvent, MarketRegimeReducer, RegimeLevel, RegimeThresholds, Skew,
};
use pump_quant_strategy::exit_cost_model::{
    executable_exit_cost_bps, CurveExitInputs, PoolExitInputs,
};
use pump_quant_strategy::scalp_position::Phase;

// ===========================================================================================
// Named constants (§102 — every threshold carries its provenance)
// ===========================================================================================

/// Maximum mints with live breadth/phase context (§99). 1024 covers the full
/// ranked watchlist plus warm history by an order of magnitude; older mints
/// yield to newer flow (oldest-activity eviction, deterministic).
pub const MAX_TRACKED_MINTS: usize = 1024;

/// Latency-window adverse drift (bps) charged on every exit, both phases:
/// the calibrated price slip across this system's observe→land window
/// (~2–3 slots) on a moving memecoin tape. Recalibrated server-side; 50 bps
/// is the conservative laptop prior (§18 cost honesty).
pub const EXIT_LATENCY_DRIFT_BPS: u32 = 50;

/// Curve-phase measured failure rate (bps): ~5% of curve exits fail to land
/// on the first attempt (congestion / curve-state races) and still pay cost —
/// the model inflates by the attempt multiplier (§715(b) landing-state
/// evaluation).
pub const CURVE_FAILURE_RATE_BPS: u32 = 500;

/// Curve-phase retry adder (bps): the extra adverse move paid by the retry
/// attempt after a failed exit (§715(b)).
pub const CURVE_RETRY_ADDER_BPS: u32 = 100;

/// Minimum cluster net-SOL exposure (lamports) to count as meaningful breadth
/// (0.01 SOL): below this a "buyer" is dust that cannot carry exit liquidity
/// (§28 meaningful-net-SOL-exposure buyers; mirrors the breadth crate's v0
/// default).
pub const BREADTH_MEANINGFUL_NET_QUOTE_LAMPORTS: u64 = 10_000_000;

/// Trailing window (events) for the breadth-decay measure — 64 swaps spans
/// the arrival cohort of one scalp horizon (§28 independent-buyer-expansion
/// decay; mirrors the breadth crate's v0 default).
pub const BREADTH_DECAY_WINDOW_EVENTS: u64 = 64;

/// Per-mint cluster capacity for the breadth reducer. 512 clusters dwarfs any
/// organic early tape; bounding below the crate's 4096 default keeps the
/// worst case across [`MAX_TRACKED_MINTS`] mints tens of MiB, not hundreds
/// (§99 memory law).
pub const BREADTH_MAX_CLUSTERS: usize = 512;

/// Per-mint distinct-id capacity per raw-uniqueness dimension (same §99
/// sizing rationale as [`BREADTH_MAX_CLUSTERS`]).
pub const BREADTH_MAX_IDS: usize = 512;

// ===========================================================================================
// Types
// ===========================================================================================

/// Compact regime view for the reflection cadence, derived from
/// [`pump_quant_market_state::regime::classify`] with `Default` thresholds.
///
/// Three independent booleans — deliberately NOT a score (§21.3 no-composite
/// rule): `frothy` (launch velocity at `High`), `imbalance_up` (market-wide
/// buy skew `Up`/`StrongUp`), `rug_elevated` (rug/collapse rate at
/// `Elevated` or worse). Missing dimensions read as `false` here because the
/// summary's consumers treat `true` as an *evidence* flag; UNKNOWN is never
/// promoted to evidence (§6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegimeSummary {
    /// Market-wide launch velocity is at [`RegimeLevel::High`].
    pub frothy: bool,
    /// Market-wide buy/sell imbalance skews up ([`Skew::Up`]/[`Skew::StrongUp`]).
    pub imbalance_up: bool,
    /// Rug/collapse rate is at [`RegimeLevel::Elevated`] or worse.
    pub rug_elevated: bool,
}

/// Per-mint context: breadth reducer, phase, and latest decoded liquidity.
#[derive(Clone, Debug)]
struct MintCtx {
    /// §28 cluster-adjusted breadth reducer for this mint.
    breadth: BreadthReducer,
    /// `true` once migrated to a pool (§21.7 phase asymmetry).
    is_pool: bool,
    /// Latest decoded reserve (lamports): curve reserve pre-migration, pool
    /// reserve proxy post-migration. Zero = unknown → no exit-cost estimate.
    liquidity_lamports: u64,
    /// Per-mint monotone event index fed to the breadth reducer (caller time,
    /// §22 — a quiet mint's decay window never advances on others' flow).
    seq: u64,
    /// Global touch ordinal (recency key for deterministic eviction).
    last_touch: u64,
}

impl MintCtx {
    /// Fresh context with the bounded breadth configuration.
    fn new(last_touch: u64) -> Self {
        MintCtx {
            breadth: BreadthReducer::new(BreadthConfig {
                meaningful_net_quote_lamports: BREADTH_MEANINGFUL_NET_QUOTE_LAMPORTS,
                decay_window_events: BREADTH_DECAY_WINDOW_EVENTS,
                max_tracked_clusters: BREADTH_MAX_CLUSTERS,
                max_tracked_ids: BREADTH_MAX_IDS,
            }),
            is_pool: false,
            liquidity_lamports: 0,
            seq: 0,
            last_touch,
        }
    }
}

/// Engine-facing market context: §21.3 regime + §28 per-mint breadth + §21.7
/// phase and executable exit cost.
///
/// All folds are per-event O(1)/O(log n); all queries are read-only.
/// Deterministic (§22): identical event sequences produce identical
/// summaries, breadth counts, and exit costs.
#[derive(Clone, Debug, Default)]
pub struct MarketContext {
    /// §21.3 market-wide regime reducer.
    regime: MarketRegimeReducer,
    /// Bounded per-mint contexts, keyed by mint bytes.
    mints: BTreeMap<[u8; 32], MintCtx>,
    /// Oldest-activity eviction index: `(last_touch, mint)` in lockstep with
    /// `mints` so eviction is `O(log n)`, never a scan.
    by_touch: BTreeSet<(u64, [u8; 32])>,
    /// Launches observed (live-market denominator input, saturating).
    launches_seen: u64,
    /// Global monotone touch ordinal (event-count time, §22 — no wall clock).
    ordinal: u64,
}

impl MarketContext {
    /// New empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a market-wide launch (regime `Launch`; live-market denominator).
    pub fn on_launch(&mut self) {
        self.regime.ingest(&MarketEvent::Launch);
        self.launches_seen = self.launches_seen.saturating_add(1);
        self.refresh_live_markets();
    }

    /// Fold a curve→pool migration for `mint`: marks the phase pool (§21.7)
    /// and counts a regime `Graduation` (§21.3).
    pub fn on_migration(&mut self, mint: &[u8; 32]) {
        self.regime.ingest(&MarketEvent::Graduation);
        self.touch(mint).is_pool = true;
        self.refresh_live_markets();
    }

    /// Fold one decoded trade: regime `Buy`/`Sell` (§21.3) plus the per-mint
    /// breadth reducer (§28) and the latest decoded liquidity (the reserve
    /// the §21.7 exit-cost model prices against).
    ///
    /// `entity` is the upstream entity-deduplicated actor id; at v0 it stands
    /// in for every id dimension (wallet, token account, fee payer, funding
    /// root, cluster) because upstream resolution already collapses one real
    /// actor to one id. `funded_net_new` is `false` and flags are empty until
    /// the funding-graph / Tier-1 flag feeds are wired — so flag-derived
    /// counts are conservative lower bounds, and `genuine_net_exposure_breadth`
    /// is an upper bound pending flags (§6.4 labeling, not fabrication).
    pub fn on_trade(
        &mut self,
        mint: &[u8; 32],
        entity: u64,
        is_buy: bool,
        quote_lamports: u64,
        base_units: u64,
        liquidity_lamports: u64,
    ) {
        self.regime.ingest(if is_buy {
            &MarketEvent::Buy
        } else {
            &MarketEvent::Sell
        });
        let ctx = self.touch(mint);
        ctx.liquidity_lamports = liquidity_lamports;
        ctx.seq = ctx.seq.saturating_add(1);
        let ev = FlowEvent {
            event_index: ctx.seq,
            side: if is_buy { Side::Buy } else { Side::Sell },
            wallet: entity,
            token_account: entity,
            fee_payer: entity,
            funding_root: entity,
            cluster: entity,
            quote_lamports,
            token_base_units: base_units,
            funded_net_new: false,
            flags: BuyerFlags::empty(),
        };
        ctx.breadth.ingest(&ev);
        self.refresh_live_markets();
    }

    /// Fold a rug-precursor / collapse terminal event (regime `Rug`, §21.3).
    pub fn on_rug_precursor(&mut self) {
        self.regime.ingest(&MarketEvent::Rug);
        self.refresh_live_markets();
    }

    /// Market phase for `mint`: `true` once migrated (pool), `false` = curve.
    /// Untracked mints default to curve — the §21.7 phase asymmetry makes the
    /// curve model the safe (analytic) default.
    #[must_use]
    pub fn is_pool(&self, mint: &[u8; 32]) -> bool {
        self.mints.get(mint).is_some_and(|c| c.is_pool)
    }

    /// Compact regime snapshot at reflection cadence, classified with
    /// `Default` [`RegimeThresholds`] (v0 illustrative bands; production
    /// supplies calibrated versions). See [`RegimeSummary`] for the exact
    /// derivation of each flag.
    #[must_use]
    pub fn regime_summary(&self) -> RegimeSummary {
        let state = self.regime.classify(&RegimeThresholds::default());
        RegimeSummary {
            frothy: state.launch_velocity == RegimeLevel::High,
            imbalance_up: matches!(
                state.buy_sell_imbalance,
                Some(Skew::Up) | Some(Skew::StrongUp)
            ),
            rug_elevated: state
                .rug_collapse_rate
                .is_some_and(|l| l >= RegimeLevel::Elevated),
        }
    }

    /// Cluster-adjusted genuine breadth for `mint`
    /// (`genuine_net_exposure_breadth`: positive-net-inventory,
    /// meaningful-net-SOL, unflagged clusters), or `None` if untracked.
    ///
    /// This — not any raw wallet count — is the corroboration input (§28
    /// "raw wallet count is not organic breadth"). Snapshot cost is
    /// O(tracked clusters), bounded by [`BREADTH_MAX_CLUSTERS`]; called at
    /// gate/reflection cadence, not per event.
    #[must_use]
    pub fn genuine_breadth(&self, mint: &[u8; 32]) -> Option<u32> {
        self.mints
            .get(mint)
            .map(|c| c.breadth.snapshot().genuine_net_exposure_breadth)
    }

    /// Phase-correct executable exit cost (bps) for exiting `size_lamports`
    /// of `mint`, via the §21.7 phase-asymmetric model (criterion 101):
    ///
    /// * curve — analytic schedule impact against the latest decoded curve
    ///   reserve + [`EXIT_LATENCY_DRIFT_BPS`] + [`CURVE_RETRY_ADDER_BPS`],
    ///   inflated by the [`CURVE_FAILURE_RATE_BPS`] attempt multiplier;
    /// * pool — constant-product realized impact + latency drift, with
    ///   `base_reserve = quote_reserve =` latest tracked liquidity as the v0
    ///   symmetric proxy (a migrated pump pool opens near-balanced; the
    ///   decoded per-side reserves replace this when the pool decoder lands).
    ///
    /// `None` for untracked mints, unknown (zero) reserves, or a model
    /// refusal — sizing without a priced exit is not allowed to proceed on a
    /// fabricated number (§18/§6.4).
    #[must_use]
    pub fn exit_cost_bps(&self, mint: &[u8; 32], size_lamports: u64) -> Option<u32> {
        let ctx = self.mints.get(mint)?;
        if ctx.liquidity_lamports == 0 {
            return None;
        }
        if ctx.is_pool {
            let pool = PoolExitInputs {
                base_reserve_lamports: ctx.liquidity_lamports,
                quote_reserve_lamports: ctx.liquidity_lamports,
                size_lamports,
                latency_drift_bps: EXIT_LATENCY_DRIFT_BPS,
            };
            executable_exit_cost_bps(Phase::Pool, None, Some(&pool)).ok()
        } else {
            let curve = CurveExitInputs {
                curve_reserve_lamports: ctx.liquidity_lamports,
                size_lamports,
                latency_drift_bps: EXIT_LATENCY_DRIFT_BPS,
                failure_rate_bps: CURVE_FAILURE_RATE_BPS,
                retry_adder_bps: CURVE_RETRY_ADDER_BPS,
            };
            executable_exit_cost_bps(Phase::Curve, Some(&curve), None).ok()
        }
    }

    /// Get-or-insert the per-mint context, stamping recency and evicting the
    /// oldest-activity mint when the bound is hit (deterministic: smallest
    /// `(last_touch, mint)` pair goes first; ordinals are unique so ties
    /// reduce to the mint key).
    fn touch(&mut self, mint: &[u8; 32]) -> &mut MintCtx {
        self.ordinal = self.ordinal.saturating_add(1);
        let now = self.ordinal;
        if let Some(prev) = self.mints.get(mint).map(|c| c.last_touch) {
            self.by_touch.remove(&(prev, *mint));
        } else {
            if self.mints.len() >= MAX_TRACKED_MINTS {
                if let Some((_, oldest)) = self.by_touch.pop_first() {
                    self.mints.remove(&oldest);
                }
            }
            self.mints.insert(*mint, MintCtx::new(now));
        }
        self.by_touch.insert((now, *mint));
        // Unwrap-free: the entry was just inserted or already present; the
        // fallback re-inserts a fresh context (unreachable in practice).
        let entry = self.mints.entry(*mint).or_insert_with(|| MintCtx::new(now));
        entry.last_touch = now;
        entry
    }

    /// Refresh the regime's live-market denominator from what this fold can
    /// observe: launches seen or distinct tracked mints, whichever is larger
    /// (v0 proxy; the server-side live-market census replaces it). Zero stays
    /// unset so the rug rate classifies as UNKNOWN, never as 0-of-0 (§6.4).
    fn refresh_live_markets(&mut self) {
        let tracked = u64::try_from(self.mints.len()).unwrap_or(u64::MAX);
        let live = self.launches_seen.max(tracked);
        if live > 0 {
            self.regime.set_live_markets(live);
        }
    }

    /// Test-only: number of tracked mints (bound audit).
    #[cfg(test)]
    fn tracked_mints(&self) -> usize {
        self.mints.len()
    }
}

// ===========================================================================================
// Tests
// ===========================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic mint key from an index.
    fn mk(i: u32) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[..4].copy_from_slice(&i.to_be_bytes());
        k
    }

    const LIQ: u64 = 1_000_000_000; // 1 SOL reserve
    const SIZE: u64 = 100_000_000; // 0.1 SOL exit

    #[test]
    fn phase_flip_changes_exit_cost_path() {
        let mut ctx = MarketContext::new();
        let m = mk(1);
        assert_eq!(ctx.exit_cost_bps(&m, SIZE), None, "untracked -> None");
        assert!(!ctx.is_pool(&m));

        ctx.on_trade(&m, 1, true, 20_000_000, 1_000, LIQ);
        // Curve: impact 1e8*1e4/1e9 = 1_000; +50 drift +100 retry = 1_150;
        // attempt multiplier 1_150*10_000/9_500 = 1_210 (truncated).
        assert_eq!(ctx.exit_cost_bps(&m, SIZE), Some(1_210));

        ctx.on_migration(&m);
        assert!(ctx.is_pool(&m));
        // Pool: impact 1e8*1e4/(1e9+1e8) = 909; +50 drift = 959.
        assert_eq!(ctx.exit_cost_bps(&m, SIZE), Some(959));

        // Zero decoded liquidity -> no fabricated exit price.
        let m2 = mk(2);
        ctx.on_trade(&m2, 1, true, 20_000_000, 1_000, 0);
        assert_eq!(ctx.exit_cost_bps(&m2, SIZE), None);
        // Migration alone (no trade yet) tracks phase but has no reserve.
        let m3 = mk(3);
        ctx.on_migration(&m3);
        assert!(ctx.is_pool(&m3));
        assert_eq!(ctx.exit_cost_bps(&m3, SIZE), None);
    }

    #[test]
    fn regime_summary_flips_on_rug_launch_and_imbalance_events() {
        let mut ctx = MarketContext::new();
        let s0 = ctx.regime_summary();
        assert!(!s0.frothy && !s0.imbalance_up && !s0.rug_elevated);

        // One launch, no rugs: rate 0 bps -> Low -> not elevated.
        ctx.on_launch();
        assert!(!ctx.regime_summary().rug_elevated);
        // One rug against one live market: 10_000 bps -> High >= Elevated.
        ctx.on_rug_precursor();
        assert!(ctx.regime_summary().rug_elevated);

        // Launch velocity: 200 launches in the window -> High -> frothy.
        assert!(!ctx.regime_summary().frothy);
        for _ in 0..199 {
            ctx.on_launch();
        }
        assert!(ctx.regime_summary().frothy);

        // Buy/sell imbalance: 10 buys vs 1 sell -> +8_181 bps -> StrongUp.
        assert!(!ctx.regime_summary().imbalance_up);
        let m = mk(1);
        for e in 0..10u64 {
            ctx.on_trade(&m, e, true, 20_000_000, 1_000, LIQ);
        }
        ctx.on_trade(&m, 99, false, 20_000_000, 1_000, LIQ);
        assert!(ctx.regime_summary().imbalance_up);
    }

    #[test]
    fn genuine_breadth_counts_only_unflagged_net_exposure_clusters() {
        let mut ctx = MarketContext::new();
        let m = mk(1);
        assert_eq!(ctx.genuine_breadth(&m), None, "untracked -> None");
        // Three distinct entities accumulate 0.02 SOL each: genuine = 3.
        for e in 1..=3u64 {
            ctx.on_trade(&m, e, true, 20_000_000, 1_000, LIQ);
        }
        assert_eq!(ctx.genuine_breadth(&m), Some(3));
        // A round-tripping entity (flat inventory) never counts as genuine.
        ctx.on_trade(&m, 4, true, 20_000_000, 1_000, LIQ);
        ctx.on_trade(&m, 4, false, 20_000_000, 1_000, LIQ);
        assert_eq!(ctx.genuine_breadth(&m), Some(3));
        // Dust breadth (< 0.01 SOL net) never counts as genuine.
        ctx.on_trade(&m, 5, true, 1_000_000, 50, LIQ);
        assert_eq!(ctx.genuine_breadth(&m), Some(3));
    }

    #[test]
    fn mint_table_bounded_with_oldest_activity_eviction() {
        let mut ctx = MarketContext::new();
        for i in 0..u32::try_from(MAX_TRACKED_MINTS).unwrap() + 1 {
            ctx.on_trade(&mk(i), u64::from(i), true, 20_000_000, 1_000, LIQ);
        }
        assert_eq!(ctx.tracked_mints(), MAX_TRACKED_MINTS);
        // Mint 0 (oldest activity) evicted; newer mints retained.
        assert_eq!(ctx.genuine_breadth(&mk(0)), None);
        assert_eq!(ctx.genuine_breadth(&mk(1)), Some(1));
        let last = u32::try_from(MAX_TRACKED_MINTS).unwrap();
        assert_eq!(ctx.genuine_breadth(&mk(last)), Some(1));
        // Re-touching an old mint refreshes recency: mint 1 survives the next
        // eviction, mint 2 (now oldest) goes.
        ctx.on_trade(&mk(1), 1, true, 20_000_000, 1_000, LIQ);
        ctx.on_trade(&mk(9_999_999), 1, true, 20_000_000, 1_000, LIQ);
        assert_eq!(ctx.tracked_mints(), MAX_TRACKED_MINTS);
        assert_eq!(ctx.genuine_breadth(&mk(1)), Some(1));
        assert_eq!(ctx.genuine_breadth(&mk(2)), None);
    }

    #[test]
    fn identical_event_sequences_reproduce_identical_context() {
        let feed = |ctx: &mut MarketContext| {
            ctx.on_launch();
            for i in 0..200u64 {
                let m = mk(u32::try_from(i % 5).unwrap());
                ctx.on_trade(&m, i % 13, i % 3 != 0, 15_000_000 + i * 7, 900 + i, LIQ + i);
            }
            ctx.on_migration(&mk(2));
            ctx.on_rug_precursor();
        };
        let (mut a, mut b) = (MarketContext::new(), MarketContext::new());
        feed(&mut a);
        feed(&mut b);
        assert_eq!(a.regime_summary(), b.regime_summary());
        for i in 0..5u32 {
            assert_eq!(a.genuine_breadth(&mk(i)), b.genuine_breadth(&mk(i)));
            assert_eq!(a.exit_cost_bps(&mk(i), SIZE), b.exit_cost_bps(&mk(i), SIZE));
            assert_eq!(a.is_pool(&mk(i)), b.is_pool(&mk(i)));
        }
        assert!(a.is_pool(&mk(2)) && b.is_pool(&mk(2)));
    }
}
