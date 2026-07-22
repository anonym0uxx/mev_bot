// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'edge_decomposition' component (leaf 'decompose_trade_exact_residual').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::edge_decomposition::*;

#[test]
fn prop_decompose_trade_exact_residual() {
    for realized in [-1000i128, -1, 0, 37, 100_000] {
        for base in [-50i128, 0, 7, 250] {
            let comps = [
                ComponentValue::measured(base),
                ComponentValue::estimated(base + 1, 8),
                ComponentValue::estimated(-base, 5),
                ComponentValue::measured(-20),
                ComponentValue::measured(-40),
                ComponentValue::measured(-5),
                ComponentValue::estimated(-10, 3),
                ComponentValue::estimated(15, 4),
                ComponentValue::unknown(),
            ];
            let attributed_expected: i128 =
                base + (base + 1) + (-base) + (-20) + (-40) + (-5) + (-10) + 15 + 0;
            let t = PerTradeEdge::new(realized, comps);
            let d = decompose_trade(&t);
            assert_eq!(d.attributed_lamports, attributed_expected);
            assert_eq!(d.residual_lamports, realized - attributed_expected);
            assert_eq!(d.attributed_lamports + d.residual_lamports, realized);
            assert_eq!(d.total_uncertainty_lamports, 8 + 5 + 3 + 4);
        }
    }
    // Edge case: all-zero measured components -> zero everything.
    let z = PerTradeEdge::new(0, [ComponentValue::measured(0); 9]);
    let dz = decompose_trade(&z);
    assert_eq!(dz.attributed_lamports, 0);
    assert_eq!(dz.residual_lamports, 0);
    assert_eq!(dz.total_uncertainty_lamports, 0);
}
