//! Tests for `ex_live_arming`.
//!
//! Nine of these are negative controls. That ratio is deliberate: this gate is
//! the only thing between a defect and the balance, and a limit that has never
//! been observed to refuse anything is not a limit.

use pump_quant_execution::ex_live_arming::*;

const SOL: u64 = 1_000_000_000;

fn envelope() -> LiveEnvelope {
    LiveEnvelope {
        max_position_lamports: SOL / 10,      // 0.1 SOL
        max_total_deployed_lamports: SOL / 2, // 0.5 SOL across all positions
        max_open_positions: 3,
        max_entries_per_hour: 20,
        daily_loss_limit_lamports: SOL / 5,   // 0.2 SOL
        max_entry_slippage_bps: 500,          // 5%
        heartbeat_timeout_ms: 5 * 60 * 1_000, // 5 minutes
    }
}

fn armed() -> LiveGate {
    let mut g = LiveGate::new();
    g.arm(envelope(), 0).expect("envelope admits trades");
    g
}

// ─────────────────────────── autonomy actually works ─────────────────────────

#[test]
fn armed_gate_admits_entries_without_any_further_approval() {
    // The point of the design: one arming action, then the bot trades. Twenty
    // consecutive entries, no human anywhere in this loop.
    let mut g = armed();
    for i in 0..20u64 {
        let a = g.admit_entry(SOL / 100, 100, i * 1_000);
        assert!(a.is_allowed(), "entry {i} was refused: {a:?}");
        g.record_entry_fill(SOL / 100, i * 1_000);
        g.record_exit_fill(SOL / 100, 1_000);
    }
    assert_eq!(g.open_positions(), 0);
}

