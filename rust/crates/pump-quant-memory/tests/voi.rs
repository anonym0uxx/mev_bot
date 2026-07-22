//! Leaf: voi. Verifies deterministic value-of-information scoring and ranking
//! against independently hand-computed expectations, including edge cases
//! (zero/negative impact, ties, overflow saturation) and monotonicity (§56.10,
//! §22).

use pump_quant_memory::rows::{Hypothesis, HypothesisId, InferenceState};
use pump_quant_memory::voi::{rank, rank_open, voi_score, REF_HALF_LIFE_SECS};

fn hyp(
    id: u64,
    impact: i128,
    prob_bps: i64,
    cost: u64,
    half_life_secs: u64,
    status: InferenceState,
) -> Hypothesis {
    Hypothesis {
        id: HypothesisId(id),
        schema_version: 1,
        statement_hash: [0u8; 32],
        expected_impact_lamports: impact,
        prob_true_bps: prob_bps,
        cost_to_test_lamports: cost,
        edge_half_life_secs: half_life_secs,
        status,
    }
}

const REF: u64 = REF_HALF_LIFE_SECS as u64; // 86_400

#[test]
fn score_matches_hand_computed_values() {
    // gross = impact * prob/10_000 * half_life/REF ; voi = gross - cost.
    // A: 50% of 1 SOL at reference half-life, minus 0.1 SOL cost.
    let a = hyp(
        1,
        1_000_000_000,
        5_000,
        100_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&a), 400_000_000);

    // B: 25% of 2 SOL at ref half-life, no cost => 0.5 SOL.
    let b = hyp(2, 2_000_000_000, 2_500, 0, REF, InferenceState::Hypothesis);
    assert_eq!(voi_score(&b), 500_000_000);

    // C: 100% of 0.5 SOL at ref half-life, minus 0.05 SOL cost.
    let c = hyp(
        3,
        500_000_000,
        10_000,
        50_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&c), 450_000_000);

    // D: fade hypothesis, negative impact dominates.
    let d = hyp(
        4,
        -1_000_000_000,
        8_000,
        10_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&d), -810_000_000);

    // E: zero impact => voi is just minus cost (here zero).
    let e = hyp(5, 0, 10_000, 0, REF, InferenceState::Hypothesis);
    assert_eq!(voi_score(&e), 0);
}

#[test]
fn half_life_scales_value() {
    // Half the reference half-life halves the gross value.
    let full = hyp(1, 1_000_000_000, 10_000, 0, REF, InferenceState::Hypothesis);
    let half = hyp(
        2,
        1_000_000_000,
        10_000,
        0,
        REF / 2,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&full), 1_000_000_000);
    assert_eq!(voi_score(&half), 500_000_000);
}

#[test]
fn ranking_is_descending_by_score_then_ascending_id() {
    let hs = vec![
        hyp(
            1,
            1_000_000_000,
            5_000,
            100_000_000,
            REF,
            InferenceState::Hypothesis,
        ), // 400M
        hyp(2, 2_000_000_000, 2_500, 0, REF, InferenceState::Hypothesis), // 500M
        hyp(
            3,
            500_000_000,
            10_000,
            50_000_000,
            REF,
            InferenceState::Hypothesis,
        ), // 450M
        hyp(
            4,
            -1_000_000_000,
            8_000,
            10_000_000,
            REF,
            InferenceState::Hypothesis,
        ), // -810M
        hyp(5, 0, 10_000, 0, REF, InferenceState::Hypothesis),            // 0
    ];
    let ranked = rank(&hs);
    let order: Vec<u64> = ranked.iter().map(|r| r.id.0).collect();
    assert_eq!(order, vec![2, 3, 1, 5, 4]);
    // Scores carried alongside.
    assert_eq!(ranked[0].score, 500_000_000);
    assert_eq!(ranked[4].score, -810_000_000);
}

#[test]
fn ties_break_by_ascending_id() {
    // Three zero-impact hypotheses all score 0; ids must come out ascending.
    let hs = vec![
        hyp(8, 0, 10_000, 0, REF, InferenceState::Hypothesis),
        hyp(3, 0, 10_000, 0, REF, InferenceState::Hypothesis),
        hyp(6, 0, 10_000, 0, REF, InferenceState::Hypothesis),
    ];
    let order: Vec<u64> = rank(&hs).iter().map(|r| r.id.0).collect();
    assert_eq!(order, vec![3, 6, 8]);
}

#[test]
fn rank_open_excludes_closed_hypotheses() {
    let hs = vec![
        hyp(
            1,
            2_000_000_000,
            10_000,
            0,
            REF,
            InferenceState::ValidatedInference,
        ), // closed
        hyp(2, 1_000_000_000, 10_000, 0, REF, InferenceState::Hypothesis), // open
        hyp(
            3,
            500_000_000,
            10_000,
            0,
            REF,
            InferenceState::RejectedInference,
        ), // closed
        hyp(
            4,
            100_000_000,
            10_000,
            0,
            REF,
            InferenceState::ProvisionalInference,
        ), // open
    ];
    let order: Vec<u64> = rank_open(&hs).iter().map(|r| r.id.0).collect();
    // Only ids 2 and 4 survive; 2 (1 SOL) outranks 4 (0.1 SOL).
    assert_eq!(order, vec![2, 4]);
}

#[test]
fn monotonic_in_each_input() {
    let base = hyp(
        1,
        1_000_000_000,
        5_000,
        100_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    let base_score = voi_score(&base);

    // More impact (positive) -> higher voi.
    let more_impact = hyp(
        1,
        2_000_000_000,
        5_000,
        100_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert!(voi_score(&more_impact) > base_score);

    // Higher probability -> higher voi.
    let more_prob = hyp(
        1,
        1_000_000_000,
        9_000,
        100_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert!(voi_score(&more_prob) > base_score);

    // Higher cost -> lower voi.
    let more_cost = hyp(
        1,
        1_000_000_000,
        5_000,
        300_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert!(voi_score(&more_cost) < base_score);

    // Longer half-life -> higher voi (positive impact).
    let more_hl = hyp(
        1,
        1_000_000_000,
        5_000,
        100_000_000,
        REF * 3,
        InferenceState::Hypothesis,
    );
    assert!(voi_score(&more_hl) > base_score);
}

#[test]
fn monotonicity_sweep_multiple_inputs() {
    // Sweep probability across many values; voi must be non-decreasing.
    let mut prev = i128::MIN;
    for p in (0..=10_000).step_by(500) {
        let h = hyp(
            9,
            750_000_000,
            p,
            25_000_000,
            REF,
            InferenceState::Hypothesis,
        );
        let s = voi_score(&h);
        assert!(s >= prev, "voi decreased as probability rose: p={p}");
        prev = s;
    }
}

#[test]
fn positive_overflow_saturates_to_max_region() {
    // impact * prob overflows i128 => gross saturates to i128::MAX; with zero cost
    // the voi stays at i128::MAX.
    let h = hyp(1, i128::MAX, 10_000, 0, 0, InferenceState::Hypothesis);
    assert_eq!(voi_score(&h), i128::MAX);
}

#[test]
fn negative_overflow_saturates_to_min() {
    let h = hyp(
        1,
        i128::MIN,
        10_000,
        1_000_000,
        1,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&h), i128::MIN);
}

#[test]
fn zero_probability_yields_negative_cost() {
    // Zero probability => gross 0 => voi is exactly minus the test cost.
    let h = hyp(
        1,
        5_000_000_000,
        0,
        42_000_000,
        REF,
        InferenceState::Hypothesis,
    );
    assert_eq!(voi_score(&h), -42_000_000);
}
