// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_no_cut_guard').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_anti_pin() {
    use ExitClass::*;
    // fabricated acceleration cannot pin the position open:
    assert!(time_stop_binds(true, true, true, 1_000, 5_000, Normal));
    // authentic fresh acceleration suppresses the clock:
    assert!(!time_stop_binds(true, true, true, 9_000, 5_000, Normal));
    // stale flow never suppresses:
    assert!(time_stop_binds(true, true, false, 9_000, 5_000, Normal));
    // emergency always binds:
    assert!(time_stop_binds(false, true, true, 9_000, 5_000, Emergency));
}
