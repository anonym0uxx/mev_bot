//! FIPS 180-4 / NIST vectors for the protocol-crate SHA-256 copy — the same
//! vectors that pin `pump-quant-governance`'s implementation, so the two
//! copies cannot silently diverge.

use pump_quant_protocol::sha256::{sha256, to_hex, Sha256};

#[test]
fn nist_empty_string() {
    assert_eq!(
        to_hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn nist_abc() {
    assert_eq!(
        to_hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn nist_448_bit_message() {
    assert_eq!(
        to_hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn nist_one_million_a() {
    let mut h = Sha256::new();
    let chunk = [b'a'; 1000];
    for _ in 0..1000 {
        h.update(&chunk);
    }
    assert_eq!(
        to_hex(&h.finalize()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

/// Streaming across arbitrary chunk boundaries must equal one-shot.
#[test]
fn streaming_equals_one_shot() {
    let msg = b"pump-quant protocol sha256 streaming equivalence vector";
    let mut h = Sha256::new();
    for chunk in msg.chunks(7) {
        h.update(chunk);
    }
    assert_eq!(h.finalize(), sha256(msg));
}
