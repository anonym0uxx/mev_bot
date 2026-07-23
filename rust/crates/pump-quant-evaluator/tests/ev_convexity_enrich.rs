//! §49 convexity enrichment builder — integration coverage feeding the ledger.
use pump_quant_evaluator::convexity_enrich::{
    build_events, event_from_mark, ConvexityMark, SizeFraction,
};
use pump_quant_evaluator::convexity_ledger::{build_ledger, RuleId, RuleKind};

fn rule(id: u64) -> RuleId {
    RuleId::new(RuleKind::PartialDeRisk, id)
}

#[test]
fn veto_and_haircut_build_non_degenerate_events() {
    let marks = vec![
        ConvexityMark::Veto {
            rule: rule(1),
            counterfactual_bps: -4_000,
            mfe_bps: 50,
        },
        ConvexityMark::Haircut {
            rule: rule(1),
            full_counterfactual_bps: 10_000,
            applied: SizeFraction::new(1, 4),
            mfe_bps: 12_000,
        },
    ];
    let events = build_events(&marks);
    assert_eq!(events.len(), 2);
    // Veto: counterfactual-vs-zero.
    assert_eq!(events[0].realized_bps, 0);
    assert_eq!(events[0].counterfactual_bps, -4_000);
    // Haircut: reduced-vs-full (quarter size of +10000 = +2500).
    assert_eq!(events[1].realized_bps, 2_500);
    assert_eq!(events[1].counterfactual_bps, 10_000);

    let led = build_ledger(&events, 5_000);
    assert_eq!(led.len(), 1);
    assert_eq!(led[0].losses_avoided_bps, 4_000);
}

#[test]
fn allowed_mark_is_not_suppressed() {
    let e = event_from_mark(&ConvexityMark::Allowed {
        rule: rule(2),
        realized_bps: 3_000,
        mfe_bps: 3_500,
    });
    assert!(!e.suppressed);
    assert_eq!(e.counterfactual_bps, 3_000);
}

#[test]
#[should_panic(expected = "denominator must be non-zero")]
fn zero_denominator_rejected() {
    let _ = SizeFraction::new(1, 0);
}
