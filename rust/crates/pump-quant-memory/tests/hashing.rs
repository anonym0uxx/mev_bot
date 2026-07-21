//! Leaf: hashing. Verifies the deterministic FNV-1a fingerprint against published
//! independent test vectors and checks the canonical framing is collision-safe.

use pump_quant_memory::hashing::{fnv1a_64, push_bytes};

#[test]
fn matches_published_fnv1a_64_vectors() {
    // Canonical FNV-1a 64-bit reference vectors (independent of our impl).
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
}

#[test]
fn deterministic_same_input_same_hash() {
    let a = fnv1a_64(b"pump-quant");
    let b = fnv1a_64(b"pump-quant");
    assert_eq!(a, b);
}

#[test]
fn different_input_different_hash() {
    assert_ne!(fnv1a_64(b"pump-quant"), fnv1a_64(b"pump-quan7"));
}

#[test]
fn length_prefix_prevents_field_splice_collision() {
    // Without length-prefixing, ("ab","c") and ("a","bc") would encode to the same
    // bytes. With it, they must differ.
    let mut x = Vec::new();
    push_bytes(&mut x, b"ab");
    push_bytes(&mut x, b"c");

    let mut y = Vec::new();
    push_bytes(&mut y, b"a");
    push_bytes(&mut y, b"bc");

    assert_ne!(x, y);
    assert_ne!(fnv1a_64(&x), fnv1a_64(&y));
}
