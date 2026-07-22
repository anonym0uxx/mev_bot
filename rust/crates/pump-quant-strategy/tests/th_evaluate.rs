//! Leaf th_evaluate: deterministic thesis-invalidation predicate (criterion 44).

use pump_quant_strategy::thesis::{
    build_thesis, evaluate_thesis, forced_action, Direction, FeatureObservation, ForcedAction,
    ThesisCondition, ThesisInputs, ThesisState, ThesisVerdict,
};

fn thesis() -> pump_quant_strategy::thesis::Thesis {
    build_thesis(&ThesisInputs {
        entry_mode: 1,
        archetype: 1,
        entry_ts_ns: 0,
        required: vec![ThesisCondition {
            // required: CVD >= 100, complete >= 8000, fresh within 500ns.
            feature_id: 10,
            direction: Direction::AtLeast,
            threshold_fp: 100,
            min_completeness_bps: 8_000,
            freshness_bound_ns: 500,
        }],
        invalidation: vec![ThesisCondition {
            // invalidation: sell_velocity >= 9000 triggers exit.
            feature_id: 20,
            direction: Direction::AtLeast,
            threshold_fp: 9_000,
            min_completeness_bps: 6_000,
            freshness_bound_ns: 2_000,
        }],
        evidence_refs: vec![],
    })
}

fn obs(
    feature_id: u32,
    value_fp: i64,
    completeness_bps: u32,
    observed_ts_ns: u64,
) -> FeatureObservation {
    FeatureObservation {
        feature_id,
        value_fp,
        completeness_bps,
        observed_ts_ns,
    }
}

#[test]
fn holds_when_required_met_and_no_invalidation() {
    let t = thesis();
    let obs_list = [obs(10, 150, 9_000, 400)];
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 600), ThesisVerdict::Holds);
    assert_eq!(forced_action(ThesisVerdict::Holds), ForcedAction::Hold);
}

#[test]
fn required_below_threshold_invalidates() {
    let t = thesis();
    let obs_list = [obs(10, 99, 9_000, 400)]; // 99 < 100
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 600), ThesisVerdict::Invalidated);
}

#[test]
fn required_stale_invalidates() {
    let t = thesis();
    // value ok but age = 700 - 100 = 600 > 500 bound.
    let obs_list = [obs(10, 150, 9_000, 100)];
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 700), ThesisVerdict::Invalidated);
}

#[test]
fn required_incomplete_invalidates() {
    let t = thesis();
    let obs_list = [obs(10, 150, 7_000, 400)]; // 7000 < 8000
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 600), ThesisVerdict::Invalidated);
}

#[test]
fn missing_required_feature_invalidates() {
    let t = thesis();
    let obs_list: [FeatureObservation; 0] = [];
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 600), ThesisVerdict::Invalidated);
}

#[test]
fn triggered_invalidation_condition_forces_exit() {
    let t = thesis();
    // required satisfied, but invalidation feature 20 hits 9500 >= 9000.
    let obs_list = [obs(10, 150, 9_000, 400), obs(20, 9_500, 8_000, 400)];
    let st = ThesisState {
        observations: &obs_list,
    };
    let v = evaluate_thesis(&t, &st, 600);
    assert_eq!(v, ThesisVerdict::Invalidated);
    assert_eq!(forced_action(v), ForcedAction::ForceExit);
}

#[test]
fn untriggered_invalidation_condition_holds() {
    let t = thesis();
    // invalidation feature present but below trigger (8000 < 9000) => holds.
    let obs_list = [obs(10, 150, 9_000, 400), obs(20, 8_000, 8_000, 400)];
    let st = ThesisState {
        observations: &obs_list,
    };
    assert_eq!(evaluate_thesis(&t, &st, 600), ThesisVerdict::Holds);
}
