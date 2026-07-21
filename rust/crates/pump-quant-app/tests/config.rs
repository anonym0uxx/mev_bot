//! Config parsing/validation contract.

use pump_quant_app::config::{Config, ConfigError, FillModeCfg};

#[test]
fn dev_portable_is_valid() {
    assert!(Config::dev_portable().validate().is_ok());
}

#[test]
fn overrides_apply_over_default() {
    let doc = "\
# a comment
promote_k = 3
gate_expected_move_bps = 777
fill_mode = 1
";
    let cfg = Config::from_str_over_default(doc).expect("parse");
    assert_eq!(cfg.promote_k, 3);
    assert_eq!(cfg.gate_expected_move_bps, 777);
    assert_eq!(cfg.fill_mode, FillModeCfg::OptimisticCeiling);
    // Untouched fields keep the portable default.
    assert_eq!(
        cfg.watchlist_capacity,
        Config::dev_portable().watchlist_capacity
    );
}

#[test]
fn unknown_key_is_rejected() {
    let e = Config::from_str_over_default("no_such_key = 1").unwrap_err();
    assert!(matches!(e, ConfigError::UnknownKey(_)));
}

#[test]
fn negative_unsigned_is_out_of_range() {
    let e = Config::from_str_over_default("promote_min_rank = -5").unwrap_err();
    assert!(matches!(e, ConfigError::OutOfRange(_, -5)));
}

#[test]
fn syntax_error_reports_line() {
    let e = Config::from_str_over_default("promote_k = 2\nbroken line\n").unwrap_err();
    assert_eq!(e, ConfigError::Syntax(2));
}

#[test]
fn inconsistent_envelope_is_rejected() {
    // floor above ceiling
    let doc = "reflect_weight_floor_bp = 30000\nreflect_weight_ceiling_bp = 10000\n";
    let e = Config::from_str_over_default(doc).unwrap_err();
    assert!(matches!(e, ConfigError::Inconsistent(_)));
}
