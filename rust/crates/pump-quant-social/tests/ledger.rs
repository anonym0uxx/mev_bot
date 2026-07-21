//! Leaf tests: the bounded SourceQualityLedger aggregate (§29.8 / §29.9).

use pump_quant_social::classification::{ClassificationConfig, DeterminantBundle};
use pump_quant_social::ledger::SourceQualityLedger;
use pump_quant_social::types::{DeterminantScore, SourceState};

fn score(v: i64) -> DeterminantScore {
    DeterminantScore {
        value_bps: v,
        sample_size: 30,
        confidence_bps: 5_000,
    }
}

fn alpha_bundle() -> DeterminantBundle {
    DeterminantBundle {
        d1: score(2_000),
        d2: score(5_000),
        d3: score(2_000),
        d4: score(0),
        d5: score(0),
        d6: score(0),
        d7: score(5_000),
        d8: score(5_000),
        d9: score(0),
        d10: score(0),
        shill_suspect: false,
        post_peak_persistent: false,
        bot_farm: false,
        echo_heavy: false,
        total_sample: 30,
    }
}

fn insufficient_bundle() -> DeterminantBundle {
    let mut b = alpha_bundle();
    b.total_sample = 3;
    b
}

#[test]
fn fold_stores_and_reads_back() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut ledger = SourceQualityLedger::with_capacity(8);
    assert!(ledger.is_empty());

    let c = ledger.fold(100, &alpha_bundle(), &cfg);
    assert_eq!(c.state, SourceState::PreFlowAlpha);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger.get(100).unwrap().state, SourceState::PreFlowAlpha);
    assert!(ledger.get(999).is_none());
}

#[test]
fn fold_updates_existing_without_growing() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut ledger = SourceQualityLedger::with_capacity(8);
    ledger.fold(100, &alpha_bundle(), &cfg);
    // Re-classify the same source as insufficient → same slot, updated state.
    ledger.fold(100, &insufficient_bundle(), &cfg);
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger.get(100).unwrap().state,
        SourceState::InsufficientSample
    );
}

#[test]
fn capacity_is_bounded_by_lru_eviction() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut ledger = SourceQualityLedger::with_capacity(2);
    ledger.fold(1, &alpha_bundle(), &cfg); // seq 0
    ledger.fold(2, &alpha_bundle(), &cfg); // seq 1
    ledger.fold(3, &alpha_bundle(), &cfg); // seq 2 → evicts source 1 (LRU)

    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger.capacity(), 2);
    assert!(ledger.get(1).is_none()); // evicted
    assert!(ledger.get(2).is_some());
    assert!(ledger.get(3).is_some());
}

#[test]
fn touching_a_source_protects_it_from_eviction() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut ledger = SourceQualityLedger::with_capacity(2);
    ledger.fold(1, &alpha_bundle(), &cfg); // seq 0
    ledger.fold(2, &alpha_bundle(), &cfg); // seq 1
    ledger.fold(1, &alpha_bundle(), &cfg); // seq 2 → source 1 now most-recent
    ledger.fold(3, &alpha_bundle(), &cfg); // seq 3 → evicts source 2 (now LRU)

    assert!(ledger.get(1).is_some());
    assert!(ledger.get(2).is_none());
    assert!(ledger.get(3).is_some());
}

#[test]
fn zero_capacity_is_clamped_to_one() {
    let mut ledger = SourceQualityLedger::with_capacity(0);
    assert_eq!(ledger.capacity(), 1);
    let cfg = ClassificationConfig::fade_first_default();
    ledger.fold(1, &alpha_bundle(), &cfg);
    ledger.fold(2, &alpha_bundle(), &cfg);
    assert_eq!(ledger.len(), 1);
}
