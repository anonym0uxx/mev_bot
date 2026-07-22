//! Modes A / B / C fill models with exit-impairment.
//!
//! Responsibility: turn a recorded market observation plus a cost/impairment model
//! into an explicit success/failure fill and a net-SOL round-trip result
//! (constitution §38). Never fills at signal price, next candle, best price, or an
//! arbitrary percentage — every fill applies fees, tips, price impact, a recorded
//! price move, and (for the adversarial mode) exit impairment. All math is integer
//! lamports / basis points (§22).
//!
//! Mode semantics (§38):
//! * **A** — causal signal replay. No profitability claim (`claimable == false`);
//!   the model reconstructs the signal-side value only and applies no exit costs.
//! * **B** — deterministic chain-state execution with fixed assumptions; the
//!   *optimistic mechanical ceiling*. Fees and price impact apply, but no exit
//!   impairment and no terminal loss.
//! * **C** — calibrated adversarial execution: fees, impact, and the full exit
//!   impairment model at a chosen [`ImpairmentLevel`], including predeclared
//!   terminal-loss treatment for unexitable positions. Only Mode C may support
//!   movement toward a live probe.

use crate::fixed::{i128_to_u64_saturating, reduce_bps, scale_signed_bps, BPS_ONE};
use crate::terminal_loss::TerminalLossPolicy;

/// Adversarial severity for Mode C exit impairment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpairmentLevel {
    /// Impairment applied at its calibrated (1×) magnitude.
    Realistic,
    /// A stress level: impairment magnitudes and retry costs are doubled, used for
    /// conservative terminal-loss / worse-execution reporting (§54).
    Pessimistic,
}

impl ImpairmentLevel {
    /// Integer multiplier this level applies to calibrated impairment magnitudes.
    #[must_use]
    pub fn factor(self) -> u32 {
        match self {
            ImpairmentLevel::Realistic => 1,
            ImpairmentLevel::Pessimistic => 2,
        }
    }
}

/// Which of the three simulator modes to run (§38).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMode {
    /// Mode A — causal signal replay, no profitability claim.
    SignalReplay,
    /// Mode B — deterministic optimistic mechanical ceiling.
    OptimisticCeiling,
    /// Mode C — calibrated adversarial execution at the given severity.
    Adversarial(ImpairmentLevel),
}

/// Recorded market state for a single round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketState {
    /// Position size / SOL notional committed on entry, in lamports.
    pub notional_lamports: u64,
    /// Recorded price move over the hold, signed basis points (e.g. `+5000` = +50%,
    /// `-3000` = -30%). Sourced from the sealed replay journal, never assumed.
    pub move_bps: i32,
    /// SOL-side liquidity depth backing the venue, in lamports. Drives price impact.
    pub depth_lamports: u64,
    /// Impact scale: price impact in bps is `notional * impact_k_bps / depth`,
    /// capped at `BPS_ONE`. A `notional` equal to `depth` yields `impact_k_bps` bps.
    pub impact_k_bps: u32,
}

/// Explicit, itemized cost model for a round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Combined protocol/creator/LP/platform fee charged on entry, in bps.
    pub entry_fee_bps: u32,
    /// Combined fee charged on exit, in bps.
    pub exit_fee_bps: u32,
    /// Fixed lamport cost on entry (priority fee + tip).
    pub entry_tip_lamports: u64,
    /// Fixed lamport cost on exit (priority fee + tip).
    pub exit_tip_lamports: u64,
}

/// Exit-impairment model (Mode C only). All magnitudes are deterministic expected
/// haircuts — no sampling — so the simulation stays reproducible (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitImpairment {
    /// Deterministic expected haircut (bps) attributed to first-sell failure and
    /// the resulting delay before a successful retry.
    pub first_sell_penalty_bps: u32,
    /// Extra slippage (bps) suffered because the market moves against the position
    /// during retry ("collapse during retry").
    pub retry_slippage_bps: u32,
    /// Additional exit fee (bps) from fee escalation across retries.
    pub fee_escalation_bps: u32,
    /// Fixed extra lamport cost per impaired exit (retry tips / priority fees).
    pub retry_tip_lamports: u64,
    /// Whether the position is terminally unexitable. If set, Mode C values the
    /// position via the predeclared [`TerminalLossPolicy`], never at mark.
    pub unexitable: bool,
}

/// Outcome of a simulated round-trip fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillResult {
    /// The mode that produced this result.
    pub mode: FillMode,
    /// Whether the net PnL may be treated as a profitability claim. `false` for
    /// Mode A (signal replay) — the caller must not aggregate it as realized PnL.
    pub claimable: bool,
    /// Total lamports committed on entry (notional + entry tip).
    pub entry_cost_lamports: u64,
    /// SOL-equivalent value of the tokens actually held immediately after entry,
    /// i.e. notional net of entry fee and entry price impact.
    pub entry_value_lamports: u64,
    /// Realized exit proceeds in lamports; `i128` because exit tips can, in
    /// pathological cases, exceed gross proceeds and drive this negative.
    pub exit_proceeds_lamports: i128,
    /// Net round-trip PnL in lamports (`exit_proceeds - entry_cost`).
    pub net_pnl_lamports: i128,
    /// Entry-side price impact actually applied, in bps.
    pub entry_impact_bps: u32,
    /// Exit-side price impact actually applied, in bps (`0` for Mode A / terminal).
    pub exit_impact_bps: u32,
    /// Whether exit impairment was applied (Mode C, exitable path).
    pub impaired: bool,
    /// Whether the position was resolved as terminally unexitable.
    pub unexitable: bool,
}

