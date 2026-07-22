// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'narrative' component (leaf 'nv_platform_lead').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_narrative::narrative::*;

#[test]
fn nv_pl_lead_directions_and_tolerance() {
    // crypto_first(100) later than mainstream(40), gap 60 > tol 10 => MainstreamLeads(60).
    assert_eq!(
        nv_platform_lead(Some(40), Some(100), 10),
        PlatformLead::MainstreamLeads(60)
    );
    // mainstream(200) later than crypto(150), gap 50 > tol 10 => CryptoLeads(50).
    assert_eq!(
        nv_platform_lead(Some(200), Some(150), 10),
        PlatformLead::CryptoLeads(50)
    );
    // gap within tolerance => Simultaneous (boundary: gap == tolerance).
    assert_eq!(
        nv_platform_lead(Some(40), Some(50), 10),
        PlatformLead::Simultaneous
    );
    // equal instants => Simultaneous.
    assert_eq!(
        nv_platform_lead(Some(77), Some(77), 0),
        PlatformLead::Simultaneous
    );
}

#[test]
fn nv_pl_missing_data_is_nodata() {
    assert_eq!(nv_platform_lead(None, Some(10), 5), PlatformLead::NoData);
    assert_eq!(nv_platform_lead(Some(10), None, 5), PlatformLead::NoData);
    assert_eq!(nv_platform_lead(None, None, 5), PlatformLead::NoData);
    // gap just over tolerance flips from Simultaneous to a lead.
    assert_eq!(
        nv_platform_lead(Some(0), Some(11), 10),
        PlatformLead::MainstreamLeads(11)
    );
}
