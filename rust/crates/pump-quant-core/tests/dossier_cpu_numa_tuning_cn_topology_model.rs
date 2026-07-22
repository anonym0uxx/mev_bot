// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'cpu_numa_tuning' component (leaf 'cn_topology_model').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_core::cpu_numa_tuning::*;

#[test]
fn prop_topology_parse_and_validate() {
    // 2 physical cores, SMT2: masks 0b0011 and 0b1100 in group 0
    let recs = vec![ProcRecord::core(0, 0b0011), ProcRecord::core(0, 0b1100)];
    let t = parse_topology(&recs).unwrap();
    assert_eq!(t.physical_cores, 2);
    assert_eq!(t.logical_cpus, 4);
    let overlap = vec![ProcRecord::core(0, 0b0011), ProcRecord::core(0, 0b0110)];
    assert!(parse_topology(&overlap).is_err());
}
