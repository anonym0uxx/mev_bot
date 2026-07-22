#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::shred::*;
#[test]
fn prop_header_bounds() {
    let good = ShredHeader::test_bytes(100, 3, 0);
    assert!(decode_header(&good).is_ok());
    for cut in 0..good.len().min(16) {
        assert!(decode_header(&good[..cut]).is_err()); // never panics, always Err
    }
    let mut bad_len = good.clone();
    bad_len[BAD_PAYLOAD_LEN_OFF] = 0xFF; // claims payload beyond buffer
    assert!(decode_header(&bad_len).is_err());
}
