#![allow(unused_imports)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_peak_never_freezes() {
    let mut s = ScalpPositionState::open(1_000, 0);
    for i in 0..10_000u64 {  // far beyond any internal buffer
        let ev = SwapEvent::test(1_000 + i, i * 1_000);
        s = apply_swap(&s, &ev);
    }
    assert_eq!(s.peak_price_fp, 1_000 + 9_999);
    let down = SwapEvent::test(500, 10_000_000);
    s = apply_swap(&s, &down);
    assert_eq!(s.peak_price_fp, 1_000 + 9_999); // monotone
    assert_eq!(s.last_price_fp, 500);
}
