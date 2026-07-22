use pump_quant_evaluator::baseline_destruction::*;

#[test]
fn destroys_field_when_all_beaten_by_corrected_margin() {
    // Two competitors -> K=2 -> effective margin = 100*2 = 200.
    // challenger 1000: vs champion 700 (margin 300>=200), vs baseline 750
    // (margin 250>=200). Binding rival is baseline (smaller advantage 250).
    let comps = [Competitor::champion(700), Competitor::baseline(750)];
    let v = baseline_destruction(1_000, &comps, 100);
    assert_eq!(
        v,
        DestructionVerdict::Defeats {
            effective_margin: 200
        }
    );
    assert!(v.defeats());
}

#[test]
fn fails_and_reports_binding_rival() {
    // K=2 -> effective 200. challenger 1000 vs champion 850 -> margin 150 < 200.
    // baseline 700 -> margin 300 ok. Binding is champion (smallest advantage).
    let comps = [Competitor::champion(850), Competitor::baseline(700)];
    let v = baseline_destruction(1_000, &comps, 100);
    assert_eq!(
        v,
        DestructionVerdict::Fails {
            effective_margin: 200,
            blocking_kind: CompetitorKind::Champion,
            blocking_value: 850,
            blocking_margin: 150,
        }
    );
}

#[test]
fn multiple_testing_correction_raises_the_bar() {
    // Same absolute margins, but more baselines -> higher K -> can flip a pass
    // into a fail. challenger 1000, required 100.
    // With 1 competitor (champion 850): effective 100, margin 150 -> Defeats.
    let one = [Competitor::champion(850)];
    assert!(baseline_destruction(1_000, &one, 100).defeats());
    // Add three trivial baselines all at 900 (margin 100 each). K=4 ->
    // effective 400; every margin (150 and 100) < 400 -> Fails. The correction,
    // not the raw comparison, is what changed.
    let many = [
        Competitor::champion(850),
        Competitor::baseline(900),
        Competitor::baseline(900),
        Competitor::baseline(900),
    ];
    let v = baseline_destruction(1_000, &many, 100);
    assert!(!v.defeats());
    if let DestructionVerdict::Fails {
        effective_margin, ..
    } = v
    {
        assert_eq!(effective_margin, 400);
    } else {
        panic!("expected Fails");
    }
}

#[test]
fn empty_field_is_no_field() {
    assert_eq!(
        baseline_destruction(1_000, &[], 100),
        DestructionVerdict::NoField
    );
}

#[test]
fn zero_margin_requires_strict_domination_of_all() {
    // required 0 -> effective 0 -> must beat-or-tie every rival.
    let comps = [Competitor::champion(1_000), Competitor::baseline(999)];
    // challenger 1000 ties champion (margin 0>=0) and beats baseline -> Defeats.
    assert!(baseline_destruction(1_000, &comps, 0).defeats());
    // challenger 999 loses to champion by 1 -> Fails.
    assert!(!baseline_destruction(999, &comps, 0).defeats());
}
