//! Leaf `ex_reconcile_fill`: on-chain fill reconciliation math.
//!
//! Ported from the legacy `momentum/reconciler.rs` `reconcile_pending`, which
//! compared logged (estimated) P&L against actual on-chain wallet SOL deltas and
//! classified each trade. The legacy code carried SOL amounts as `f64`; here
//! every amount is integer **lamports** so the reconciliation is exact and
//! constitution §22 compliant (no floats in an outcome-controlling path).
//!
//! ## Responsibility
//! Given the expected (logged) fill and the confirmed on-chain fill, compute the
//! realized net P&L in lamports, the discrepancy against the log, and the
//! reconciliation status.
//!
//! ## Legacy fidelity
//! - `onchain_pnl = sell_received - buy_spent` → [`ReconResult::realized_net_lamports`].
//! - `discrepancy = onchain_pnl - log_pnl` → [`ReconResult::discrepancy_lamports`].
//! - `|discrepancy| > tolerance` ⇒ `Discrepancy`, else `Reconciled`.
//! - Failure / stale short-circuits map to `BuyNotConfirmed` / `SellNotConfirmed`.
//!
//! ## Constitution refs
//! - §22: lamports as `u64` inputs, widened to `i128` for signed differences.
//! - Overflow: all arithmetic is done in `i128`, which cannot overflow for any
//!   pair of `u64` lamport magnitudes.

/// Minimum reconciliation granularity in lamports (criterion 102, §102).
///
/// This is the legacy fixed tolerance (`0.0001 SOL = 100_000 lamports`), now
/// demoted from a hard-coded passthrough to the **floor** of a bankroll-scaled
/// granularity: even a tiny bankroll never reconciles finer than this, because
/// sub-100k-lamport dust deltas are below the noise floor of fee/rent rounding.
pub const RECON_GRANULARITY_FLOOR_LAMPORTS: u64 = 100_000;

/// Maximum reconciliation granularity in lamports (criterion 102, §102).
///
/// Caps the tolerance so that a very large bankroll does not widen the
/// discrepancy band so far that a genuine mis-fill is masked. `0.05 SOL`.
pub const RECON_GRANULARITY_CEIL_LAMPORTS: u64 = 50_000_000;

/// Fraction of bankroll (in basis points) used as the reconciliation
/// granularity before the floor/ceiling clamp (criterion 102, §102).
///
/// `10 bps = 0.1%`: the reconciliation tolerance tracks position scale, so the
/// absolute discrepancy band grows with the capital at risk rather than staying
/// pinned to a fixed lamport count that is too coarse for small trades and too
/// tight for large ones.
pub const RECON_GRANULARITY_BPS: u64 = 10;

/// Deterministic, bankroll-derived reconciliation granularity in lamports
/// (criterion 102 defect #10).
///
/// Replaces the fixed legacy `100_000`-lamport tolerance with a value that
/// scales with `bankroll_lamports`: `bankroll * RECON_GRANULARITY_BPS / 10_000`,
/// clamped to `[RECON_GRANULARITY_FLOOR_LAMPORTS, RECON_GRANULARITY_CEIL_LAMPORTS]`.
///
/// Properties (see tests): monotonic non-decreasing in bankroll, floored, and
/// ceiled. Integer-only; the intermediate product is widened to `u128` so no
/// `u64` bankroll can overflow the multiply.
#[must_use]
pub fn recon_granularity_lamports(bankroll_lamports: u64) -> u64 {
    let raw = (u128::from(bankroll_lamports) * u128::from(RECON_GRANULARITY_BPS)) / 10_000;
    let raw = u64::try_from(raw).unwrap_or(u64::MAX);
    raw.clamp(
        RECON_GRANULARITY_FLOOR_LAMPORTS,
        RECON_GRANULARITY_CEIL_LAMPORTS,
    )
}

/// The logged (estimated) side of a trade, as recorded on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedFill {
    /// Logged net P&L in lamports (signed; may be negative for a loss).
    pub log_net_lamports: i128,
    /// Whether a buy signature was recorded for this trade.
    pub buy_recorded: bool,
    /// Whether a sell signature was recorded for this trade.
    pub sell_recorded: bool,
    /// Absolute discrepancy tolerance in lamports. `|discrepancy|` strictly
    /// greater than this flags a `Discrepancy`. Ported from the legacy
    /// `discrepancy_tolerance_sol` (0.0001 SOL = 100_000 lamports).
    pub tolerance_lamports: i128,
}

/// The confirmed on-chain side of a trade, distilled from `getTransaction`
/// wallet balance deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnchainFill {
    /// Buy TX confirmed on-chain.
    pub buy_confirmed: bool,
    /// Sell TX confirmed on-chain.
    pub sell_confirmed: bool,
    /// Lamports spent on the buy (magnitude of the wallet balance decrease).
    pub buy_spent_lamports: u64,
    /// Lamports received on the sell (magnitude of the wallet balance increase).
    pub sell_received_lamports: u64,
    /// Buy TX definitively failed on-chain (instruction error).
    pub buy_failed: bool,
    /// Sell TX definitively failed on-chain (instruction error).
    pub sell_failed: bool,
    /// The trade has exceeded the stale timeout without full confirmation.
    pub stale: bool,
}

