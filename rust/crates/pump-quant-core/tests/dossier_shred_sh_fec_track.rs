#![allow(unused_imports)]
use pump_quant_core::shred::*;
#[test]
fn prop_fec_complete_once_and_conflicts() {
    let mut t = FecTable::with_capacity(8);
    let mk = |i| (ShredHeader::test(5, i, /*fec*/0, /*expected*/3), [i as u8; 8]);
    let (h0, p0) = mk(0); let (h1, p1) = mk(1); let (h2, p2) = mk(2);
    assert!(matches!(track(&mut t, &h0, &p0), Track::Stored));
    assert!(matches!(track(&mut t, &h0, &p0), Track::Duplicate));
    assert!(matches!(track(&mut t, &h0, &[9u8; 8]), Track::Conflicting));
    assert!(matches!(track(&mut t, &h1, &p1), Track::Stored));
    assert!(matches!(track(&mut t, &h2, &p2), Track::SetComplete(_)));
}