#[test]
fn exits_are_admitted_unconditionally() {
    // Disarmed for every reason there is; exits still go through. A kill switch
    // that blocks selling traps capital instead of protecting it.
    let mut g = armed();
    for r in [
        DisarmReason::NeverArmed,
        DisarmReason::OperatorDisarmed,
        DisarmReason::DailyLossBreached,
        DisarmReason::HeartbeatLost,
    ] {
        g.disarm(r);
        assert!(g.admit_exit().is_allowed(), "exit refused while {r:?}");
    }
    assert!(LiveGate::new().admit_exit().is_allowed());
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_fresh_gate_refuses_entries() {
    let mut g = LiveGate::new();
    assert_eq!(
        g.admit_entry(1, 0, 0),
        Admission::Deny(DenyReason::Disarmed(DisarmReason::NeverArmed))
    );
}

#[test]
fn negative_control_empty_envelope_cannot_be_armed() {
    let mut g = LiveGate::new();
    assert_eq!(
        g.arm(LiveEnvelope::closed(), 0),
        Err(DenyReason::EnvelopeClosed)
    );
    // A zero in any single limit must not read as "unlimited".
    for mutate in [
        |e: &mut LiveEnvelope| e.max_position_lamports = 0,
        |e: &mut LiveEnvelope| e.max_total_deployed_lamports = 0,
        |e: &mut LiveEnvelope| e.max_open_positions = 0,
        |e: &mut LiveEnvelope| e.max_entries_per_hour = 0,
    ] {
        let mut env = envelope();
        mutate(&mut env);
        assert_eq!(
            LiveGate::new().arm(env, 0),
            Err(DenyReason::EnvelopeClosed),
            "a zeroed limit must close the envelope, not open it"
        );
    }
}

#[test]
fn negative_control_oversize_position_refused() {
    let mut g = armed();
    let over = envelope().max_position_lamports + 1;
    assert_eq!(
        g.admit_entry(over, 0, 0),
        Admission::Deny(DenyReason::PositionTooLarge {
            requested: over,
            ceiling: SOL / 10
        })
    );
    // Exactly at the ceiling is admitted - the limit is inclusive.
    assert!(g.admit_entry(SOL / 10, 0, 0).is_allowed());
}

#[test]
fn negative_control_deployed_cap_refused() {
    let mut g = armed();
    // Three positions of 0.1 SOL, then a fourth would exceed 0.5 SOL deployed
    // only if open_positions allowed it; raise that so the cap is what binds.
    let mut env = envelope();
    env.max_open_positions = 10;
    env.max_total_deployed_lamports = SOL / 4; // 0.25 SOL
    g.arm(env, 0).unwrap();

    g.record_entry_fill(SOL / 10, 0);
    g.record_entry_fill(SOL / 10, 0);
    assert_eq!(g.deployed_lamports(), SOL / 5);

    match g.admit_entry(SOL / 10, 0, 0) {
        Admission::Deny(DenyReason::DeployedCapExceeded { would_be, ceiling }) => {
            assert_eq!(would_be, SOL / 10 * 3);
            assert_eq!(ceiling, SOL / 4);
        }
        other => panic!("expected DeployedCapExceeded, got {other:?}"),
    }
}

#[test]
fn negative_control_too_many_open_refused() {
    let mut g = armed();
    for _ in 0..3 {
        g.record_entry_fill(SOL / 100, 0);
    }
    assert_eq!(
        g.admit_entry(SOL / 100, 0, 0),
        Admission::Deny(DenyReason::TooManyOpen {
            open: 3,
            ceiling: 3
        })
    );
}

#[test]
fn negative_control_rate_limit_refused_then_recovers() {
    let mut g = armed();
    for i in 0..20u64 {
        g.record_entry_fill(1, i);
        g.record_exit_fill(1, 0);
    }
    match g.admit_entry(1, 0, 100) {
        Admission::Deny(DenyReason::RateLimited { in_window, ceiling }) => {
            assert_eq!(in_window, 20);
            assert_eq!(ceiling, 20);
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    // One hour and a millisecond later the window has rolled clear. The
    // heartbeat is required: an hour of silence would otherwise trip the
    // dead-man's switch first, which is the correct precedence and is asserted
    // separately in negative_control_dead_mans_switch_fires_on_stale_heartbeat.
    g.heartbeat(RATE_WINDOW_MS + 1);
    assert!(g.admit_entry(1, 0, RATE_WINDOW_MS + 1).is_allowed());
}

#[test]
fn negative_control_wide_slippage_refused() {
    let mut g = armed();
    assert_eq!(
        g.admit_entry(1, 501, 0),
        Admission::Deny(DenyReason::SlippageTooWide {
            quoted_bps: 501,
            ceiling_bps: 500
        })
    );
    assert!(g.admit_entry(1, 500, 0).is_allowed());
}

#[test]
fn negative_control_daily_loss_trips_the_kill_switch() {
    let mut g = armed();
    g.record_entry_fill(SOL / 10, 0);
    // A loss of exactly the limit trips it - the limit is a ceiling on loss.
    g.record_exit_fill(SOL / 10, -((SOL / 5) as i64));
    assert_eq!(
        g.state(0),
        ArmState::Disarmed(DisarmReason::DailyLossBreached)
    );
    assert_eq!(
        g.admit_entry(1, 0, 0),
        Admission::Deny(DenyReason::Disarmed(DisarmReason::DailyLossBreached))
    );
    // And the position that is still open can still be closed.
    assert!(g.admit_exit().is_allowed());
}

#[test]
fn negative_control_a_new_day_does_not_undo_the_kill_switch() {
    // A system that can wait out its own kill switch does not have one.
    let mut g = armed();
    g.record_entry_fill(SOL / 10, 0);
    g.record_exit_fill(SOL / 10, -((SOL / 5) as i64));
    assert_eq!(
        g.state(0),
        ArmState::Disarmed(DisarmReason::DailyLossBreached)
    );

    g.roll_day(1);
    assert_eq!(g.day_realized_pnl(), 0, "the daily counter resets");
    assert_eq!(
        g.state(0),
        ArmState::Disarmed(DisarmReason::DailyLossBreached),
        "but arming does not - only an operator re-arms"
    );
    // The operator re-arming is what restores trading.
    g.arm(envelope(), 0).unwrap();
    assert!(g.admit_entry(1, 0, 0).is_allowed());
}

#[test]
fn negative_control_dead_mans_switch_fires_on_stale_heartbeat() {
    let mut g = armed();
    let timeout = envelope().heartbeat_timeout_ms;
    // Inside the window: still armed.
    assert_eq!(g.state(timeout), ArmState::Armed);
    // Past it: disarmed, and entries refused.
    assert_eq!(
        g.state(timeout + 1),
        ArmState::Disarmed(DisarmReason::HeartbeatLost)
    );
    assert_eq!(
        g.admit_entry(1, 0, timeout + 1),
        Admission::Deny(DenyReason::Disarmed(DisarmReason::HeartbeatLost))
    );
    // Exits keep working while the operator is away - that is the point.
    assert!(g.admit_exit().is_allowed());
}

// ──────────────────────────────── bookkeeping ────────────────────────────────

#[test]
fn heartbeat_keeps_an_armed_gate_alive() {
    let mut g = armed();
    let timeout = envelope().heartbeat_timeout_ms;
    for i in 1..10u64 {
        let t = i * (timeout / 2);
        g.heartbeat(t);
        assert_eq!(g.state(t + timeout), ArmState::Armed);
    }
}

#[test]
fn zero_heartbeat_timeout_disables_the_switch() {
    // A deliberate operator choice, and recorded as one rather than defaulting.
    let mut env = envelope();
    env.heartbeat_timeout_ms = 0;
    let mut g = LiveGate::new();
    g.arm(env, 0).unwrap();
    assert_eq!(g.state(u64::MAX), ArmState::Armed);
}

#[test]
fn fills_move_the_counters_and_closes_return_capital() {
    let mut g = armed();
    g.record_entry_fill(SOL / 10, 0);
    g.record_entry_fill(SOL / 10, 0);
    assert_eq!(g.open_positions(), 2);
    assert_eq!(g.deployed_lamports(), SOL / 5);

    g.record_exit_fill(SOL / 10, 5_000);
    assert_eq!(g.open_positions(), 1);
    assert_eq!(g.deployed_lamports(), SOL / 10);
    assert_eq!(g.day_realized_pnl(), 5_000);
}

#[test]
fn rate_window_counts_fills_not_submissions() {
    // Only record_entry_fill advances the window. A run of failed submissions
    // must not consume the hourly budget.
    let mut g = armed();
    for _ in 0..50 {
        let _ = g.admit_entry(1, 0, 0);
    }
    assert_eq!(g.entries_in_window(0), 0);
    assert!(g.admit_entry(1, 0, 0).is_allowed());
}

#[test]
fn accounting_saturates_rather_than_wrapping() {
    let mut g = armed();
    // Closing more than was opened must not wrap the counters below zero.
    g.record_exit_fill(SOL, 0);
    assert_eq!(g.deployed_lamports(), 0);
    assert_eq!(g.open_positions(), 0);

    // And an absurd PnL must not wrap the daily total.
    let mut g2 = armed();
    g2.record_exit_fill(0, i64::MAX);
    g2.record_exit_fill(0, i64::MAX);
    assert_eq!(g2.day_realized_pnl(), i64::MAX);
}
