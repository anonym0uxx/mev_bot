#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::exit_ladder::*;
#[test]
fn prop_target_above_floor_or_inadmissible() {
    assert_eq!(derive_target_bps(200, 100, Some(500)), Some(300));
    assert_eq!(derive_target_bps(200, 100, Some(250)), None); // MFE can't pay the floor
    assert_eq!(derive_target_bps(200, 100, None), Some(300));
    assert_eq!(derive_target_bps(u32::MAX, 1, None), None); // overflow
}
