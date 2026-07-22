// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'shred' component (leaf 'sh_fec_track').
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
use pump_quant_core::shred::*;

#[test]
fn prop_fec_complete_once_and_conflicts() {
    let mut t = FecTable::with_capacity(8);
    let mk = |i| {
        (
            ShredHeader::test(5, i, /*fec*/ 0, /*expected*/ 3),
            [i as u8; 8],
        )
    };
    let (h0, p0) = mk(0);
    let (h1, p1) = mk(1);
    let (h2, p2) = mk(2);
    assert!(matches!(track(&mut t, &h0, &p0), Track::Stored));
    assert!(matches!(track(&mut t, &h0, &p0), Track::Duplicate));
    assert!(matches!(track(&mut t, &h0, &[9u8; 8]), Track::Conflicting));
    assert!(matches!(track(&mut t, &h1, &p1), Track::Stored));
    assert!(matches!(track(&mut t, &h2, &p2), Track::SetComplete(_)));
}
