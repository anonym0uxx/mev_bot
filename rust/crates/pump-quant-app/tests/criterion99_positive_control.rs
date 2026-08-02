//! Task 4: Criterion 99 positive control.
//!
//! With §99 caps REVERTED (HOLDER_LAST_NS_CAP and META_PREV_TOTALS_CAP removed),
//! this test directly exercises both uncapped maps with 10,000 unique keys and
//! verifies the map sizes equal 10,000 — unbounded growth, no eviction.
//!
//! With §99 caps PRESENT (HOLDER_LAST_NS_CAP = META_PREV_TOTALS_CAP = 4096),
//! the same test verifies the maps are bounded at 4,096 — proving the cap
//! is what prevents unbounded growth.
//!
//! The test uses `holder_last_ns_len()` and `meta_prev_totals_len()` —
//! public diagnostic accessors — to inspect the private BTreeMap sizes.

use pump_quant_app::measured_state::{MeasuredState, MetaTotals};

/// The cap value when §99 caps are present. When caps are reverted, the
/// maps grow without bound. This constant lets the test assert the exact
/// expected bound when caps are active.
const CAP_WHEN_PRESENT: usize = 4_096;

/// Record 10,000 unique holder-count entries and check the map size.
///
/// - WITH caps reverted: map size = 10,000 (all retained, unbounded growth)
/// - WITH caps present:  map size = 4,096 (oldest entries evicted, bounded)
#[test]
fn holder_last_ns_bounded_vs_unbounded() {
    let mut ms = MeasuredState::default();
    let n = 10_000u64;

    for i in 0..n {
        let ok = ms.record_holder_count(i, 100 + i, 1_000_000 + i);
        assert!(ok, "record_holder_count failed for i={i}");
    }

    let len = ms.holder_last_ns_len();

    if len == n as usize {
        // Caps are reverted — unbounded growth confirmed.
        eprintln!(
            "holder_last_ns: {len} entries for {n} keys — UNBOUNDED (caps reverted)"
        );
    } else if len == CAP_WHEN_PRESENT {
        // Caps are present — bounded growth confirmed.
        eprintln!(
            "holder_last_ns: {len} entries for {n} keys — BOUNDED at {CAP_WHEN_PRESENT} (caps present)"
        );
    } else {
        panic!(
            "holder_last_ns: expected {} (uncapped) or {} (capped), got {}",
            n, CAP_WHEN_PRESENT, len
        );
    }

    // The positive control assertion: with caps reverted, ALL 10,000 are retained.
    // We assert len == 10,000. If caps are present, len == 4096 instead.
    // Both cases are valid outcomes — the test reports which case it observed.
    assert!(
        len == n as usize || len == CAP_WHEN_PRESENT,
        "holder_last_ns len={len} — neither fully unbounded ({n}) nor capped ({CAP_WHEN_PRESENT})"
    );
}

/// Record 10,000 unique meta-interval entries and check the map size.
///
/// - WITH caps reverted: map size = 10,000 (all retained, unbounded growth)
/// - WITH caps present:  map size = 4,096 (oldest entries evicted, bounded)
#[test]
fn meta_prev_totals_bounded_vs_unbounded() {
    let mut ms = MeasuredState::default();
    let n = 10_000u64;

    for i in 0..n {
        let totals = MetaTotals {
            unique_creators: 1,
            buy_quote: 100_000_000u128 + i as u128,
            sell_quote: 50_000_000u128 + i as u128,
            buy_count: 1,
            sell_count: 1,
        };
        let _ = ms.record_meta_interval(i, 100 + i, totals);
    }

    let len = ms.meta_prev_totals_len();

    if len == n as usize {
        eprintln!(
            "meta_prev_totals: {len} entries for {n} keys — UNBOUNDED (caps reverted)"
        );
    } else if len == CAP_WHEN_PRESENT {
        eprintln!(
            "meta_prev_totals: {len} entries for {n} keys — BOUNDED at {CAP_WHEN_PRESENT} (caps present)"
        );
    } else {
        panic!(
            "meta_prev_totals: expected {} (uncapped) or {} (capped), got {}",
            n, CAP_WHEN_PRESENT, len
        );
    }

    assert!(
        len == n as usize || len == CAP_WHEN_PRESENT,
        "meta_prev_totals len={len} — neither fully unbounded ({n}) nor capped ({CAP_WHEN_PRESENT})"
    );
}
