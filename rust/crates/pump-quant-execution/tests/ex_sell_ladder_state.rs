#![allow(unused_imports)]
use pump_quant_execution::ex_sell_ladder_state::*;

// Independent reference model of the transition, recomputed here so a memorized
// answer cannot pass.
fn expected_next(cur: LadderState, ctx: LadderCtx) -> LadderState {
    let mut n = cur;
    let now = ctx.now_ms;
    match ctx.outcome {
        SellOutcome::Confirmed | SellOutcome::BalanceZero => n.phase = LadderPhase::Completed,
        SellOutcome::Pending | SellOutcome::MaybeConfirmed => {
            n.phase = LadderPhase::Active;
            n.last_attempt_ms = now;
        }
        SellOutcome::CircuitOpen { remaining_ms } => {
            n.phase = LadderPhase::Active;
            n.last_attempt_ms = now + remaining_ms;
        }
        SellOutcome::Failed => {
            let adv = cur.level + 1;
            n.last_attempt_ms = now;
            if n.first_attempt_ms == 0 {
                n.first_attempt_ms = now;
            }
            if adv >= 5 {
                n.level = 4;
                n.phase = LadderPhase::Exhausted;
                n.last_attempt_ms = now + 25_000;
            } else {
                n.level = adv;
                n.phase = LadderPhase::Active;
            }
        }
    }
    n
}

#[test]
fn ladder_constant_is_monotonically_aggressive() {
    assert_eq!(ESCALATION_LADDER.len(), 5);
    for i in 1..ESCALATION_LADDER.len() {
        assert!(ESCALATION_LADDER[i].max_slippage_bps >= ESCALATION_LADDER[i - 1].max_slippage_bps);
        assert!(
            ESCALATION_LADDER[i].extra_priority_lamports
                >= ESCALATION_LADDER[i - 1].extra_priority_lamports
        );
    }
    assert_eq!(ESCALATION_LADDER[4].strategy, SellStrategy::ForceMarketSell);
    assert_eq!(ESCALATION_LADDER[0].max_slippage_bps, 300);
    assert_eq!(ESCALATION_LADDER[4].max_slippage_bps, 9_900);
}

#[test]
fn confirmed_completes_and_preserves_level() {
    let cur = LadderState {
        level: 2,
        phase: LadderPhase::Active,
        last_attempt_ms: 500,
        first_attempt_ms: 100,
        queued_at_ms: 50,
    };
    let ctx = LadderCtx {
        now_ms: 9_000,
        outcome: SellOutcome::Confirmed,
    };
    let got = sell_ladder_next(cur, ctx);
    assert_eq!(got, expected_next(cur, ctx));
    assert_eq!(got.phase, LadderPhase::Completed);
    assert_eq!(got.level, 2);
}

#[test]
fn failed_escalates_one_level_and_stamps_first_attempt() {
    let cur = LadderState::new(1_000);
    let ctx = LadderCtx {
        now_ms: 4_000,
        outcome: SellOutcome::Failed,
    };
    let got = sell_ladder_next(cur, ctx);
    assert_eq!(got, expected_next(cur, ctx));
    assert_eq!(got.level, 1);
    assert_eq!(got.first_attempt_ms, 4_000);
    assert_eq!(got.last_attempt_ms, 4_000);
    assert_eq!(got.phase, LadderPhase::Active);
}

#[test]
fn failed_at_last_level_exhausts_with_cooldown() {
    let cur = LadderState {
        level: 4,
        phase: LadderPhase::Active,
        last_attempt_ms: 20_000,
        first_attempt_ms: 5_000,
        queued_at_ms: 1_000,
    };
    let ctx = LadderCtx {
        now_ms: 30_000,
        outcome: SellOutcome::Failed,
    };
    let got = sell_ladder_next(cur, ctx);
    assert_eq!(got, expected_next(cur, ctx));
    assert_eq!(got.level, 4);
    assert_eq!(got.phase, LadderPhase::Exhausted);
    // now + EXHAUSTED_COOLDOWN_MS
    assert_eq!(got.last_attempt_ms, 30_000 + EXHAUSTED_COOLDOWN_MS);
    assert_eq!(EXHAUSTED_COOLDOWN_MS, 25_000);
}

#[test]
fn maybe_confirmed_does_not_escalate() {
    let cur = LadderState {
        level: 3,
        phase: LadderPhase::Active,
        last_attempt_ms: 100,
        first_attempt_ms: 50,
        queued_at_ms: 10,
    };
    let ctx = LadderCtx {
        now_ms: 7_777,
        outcome: SellOutcome::MaybeConfirmed,
    };
    let got = sell_ladder_next(cur, ctx);
    assert_eq!(got, expected_next(cur, ctx));
    assert_eq!(got.level, 3);
    assert_eq!(got.last_attempt_ms, 7_777);
}

#[test]
fn circuit_open_delays_by_remaining() {
    let cur = LadderState::new(0);
    let ctx = LadderCtx {
        now_ms: 2_000,
        outcome: SellOutcome::CircuitOpen {
            remaining_ms: 8_500,
        },
    };
    let got = sell_ladder_next(cur, ctx);
    assert_eq!(got, expected_next(cur, ctx));
    assert_eq!(got.level, 0);
    assert_eq!(got.last_attempt_ms, 10_500);
    assert_eq!(got.phase, LadderPhase::Active);
}

#[test]
fn full_failure_sweep_matches_reference() {
    // Drive a fresh sell through 6 consecutive failures and compare each step.
    let mut cur = LadderState::new(0);
    let mut now = 1_000u64;
    let expected_levels = [1u8, 2, 3, 4, 4, 4];
    for (i, &exp_level) in expected_levels.iter().enumerate() {
        let ctx = LadderCtx {
            now_ms: now,
            outcome: SellOutcome::Failed,
        };
        let next = sell_ladder_next(cur, ctx);
        assert_eq!(next, expected_next(cur, ctx), "step {i}");
        assert_eq!(next.level, exp_level, "level at step {i}");
        cur = next;
        now += 3_000;
    }
    assert_eq!(cur.phase, LadderPhase::Exhausted);
}

#[test]
fn escalation_accessor_clamps() {
    let cur = LadderState {
        level: 9,
        phase: LadderPhase::Active,
        last_attempt_ms: 0,
        first_attempt_ms: 0,
        queued_at_ms: 0,
    };
    assert_eq!(cur.escalation().strategy, SellStrategy::ForceMarketSell);
}
