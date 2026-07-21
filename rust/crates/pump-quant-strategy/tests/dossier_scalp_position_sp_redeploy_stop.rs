#![allow(unused_imports)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_rate_comparator() {
    assert!(should_exit_on_rate(10, 100, 20, true, false));   // 10 < 80 -> exit
    assert!(!should_exit_on_rate(90, 100, 20, true, true));   // 90 >= 80 -> hold
    assert!(!should_exit_on_rate(-5, 0, 0, true, false) == false); // negative hold vs 0 redeploy -> exit
    assert!(should_exit_on_rate(0, 0, 0, false, true));       // stale -> baseline
    assert!(!should_exit_on_rate(0, 0, 0, false, false));     // stale -> baseline
}
