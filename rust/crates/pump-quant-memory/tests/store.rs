//! Leaf: store. Verifies the memory-bounded QuantMemoryStore: per-table capacity
//! rejection (§57 durability-first), lookup, the sealed-experiment lifecycle via
//! the store, and the VOI queue convenience (§29.9, §56.1, §56.10).

use pump_quant_memory::rows::{
    AmplificationEdge, AssignmentId, CallMarkout, CategoryAssignment, EdgeId, EdgeKind, Experiment,
    ExperimentId, ExperimentResult, Hypothesis, HypothesisId, InferenceState, LifecycleTiming,
    MarkoutHorizon, MarkoutId, MetaCategory, MetaCategoryId, MetaLifecycle, MetaRotationSnapshot,
    ResultId, SnapshotId, SocialCall, SocialCallId, SourceClassification, SourceId,
    SourceQualityEntry,
};
use pump_quant_memory::store::{QuantMemoryStore, StoreError};

fn hyp(id: u64, impact: i128) -> Hypothesis {
    Hypothesis {
        id: HypothesisId(id),
        schema_version: 1,
        statement_hash: [0u8; 32],
        expected_impact_lamports: impact,
        prob_true_bps: 10_000,
        cost_to_test_lamports: 0,
        edge_half_life_secs: 86_400,
        status: InferenceState::Hypothesis,
    }
}

fn exp(id: u64) -> Experiment {
    Experiment {
        id: ExperimentId(id),
        hypothesis_id: HypothesisId(id),
        schema_version: 1,
        title_hash: [1u8; 32],
        causal_mechanism_hash: [2u8; 32],
        dataset_hash: [3u8; 32],
        config_hash: 1,
        created_at_ns: 1,
        sealed: false,
        seal_hash: None,
    }
}

#[test]
fn capacity_is_enforced_per_table() {
    let mut s = QuantMemoryStore::new(2);
    assert_eq!(s.insert_hypothesis(hyp(1, 1)), Ok(()));
    assert_eq!(s.insert_hypothesis(hyp(2, 1)), Ok(()));
    // Third insert into a full table is rejected, never silently evicting (§57).
    assert_eq!(
        s.insert_hypothesis(hyp(3, 1)),
        Err(StoreError::CapacityExceeded)
    );
    assert_eq!(s.hypotheses.len(), 2);
    // A different table still has its own budget.
    assert_eq!(s.insert_experiment(exp(1)), Ok(()));
}

#[test]
fn all_ten_tables_accept_inserts() {
    let mut s = QuantMemoryStore::new(4);
    assert_eq!(s.insert_hypothesis(hyp(1, 1)), Ok(()));
    assert_eq!(s.insert_experiment(exp(1)), Ok(()));
    assert_eq!(
        s.insert_result(ExperimentResult {
            id: ResultId(1),
            experiment_id: ExperimentId(1),
            net_sol_effect_lamports: 123,
            significance_bps: 9_900,
            outcome: InferenceState::ValidatedInference,
            reconciled_at_ns: 10,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_social_call(SocialCall {
            id: SocialCallId(1),
            source_id: SourceId(1),
            token_hash: [5u8; 32],
            captured_at_ns: 20,
            content_hash: [6u8; 32],
            timing: LifecycleTiming::PreFlow,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_call_markout(CallMarkout {
            id: MarkoutId(1),
            call_id: SocialCallId(1),
            horizon: MarkoutHorizon::M30,
            executable_return_bps: -650,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_source_quality(SourceQualityEntry {
            source_id: SourceId(1),
            classification: SourceClassification::InsufficientSample,
            confidence_bps: 1_000,
            sample_size: 3,
            mean_markout_30m_bps: -650,
            updated_at_ns: 30,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_amplification_edge(AmplificationEdge {
            id: EdgeId(1),
            from_source: SourceId(2),
            to_source: SourceId(1),
            token_hash: [5u8; 32],
            observed_at_ns: 40,
            kind: EdgeKind::Forward,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_meta_category(MetaCategory {
            id: MetaCategoryId(1),
            name_hash: [7u8; 32],
            lifecycle: MetaLifecycle::Emerging,
            updated_at_ns: 50,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_category_assignment(CategoryAssignment {
            id: AssignmentId(1),
            category_id: MetaCategoryId(1),
            token_hash: [5u8; 32],
            confidence_bps: 8_000,
            assigned_at_ns: 60,
        }),
        Ok(())
    );
    assert_eq!(
        s.insert_meta_rotation_snapshot(MetaRotationSnapshot {
            id: SnapshotId(1),
            category_id: MetaCategoryId(1),
            taken_at_ns: 70,
            lifecycle: MetaLifecycle::Accelerating,
            launch_share_bps: 1_500,
        }),
        Ok(())
    );
}

#[test]
fn seal_experiment_via_store_then_lookup() {
    let mut s = QuantMemoryStore::new(4);
    s.insert_experiment(exp(1)).unwrap();
    let hash = s.seal_experiment(ExperimentId(1)).expect("seal ok");
    let stored = s.experiment(ExperimentId(1)).unwrap();
    assert!(stored.sealed);
    assert_eq!(stored.seal_hash, Some(hash));
    assert!(stored.verify_integrity());
}

#[test]
fn sealing_unknown_experiment_is_not_found() {
    let mut s = QuantMemoryStore::new(4);
    assert_eq!(
        s.seal_experiment(ExperimentId(99)),
        Err(StoreError::NotFound)
    );
}

#[test]
fn resealing_via_store_reports_already_sealed() {
    let mut s = QuantMemoryStore::new(4);
    s.insert_experiment(exp(1)).unwrap();
    s.seal_experiment(ExperimentId(1)).unwrap();
    assert_eq!(
        s.seal_experiment(ExperimentId(1)),
        Err(StoreError::AlreadySealed)
    );
}

#[test]
fn dataset_update_refused_after_seal_via_store() {
    let mut s = QuantMemoryStore::new(4);
    s.insert_experiment(exp(1)).unwrap();
    // Allowed while unsealed.
    assert_eq!(
        s.update_experiment_dataset(ExperimentId(1), [8u8; 32]),
        Ok(())
    );
    s.seal_experiment(ExperimentId(1)).unwrap();
    // Refused after sealing (§56.1/§56.4 immutability).
    assert_eq!(
        s.update_experiment_dataset(ExperimentId(1), [0u8; 32]),
        Err(StoreError::SealedImmutable)
    );
    assert!(s.experiment(ExperimentId(1)).unwrap().verify_integrity());
}

#[test]
fn voi_queue_ranks_only_open_hypotheses() {
    let mut s = QuantMemoryStore::new(8);
    s.insert_hypothesis(hyp(1, 1_000_000_000)).unwrap();
    s.insert_hypothesis(hyp(2, 3_000_000_000)).unwrap();
    let mut closed = hyp(3, 9_000_000_000);
    closed.status = InferenceState::RejectedInference;
    s.insert_hypothesis(closed).unwrap();

    let q = s.voi_queue();
    let order: Vec<u64> = q.iter().map(|r| r.id.0).collect();
    // id 3 is rejected (closed) and excluded; 2 outranks 1 by impact.
    assert_eq!(order, vec![2, 1]);
}