/// Reconciliation status for a single trade. Mirrors the legacy
/// `ReconcileStatus` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconStatus {
    /// Still awaiting confirmation; no terminal decision yet.
    Pending,
    /// On-chain P&L matches the log within tolerance.
    Reconciled,
    /// On-chain P&L differs from the log by more than the tolerance.
    Discrepancy,
    /// The buy never confirmed (phantom trade).
    BuyNotConfirmed,
    /// The buy confirmed but the sell never did (token stuck).
    SellNotConfirmed,
}

/// Result of reconciling one trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconResult {
    /// Terminal (or `Pending`) reconciliation status.
    pub status: ReconStatus,
    /// Realized on-chain net P&L in lamports (`sell_received - buy_spent`).
    /// Zero unless both legs confirmed.
    pub realized_net_lamports: i128,
    /// Realized minus logged P&L, in lamports (`0` unless both legs confirmed).
    pub discrepancy_lamports: i128,
    /// Whether the buy leg is considered confirmed.
    pub buy_confirmed: bool,
    /// Whether the sell leg is considered confirmed.
    pub sell_confirmed: bool,
}

/// Reconcile a logged trade against its confirmed on-chain fill.
///
/// Decision order (faithful to the legacy `reconcile_pending` flow):
/// 1. A definitively failed buy ⇒ `BuyNotConfirmed`.
/// 2. A definitively failed sell ⇒ `SellNotConfirmed`.
/// 3. Stale + buy still unconfirmed ⇒ `BuyNotConfirmed` (phantom trade).
/// 4. Stale + sell still unconfirmed ⇒ `SellNotConfirmed` (stuck token).
/// 5. Both legs confirmed ⇒ compute realized P&L and compare to the log
///    (`Discrepancy` if `|realized - log| > tolerance`, else `Reconciled`).
/// 6. Otherwise ⇒ `Pending`.
pub fn reconcile_fill(expected: ExpectedFill, onchain: OnchainFill) -> ReconResult {
    // 1 & 2: definitive on-chain failures take precedence.
    if onchain.buy_failed {
        return terminal(
            ReconStatus::BuyNotConfirmed,
            onchain.buy_confirmed,
            onchain.sell_confirmed,
        );
    }
    if onchain.sell_failed {
        return terminal(
            ReconStatus::SellNotConfirmed,
            onchain.buy_confirmed,
            onchain.sell_confirmed,
        );
    }

    // 3 & 4: stale-timeout classification.
    if onchain.stale {
        if !onchain.buy_confirmed {
            return terminal(ReconStatus::BuyNotConfirmed, false, onchain.sell_confirmed);
        }
        if !onchain.sell_confirmed {
            return terminal(ReconStatus::SellNotConfirmed, true, false);
        }
    }

    // 5: both legs confirmed — compute realized P&L in lamports.
    if onchain.buy_confirmed && onchain.sell_confirmed {
        let realized =
            i128::from(onchain.sell_received_lamports) - i128::from(onchain.buy_spent_lamports);
        let discrepancy = realized - expected.log_net_lamports;
        let status = if discrepancy.abs() > expected.tolerance_lamports {
            ReconStatus::Discrepancy
        } else {
            ReconStatus::Reconciled
        };
        return ReconResult {
            status,
            realized_net_lamports: realized,
            discrepancy_lamports: discrepancy,
            buy_confirmed: true,
            sell_confirmed: true,
        };
    }

    // 6: nothing terminal yet.
    terminal(
        ReconStatus::Pending,
        onchain.buy_confirmed,
        onchain.sell_confirmed,
    )
}

/// Reconcile a logged trade using a **bankroll-derived** discrepancy tolerance
/// (criterion 102 defect #10).
///
/// Identical to [`reconcile_fill`] except the tolerance is not taken from
/// `expected.tolerance_lamports` (the legacy fixed `100_000` passthrough) but
/// computed from `bankroll_lamports` via [`recon_granularity_lamports`], so the
/// reconciliation granularity scales with the capital at risk. The supplied
/// `expected.tolerance_lamports` is ignored on this path.
///
/// This is the entry point the reconciler should use once a bankroll figure is
/// available; [`reconcile_fill`] is retained for callers that carry an explicit
/// tolerance.
#[must_use]
pub fn reconcile_fill_with_bankroll(
    expected: ExpectedFill,
    onchain: OnchainFill,
    bankroll_lamports: u64,
) -> ReconResult {
    let granularity = recon_granularity_lamports(bankroll_lamports);
    let expected = ExpectedFill {
        tolerance_lamports: i128::from(granularity),
        ..expected
    };
    reconcile_fill(expected, onchain)
}

/// Build a non-computed (zero P&L) terminal result.
#[inline]
fn terminal(status: ReconStatus, buy_confirmed: bool, sell_confirmed: bool) -> ReconResult {
    ReconResult {
        status,
        realized_net_lamports: 0,
        discrepancy_lamports: 0,
        buy_confirmed,
        sell_confirmed,
    }
}
