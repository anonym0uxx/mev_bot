//! Leaf: `sha256`. Verified against published FIPS 180-4 / NIST test vectors
//! (independent, externally-computed expectations) plus streaming/one-shot
//! equivalence over multiple chunkings.

use pump_quant_governance::sha256::{sha256, to_hex, Sha256};

/// NIST FIPS 180-4 example vectors — expected digests are the published values,
/// not anything this crate produced.
#[test]
fn nist_known_answer_vectors() {
    assert_eq!(
        to_hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        to_hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        to_hex(&sha256(b"a")),
        "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
    );
    // 56-byte (448-bit) message — exercises the extra-block padding path.
    assert_eq!(
        to_hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

/// The classic one-million-'a' vector: many blocks, exact published digest.
#[test]
fn nist_long_message_vector() {
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

/// Streaming in arbitrary chunk sizes must equal the one-shot digest, for
/// several messages and several chunkings (property over multiple inputs).
#[test]
fn streaming_matches_one_shot() {
    let messages: [&[u8]; 4] = [
        b"",
        b"the quick brown fox",
        &[0u8; 64],     // exactly one block
        &[0xabu8; 130], // spans three blocks with a partial tail
    ];
    for msg in messages {
        let expected = sha256(msg);
        for chunk_size in [1usize, 3, 7, 16, 31, 64, 127] {
            let mut h = Sha256::new();
            for part in msg.chunks(chunk_size.max(1)) {
                h.update(part);
            }
            assert_eq!(h.finalize(), expected, "chunk_size={chunk_size}");
        }
    }
}

/// A single-bit change flips the digest (avalanche sanity, not a proof).
#[test]
fn distinct_inputs_distinct_digests() {
    assert_ne!(sha256(b"governance"), sha256(b"Governance"));
    assert_ne!(sha256(b"a"), sha256(b"aa"));
}
