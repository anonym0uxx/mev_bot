#![allow(unused_imports)]
use pump_quant_core::lockfree::*;
#[test]
fn prop_seqlock_no_torn_reads() {
    #[derive(Copy, Clone, PartialEq, Debug)]
    struct Pair(u64, u64); // invariant: .1 == .0 * 2
    let cell = std::sync::Arc::new(SeqCell::new(Pair(0, 0)));
    let w = cell.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..200_000u64 { w.write(Pair(i, i * 2)); }
    });
    for _ in 0..200_000 {
        let p = cell.read();
        assert_eq!(p.1, p.0 * 2, "torn read observed");
    }
    writer.join().unwrap();
}
