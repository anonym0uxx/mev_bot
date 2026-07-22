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

use pump_quant_strategy::exit_ladder::protection_level_fp;
use std::collections::BTreeMap;

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
        }
    }
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
        gross -= gross * u128::from(p.exit_impair_bps.min(10_000)) / 10_000;
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
}

impl ScalpLifecycle {
    /// A fresh manager under `params`, holding at most `cap` concurrent positions.
    #[must_use]
    pub fn new(params: LifecycleParams, cap: usize) -> Self {
        Self {
            open: BTreeMap::new(),
            params,
            cap: cap.max(1),
        }
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
            },
        );
        true
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
    pub fn scale_in(&mut self, mint: &[u8; 32], add_lamports: u64, add_cost_lamports: u64) -> bool {
        let Some(pos) = self.open.get_mut(mint) else {
            return false;
        };
        if pos.scaled || pos.tranche_mask != 0 || pos.remaining_bps != 10_000 {
            return false;
        }
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
    ) -> Option<Exit> {
        let p = self.params;
        let pos = self.open.get_mut(mint)?;
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
        let trail = pos.trail_bps(&p);
        let protect =
            protection_level_fp(pos.peak_price_fp, pos.entry_price_fp, trail, p.hard_sl_bps);
        if price_fp <= protect {
            // Distinguish the hard stop (at/below entry−hard_sl) from the trail.
            let hard_level =
                protection_level_fp(pos.entry_price_fp, pos.entry_price_fp, 0, p.hard_sl_bps);
            let reason = if price_fp <= hard_level {
                ExitReason::HardStop
            } else {
                ExitReason::TrailingStop
            };
            return Some(self.close(mint, mult, reason));
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
        if mult >= p.tp1_bps && (pos.tranche_mask & 0b001) == 0 {
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
        if mult >= p.tp2_bps && (pos.tranche_mask & 0b010) == 0 {
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
        if mult >= p.tp3_bps && (pos.tranche_mask & 0b100) == 0 {
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
        let mut fired: Vec<[u8; 32]> = Vec::new();
        for (mint, pos) in self.open.iter() {
            let not_advancing = tick.saturating_sub(pos.last_high_tick) >= p.stall_ticks;
            let aged = tick.saturating_sub(pos.entry_tick) >= p.max_hold_ticks;
            if not_advancing && aged {
                fired.push(*mint);
            }
        }
        let mut out = Vec::with_capacity(fired.len());
        for mint in fired {
            let mult = latest_price_fp(&mint)
                .map(|pr| self.open[&mint].mult_bps(pr))
                .unwrap_or(10_000);
            out.push(self.close(&mint, mult, ExitReason::TimeStop));
        }
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
        for mint in mints {
            let mult = latest_price_fp(&mint)
                .map(|pr| self.open[&mint].mult_bps(pr))
                .unwrap_or(10_000);
            out.push(self.close(&mint, mult, ExitReason::ForceClose));
        }
        out
    }

    /// Realize the entire remaining position at `mult_bps` and remove it.
    fn close(&mut self, mint: &[u8; 32], mult_bps: u32, reason: ExitReason) -> Exit {
        let mut pos = self.open.remove(mint).expect("close on an open position");
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
            if let Some(e) = lc.on_trade(&[1u8; 32], price, 500_000, i as u64 + 1) {
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
        lc.on_trade(&[1u8; 32], 1_100_000, 500_000, 1);
        let e = lc
            .on_trade(&[1u8; 32], 660_000, -900_000, 2)
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
                .on_trade(&[1u8; 32], price, -100_000, i as u64 + 1)
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
        lc.on_trade(&[1u8; 32], 1_050_000, 5_000_000, 1);
        lc.on_trade(&[1u8; 32], 1_060_000, 5_000_000, 2);
        let e = lc
            .on_trade(&[1u8; 32], 1_055_000, -9_000_000, 3)
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
                if let Some(e) = lc.on_trade(&[1u8; 32], price, *q, i as u64 + 1) {
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
        assert!(lc.on_trade(&[9u8; 32], 1_000_000, 1, 1).is_none());
        assert!(lc.force_close_all(&|_| None).is_empty());
    }
}
