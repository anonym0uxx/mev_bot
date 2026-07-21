#![allow(unused_imports)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_anti_pin() {
    use ExitClass::*;
    // fabricated acceleration cannot pin the position open:
    assert!(time_stop_binds(true, true, true, 1_000, 5_000, Normal));
    // authentic fresh acceleration suppresses the clock:
    assert!(!time_stop_binds(true, true, true, 9_000, 5_000, Normal));
    // stale flow never suppresses:
    assert!(time_stop_binds(true, true, false, 9_000, 5_000, Normal));
    // emergency always binds:
    assert!(time_stop_binds(false, true, true, 9_000, 5_000, Emergency));
}
