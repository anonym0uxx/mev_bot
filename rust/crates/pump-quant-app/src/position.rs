//! The held-position **exit lifecycle** — OPEN → UPDATE(per-swap) → CLOSE.
//!
//! Before this, the engine booked a scalp's PnL in one shot at admit time under a
//! flat expected-move assumption: it never tracked the position forward, so it
//! could neither harvest a runner nor cut a rug. This module closes that gap — the
//! §24 mandate that "the scalp lane's position state must be driven per-swap from
//! the decoded market-state event stream," and the §48 exit-family whose objective
//! is **net-SOL expectancy under survival constraints** (never win-rate).
//!
//! The research (arxiv + Monte Carlo, project docs) is decisive for a low-cap
//! memecoin scalper: the payoff is two-tailed — a convex right tail (occasional
//! 5–50×) and a mostly-**non-exitable** left tail (rug gaps to ~0). So we HARVEST
//! the right tail with a vol-scaled trailing stop that widens as the position runs,
//! and DEFEND the left tail by leaving *before* the gap: a rug-precursor dump, a
//! CVD/flow thesis-invalidation, a principal-recovery scale-out, and a conditional
//! time-stop. In the MC this roughly doubled mean net SOL per trade and cut the
//! left-tail CVaR ~64% vs the one-shot fill.
//!
//! # Discipline (binding)
//! * **Deterministic, integer, tick-clocked (§22).** Price is carried as a
//!   fixed-point `u64` (`PRICE_SCALE` units); the multiple-of-entry is bps
//!   (`10_000` = break-even). Time is the engine's logical tick — no wall-clock,
//!   no float. The same swap stream always yields the same exits and the same net.
//! * **Corroboration-tier / never a veto.** The lifecycle only *manages* a position
//!   the gate already admitted; creator/flow risk can shrink or exit it, never
//!   authorise entry (that stays with the on-chain-confirmation gate, §29/§71).
//! * **Trailing reuses the strategy leaf.** The trailing + hard-SL protection level
//!   is `pump_quant_strategy::exit_ladder::protection_level_fp` (whole-lifecycle
//!   protection, armed at entry — the §24 defect-2 fix), not a re-implementation.
//! * **Bounded (§99).** Open positions are capped; the manager evicts nothing
//!   silently — a position is only removed when it CLOSES.

use pump_quant_signals::microstructure::{burst_phase, BurstPhase};
use pump_quant_strategy::exit_ladder::protection_level_fp;
use std::collections::BTreeMap;

/// §24(d) LAW 5: length of the bounded recent-gap ring whose max is the climax
/// detector's slow-baseline arrival reference (§99 fixed per-position footprint).
const ARR_RING: usize = 8;

/// Named lifecycle parameters (§102 — each a documented scale, not a magic number).
/// Multiples are bps of entry price (`10_000` = 1.0× = break-even); fractions are
/// bps of the *original* position size; times are logical ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleParams {
    /// Catastrophic hard stop below entry (bps drawdown from entry). Slow-bleed
    /// backstop — NOT rug protection (a rug gaps through it; the precursor catches
    /// that).
    pub hard_sl_bps: u32,
    /// Minimum trailing-stop width from the running peak (bps).
    pub trail_base_bps: u32,
    /// Trailing widens with the move: `trail = clamp(base, (peak_mult-1x)/k, max)`.
    pub trail_k_div: u32,
    /// Maximum trailing width (bps) — a very large winner is never stopped by noise.
    pub trail_max_bps: u32,
    /// Take-profit tranche 1 target (mult bps): sell enough to recover principal.
    pub tp1_bps: u32,
    /// Tranche 2 target (mult bps) and the fraction of ORIGINAL size to sell (bps).
    pub tp2_bps: u32,
    /// Tranche 2 sell fraction (bps of original size).
    pub tp2_frac_bps: u32,
    /// Tranche 3 target (mult bps).
    pub tp3_bps: u32,
    /// Tranche 3 sell fraction (bps of original size).
    pub tp3_frac_bps: u32,
    /// Thesis-invalidation: exit the remainder when cumulative volume delta falls to
    /// this fraction (bps) of its peak — order flow has rolled over (§21.7/§32).
    pub cvd_hold_frac_bps: u32,
    /// Runner stall: exit if no new high for this many ticks while in profit.
    pub stall_ticks: u64,
    /// Hard time-stop (ticks since entry) — binds only when not making new highs.
    pub max_hold_ticks: u64,
    /// Rug precursor: a single-swap price drop of at least this (bps) dumps the
    /// remainder immediately (accept slippage; being early beats a total-loss gap).
    pub precursor_drop_bps: u32,
    /// Round-trip venue fee charged on each sell tranche (bps of proceeds).
    pub fee_bps: u32,
    /// First-sell penalty (bps of the sold notional), charged once on the first exit.
    pub first_sell_penalty_bps: u32,
    /// Fixed tip (lamports) charged per sell tranche.
    pub tip_lamports: u64,
    /// Adversarial exit impairment (bps of gross proceeds) applied to EVERY sell —
    /// the §38 Mode-C severity threading: 0 under SignalReplay/OptimisticCeiling,
    /// the configured retry-slippage under AdversarialRealistic, doubled under
    /// AdversarialPessimistic. Keeps paper net-SOL from compounding optimism into
    /// the sizing and reflection loops.
    pub exit_impair_bps: u32,
    /// §24(d) LAW 5: arm the exit-into-strength trigger (sell into a buy-side burst
    /// climax while in profit). Per-position so shadow challengers can carry it
    /// independently of the incumbent (§48 pre-registered axis).
    pub into_strength_exit_enable: bool,
    /// FILL FIDELITY: when true, every sell additionally pays its own
    /// constant-product curve impact, `notional · 10_000 / vsol` bps, on top of
    /// `exit_impair_bps`. Selling at the observed print credits us a price the curve
    /// would never have given — see `curve_fill::own_impact_bps`. Default false so
    /// the historical pins hold; MUST be true for any real-data backtest.
    pub curve_exact_fill: bool,
    /// §24(d) LAW 5: burst arrival-rate elevation multiple (bps of 10_000) over
    /// baseline that a plateaued recent window must clear to count as a climax.
    pub into_strength_climax_bp: u32,
    /// §24 LAW 6: arm the volatility-scaled stop/trail (widen the hard stop and
    /// trailing width with realized vol, inside the envelope). Per-position.
    pub vol_stop_enable: bool,
    /// §24 LAW 6: fraction (bps of 10_000) of the position's realized-vol bps added
    /// to the base stop/trail width before the envelope clamp.
    pub vol_stop_scale_bp: u32,
}

