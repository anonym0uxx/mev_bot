// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_parse' component (leaf 'sp_parse').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_parse::*;

#[test]
fn sp_parse_full_and_failclosed_and_deterministic() {
    let raw = br#"{"platform":"x","author":"kolguy","community":"",
        "text":"send it $WIF EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "likes":100,"reposts":20,"replies":5,"echo":false}"#;
    let ev = parse_social_event(raw, 1_000_000_000).unwrap();
    assert_eq!(ev.platform, SocialPlatform::X);
    assert_eq!(ev.observed_at_ns, 1_000_000_000);
    assert_eq!(ev.engagement, 125);
    assert_eq!(ev.n_cashtags, 1);
    assert_eq!(ev.n_mints, 1);
    assert!(ev.is_targeted());
    // Fail-closed cases.
    assert!(parse_social_event(br#"{"platform":"foo","author":"a","text":"t"}"#, 0).is_none());
    assert!(parse_social_event(br#"{"platform":"x","text":"t"}"#, 0).is_none());
    assert!(parse_social_event(b"not json", 0).is_none());
    // Determinism.
    assert_eq!(
        parse_social_event(raw, 7).unwrap(),
        parse_social_event(raw, 7).unwrap()
    );
}
