// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'catalyst_classifier' component (leaf 'classify_no_data_fade_first').
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
fn classify_no_data_fade_first() {
    let t = CatalystThresholds::standard();
    for echo in [0u64, 6_000, 9_000, 10_000] {
        for coord in [0u64, 5_000, 9_000, 10_000] {
            for stream in [false, true] {
                for vel in [-5i64, 0, 7] {
                    let mut f = base();
                    f.sample_count = 0;
                    f.copy_echo_density_bps = echo;
                    f.coordination_bps = coord;
                    f.creator_funding_bps = 9_000;
                    f.platform_surge_bps = 9_000;
                    f.streamer_event = stream;
                    f.attention_velocity = vel;
                    f.net_flow = -1;
                    f.unique_sources = 100;
                    f.independent_breadth = 100;
                    assert_eq!(classify(&f, &t), SocialCatalyst::Unknown);
                }
            }
        }
    }
    let mut g = base();
    g.sample_count = 1;
    g.copy_echo_density_bps = 9_000;
    g.coordination_bps = 9_000;
    assert_eq!(classify(&g, &t), SocialCatalyst::CoordinatedSpam);
}
