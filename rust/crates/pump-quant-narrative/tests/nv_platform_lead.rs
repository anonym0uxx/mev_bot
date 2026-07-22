use pump_quant_narrative::{nv_platform_lead, PlatformLead};

#[test]
fn mainstream_leads_beyond_tolerance() {
    // mainstream at t=100, crypto at t=500, tolerance 50 -> gap 400.
    assert_eq!(
        nv_platform_lead(Some(100), Some(500), 50),
        PlatformLead::MainstreamLeads(400)
    );
}

#[test]
fn crypto_leads_beyond_tolerance() {
    // crypto first (t=100) before mainstream (t=460), tolerance 50 -> gap 360.
    assert_eq!(
        nv_platform_lead(Some(460), Some(100), 50),
        PlatformLead::CryptoLeads(360)
    );
}

#[test]
fn within_tolerance_is_simultaneous() {
    // gap 30 <= tolerance 50.
    assert_eq!(
        nv_platform_lead(Some(100), Some(130), 50),
        PlatformLead::Simultaneous
    );
    assert_eq!(
        nv_platform_lead(Some(130), Some(100), 50),
        PlatformLead::Simultaneous
    );
}

#[test]
fn equal_instants_are_simultaneous() {
    assert_eq!(
        nv_platform_lead(Some(200), Some(200), 0),
        PlatformLead::Simultaneous
    );
}

#[test]
fn boundary_gap_equal_tolerance_is_simultaneous() {
    // gap 50 == tolerance 50 -> Simultaneous (<=).
    assert_eq!(
        nv_platform_lead(Some(100), Some(150), 50),
        PlatformLead::Simultaneous
    );
    // one past tolerance flips it.
    assert_eq!(
        nv_platform_lead(Some(100), Some(151), 50),
        PlatformLead::MainstreamLeads(51)
    );
}

#[test]
fn missing_data_never_fabricates() {
    assert_eq!(nv_platform_lead(None, Some(100), 0), PlatformLead::NoData);
    assert_eq!(nv_platform_lead(Some(100), None, 0), PlatformLead::NoData);
    assert_eq!(nv_platform_lead(None, None, 0), PlatformLead::NoData);
}

#[test]
fn large_gap_saturates() {
    assert_eq!(
        nv_platform_lead(Some(0), Some(u64::MAX), 0),
        PlatformLead::MainstreamLeads(u64::MAX)
    );
}
