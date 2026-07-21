#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
//! Property + example tests for the criterion-20 latency percentile estimator.
//!
//! Expectations are computed independently of the histogram implementation
//! (brute-force sorted vectors, closed-form bucket arithmetic), across many
//! inputs including edge cases, so a memorized answer cannot pass.

use pump_quant_core::latency::*;

/// Independent reference for the inclusive `[lo, hi]` a value must fall in,
/// derived straight from the documented layout rather than from `bucket_bounds`.
fn ref_bounds(ns: u64) -> (u64, u64) {
    if ns < SUB_COUNT {
        return (ns, ns);
    }
    let bits = 64 - ns.leading_zeros();
    let exp = bits - SUB_BITS;
    let shift = exp - 1;
    let lo = (ns >> shift) << shift; // clear the low `shift` bits
    let hi = lo + ((1u64 << shift) - 1);
    (lo, hi)
}

#[test]
fn bucket_index_and_bounds_contain_value() {
    // Edge cases + a spread across the whole u64 range.
    let mut samples = vec![
        0u64,
        1,
        63,
        64,
        65,
        127,
        128,
        129,
        1_000,
        1_023,
        1_024,
        1_000_000,
        1_000_000_000,
        u64::MAX / 2,
        u64::MAX - 1,
        u64::MAX,
    ];
    // Add a deterministic geometric sweep.
    let mut v = 1u64;
    while v < u64::MAX / 3 {
        samples.push(v);
        samples.push(v + 1);
        samples.push(v.wrapping_sub(1).max(1));
        v = v.saturating_mul(3).saturating_add(7);
    }

    for &ns in &samples {
        let i = bucket_index(ns);
        assert!(i < NUM_BUCKETS, "index {i} out of range for ns={ns}");
        let (lo, hi) = bucket_bounds(i).expect("in-range index has bounds");
        assert!(
            lo <= ns && ns <= hi,
            "ns={ns} not inside bucket {i} = [{lo},{hi}]"
        );
        // Cross-check against the independent reference bounds.
        assert_eq!((lo, hi), ref_bounds(ns), "bounds mismatch for ns={ns}");
    }
}

#[test]
fn top_bucket_upper_bound_is_u64_max() {
    let i = bucket_index(u64::MAX);
    let (_lo, hi) = bucket_bounds(i).unwrap();
    assert_eq!(
        hi,
        u64::MAX,
        "final bucket must cap at u64::MAX without wrap"
    );
}

#[test]
fn bucket_index_is_monotonic_nondecreasing() {
    let mut prev = 0usize;
    let mut v = 0u64;
    // Walk a dense low range then a sparse high range.
    for step in [1u64, 7, 53, 1_009, 1_000_003, 999_999_937] {
        for _ in 0..300 {
            let i = bucket_index(v);
            assert!(i >= prev, "index dropped at v={v}: {i} < {prev}");
            prev = i;
            v = match v.checked_add(step) {
                Some(n) => n,
                None => break,
            };
        }
    }
}

#[test]
fn empty_histogram_returns_none() {
    let h = LatencyHistogram::new();
    assert!(h.is_empty());
    assert_eq!(h.count(), 0);
    assert_eq!(h.quantile(P50), None);
    assert_eq!(h.p99(), None);
    assert_eq!(h.min(), None);
    assert_eq!(h.max(), None);
}

#[test]
fn exact_small_values_match_hand_computed_nearest_rank() {
    // All values < SUB_COUNT ⇒ every bucket is exact (hi == value), so the
    // quantile is exactly the nearest-rank order statistic. Values 0..60, N=60.
    let mut h = LatencyHistogram::new();
    for v in 0..60u64 {
        h.record(v);
    }
    assert_eq!(h.count(), 60);
    // rank = ceil(p*60/100000): p50->30, p95->57, p99->60, p999->60.
    // Sorted values are 0..=59, so the k-th smallest (1-indexed) is k-1.
    assert_eq!(h.p50(), Some(29)); // rank 30 -> value 29
    assert_eq!(h.p95(), Some(56)); // rank 57 -> value 56
    assert_eq!(h.p99(), Some(59)); // rank 60 -> value 59
    assert_eq!(h.p999(), Some(59)); // rank 60 -> value 59
                                    // p0/p100 endpoints.
    assert_eq!(h.quantile(0), Some(0)); // rank clamped to 1 -> smallest = 0
    assert_eq!(h.quantile(QUANTILE_SCALE), Some(59)); // rank 60 -> largest = 59
                                                      // Over-range p is clamped, not wrapped.
    assert_eq!(h.quantile(QUANTILE_SCALE + 500), Some(59));
}

