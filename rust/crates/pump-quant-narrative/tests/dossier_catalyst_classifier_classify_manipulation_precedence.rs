// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'catalyst_classifier' component (leaf 'classify_manipulation_precedence').
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
fn classify_manipulation_precedence() {
    let t = CatalystThresholds::standard();

    let mut spam = base();
    spam.copy_echo_density_bps = t.echo_bps;
    spam.coordination_bps = t.coordination_bps;
    spam.attention_velocity = 500;
    spam.net_flow = 0;
    assert_eq!(classify(&spam, &t), SocialCatalyst::CoordinatedSpam);

    let mut echo = base();
    echo.copy_echo_density_bps = t.echo_bps;
    echo.coordination_bps = t.coordination_bps - 1;
    assert_eq!(classify(&echo, &t), SocialCatalyst::CopyEcho);

    let mut cr = base();
    cr.copy_echo_density_bps = t.echo_bps - 1;
    cr.coordination_bps = 10_000;
    cr.creator_funding_bps = t.creator_bps;
    assert_eq!(classify(&cr, &t), SocialCatalyst::CreatorFundedPush);

    let mut both = base();
    both.creator_funding_bps = t.creator_bps;
    both.platform_surge_bps = 10_000;
    both.streamer_event = true;
    assert_eq!(classify(&both, &t), SocialCatalyst::CreatorFundedPush);

    let mut pl = base();
    pl.platform_surge_bps = t.platform_bps;
    pl.streamer_event = true;
    assert_eq!(classify(&pl, &t), SocialCatalyst::PlatformVisibilitySurge);
}
