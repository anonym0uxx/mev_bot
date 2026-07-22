#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::exit_ladder::*;
#[test]
fn prop_escalation_monotone_and_urgent() {
    let s0 = EscalationState::new(1_000);
    let s1 = next_escalation(s0, true, 1, 1_000);
    let s2 = next_escalation(s1, true, 1, 1_000);
    assert!(s2.level > s1.level && s1.level > s0.level);
    let slow = next_escalation(s0, true, 1, 1_000).cooldown_ms;
    let fast = next_escalation(s0, true, 100, 1_000).cooldown_ms;
    assert!(fast < slow); // collapse => shorter cooldown (defect-list exception)
    let mut s = s0;
    for _ in 0..10 {
        s = next_escalation(s, true, 10, 1_000);
    }
    assert_eq!(s.level, 4);
    assert!(s.emergency_path_required);
}
