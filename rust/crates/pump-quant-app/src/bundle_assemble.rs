//! C1 — assembling the live decision bundle from the planes that already exist.
//!
//! WHAT THIS IS. The engine cannot serve Qwen until something turns the live state into the
//! `DecisionBundle` the proposal crate renders. Every block already has a producer: the as-of-t
//! state line ([`crate::state_ledger`]), the enrichment block ([`crate::enrichment`]), the flow
//! aggregates ([`crate::flow_feed`] → the reducer), the venue ([`crate::venue`]), and the curve/AMM
//! annotations (already typed in the proposal crate). This module is the join, and nothing else:
//! it does NOT derive facts of its own, so a block that cannot be produced honestly is a REFUSAL
//! rather than a fabricated field.
//!
//! THE REFUSALS ARE THE POINT. `StateSnapshot` carries three guards the corpus's builder learned
//! the hard way, and a bundle built over any of them is out-of-distribution input dressed as a
//! normal prompt:
//!
//! - `complete == false` — a window could not be computed from retained tape, so some field is not
//!   the corpus's number.
//! - `identity_known == false` — a print in the concentration window carried no trader, so
//!   `unique_traders` and the top-1/top-5 shares are wrong. (The `(mint, slot)` join is what makes
//!   this true; before it, the reserve-delta path sets `buyer_entity: 0` by design.)
//! - `token_leg_known == false` — a print carried no token leg, so it could not be cleared against
//!   the dust floor and the volume/return fields are not the corpus's.
//!
//! PRICE UNITS — and a correction the C5 parity harness forced (2026-09-20). `StateSnapshot`'s
//! field is NAMED `price_sol_per_raw` in the corpus, which is a misnomer: the value it holds is the
//! ratio of the tape's two legs, i.e. **lamports per raw token**, which is exactly the bundle's
//! unit. Multiplying by 1e9 (as this module first did, trusting the name) put every price 9 orders
//! out. The ledger field is now named `price_lamports_per_raw_token` and carries the corpus value
//! unconverted (see `state_ledger::PRICE_SCALE`'s note on the two real scales).
//!
//! The evidence is a real corpus row: the harness derived `0.9562185430525267` from the tape and the
//! corpus's own prompt stores `price_lamports_per_raw_token=0.9562185430525267` — identical digits.
//! So the mapping is IDENTITY, and the defect was the ledger field's name. Renaming it is tracked as a
//! C5 finding; the conversion is removed here and pinned by a test.

#![forbid(unsafe_code)]

use pump_quant_proposal::decision::{
    AmmState, CurveState, DecisionBundle, DevHistoryDecision, EnrichedCandidate,
};
use pump_quant_proposal::{render_decision, FlowState, PyNum};

use crate::enrichment::EnrichedSnapshot;
use crate::state_ledger::StateSnapshot;

// (No unit constant: the ledger's price is already lamports per raw token — see the module docs.)

/// Why a bundle could not be assembled. Every variant is a missing or untrustworthy INPUT — never a
/// decision, because this module resolves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssemblyRefusal {
    /// A window could not be computed from retained tape.
    TapeIncomplete,
    /// A concentration-window print carried no trader.
    IdentityUnknown,
    /// A print carried no token leg.
    TokenLegUnknown,
    /// The state line and the enrichment block disagree about whether this mint has any history.
    NoHistory,
    /// The SIZE OPTIONS depth is unpriceable, so the prompt would say `pool depth na` — a form that
    /// occurs 0 times in the 132,326 sized rows of the c12 corpus (F1).
    DepthUnknown,
}

impl AssemblyRefusal {
    /// Stable label for journals — never reword.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AssemblyRefusal::TapeIncomplete => "bundle_tape_incomplete",
            AssemblyRefusal::IdentityUnknown => "bundle_identity_unknown",
            AssemblyRefusal::TokenLegUnknown => "bundle_token_leg_unknown",
            AssemblyRefusal::NoHistory => "bundle_no_history",
            AssemblyRefusal::DepthUnknown => "bundle_depth_unknown",
        }
    }
}

