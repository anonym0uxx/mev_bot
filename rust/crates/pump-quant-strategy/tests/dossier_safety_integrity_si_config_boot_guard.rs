// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_config_boot_guard').
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
fn live_armed_committed_rejected() {
    let cfg = BootConfig {
        live_armed: true,
        committed_to_source: true,
        shadow: false,
        live: false,
    };
    assert_eq!(
        validate_boot_config(&cfg),
        Err(BootError::LiveArmedCommitted)
    );
}
#[test]
fn contradictory_rejected() {
    let cfg = BootConfig {
        live_armed: false,
        committed_to_source: true,
        shadow: true,
        live: true,
    };
    assert_eq!(validate_boot_config(&cfg), Err(BootError::Contradictory));
}
#[test]
fn clean_config_accepted() {
    let cfg = BootConfig {
        live_armed: false,
        committed_to_source: true,
        shadow: true,
        live: false,
    };
    let v = validate_boot_config(&cfg).expect("clean config should validate");
    assert!(v.shadow());
    assert!(!v.live());
}
