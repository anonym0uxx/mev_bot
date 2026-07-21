#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::lockfree::*;
#[test]
fn prop_spsc_exactly_once_in_order() {
    let (mut p, mut c) = Spsc::<u64, 1024>::new().split();
    let h = std::thread::spawn(move || {
        let mut got = Vec::new();
        while got.len() < 100_000 {
            if let Some(v) = c.pop() {
                got.push(v);
            } else {
                std::hint::spin_loop();
            }
        }
        got
    });
    let mut i = 0u64;
    while i < 100_000 {
        if p.push(i).is_ok() {
            i += 1;
        } else {
            std::hint::spin_loop();
        }
    }
    let got = h.join().unwrap();
    assert_eq!(got.len(), 100_000);
    assert!(got.windows(2).all(|w| w[1] == w[0] + 1)); // in order, exactly once
}
