// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_zone' component (leaf 'zone_ordinal_ordering').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::entry_zone::*;

#[test]
fn gd_zone_ordinal_dense_and_monotone() {
    let zones = [
        EntryZone::Sub5kPreAttention,
        EntryZone::Band5kTo9kEarlyValidation,
        EntryZone::Band9kTo20kTarget,
        EntryZone::Band20kTo50kMomentumConfirmed,
        EntryZone::PreMigrationLate,
        EntryZone::MigrationEdge,
        EntryZone::PostMigrationRevival,
    ];

    // Ordinals are the dense sequence 0..7 in declaration order.
    for (i, z) in zones.iter().enumerate() {
        assert_eq!(z.ordinal(), i as u8);
    }

    // Ordinal order matches the enum's derived Ord (deterministic output order).
    for w in zones.windows(2) {
        assert!(w[0].ordinal() < w[1].ordinal());
        assert!(w[0] < w[1]);
    }

    // Well-formedness of the standard ladder is what the classifier relies on.
    assert!(ZoneThresholds::standard().is_well_formed());
    let bad = ZoneThresholds {
        sub5k: 5_000,
        early9k: 9_000,
        target20k: 9_000, // equal -> not strictly ascending
        momentum50k: 50_000,
    };
    assert!(!bad.is_well_formed());
}
