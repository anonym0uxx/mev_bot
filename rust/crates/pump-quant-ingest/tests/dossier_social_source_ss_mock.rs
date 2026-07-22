// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_source' component (leaf 'ss_mock').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_source::*;

#[test]
fn ss_mock_replays_then_drains() {
    let p = |t: u64| {
        RawSocialPayload::new(
            br#"{"platform":"x","author":"a","text":"$WIF","likes":1}"#.to_vec(),
            t,
        )
    };
    let mut src = MockSocialSource::new()
        .with_batch(vec![p(10)])
        .with_batch(vec![p(20), p(21)]);
    assert_eq!(src.next_batch().len(), 1);
    assert_eq!(src.next_batch().len(), 2);
    assert!(src.next_batch().is_empty());
    assert!(
        src.next_batch().is_empty(),
        "draining past the end stays empty"
    );
    assert!(src.is_drained());
}
