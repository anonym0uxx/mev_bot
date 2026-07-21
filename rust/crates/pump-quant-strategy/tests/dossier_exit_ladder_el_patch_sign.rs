// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'exit_ladder' component (leaf 'el_patch_sign').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_strategy::exit_ladder::*;

#[test]
fn prop_patch_idempotent_and_exact() {
    let mut t = arm_exit_template(&ExitAccounts::test(), &ExitParams::test()).unwrap();
    let bh = [7u8; 32];
    patch_and_finalize(&mut t, &bh, 123, 100).unwrap();
    let snap = t.msg_bytes.clone();
    patch_and_finalize(&mut t, &bh, 123, 100).unwrap();
    assert_eq!(snap, t.msg_bytes);
    assert_eq!(&t.msg_bytes[t.blockhash_off..t.blockhash_off + 32], &bh);
}
