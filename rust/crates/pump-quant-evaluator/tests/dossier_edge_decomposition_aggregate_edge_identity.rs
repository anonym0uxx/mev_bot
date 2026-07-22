// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'edge_decomposition' component (leaf 'aggregate_edge_identity').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::edge_decomposition::*;

#[test]
fn prop_aggregate_edge_identity() {
    fn trade(sel: i128, realized: i128) -> PerTradeEdge {
        let comps = [
            ComponentValue::measured(sel),
            ComponentValue::estimated(50, 8),
            ComponentValue::estimated(-30, 5),
            ComponentValue::measured(-20),
            ComponentValue::measured(-40),
            ComponentValue::measured(-5),
            ComponentValue::estimated(-10, 3),
            ComponentValue::estimated(15, 4),
            ComponentValue::measured(-25),
        ];
        PerTradeEdge::new(realized, comps)
    }
    // attributed(sel) = sel + 50 - 30 - 20 - 40 - 5 - 10 + 15 - 25 = sel - 65.
    let trades = vec![trade(100, 50), trade(200, -10), trade(0, 0)];
    let a = aggregate_edge(&trades);
    assert_eq!(a.n, 3);
    let comp_sum: i128 = a.components.iter().map(|c| c.sum_lamports).sum();
    assert_eq!(comp_sum + a.residual_lamports, a.realized_net_lamports);
    assert_eq!(a.realized_net_lamports, 40); // 50 - 10 + 0
    assert_eq!(a.components[0].sum_lamports, 300); // 100 + 200 + 0
                                                   // Residuals: 50-35=15, -10-135=-145, 0-(-65)=65 -> net -65, abs 225.
    assert_eq!(a.residual_lamports, -65);
    assert_eq!(a.residual_abs_sum_lamports, 225);
    assert_eq!(a.components[0].worst_quality, Attribution::Measured);
    assert_eq!(a.components[0].quality_counts, [3, 0, 0, 0]);
    assert_eq!(a.components[1].worst_quality, Attribution::Estimated);
    assert_eq!(a.components[1].uncertainty_lamports, 24);
    // Edge case: empty book fully zeroed, worst quality Unknown.
    let e = aggregate_edge(&[]);
    assert_eq!(e.n, 0);
    assert_eq!(e.residual_lamports, 0);
    assert_eq!(e.realized_net_lamports, 0);
    assert_eq!(e.components[0].worst_quality, Attribution::Unknown);
    assert_eq!(e.components[0].quality_counts, [0, 0, 0, 0]);
}
