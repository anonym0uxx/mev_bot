// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'micro' component (leaf 'mc_classify_divergence').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_features::micro::*;

#[test]
fn mc_classify_divergence_props() {
    // Same-sign price & CVD -> Confirmation (both branches).
    assert_eq!(classify_divergence(5, 5), CvdDivergence::Confirmation);
    assert_eq!(classify_divergence(-5, -5), CvdDivergence::Confirmation);
    // Price up, CVD not confirming (<=0) -> Bearish (exhaustion).
    assert_eq!(classify_divergence(5, 0), CvdDivergence::Bearish);
    assert_eq!(classify_divergence(5, -3), CvdDivergence::Bearish);
    // Price down, CVD not confirming (>=0) -> Bullish.
    assert_eq!(classify_divergence(-5, 0), CvdDivergence::Bullish);
    assert_eq!(classify_divergence(-5, 2), CvdDivergence::Bullish);
    // Flat price -> Neutral regardless of CVD.
    assert_eq!(classify_divergence(0, 9), CvdDivergence::Neutral);
    assert_eq!(classify_divergence(0, -9), CvdDivergence::Neutral);
    assert_eq!(classify_divergence(0, 0), CvdDivergence::Neutral);

    // Total function: exactly one classification per input; exhaustiveness sweep.
    for pd in -3i128..=3 {
        for cd in -3i128..=3 {
            let got = classify_divergence(pd, cd);
            let expected = if pd > 0 {
                if cd > 0 {
                    CvdDivergence::Confirmation
                } else {
                    CvdDivergence::Bearish
                }
            } else if pd < 0 {
                if cd < 0 {
                    CvdDivergence::Confirmation
                } else {
                    CvdDivergence::Bullish
                }
            } else {
                CvdDivergence::Neutral
            };
            assert_eq!(got, expected, "pd={pd} cd={cd}");
        }
    }
}
