// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_parse' component (leaf 'sp_horizon').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_parse::*;

#[test]
fn sp_horizon_orders_upstream_to_legible() {
    // Telegram (upstream) < X < TikTok < Web (most legible). Provenance ordering
    // only: a lower rank sits earlier, it does not score higher.
    assert!(SocialPlatform::Telegram.horizon_rank() < SocialPlatform::X.horizon_rank());
    assert!(SocialPlatform::X.horizon_rank() < SocialPlatform::TikTok.horizon_rank());
    assert!(SocialPlatform::TikTok.horizon_rank() < SocialPlatform::Web.horizon_rank());
}
