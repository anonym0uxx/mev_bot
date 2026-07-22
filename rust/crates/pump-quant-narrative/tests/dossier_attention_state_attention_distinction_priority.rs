// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'attention_state' component (leaf 'attention_distinction_priority').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::attention_state::*;

#[test]
fn prop_attention_distinction_priority() {
    let st = |velocity: i64, copycat_count: u32, unique_sources: u32| AttentionState {
        unique_sources,
        unique_communities: 1,
        weighted_mentions_1m: 0,
        weighted_mentions_5m: 0,
        engagement_velocity: velocity,
        engagement_acceleration: 0,
        source_concentration: 0,
        narrative_age_ns: 0,
        copycat_count,
        freshness: 0,
    };
    // Property: total_mentions == 0 always yields NoSignal regardless of the other arguments.
    for v in [-5i64, 0, 5] {
        for cc in [0u32, 100] {
            let s = st(v, cc, 10);
            assert_eq!(
                nv_attention_distinction(&s, -1, 3_000, 0, 3),
                AttentionDistinction::NoSignal
            );
        }
    }
    // Rule 2 outranks rule 3: rising + outflow with heavy copycat -> LateExit.
    assert_eq!(
        nv_attention_distinction(&st(5, 100, 10), -1, 3_000, 100, 3),
        AttentionDistinction::LateExitLiquidityPromotion
    );
    // Rule 3: rising, positive flow, copycat share 50% >= 30% threshold.
    assert_eq!(
        nv_attention_distinction(&st(5, 5, 10), 100, 3_000, 10, 3),
        AttentionDistinction::CopycatAttention
    );
    // Rule 4: velocity < 0 -> Decaying.
    assert_eq!(
        nv_attention_distinction(&st(-5, 0, 10), 100, 3_000, 10, 3),
        AttentionDistinction::DecayingAttention
    );
    // Rule 5: rising, broad, low copycat -> Organic.
    assert_eq!(
        nv_attention_distinction(&st(5, 0, 10), 100, 3_000, 10, 3),
        AttentionDistinction::OrganicEmergence
    );
    // Rule 6: rising but too narrow, and flat velocity, both -> Saturated.
    assert_eq!(
        nv_attention_distinction(&st(5, 0, 1), 100, 3_000, 10, 3),
        AttentionDistinction::SaturatedAttention
    );
    assert_eq!(
        nv_attention_distinction(&st(0, 0, 10), 100, 3_000, 10, 3),
        AttentionDistinction::SaturatedAttention
    );
}
