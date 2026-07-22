#![allow(unused_imports)]
use pump_quant_protocol::decode::*;

use pump_quant_protocol::registry::{self, Venue};

/// Build a 35-byte PumpSwap pool account with the given fields.
///
/// The 0..8 discriminator is written with the registry's expected `Pool`
/// identity so the decoder's §18.2 fail-closed check passes.
fn pool_bytes(bump: u8, index: u16, base: u64, quote: u64, lp: u64) -> Vec<u8> {
    let mut b = vec![0u8; 35];
    b[0..8].copy_from_slice(&registry::account_discriminator(Venue::PumpSwap));
    b[8] = bump;
    b[9..11].copy_from_slice(&index.to_le_bytes());
    b[11..19].copy_from_slice(&base.to_le_bytes());
    b[19..27].copy_from_slice(&quote.to_le_bytes());
    b[27..35].copy_from_slice(&lp.to_le_bytes());
    b
}

#[test]
fn decodes_fields_at_correct_offsets() {
    let bytes = pool_bytes(254, 7, 123_456_789, 987_654_321, 1_000_000);
    let p = decode_pumpswap_pool(&bytes).expect("should decode");
    assert_eq!(p.pool_bump, 254);
    assert_eq!(p.index, 7);
    assert_eq!(p.base_reserve, 123_456_789);
    assert_eq!(p.quote_reserve, 987_654_321);
    assert_eq!(p.lp_supply, 1_000_000);
}

#[test]
fn decodes_max_values() {
    let bytes = pool_bytes(u8::MAX, u16::MAX, u64::MAX, u64::MAX, u64::MAX);
    let p = decode_pumpswap_pool(&bytes).unwrap();
    assert_eq!(p.pool_bump, u8::MAX);
    assert_eq!(p.index, u16::MAX);
    assert_eq!(p.base_reserve, u64::MAX);
    assert_eq!(p.quote_reserve, u64::MAX);
    assert_eq!(p.lp_supply, u64::MAX);
}

#[test]
fn rejects_short_buffer() {
    assert!(decode_pumpswap_pool(&[0u8; 34]).is_none());
    assert!(decode_pumpswap_pool(&[]).is_none());
}

#[test]
fn extra_trailing_bytes_are_ok() {
    let mut bytes = pool_bytes(1, 2, 3, 4, 5);
    bytes.extend_from_slice(&[7u8; 40]);
    let p = decode_pumpswap_pool(&bytes).unwrap();
    assert_eq!(p.base_reserve, 3);
    assert_eq!(p.quote_reserve, 4);
    assert_eq!(p.lp_supply, 5);
}

#[test]
fn rejects_zero_discriminator() {
    let mut bytes = pool_bytes(1, 2, 3, 4, 5);
    bytes[0..8].copy_from_slice(&[0u8; 8]);
    assert!(decode_pumpswap_pool(&bytes).is_none());
}

#[test]
fn rejects_foreign_program_discriminator() {
    let mut bytes = pool_bytes(1, 2, 3, 4, 5);
    bytes[0..8].copy_from_slice(&registry::account_discriminator(Venue::PumpFun));
    assert!(decode_pumpswap_pool(&bytes).is_none());
}
