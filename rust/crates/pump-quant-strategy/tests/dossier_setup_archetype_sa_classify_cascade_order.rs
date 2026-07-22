// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'setup_archetype' component (leaf 'sa_classify_cascade_order').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::setup_archetype::*;

fn base() -> ArchetypeFeatures {
    ArchetypeFeatures {
        mechanically_sellable: true,
        exit_capacity_bps: 8_000,
        token_age_secs: 100_000,
        migration: MigrationPhase::PreMigration,
        bundle_sniper_bps: 0,
        cluster_share_bps: 0,
        creator_ownership_bps: 0,
        creator_recycle: false,
        independent_buyers: 0,
        attention_velocity: 0,
        livestream_active: false,
        community_score_bps: 0,
        social_stunt_bps: 0,
        platform_visibility_bps: 0,
        structure: StructureState::None,
        rotation_inflow: 0,
    }
}

#[test]
fn priority_cascade_first_match_wins() {
    let th = ClassifierThresholds::test();

    let mut f = base();
    f.bundle_sniper_bps = th.bundle_trap_bps;
    f.structure = StructureState::ActiveContinuation;
    f.migration = MigrationPhase::MigrationEdge;
    assert_eq!(
        classify_archetype(&f, &th),
        SetupArchetype::BundleSniperTrap
    );

    let mut g = base();
    g.cluster_share_bps = th.cluster_pump_bps;
    g.structure = StructureState::Reclaim;
    assert_eq!(
        classify_archetype(&g, &th),
        SetupArchetype::WalletClusterPump
    );

    let mut h = base();
    h.structure = StructureState::ActiveContinuation;
    h.migration = MigrationPhase::MigrationEdge;
    h.attention_velocity = 100_000;
    assert_eq!(
        classify_archetype(&h, &th),
        SetupArchetype::ActiveContinuation
    );

    let mut m = base();
    m.migration = MigrationPhase::MigrationEdge;
    m.attention_velocity = 100_000;
    m.livestream_active = true;
    assert_eq!(
        classify_archetype(&m, &th),
        SetupArchetype::MigrationMomentum
    );

    let mut u = base();
    u.bundle_sniper_bps = th.bundle_trap_bps - 1;
    u.cluster_share_bps = th.cluster_pump_bps - 1;
    u.attention_velocity = th.attention_shock - 1;
    assert_eq!(classify_archetype(&u, &th), SetupArchetype::Unknown);
}