impl LifecycleParams {
    /// Shipped defaults, chosen from the research/MC (project docs). Rationale is on
    /// each constant; every one is operator-overridable via [`crate::config::Config`].
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            hard_sl_bps: 3_500,    // −35% catastrophic backstop
            trail_base_bps: 2_200, // ≥22% give-back before trailing out
            trail_k_div: 4,        // widen the trail as the winner runs
            trail_max_bps: 12_000,
            tp1_bps: 13_500,          // +35%: recover principal (60% collapse <20min)
            tp2_bps: 25_000,          // 2.5×: trim
            tp2_frac_bps: 3_000,      // sell 30% of original
            tp3_bps: 50_000,          // 5×: trim
            tp3_frac_bps: 3_000,      // sell 30% of original
            cvd_hold_frac_bps: 4_500, // flow gave back >55% of peak → thesis dead
            stall_ticks: 25,
            max_hold_ticks: 300,
            precursor_drop_bps: 3_000, // −30% single-swap step = collapse onset
            fee_bps: 100,
            first_sell_penalty_bps: 150,
            tip_lamports: 10_000,
            exit_impair_bps: 0, // Mode A/B default; engine sets from cfg.fill_mode
            curve_exact_fill: false, // fill fidelity; MUST be armed for real-data backtests
            into_strength_exit_enable: false, // LAW 5 off by default; operator/challenger arms
            into_strength_climax_bp: 20_000, // 2× baseline arrival = a genuine climax
            vol_stop_enable: false, // LAW 6 off by default
            vol_stop_scale_bp: 5_000, // 0.5× realized-vol added to base stop/trail
        }
    }
}

impl Default for LifecycleParams {
    fn default() -> Self {
        Self::standard()
    }
}

/// Why a (partial or full) exit fired — recorded for attribution / the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// A single-swap collapse onset — dump the remainder immediately.
    RugPrecursor,
    /// Price hit the catastrophic hard stop below entry.
    HardStop,
    /// Order flow (CVD) rolled over, or the runner stalled in profit.
    ThesisInvalidation,
    /// A take-profit tranche (principal recovery or a trim).
    TakeProfitLadder,
    /// The vol-scaled trailing stop off the peak.
    TrailingStop,
    /// The conditional time-stop (not advancing).
    TimeStop,
    /// Forced close at end of run (no trigger fired first).
    ForceClose,
    /// §26 confirmed creator-dump hard exit — the deployer distributed past the
    /// veto threshold while the position was held (operator-approved reversal).
    CreatorDump,
    /// §24(d) exit-into-strength — sold the remainder INTO an authentic buy-side
    /// burst climax while in profit (harvest the buyers, not the exhaustion).
    IntoStrength,
}

impl ExitReason {
    /// Whether this exit closes the whole remaining position (vs a partial tranche).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, ExitReason::TakeProfitLadder)
    }

    /// A stable small code for the decision journal.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            ExitReason::RugPrecursor => 1,
            ExitReason::HardStop => 2,
            ExitReason::ThesisInvalidation => 3,
            ExitReason::TakeProfitLadder => 4,
            ExitReason::TrailingStop => 5,
            ExitReason::TimeStop => 6,
            ExitReason::ForceClose => 7,
            ExitReason::CreatorDump => 8,
            ExitReason::IntoStrength => 9,
        }
    }
}

/// §24 LAW 2 per-market derived take-profit ladder, computed once at admit from
/// the gate's measured round-trip cost (via `exit_ladder::derive_target_bps`) and
/// the cost-priced rung count (via `exit_ladder::ladder_rungs`). Replaces the
/// fixed tp1/tp2/tp3 multiples for the position it is armed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivedTargets {
    /// Tranche-1 target, mult bps of entry (10_000 = break-even).
    pub tp1_bps: u32,
    /// Tranche-2 target, mult bps.
    pub tp2_bps: u32,
    /// Tranche-3 target, mult bps.
    pub tp3_bps: u32,
    /// Cost-priced rung COUNT (1..=3): tranches beyond this are disabled — a
    /// position too small to carry a rung above the fixed-cost floor takes fewer.
    pub rungs: u8,
}

/// One realized (partial or full) exit event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The market.
    pub mint: [u8; 32],
    /// Net realized lamports for THIS exit (proceeds − fees − penalty − tip),
    /// signed; already net of the pro-rata entry cost of the sold fraction.
    pub net_lamports: i128,
    /// Why it fired.
    pub reason: ExitReason,
    /// Whether the position is now fully closed.
    pub closed: bool,
    /// Maximum favorable excursion over the position's life, bps of entry
    /// (peak/entry − 1). Feeds §48 MFE-capture efficiency and §49 convexity rows.
    pub mfe_bps: i64,
    /// Maximum adverse excursion, bps of entry (trough/entry − 1; ≤ 0).
    pub mae_bps: i64,
}

