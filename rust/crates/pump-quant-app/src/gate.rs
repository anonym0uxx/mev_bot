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
//! The size-band's three COST inputs — impact denominator, protocol bps and base
//! fixed lamports — are no longer config constants. They are derived per candidate
//! from the market's own SOL-side reserve by [`crate::cost_model`], the single
//! authority on what a round trip costs, so the gate prices the market in front of it
//! rather than a market the operator once configured. Everything else the size-band
//! consults (expected move, fail rate, margin, operator floor) still comes from
//! [`crate::config::Config`].

use crate::config::Config;
use crate::curve_depth::CurveDepth;
use crate::priced_move::PricedMove;
use pump_quant_strategy::economic_gate::{
    effective_fixed_lamports, floor_size_band, round_trip_cost_bps, size_band, ImpactCurve,
    SizeBand, Verdict,
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
    /// **Re-pin #29 — TP1 REACHABILITY.** The calibrated model estimated a realistic
    /// upside that cannot reach TP1 (`lc_tp1_bps`) after round-trip costs. Entering
    /// such a candidate would mean TP1 never fires, leaving the position to rely
    /// entirely on the hard stop or trailing exit — suboptimal for non-moonshot
    /// tokens. This check fires ONLY when the model has spoken (MoveSource::Model);
    /// cold-start candidates are still admitted for evidence-gathering so the model
    /// can calibrate. Without this gate, the bot enters trades where the cost-aware
    /// TP ladder has no room to operate (ArXiv:2606.08232 fat-tail capture design).
    Tp1Unreachable,
    /// §Quant-Rev-7 — RE-ENTRY COOLDOWN. The mint was recently exited (position
    /// closed within `reentry_cooldown_ticks` ticks ago) and is still in cooldown.
    /// Re-entering the same mint in a tight loop after thesis invalidation causes
    /// death-by-a-thousand-cuts: each cycle bleeds ~0.0028 SOL in slippage with no
    /// new information. The cooldown forces the bot to wait for fresh price action
    /// before re-engaging, breaking the re-entry loop that accounted for 56% of
    /// total paper losses. This is a SELECTION refusal, not an economic one — the
    /// trade may be perfectly viable, the mint is simply on temporary blackout.
    /// Cannot fire in the golden tape (no position ever closes → set never populated).
    ReentryCooldown,
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
    /// The market's SOL-side depth **and where it came from** — the price reserve that
    /// sets impact and the payout reserve that caps capacity, carried together so they
    /// cannot be confused (`docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md`).
    ///
    /// Replaces the bare `sellable_depth_lamports`, which had three producers with
    /// three different meanings — an external assertion, a copy of the VIRTUAL
    /// reserve, and a hardcoded 0.2 SOL — and no way to tell them apart.
    pub depth: CurveDepth,
    /// The numeric feature snapshot from the on-chain flow lane.
    pub numeric: Features,
}

