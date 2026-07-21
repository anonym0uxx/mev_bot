#![allow(unused_imports)]
use pump_quant_core::shred::*;
#[test]
fn prop_reassembly_order_and_timing() {
    let set = CompleteSet::test(&[(2, b"cc", 30), (0, b"aa", 10), (1, b"bb", 5)]);
    let mut out = SegBuf::new();
    let meta = reassemble(&set, &mut out).unwrap();
    assert_eq!(out.bytes(), b"aabbcc");   // index order, not arrival order
    assert_eq!(meta.first_local_arrival_ns, 5); // earliest constituent
}
