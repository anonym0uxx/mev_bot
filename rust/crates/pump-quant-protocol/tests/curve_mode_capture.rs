//! Curve mode flags decoded from REAL captured account bytes (proc/cont_wire05.ndjson, 151-byte BondingCurve accounts).
//! Canonical curve 4kmGqTmf.. (lines 60 -> 617, a direct pump buy between) keeps `vsol - real_sol` = 30 SOL;
//! Mayhem curve 9zsF8bqM.. (lines 377 -> 519, a Mayhem-routed buy between) moves it. See proc/OFFSET_bM3a_REPORT.md.
use pump_quant_protocol::decode::{decode_pump_curve, decode_pump_curve_mode, PumpCurveMode};

const CANON_60: &str = "17b7f83760d8ac60d0fdc72c3b8a02005270b27b0a000000d065b5e0a98b010052c48e7f030000000080c6a47e8d030000d62966c646c527ce45e3570a5873a89dbd0557e591709b9c53f2e83c6cd6f14d00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const CANON_617: &str = "17b7f83760d8ac60f3bfe4c80a830200db31b3990a000000f327d27c79840100db858f9d030000000080c6a47e8d030000d62966c646c527ce45e3570a5873a89dbd0557e591709b9c53f2e83c6cd6f14d00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const MAYHEM_377: &str = "17b7f83760d8ac608726676479c10100e0e0d11000000000878e5418e8c20000e8eed504000000000080c6a47e8d0300000b8f2c61b1b84977fbee179a39382d2d292f4e5f2d57cbf2a648c3dd55734dbb01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const MAYHEM_519: &str = "17b7f83760d8ac60f6bbc2d02db90100f128af1100000000f623b0849cba0000e1e42605000000000080c6a47e8d0300000b8f2c61b1b84977fbee179a39382d2d292f4e5f2d57cbf2a648c3dd55734dbb01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

fn bytes(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn offset(h: &str) -> u64 {
    let c = decode_pump_curve(&bytes(h)).unwrap();
    c.virtual_sol - c.real_sol
}

#[test]
fn captured_canonical_curve_is_not_mayhem_and_keeps_the_30_sol_offset() {
    for h in [CANON_60, CANON_617] {
        assert_eq!(bytes(h).len(), 151);
        assert_eq!(
            decode_pump_curve_mode(&bytes(h)),
            Some(PumpCurveMode {
                mayhem: false,
                cashback: false
            })
        );
        assert_eq!(offset(h), 30_000_000_000);
    }
}

#[test]
fn captured_mayhem_curve_is_flagged_and_its_offset_moves_across_a_trade() {
    for h in [MAYHEM_377, MAYHEM_519] {
        assert_eq!(
            decode_pump_curve_mode(&bytes(h)),
            Some(PumpCurveMode {
                mayhem: true,
                cashback: false
            })
        );
    }
    // Same reserves the program's TradeEvent reported (tx 4Fo9sxkk.., Mayhem-routed buy of 5,305,849 lamports).
    let c = decode_pump_curve(&bytes(MAYHEM_519)).unwrap();
    assert_eq!(
        (c.virtual_sol, c.virtual_token, c.real_sol, c.real_token),
        (
            296_691_953,
            485_081_403_800_566,
            86_435_041,
            205_181_403_800_566
        )
    );
    assert_eq!(offset(MAYHEM_377), 201_060_856);
    assert_eq!(offset(MAYHEM_519), 210_256_912);
    // Token offset is the canonical constant on both: only the SOL side is re-parameterised.
    let a = decode_pump_curve(&bytes(MAYHEM_377)).unwrap();
    assert_eq!(a.virtual_token - a.real_token, 279_900_000_000_000);
    assert_eq!(c.virtual_token - c.real_token, 279_900_000_000_000);
}

#[test]
fn mode_is_unknown_not_false_on_short_foreign_or_noncanonical_accounts() {
    let b = bytes(MAYHEM_377);
    // Older 49-byte layout: reserves decode, mode is UNKNOWN (never "not mayhem").
    assert!(decode_pump_curve(&b[..49]).is_some());
    assert_eq!(decode_pump_curve_mode(&b[..49]), None);
    assert_eq!(decode_pump_curve_mode(&b[..82]), None);
    let mut foreign = b.clone();
    foreign[0] ^= 1;
    assert_eq!(decode_pump_curve_mode(&foreign), None);
    let mut junk = b;
    junk[81] = 2;
    assert_eq!(decode_pump_curve_mode(&junk), None);
}
