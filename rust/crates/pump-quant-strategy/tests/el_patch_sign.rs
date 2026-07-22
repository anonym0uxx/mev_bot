#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
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
