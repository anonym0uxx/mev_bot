//! Tests for the wallet signer.
//!
//! The signing path is exercised against RFC 8032 test vector 1, so the
//! signatures produced here are checkable against any other ed25519
//! implementation rather than only against ourselves.

use pq_stream_capture::signer::*;
use std::io::Write;
use std::path::PathBuf;

/// RFC 8032 §7.1 test vector 1.
const V1_SEED_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const V1_PUB_HEX: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
/// Base58 of the vector-1 public key, computed independently in Python.
const V1_ADDRESS: &str = "FVen3X669xLzsi6N2V91DoiyzHzg1uAgqiT8jZ9nS96Z";

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn keypair_json() -> String {
    let mut v = hex(V1_SEED_HEX);
    v.extend(hex(V1_PUB_HEX));
    let parts: Vec<String> = v.iter().map(|b| b.to_string()).collect();
    format!("[{}]", parts.join(","))
}

fn write_temp(name: &str, contents: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("pq-signer-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    p
}

// ────────────────────────────── base58 codec ─────────────────────────────────

#[test]
fn base58_matches_an_independently_computed_vector() {
    assert_eq!(encode_base58(&hex(V1_PUB_HEX)), V1_ADDRESS);
    assert_eq!(
        decode_base58_32(V1_ADDRESS).unwrap().to_vec(),
        hex(V1_PUB_HEX)
    );
}

#[test]
fn base58_round_trips_and_preserves_leading_zeros() {
    let mut b = [0u8; 32];
    b[31] = 1;
    let s = encode_base58(&b);
    assert!(s.starts_with('1'), "leading zero bytes encode as '1'");
    assert_eq!(decode_base58_32(&s).unwrap(), b);

    let all_zero = [0u8; 32];
    assert_eq!(encode_base58(&all_zero), "1".repeat(32));
    assert_eq!(decode_base58_32(&"1".repeat(32)).unwrap(), all_zero);
}

#[test]
fn negative_control_decoder_rejects_truncation_and_bad_charset() {
    // Truncated: still base58, still short enough to fit in 32 bytes, but its
    // leading zero bytes have no leading '1's to justify them.
    assert!(decode_base58_32(&V1_ADDRESS[..36]).is_none());
    assert!(decode_base58_32("").is_none());
    // 0, O, I, l are outside the alphabet.
    assert!(decode_base58_32("0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl").is_none());
    // Too long to fit in 32 bytes.
    assert!(decode_base58_32(&format!("{V1_ADDRESS}{V1_ADDRESS}")).is_none());
}

// ─────────────────────────────── load and sign ───────────────────────────────

#[test]
fn loads_a_valid_keypair_and_signs_verifiably() {
    let p = write_temp("valid.json", &keypair_json());
    let s = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap();

    assert_eq!(s.address(), V1_ADDRESS);
    assert_eq!(s.public_key_bytes(), &hex(V1_PUB_HEX)[..]);

    let msg = b"transfer 1 lamport";
    let sig = s.sign(msg).unwrap();
    assert_eq!(sig.len(), SIGNATURE_BYTES);
    assert!(verify_signature(s.public_key_bytes(), msg, &sig));

    // ed25519 is deterministic: the same message and key give the same bytes,
    // which is what makes a signature reproducible in replay.
    assert_eq!(s.sign(msg).unwrap(), sig);
}

#[test]
fn negative_control_signature_does_not_verify_for_a_different_message() {
    let p = write_temp("valid2.json", &keypair_json());
    let s = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap();
    let sig = s.sign(b"buy").unwrap();
    assert!(!verify_signature(s.public_key_bytes(), b"sell", &sig));

    // Nor for a different key.
    let mut other = hex(V1_PUB_HEX);
    other[0] ^= 0xff;
    assert!(!verify_signature(&other, b"buy", &sig));

    // Nor with a mutated signature.
    let mut bad = sig;
    bad[0] ^= 0x01;
    assert!(!verify_signature(s.public_key_bytes(), b"buy", &bad));
}

#[test]
fn self_test_passes_on_a_loaded_signer() {
    let p = write_temp("valid3.json", &keypair_json());
    let s = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap();
    assert!(s.self_test().is_ok());
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_wrong_expected_wallet_is_refused() {
    // The control that matters most: the file is a perfectly valid keypair, but
    // it is not the wallet the caller said it was loading.
    let p = write_temp("valid4.json", &keypair_json());
    let other = "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta";
    match WalletSigner::load_solana_keypair(&p, other) {
        Err(SignerError::WrongWallet { expected, found }) => {
            assert_eq!(expected, other);
            assert_eq!(found, V1_ADDRESS);
        }
        other => panic!("expected WrongWallet, got {other:?}"),
    }
}

#[test]
fn negative_control_inconsistent_keypair_is_refused() {
    // Public half does not derive from the secret half - a corrupted or
    // hand-edited file. It would otherwise sign, and every validator would
    // reject the result.
    let mut v = hex(V1_SEED_HEX);
    let mut pub_bytes = hex(V1_PUB_HEX);
    pub_bytes[0] ^= 0xff;
    v.extend(pub_bytes);
    let json = format!(
        "[{}]",
        v.iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let p = write_temp("inconsistent.json", &json);
    assert!(matches!(
        WalletSigner::load_solana_keypair(&p, V1_ADDRESS),
        Err(SignerError::InconsistentKeypair { .. })
    ));
}

#[test]
fn negative_control_malformed_files_are_refused() {
    let cases = [
        ("not_array.json", "9d61b19d"),
        ("short.json", "[1,2,3]"),
        ("too_many.json", &format!("[{}]", vec!["1"; 65].join(","))),
        (
            "not_bytes.json",
            &format!("[{}]", vec!["300"; 64].join(",")),
        ),
        (
            "empty_elem.json",
            &format!("[{},,]", vec!["1"; 62].join(",")),
        ),
        ("text.json", "[a,b,c]"),
    ];
    for (name, body) in cases {
        let p = write_temp(name, body);
        let r = WalletSigner::load_solana_keypair(&p, V1_ADDRESS);
        assert!(
            matches!(r, Err(SignerError::Malformed { .. })),
            "{name} should be Malformed, got {r:?}"
        );
    }
}

#[test]
fn negative_control_missing_file_is_refused() {
    let p = PathBuf::from("/nonexistent/pq-signer/nope.json");
    assert!(matches!(
        WalletSigner::load_solana_keypair(&p, V1_ADDRESS),
        Err(SignerError::Unreadable { .. })
    ));
}

#[test]
fn negative_control_oversize_file_is_refused_before_parsing() {
    // A wrong path pointed at a log or a core dump must not be parsed at all.
    let big = "0".repeat((MAX_KEYFILE_BYTES + 1) as usize);
    let p = write_temp("huge.json", &big);
    assert!(matches!(
        WalletSigner::load_solana_keypair(&p, V1_ADDRESS),
        Err(SignerError::TooLarge { .. })
    ));
}

#[test]
fn negative_control_bad_expected_address_is_refused_before_reading_the_file() {
    // Validating the caller's own claim first means a typo'd expected address
    // never causes a keyfile read.
    let p = PathBuf::from("/nonexistent/never-read.json");
    assert!(matches!(
        WalletSigner::load_solana_keypair(&p, "not-a-valid-address"),
        Err(SignerError::BadExpectedAddress { .. })
    ));
}

#[test]
fn negative_control_empty_and_oversize_messages_are_refused() {
    let p = write_temp("valid5.json", &keypair_json());
    let s = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap();
    assert!(matches!(
        s.sign(&[]),
        Err(SignerError::MessageRejected { bytes: 0, .. })
    ));
    let huge = vec![0u8; 1233];
    assert!(matches!(
        s.sign(&huge),
        Err(SignerError::MessageRejected { bytes: 1233, .. })
    ));
    // Exactly at the packet limit is fine.
    assert!(s.sign(&vec![0u8; 1232]).is_ok());
}

// ────────────────────────────── leak resistance ──────────────────────────────

#[test]
fn debug_output_carries_the_address_and_no_secret() {
    let p = write_temp("valid6.json", &keypair_json());
    let s = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap();
    let rendered = format!("{s:?}");
    assert!(rendered.contains(V1_ADDRESS));
    assert!(rendered.contains("<never printed>"));
    // The seed must not appear in any representation, in any encoding.
    assert!(!rendered.contains(V1_SEED_HEX));
    assert!(!rendered.contains(&encode_base58(&hex(V1_SEED_HEX))));
    for b in hex(V1_SEED_HEX) {
        // A byte-array debug print would show decimal elements; make sure the
        // whole sequence is not present.
        let _ = b;
    }
    assert!(!rendered.contains("157, 97, 177"));
}

#[test]
fn errors_never_carry_key_material() {
    let mut v = hex(V1_SEED_HEX);
    let mut pb = hex(V1_PUB_HEX);
    pb[0] ^= 0xff;
    v.extend(pb);
    let json = format!(
        "[{}]",
        v.iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let p = write_temp("leaky.json", &json);
    let e = WalletSigner::load_solana_keypair(&p, V1_ADDRESS).unwrap_err();
    let rendered = format!("{e}");
    assert!(!rendered.contains("157"));
    assert!(!rendered.contains(V1_SEED_HEX));
    assert!(rendered.contains("inconsistent"));
}
