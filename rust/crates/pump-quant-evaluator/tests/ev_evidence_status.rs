use pump_quant_evaluator::evidence_status::*;

#[test]
fn paper_and_shadow_can_never_claim_proven_live_edge() {
    for s in [EvidenceStatus::Paper, EvidenceStatus::Shadow] {
        assert!(!s.claims_proven_live_edge());
        assert!(!s.is_live_backed());
        assert_eq!(
            tag_proven_live_edge(s),
            Err(EvidenceError::CounterfactualCohort { status: s })
        );
    }
}

#[test]
fn live_probe_is_backed_but_not_yet_proven() {
    let s = EvidenceStatus::LiveProbe;
    assert!(s.is_live_backed());
    assert!(!s.claims_proven_live_edge());
    assert_eq!(
        tag_proven_live_edge(s),
        Err(EvidenceError::NotYetReconciled { status: s })
    );
}

#[test]
fn reconciled_live_promotes_to_proven() {
    assert_eq!(
        tag_proven_live_edge(EvidenceStatus::ReconciledLive),
        Ok(EvidenceStatus::ProvenLiveEdge)
    );
}

#[test]
fn proven_is_idempotent() {
    assert_eq!(
        tag_proven_live_edge(EvidenceStatus::ProvenLiveEdge),
        Ok(EvidenceStatus::ProvenLiveEdge)
    );
    assert!(EvidenceStatus::ProvenLiveEdge.claims_proven_live_edge());
}

#[test]
fn ladder_ordering_is_monotone() {
    // The derived ordering is the promotion ladder, weakest to strongest.
    assert!(EvidenceStatus::Paper < EvidenceStatus::Shadow);
    assert!(EvidenceStatus::Shadow < EvidenceStatus::LiveProbe);
    assert!(EvidenceStatus::LiveProbe < EvidenceStatus::ReconciledLive);
    assert!(EvidenceStatus::ReconciledLive < EvidenceStatus::ProvenLiveEdge);
}
