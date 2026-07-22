use pump_quant_evaluator::regression_gate::*;

#[test]
fn all_pass_is_pass() {
    let results = [
        RegressionResult::new(1, true),
        RegressionResult::new(2, true),
        RegressionResult::new(3, true),
    ];
    assert_eq!(regression_gate(&results), GateOutcome::Pass);
    assert!(regression_gate(&results).passed());
}

#[test]
fn any_failure_blocks_and_lists_failing_ids_in_order() {
    let results = [
        RegressionResult::new(1, true),
        RegressionResult::new(2, false),
        RegressionResult::new(3, true),
        RegressionResult::new(4, false),
    ];
    assert_eq!(
        regression_gate(&results),
        GateOutcome::Blocked {
            failing: vec![RegressionId(2), RegressionId(4)],
        }
    );
    assert!(!regression_gate(&results).passed());
}

#[test]
fn single_failure_blocks() {
    let results = [RegressionResult::new(9, false)];
    assert_eq!(
        regression_gate(&results),
        GateOutcome::Blocked {
            failing: vec![RegressionId(9)],
        }
    );
}

#[test]
fn empty_battery_passes_vacuously() {
    assert_eq!(regression_gate(&[]), GateOutcome::Pass);
}
