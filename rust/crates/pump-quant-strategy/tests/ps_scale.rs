//! Leaf ps_scale: HotPathPositionScaler scale-in/out decision (criterion 75).

use pump_quant_strategy::economic_gate::SizeBand;
use pump_quant_strategy::position_scaler::{
    scale_confirmation, scale_decision, Confirmation, ScaleAction,
};
use pump_quant_strategy::scalp_position::{FlowState, ScalpPositionState};

fn profit_pos() -> ScalpPositionState {
    let mut p = ScalpPositionState::open(1_000, 0);
    p.last_price_fp = 1_200; // in profit
    p
}

fn favorable_flow() -> FlowState {
    let mut f = FlowState::new();
    f.cvd_fp = 500; // net buys
    f.arrival_accel_fp = 10; // accelerating
    f.authenticity_fp = 9_000;
    f
}

#[test]
fn confirmation_requires_all_conditions() {
    let min_auth = 8_000;
    assert_eq!(
        scale_confirmation(&profit_pos(), &favorable_flow(), min_auth),
        Confirmation::Confirmed
    );

    // Not in profit.
    let mut p = profit_pos();
    p.last_price_fp = 900;
    assert_eq!(
        scale_confirmation(&p, &favorable_flow(), min_auth),
        Confirmation::Denied
    );

    // Low authenticity.
    let mut f = favorable_flow();
    f.authenticity_fp = 7_000;
    assert_eq!(
        scale_confirmation(&profit_pos(), &f, min_auth),
        Confirmation::Denied
    );

    // Negative CVD.
    let mut f = favorable_flow();
    f.cvd_fp = -1;
    assert_eq!(
        scale_confirmation(&profit_pos(), &f, min_auth),
        Confirmation::Denied
    );
}

#[test]
fn refuse_band_blocks() {
    let band = SizeBand::refuse();
    let a = scale_decision(&profit_pos(), &favorable_flow(), &band, 1_000, 8_000);
    assert_eq!(a, ScaleAction::Blocked);
}

#[test]
fn no_position_holds() {
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let a = scale_decision(&profit_pos(), &favorable_flow(), &band, 0, 8_000);
    assert_eq!(a, ScaleAction::Hold);
}

#[test]
fn confirmed_scales_in_by_x_min_rung() {
    // band x_min=1_000, x_max=10_000; current=2_000; headroom=8_000; rung=1_000.
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let a = scale_decision(&profit_pos(), &favorable_flow(), &band, 2_000, 8_000);
    assert_eq!(a, ScaleAction::ScaleIn { add: 1_000 });
}

#[test]
fn scale_in_tops_up_to_cap() {
    // current=9_500, headroom=500 < rung 1_000 => add=500.
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let a = scale_decision(&profit_pos(), &favorable_flow(), &band, 9_500, 8_000);
    assert_eq!(a, ScaleAction::ScaleIn { add: 500 });
}

#[test]
fn at_cap_holds() {
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let a = scale_decision(&profit_pos(), &favorable_flow(), &band, 10_000, 8_000);
    assert_eq!(a, ScaleAction::Hold);
}

#[test]
fn deterioration_scales_out_one_rung() {
    // Negative CVD => deterioration; remove min(current, x_min) = min(2_000,1_000)=1_000.
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let mut f = favorable_flow();
    f.cvd_fp = -100;
    let a = scale_decision(&profit_pos(), &f, &band, 2_000, 8_000);
    assert_eq!(a, ScaleAction::ScaleOut { remove: 1_000 });
}

#[test]
fn deterioration_caps_removal_at_current_size() {
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let mut f = favorable_flow();
    f.authenticity_fp = 100; // fabrication suspicion
    let a = scale_decision(&profit_pos(), &f, &band, 500, 8_000);
    assert_eq!(a, ScaleAction::ScaleOut { remove: 500 });
}

#[test]
fn deterministic_same_inputs_same_action() {
    let band = SizeBand::admit(1_000, 3_000, 10_000);
    let p = profit_pos();
    let f = favorable_flow();
    let a1 = scale_decision(&p, &f, &band, 2_000, 8_000);
    let a2 = scale_decision(&p, &f, &band, 2_000, 8_000);
    assert_eq!(a1, a2);
}
