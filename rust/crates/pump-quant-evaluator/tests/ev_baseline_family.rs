//! §52 baseline family — integration coverage + destruction wiring.
use pump_quant_evaluator::baseline_destruction::{baseline_destruction, DestructionVerdict};
use pump_quant_evaluator::baseline_family::{
    as_competitors, run_baseline, run_family, BaselineKind, FamilyParams, FeeModel, TapeEvent,
};

fn tape() -> Vec<TapeEvent> {
    vec![
        TapeEvent::test(0, true, true, 10, 5_000, 3_000),
        TapeEvent::test(1, true, false, -5, -2_000, -1_000),
        TapeEvent::test(2, true, true, 20, 9_000, 4_000),
    ]
}

#[test]
fn family_has_five_baselines_in_order() {
    let fam = run_family(&tape(), &FeeModel::new(0), &FamilyParams::default_params());
    assert_eq!(fam.len(), 5);
    assert_eq!(fam[0].kind, BaselineKind::RandomEligibleEntry);
    assert_eq!(fam[4].kind, BaselineKind::HoldToDeath);
}

#[test]
fn family_feeds_destruction_verdict() {
    let fam = run_family(&tape(), &FeeModel::new(0), &FamilyParams::default_params());
    let comps = as_competitors(&fam);
    // A huge challenger destroys the whole naive field.
    let v = baseline_destruction(1_000_000, &comps, 0);
    assert!(matches!(v, DestructionVerdict::Defeats { .. }));
    // A tiny challenger fails.
    let v2 = baseline_destruction(-1, &comps, 0);
    assert!(!v2.defeats());
}

#[test]
fn random_eligible_selection_is_rng_free_repeatable() {
    let p = FamilyParams {
        sample_k: 3,
        sample_phase: 1,
        score_threshold: 0,
    };
    let a = run_baseline(
        BaselineKind::RandomEligibleEntry,
        &tape(),
        &FeeModel::new(0),
        &p,
    );
    let b = run_baseline(
        BaselineKind::RandomEligibleEntry,
        &tape(),
        &FeeModel::new(0),
        &p,
    );
    assert_eq!(a, b);
}

#[test]
fn fees_reduce_net_by_entries_times_fee() {
    let fam = run_family(
        &tape(),
        &FeeModel::new(100),
        &FamilyParams::default_params(),
    );
    let hold = fam
        .iter()
        .find(|r| r.kind == BaselineKind::HoldToDeath)
        .unwrap();
    assert_eq!(hold.fee_lamports, 100 * hold.entries as i128);
    assert_eq!(hold.net_lamports, hold.gross_lamports - hold.fee_lamports);
}
