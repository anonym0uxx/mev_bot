//! Leaf tests for point-in-time feature serving (constitution 20).
//!
//! Centerpiece: a property test proving the no-look-ahead guarantee across many
//! generated inputs with independently-computed expectations.

use pump_quant_features::timed_feature::{TimedFeature, TimedFeatureStore};
use pump_quant_features::types::Completeness;

/// Deterministic integer LCG (Numerical Recipes constants). Test-only generator —
/// no RNG in library logic; this seeds reproducible multi-input property cases.
struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn in_range(&mut self, lo: u64, hi: u64) -> u64 {
        // hi exclusive; lo < hi assumed.
        lo + self.next_u64() % (hi - lo)
    }
}

fn feat(value: i64, max_info: u64, comp_complete: u64) -> TimedFeature<i64> {
    TimedFeature::new(
        value,
        vec![value as u64],
        max_info,
        comp_complete,
        1,
        Completeness::Complete,
    )
}

/// Independent, brute-force reference for `as_of`: scan all features, keep those
/// servable at the cutoff, and pick the one with the greatest
/// `(max_information_time_ns, computation_complete_ns)`. Ties resolve to the last
/// such feature in iteration order (matching the store's "last wins" rule).
fn expected_as_of(feats: &[TimedFeature<i64>], cutoff: u64) -> Option<i64> {
    let mut best: Option<&TimedFeature<i64>> = None;
    for f in feats {
        if f.max_information_time_ns <= cutoff && f.computation_complete_ns <= cutoff {
            best = match best {
                None => Some(f),
                Some(b) => {
                    let fk = (f.max_information_time_ns, f.computation_complete_ns);
                    let bk = (b.max_information_time_ns, b.computation_complete_ns);
                    if fk >= bk {
                        Some(f)
                    } else {
                        Some(b)
                    }
                }
            };
        }
    }
    best.map(|f| f.value)
}

#[test]
fn empty_store_serves_nothing() {
    let store: TimedFeatureStore<i64> = TimedFeatureStore::with_capacity(8);
    assert!(store.as_of(0).is_none());
    assert!(store.as_of(u64::MAX).is_none());
}

#[test]
fn computation_time_gates_serving() {
    // Information is old (t=10) but computation finished late (t=100): the value
    // must not be servable before t=100, even though its inputs are ancient.
    let mut store = TimedFeatureStore::with_capacity(8);
    store.push(feat(7, 10, 100));
    assert!(
        store.as_of(99).is_none(),
        "not servable before computation done"
    );
    assert_eq!(store.as_of(100).map(|f| f.value), Some(7));
    assert_eq!(store.as_of(500).map(|f| f.value), Some(7));
}

#[test]
fn serves_freshest_information() {
    let mut store = TimedFeatureStore::with_capacity(8);
    store.push(feat(1, 10, 10));
    store.push(feat(2, 20, 20));
    store.push(feat(3, 30, 30));
    assert_eq!(store.as_of(9).map(|f| f.value), None);
    assert_eq!(store.as_of(10).map(|f| f.value), Some(1));
    assert_eq!(store.as_of(25).map(|f| f.value), Some(2));
    assert_eq!(store.as_of(100).map(|f| f.value), Some(3));
}

#[test]
fn push_order_does_not_matter() {
    // Inserting the same features in a scrambled order yields identical serving.
    let a = feat(1, 10, 12);
    let b = feat(2, 20, 20);
    let c = feat(3, 15, 30);
    let mut s1 = TimedFeatureStore::with_capacity(8);
    for f in [a.clone(), b.clone(), c.clone()] {
        s1.push(f);
    }
    let mut s2 = TimedFeatureStore::with_capacity(8);
    for f in [c, a, b] {
        s2.push(f);
    }
    for cutoff in [0u64, 11, 20, 29, 30, 31, 1000] {
        assert_eq!(
            s1.as_of(cutoff).map(|f| f.value),
            s2.as_of(cutoff).map(|f| f.value),
            "cutoff {cutoff}"
        );
    }
}

/// PROPERTY: no look-ahead. For every generated feature set and every cutoff T,
/// `as_of(T)` (a) is servable at T, (b) equals the brute-force reference, and
/// (c) is invariant to the removal of every feature whose `servable_at() > T`.
/// (c) is the operational definition of "no look-ahead": future data cannot
/// influence a past decision.
#[test]
fn property_no_look_ahead() {
    for seed in 0..400u64 {
        let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));
        let n = rng.in_range(1, 40) as usize;
        let mut feats: Vec<TimedFeature<i64>> = Vec::with_capacity(n);
        for i in 0..n {
            let info = rng.in_range(0, 1000);
            // computation completes at or after information time in most cases,
            // but sometimes before, to exercise the max() servability gate.
            let comp = if rng.next_u64().is_multiple_of(4) {
                rng.in_range(0, 1000)
            } else {
                info + rng.in_range(0, 50)
            };
            feats.push(feat(i as i64, info, comp));
        }

        // capacity == n so nothing is evicted; eviction is tested separately.
        let mut store = TimedFeatureStore::with_capacity(n);
        for f in &feats {
            store.push(f.clone());
        }

        for cutoff in (0..=1050).step_by(37) {
            let got = store.as_of(cutoff).map(|f| f.value);
            let expected = expected_as_of(&feats, cutoff);
            assert_eq!(got, expected, "seed {seed} cutoff {cutoff}: value mismatch");

            if let Some(f) = store.as_of(cutoff) {
                assert!(
                    f.is_servable_at(cutoff),
                    "seed {seed} cutoff {cutoff}: served a non-servable feature"
                );
            }

            // Invariance: a store built from ONLY the past-or-present features must
            // serve identically. This is the crux of the no-look-ahead guarantee.
            let mut past_only = TimedFeatureStore::with_capacity(n.max(1));
            for f in &feats {
                if f.servable_at() <= cutoff {
                    past_only.push(f.clone());
                }
            }
            assert_eq!(
                past_only.as_of(cutoff).map(|f| f.value),
                got,
                "seed {seed} cutoff {cutoff}: future data changed the answer"
            );
        }
    }
}

#[test]
fn eviction_is_bounded_and_keeps_recent() {
    // Capacity 3, push 6 ascending-servable features. The store keeps the last 3;
    // recent cutoffs are unaffected, only very-old cutoffs lose their answer.
    let mut store = TimedFeatureStore::with_capacity(3);
    for i in 0..6u64 {
        let t = (i + 1) * 10;
        store.push(feat(i as i64, t, t));
    }
    assert_eq!(store.len(), 3);
    assert!(store.capacity() >= store.len());
    // Freshest three are values 3,4,5 at times 40,50,60.
    assert_eq!(store.as_of(1000).map(|f| f.value), Some(5));
    assert_eq!(store.as_of(45).map(|f| f.value), Some(3));
    // The two oldest were evicted, so an early cutoff no longer resolves.
    assert_eq!(store.as_of(15).map(|f| f.value), None);
}
