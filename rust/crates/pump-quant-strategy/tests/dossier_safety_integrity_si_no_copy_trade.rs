// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_no_copy_trade').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

assert every skipped stage blocks; assert no direct ExternalSignal->Order; assert Order has no mirror-source field