/// One held position, integer/fixed-point.
#[derive(Clone, Copy, Debug)]
struct HeldPosition {
    /// Freshest observed curve SOL side (lamports). Updated on every swap; the
    /// depth our own exit walks when `curve_exact_fill` is armed. 0 = unknown,
    /// which fails CLOSED (no curve impact can be priced, so no sell is credited
    /// a price the curve would not have given).
    liq_lamports: u64,
    entry_price_fp: u64,
    peak_price_fp: u64,
    prev_price_fp: u64,
    /// Total deployed notional (lamports) at entry.
    size_lamports: u64,
    /// Pro-rata entry cost per unit fraction: `size + entry tip` (recovered at TP1).
    cost_lamports: u64,
    /// Fraction of the original position still held, in bps (10_000 = full).
    remaining_bps: u32,
    cvd: i128,
    cvd_peak: i128,
    entry_tick: u64,
    last_high_tick: u64,
    /// Bit i set once tranche i has been taken.
    tranche_mask: u8,
    took_first_sell: bool,
    /// Lowest price seen since entry (MAE tracking; §48 excursion rows).
    trough_price_fp: u64,
    /// Meta-saturation exit pressure (§21.4 third consumption limb): when set, the
    /// stall window and trail cap are HALVED — a saturating category tightens its
    /// open positions' exits without vetoing them.
    pressure: bool,
    /// Whether the probe→confirm scale-in has already fired (§33: one-shot).
    scaled: bool,
    /// §24 LAW 2 per-market derived take-profit ladder (None = use the fixed
    /// `LifecycleParams` tp1/tp2/tp3). Armed at admit via [`ScalpLifecycle::arm_context`].
    derived: Option<DerivedTargets>,
    /// §24 LAW 6 realized volatility of the recent bar window at admit, bps —
    /// scales the hard stop / trail width inside the envelope.
    vol_bps: u32,
    /// §24(d) LAW 5 swap-arrival tracking (for `burst_phase`): logical tick of the
    /// previous swap, the previous inter-swap gap, and a bounded ring of the recent
    /// inter-swap gaps whose MAX is the slow-baseline reference (truncation-free,
    /// unlike an integer EMA of small gaps).
    last_tick: u64,
    prior_gap: u64,
    gap_ring: [u64; ARR_RING],
    gap_ring_len: u64,
    /// Swaps observed (the climax detector needs a short baseline first).
    trades_seen: u64,
}

impl HeldPosition {
    /// (MFE, MAE) in signed bps of entry: peak/entry−1 and trough/entry−1.
    fn excursions_bps(&self) -> (i64, i64) {
        let m = |p: u64| -> i64 {
            if self.entry_price_fp == 0 {
                return 0;
            }
            ((u128::from(p) * 10_000 / u128::from(self.entry_price_fp)) as i64) - 10_000
        };
        (m(self.peak_price_fp), m(self.trough_price_fp))
    }

    #[inline]
    fn mult_bps(&self, price_fp: u64) -> u32 {
        if self.entry_price_fp == 0 {
            return 10_000;
        }
        ((u128::from(price_fp) * 10_000) / u128::from(self.entry_price_fp)) as u32
    }

    /// Vol-scaled trailing width: widens as the position runs (bps). Under
    /// meta-saturation pressure the cap halves — a saturating category gives a
    /// winner less room before the trail takes it (§21.4).
    fn trail_bps(&self, p: &LifecycleParams) -> u32 {
        let peak_mult = self.mult_bps(self.peak_price_fp);
        let excess = peak_mult.saturating_sub(10_000);
        let scaled = excess / p.trail_k_div.max(1);
        let max = if self.pressure {
            (p.trail_max_bps / 2).max(p.trail_base_bps)
        } else {
            p.trail_max_bps
        };
        scaled.clamp(p.trail_base_bps.min(max), max)
    }

    /// §24 LAW 6: the effective (trail, hard_sl) after volatility scaling, clamped
    /// INSIDE the position's `[trail_base_bps, trail_max_bps]` envelope (never
    /// outside floor/ceiling — §56.2). When `vol_stop_enable` is off this is the
    /// unscaled `(base_trail, p.hard_sl_bps)`, byte-identical to prior behaviour.
    fn protection_widths(&self, p: &LifecycleParams) -> (u32, u32) {
        let base_trail = self.trail_bps(p);
        if !p.vol_stop_enable {
            return (base_trail, p.hard_sl_bps);
        }
        let extra = ((u64::from(self.vol_bps) * u64::from(p.vol_stop_scale_bp)) / 10_000) as u32;
        let floor = p.trail_base_bps.min(p.trail_max_bps);
        let ceil = p.trail_max_bps;
        let trail = base_trail.saturating_add(extra).clamp(floor, ceil);
        // A volatile market gets a WIDER hard stop (more room for noise), never
        // tighter than the base and never past the envelope ceiling.
        let hard_sl = p
            .hard_sl_bps
            .saturating_add(extra)
            .clamp(p.hard_sl_bps.min(ceil), ceil);
        (trail, hard_sl)
    }

