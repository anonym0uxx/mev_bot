// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'rank' component (leaf 'wr_recency_factor').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_watchlist::rank::*;

fn mk_cand_unused() {}

#[test]
fn wr_recency_factor_props() {
    // Full recency at age 0 (now == discovered_at).
    assert_eq!(recency_factor(0, 0, 100), RECENCY_ONE);
    assert_eq!(recency_factor(50, 50, 100), RECENCY_ONE);
    // Zero at exactly TTL and beyond.
    assert_eq!(recency_factor(0, 100, 100), 0);
    assert_eq!(recency_factor(0, 250, 100), 0);
    // ttl == 0 disables the lane.
    assert_eq!(recency_factor(0, 0, 0), 0);
    // Future discovery (now < discovered_at) saturates age to 0 => full, never negative.
    assert_eq!(recency_factor(500, 100, 1000), RECENCY_ONE);
    // Exact closed form + monotonic non-increasing across the whole horizon.
    let ttl = 1000u64;
    let mut prev = u64::MAX;
    for age in 0..=ttl {
        let r = recency_factor(0, age, ttl);
        let expected = if age >= ttl {
            0
        } else {
            ((RECENCY_ONE as u128) * ((ttl - age) as u128) / (ttl as u128)) as u64
        };
        assert_eq!(r, expected, "closed form mismatch at age {age}");
        assert!(r <= prev, "recency must not increase with age at {age}");
        prev = r;
    }
    // Spot value.
    assert_eq!(recency_factor(0, 250, 1000), 750_000);
}
