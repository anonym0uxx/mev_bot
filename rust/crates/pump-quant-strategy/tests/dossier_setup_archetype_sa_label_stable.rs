// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'setup_archetype' component (leaf 'sa_label_stable').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::setup_archetype::*;

#[test]
fn labels_are_stable_and_distinct() {
    assert_eq!(SetupArchetype::FreshMintFlow.label(), 0);
    assert_eq!(SetupArchetype::CleanOrganicBreadth.label(), 1);
    assert_eq!(SetupArchetype::BundleSniperTrap.label(), 7);
    assert_eq!(SetupArchetype::MigrationMomentum.label(), 8);
    assert_eq!(SetupArchetype::UntradeableTrap.label(), 21);
    assert_eq!(SetupArchetype::Unknown.label(), 22);

    let all = [
        SetupArchetype::FreshMintFlow,
        SetupArchetype::CleanOrganicBreadth,
        SetupArchetype::CtAttentionShock,
        SetupArchetype::PumpLiveStream,
        SetupArchetype::CreatorCultOrCommunity,
        SetupArchetype::DevRecycleRisk,
        SetupArchetype::WalletClusterPump,
        SetupArchetype::BundleSniperTrap,
        SetupArchetype::MigrationMomentum,
        SetupArchetype::PostMigrationRevival,
        SetupArchetype::SocialStuntOrMeta,
        SetupArchetype::PlatformVisibilitySpike,
        SetupArchetype::HighRiskTradableImpulse,
        SetupArchetype::ActiveContinuation,
        SetupArchetype::BreakoutRetest,
        SetupArchetype::FailedBreakdownReversal,
        SetupArchetype::Reclaim,
        SetupArchetype::CompressionExpansion,
        SetupArchetype::MeanReversionSnap,
        SetupArchetype::LiquidityDislocation,
        SetupArchetype::CapitalRotationScalp,
        SetupArchetype::UntradeableTrap,
        SetupArchetype::Unknown,
    ];
    assert_eq!(all.len(), 23);

    let mut seen = [false; 23];
    for a in all {
        let l = a.label() as usize;
        assert!(l < 23, "label {l} out of range");
        assert!(!seen[l], "duplicate label {l}");
        seen[l] = true;
    }
    assert!(seen.iter().all(|&b| b), "labels not dense over 0..=22");

    assert_eq!(
        SetupArchetype::Reclaim.label(),
        SetupArchetype::Reclaim.label()
    );
}