    /// §24(d) LAW 5: whether the current swap sits at an authentic buy-side burst
    /// CLIMAX while in profit — `pump_quant_signals::microstructure::burst_phase`
    /// over the position's own swap-arrival stream (recent gap vs the prior gap vs
    /// a slow-decaying baseline gap). Deterministic integer rates (§22).
    fn buy_climax(
        &self,
        price_fp: u64,
        signed_quote: i128,
        tick: u64,
        p: &LifecycleParams,
    ) -> bool {
        // In profit and this swap is a net buy — the "into strength" precondition
        // (the burst-arrival plateau below supplies the "climax", not exhaustion).
        if self.mult_bps(price_fp) <= 10_000 || signed_quote <= 0 {
            return false;
        }
        if self.trades_seen < 3 {
            return false; // need a baseline before a climax can be defined
        }
        const RATE_SCALE: u64 = 1_000_000;
        let recent_gap = tick.saturating_sub(self.last_tick).max(1);
        // Baseline = the slowest recent inter-swap gap (max over the ring): a climax
        // is the recent arrival rate strongly elevated over that slow baseline.
        let base_gap = self.gap_ring.iter().copied().max().unwrap_or(1).max(1);
        let recent = RATE_SCALE / recent_gap;
        let prior = RATE_SCALE / self.prior_gap.max(1);
        let baseline = RATE_SCALE / base_gap;
        let mult = (u64::from(p.into_strength_climax_bp) / 10_000).max(1);
        matches!(
            burst_phase(recent, prior, baseline, mult),
            BurstPhase::Climax
        )
    }

    /// Net lamports realized by selling `frac_bps` of the ORIGINAL size at
    /// `mult_bps`, charging fee + (once) first-sell penalty + tip, and netting the
    /// pro-rata entry cost. Integer, saturating (§22).
    fn realize(&mut self, frac_bps: u32, mult_bps: u32, p: &LifecycleParams) -> i128 {
        let frac_bps = frac_bps.min(self.remaining_bps);
        if frac_bps == 0 {
            return 0;
        }
        let notional = u128::from(self.size_lamports) * u128::from(frac_bps) / 10_000;
        let mut gross = notional * u128::from(mult_bps) / 10_000;
        // §38 adversarial impairment: every sell pays the configured extra slippage
        // under Mode C (0 in Modes A/B), so paper proceeds are execution-honest.
        // FILL FIDELITY: our own sell walks the constant-product curve, so it
        // realizes `vsol/(vsol + notional)` of the observed print — exactly
        // `notional · 10_000 / vsol` bps of adverse impact (the token reserve
        // cancels; see `curve_fill::own_impact_bps`). Added to the §38 adversarial
        // impairment rather than replacing it: they are different frictions.
        let curve_bps = if p.curve_exact_fill && self.liq_lamports > 0 {
            u32::try_from(notional * 10_000 / u128::from(self.liq_lamports)).unwrap_or(10_000)
        } else {
            0
        };
        let impair = p.exit_impair_bps.saturating_add(curve_bps).min(10_000);
        gross -= gross * u128::from(impair) / 10_000;
        let fee = gross * u128::from(p.fee_bps) / 10_000;
        let penalty = if self.took_first_sell {
            0
        } else {
            notional * u128::from(p.first_sell_penalty_bps) / 10_000
        };
        self.took_first_sell = true;
        let cost = u128::from(self.cost_lamports) * u128::from(frac_bps) / 10_000;
        self.remaining_bps -= frac_bps;
        // proceeds − fee − penalty − tip − pro-rata entry cost
        let proceeds = gross.saturating_sub(fee).saturating_sub(penalty);
        (proceeds as i128)
            .saturating_sub(cost as i128)
            .saturating_sub(i128::from(p.tip_lamports))
    }
}

/// The bounded per-mint held-position manager. Fed by the engine's admit + swap +
/// tick path; a run that admits nothing holds nothing and books nothing.
#[derive(Clone, Debug)]
pub struct ScalpLifecycle {
    open: BTreeMap<[u8; 32], HeldPosition>,
    params: LifecycleParams,
    cap: usize,
    /// Reused per-tick scratch for the `on_tick` time-stop scan (O5): the mints
    /// whose time-stop fired this tick. Cleared (not freed) each call via
    /// `mem::take`, so the per-tick exit scan allocates nothing in steady state.
    /// Bounded by `cap` (≤ max_concurrent_positions). No state crosses ticks.
    fired_buf: Vec<[u8; 32]>,
}

impl ScalpLifecycle {
    /// A fresh manager under `params`, holding at most `cap` concurrent positions.
    #[must_use]
    pub fn new(params: LifecycleParams, cap: usize) -> Self {
        Self {
            open: BTreeMap::new(),
            params,
            cap: cap.max(1),
            fired_buf: Vec::with_capacity(cap.max(1)),
        }
    }

    /// The incumbent exit parameters this manager runs under (read-only).
    ///
    /// The §48 tournament and the LAW B8 proposal derivation both need to diff
    /// against the LIVE policy; reading it back from the single owner is the only
    /// way those two can never disagree about what the incumbent actually is.
    #[must_use]
    pub const fn params(&self) -> &LifecycleParams {
        &self.params
    }

    /// Whether any position is open (an empty manager books nothing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Number of open positions (bounded by `cap`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// Whether `mint` currently has an open position.
    #[must_use]
    pub fn has(&self, mint: &[u8; 32]) -> bool {
        self.open.contains_key(mint)
    }

