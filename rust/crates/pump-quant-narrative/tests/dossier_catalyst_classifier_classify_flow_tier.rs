// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'catalyst_classifier' component (leaf 'classify_flow_tier').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::catalyst_classifier::*;

fn base() -> CatalystFeatures {
    CatalystFeatures {
        sample_count: 100,
        copy_echo_density_bps: 0,
        coordination_bps: 0,
        creator_funding_bps: 0,
        platform_surge_bps: 0,
        streamer_event: false,
        net_flow: 0,
        attention_velocity: 0,
        unique_sources: 0,
        independent_breadth: 0,
    }
}

#[test]
fn classify_flow_tier() {
    let mut t = CatalystThresholds::standard();
    t.exit_flow = 100;

    let mut le = base();
    le.attention_velocity = 300;
    le.net_flow = -101;
    assert_eq!(
        classify(&le, &t),
        SocialCatalyst::LateExitLiquidityPromotion
    );

    let mut edge = base();
    edge.attention_velocity = 300;
    edge.net_flow = -100;
    assert_eq!(classify(&edge, &t), SocialCatalyst::PreFlowDiscovery);

    let mut pf = base();
    pf.attention_velocity = 1;
    pf.net_flow = 0;
    assert_eq!(classify(&pf, &t), SocialCatalyst::PreFlowDiscovery);

    let mut la = base();
    la.attention_velocity = 1;
    la.net_flow = 5_000;
    assert_eq!(classify(&la, &t), SocialCatalyst::LiveFlowAmplifier);

    let flat = base();
    assert_eq!(classify(&flat, &t), SocialCatalyst::Unknown);

    let mut gc = base();
    gc.unique_sources = t.genuine_sources;
    gc.independent_breadth = t.genuine_breadth;
    assert_eq!(classify(&gc, &t), SocialCatalyst::GenuineCommunityFormation);
}
