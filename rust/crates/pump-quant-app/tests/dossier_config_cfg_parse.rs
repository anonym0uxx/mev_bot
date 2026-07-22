// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'config' component (leaf 'cfg_parse').
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
fn parse_applies_overrides_and_keeps_defaults() {
    let doc = "# a comment\npromote_k = 3\ngate_expected_move_bps = 777\nfill_mode = 1\n";
    let cfg = Config::from_str_over_default(doc).expect("parse");
    assert_eq!(cfg.promote_k, 3);
    assert_eq!(cfg.gate_expected_move_bps, 777);
    assert_eq!(cfg.fill_mode, FillModeCfg::OptimisticCeiling);
    assert_eq!(
        cfg.watchlist_capacity,
        Config::dev_portable().watchlist_capacity
    );
}

#[test]
fn parse_syntax_error_reports_one_based_line() {
    let e = Config::from_str_over_default("promote_k = 2\nbroken line\n").unwrap_err();
    assert_eq!(e, ConfigError::Syntax(2));
}

#[test]
fn parse_unknown_key_propagates() {
    let e = Config::from_str_over_default("no_such_key = 1").unwrap_err();
    assert_eq!(e, ConfigError::UnknownKey("no_such_key".to_string()));
}
