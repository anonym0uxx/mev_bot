// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_ingest' component (leaf 'si_coordination').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::social_ingest::*;

#[test]
fn si_coordination_needs_distinct_authors_in_window() {
    let mk = |author: &str, at: u64| {
        let json = format!(
            r#"{{"platform":"telegram","author":"{author}","text":"BUY $PEPE same copypasta","likes":1}}"#
        );
        pump_quant_ingest::social_parse::parse_social_event(json.as_bytes(), at).unwrap()
    };
    let a = mk("chanA", 100);
    let b = mk("chanB", 150);
    let a2 = mk("chanA", 160); // same author as a
    assert_eq!(a.content_hash, b.content_hash);
    let coord = coordinated_content(&[a, b, a2], 1_000);
    assert_eq!(coord.len(), 1);
    assert_eq!(coord[0], (a.content_hash, 2));
    // Identical content outside the window is not coordinated.
    let far = mk("chanC", 10_000);
    assert!(coordinated_content(&[a, far], 100).is_empty());
}
