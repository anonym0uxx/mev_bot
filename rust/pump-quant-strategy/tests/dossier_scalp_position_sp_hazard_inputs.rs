// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_hazard_inputs').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_covariates_not_in_cell_key() {
    let a = hazard_inputs(&ScalpPositionState::test(), &FlowState::test(), 9_000, 0);
    let b = hazard_inputs(&ScalpPositionState::test(), &FlowState::test(), 1_000, 8_000);
    assert_eq!(a.cell_key(), b.cell_key()); // covariates differ, cell identical
    assert_ne!(a.conviction_fp, b.conviction_fp);
}
