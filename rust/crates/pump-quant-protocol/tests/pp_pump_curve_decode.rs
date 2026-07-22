#![allow(unused_imports)]
use pump_quant_protocol::decode::*;

use pump_quant_protocol::registry::{self, Venue};

/// Build a 49-byte bonding-curve account with the given fields.
///
/// The 0..8 discriminator is written with the registry's expected
/// `BondingCurve` identity so the decoder's §18.2 fail-closed check passes.
fn curve_bytes(v_token: u64, v_sol: u64, r_token: u64, r_sol: u64, complete: u8) -> Vec<u8> {
    let mut b = vec![0u8; 49];
    b[0..8].copy_from_slice(&registry::account_discriminator(Venue::PumpFun));
    b[8..16].copy_from_slice(&v_token.to_le_bytes());
    b[16..24].copy_from_slice(&v_sol.to_le_bytes());
    b[24..32].copy_from_slice(&r_token.to_le_bytes());
    b[32..40].copy_from_slice(&r_sol.to_le_bytes());
    // 40..48 token_total_supply left as zeros.
    b[48] = complete;
    b
}

#[test]
fn decodes_fields_at_correct_offsets() {
    let bytes = curve_bytes(
        1_072_000_000_000_000, // virtual_token
        30_000_000_000,        // virtual_sol
        793_100_000_000_000,   // real_token
        0,                     // real_sol
        0,                     // complete = false
    );
    let c = decode_pump_curve(&bytes).expect("should decode");
    assert_eq!(c.virtual_token, 1_072_000_000_000_000);
    assert_eq!(c.virtual_sol, 30_000_000_000);
    assert_eq!(c.real_token, 793_100_000_000_000);
    assert_eq!(c.real_sol, 0);
    assert!(!c.complete);
}

#[test]
fn decodes_complete_true() {
    let bytes = curve_bytes(5, 6, 7, 8, 1);
    let c = decode_pump_curve(&bytes).unwrap();
    assert!(c.complete);
    assert_eq!(c.virtual_token, 5);
    assert_eq!(c.real_sol, 8);
}

#[test]
fn rejects_short_buffer() {
    assert!(decode_pump_curve(&[0u8; 48]).is_none());
    assert!(decode_pump_curve(&[]).is_none());
}

#[test]
fn rejects_non_canonical_bool() {
    let mut bytes = curve_bytes(1, 2, 3, 4, 2); // 2 is not a valid bool
    assert!(decode_pump_curve(&bytes).is_none());
    bytes[48] = 255;
    assert!(decode_pump_curve(&bytes).is_none());
}

#[test]
fn extra_trailing_bytes_are_ok() {
    let mut bytes = curve_bytes(11, 22, 33, 44, 1);
    bytes.extend_from_slice(&[9, 9, 9, 9]);
    let c = decode_pump_curve(&bytes).unwrap();
    assert_eq!(c.virtual_token, 11);
    assert_eq!(c.virtual_sol, 22);
}

#[test]
fn rejects_zero_discriminator() {
    // A well-formed 49-byte buffer with a zeroed discriminator must fail closed.
    let mut bytes = curve_bytes(1, 2, 3, 4, 0);
    bytes[0..8].copy_from_slice(&[0u8; 8]);
    assert!(decode_pump_curve(&bytes).is_none());
}

#[test]
fn rejects_foreign_program_discriminator() {
    // Same length, but the PumpSwap Pool discriminator — a foreign account type.
    let mut bytes = curve_bytes(1, 2, 3, 4, 0);
    bytes[0..8].copy_from_slice(&registry::account_discriminator(Venue::PumpSwap));
    assert!(decode_pump_curve(&bytes).is_none());
}
