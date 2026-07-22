// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'config' component (leaf 'cfg_validate').
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
fn validate_accepts_dev_portable() {
    assert_eq!(Config::dev_portable().validate(), Ok(()));
}

#[test]
fn validate_rejects_floor_above_ceiling() {
    let mut cfg = Config::dev_portable();
    cfg.reflect_weight_floor_bp = 30_000;
    cfg.reflect_weight_ceiling_bp = 10_000;
    let e = cfg.validate().unwrap_err();
    assert_eq!(e, ConfigError::Inconsistent("weight floor exceeds ceiling"));
}

#[test]
fn validate_rejects_zero_promote_k() {
    let mut cfg = Config::dev_portable();
    cfg.promote_k = 0;
    let e = cfg.validate().unwrap_err();
    assert_eq!(e, ConfigError::Inconsistent("promote_k must be positive"));
}
