// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'decode' component (leaf 'decode_pump_curve_identity_and_fields').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_protocol::decode::*;

#[test]
fn decode_pump_curve_identity_and_fields() {
    const PF_DISC: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
    const PS_DISC: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
    fn curve_bytes(
        disc: [u8; 8],
        v_token: u64,
        v_sol: u64,
        r_token: u64,
        r_sol: u64,
        complete: u8,
    ) -> Vec<u8> {
        let mut b = vec![0u8; 49];
        b[0..8].copy_from_slice(&disc);
        b[8..16].copy_from_slice(&v_token.to_le_bytes());
        b[16..24].copy_from_slice(&v_sol.to_le_bytes());
        b[24..32].copy_from_slice(&r_token.to_le_bytes());
        b[32..40].copy_from_slice(&r_sol.to_le_bytes());
        b[48] = complete;
        b
    }
    let samples: [(u64, u64, u64, u64); 4] = [
        (0, 0, 0, 0),
        (100, 200, 300, 400),
        (
            1_072_000_000_000_000,
            30_000_000_000,
            793_100_000_000_000,
            0,
        ),
        (u64::MAX, u64::MAX, u64::MAX, u64::MAX),
    ];
    for &(vt, vs, rt, rs) in samples.iter() {
        for &flag in &[0u8, 1u8] {
            let b = curve_bytes(PF_DISC, vt, vs, rt, rs, flag);
            let c = decode_pump_curve(&b).expect("valid identity must decode");
            assert_eq!(c.virtual_token, vt);
            assert_eq!(c.virtual_sol, vs);
            assert_eq!(c.real_token, rt);
            assert_eq!(c.real_sol, rs);
            assert_eq!(c.complete, flag == 1);
        }
    }
    let c = decode_pump_curve(&curve_bytes(PF_DISC, 5, 6, 7, 8, 1)).unwrap();
    assert_eq!(c.virtual_token, 5);
    assert_eq!(c.virtual_sol, 6);
    assert_eq!(c.real_token, 7);
    assert_eq!(c.real_sol, 8);
    assert!(c.complete);
    assert!(decode_pump_curve(&curve_bytes([0u8; 8], 5, 6, 7, 8, 0)).is_none());
    assert!(decode_pump_curve(&curve_bytes(PS_DISC, 5, 6, 7, 8, 0)).is_none());
    let mut flipped = PF_DISC;
    flipped[0] ^= 0x01;
    assert!(decode_pump_curve(&curve_bytes(flipped, 5, 6, 7, 8, 0)).is_none());
    for bad in [2u8, 3, 128, 255] {
        assert!(decode_pump_curve(&curve_bytes(PF_DISC, 1, 2, 3, 4, bad)).is_none());
    }
    for len in 0..49usize {
        let short = vec![PF_DISC[0]; len];
        assert!(
            decode_pump_curve(&short).is_none(),
            "len {} must be rejected",
            len
        );
    }
    let mut nearly = curve_bytes(PF_DISC, 1, 2, 3, 4, 0);
    nearly.truncate(48);
    assert!(decode_pump_curve(&nearly).is_none());
}
