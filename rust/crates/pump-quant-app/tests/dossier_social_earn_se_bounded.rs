// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_earn' component (leaf 'se_bounded').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_app::social_earn::*;

#[test]
fn se_bounded_eviction_and_determinism() {
    let params = SocialEarnParams {
        track_cap: 2,
        ..SocialEarnParams::standard()
    };
    let mut e = SocialEarn::new(params);
    e.record_call(1, [1u8; 32], 1);
    e.record_call(2, [1u8; 32], 2);
    e.record_call(3, [2u8; 32], 3);
    e.record_call(4, [3u8; 32], 4);
    e.record_outcome(&[1u8; 32], 1_000);
    e.reconcile();
    assert!(
        e.quality_bps_for(1).is_some(),
        "retained mint's caller earns"
    );
    let build = || {
        let mut g = SocialEarn::new(SocialEarnParams::standard());
        for (s, m, ts, net) in [
            (1u64, [1u8; 32], 100u64, 5_000i128),
            (2, [1u8; 32], 110, 5_000),
            (1, [2u8; 32], 120, -2_000),
        ] {
            g.record_call(s, m, ts);
            g.record_outcome(&m, net);
        }
        g.reconcile();
        (g.quality_bps_for(1), g.quality_bps_for(2))
    };
    assert_eq!(build(), build());
}
