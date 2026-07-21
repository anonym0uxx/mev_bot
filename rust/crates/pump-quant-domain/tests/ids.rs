//! Tests for identifier newtypes: Mint hex codec, Slot arithmetic, id ordering.
//! Expectations are computed independently of the crate under test.

use pump_quant_domain::ids::{Mint, ParseMintError, ProviderId, Slot, SourceId, TradeId};

/// Independent reference hex encoder (used to cross-check `Mint::to_hex`).
fn ref_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::new();
    for b in bytes {
        // Two-digit lowercase, computed with plain integer math.
        let hi = b / 16;
        let lo = b % 16;
        for nib in [hi, lo] {
            let c = if nib < 10 {
                (b'0' + nib) as char
            } else {
                (b'a' + (nib - 10)) as char
            };
            s.push(c);
        }
    }
    s
}

#[test]
fn mint_hex_roundtrip_multiple_inputs() {
    // Multiple inputs incl. edge cases: all-zero, all-0xff, ascending,
    // descending, and a mixed pattern.
    let mut ascending = [0u8; 32];
    let mut descending = [0u8; 32];
    let mut mixed = [0u8; 32];
    for i in 0..32u8 {
        ascending[i as usize] = i;
        descending[i as usize] = 255 - i;
        mixed[i as usize] = i.wrapping_mul(37).wrapping_add(11);
    }
    let cases = [[0u8; 32], [0xffu8; 32], ascending, descending, mixed];

    for bytes in cases {
        let mint = Mint::from_bytes(bytes);
        let expected = ref_hex(&bytes);
        assert_eq!(mint.to_hex(), expected, "to_hex mismatch");
        assert_eq!(mint.to_hex().len(), 64);
        // Display matches to_hex.
        assert_eq!(format!("{mint}"), expected);
        // Round-trip through parser.
        let parsed = Mint::from_hex(&expected).expect("valid hex parses");
        assert_eq!(parsed, mint);
        assert_eq!(parsed.as_bytes(), &bytes);
    }
}

#[test]
fn mint_from_hex_accepts_prefix_and_uppercase() {
    // Known vector: byte 0xDE,0xAD then zeros — computed by hand.
    let mut bytes = [0u8; 32];
    bytes[0] = 0xDE;
    bytes[1] = 0xAD;
    let lower = ref_hex(&bytes); // starts with "dead"
    assert!(lower.starts_with("dead0000"));

    let upper = lower.to_uppercase();
    let prefixed = format!("0x{lower}");
    let expected = Mint::from_bytes(bytes);
    assert_eq!(Mint::from_hex(&lower).unwrap(), expected);
    assert_eq!(Mint::from_hex(&upper).unwrap(), expected);
    assert_eq!(Mint::from_hex(&prefixed).unwrap(), expected);
}

#[test]
fn mint_from_hex_rejects_bad_input() {
    // Wrong length.
    assert_eq!(
        Mint::from_hex("abcd"),
        Err(ParseMintError::BadLength { found: 4 })
    );
    // Correct length, one bad char at the very end ('g').
    let mut s = "0".repeat(63);
    s.push('g');
    assert_eq!(
        Mint::from_hex(&s),
        Err(ParseMintError::BadChar { byte: b'g' })
    );
    // 0x prefix that leaves 63 chars => bad length 63.
    let s2 = format!("0x{}", "0".repeat(63));
    assert_eq!(
        Mint::from_hex(&s2),
        Err(ParseMintError::BadLength { found: 63 })
    );
}

#[test]
fn mint_zero_constant() {
    assert_eq!(Mint::ZERO, Mint::from_bytes([0u8; 32]));
    assert_eq!(Mint::ZERO.to_hex(), "0".repeat(64));
}

#[test]
fn mint_ord_is_lexicographic_over_bytes() {
    let a = Mint::from_bytes([0u8; 32]);
    let mut b_bytes = [0u8; 32];
    b_bytes[0] = 1;
    let b = Mint::from_bytes(b_bytes);
    let mut c_bytes = [0u8; 32];
    c_bytes[31] = 1;
    let c = Mint::from_bytes(c_bytes);
    // Most-significant byte dominates: a < c < b.
    assert!(a < c);
    assert!(c < b);
    assert!(a < b);
}

#[test]
fn slot_distance_and_next() {
    // distance_to saturates at zero going backward.
    assert_eq!(Slot(10).distance_to(Slot(25)), 15);
    assert_eq!(Slot(25).distance_to(Slot(10)), 0);
    assert_eq!(Slot(7).distance_to(Slot(7)), 0);
    // next saturates at u64::MAX.
    assert_eq!(Slot(41).next(), Slot(42));
    assert_eq!(Slot(u64::MAX).next(), Slot(u64::MAX));
    // Ordering.
    assert!(Slot(1) < Slot(2));
}

#[test]
fn opaque_ids_order_hash_default() {
    use std::collections::BTreeSet;
    // Ordering follows the wrapped integer.
    assert!(TradeId(1) < TradeId(2));
    assert!(SourceId(1) < SourceId(9));
    assert!(ProviderId(3) < ProviderId(4));
    // Default is zero.
    assert_eq!(TradeId::default(), TradeId(0));
    assert_eq!(SourceId::default(), SourceId(0));
    assert_eq!(ProviderId::default(), ProviderId(0));
    // Hash/Eq usable as set keys; dedup works.
    let set: BTreeSet<TradeId> = [TradeId(5), TradeId(5), TradeId(1)].into_iter().collect();
    assert_eq!(set.len(), 2);
    assert_eq!(set.iter().next(), Some(&TradeId(1)));
}
