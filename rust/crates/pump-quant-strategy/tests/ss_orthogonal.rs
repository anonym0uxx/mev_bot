//! Leaf ss_orthogonal: multi-dimensional orthogonal strategy state (criterion 47).

use pump_quant_strategy::strategy_state::{Dimension, DimensionKind, StrategyState};

fn dim(raw: i64) -> Dimension {
    Dimension::new(raw, raw * 2, 9_000, 100, 8_000, 1)
}

fn state() -> StrategyState {
    StrategyState::new(dim(10), dim(20), dim(30), dim(40))
}

#[test]
fn dimensions_are_independently_inspectable() {
    let s = state();
    assert_eq!(s.entry().raw_fp, 10);
    assert_eq!(s.size().raw_fp, 20);
    assert_eq!(s.exit().raw_fp, 30);
    assert_eq!(s.hold().raw_fp, 40);
    // Uniform accessor agrees with named accessors.
    assert_eq!(s.get(DimensionKind::Entry), s.entry());
    assert_eq!(s.get(DimensionKind::Size), s.size());
    assert_eq!(s.get(DimensionKind::Exit), s.exit());
    assert_eq!(s.get(DimensionKind::Hold), s.hold());
}

#[test]
fn mutating_one_dimension_leaves_others_unchanged() {
    let s = state();
    let s2 = s.with(DimensionKind::Exit, dim(99));
    // Exit changed.
    assert_eq!(s2.exit().raw_fp, 99);
    // The other three are byte-identical — no collapsed score coupling them.
    assert_eq!(s2.entry(), s.entry());
    assert_eq!(s2.size(), s.size());
    assert_eq!(s2.hold(), s.hold());
}

#[test]
fn each_dimension_can_be_set_independently() {
    let base = state();
    for kind in [
        DimensionKind::Entry,
        DimensionKind::Size,
        DimensionKind::Exit,
        DimensionKind::Hold,
    ] {
        let changed = base.with(kind, dim(777));
        assert_eq!(changed.get(kind).raw_fp, 777);
        // Exactly one dimension differs from base.
        let differing = changed
            .dimensions()
            .iter()
            .zip(base.dimensions().iter())
            .filter(|(a, b)| a.1 != b.1)
            .count();
        assert_eq!(
            differing, 1,
            "changing {kind:?} altered more than one dimension"
        );
    }
}

#[test]
fn provenance_metadata_preserved() {
    let d = Dimension::new(5, 50, 7_000, 250, 6_000, 3);
    let s = StrategyState::new(d, dim(1), dim(2), dim(3));
    let e = s.entry();
    assert_eq!(e.raw_fp, 5);
    assert_eq!(e.derived_fp, 50);
    assert_eq!(e.completeness_bps, 7_000);
    assert_eq!(e.freshness_ns, 250);
    assert_eq!(e.confidence_bps, 6_000);
    assert_eq!(e.source, 3);
}
