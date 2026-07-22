// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_zone' component (leaf 'stratify_by_zone_aggregate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::entry_zone::*;

#[test]
fn gd_stratify_by_zone_aggregate() {
    let z = EntryZone::Band9kTo20kTarget;
    let rows = vec![
        // Out-of-ordinal-order input; two Target zone rows and one Sub5k row.
        ZoneOutcomeRow::test(z, 1_000, 100, 10, false, 5_000, -1_000),
        ZoneOutcomeRow::test(z, -400, 50, 5, true, 2_000, -3_000),
        ZoneOutcomeRow::test(
            EntryZone::Sub5kPreAttention,
            9_999,
            1,
            1,
            false,
            8_000,
            -500,
        ),
    ];
    let out = stratify_by_zone(&rows);

    // One strata per present zone, ascending ordinal order.
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].zone, EntryZone::Sub5kPreAttention); // ordinal 0
    assert_eq!(out[1].zone, z); // ordinal 2

    // Sub5k singleton.
    let sub = out[0];
    assert_eq!(sub.n, 1);
    assert_eq!(sub.net_lamports, 9_999);
    assert_eq!(sub.rug_count, 0);
    assert_eq!(sub.rug_rate_bps, 0);
    assert_eq!(sub.median_mfe_bps, 8_000);
    assert_eq!(sub.median_mae_bps, -500);

    // Target aggregate: checked sums, rug rate in bps, integer-avg medians.
    let t = out[1];
    assert_eq!(t.n, 2);
    assert_eq!(t.net_lamports, 600);
    assert_eq!(t.fees, 150);
    assert_eq!(t.impact_lamports, 15);
    assert_eq!(t.rug_count, 1);
    assert_eq!(t.rug_rate_bps, 5_000); // 1 * 10_000 / 2
    assert_eq!(t.median_mfe_bps, 3_500); // (2_000 + 5_000) / 2
    assert_eq!(t.median_mae_bps, -2_000); // (-3_000 + -1_000) / 2
}

#[test]
fn gd_stratify_by_zone_empty_and_all_rugged() {
    // Empty input -> empty output (no fabricated zero rows).
    assert!(stratify_by_zone(&[]).is_empty());

    // Every outcome rugged -> full 10_000 bps rug rate.
    let z = EntryZone::MigrationEdge;
    let rows = vec![
        ZoneOutcomeRow::test(z, -1_000, 10, 0, true, 100, -9_000),
        ZoneOutcomeRow::test(z, -2_000, 10, 0, true, 50, -9_500),
    ];
    let out = stratify_by_zone(&rows);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].n, 2);
    assert_eq!(out[0].rug_count, 2);
    assert_eq!(out[0].rug_rate_bps, 10_000);
    assert_eq!(out[0].net_lamports, -3_000);
    assert_eq!(out[0].median_mae_bps, -9_250); // (-9_500 + -9_000) / 2
}
