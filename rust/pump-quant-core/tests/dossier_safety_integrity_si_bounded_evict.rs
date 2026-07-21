// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_bounded_evict').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

assert never exceeds N; assert oldest evicted; assert deterministic order; assert len/cap exposed
