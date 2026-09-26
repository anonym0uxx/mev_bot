//! **NARRATIVE PRECONDITION** (operator ruling 2026-09-26) — the gate consumes the
//! resolved name→meta verdict, and this file pins how.
//!
//! The ruling: a memecoin's NAME must be inferred against the current documented
//! meta before entry, and the inference is BINDING — a real trader only enters a
//! coin they are SURE matches the attention narrative. The predicate that
//! satisfies it (positive evidence of a RISING narrative, via the lexical or the
//! attention lane) lives in `pump_quant_narrative::entry_narrative::nv_narrative_verdict`
//! and is unit-tested there. This file pins the CONSUMER side: the gate.
//!
//! Four properties, each of which is a way this could be wrong:
//!
//! 1. **OBSERVE never refuses.** The default mode records the verdict without
//!    binding, because the cohort the gate would refuse has not yet been priced
//!    through the engine (a barrier proxy and `score_entry` can disagree in sign
//!    on the same rows). If OBSERVE refused, the measurement could never run.
//! 2. **ENFORCE refuses each verdict with its OWN code.** A single collapsed code
//!    would hide which refusal reason dominates in the journal.
//! 3. **SENTINEL DISCIPLINE — unobserved is ADMITTED.** `narrative_verdict == 0`
//!    means the name was never resolved. A filter that never ran must not refuse
//!    the trade, or every market without a `NarrativeResolved` event dies silently.
//!    This is also what keeps the golden tape byte-identical.
//! 4. **Eligible is never refused** by the precondition.

use pump_quant_app::config::Config;
use pump_quant_app::gate::{decide, Confirmation, GateDecision, GateReject};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};

/// Every refusal this precondition can emit.
const NARRATIVE_REJECTS: [GateReject; 4] = [
    GateReject::NarrativeSaturated,
    GateReject::NarrativeNoAttach,
    GateReject::NarrativeThrowaway,
    GateReject::NarrativeUnresolved,
];

/// 60 SOL of SOL-side reserve — an ordinary mid-curve book.
const VSOL: u64 = 60_000_000_000;

fn cfg(mode: u8) -> Config {
    let mut c = Config::dev_portable();
    c.narrative_gate_mode = mode;
    c
}

fn features(verdict: u8, stage: u8, family: u8) -> Features {
    Features {
        liquidity_lamports: VSOL,
        buy_pressure_bp: 10_000,
        unique_buyers: 200,
        age_slots: 1_000,
        buy_ratio_bp: 10_000,
        max_trade_lamports: 0,
        trades_observed: 500,
        volume_lamports: 100_000_000_000,
        narrative_verdict: verdict,
        narrative_stage: stage,
        narrative_family: family,
        narrative_lexicon_version: 7,
        ..Features::default()
    }
}

fn cand(verdict: u8, stage: u8, family: u8) -> Candidate {
    Candidate::new(
        Mint::new([7u8; 32]),
        Lane::ActiveMarketScalp,
        1_000,
        0,
        features(verdict, stage, family),
    )
}

fn conf(verdict: u8, stage: u8, family: u8) -> Confirmation {
    Confirmation {
        depth: pump_quant_app::curve_depth::CurveDepth::derived(VSOL),
        numeric: features(verdict, stage, family),
    }
}

fn cold_start(c: &Config) -> pump_quant_app::priced_move::PricedMove {
    pump_quant_app::priced_move::PricedMove::for_candidate(
        None,
        Lane::ActiveMarketScalp,
        0,
        0,
        c.gate_expected_move_bps,
        c.expectancy_min_lane_trades,
    )
}

/// The precondition must actually be REACHABLE — if an earlier gate refuses the
/// fixture, every assertion below would pass vacuously and prove nothing.
fn assert_reaches_the_precondition(c: &Config, verdict: u8) {
    let d = decide(
        &cand(verdict, 2, 1),
        Some(conf(verdict, 2, 1)),
        c,
        cold_start(c),
    );
    assert!(
        !matches!(
            d,
            GateDecision::Reject(GateReject::NeedsOnchainConfirmation)
                | GateDecision::Reject(GateReject::NoNumericConfirmation)
                | GateDecision::Reject(GateReject::EntryQualityFilter)
                | GateDecision::Reject(GateReject::OutsideMcapBand)
                | GateDecision::Reject(GateReject::EconomicallyUnviable)
        ),
        "fixture is refused BEFORE the narrative precondition (got {d:?}); the test would be vacuous"
    );
}

#[test]
fn observe_mode_never_refuses_on_the_verdict() {
    let c = cfg(0);
    for verdict in [2u8, 3, 4, 5] {
        let d = decide(
            &cand(verdict, 2, 1),
            Some(conf(verdict, 2, 1)),
            &c,
            cold_start(&c),
        );
        assert!(
            !NARRATIVE_REJECTS
                .iter()
                .any(|r| matches!(d, GateDecision::Reject(x) if x == *r)),
            "OBSERVE (default) must record verdict {verdict} without refusing; got {d:?}"
        );
    }
}

#[test]
fn enforce_mode_refuses_each_verdict_with_its_own_code() {
    let c = cfg(1);
    assert_reaches_the_precondition(&c, 5);
    let cases = [
        (2u8, GateReject::NarrativeSaturated),
        (3, GateReject::NarrativeNoAttach),
        (4, GateReject::NarrativeThrowaway),
        (5, GateReject::NarrativeUnresolved),
    ];
    for (verdict, want) in cases {
        assert_eq!(
            decide(
                &cand(verdict, 2, 1),
                Some(conf(verdict, 2, 1)),
                &c,
                cold_start(&c)
            ),
            GateDecision::Reject(want),
            "verdict {verdict} must refuse with its own code"
        );
    }
}

#[test]
fn unobserved_verdict_is_admitted_even_in_enforce() {
    let c = cfg(1);
    // 0 = the name was never resolved. A filter that never ran must not refuse.
    let d = decide(&cand(0, 0, 0), Some(conf(0, 0, 0)), &c, cold_start(&c));
    assert!(
        !NARRATIVE_REJECTS
            .iter()
            .any(|r| matches!(d, GateDecision::Reject(x) if x == *r)),
        "an UNOBSERVED verdict must be admitted past the precondition; got {d:?}"
    );
}

#[test]
fn eligible_verdict_is_never_refused_by_the_precondition() {
    let c = cfg(1);
    for stage in [1u8, 2, 0] {
        let d = decide(
            &cand(1, stage, 1),
            Some(conf(1, stage, 1)),
            &c,
            cold_start(&c),
        );
        assert!(
            !NARRATIVE_REJECTS.iter().any(|r| matches!(d, GateDecision::Reject(x) if x == *r)),
            "Eligible (verdict 1, stage {stage}) must not be refused by the precondition; got {d:?}"
        );
    }
}
