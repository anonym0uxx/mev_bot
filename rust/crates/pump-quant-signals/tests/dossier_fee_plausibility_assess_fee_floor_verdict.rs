// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fee_plausibility' component (leaf 'assess_fee_floor_verdict').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::fee_plausibility::*;

#[test]
fn assess_fee_floor_verdict_props() {
    let cfg = FeeFloorConfig::neutral(); // min_activity 8, floor 5_000 * 1e6

    // Below min_activity => InsufficientActivity, never fades, echoes count.
    for a in 0u64..cfg.min_activity {
        let r = assess_fee_floor(1_000_000_000, a, &cfg);
        assert_eq!(r.status, FeeFloorStatus::InsufficientActivity);
        assert_eq!(r.fade_bps, 0);
        assert_eq!(r.activity_count, a);
    }

    // Sweep fees at fixed sufficient activity, reconstructing the whole verdict.
    let activity = 100u64;
    for f in 0u128..600 {
        let total = f * 100_000; // scale up so intensity spans below/above floor
        let r = assess_fee_floor(total, activity, &cfg);
        let intensity = fee_intensity(total, activity);
        assert_eq!(r.intensity, intensity);
        assert_eq!(r.activity_count, activity);
        // fade_bps is always a valid basis-point magnitude.
        assert!(r.fade_bps <= 10_000);
        if intensity >= cfg.floor_intensity {
            assert_eq!(r.status, FeeFloorStatus::Plausible);
            assert_eq!(r.fade_bps, 0);
        } else {
            assert_eq!(r.status, FeeFloorStatus::ImplausiblyLow);
            let deficit = cfg.floor_intensity - intensity;
            let expected =
                ((deficit as u128 * 10_000) / cfg.floor_intensity as u128).min(10_000) as u32;
            assert_eq!(r.fade_bps, expected, "total={total}");
            // Implausibly-low always carries a strictly positive fade.
            assert!(r.fade_bps >= 1);
        }
    }

    // floor_intensity == 0 => always Plausible with no fade (guarded div).
    let zero_floor = FeeFloorConfig {
        min_activity: 1,
        floor_intensity: 0,
    };
    assert_eq!(
        assess_fee_floor(0, 100, &zero_floor).status,
        FeeFloorStatus::Plausible
    );
    assert_eq!(assess_fee_floor(0, 100, &zero_floor).fade_bps, 0);

    // Concrete rejection case: 100 txs at 1_000 lamports each => 8_000 bps fade.
    let r = assess_fee_floor(100_000, 100, &cfg);
    assert_eq!(r.status, FeeFloorStatus::ImplausiblyLow);
    assert_eq!(r.intensity, 1_000 * INTENSITY_SCALE as u64);
    assert_eq!(r.fade_bps, 8_000);
}