    /// OPEN a position on admit at `entry_price_fp` for `size_lamports`, tagged with
    /// the logical `tick` and its `entry_cost_lamports` (principal to recover). A
    /// second open on a mint already held is ignored (one scalp per mint at a time).
    /// Bounded (§99): refused when `cap` concurrent positions are already open.
    pub fn open(
        &mut self,
        mint: [u8; 32],
        entry_price_fp: u64,
        size_lamports: u64,
        entry_cost_lamports: u64,
        tick: u64,
    ) -> bool {
        if self.open.contains_key(&mint) || self.open.len() >= self.cap {
            return false;
        }
        self.open.insert(
            mint,
            HeldPosition {
                liq_lamports: 0,
                entry_price_fp,
                peak_price_fp: entry_price_fp,
                prev_price_fp: entry_price_fp,
                size_lamports,
                cost_lamports: entry_cost_lamports,
                remaining_bps: 10_000,
                cvd: 0,
                cvd_peak: 0,
                entry_tick: tick,
                last_high_tick: tick,
                tranche_mask: 0,
                took_first_sell: false,
                trough_price_fp: entry_price_fp,
                pressure: false,
                scaled: false,
                derived: None,
                vol_bps: 0,
                last_tick: tick,
                prior_gap: 0,
                gap_ring: [0; ARR_RING],
                gap_ring_len: 0,
                trades_seen: 0,
            },
        );
        true
    }

    /// §24 LAW 2 / LAW 6 admit-time context: arm the per-market derived take-profit
    /// ladder (`derived`, computed by the engine from the gate's measured round-trip
    /// cost) and record the recent-window realized volatility (`vol_bps`) used to
    /// scale the stop/trail. Idempotent-safe; a no-op when `mint` is not held. When
    /// `derived` is `None` and `vol_bps` is `0` the position is byte-identical to one
    /// never armed (the off-by-default path).
    pub fn arm_context(&mut self, mint: &[u8; 32], vol_bps: u32, derived: Option<DerivedTargets>) {
        if let Some(pos) = self.open.get_mut(mint) {
            pos.vol_bps = vol_bps;
            pos.derived = derived;
        }
    }

    /// Apply meta-saturation exit pressure to an open position (§21.4): halves the
    /// stall window and trail cap from now on. Idempotent; no-op when not held.
    pub fn apply_pressure(&mut self, mint: &[u8; 32]) {
        if let Some(pos) = self.open.get_mut(mint) {
            pos.pressure = true;
        }
    }

    /// One-shot probe→confirm **scale-in** (§33 Layer 1 / crit 75): add
    /// `add_lamports` of size and `add_cost_lamports` of entry cost to an open
    /// position at the current mark. Fires at most once per position (the probe
    /// opened it; deterministic confirmation scales it to target); refused after
    /// any tranche has been sold (never re-risk a de-risking position). Returns
    /// whether the scale-in was applied.
    /// §33 one-shot scale-in, at a **weighted-average cost basis**.
    ///
    /// # Why the basis must move (audit 2026-07-25)
    ///
    /// This function used to add `add_lamports` to `size_lamports` while leaving
    /// `entry_price_fp` untouched. Every later [`Self::realize`] computes
    /// `gross = size × mult_bps / 10_000` with `mult_bps` measured against
    /// `entry_price_fp`, so lamports bought at the CURRENT mark were booked as if
    /// they had been bought at the ORIGINAL entry — pure phantom profit equal to
    /// `add_lamports × (mark/entry − 1)` at the moment of the add. The trigger
    /// (evidenced authenticity + structure not Downtrend) conditions on a RISING
    /// tape, so the error was systematically in our favour: precisely the direction
    /// that flatters a backtest and cannot be collected in cash.
    ///
    /// Blending the basis makes `size × mult_bps` mean "units × exit price" again,
    /// which is the only reading under which realized net SOL is real.
    ///
    /// # Arithmetic (§22) — and why it is NOT the obvious weighted average
    ///
    /// `size_lamports` is a **NOTIONAL** (lamports deployed), not a unit count, and
    /// `realize` computes `gross = size × mult_bps / 10_000`. The units a notional
    /// `s` buys at price `p` is `s / p`, so the basis that makes
    /// `total_notional × P / B` equal the true `units × P` is the **harmonic**
    /// (notional-weighted) mean, not the arithmetic one:
    ///
    /// ```text
    ///     B = (s1 + s2) · p1 · p2 / (s1·p2 + s2·p1)
    /// ```
    ///
    /// The arithmetic mean `(s1·p1 + s2·p2)/(s1+s2)` is the natural-looking answer
    /// and it is WRONG here — it would leave a residual phantom of ~0.9% of the
    /// added tranche at a 1.2× add, because it implicitly treats notionals as unit
    /// counts. `tests/scale_in_basis.rs` catches exactly that error.
    ///
    /// Integer-only via `u128`, rounded **UP** (`div_ceil`), which is fail-closed: a
    /// higher basis can only LOWER every subsequent `mult_bps`, so truncation never
    /// manufactures profit. Products are `checked_*`; the function REFUSES the add
    /// on overflow or on a missing (zero) mark rather than booking risk against
    /// unknown evidence (§6.4).
    ///
    /// A residual of at most **1 bp of notional** remains after blending, because
    /// `mult_bps` is itself quantized to integer basis points — that is a property
    /// of the price representation, not of this function, and it is conservative.
    pub fn scale_in(
        &mut self,
        mint: &[u8; 32],
        add_lamports: u64,
        add_cost_lamports: u64,
        mark_price_fp: u64,
    ) -> bool {
        let Some(pos) = self.open.get_mut(mint) else {
            return false;
        };
        if pos.scaled || pos.tranche_mask != 0 || pos.remaining_bps != 10_000 {
            return false;
        }
        // A zero mark is missing evidence, not a price — never add risk on it.
        if add_lamports == 0 || mark_price_fp == 0 || pos.entry_price_fp == 0 {
            return false;
        }
        let s1 = u128::from(pos.size_lamports);
        let s2 = u128::from(add_lamports);
        let p1 = u128::from(pos.entry_price_fp);
        let p2 = u128::from(mark_price_fp);
        // den = s1·p2 + s2·p1  (proportional to total UNITS held)
        let Some(den) = s1
            .checked_mul(p2)
            .and_then(|a| s2.checked_mul(p1).and_then(|b| a.checked_add(b)))
        else {
            return false;
        };
        if den == 0 {
            return false;
        }
        // num = (s1 + s2)·p1·p2
        let Some(num) = s1
            .checked_add(s2)
            .and_then(|s| s.checked_mul(p1))
            .and_then(|s| s.checked_mul(p2))
        else {
            return false;
        };
        let Ok(blended) = u64::try_from(num.div_ceil(den)) else {
            return false;
        };
        if blended == 0 {
            return false;
        }
        pos.entry_price_fp = blended;
        pos.scaled = true;
        pos.size_lamports = pos.size_lamports.saturating_add(add_lamports);
        pos.cost_lamports = pos.cost_lamports.saturating_add(add_cost_lamports);
        true
    }

