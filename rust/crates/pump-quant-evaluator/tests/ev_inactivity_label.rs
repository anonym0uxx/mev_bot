#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_evaluator::evaluator_stats::*;
#[test]
fn prop_inactivity_label_golden() {
    // swaps at 0, 10, 300; window ends 1000; delta_t = 200
    // gaps: 0->10 (10), 10->300 (290 >= 200: FIRST qualifying gap), 300->1000 (700)
    let l = label_terminal(&[0, 10, 300], 1_000, 200);
    assert!(l.dead);
    assert_eq!(l.died_at_ns, Some(10)); // first qualifying gap starts after the swap at 10
    assert_eq!(l.params_version, (200, 1_000));
    let alive = label_terminal(&[0, 100, 200, 300, 900], 1_000, 700);
    assert!(!alive.dead);
    let tail_dead = label_terminal(&[0, 50], 1_000, 500);
    assert_eq!(tail_dead.died_at_ns, Some(50)); // trailing (last, window_end) gap qualifies
}
