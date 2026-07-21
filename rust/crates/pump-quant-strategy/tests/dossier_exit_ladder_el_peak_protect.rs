// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_peak_protect').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_protection_whole_life_monotone() {
    let p0 = protection_level_fp(1_000, 1_000, 500, 800);
    assert!(p0 > 0); // armed at entry, not after TP2
    let p1 = protection_level_fp(2_000, 1_000, 500, 800);
    assert!(p1 >= p0); // peak up -> protection never down
    assert!(
        protection_level_fp(2_000, 1_000, 500, 800) >= protection_level_fp(1_000, 1_000, 500, 800)
    );
}
