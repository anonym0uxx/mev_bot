use pump_quant_evaluator::evaluator_pin::*;

// Independently-computed expectations: published FNV-1a-64 test vectors.
// "" -> 0xcbf29ce484222325, "a" -> 0xaf63dc4c8601ec8c,
// "foobar" -> 0x85944171f73967e8. A hardcoded/constant implementation fails
// across these three distinct inputs.
#[test]
fn fnv1a_matches_known_vectors() {
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
}

// Reconstruct the single-byte vector from first principles: hash = offset ^ b,
// then * prime (wrapping). This is an independent computation, not a reuse of
// the module's loop.
#[test]
fn fnv1a_single_byte_from_first_principles() {
    let offset: u64 = 0xcbf2_9ce4_8422_2325;
    let prime: u64 = 0x0000_0100_0000_01b3;
    let b = b'a' as u64;
    let expected = (offset ^ b).wrapping_mul(prime);
    assert_eq!(fnv1a_64(b"a"), expected);
}

#[test]
fn verify_accepts_matching_pin_and_rejects_mutation() {
    let artifact = b"evaluator-v1-config";
    let good = PinnedDigest(fnv1a_64(artifact));
    assert_eq!(verify_evaluator_pin(artifact, good), PinVerdict::Verified);
    assert!(verify_evaluator_pin(artifact, good).is_verified());

    // Flip one byte: digest must differ, verdict must be Mismatch carrying both.
    let mutated = b"evaluator-v2-config";
    match verify_evaluator_pin(mutated, good) {
        PinVerdict::Mismatch { computed, pinned } => {
            assert_eq!(pinned, good.0);
            assert_eq!(computed, fnv1a_64(mutated));
            assert_ne!(computed, pinned);
        }
        PinVerdict::Verified => panic!("mutated artifact must not verify"),
    }
}

#[test]
fn accept_if_pinned_gates_results() {
    let artifact = b"frozen-evaluator";
    let pin = PinnedDigest(fnv1a_64(artifact));
    // Verified -> result flows through.
    assert_eq!(accept_if_pinned(artifact, pin, 42u32), Ok(42));
    // Mismatch -> result refused.
    let wrong = PinnedDigest(pin.0 ^ 0xFF);
    assert!(accept_if_pinned(artifact, wrong, 42u32).is_err());
}