#[test]
fn general_quantiles_match_bruteforce_order_statistic() {
    // Deterministic pseudo-samples spanning several octaves. Expectation is the
    // upper bound of the bucket holding the nearest-rank order statistic,
    // computed from an independently-sorted copy of the raw samples.
    let mut raw = Vec::new();
    let mut x = 12_345u64;
    for _ in 0..5_000 {
        // Simple LCG for reproducible spread; only used to build test inputs.
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        // Spread across ~ns..~ms latencies.
        let ns = (x >> 20) % 5_000_000;
        raw.push(ns);
    }

    let mut h = LatencyHistogram::new();
    for &ns in &raw {
        h.record(ns);
    }
    assert_eq!(h.count() as usize, raw.len());

    let mut sorted = raw.clone();
    sorted.sort_unstable();
    let n = sorted.len() as u64;

    for &p in &[0u64, P50, P95, P99, P999, 25_000, 75_000, QUANTILE_SCALE] {
        // Independent nearest-rank: rank = max(1, ceil(p*n/SCALE)).
        let rank = (p as u128 * n as u128).div_ceil(QUANTILE_SCALE as u128) as u64;
        let rank = rank.max(1);
        let order_stat = sorted[(rank - 1) as usize];
        // The histogram reports that value's bucket upper bound.
        let (_lo, hi) = ref_bounds(order_stat);
        assert_eq!(
            h.quantile(p),
            Some(hi),
            "quantile mismatch at p={p} (rank={rank}, order_stat={order_stat})"
        );
    }
}

#[test]
fn quantiles_are_monotonic_nondecreasing() {
    let mut h = LatencyHistogram::new();
    let mut x = 1u64;
    for _ in 0..2_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        h.record((x >> 16) % 2_000_000);
    }
    let p50 = h.p50().unwrap();
    let p95 = h.p95().unwrap();
    let p99 = h.p99().unwrap();
    let p999 = h.p999().unwrap();
    assert!(p50 <= p95, "p50={p50} > p95={p95}");
    assert!(p95 <= p99, "p95={p95} > p99={p99}");
    assert!(p99 <= p999, "p99={p99} > p999={p999}");
    assert!(h.min().unwrap() <= p50);
    assert!(p999 <= h.max().unwrap());
}

#[test]
fn merge_equals_recording_union() {
    let a_vals: Vec<u64> = (0..500).map(|i| (i * 37 + 3) % 100_000).collect();
    let b_vals: Vec<u64> = (0..800).map(|i| (i * 911 + 17) % 5_000_000).collect();

    let mut a = LatencyHistogram::new();
    for &v in &a_vals {
        a.record(v);
    }
    let mut b = LatencyHistogram::new();
    for &v in &b_vals {
        b.record(v);
    }
    a.merge(&b);

    // Independent: record the union into a fresh histogram.
    let mut union = LatencyHistogram::new();
    for &v in a_vals.iter().chain(b_vals.iter()) {
        union.record(v);
    }

    assert_eq!(a.count(), union.count());
    for &p in &[P50, P95, P99, P999] {
        assert_eq!(a.quantile(p), union.quantile(p), "merge diverged at p={p}");
    }
}

#[test]
fn single_sample_reports_its_bucket_for_every_percentile() {
    let ns = 1_234_567u64;
    let mut h = LatencyHistogram::new();
    h.record(ns);
    let (_lo, hi) = ref_bounds(ns);
    for &p in &[0u64, P50, P95, P99, P999, QUANTILE_SCALE] {
        assert_eq!(h.quantile(p), Some(hi));
    }
}
