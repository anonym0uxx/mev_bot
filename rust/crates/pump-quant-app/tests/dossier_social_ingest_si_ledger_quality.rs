// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_ingest' component (leaf 'si_ledger_quality').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::social_ingest::*;

#[test]
fn si_ledger_quality_unseen_is_public_burned() {
    let ledger = pump_quant_social::ledger::SourceQualityLedger::with_capacity(8);
    let policy = SourceQualityPolicy::conservative();
    // A source the ledger has never reconciled earns only the baseline floor.
    assert_eq!(ledger_quality(&ledger, 999, &policy), policy.baseline_bp);
    // The baseline is a conservative, non-alpha floor (well under full weight).
    assert!(policy.baseline_bp < policy.pre_flow_alpha_bp);
    assert!(policy.baseline_bp <= 2_000);
}
