#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::scalp_position::*;

#[test]
fn prop_covariates_not_in_cell_key() {
    let a = hazard_inputs(&ScalpPositionState::test(), &FlowState::test(), 9_000, 0);
    let b = hazard_inputs(
        &ScalpPositionState::test(),
        &FlowState::test(),
        1_000,
        8_000,
    );
    assert_eq!(a.cell_key(), b.cell_key()); // covariates differ, cell identical
    assert_ne!(a.conviction_fp, b.conviction_fp);
}
