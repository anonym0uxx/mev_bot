//! The entry gate: corroboration discipline + economic viability.
//!
//! A candidate reaching the top of the watchlist is a *hypothesis*, not an order.
//! Two independent hurdles stand between it and capital, and both are hard:
//!
//! 1. **On-chain corroboration (§29, §71).** Entry is authorised only when the
//!    market has an on-chain confirmation *and* real numeric microstructure. Social,
//!    narrative and wallet lanes can push a mint to the top of the watchlist, but
//!    their evidence alone is never sufficient — without a numeric feature snapshot
//!    and an [`crate::event::AppEvent::OnchainConfirm`], the gate refuses. This is
//!    the fade-first, corroboration-tier rule made mechanical.
//!
//! 2. **Economic viability (§18).** Given confirmed depth, the real
//!    `economic_gate::size_band` leaf computes the viable size band `[x_min, x_cost,
//!    x_max]` net of fees, tips, expected failure and a safety margin. A band that
//!    collapses (`Refuse`) means there is no size at which the edge survives its own
//!    costs, and the candidate is dropped.
//!
//! No parameter here is hard-coded: every number the size-band consults comes from
//! [`crate::config::Config`].

use crate::config::Config;
use pump_quant_strategy::economic_gate::{size_band, ImpactCurve, SizeBand, Verdict};
use pump_quant_watchlist::candidate::{Candidate, Features};

/// Why the gate refused a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateReject {
    /// No on-chain confirmation has been seen for this market. Corroboration lanes
    /// cannot substitute for it (§29, §71).
    NeedsOnchainConfirmation,
    /// The market has no numeric microstructure snapshot yet, so nothing verifiable
    /// backs the corroboration. Entry on corroboration alone is prohibited.
    NoNumericConfirmation,
    /// The economic size-band collapsed: no size clears costs with margin (§18).
    EconomicallyUnviable,
}

/// The gate's verdict on one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateDecision {
    /// Admit at the given size band; the scalp stage sizes within `[x_min, x_max]`.
    Admit(SizeBand),
    /// Refuse, with the binding reason.
    Reject(GateReject),
}

/// The on-chain facts the gate needs about a market to decide it.
#[derive(Clone, Copy, Debug)]
pub struct Confirmation {
    /// Sellable depth proven on-chain, lamports (0 = unproven).
    pub sellable_depth_lamports: u64,
    /// The numeric feature snapshot from the on-chain flow lane.
    pub numeric: Features,
}

/// Decide one candidate.
///
/// `confirmation` is `Some` only when an `OnchainConfirm` has been recorded for the
/// candidate's mint *and* the numeric lane holds a feature snapshot for it. The two
/// together are the on-chain truth requirement; either missing is a hard refuse.
#[must_use]
pub fn decide(
    _candidate: &Candidate,
    confirmation: Option<Confirmation>,
    cfg: &Config,
) -> GateDecision {
    let conf = match confirmation {
        Some(c) if c.sellable_depth_lamports > 0 => c,
        Some(_) => return GateDecision::Reject(GateReject::NeedsOnchainConfirmation),
        None => return GateDecision::Reject(GateReject::NeedsOnchainConfirmation),
    };
    if conf.numeric.liquidity_lamports == 0 {
        return GateDecision::Reject(GateReject::NoNumericConfirmation);
    }

    let impact = ImpactCurve::linear_test(cfg.gate_impact_den);
    let band = size_band(
        cfg.gate_expected_move_bps,
        cfg.gate_base_fixed_lamports,
        cfg.gate_fail_rate_bps,
        cfg.gate_protocol_bps,
        cfg.gate_margin_bps,
        conf.numeric.liquidity_lamports,
        &impact,
        conf.sellable_depth_lamports,
    );

    match band.verdict {
        Verdict::Admit => GateDecision::Admit(band),
        Verdict::Refuse => GateDecision::Reject(GateReject::EconomicallyUnviable),
    }
}