/// Round exactly as the corpus's serializer does: `round(x, n)` in Python, i.e. ties-to-EVEN.
///
/// WHY THIS IS NOT PEDANTRY. The corpus renders these fields through `serialize.py`, which rounds
/// each one — `age_s` to 1 dp, `last_trade_age_s` to 2, and `ret_5s_bp` / `ret_30s_bp` /
/// `price_volatility_30s_bp` / `top1_trader_share` / `top5_trader_share` / `buyer_seller_ratio` to 5.
/// Rendering full precision puts `ret_5s_bp=5.4304577631370154` where the model trained on
/// `ret_5s_bp=5.43046`: the same value, a different token sequence, i.e. out-of-distribution input
/// from a *correct* number. Ties-to-even (not Rust's default `round`'s ties-away-from-zero) is what
/// Python does, and it is observable — 0.5 cases differ.
#[must_use]
pub fn py_round(x: f64, dp: u32) -> f64 {
    // ONE AUTHORITY. This used to be a second, subtly different implementation of the same idea
    // (`(x * 10^dp).round_ties_even() / 10^dp`), which reads an exactly-representable scaled product
    // as a tie and rounds the wrong way on values like 3.865 — see `fmt::round_half_even_nd`.
    pump_quant_proposal::fmt::round_half_even_nd(x, dp as usize)
}

/// Everything the bundle needs, supplied by the caller — this module derives nothing itself.
pub struct BundleInputs<'a> {
    /// The as-of-t state line.
    pub snapshot: &'a StateSnapshot,
    /// The as-of-t enrichment block.
    pub enriched: &'a EnrichedSnapshot,
    /// The clock both of the above were taken at.
    pub t_dec_ms: i64,
    /// Which venue the tape actually shows (the observation, not the sizing decision).
    pub venue: &'a str,
    /// `mcap_sol_at_t`, sourced from the curve/AMM plane — deliberately NOT derived from trades,
    /// which cannot see the supply. `None` renders as `na`, the corpus's own encoding.
    pub mcap_sol_at_t: Option<f64>,
    /// `mcap_source`: which plane the mcap came from (`curve` / `amm`).
    pub mcap_source: &'a str,
    /// The thirteen flow aggregates.
    pub flow: &'a FlowState,
    /// True when the clock had zero prior tape events: the flow line is the `no_prior_flow` marker
    /// rather than fabricated zeros.
    pub flow_no_prior: bool,
    /// The curve annotation.
    pub curve: CurveState,
    /// The AMM annotation.
    pub amm: AmmState,
    /// The creator-history block (its producer is separate; this module does not invent it).
    pub dev: DevHistoryDecision,
    /// Pool depth in SOL the SIZE OPTIONS cost was measured against, `None` when unpriceable.
    pub size_depth_sol: Option<f64>,
    /// Whether OUR next fill lands on the AMM.
    pub size_amm: bool,
}

