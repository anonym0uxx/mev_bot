// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'config' component (leaf 'cfg_apply').
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
use pump_quant_app::config::*;

#[test]
fn apply_valid_override_mutates_named_field() {
    let mut cfg = Config::dev_portable();
    assert_eq!(cfg.apply("promote_k", 3), Ok(()));
    assert_eq!(cfg.promote_k, 3);
    assert_eq!(cfg.apply("gate_expected_move_bps", 777), Ok(()));
    assert_eq!(cfg.gate_expected_move_bps, 777);
}

#[test]
fn apply_unknown_key_is_rejected() {
    let mut cfg = Config::dev_portable();
    let e = cfg.apply("no_such_key", 1).unwrap_err();
    assert_eq!(e, ConfigError::UnknownKey("no_such_key".to_string()));
}

#[test]
fn apply_negative_into_unsigned_is_out_of_range() {
    let mut cfg = Config::dev_portable();
    let e = cfg.apply("promote_min_rank", -5).unwrap_err();
    assert_eq!(
        e,
        ConfigError::OutOfRange("promote_min_rank".to_string(), -5)
    );
}

#[test]
fn apply_clamps_denominator_and_mult_to_minimum_one() {
    let mut cfg = Config::dev_portable();
    assert_eq!(cfg.apply("gate_impact_den", 0), Ok(()));
    assert_eq!(cfg.gate_impact_den, 1);
    assert_eq!(cfg.apply("confirmed_capacity_mult", 0), Ok(()));
    assert_eq!(cfg.confirmed_capacity_mult, 1);
}