    /// UPDATE on a decoded swap for `mint`. Advances peak/CVD, then evaluates the
    /// priority-ordered triggers and returns any exit that fired (partial ladder
    /// tranches keep the position open; every other trigger closes it). `None` when
    /// no position is held or nothing fired.
    pub fn on_trade(
        &mut self,
        mint: &[u8; 32],
        price_fp: u64,
        signed_quote: i128,
        tick: u64,
        liq_lamports: u64,
    ) -> Option<Exit> {
        let p = self.params;
        let pos = self.open.get_mut(mint)?;
        // Freshest curve depth — what our own exit would walk (fails closed at 0).
        pos.liq_lamports = liq_lamports;
        pos.cvd = pos.cvd.saturating_add(signed_quote);
        if pos.cvd > pos.cvd_peak {
            pos.cvd_peak = pos.cvd;
        }
        if price_fp > pos.peak_price_fp {
            pos.peak_price_fp = price_fp;
            pos.last_high_tick = tick;
        }
        if price_fp < pos.trough_price_fp {
            pos.trough_price_fp = price_fp;
        }
        // §24(d) LAW 5: evaluate the buy-side burst climax against the CURRENT
        // swap-arrival gaps, THEN advance the gap/baseline track (so "climax" is a
        // plateau of this swap's arrival vs the prior swap's, over the baseline).
        let climax =
            p.into_strength_exit_enable && pos.buy_climax(price_fp, signed_quote, tick, &p);
        let recent_gap = tick.saturating_sub(pos.last_tick);
        // Record this gap in the bounded ring (its max is the baseline reference).
        let slot = (pos.gap_ring_len as usize) % ARR_RING;
        pos.gap_ring[slot] = recent_gap;
        pos.gap_ring_len = pos.gap_ring_len.saturating_add(1);
        pos.prior_gap = recent_gap;
        pos.last_tick = tick;
        pos.trades_seen = pos.trades_seen.saturating_add(1);
        // Capture the previous print and advance it unconditionally, BEFORE any
        // trigger can early-return — the precursor must always compare consecutive
        // prints (a stale prev after a ladder tranche would blind it).
        let prev_price_fp = pos.prev_price_fp;
        pos.prev_price_fp = price_fp;
        let mult = pos.mult_bps(price_fp);

        // P0 rug precursor: a large single-swap fall — dump the remainder now.
        if prev_price_fp > 0 && price_fp < prev_price_fp {
            let drop = ((u128::from(prev_price_fp - price_fp) * 10_000) / u128::from(prev_price_fp))
                as u32;
            if drop >= p.precursor_drop_bps {
                return Some(self.close(mint, mult, ExitReason::RugPrecursor));
            }
        }

        // P1 hard stop + P4 trailing, via the strategy protection leaf (whole-life).
        // §24 LAW 6: the trail/hard-stop widths are volatility-scaled inside the
        // envelope (identity when `vol_stop_enable` is off).
        let (trail, hard_sl) = pos.protection_widths(&p);
        let protect = protection_level_fp(pos.peak_price_fp, pos.entry_price_fp, trail, hard_sl);
        if price_fp <= protect {
            // Distinguish the hard stop (at/below entry−hard_sl) from the trail.
            let hard_level =
                protection_level_fp(pos.entry_price_fp, pos.entry_price_fp, 0, hard_sl);
            let reason = if price_fp <= hard_level {
                ExitReason::HardStop
            } else {
                ExitReason::TrailingStop
            };
            return Some(self.close(mint, mult, reason));
        }

        // §24(d) LAW 5 exit-into-strength: an authentic buy-side climax while in
        // profit — sell the remainder INTO the buyers (harvest strength rather than
        // wait for exhaustion). Terminal; ranks below the protective stops above.
        if climax {
            return Some(self.close(mint, mult, ExitReason::IntoStrength));
        }

        // P2 thesis-invalidation: CVD rolled over, or a stall while in profit.
        let cvd_dead = pos.cvd_peak > 0
            && pos.cvd < pos.cvd_peak.saturating_mul(i128::from(p.cvd_hold_frac_bps)) / 10_000;
        let stall_window = if pos.pressure {
            (p.stall_ticks / 2).max(1)
        } else {
            p.stall_ticks
        };
        let stalled = mult > 10_000 && tick.saturating_sub(pos.last_high_tick) >= stall_window;
        if cvd_dead || stalled {
            return Some(self.close(mint, mult, ExitReason::ThesisInvalidation));
        }

        // P3 principal-recovery ladder (partial tranches; position stays open).
        // §24 LAW 2: when a per-market DERIVED ladder is armed, its cost-derived
        // tp1/tp2/tp3 multiples and cost-priced rung COUNT replace the fixed
        // constants; tranches beyond the rung count are disabled. Off = the fixed
        // 3-rung ladder, byte-identical to prior behaviour.
        let (t1, t2, t3, max_rungs) = match pos.derived {
            Some(d) => (d.tp1_bps, d.tp2_bps, d.tp3_bps, d.rungs),
            None => (p.tp1_bps, p.tp2_bps, p.tp3_bps, 3u8),
        };
        if max_rungs >= 1 && mult >= t1 && (pos.tranche_mask & 0b001) == 0 {
            // Sell the fraction that recovers principal+cost at this multiple.
            let recover_frac = ((u128::from(pos.cost_lamports) * 10_000)
                / (u128::from(pos.size_lamports).max(1) * u128::from(mult) / 10_000).max(1))
                as u32;
            let frac = recover_frac.min(pos.remaining_bps);
            pos.tranche_mask |= 0b001;
            let net = pos.realize(frac, mult, &p);
            let (mfe_bps, mae_bps) = pos.excursions_bps();
            return Some(Exit {
                mint: *mint,
                net_lamports: net,
                reason: ExitReason::TakeProfitLadder,
                closed: pos.remaining_bps == 0,
                mfe_bps,
                mae_bps,
            });
        }
        if max_rungs >= 2 && mult >= t2 && (pos.tranche_mask & 0b010) == 0 {
            pos.tranche_mask |= 0b010;
            let net = pos.realize(p.tp2_frac_bps, mult, &p);
            let (mfe_bps, mae_bps) = pos.excursions_bps();
            return Some(Exit {
                mint: *mint,
                net_lamports: net,
                reason: ExitReason::TakeProfitLadder,
                closed: pos.remaining_bps == 0,
                mfe_bps,
                mae_bps,
            });
        }
        if max_rungs >= 3 && mult >= t3 && (pos.tranche_mask & 0b100) == 0 {
            pos.tranche_mask |= 0b100;
            let net = pos.realize(p.tp3_frac_bps, mult, &p);
            let (mfe_bps, mae_bps) = pos.excursions_bps();
            return Some(Exit {
                mint: *mint,
                net_lamports: net,
                reason: ExitReason::TakeProfitLadder,
                closed: pos.remaining_bps == 0,
                mfe_bps,
                mae_bps,
            });
        }

        None
    }