/// Decide one candidate.
///
/// `confirmation` is `Some` only when an `OnchainConfirm` has been recorded for the
/// candidate's mint *and* the numeric lane holds a feature snapshot for it. The two
/// together are the on-chain truth requirement; either missing is a hard refuse.
#[must_use]
/// `priced_move` is the ONE expected-move estimate for this candidate
/// ([`crate::priced_move::PricedMove`]), computed once by [`crate::engine`] and handed
/// to BOTH this function and §23 arbitration. It carries its own provenance — the
/// calibrated model, the lane's realized evidence, or the cold-start constant — so
/// "what did we think this was worth, and who told us" is a journalled fact rather
/// than an unanswerable question (`docs/EDGE_PROVENANCE_2026-07-27.md §4`).
///
/// Admission consumes [`PricedMove::admission_bps`] — the POPULATION view (the
/// calibrated model when it has spoken, else the cold-start prior). §23 arbitration
/// consumes the same object's ranking view. That asymmetry is deliberate and is
/// argued, with the measurement behind it, in [`crate::priced_move`].
pub fn decide(
    _candidate: &Candidate,
    confirmation: Option<Confirmation>,
    cfg: &Config,
    priced_move: PricedMove,
) -> GateDecision {
    let Some(conf) = confirmation else {
        return GateDecision::Reject(GateReject::NeedsOnchainConfirmation);
    };
    // DEPTH, FROM ONE AUTHORITY. `price_reserve` is what our own order costs against;
    // `payout_reserve` is what a seller can actually receive. On a bonding curve these
    // differ by the 30 SOL seed — the second is `None` exactly when the first is, and
    // an `Unknown` basis (undecoded pool, impossible reserve, or a decoded pair that
    // contradicts the venue's own arithmetic) refuses BOTH rather than fabricating a
    // zero that would still size (§18.2).
    let (Some(vsol), Some(payout)) = (conf.depth.price_reserve(), conf.depth.payout_reserve())
    else {
        return GateDecision::Reject(GateReject::NeedsOnchainConfirmation);
    };
    // NOTE on the retired `sellable_depth_lamports > 0` guard. A curve at exactly its
    // seed reserve escrows NOTHING — nobody has bought into it — and that used to be
    // refused here as `NeedsOnchainConfirmation`. It is not a corroboration failure:
    // the market IS confirmed, it simply has no capacity, and `size_band` refuses it
    // on `x_max == 0` a few lines below as `EconomicallyUnviable`. Depth that has not
    // been PROVEN (an `Unknown` basis, refused just above) and depth that does not
    // EXIST are different facts and now carry different reject codes.
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
            vsol,
            u128::from(cfg.mcap_band_lo_lamports),
            u128::from(cfg.mcap_band_hi_lamports),
        )
    {
        return GateDecision::Reject(GateReject::OutsideMcapBand);
    }

    // ---- COST INPUTS, DERIVED PER CANDIDATE (2026-07-28 cost-model unification).
    //
    // `economic_gate::size_band` lives in `pump-quant-strategy`, which this crate
    // DEPENDS ON, so it cannot call `cost_model` without a dependency cycle. The
    // resolution is not to move a crate: it is that the caller — this function, which
    // can see both — derives the three cost inputs from the market in front of it and
    // hands them down. `size_band` stays the pure arithmetic leaf it always was; the
    // economics stop being config constants.
    //
    // 1. IMPACT. `linear_test(vsol / 10_000)` makes the gate's linear impact model
    //    EXACTLY `curve_fill::own_impact_bps` for THIS market (`cost_model::
    //    impact_den_for`). The old `cfg.gate_impact_den` was a single number standing
    //    in for every pool on the venue; it was 250_000 (a 0.025 SOL pool) while the
    //    tape priced 30-67 SOL pools, then 3_000_000 (a 30 SOL pool) while the tape
    //    priced up to 67. A static denominator can only ever be right for one depth.
    // 2. PROTOCOL. Two legs of the venue's own fee and NOTHING ELSE. This removes the
    //    phantom 200 bps of "bid/ask spread" the golden tape's 450 carried: a
    //    constant-product AMM has one reserve ratio and one price, and the cost of
    //    crossing size is own impact — already charged, on both legs, just above.
    //    The rate is market-cap tiered, so it too is per-candidate.
    // 3. FIXED. `FIXED_LAMPORTS_PER_LEG · (1 + exit_tranches)` plus the one signature
    //    that reclaims the ATA deposit. See `cost_model::gate_base_fixed_lamports`.
    //
    // 4. CAPACITY. `x_max`'s sellability bound is the PAYOUT reserve, not the price
    //    reserve. On a bonding curve `real_sol = virtual_sol − 30 SOL`, so a fixture
    //    or decoder that hands the price reserve to this argument overstates capacity
    //    by 30x at `vsol = 31 SOL` and without bound at the seed reserve, where a
    //    curve nobody has bought into can pay out nothing at all. This argument is
    //    where that overstatement dies.
    let impact = ImpactCurve::linear_test(crate::cost_model::impact_den_for(vsol));
    let band = size_band(
        priced_move.admission_bps(),
        crate::cost_model::gate_base_fixed_lamports(cfg.gate_exit_tranches),
        cfg.gate_fail_rate_bps,
        crate::cost_model::gate_protocol_bps(vsol),
        cfg.gate_margin_bps,
        vsol,
        &impact,
        payout,
    );

    // Criterion 112 / A-6 operator floor: lift the band's lower edge to the absolute
    // minimum trade size so the effective x_min is `max(min_trade_size, x_min)`. A
    // market too thin to absorb a floor-sized clip (`effective x_min > x_max`)
    // collapses to `Refuse` here — the engine never emits a sub-floor order and never
    // exceeds x_max. Applied above the dossier-locked economic leaf.
    let band = floor_size_band(band, cfg.min_trade_size_lamports);

    // ---- RE-PIN #29 — TP1 REACHABILITY (ArXiv:2606.08232 fat-tail capture design).
    //
    // The cost-aware TP ladder's TP1 sits at +10% (11_000 bps). If the calibrated
    // model estimates a realistic upside that can't reach TP1 after round-trip costs,
    // admitting this candidate means TP1 never fires — the position would rely
    // entirely on the hard stop or trailing exit. That is suboptimal for non-moonshot
    // tokens and defeats the ladder's purpose: the ladder is designed to lock profit
    // early on fat-tail moonshots, not to hold a position to the hard stop.
    //
    // This check fires ONLY when the calibrated model has spoken (MoveSource::Model).
    // Cold-start candidates (MoveSource::ColdStart) are still admitted because:
    //   1. The model needs paper trades to calibrate — refusing all cold-start
    //      candidates would starve it of evidence forever.
    //   2. The cold-start prior (gate_expected_move_bps = 3_400) is a POPULATION
    //      estimate, not a per-candidate estimate; it has no opinion on whether THIS
    //      token can reach TP1.
    //
    // The round-trip cost is evaluated at x_cost (the cost-minimizing size), which is
    // the BEST possible round-trip cost for this market — if the model's estimated
    // upside can't reach TP1 even at the minimum cost, no size will make TP1 reachable.
    if let Verdict::Admit = band.verdict {
        if let crate::priced_move::MoveSource::Model { .. } = priced_move.admission_source() {
            if let Some(eff) = effective_fixed_lamports(
                crate::cost_model::gate_base_fixed_lamports(cfg.gate_exit_tranches),
                cfg.gate_fail_rate_bps,
            ) {
                if let Some(rt_bps) = round_trip_cost_bps(
                    band.x_cost,
                    eff,
                    crate::cost_model::gate_protocol_bps(vsol),
                    &impact,
                ) {
                    let tp1_with_cost = cfg.lc_tp1_bps.saturating_add(rt_bps);
                    if priced_move.admission_bps() < tp1_with_cost {
                        return GateDecision::Reject(GateReject::Tp1Unreachable);
                    }
                }
            }
        }
    }

    match band.verdict {
        Verdict::Admit => GateDecision::Admit(band),
        Verdict::Refuse => GateDecision::Reject(GateReject::EconomicallyUnviable),
    }
}
