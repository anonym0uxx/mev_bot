//! Leaf: `hashing`. Determinism (order-independence), sensitivity, domain
//! separation, and composition against the independently-validated primitives.

use pump_quant_governance::canonical::CanonicalValue;
use pump_quant_governance::hashing::{
    evaluator_release_hash, strategy_hash, EVALUATOR_DOMAIN, STRATEGY_DOMAIN,
};
use pump_quant_governance::sha256::Sha256;
use std::collections::BTreeMap;

fn sample_config(size_bps: i128) -> CanonicalValue {
    let mut params = BTreeMap::new();
    params.insert("size_bps".to_string(), CanonicalValue::I128(size_bps));
    params.insert("max_hold_ms".to_string(), CanonicalValue::U64(30_000));
    params.insert("into_strength".to_string(), CanonicalValue::Bool(true));
    let mut cfg = BTreeMap::new();
    cfg.insert(
        "lane".to_string(),
        CanonicalValue::Text("scalp".to_string()),
    );
    cfg.insert("params".to_string(), CanonicalValue::Map(params));
    CanonicalValue::Map(cfg)
}

/// Same logical config built with different map insertion orders → equal hash.
#[test]
fn strategy_hash_is_order_independent() {
    let mut a = BTreeMap::new();
    a.insert("x".to_string(), CanonicalValue::U64(1));
    a.insert("y".to_string(), CanonicalValue::U64(2));
    let mut b = BTreeMap::new();
    b.insert("y".to_string(), CanonicalValue::U64(2));
    b.insert("x".to_string(), CanonicalValue::U64(1));
    assert_eq!(
        strategy_hash(&CanonicalValue::Map(a)),
        strategy_hash(&CanonicalValue::Map(b))
    );
}

/// Any change to any field changes the strategy hash.
#[test]
fn strategy_hash_is_sensitive() {
    assert_ne!(
        strategy_hash(&sample_config(250)),
        strategy_hash(&sample_config(251))
    );
}

/// The same config hashed as a strategy vs an evaluator release must differ
/// (domain separation prevents cross-role identity reuse).
#[test]
fn domains_are_separated() {
    let cfg = sample_config(250);
    assert_ne!(strategy_hash(&cfg).0, evaluator_release_hash(&cfg).0);
}

/// Composition check: the strategy hash equals
/// `SHA-256( len(domain) || domain || canonical_encoding )`, computed here from
/// the independently NIST-validated `Sha256` and the byte-tested canonical
/// encoder — an expectation derived without calling `strategy_hash` itself.
#[test]
fn hash_matches_independent_composition() {
    let cfg = sample_config(777);

    let mut expect = Sha256::new();
    expect.update(&(STRATEGY_DOMAIN.len() as u64).to_be_bytes());
    expect.update(STRATEGY_DOMAIN);
    expect.update(&cfg.encode());
    assert_eq!(strategy_hash(&cfg).0, expect.finalize());

    let mut expect_eval = Sha256::new();
    expect_eval.update(&(EVALUATOR_DOMAIN.len() as u64).to_be_bytes());
    expect_eval.update(EVALUATOR_DOMAIN);
    expect_eval.update(&cfg.encode());
    assert_eq!(evaluator_release_hash(&cfg).0, expect_eval.finalize());
}

/// Reproducible across repeated calls, and hex form is 64 chars.
#[test]
fn hash_is_reproducible_and_hex_formatted() {
    let cfg = sample_config(100);
    assert_eq!(strategy_hash(&cfg), strategy_hash(&cfg));
    assert_eq!(strategy_hash(&cfg).to_hex().len(), 64);
    assert_eq!(evaluator_release_hash(&cfg).to_hex().len(), 64);
}
