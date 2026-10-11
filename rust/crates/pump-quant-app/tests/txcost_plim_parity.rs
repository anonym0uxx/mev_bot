//! Integration: the PRICE LIMIT R9 builder-cost accessor (pump-quant-protocol, which cannot depend on
//! pump-quant-app) and the single versioned TxCostConfig (pump-quant-app::txcost) must name the SAME
//! planned per-signature fee, compute plan and tip. If either side changes, this fails; the fix is
//! to change both in one versioned step, never to let them drift.
use pump_quant_app::txcost::TXCOST_V1_20261010 as V1;
use pump_quant_protocol::tx_build::planned_lamports_per_signature;

#[test]
fn plim_r9_signature_fee_equals_the_shared_txcost_v1() {
    assert_eq!(planned_lamports_per_signature(), V1.lamports_per_signature);
}

#[test]
fn shared_v1_planned_leg_is_the_10600_builder_structure() {
    // base 5,000 x 1 signature + ceil(120,000 CU x 5,000 uL / 1e6) = 600 priority + 5,000 tip.
    assert_eq!(V1.signatures, 1);
    assert_eq!(V1.cu_limit, 120_000);
    assert_eq!(V1.cu_price_micro_lamports, 5_000);
    assert_eq!(V1.tip_lamports, 5_000);
    assert_eq!(V1.planned_leg_lamports(), 10_600);
    assert_eq!(
        V1.stress_allowance_lamports, 0,
        "a stress allowance is never a planned fee"
    );
}