/// Price impact in bps for `notional` traded against `depth` at scale `k`.
///
/// `notional * k / depth`, saturated to `BPS_ONE`. Zero depth means the trade
/// cannot be absorbed at all: full `BPS_ONE` impact by contract.
#[must_use]
pub fn impact_bps(notional: u64, depth: u64, k: u32) -> u32 {
    if depth == 0 {
        return BPS_ONE;
    }
    let raw = (notional as u128) * (k as u128) / (depth as u128);
    if raw > BPS_ONE as u128 {
        BPS_ONE
    } else {
        raw as u32
    }
}

/// Simulate a single round-trip fill under the given mode.
///
/// Flow (all integer, deterministic):
/// 1. entry: `notional` net of `entry_fee_bps` then entry impact ⇒ entry value;
/// 2. hold: entry value scaled by the recorded signed `move_bps` ⇒ mark;
/// 3. exit: mode-specific application of exit fee, exit impact, impairment, and
///    terminal-loss treatment ⇒ proceeds;
/// 4. `net_pnl = proceeds - entry_cost`.
///
/// `terminal` is consulted only on the Mode C unexitable path; it is ignored
/// otherwise but always accepted so the call site is mode-agnostic.
#[must_use]
pub fn simulate_fill(
    market: &MarketState,
    costs: &CostModel,
    imp: &ExitImpairment,
    mode: FillMode,
    terminal: &TerminalLossPolicy,
) -> FillResult {
    let notional = market.notional_lamports;
    let entry_impact = impact_bps(notional, market.depth_lamports, market.impact_k_bps);
    let after_entry_fee = reduce_bps(notional, costs.entry_fee_bps);
    let entry_value = reduce_bps(after_entry_fee, entry_impact);
    let entry_cost = notional.saturating_add(costs.entry_tip_lamports);

    // Mark: entry value carried forward by the recorded price move.
    let mark_i128 = scale_signed_bps(entry_value, market.move_bps);
    let mark = i128_to_u64_saturating(mark_i128);

    match mode {
        FillMode::SignalReplay => {
            // Mode A: signal-side value only, no exit costs, NOT a profitability claim.
            let proceeds = mark_i128;
            FillResult {
                mode,
                claimable: false,
                entry_cost_lamports: entry_cost,
                entry_value_lamports: entry_value,
                exit_proceeds_lamports: proceeds,
                net_pnl_lamports: proceeds - entry_cost as i128,
                entry_impact_bps: entry_impact,
                exit_impact_bps: 0,
                impaired: false,
                unexitable: false,
            }
        }
        FillMode::OptimisticCeiling => {
            // Mode B: fees + impact, no impairment, always sellable.
            let exit_impact = impact_bps(mark, market.depth_lamports, market.impact_k_bps);
            let after_exit_fee = reduce_bps(mark, costs.exit_fee_bps);
            let after_impact = reduce_bps(after_exit_fee, exit_impact);
            let proceeds = after_impact as i128 - costs.exit_tip_lamports as i128;
            FillResult {
                mode,
                claimable: true,
                entry_cost_lamports: entry_cost,
                entry_value_lamports: entry_value,
                exit_proceeds_lamports: proceeds,
                net_pnl_lamports: proceeds - entry_cost as i128,
                entry_impact_bps: entry_impact,
                exit_impact_bps: exit_impact,
                impaired: false,
                unexitable: false,
            }
        }
        FillMode::Adversarial(level) => {
            if imp.unexitable {
                // Terminal loss: value the BASIS via the predeclared policy, never mark.
                let terminal_value = terminal.terminal_value(entry_value);
                let proceeds = terminal_value as i128;
                return FillResult {
                    mode,
                    claimable: true,
                    entry_cost_lamports: entry_cost,
                    entry_value_lamports: entry_value,
                    exit_proceeds_lamports: proceeds,
                    net_pnl_lamports: proceeds - entry_cost as i128,
                    entry_impact_bps: entry_impact,
                    exit_impact_bps: 0,
                    impaired: true,
                    unexitable: true,
                };
            }
            let factor = level.factor();
            let exit_impact = impact_bps(mark, market.depth_lamports, market.impact_k_bps);
            let exit_fee_total = costs
                .exit_fee_bps
                .saturating_add(imp.fee_escalation_bps.saturating_mul(factor))
                .min(BPS_ONE);
            let impair_bps = imp
                .first_sell_penalty_bps
                .saturating_add(imp.retry_slippage_bps)
                .saturating_mul(factor)
                .min(BPS_ONE);
            let retry_tip_total = imp.retry_tip_lamports.saturating_mul(factor as u64);

            let after_exit_fee = reduce_bps(mark, exit_fee_total);
            let after_impact = reduce_bps(after_exit_fee, exit_impact);
            let after_impair = reduce_bps(after_impact, impair_bps);
            let proceeds =
                after_impair as i128 - costs.exit_tip_lamports as i128 - retry_tip_total as i128;
            FillResult {
                mode,
                claimable: true,
                entry_cost_lamports: entry_cost,
                entry_value_lamports: entry_value,
                exit_proceeds_lamports: proceeds,
                net_pnl_lamports: proceeds - entry_cost as i128,
                entry_impact_bps: entry_impact,
                exit_impact_bps: exit_impact,
                impaired: true,
                unexitable: false,
            }
        }
    }
}
