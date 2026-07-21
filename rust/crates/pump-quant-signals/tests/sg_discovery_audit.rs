//! Tests for the launch-discovery completeness auditor (criterion 73).
//! Expectations computed independently over multiple inputs incl. edge cases.

use pump_quant_signals::discovery_audit::*;

#[test]
fn complete_when_all_known_observed() {
    let known = [10, 20, 30];
    let observed = [30, 20, 10, 99]; // superset + extra.
    let a = audit_launch_coverage(&known, &observed);
    assert_eq!(a.known_count, 3);
    assert_eq!(a.observed_count, 4);
    assert_eq!(a.matched_count, 3);
    assert_eq!(a.recall_bps, 10_000);
    assert!(a.missing.is_empty());
    assert_eq!(a.unexpected, vec![99]);
    assert_eq!(a.verdict(), CoverageVerdict::Complete);
}

#[test]
fn incomplete_reports_shortfall_and_recall() {
    // known 4, observed covers 3 of them -> recall 7500 bps, missing = [40].
    let known = [10, 20, 30, 40];
    let observed = [10, 20, 30];
    let a = audit_launch_coverage(&known, &observed);
    assert_eq!(a.matched_count, 3);
    assert_eq!(a.recall_bps, 7_500);
    assert_eq!(a.missing, vec![40]);
    assert_eq!(
        a.verdict(),
        CoverageVerdict::Incomplete {
            shortfall: vec![40],
            recall_bps: 7_500,
        }
    );
}

#[test]
fn shortfall_is_sorted_and_deduplicated() {
    // Duplicates collapse; missing sorted ascending.
    let known = [50, 40, 30, 20, 10, 10];
    let observed = [10, 30, 30];
    let a = audit_launch_coverage(&known, &observed);
    assert_eq!(a.known_count, 5); // deduped
    assert_eq!(a.matched_count, 2); // 10 and 30
                                    // recall = 2*10000/5 = 4000.
    assert_eq!(a.recall_bps, 4_000);
    assert_eq!(a.missing, vec![20, 40, 50]);
}

#[test]
fn empty_known_universe_is_vacuously_complete() {
    let a = audit_launch_coverage(&[], &[1, 2, 3]);
    assert_eq!(a.recall_bps, 10_000);
    assert_eq!(a.verdict(), CoverageVerdict::Complete);
    assert_eq!(a.unexpected, vec![1, 2, 3]);
}

#[test]
fn observed_empty_zero_recall() {
    let a = audit_launch_coverage(&[1, 2], &[]);
    assert_eq!(a.recall_bps, 0);
    assert_eq!(
        a.verdict(),
        CoverageVerdict::Incomplete {
            shortfall: vec![1, 2],
            recall_bps: 0,
        }
    );
}
