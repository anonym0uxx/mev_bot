// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_earn' component (leaf 'se_earn').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::social_earn::*;

#[test]
fn se_earn_favorable_losing_and_unproven() {
    let mut e = SocialEarn::new(SocialEarnParams::standard());
    // Unproven source has no earned quality (baseline fallback).
    assert_eq!(e.quality_bps_for(42), None);
    // A favorable realized outcome earns the full favorable-rate.
    e.record_call(7, [1u8; 32], 1_000);
    e.record_outcome(&[1u8; 32], 5_000_000);
    e.reconcile();
    assert_eq!(e.quality_bps_for(7), Some(10_000));
    // A loss earns zero.
    e.record_call(9, [2u8; 32], 1_000);
    e.record_outcome(&[2u8; 32], -3_000_000);
    e.reconcile();
    assert_eq!(e.quality_bps_for(9), Some(0));
    // An outcome for a mint no source called records nothing.
    e.record_outcome(&[3u8; 32], 1);
    e.reconcile();
    assert_eq!(e.quality_bps_for(123), None);
}
