// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'cpu_numa_tuning' component (leaf 'cn_pin_plan').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_plan_disjoint_and_smt_isolated() {
    let recs = vec![ProcRecord::core(0, 0b0011), ProcRecord::core(0, 0b1100),
                    ProcRecord::core(0, 0b110000), ProcRecord::core(0, 0b11000000)];
    let t = parse_topology(&recs).unwrap();
    let hot = [HotThreadSpec::test("reducer"), HotThreadSpec::test("submit")];
    let p = derive_plan(&t, &hot).unwrap();
    assert_eq!(p.assignments.len(), 2);
    let hot_mask: u64 = p.assignments.iter().map(|a| a.1.mask).fold(0, |m, x| m | x);
    assert_eq!(hot_mask & p.reserved_idle.mask, 0);
    assert_eq!(hot_mask & p.control_mask.mask, 0);
    assert_eq!(p.reserved_idle.mask & p.control_mask.mask, 0);
    assert_eq!(hot_mask.count_ones(), 2); // one logical CPU each
    let too_many = [HotThreadSpec::test("a"), HotThreadSpec::test("b"),
                    HotThreadSpec::test("c"), HotThreadSpec::test("d"),
                    HotThreadSpec::test("e")];
    assert!(matches!(derive_plan(&t, &too_many), Err(PlanError::Insufficient)));
}
