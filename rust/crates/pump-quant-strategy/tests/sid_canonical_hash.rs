//! Leaf sid_canonical_hash: reproducible StrategyId canonical digest (criterion 33).

use pump_quant_strategy::strategy_id::{
    fnv1a_64, strategy_id_hash, SizingFamily, StrategyConfig, FNV_OFFSET, FNV_PRIME,
};

/// Independent reference FNV-1a implementation for cross-checking.
fn ref_fnv1a(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

#[test]
fn fnv_matches_independent_reference_multiple_inputs() {
    let cases: [&[u8]; 5] = [
        b"",
        b"a",
        b"abc",
        b"the quick brown fox",
        &[0, 255, 128, 1, 2],
    ];
    for c in cases {
        assert_eq!(fnv1a_64(c), ref_fnv1a(c), "mismatch for {c:?}");
    }
    // Known FNV-1a 64 vectors.
    assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
}

#[test]
fn identical_configs_hash_equal() {
    let a = StrategyConfig::test();
    let b = StrategyConfig::test();
    assert_eq!(strategy_id_hash(&a), strategy_id_hash(&b));
    // And equals the digest over the canonical bytes computed independently.
    assert_eq!(strategy_id_hash(&a), ref_fnv1a(&a.canonical_bytes()));
}

#[test]
fn any_field_change_changes_hash() {
    let base = StrategyConfig::test();
    let h = strategy_id_hash(&base);

    let mut c = base.clone();
    c.name.push('!');
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.entry_mode += 1;
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.archetype += 1;
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.sizing = SizingFamily::LogUtility;
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.params_fp.push(0);
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.params_fp[0] += 1;
    assert_ne!(strategy_id_hash(&c), h);

    let mut c = base.clone();
    c.feature_schema_version += 1;
    assert_ne!(strategy_id_hash(&c), h);
}

#[test]
fn canonical_framing_avoids_delimiter_collisions() {
    // Two configs that would collide under naive concatenation must differ under
    // length-framed canonical bytes: name "ab"+param vs "a"+different param.
    let mut a = StrategyConfig::test();
    a.name = "ab".to_string();
    a.params_fp = vec![1];
    let mut b = StrategyConfig::test();
    b.name = "a".to_string();
    b.params_fp = vec![1];
    assert_ne!(strategy_id_hash(&a), strategy_id_hash(&b));
    assert_ne!(a.canonical_bytes(), b.canonical_bytes());
}
