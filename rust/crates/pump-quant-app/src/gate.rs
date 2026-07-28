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
use pump_quant_strategy::economic_gate::{
    floor_size_band, size_band, ImpactCurve, SizeBand, Verdict,
};
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
    /// The market's bonding-curve market cap sits outside the operator's target band
    /// (`mcap_band_*`). A SELECTION refusal, not an economic one: the trade may be
    /// perfectly viable, it is simply not the population this bot is aimed at.
    /// Distinguished from `EconomicallyUnviable` so band tuning never contaminates the
    /// cost-floor reject statistics.
    OutsideMcapBand,
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
/// `expected_move_bps_override` is the per-candidate conditional estimate from
/// [`crate::expected_move`] when the model is armed AND its stratum cleared the sample
/// floor. `None` means "the estimator refused" and the configured cold-start constant
/// is used instead — the shipped path, byte-identical to the pre-model engine. The
/// fallback is an explicit parameter rather than a default inside this function so that
/// "we priced this on the constant" is visible at the call site and journallable
/// (`docs/EDGE_PROVENANCE_2026-07-27.md §4`).
pub fn decide(
    _candidate: &Candidate,
    confirmation: Option<Confirmation>,
    cfg: &Config,
    expected_move_bps_override: Option<u32>,
) -> GateDecision {
    let conf = match confirmation {
        Some(c) if c.sellable_depth_lamports > 0 => c,
        Some(_) => return GateDecision::Reject(GateReject::NeedsOnchainConfirmation),
        None => return GateDecision::Reject(GateReject::NeedsOnchainConfirmation),
    };
    if conf.numeric.liquidity_lamports == 0 {
        return GateDecision::Reject(GateReject::NoNumericConfirmation);
    }

    // OPERATOR TARGET BAND (default OFF). On a constant-product curve with virtual
    // reserves the market cap is `vsol^2 / MCAP_DIVISOR` — the token side cancels, so
    // `liquidity_lamports` alone prices it with no oracle and no extra decode
    // (`curve_state`). Selection is applied BEFORE the economic band so that an
    // out-of-band market never consumes gate work or pollutes the cost statistics.
    if cfg.mcap_band_enable
        && !crate::curve_state::mcap_in_band(
            conf.numeric.liquidity_lamports,
            u128::from(cfg.mcap_band_lo_lamports),
            u128::from(cfg.mcap_band_hi_lamports),
        )
    {
        return GateDecision::Reject(GateReject::OutsideMcapBand);
    }

    let impact = ImpactCurve::linear_test(cfg.gate_impact_den);
    let band = size_band(
        expected_move_bps_override.unwrap_or(cfg.gate_expected_move_bps),
        cfg.gate_base_fixed_lamports,
        cfg.gate_fail_rate_bps,
        cfg.gate_protocol_bps,
        cfg.gate_margin_bps,
        conf.numeric.liquidity_lamports,
        &impact,
        conf.sellable_depth_lamports,
    );

    // Criterion 112 / A-6 operator floor: lift the band's lower edge to the absolute
    // minimum trade size so the effective x_min is `max(min_trade_size, x_min)`. A
    // market too thin to absorb a floor-sized clip (`effective x_min > x_max`)
    // collapses to `Refuse` here — the engine never emits a sub-floor order and never
    // exceeds x_max. Applied above the dossier-locked economic leaf.
    let band = floor_size_band(band, cfg.min_trade_size_lamports);

    match band.verdict {
        Verdict::Admit => GateDecision::Admit(band),
        Verdict::Refuse => GateDecision::Reject(GateReject::EconomicallyUnviable),
    }
}
