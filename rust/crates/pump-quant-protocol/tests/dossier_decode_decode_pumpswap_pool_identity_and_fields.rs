// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'decode' component (leaf 'decode_pumpswap_pool_identity_and_fields').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_protocol::decode::*;

#[test]
fn decode_pumpswap_pool_identity_and_fields() {
    const PF_DISC: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
    const PS_DISC: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
    fn pool_bytes(disc: [u8; 8], bump: u8, index: u16, base: u64, quote: u64, lp: u64) -> Vec<u8> {
        let mut b = vec![0u8; 35];
        b[0..8].copy_from_slice(&disc);
        b[8] = bump;
        b[9..11].copy_from_slice(&index.to_le_bytes());
        b[11..19].copy_from_slice(&base.to_le_bytes());
        b[19..27].copy_from_slice(&quote.to_le_bytes());
        b[27..35].copy_from_slice(&lp.to_le_bytes());
        b
    }
    let samples: [(u8, u16, u64, u64, u64); 4] = [
        (0, 0, 0, 0, 0),
        (254, 7, 123_456_789, 987_654_321, 1_000_000),
        (255, 65_535, u64::MAX, u64::MAX, u64::MAX),
        (1, 513, 1, 2, 3),
    ];
    for &(bump, index, base, quote, lp) in samples.iter() {
        let b = pool_bytes(PS_DISC, bump, index, base, quote, lp);
        let p = decode_pumpswap_pool(&b).expect("valid identity must decode");
        assert_eq!(p.pool_bump, bump);
        assert_eq!(p.index, index);
        assert_eq!(p.base_reserve, base);
        assert_eq!(p.quote_reserve, quote);
        assert_eq!(p.lp_supply, lp);
    }
    let p = decode_pumpswap_pool(&pool_bytes(
        PS_DISC,
        u8::MAX,
        u16::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
    ))
    .unwrap();
    assert_eq!(p.pool_bump, 255);
    assert_eq!(p.index, 65_535);
    assert_eq!(p.base_reserve, u64::MAX);
    assert_eq!(p.quote_reserve, u64::MAX);
    assert_eq!(p.lp_supply, u64::MAX);
    assert!(decode_pumpswap_pool(&pool_bytes([0u8; 8], 1, 2, 3, 4, 5)).is_none());
    assert!(decode_pumpswap_pool(&pool_bytes(PF_DISC, 1, 2, 3, 4, 5)).is_none());
    let mut flipped = PS_DISC;
    flipped[7] ^= 0x80;
    assert!(decode_pumpswap_pool(&pool_bytes(flipped, 1, 2, 3, 4, 5)).is_none());
    for len in 0..35usize {
        let short = vec![PS_DISC[len % 8]; len];
        assert!(
            decode_pumpswap_pool(&short).is_none(),
            "len {} must be rejected",
            len
        );
    }
    let mut nearly = pool_bytes(PS_DISC, 1, 2, 3, 4, 5);
    nearly.truncate(34);
    assert!(decode_pumpswap_pool(&nearly).is_none());
    let mut extra = pool_bytes(PS_DISC, 9, 10, 11, 12, 13);
    extra.extend_from_slice(&[7u8; 40]);
    let p = decode_pumpswap_pool(&extra).unwrap();
    assert_eq!(p.base_reserve, 11);
    assert_eq!(p.lp_supply, 13);
}
