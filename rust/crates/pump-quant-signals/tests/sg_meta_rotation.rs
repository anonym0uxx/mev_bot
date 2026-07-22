//! Tests for the §21.4 MetaRotationState time-safe category-assignment
//! validator (criterion 81). Expectations computed independently.

use pump_quant_signals::meta_rotation::*;

fn tax() -> Taxonomy {
    Taxonomy::new(3, [1u32, 2, 5])
}

fn assignment(cat: u32, ver: u32, a_ts: u64, o_ts: u64) -> CategoryAssignment {
    CategoryAssignment {
        token: 777,
        category_id: cat,
        taxonomy_version: ver,
        assignment_ts_ms: a_ts,
        observation_ts_ms: o_ts,
    }
}

#[test]
fn accepts_valid_pinned_non_retroactive() {
    // version 3 matches, category 2 exists, assignment 100 <= observation 200.
    let a = assignment(2, 3, 100, 200);
    assert_eq!(validate_assignment(&a, &tax()), AssignmentVerdict::Accepted);
    // assignment_ts == observation_ts is allowed (boundary).
    let b = assignment(5, 3, 200, 200);
    assert_eq!(validate_assignment(&b, &tax()), AssignmentVerdict::Accepted);
}

#[test]
fn rejects_retroactive_future_dated_assignment() {
    // assignment 300 > observation 200 -> look-ahead leakage.
    let a = assignment(2, 3, 300, 200);
    assert_eq!(
        validate_assignment(&a, &tax()),
        AssignmentVerdict::RejectedRetroactive
    );
}

#[test]
fn rejects_unpinned_taxonomy_version() {
    // assignment claims version 2 but active taxonomy is version 3.
    let a = assignment(2, 2, 100, 200);
    assert_eq!(
        validate_assignment(&a, &tax()),
        AssignmentVerdict::RejectedTaxonomyUnpinned
    );
}

#[test]
fn rejects_unknown_category() {
    // category 9 not in {1,2,5}.
    let a = assignment(9, 3, 100, 200);
    assert_eq!(
        validate_assignment(&a, &tax()),
        AssignmentVerdict::RejectedUnknownCategory
    );
}

#[test]
fn version_check_precedes_category_check() {
    // Both version wrong AND category unknown: version rejection wins (priority).
    let a = assignment(9, 99, 100, 200);
    assert_eq!(
        validate_assignment(&a, &tax()),
        AssignmentVerdict::RejectedTaxonomyUnpinned
    );
}

#[test]
fn taxonomy_contains_helper() {
    let t = tax();
    assert!(t.contains(5));
    assert!(!t.contains(4));
    assert_eq!(t.version, 3);
}