    /// UPDATE on a logical tick: the conditional time-stop for every open position
    /// that is not advancing. Returns the exits that fired (closing those positions).
    pub fn on_tick(
        &mut self,
        tick: u64,
        latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>,
    ) -> Vec<Exit> {
        let p = self.params;
        // Reused scratch (O5): identical contents/order to the old per-tick `Vec`
        // (BTreeMap iteration order), so exits fire in the same order — digest-safe.
        let mut fired = std::mem::take(&mut self.fired_buf);
        fired.clear();
        for (mint, pos) in self.open.iter() {
            let not_advancing = tick.saturating_sub(pos.last_high_tick) >= p.stall_ticks;
            let aged = tick.saturating_sub(pos.entry_tick) >= p.max_hold_ticks;
            if not_advancing && aged {
                fired.push(*mint);
            }
        }
        let mut out = Vec::with_capacity(fired.len());
        for mint in fired.drain(..) {
            // §38: a position whose mark is UNKNOWN may never be valued at an
            // assumed-flat price — the predeclared terminal rule values it at
            // the hard-stop distance below entry (conservative, fail-closed).
            let mult = latest_price_fp(&mint)
                .map(|pr| self.open[&mint].mult_bps(pr))
                .unwrap_or(10_000_u32.saturating_sub(p.hard_sl_bps));
            out.push(self.close(&mint, mult, ExitReason::TimeStop));
        }
        self.fired_buf = fired;
        out
    }

    /// Force-exit one held position at the given mark (fixed-point price), booking
    /// it under `reason` — the engine's escalation seam (e.g. the VPIN extreme
    /// sell-dominant tier: a distributed multi-swap dump the single-print
    /// rug-precursor cannot see). `None` when no position is held on `mint`.
    pub fn close_at(&mut self, mint: &[u8; 32], price_fp: u64, reason: ExitReason) -> Option<Exit> {
        if !self.open.contains_key(mint) {
            return None;
        }
        let mult = self.open[mint].mult_bps(price_fp);
        Some(self.close(mint, mult, reason))
    }

    /// Force-close every remaining open position at its last-known multiple (end of
    /// run). Deterministic BTreeMap order.
    pub fn force_close_all(
        &mut self,
        latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>,
    ) -> Vec<Exit> {
        let mints: Vec<[u8; 32]> = self.open.keys().copied().collect();
        let mut out = Vec::with_capacity(mints.len());
        let sl = self.params.hard_sl_bps;
        for mint in mints {
            // §38 terminal rule: unknown mark ⇒ hard-stop-distance valuation,
            // never assumed break-even (see the time-stop path above).
            let mult = latest_price_fp(&mint)
                .map(|pr| self.open[&mint].mult_bps(pr))
                .unwrap_or(10_000_u32.saturating_sub(sl));
            out.push(self.close(&mint, mult, ExitReason::ForceClose));
        }
        out
    }

