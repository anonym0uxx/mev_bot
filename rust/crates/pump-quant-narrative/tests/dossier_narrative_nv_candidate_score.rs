// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'narrative' component (leaf 'nv_candidate_score').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::narrative::*;

#[test]
fn nv_cs_confirmed_max_and_fade_cap() {
    let inputs = (
        LifecycleStage::Emergence,
        AttentionMoneyDivergence::Confirmed,
        2 * FP_ONE,
        FP_ONE,
    );
    // 350 + 300 + 200 + 150 = 1000, confirmed => uncapped.
    assert_eq!(
        nv_candidate_score(inputs.0, inputs.1, inputs.2, inputs.3, true),
        1000
    );
    // Same inputs unconfirmed => fade-first hard cap at 500.
    assert_eq!(
        nv_candidate_score(inputs.0, inputs.1, inputs.2, inputs.3, false),
        500
    );
}

#[test]
fn nv_cs_component_arithmetic_and_bands() {
    // Virality 300 + MoneyLeads 120 + virality(1.0 -> 100) + prelegibility(0.5 -> 75) = 595.
    assert_eq!(
        nv_candidate_score(
            LifecycleStage::Virality,
            AttentionMoneyDivergence::MoneyLeads,
            FP_ONE,
            FP_ONE / 2,
            true
        ),
        595
    );
    // Virality band saturates at 200; pre-legibility clamps input to FP_ONE (=>150).
    // Formation 100 + AttentionLeads 200 + 200 + 150 = 650.
    assert_eq!(
        nv_candidate_score(
            LifecycleStage::Formation,
            AttentionMoneyDivergence::AttentionLeads,
            10 * FP_ONE,
            10 * FP_ONE,
            true
        ),
        650
    );
    // All-zero components.
    assert_eq!(
        nv_candidate_score(
            LifecycleStage::Decay,
            AttentionMoneyDivergence::Saturating,
            0,
            0,
            true
        ),
        0
    );
    // Unconfirmed but already below cap is untouched: Formation 100 only.
    assert_eq!(
        nv_candidate_score(
            LifecycleStage::Formation,
            AttentionMoneyDivergence::Saturating,
            0,
            0,
            false
        ),
        100
    );
}
