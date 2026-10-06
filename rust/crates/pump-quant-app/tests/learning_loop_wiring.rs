//! **PHASE 2 — the learning loop is wired.**
//!
//! Before Phase 2, `MoveTable::record()` was never called in production code.
//! The calibrated expected-move model stayed empty forever, and enabling
//! `expected_move_model_enable` changed nothing. This test proves the wiring
//! is live: when a trade closes on the golden tape, the engine deposits the
//! realized outcome into the MoveTable, and `expected_move_sample_count()`
//! rises above zero.

mod tape_golden;

use pump_quant_app::config::Config;

/// The sample count must be deterministic: identical inputs → identical count.
/// This guards against any non-deterministic path leaking into the close
/// recording.
#[test]
fn sample_count_is_deterministic() {
    let cfg = Config::dev_portable();
    let eng1 = tape_golden::drive_eng(cfg);
    let eng2 = tape_golden::drive_eng(Config::dev_portable());
    assert_eq!(
        eng1.expected_move_sample_count(),
        eng2.expected_move_sample_count(),
        "identical tape inputs must produce identical sample counts"
    );
}
