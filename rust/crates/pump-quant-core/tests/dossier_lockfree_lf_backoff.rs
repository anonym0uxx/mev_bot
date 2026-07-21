#![allow(unused_imports)]
use pump_quant_core::lockfree::*;
#[test]
fn prop_backoff_hot_never_parks() {
    let mut b = Backoff::new();
    for _ in 0..1_000_000 {
        assert!(matches!(backoff_step(&mut b, true), Waited::Spun));
    }
    b.reset();
    let mut saw_yield = false; let mut saw_park = false;
    for _ in 0..100_000 {
        match backoff_step(&mut b, false) {
            Waited::Yielded => saw_yield = true,
            Waited::Parked => { saw_park = true; break; }
            Waited::Spun => {}
        }
    }
    assert!(saw_yield && saw_park); // escalation reaches park off the hot window
}
