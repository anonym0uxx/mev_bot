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

/// The golden tape contains real trades with real entry and exit fills.
/// After Phase 2, every closed trade must deposit one sample into the
/// expected-move model. This test proves the learning loop is wired.
#[test]
fn closed_trades_deposit_samples_into_the_move_table() {
    let cfg = Config::dev_portable();
    let eng = tape_golden::drive_eng(cfg);
    // Before Phase 2, this was always 0 because record() was never called.
    // After Phase 2, every closed trade deposits one sample.
    assert!(
        eng.expected_move_sample_count() > 0,
        "the learning loop must accumulate samples from closed trades; \
         got {} — record() is not wired into the close path",
        eng.expected_move_sample_count()
    );
}

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
