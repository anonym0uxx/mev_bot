// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'determinants' component (leaf 'dt_d6_integrity').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_social::determinants::*;

#[test]
fn dt_d6_integrity_prop() {
    // Clean record with disclosure: 10000 - 0 - 0 + 1500 - 5000(recentre) = 6500.
    // conf = 10000*10/(10+20) = 3333.
    let clean = IntegrityEvidence {
        deleted_losing_calls: 0,
        total_losing_calls: 4,
        edit_count: 0,
        total_calls: 10,
        disclosure_present: true,
    };
    let c = d6_integrity(&clean);
    assert_eq!(c.value_bps, 6_500);
    assert_eq!(c.sample_size, 10);
    assert_eq!(c.confidence_bps, 3_333);

    // Full scrubber, no disclosure: 10000 - 10000 - 0 + 0 - 5000 = -5000.
    let scrubber = IntegrityEvidence {
        deleted_losing_calls: 4,
        total_losing_calls: 4,
        edit_count: 0,
        total_calls: 10,
        disclosure_present: false,
    };
    assert_eq!(d6_integrity(&scrubber).value_bps, -5_000);

    // Edit penalty is capped at 3000: 20 edits * 300 = 6000 -> capped 3000.
    // 10000 - 0 - 3000 + 0 - 5000 = 2000.
    let edited = IntegrityEvidence {
        deleted_losing_calls: 0,
        total_losing_calls: 4,
        edit_count: 20,
        total_calls: 10,
        disclosure_present: false,
    };
    assert_eq!(d6_integrity(&edited).value_bps, 2_000);

    // Edge: no calls -> empty score.
    let empty = IntegrityEvidence {
        deleted_losing_calls: 0,
        total_losing_calls: 0,
        edit_count: 0,
        total_calls: 0,
        disclosure_present: true,
    };
    let e = d6_integrity(&empty);
    assert_eq!(e.value_bps, 0);
    assert_eq!(e.sample_size, 0);
    assert_eq!(e.confidence_bps, 0);
}