    /// Realize the entire remaining position at `mult_bps` and remove it.
    fn close(&mut self, mint: &[u8; 32], mult_bps: u32, reason: ExitReason) -> Exit {
        let mut pos = self.open.remove(mint).expect("close on an open position"); // LINT-ALLOW(hot_panic): infallible — every caller (on_tick/close_at/force_close_all) checks open membership before close()
        let (mfe_bps, mae_bps) = pos.excursions_bps();
        let net = pos.realize(pos.remaining_bps, mult_bps, &self.params);
        Exit {
            mint: *mint,
            net_lamports: net,
            reason,
            closed: true,
            mfe_bps,
            mae_bps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: LifecycleParams = LifecycleParams::standard();
    /// Real pump.fun virtual-SOL depth at launch (~30 SOL). **Amendment A-13(1):** every
    /// fixture that prices an order declares the depth it is walking, so the participation
    /// rate `clip / vsol` is visible rather than assumed. These unit tests run under
    /// `LifecycleParams::standard()`, which ships `curve_exact_fill = false`, so this value
    /// is decision-INERT here; it is stated so that arming the fill in a unit test charges a
    /// realistic 33 bps rather than the 8,333 bps the old golden tape silently charged nobody.
    const TEST_LIQ_LAMPORTS: u64 = 30_000_000_000;

    fn open_one(size: u64, entry: u64) -> ScalpLifecycle {
        let mut lc = ScalpLifecycle::new(P, 64);
        lc.open([1u8; 32], entry, size, size + P.tip_lamports, 0);
        lc
    }

    #[test]
    fn trailing_harvests_a_runner() {
        // Price ramps to 4× then falls back: the trail should close the remainder
        // well above entry, and the ladder should have banked tranches on the way.
        let mut lc = open_one(1_000_000, 1_000_000);
        let mut total: i128 = 0;
        let mut closed = false;
        // ramp up: 1x -> 4x over rising prices with buy flow
        for (i, m) in [12_000u64, 14_000, 20_000, 28_000, 40_000, 30_000]
            .iter()
            .enumerate()
        {
            let price = 1_000_000 * m / 10_000;
            if let Some(e) = lc.on_trade(&[1u8; 32], price, 500_000, i as u64 + 1, TEST_LIQ_LAMPORTS) {
                total += e.net_lamports;
                closed = e.closed;
            }
        }
        assert!(
            total > 0,
            "a 4x runner nets positive after trailing out ({total})"
        );
        assert!(closed || !lc.has(&[1u8; 32]), "runner eventually closed");
    }

    #[test]
    fn rug_precursor_dumps_early() {
        let mut lc = open_one(1_000_000, 1_000_000);
        // one up tick, then a −40% single-swap collapse
        lc.on_trade(&[1u8; 32], 1_100_000, 500_000, 1, TEST_LIQ_LAMPORTS);
        let e = lc
            .on_trade(&[1u8; 32], 660_000, -900_000, 2, TEST_LIQ_LAMPORTS)
            .expect("precursor fires");
        assert_eq!(e.reason, ExitReason::RugPrecursor);
        assert!(e.closed);
        assert!(!lc.has(&[1u8; 32]), "position closed on the precursor");
    }

    #[test]
    fn hard_stop_backstops_a_bleed() {
        let mut lc = open_one(1_000_000, 1_000_000);
        // gentle bleed to −40% in small steps (no single-step precursor)
        let mut last = None;
        for (i, m) in [9_500u64, 9_000, 8_500, 8_000, 7_000, 6_000]
            .iter()
            .enumerate()
        {
            let price = 1_000_000 * m / 10_000;
            last = lc
                .on_trade(&[1u8; 32], price, -100_000, i as u64 + 1, TEST_LIQ_LAMPORTS)
                .or(last);
        }
        let e = last.expect("a stop fired");
        assert!(matches!(
            e.reason,
            ExitReason::HardStop | ExitReason::TrailingStop
        ));
        assert!(
            e.net_lamports < 0,
            "a bleed realizes a loss, but a bounded one"
        );
        // bounded: never worse than ~ −(hard_sl + costs) of size
        assert!(
            e.net_lamports > -600_000,
            "loss is bounded by the hard stop"
        );
    }

    #[test]
    fn thesis_invalidation_on_cvd_rollover() {
        let mut lc = open_one(1_000_000, 1_000_000);
        // build CVD peak with buys at a small profit, then CVD rolls over on sells
        lc.on_trade(&[1u8; 32], 1_050_000, 5_000_000, 1, TEST_LIQ_LAMPORTS);
        lc.on_trade(&[1u8; 32], 1_060_000, 5_000_000, 2, TEST_LIQ_LAMPORTS);
        let e = lc
            .on_trade(&[1u8; 32], 1_055_000, -9_000_000, 3, TEST_LIQ_LAMPORTS)
            .expect("thesis fires on flow rollover");
        assert_eq!(e.reason, ExitReason::ThesisInvalidation);
        assert!(e.closed);
    }

    #[test]
    fn determinism_same_stream_same_result() {
        let run = || {
            let mut lc = open_one(1_000_000, 1_000_000);
            let mut acc: i128 = 0;
            for (i, (m, q)) in [
                (12_000u64, 400_000i128),
                (18_000, 400_000),
                (9_000, -800_000),
            ]
            .iter()
            .enumerate()
            {
                let price = 1_000_000 * m / 10_000;
                if let Some(e) = lc.on_trade(&[1u8; 32], price, *q, i as u64 + 1, TEST_LIQ_LAMPORTS) {
                    acc += e.net_lamports;
                }
            }
            acc
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn empty_manager_books_nothing() {
        let mut lc = ScalpLifecycle::new(P, 64);
        assert!(lc.is_empty());
        assert!(lc.on_trade(&[9u8; 32], 1_000_000, 1, 1, TEST_LIQ_LAMPORTS).is_none());
        assert!(lc.force_close_all(&|_| None).is_empty());
    }
}
