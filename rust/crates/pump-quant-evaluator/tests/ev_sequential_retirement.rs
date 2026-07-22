use pump_quant_evaluator::evaluator_stats::Lane;
use pump_quant_evaluator::sequential_retirement::*;

fn cfg(reference: i128, slack: i128, threshold: i128, min_samples: u32) -> RetirementConfig {
    RetirementConfig {
        lane: Lane::Scalp,
        reference_lamports: reference,
        slack_lamports: slack,
        threshold_lamports: threshold,
        min_samples,
    }
}

// Hand-computed CUSUM: reference 0, slack 0, threshold 100. Each outcome -30 ->
// shortfall +30. g: 30,60,90,120. First g>=100 at sample 4. min_samples 2 so
// retirement is allowed there.
#[test]
fn retires_when_deficit_crosses_after_horizon() {
    let outcomes = [-30i128, -30, -30, -30, -30];
    let d = sequential_retirement(&outcomes, &cfg(0, 0, 100, 2));
    assert_eq!(d.verdict, RetirementVerdict::Retire);
    assert_eq!(d.decided_at_sample, Some(4));
    assert_eq!(d.peak_deficit, 150); // 5th outcome pushes g to 150
    assert_eq!(d.n, 5);
}

// Same stream but learning horizon of 5 blocks the sample-4 crossing; the only
// eligible sample (5) still has g=150>=100, so it retires at 5. Prove the guard
// moved the decision, not that it vanished.
#[test]
fn learning_horizon_delays_decision() {
    let outcomes = [-30i128, -30, -30, -30, -30];
    let d = sequential_retirement(&outcomes, &cfg(0, 0, 100, 5));
    assert_eq!(d.verdict, RetirementVerdict::Retire);
    assert_eq!(d.decided_at_sample, Some(5));
}

// With min_samples beyond the stream length, RETIRE can never bind even though
// the deficit is huge -> CONTINUE. Independent horizon check.
#[test]
fn horizon_beyond_stream_forbids_retirement() {
    let outcomes = [-30i128, -30, -30, -30];
    let d = sequential_retirement(&outcomes, &cfg(0, 0, 100, 10));
    assert_eq!(d.verdict, RetirementVerdict::Continue);
    assert_eq!(d.decided_at_sample, None);
    assert_eq!(d.peak_deficit, 120);
}

// Slack absorbs noise: reference 0, slack 40, threshold 100. Outcomes of -30
// give shortfall -30-40 = -70 -> clamped, g stays 0 forever. A healthy-ish lane
// is NOT retired. Independent computation of the reflecting barrier.
#[test]
fn slack_reflects_benign_noise() {
    let outcomes = [-30i128, -30, -30, -30, -30, -30];
    let d = sequential_retirement(&outcomes, &cfg(0, 40, 100, 1));
    assert_eq!(d.verdict, RetirementVerdict::Continue);
    assert_eq!(d.peak_deficit, 0);
}

// A recovery run cannot bank credit against a later slump because of the zero
// reflection. reference 0, slack 0, threshold 50.
// wins +100 (shortfall -100 -> g=0), then losses -20 x3 -> g:20,40,60>=50 at
// the 3rd loss (sample 6). Peak 60.
#[test]
fn zero_reflection_prevents_credit_banking() {
    let outcomes = [100i128, 100, 100, -20, -20, -20];
    let d = sequential_retirement(&outcomes, &cfg(0, 0, 50, 1));
    assert_eq!(d.verdict, RetirementVerdict::Retire);
    assert_eq!(d.decided_at_sample, Some(6));
    assert_eq!(d.peak_deficit, 60);
}

#[test]
fn nonzero_reference_shifts_the_null() {
    // reference 10: an outcome of exactly 10 is neutral, below 10 erodes.
    // outcomes 5,5,5 -> shortfall 5 each -> g:5,10,15. threshold 12 -> cross at 3.
    let outcomes = [5i128, 5, 5];
    let d = sequential_retirement(&outcomes, &cfg(10, 0, 12, 1));
    assert_eq!(d.verdict, RetirementVerdict::Retire);
    assert_eq!(d.decided_at_sample, Some(3));
}
