//! Operator decision 2026-10-09 (SKIP Mayhem-mode coins), WIRE leg: the daemon's producer
//! `decode::curve_mode_observed` on REAL captured BondingCurve account bytes (proc/cont_wire05.ndjson, 151-byte
//! accounts; same fixtures as pump-quant-protocol/tests/curve_mode_capture.rs), and its round trip through the
//! v3 event codec. An undecodable account emits NOTHING (the engine keeps the mode UNKNOWN and refuses).
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_junction::decode::curve_mode_observed;
use pump_quant_junction::event_codec::{decode, encode, is_critical};

const CANON_60: &str = "17b7f83760d8ac60d0fdc72c3b8a02005270b27b0a000000d065b5e0a98b010052c48e7f030000000080c6a47e8d030000d62966c646c527ce45e3570a5873a89dbd0557e591709b9c53f2e83c6cd6f14d00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const MAYHEM_377: &str = "17b7f83760d8ac608726676479c10100e0e0d11000000000878e5418e8c20000e8eed504000000000080c6a47e8d0300000b8f2c61b1b84977fbee179a39382d2d292f4e5f2d57cbf2a648c3dd55734dbb01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

fn bytes(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

const MB: [u8; 32] = [7u8; 32];

#[test]
fn captured_mayhem_account_produces_a_mayhem_mode_event() {
    let pe = curve_mode_observed(&MB, &bytes(MAYHEM_377), 377).expect("decodable");
    assert_eq!(
        pe.event,
        AppEvent::CurveModeObserved {
            mint: Mint(MB),
            mayhem: true,
            slot: 377
        }
    );
}

#[test]
fn captured_canonical_account_produces_a_canonical_mode_event() {
    let pe = curve_mode_observed(&MB, &bytes(CANON_60), 60).expect("decodable");
    assert_eq!(
        pe.event,
        AppEvent::CurveModeObserved {
            mint: Mint(MB),
            mayhem: false,
            slot: 60
        }
    );
}

#[test]
fn an_undecodable_account_emits_no_mode_event_never_a_false() {
    let b = bytes(MAYHEM_377);
    assert!(
        curve_mode_observed(&MB, &b[..49], 1).is_none(),
        "old layout"
    );
    assert!(curve_mode_observed(&MB, &b[..82], 1).is_none(), "truncated");
    let mut foreign = b.clone();
    foreign[0] ^= 1;
    assert!(curve_mode_observed(&MB, &foreign, 1).is_none(), "foreign");
    let mut junk = b;
    junk[81] = 2;
    assert!(
        curve_mode_observed(&MB, &junk, 1).is_none(),
        "noncanonical bool"
    );
}

#[test]
fn the_mode_event_round_trips_and_is_a_critical_kind() {
    let ev = curve_mode_observed(&MB, &bytes(MAYHEM_377), 377)
        .unwrap()
        .event;
    let line = encode(&ev, 377);
    let (kind, back) = decode(&line).expect("decodes");
    assert_eq!(kind, "CurveModeObserved");
    assert_eq!(back, ev);
    assert!(
        is_critical("CurveModeObserved"),
        "its loss makes a replay incomplete"
    );
    // A line missing the flag is REJECTED, never defaulted to false.
    let stripped = line.replace(r#""mayhem":true,"#, "");
    assert_ne!(stripped, line);
    assert!(decode(&stripped).is_err());
}
