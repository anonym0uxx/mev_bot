//! `SourceOutcomeLedger` leaf tests: per-source realized-net-SOL attribution,
//! keyed on a fully-qualified [`SourceRef`] so a Discord alpha room is graded on
//! the SOL it earned — distinct from an X account that shares the same numeric id
//! (§29.8 / §71 / §74). Pure, deterministic, bounded.

use pump_quant_social::ledger::SourceOutcomeLedger;
use pump_quant_social::types::{SourceKind, SourceRef};

#[test]
fn discord_source_records_and_reconciles_distinctly_from_x() {
    let mut led = SourceOutcomeLedger::with_capacity(8);
    // A Discord alpha room and an X account collide on the numeric id (42) but are
    // DIFFERENT sources — the kind disambiguates them.
    let room = SourceRef::discord(42);
    let x_acct = SourceRef::new(SourceKind::X, 42);
    assert_ne!(room, x_acct);

    // The room reconciles two realized outcomes (a winner then a loss give-back);
    // the X account reconciles one loss.
    led.record(room, 900_000);
    led.record(room, -150_000);
    led.record(x_acct, -300_000);

    // Each source carries exactly its own realized net + count — no crossover.
    assert_eq!(led.net_sol(room), 750_000);
    assert_eq!(led.trade_count(room), 2);
    assert_eq!(led.net_sol(x_acct), -300_000);
    assert_eq!(led.trade_count(x_acct), 1);

    // Two distinct sources tracked; total folds both.
    assert_eq!(led.len(), 2);
    assert_eq!(led.total_net_sol(), 450_000);
}

#[test]
fn untracked_source_reads_zero_never_a_loss() {
    let mut led = SourceOutcomeLedger::with_capacity(4);
    led.record(SourceRef::discord(1), 10);
    let unseen = SourceRef::new(SourceKind::Telegram, 99);
    assert_eq!(led.net_sol(unseen), 0);
    assert_eq!(led.trade_count(unseen), 0);
    assert!(led.get(unseen).is_none());
}

#[test]
fn distinct_discord_rooms_are_independent() {
    let mut led = SourceOutcomeLedger::with_capacity(8);
    let room_a = SourceRef::discord(1001);
    let room_b = SourceRef::discord(1002);
    led.record(room_a, 500_000);
    led.record(room_b, -400_000);
    assert_eq!(led.net_sol(room_a), 500_000);
    assert_eq!(led.net_sol(room_b), -400_000);
}

#[test]
fn ledger_is_capacity_bounded_with_lru_eviction() {
    let mut led = SourceOutcomeLedger::with_capacity(2);
    let a = SourceRef::discord(1);
    let b = SourceRef::discord(2);
    let c = SourceRef::discord(3);
    led.record(a, 100); // seq 0
    led.record(b, 200); // seq 1
                        // Touch `a` so `b` becomes the least-recently-updated source.
    led.record(a, 50); // seq 2  -> a total 150
    led.record(c, 300); // seq 3  -> evicts b (smallest update_seq)
    assert_eq!(led.len(), 2, "never exceeds capacity");
    assert_eq!(led.net_sol(a), 150);
    assert_eq!(led.net_sol(c), 300);
    assert_eq!(led.net_sol(b), 0, "least-recently-updated source evicted");
}

#[test]
fn net_sol_saturates_instead_of_wrapping() {
    let mut led = SourceOutcomeLedger::with_capacity(2);
    let room = SourceRef::discord(7);
    led.record(room, i64::MAX);
    led.record(room, i64::MAX);
    assert_eq!(
        led.net_sol(room),
        i64::MAX,
        "clamps, never wraps to negative"
    );
}
