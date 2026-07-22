// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_zone' component (leaf 'classify_entry_zone_taxonomy').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::entry_zone::*;

#[test]
fn gd_classify_entry_zone_taxonomy() {
    let th = ZoneThresholds::standard();
    let p = MigrationPhase::PreMigration;

    // Half-open [lo, hi) cap ladder for pre-migration.
    assert_eq!(classify_entry_zone(0, p, th), EntryZone::Sub5kPreAttention);
    assert_eq!(
        classify_entry_zone(4_999, p, th),
        EntryZone::Sub5kPreAttention
    );
    assert_eq!(
        classify_entry_zone(5_000, p, th),
        EntryZone::Band5kTo9kEarlyValidation
    );
    assert_eq!(
        classify_entry_zone(8_999, p, th),
        EntryZone::Band5kTo9kEarlyValidation
    );
    assert_eq!(
        classify_entry_zone(9_000, p, th),
        EntryZone::Band9kTo20kTarget
    );
    assert_eq!(
        classify_entry_zone(19_999, p, th),
        EntryZone::Band9kTo20kTarget
    );
    assert_eq!(
        classify_entry_zone(20_000, p, th),
        EntryZone::Band20kTo50kMomentumConfirmed
    );
    assert_eq!(
        classify_entry_zone(49_999, p, th),
        EntryZone::Band20kTo50kMomentumConfirmed
    );
    // At/above the top edge, a still-pre-migration token is Late, not a band.
    assert_eq!(
        classify_entry_zone(50_000, p, th),
        EntryZone::PreMigrationLate
    );
    assert_eq!(
        classify_entry_zone(u64::MAX, p, th),
        EntryZone::PreMigrationLate
    );

    // Phase dominates the cap band entirely.
    assert_eq!(
        classify_entry_zone(0, MigrationPhase::Migrating, th),
        EntryZone::MigrationEdge
    );
    assert_eq!(
        classify_entry_zone(u64::MAX, MigrationPhase::Migrating, th),
        EntryZone::MigrationEdge
    );
    assert_eq!(
        classify_entry_zone(0, MigrationPhase::PostMigration, th),
        EntryZone::PostMigrationRevival
    );
    assert_eq!(
        classify_entry_zone(u64::MAX, MigrationPhase::PostMigration, th),
        EntryZone::PostMigrationRevival
    );
}

#[test]
#[should_panic(expected = "strictly ascending")]
fn gd_classify_entry_zone_rejects_ill_formed() {
    let bad = ZoneThresholds {
        sub5k: 5_000,
        early9k: 5_000, // not strictly ascending
        target20k: 20_000,
        momentum50k: 50_000,
    };
    let _ = classify_entry_zone(1_000, MigrationPhase::PreMigration, bad);
}
