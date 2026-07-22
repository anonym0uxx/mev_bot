// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fee_plausibility' component (leaf 'fee_intensity_scaled_metric').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::fee_plausibility::*;

#[test]
fn fee_intensity_scaled_metric_props() {
    // Zero-activity guard: always 0 regardless of fees.
    assert_eq!(fee_intensity(0, 0), 0);
    assert_eq!(fee_intensity(1_000_000, 0), 0);
    assert_eq!(fee_intensity(u128::MAX, 0), 0);

    // Zero fees => zero intensity for any positive activity.
    for a in 1u64..50 {
        assert_eq!(fee_intensity(0, a), 0);
    }

    // Exact scaled formula: fees * INTENSITY_SCALE / activity, and
    // monotonic non-decreasing in fees for fixed activity.
    for a in 1u64..40 {
        let mut prev = 0u64;
        for f in 0u128..200 {
            let got = fee_intensity(f, a);
            let expected =
                (f.saturating_mul(INTENSITY_SCALE) / a as u128).min(u64::MAX as u128) as u64;
            assert_eq!(got, expected, "fees={f} activity={a}");
            assert!(got >= prev, "non-monotonic fees={f} activity={a}");
            prev = got;
        }
    }

    // Concrete documented value: 100_000 lamports over 10 activity.
    assert_eq!(fee_intensity(100_000, 10), 10_000 * INTENSITY_SCALE as u64);

    // Saturating cast: a product exceeding u64::MAX clamps, never wraps/panics.
    assert_eq!(fee_intensity(u128::MAX, 1), u64::MAX);
    assert_eq!(fee_intensity(u128::MAX, 2), u64::MAX);
}
