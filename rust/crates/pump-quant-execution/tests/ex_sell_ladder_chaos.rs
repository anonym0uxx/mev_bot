#![allow(unused_imports)]
//! G3 / criterion 79: chaos (property) test for the ExitRemediationLadder.
//!
//! Drives `sell_ladder_next` + `should_escalate` with deterministic,
//! hash-derived pseudo-random *mixed* sequences of Failed / CircuitOpen /
//! MaybeConfirmed / partial-fill / template-invalidation events and asserts the
//! ladder ALWAYS terminates in `Exhausted` or a completed sell within a bounded
//! number of steps — no stuck state, no model input, no rng crate (the entropy
//! is a splitmix64 of the sequence index).

use pump_quant_execution::ex_sell_ladder_escalate::{should_escalate, LadderThresholds, MAX_LEVEL};
use pump_quant_execution::ex_sell_ladder_state::{
    sell_ladder_next, LadderCtx, LadderPhase, LadderState, SellOutcome, LADDER_LEN,
};

/// splitmix64 finalizer — a pure integer hash, NOT an rng crate.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic draw for `(seed, step)`.
fn draw(seed: u64, step: u32) -> u64 {
    mix(seed
        .wrapping_mul(0x0000_0100_0000_01B3)
        .wrapping_add(u64::from(step).wrapping_mul(0xD1B5_4A32_D192_ED03)))
}

/// Max steps allowed before we declare the ladder "stuck". The ladder can only
/// escalate `LADDER_LEN` times before it is `Exhausted`; the bound is generous.
const MAX_STEPS: u32 = 32;

/// Circuit-open remaining is capped so the step delta always outruns it,
/// guaranteeing the escalation clock eventually fires.
const CIRCUIT_REMAINING_CAP: u64 = 5_000;

#[test]
fn ladder_always_terminates_under_chaos() {
    let thresholds = LadderThresholds::default();
    let floor = thresholds.min_unfilled_bps;
    let max_timeout = *thresholds.level_timeout_ms.iter().max().unwrap(); // 10_000

    let mut completed_runs = 0u32;
    let mut exhausted_runs = 0u32;
    let seeds: u64 = 500; // ≥ 200 generated sequences

    for seed in 0..seeds {
        let mut state = LadderState::new(0);
        let mut now: u64 = 1_000;
        let mut unfilled_bps: u32 = 10_000;
        let mut terminal: Option<LadderPhase> = None;
        let mut steps = 0u32;

        while steps < MAX_STEPS {
            // Bounded-state invariants must hold at every step.
            assert!(
                state.level < LADDER_LEN,
                "level out of range: {}",
                state.level
            );

            let elapsed = now.saturating_sub(state.last_attempt_ms);
            let d = draw(seed, steps);

            // Choose the outcome for this step.
            let outcome = if unfilled_bps < floor {
                // Order is essentially filled: it completes.
                SellOutcome::Confirmed
            } else {
                match d % 7 {
                    0 => SellOutcome::Confirmed,
                    1 => SellOutcome::BalanceZero,
                    2 => SellOutcome::Failed,
                    // template-invalidation: the built template is invalid and
                    // must be rebuilt at a higher level — modelled as Failed.
                    6 => SellOutcome::Failed,
                    3 => {
                        // MaybeConfirmed: escalate on timeout, else keep waiting.
                        if should_escalate(state.level, elapsed, unfilled_bps, &thresholds) {
                            SellOutcome::Failed
                        } else {
                            SellOutcome::MaybeConfirmed
                        }
                    }
                    4 => {
                        // CircuitOpen: escalate on timeout, else back off.
                        if should_escalate(state.level, elapsed, unfilled_bps, &thresholds) {
                            SellOutcome::Failed
                        } else {
                            let remaining = 1 + (d >> 8) % CIRCUIT_REMAINING_CAP;
                            SellOutcome::CircuitOpen {
                                remaining_ms: remaining,
                            }
                        }
                    }
                    _ => {
                        // partial-fill: reduce the unfilled fraction, then either
                        // complete (if now below the floor) or wait/escalate.
                        let fill = 1 + ((d >> 16) % 4_000) as u32;
                        unfilled_bps = unfilled_bps.saturating_sub(fill);
                        if unfilled_bps < floor {
                            SellOutcome::Confirmed
                        } else if should_escalate(state.level, elapsed, unfilled_bps, &thresholds) {
                            SellOutcome::Failed
                        } else {
                            SellOutcome::MaybeConfirmed
                        }
                    }
                }
            };

            let ctx = LadderCtx {
                now_ms: now,
                outcome,
            };
            state = sell_ladder_next(state, ctx);

            match state.phase {
                LadderPhase::Completed | LadderPhase::Exhausted => {
                    terminal = Some(state.phase);
                    break;
                }
                LadderPhase::Active => {}
            }

            // Advance the clock by more than the largest timeout + circuit cap so
            // the escalation clock is guaranteed to fire on the next step.
            let delta = (max_timeout + CIRCUIT_REMAINING_CAP + 1_000) + (d % 8_000);
            now = now.saturating_add(delta);
            steps += 1;
        }

        let phase = terminal.unwrap_or_else(|| {
            panic!("seed {seed}: ladder did NOT terminate within {MAX_STEPS} steps (stuck state)")
        });
        match phase {
            LadderPhase::Completed => completed_runs += 1,
            LadderPhase::Exhausted => exhausted_runs += 1,
            LadderPhase::Active => unreachable!(),
        }
        // Bounded: it must have terminated strictly before the step budget.
        assert!(steps < MAX_STEPS, "seed {seed}: hit step budget");
    }

    // Sanity: the chaos actually exercised BOTH terminal outcomes across seeds,
    // otherwise the test would be vacuous.
    assert!(completed_runs > 0, "no run ever completed a sell");
    assert!(exhausted_runs > 0, "no run ever exhausted the ladder");
    assert_eq!(completed_runs + exhausted_runs, seeds as u32);
    // MAX_LEVEL is the last escalation index; consistency with the state module.
    assert_eq!(u64::from(MAX_LEVEL) + 1, u64::from(LADDER_LEN));
}

#[test]
fn pure_waiting_then_timeout_escalates_to_exhausted() {
    // A degenerate sequence: never confirmed, always timing out -> must exhaust.
    let thresholds = LadderThresholds::default();
    let mut state = LadderState::new(0);
    let mut now = 0u64;
    for _ in 0..LADDER_LEN {
        now += 20_000; // beyond any level timeout
        let elapsed = now.saturating_sub(state.last_attempt_ms);
        // Timeout is due at every step here; the run-loop escalates (Failed).
        assert!(
            state.level >= MAX_LEVEL || should_escalate(state.level, elapsed, 10_000, &thresholds)
        );
        state = sell_ladder_next(
            state,
            LadderCtx {
                now_ms: now,
                outcome: SellOutcome::Failed,
            },
        );
    }
    assert_eq!(state.phase, LadderPhase::Exhausted);
    assert_eq!(state.level, LADDER_LEN - 1);
}
