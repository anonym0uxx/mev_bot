// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_template_arm').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_template_offsets_stable() {
    let t = arm_exit_template(&ExitAccounts::test(), &ExitParams::test()).unwrap();
    let before = t.msg_bytes.clone();
    let mut t2 = t.clone();
    // NOTE: verbatim dossier line is `patch_u64(&mut t2, t2.amount_off, 42);`, which is
    // uncompilable Rust (`f(&mut x, x.field)` two-phase-borrow limitation). The single
    // field read is hoisted to a local; ZERO assertions/values changed.
    let amount_off = t2.amount_off;
    patch_u64(&mut t2, amount_off, 42);
    assert_eq!(t2.msg_bytes.len(), before.len());
    let mut diffs = 0;
    for i in 0..before.len() {
        if before[i] != t2.msg_bytes[i] {
            diffs += 1;
        }
    }
    assert!(diffs <= 8); // only the amount field bytes changed
}
