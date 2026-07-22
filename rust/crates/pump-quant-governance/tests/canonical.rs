//! Leaf: `canonical`. Exact-byte encoding assertions (independently computed by
//! hand), order-independence of maps, and injectivity across types.

use pump_quant_governance::canonical::CanonicalValue;
use std::collections::BTreeMap;

/// Hand-computed exact encodings. Each expectation is written out byte-by-byte
/// from the documented wire format (tag, then fixed 8-byte BE length where
/// applicable, then body), not read back from the encoder.
#[test]
fn exact_byte_encodings() {
    assert_eq!(CanonicalValue::Bool(true).encode(), vec![0x01, 0x01]);
    assert_eq!(CanonicalValue::Bool(false).encode(), vec![0x01, 0x00]);

    // U64(1): tag 0x02, then 00 00 00 00 00 00 00 01.
    assert_eq!(
        CanonicalValue::U64(1).encode(),
        vec![0x02, 0, 0, 0, 0, 0, 0, 0, 1]
    );

    // I128(-1): tag 0x03, then sixteen 0xFF bytes (two's complement BE).
    let mut expect_i128 = vec![0x03u8];
    expect_i128.extend_from_slice(&[0xFFu8; 16]);
    assert_eq!(CanonicalValue::I128(-1).encode(), expect_i128);

    // Text("ab"): tag 0x05, len=2 (8-byte BE), then 'a','b'.
    assert_eq!(
        CanonicalValue::Text("ab".to_string()).encode(),
        vec![0x05, 0, 0, 0, 0, 0, 0, 0, 2, 0x61, 0x62]
    );

    // Bytes([0xDE,0xAD]): tag 0x04, len=2, then payload.
    assert_eq!(
        CanonicalValue::Bytes(vec![0xDE, 0xAD]).encode(),
        vec![0x04, 0, 0, 0, 0, 0, 0, 0, 2, 0xDE, 0xAD]
    );
}

/// A map hashes/encodes independently of insertion order (BTreeMap canonicality).
#[test]
fn map_encoding_is_insertion_order_independent() {
    let mut a = BTreeMap::new();
    a.insert("alpha".to_string(), CanonicalValue::U64(1));
    a.insert("beta".to_string(), CanonicalValue::U64(2));
    a.insert("gamma".to_string(), CanonicalValue::U64(3));

    let mut b = BTreeMap::new();
    // Reverse insertion order.
    b.insert("gamma".to_string(), CanonicalValue::U64(3));
    b.insert("beta".to_string(), CanonicalValue::U64(2));
    b.insert("alpha".to_string(), CanonicalValue::U64(1));

    assert_eq!(
        CanonicalValue::Map(a).encode(),
        CanonicalValue::Map(b).encode()
    );
}

/// Changing any value or key changes the encoding.
#[test]
fn map_encoding_is_sensitive() {
    let base = {
        let mut m = BTreeMap::new();
        m.insert("k".to_string(), CanonicalValue::U64(1));
        CanonicalValue::Map(m)
    };
    let changed_value = {
        let mut m = BTreeMap::new();
        m.insert("k".to_string(), CanonicalValue::U64(2));
        CanonicalValue::Map(m)
    };
    let changed_key = {
        let mut m = BTreeMap::new();
        m.insert("k2".to_string(), CanonicalValue::U64(1));
        CanonicalValue::Map(m)
    };
    assert_ne!(base.encode(), changed_value.encode());
    assert_ne!(base.encode(), changed_key.encode());
}

/// Distinct types with the "same" scalar do not collide (type tags disambiguate),
/// and length prefixing prevents a value being another's prefix.
#[test]
fn distinct_types_do_not_collide() {
    assert_ne!(
        CanonicalValue::U64(1).encode(),
        CanonicalValue::I128(1).encode()
    );
    // List of one U64(1) vs a bare U64(1): different framing, never equal.
    assert_ne!(
        CanonicalValue::List(vec![CanonicalValue::U64(1)]).encode(),
        CanonicalValue::U64(1).encode()
    );
    // "ab" as Text vs Bytes: same body bytes, different tag.
    assert_ne!(
        CanonicalValue::Text("AB".to_string()).encode(),
        CanonicalValue::Bytes(vec![0x41, 0x42]).encode()
    );
    // List order is significant.
    assert_ne!(
        CanonicalValue::List(vec![CanonicalValue::U64(1), CanonicalValue::U64(2)]).encode(),
        CanonicalValue::List(vec![CanonicalValue::U64(2), CanonicalValue::U64(1)]).encode()
    );
}

/// Nested structures encode deterministically and stably.
#[test]
fn nested_encoding_is_deterministic() {
    let build = || {
        let mut inner = BTreeMap::new();
        inner.insert("size_bps".to_string(), CanonicalValue::I128(250));
        inner.insert("enabled".to_string(), CanonicalValue::Bool(true));
        let mut outer = BTreeMap::new();
        outer.insert(
            "lane".to_string(),
            CanonicalValue::Text("scalp".to_string()),
        );
        outer.insert("params".to_string(), CanonicalValue::Map(inner));
        CanonicalValue::Map(outer)
    };
    assert_eq!(build().encode(), build().encode());
}
