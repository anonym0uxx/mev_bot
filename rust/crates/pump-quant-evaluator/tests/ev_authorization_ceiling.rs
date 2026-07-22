use pump_quant_evaluator::authorization_ceiling::*;

#[test]
fn backtest_only_never_exceeds_minimum_probe() {
    assert_eq!(
        max_authorized_action(EvidenceStage::BacktestOnly),
        ActionCeiling::MinimumProbe
    );
    assert!(!authorizes_scaled_capital(EvidenceStage::BacktestOnly));
}

#[test]
fn walk_forward_still_capped_at_minimum_probe() {
    assert_eq!(
        max_authorized_action(EvidenceStage::WalkForwardValidated),
        ActionCeiling::MinimumProbe
    );
    assert!(!authorizes_scaled_capital(
        EvidenceStage::WalkForwardValidated
    ));
}

#[test]
fn only_reconciled_live_edge_authorizes_scaled_capital() {
    assert_eq!(
        max_authorized_action(EvidenceStage::ReconciledLiveEdge),
        ActionCeiling::ScaledCapital
    );
    assert!(authorizes_scaled_capital(EvidenceStage::ReconciledLiveEdge));
    // No weaker stage authorizes scaled capital.
    for s in [
        EvidenceStage::BacktestOnly,
        EvidenceStage::WalkForwardValidated,
        EvidenceStage::ShadowValidated,
        EvidenceStage::LiveProbeValidated,
    ] {
        assert!(!authorizes_scaled_capital(s));
    }
}

#[test]
fn live_probe_unlocks_scaled_probe_not_capital() {
    assert_eq!(
        max_authorized_action(EvidenceStage::LiveProbeValidated),
        ActionCeiling::ScaledProbe
    );
}

#[test]
fn ceiling_is_monotone_in_evidence_strength() {
    let stages = [
        EvidenceStage::BacktestOnly,
        EvidenceStage::WalkForwardValidated,
        EvidenceStage::ShadowValidated,
        EvidenceStage::LiveProbeValidated,
        EvidenceStage::ReconciledLiveEdge,
    ];
    // Ceilings never decrease as evidence strengthens.
    for w in stages.windows(2) {
        assert!(max_authorized_action(w[0]) <= max_authorized_action(w[1]));
    }
}
