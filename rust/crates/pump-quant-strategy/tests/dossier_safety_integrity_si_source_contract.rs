// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_source_contract').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn two_distinct_sources_drive_runtime() {
    let null = NullSource { id: 1 };
    let mut rt_null = StrategyRuntime::new(Box::new(null));
    assert_eq!(rt_null.drive_to_empty(), 0);
    assert_eq!(rt_null.source_id(), SourceId(1));

    let fake = FakeSource::new(
        2,
        vec![
            Observation {
                seq: 1,
                payload: 10,
            },
            Observation {
                seq: 2,
                payload: 20,
            },
            Observation {
                seq: 3,
                payload: 30,
            },
        ],
    );
    let mut rt_fake = StrategyRuntime::new(Box::new(fake));
    assert_eq!(rt_fake.drive_to_empty(), 3);
    assert_eq!(rt_fake.source_id(), SourceId(2));
}