/// Assemble the bundle, or refuse with the missing input's name.
pub fn assemble(inputs: &BundleInputs<'_>) -> Result<DecisionBundle, AssemblyRefusal> {
    let s = inputs.snapshot;
    // Guards first, in the order the corpus learned them, so the cause is the most upstream one.
    if !s.complete {
        return Err(AssemblyRefusal::TapeIncomplete);
    }
    if !s.identity_known {
        return Err(AssemblyRefusal::IdentityUnknown);
    }
    if !s.token_leg_known {
        return Err(AssemblyRefusal::TokenLegUnknown);
    }
    // A state line with no prior trades cannot be the same tape the enrichment block was reduced
    // from. Refused rather than rendered as a bundle whose blocks disagree.
    if s.n_prior_trades == 0 || inputs.enriched.n_buys_at_t + inputs.enriched.n_sells_at_t == 0 {
        return Err(AssemblyRefusal::NoHistory);
    }
    // F1 — THE VALUE-BRANCH GATE, FAIL CLOSED ON NEVER-TRAINED INPUTS.
    //
    // Measured on the c12 corpus (215,711 rows, train+validation+examination, 2026-09-21):
    //   curve_present=false            0        <- unreachable here: `venue != "unknown"` below,
    //   no_prior_flow=true             0           and `eligibility` refuses VenueUnknown
    //   price_lamports_per_raw_token=absent  0  <- `serve` has no price without prior trades, so
    //   evidence_status=partial        0           `eligibility` refuses FewPriorTrades first
    //   pool depth na                  0 (of 132,326 rows carrying SIZE OPTIONS)
    //   amm_reserves=absent    59,995  <- TRAINED: must NOT be refused
    //   curve_reserves=absent      680 <- TRAINED: must NOT be refused
    //
    // Three of F1's five states cannot reach this point at all (they are refused upstream by
    // `state_ledger::eligibility`), and the two reserve-absence forms are in-distribution, so
    // refusing them would cut most of the tradeable population. That leaves exactly one
    // live-reachable, never-trained input: an unpriceable SIZE OPTIONS depth. The model has never
    // been asked to size a clip without a depth, so it is refused rather than rendered as `na`.
    // PARTIAL EVIDENCE IS ACCEPTED BY DESIGN (operator ruling, 2026-09-21): not every token will
    // have full evidence, and the brain has to be able to infer on what exists. It is also never
    // trained as a refusal — this module measured the "refuse on partial" rule firing on 11 of 12
    // real corpus rows, which is a shutdown, not a gate. Rendered as `evidence_status=partial`,
    // never refused, never rewritten to `complete`.
    if inputs.size_depth_sol.is_none() {
        return Err(AssemblyRefusal::DepthUnknown);
    }
    // THE BAND, AND WHY THIS DOES NOT REFUSE. The corpus drops trades outside 10x its mint's
    // median price, computed over the WHOLE run — a lookahead. The ledger applies the same hygiene
    // causally (see `banded_prints_dropped`), so `s` is the closest lookahead-free reading of the
    // block the model trained on. Refusing when the counter is non-zero was tried and rejected by
    // measurement: it fired on 11 of 12 real corpus rows, which is not a gate but a shutdown. The
    // residual gap (boundary trades the two rules classify differently) is reported as telemetry
    // and closes only with a causal-band corpus rebuild — deliberately deferred, because a rebuild
    // forces an SFT redo.

    Ok(DecisionBundle {
        t_dec_ms: inputs.t_dec_ms,
        // round(x, 1) — the corpus's serializer, field by field.
        age_s: PyNum::Float(py_round(s.age_s, 1)),
        last_trade_age_s: PyNum::Float(py_round(s.last_trade_age_s, 2)),
        venue: inputs.venue.to_string(),
        // `curve_venue_present` in the corpus, and its own definition is the OBSERVATION, not the
        // annotation: `build_states_v2` line 300 is `curve_venue_present = venue != "unknown"`.
        // Reading it off `CurveState::Absent` instead (as this did) made a bundle whose tape
        // showed a live venue render `curve_present=False` — a real divergence the C5 harness
        // caught, and the reason the harness's `CurveState::Absent` placeholder "masked signal".
        // `serve` already refuses an unknown-venue clock (`ClockRefusal::VenueUnknown`), so this
        // is `true` for every served row — exactly as it is on every corpus row.
        curve_present: s.venue != "unknown",
        // The corpus bands its own tape on a whole-run median (see the ledger's
        // `prices_outside_causal_band`). We cannot reproduce that causally, so when such trades are
        // present the block says `partial` — the corpus's own word for a block that is not exactly
        // what it would have written — rather than claiming `complete` over numbers it would have
        // banded differently.
        evidence_status: s.evidence_status.clone(),
        n_prior_trades: s.n_prior_trades as i64,
        buy_count: s.buy_count as i64,
        sell_count: s.sell_count as i64,
        unique_traders: s.unique_traders as i64,
        // IDENTITY, not a conversion: the ledger's value is already the corpus's
        // `price_lamports_per_raw_token` (see the module docs — the field name lies).
        price_lamports_per_raw_token: PyNum::Float(s.price_lamports_per_raw_token),
        ret_5s_bp: s.ret_5s_bp.map(|v| PyNum::Float(py_round(v, 5))),
        ret_30s_bp: s.ret_30s_bp.map(|v| PyNum::Float(py_round(v, 5))),
        vol_30s_bp: s
            .price_volatility_30s_bp
            .map(|v| PyNum::Float(py_round(v, 5))),
        buy_volume_lamports: s.buy_volume_lamports,
        sell_volume_lamports: s.sell_volume_lamports,
        net_flow_lamports: s.net_flow_lamports,
        top1_trader_share: s
            .top1_trader_share
            .map_or(PyNum::Float(0.0), |v| PyNum::Float(py_round(v, 5))),
        top5_trader_share: s
            .top5_trader_share
            .map_or(PyNum::Float(0.0), |v| PyNum::Float(py_round(v, 5))),
        buyer_seller_ratio: s.buyer_seller_ratio.map(|v| PyNum::Float(py_round(v, 5))),
        enriched: EnrichedCandidate {
            // An unsourced mcap stays `None` -> `na`; the corpus distinguishes that from 0.
            mcap_sol_at_t: inputs.mcap_sol_at_t.map(|v| PyNum::Float(py_round(v, 6))),
            mcap_source: inputs.mcap_source.to_string(),
            holders_at_t: PyNum::Int(inputs.enriched.holders_at_t as i64),
            // round(x, 6) — the c9 producer's own precision for this block.
            top1_float_share: PyNum::Float(py_round(inputs.enriched.top1_float_share, 6)),
            top5_float_share: PyNum::Float(py_round(inputs.enriched.top5_float_share, 6)),
            holder_hhi: PyNum::Float(py_round(inputs.enriched.holder_hhi, 6)),
            bundle_slots: PyNum::Int(inputs.enriched.bundle_slots as i64),
            bundle_wallets: PyNum::Int(inputs.enriched.bundle_wallets as i64),
            volume_sol_at_t: PyNum::Float(py_round(inputs.enriched.volume_sol_at_t, 6)),
            wash_ratio: PyNum::Float(py_round(inputs.enriched.wash_ratio, 6)),
        },
        dev: inputs.dev.clone(),
        flow: inputs.flow.clone(),
        flow_no_prior: inputs.flow_no_prior,
        curve: inputs.curve.clone(),
        amm: inputs.amm.clone(),
        size_depth_sol: inputs.size_depth_sol,
        size_amm: inputs.size_amm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> StateSnapshot {
        StateSnapshot {
            n_prior_trades: 41,
            buy_count: 30,
            sell_count: 11,
            unique_traders: 22,
            sol_volume_lamports: 3_000_000_000,
            buy_volume_lamports: 2_100_000_000,
            sell_volume_lamports: 900_000_000,
            net_flow_lamports: 1_200_000_000,
            // The corpus row's own `price_lamports_per_raw_token`, i.e. lamports per raw token.
            price_lamports_per_raw_token: 0.02445740498411998,
            age_s: 12.0,
            last_trade_age_s: 1.0,
            top1_trader_share: Some(0.41),
            top5_trader_share: Some(0.78),
            buyer_seller_ratio: Some(2.727),
            price_volatility_30s_bp: Some(180.0),
            venue: "pumpfun".to_string(),
            evidence_status: "complete".to_string(),
            ret_5s_bp: Some(120.0),
            ret_30s_bp: Some(-30.0),
            ret_300s_bp: None,
            complete: true,
            identity_known: true,
            token_leg_known: true,
            banded_prints_dropped: 0,
        }
    }

    fn enriched() -> EnrichedSnapshot {
        EnrichedSnapshot {
            n_buys_at_t: 30,
            n_sells_at_t: 11,
            holders_at_t: 18,
            top1_float_share: 0.34,
            top5_float_share: 0.71,
            holder_hhi: 0.11,
            bundle_slots: 2,
            bundle_wallets: 5,
            volume_sol_at_t: 3.0,
            wash_ratio: 0.05,
            round_trip_wallets: 2,
        }
    }

    fn flow() -> FlowState {
        FlowState {
            entrants_60s: 4,
            entrants_300s: 9,
            net_flow_sol_300s: 12.5,
            fresh_wallet_share_300s: Some(0.5),
            flow_lookback_d: 0.0,
            sniper_share_300s: Some(0.0),
            bot_uniform_share_300s: Some(0.0),
            smart_entrants_300s: 0,
            smart_net_flow_sol_300s: 0.0,
            coentry_wallets_300s: 2,
            creator_trading_own_mint: false,
            entrant_fee_p90_lamports: Some(5_000),
            entrant_cu_p50: Some(40_000),
        }
    }

    fn inputs<'a>(
        s: &'a StateSnapshot,
        e: &'a EnrichedSnapshot,
        f: &'a FlowState,
    ) -> BundleInputs<'a> {
        BundleInputs {
            snapshot: s,
            enriched: e,
            t_dec_ms: 1_700_000_000_000,
            venue: "pumpfun",
            mcap_sol_at_t: Some(62.5),
            mcap_source: "curve",
            flow: f,
            flow_no_prior: false,
            curve: CurveState::Absent {
                reason: "no_reserves".to_string(),
            },
            amm: AmmState::Absent {
                reason: "no_pool".to_string(),
            },
            dev: DevHistoryDecision {
                creator_past_launches: None,
                creator_known: 0,
            },
            size_depth_sol: Some(62.5),
            size_amm: false,
        }
    }

    /// F1 RULING (operator, 2026-09-21): a partial evidence packet is **ACCEPTED** — not every
    /// token will have full evidence and the brain has to be able to infer on what exists. So the
    /// packet is rendered as `evidence_status=partial`: never refused, never rewritten to
    /// `complete` (which would be the dishonest repair, and would also be OOD — the corpus's own
    /// partial rows are the ones the model was asked to infer on).
    #[test]
    fn a_partial_evidence_packet_assembles_and_keeps_its_status() {
        let (mut s, e, f) = (snapshot(), enriched(), flow());
        s.evidence_status = "partial".to_string();
        let b = assemble(&inputs(&s, &e, &f)).expect("partial evidence must still assemble");
        assert_eq!(
            b.evidence_status, "partial",
            "the status travels as given — never rewritten to complete"
        );
    }

    #[test]
    fn a_complete_input_set_renders_a_bundle_with_the_price_in_the_bundles_unit() {
        let (s, e, f) = (snapshot(), enriched(), flow());
        let b = assemble(&inputs(&s, &e, &f)).expect("complete inputs assemble");
        assert_eq!(b.t_dec_ms, 1_700_000_000_000);
        assert_eq!(b.enriched.holders_at_t, PyNum::Int(18));
        assert_eq!(b.n_prior_trades, 41);
        // The conversion, asserted rather than trusted: the ledger's SOL-per-raw price times 1e9 is
        // the bundle's lamports-per-raw price, and the rendered prompt must carry that value.
        let rendered = render_decision(&b);
        // The lamports-per-raw form is the corpus's own digits, and the SOL-per-raw form is that
        // same price divided by 1e9 (`%.10g` is the corpus's rendering). Pinning BOTH is what makes
        // a unit slip impossible to introduce silently: the name says SOL, the value is lamports.
        assert!(
            rendered.contains("price_lamports_per_raw_token=0.02445740498"),
            "lamports-per-raw form missing or wrong:\n{rendered}"
        );
        assert!(
            rendered.contains("price_sol_per_raw_token=2.445740498e-11"),
            "SOL-per-raw form missing or wrong:\n{rendered}"
        );
        assert!(rendered.contains("ENRICHED CANDIDATE STATE"), "{rendered}");
        assert!(rendered.contains("LIVE FLOW STATE"), "{rendered}");
    }

    /// The rounding law, pinned where it is observable: the corpus's `round()` is ties-to-EVEN, and a
    /// ties-away Rust `round()` would differ on exactly the half-way cases.
    #[test]
    fn numbers_round_as_pythons_round_does() {
        // The C5 evidence: full precision in, the corpus's token sequence out.
        assert_eq!(py_round(5.4304577631370154, 5), 5.43046);
        assert_eq!(py_round(22.264204271968236, 5), 22.2642);
        assert_eq!(py_round(0.6618963615440171, 6), 0.661896);
        assert_eq!(py_round(316.9502157193907, 1), 317.0);
        assert_eq!(py_round(0.216315, 2), 0.22);
        // Ties go to the EVEN digit: 0.125 -> 0.12, not 0.13 (which is what `f64::round` gives).
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.135, 2), 0.14);
        // A non-finite value is returned untouched rather than becoming a number.
        assert!(py_round(f64::NAN, 5).is_nan());
        assert!(py_round(f64::INFINITY, 5).is_infinite());
    }

    #[test]
    fn each_untrustworthy_input_refuses_with_its_own_cause() {
        let (mut s, e, f) = (snapshot(), enriched(), flow());
        s.identity_known = false;
        assert_eq!(
            assemble(&inputs(&s, &e, &f)).unwrap_err(),
            AssemblyRefusal::IdentityUnknown
        );
        assert_eq!(
            AssemblyRefusal::IdentityUnknown.as_str(),
            "bundle_identity_unknown"
        );

        let (mut s2, e2, f2) = (snapshot(), enriched(), flow());
        s2.token_leg_known = false;
        assert_eq!(
            assemble(&inputs(&s2, &e2, &f2)).unwrap_err(),
            AssemblyRefusal::TokenLegUnknown
        );

        let (mut s3, e3, f3) = (snapshot(), enriched(), flow());
        s3.complete = false;
        assert_eq!(
            assemble(&inputs(&s3, &e3, &f3)).unwrap_err(),
            AssemblyRefusal::TapeIncomplete
        );

        // Blocks that disagree about whether there is history at all.
        let (mut s4, mut e4, f4) = (snapshot(), enriched(), flow());
        e4.n_buys_at_t = 0;
        e4.n_sells_at_t = 0;
        assert_eq!(
            assemble(&inputs(&s4, &e4, &f4)).unwrap_err(),
            AssemblyRefusal::NoHistory
        );
        s4.n_prior_trades = 0;
        e4.n_buys_at_t = 30;
        e4.n_sells_at_t = 11;
        assert_eq!(
            assemble(&inputs(&s4, &e4, &f4)).unwrap_err(),
            AssemblyRefusal::NoHistory
        );
    }

    #[test]
    fn an_unsourced_mcap_stays_absent_rather_than_becoming_zero() {
        let (s, e, f) = (snapshot(), enriched(), flow());
        let mut i = inputs(&s, &e, &f);
        i.mcap_sol_at_t = None;
        let b = assemble(&i).expect("still assemblable");
        assert_eq!(b.enriched.mcap_sol_at_t, None);
        // The corpus renders an unsourced mcap as `na`, which is a different prompt from `0`.
        let rendered = render_decision(&b);
        assert!(rendered.contains("mcap_sol_at_t=na"), "{rendered}");
    }

    /// F1: an unpriceable depth is the ONE live-reachable, never-trained input that can reach this
    /// module — the other four states F1 names are refused upstream, and the two reserve-absence
    /// forms are in-distribution (see the guard's measurement). It must fail closed with its own
    /// cause, and a priced depth must keep assembling.
    #[test]
    fn an_unpriceable_depth_refuses_rather_than_rendering_pool_depth_na() {
        let (s, e, f) = (snapshot(), enriched(), flow());
        let mut i = inputs(&s, &e, &f);
        i.size_depth_sol = Some(62.5);
        assert!(assemble(&i).is_ok(), "a priced depth still assembles");

        i.size_depth_sol = None;
        assert_eq!(
            assemble(&i).unwrap_err(),
            AssemblyRefusal::DepthUnknown,
            "an unknown depth is out-of-distribution and must be refused, not rendered as `na`"
        );
        assert_eq!(
            AssemblyRefusal::DepthUnknown.as_str(),
            "bundle_depth_unknown"
        );
    }
}
