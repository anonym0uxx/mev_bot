// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_failed_sell_state').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

assert FailedSell and TerminalLoss are distinct from Closed(0); loss includes fixed cost
