//! Tests for provenance vocabulary: the authority ordering on EvidenceStage,
//! DeliveryMode/DatasetFidelity/SourceLifecycleStatus predicates and round-trips.

use pump_quant_domain::evidence::{
    DatasetFidelity, DeliveryMode, EvidenceStage, SourceLifecycleStatus,
};

#[test]
fn evidence_stage_authority_is_totally_ordered_ascending() {
    // The ladder must be strictly increasing in authority, weakest first.
    let ladder = [
        EvidenceStage::EarliestSignal,
        EvidenceStage::StructuredObservation,
        EvidenceStage::CanonicalRepair,
        EvidenceStage::ReconciledExecution,
    ];
    assert_eq!(EvidenceStage::ALL, ladder);
    for (i, &si) in ladder.iter().enumerate() {
        assert_eq!(si.authority_rank(), i as u8);
        for (j, &sj) in ladder.iter().enumerate() {
            // Ordering agrees with rank for every pair.
            assert_eq!(si < sj, (i as u8) < (j as u8), "order {i} vs {j}");
            assert_eq!(si.at_least_as_authoritative_as(sj), i >= j);
        }
    }
}

#[test]
fn evidence_stage_earliest_is_weakest_reconciled_is_truth() {
    // Timing-vs-authority inversion: earliest arrives first yet ranks lowest.
    assert_eq!(EvidenceStage::EarliestSignal.authority_rank(), 0);
    assert!(EvidenceStage::EarliestSignal < EvidenceStage::ReconciledExecution);
    // Only reconciled execution is canonical truth.
    for s in EvidenceStage::ALL {
        assert_eq!(
            s.is_canonical_truth(),
            s == EvidenceStage::ReconciledExecution
        );
    }
    // Round-trip.
    for s in EvidenceStage::ALL {
        assert_eq!(EvidenceStage::from_u8(s.authority_rank()), Some(s));
    }
    assert_eq!(EvidenceStage::from_u8(4), None);
}

#[test]
fn delivery_mode_only_live_is_live() {
    for m in [
        DeliveryMode::Live,
        DeliveryMode::ProviderReplay,
        DeliveryMode::RpcRepair,
        DeliveryMode::CanonicalBackfill,
    ] {
        assert_eq!(m.is_live(), m == DeliveryMode::Live);
        assert_eq!(DeliveryMode::from_u8(m as u8), Some(m));
    }
    assert_eq!(DeliveryMode::from_u8(4), None);
    // Explicit discriminants.
    assert_eq!(DeliveryMode::Live as u8, 0);
    assert_eq!(DeliveryMode::CanonicalBackfill as u8, 3);
}

#[test]
fn dataset_fidelity_rank_and_ordering() {
    let ladder = [
        DatasetFidelity::CanonicalBackfill,
        DatasetFidelity::DualFeedRecorded,
        DatasetFidelity::LiveShadowRecorded,
        DatasetFidelity::ReconciledLiveExecution,
    ];
    for (i, &f) in ladder.iter().enumerate() {
        assert_eq!(f.rank(), i as u8);
        assert_eq!(
            f.is_reconciled_live(),
            f == DatasetFidelity::ReconciledLiveExecution
        );
    }
    // Strictly increasing trust.
    assert!(DatasetFidelity::CanonicalBackfill < DatasetFidelity::ReconciledLiveExecution);
    assert!(DatasetFidelity::DualFeedRecorded < DatasetFidelity::LiveShadowRecorded);
}

#[test]
fn source_lifecycle_active_and_terminal_predicates() {
    use SourceLifecycleStatus::*;
    let all = [
        ActivePrimary,
        ActiveRedundant,
        Transitional,
        Degraded,
        SunsetPending,
        Disabled,
        Retired,
    ];
    // Round-trip and discriminants.
    for (i, s) in all.iter().enumerate() {
        assert_eq!(*s as u8, i as u8);
        assert_eq!(SourceLifecycleStatus::from_u8(i as u8), Some(*s));
    }
    assert_eq!(SourceLifecycleStatus::from_u8(7), None);

    // Only the two active states may back new positions.
    for s in all {
        let expect_active = matches!(s, ActivePrimary | ActiveRedundant);
        assert_eq!(s.is_active_for_new_positions(), expect_active, "{s}");
        let expect_terminal = matches!(s, Disabled | Retired);
        assert_eq!(s.is_terminal(), expect_terminal, "{s}");
        // Active and terminal are mutually exclusive.
        assert!(!(s.is_active_for_new_positions() && s.is_terminal()));
    }
}
