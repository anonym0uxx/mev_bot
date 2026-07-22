// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'cpu_numa_tuning' component (leaf 'cn_os_apply').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::cpu_numa_tuning::*;

#[test]
fn prop_apply_verifies_readback() {
    let recs = vec![ProcRecord::core(0, 0b0011), ProcRecord::core(0, 0b1100)];
    let t = parse_topology(&recs).unwrap();
    let plan = derive_plan(&t, &[HotThreadSpec::test("x")]).unwrap();
    let mut ok = MockOs::faithful();
    let r = apply_plan(&mut ok, &plan, Prio::High);
    assert!(r.mismatches.is_empty() && r.errors.is_empty());
    let mut lying = MockOs::returns_wrong_affinity();
    let r2 = apply_plan(&mut lying, &plan, Prio::High);
    assert_eq!(r2.mismatches.len(), 1); // silent no-op is surfaced, not trusted
}
