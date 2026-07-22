use pump_quant_evaluator::holdout_overlap::*;
use std::collections::BTreeSet;

fn set(ids: &[u64]) -> BTreeSet<FamilyId> {
    ids.iter().copied().map(FamilyId).collect()
}

#[test]
fn disjoint_sets_are_clean() {
    let train = set(&[1, 2, 3]);
    let holdout = set(&[4, 5, 6]);
    let o = holdout_overlap(&train, &holdout);
    assert!(o.is_clean);
    assert_eq!(o.leak_count(), 0);
    assert!(o.leaked.is_empty());
}

#[test]
fn shared_families_reported_sorted() {
    let train = set(&[1, 2, 3, 7, 9]);
    let holdout = set(&[9, 3, 10]);
    // Intersection = {3, 9}, reported ascending.
    let o = holdout_overlap(&train, &holdout);
    assert!(!o.is_clean);
    assert_eq!(o.leaked, vec![FamilyId(3), FamilyId(9)]);
    assert_eq!(o.leak_count(), 2);
}

#[test]
fn single_shared_family_flags_leak() {
    let train = set(&[100]);
    let holdout = set(&[100]);
    let o = holdout_overlap(&train, &holdout);
    assert!(!o.is_clean);
    assert_eq!(o.leaked, vec![FamilyId(100)]);
}

#[test]
fn empty_sets_are_clean() {
    let empty: BTreeSet<FamilyId> = BTreeSet::new();
    assert!(holdout_overlap(&empty, &empty).is_clean);
    assert!(holdout_overlap(&set(&[1, 2]), &empty).is_clean);
    assert!(holdout_overlap(&empty, &set(&[1, 2])).is_clean);
}
