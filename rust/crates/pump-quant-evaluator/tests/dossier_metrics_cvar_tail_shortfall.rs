// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'metrics' component (leaf 'cvar_tail_shortfall').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::metrics::*;

#[test]
fn gd_cvar_tail_shortfall() {
    // 10 returns, worst 20% (alpha=2000) -> ceil(10*2000/10000)=2 worst.
    let returns = vec![-900i64, -800, -100, 0, 100, 200, 300, 400, 500, 1_000];
    let r = cvar(&returns, 2_000).unwrap();
    assert_eq!(r.tail_n, 2);
    assert_eq!(r.alpha_bps, 2_000);
    assert_eq!(r.var_bps, -800); // boundary = least-bad in tail = max of tail
    assert_eq!(r.cvar_bps, -850); // mean(-900, -800)
                                  // cvar <= var (shortfall no better than boundary).
    assert!(r.cvar_bps <= r.var_bps);

    // Order-independence: shuffled input yields identical report.
    let shuffled = vec![1_000i64, -100, 500, -900, 300, 0, 400, -800, 200, 100];
    assert_eq!(cvar(&shuffled, 2_000).unwrap(), r);

    // Min one in tail: tiny alpha still selects the single worst.
    let small = vec![-500i64, 100, 200];
    let s = cvar(&small, 100).unwrap();
    assert_eq!(s.tail_n, 1);
    assert_eq!(s.var_bps, -500);
    assert_eq!(s.cvar_bps, -500);

    // Rejection/edge: empty and zero-alpha yield None.
    assert!(cvar(&[], 500).is_none());
    assert!(cvar(&[100, 200], 0).is_none());
}
