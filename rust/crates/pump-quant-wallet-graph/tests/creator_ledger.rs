//! §27/§29.9 survived-migration creator ledger: happy path, small-n refusal,
//! point-in-time safety, boundary/monotonicity, and bounded-capacity churn.

use pump_quant_wallet_graph::creator_ledger::{
    classify_track, CreatorLedger, CreatorLedgerConfig, CreatorTrack, CreatorTrackSummary,
    LedgerWrite, CREATOR_MIN_SURVIVED_FOR_PROVEN, CREATOR_SURVIVAL_HORIZON_SLOTS,
};
use pump_quant_wallet_graph::{TokenId, WalletId};

/// A compact fixture: 1 000-slot survival horizon, two survivors for proven,
/// three launches in a 10 000-slot window for serial.
fn cfg() -> CreatorLedgerConfig {
    CreatorLedgerConfig {
        survival_horizon_slots: 1_000,
        min_survived_for_proven: 2,
        serial_window_slots: 10_000,
        serial_min_launches: 3,
        min_rugs_for_toxic: 1,
        max_creators: 8,
        max_launches_per_creator: 8,
    }
}

const C: WalletId = WalletId(7);

/// Launch `token` at `slot` and migrate it at `slot + 1`.
fn launch_and_migrate(l: &mut CreatorLedger, token: u64, slot: u64) {
    assert_eq!(
        l.record_launch(C, TokenId(token), slot),
        LedgerWrite::Recorded
    );
    assert_eq!(
        l.record_migration(C, TokenId(token), slot + 1),
        LedgerWrite::Recorded
    );
}

// ---------------------------------------------------------------------------
// Happy path — Proven becomes reachable
// ---------------------------------------------------------------------------

#[test]
fn two_survived_migrations_make_a_creator_proven() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    launch_and_migrate(&mut l, 2, 100);

    // Both migrations cleared the 1 000-slot horizon by slot 1 101.
    let s = l.summary_as_of(C, 1_101).expect("tracked creator");
    assert_eq!(s.launches, 2);
    assert_eq!(s.migrated, 2);
    assert_eq!(s.survived, 2);
    assert_eq!(s.rugged, 0);
    assert!(!s.truncated);
    assert_eq!(l.classify_as_of(C, 1_101), CreatorTrack::Proven);
}

#[test]
fn a_rug_beats_a_survival_record() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    launch_and_migrate(&mut l, 2, 100);
    assert_eq!(l.classify_as_of(C, 1_101), CreatorTrack::Proven);

    // A third launch rugs. The risk read dominates from that slot onward.
    assert_eq!(l.record_launch(C, TokenId(3), 1_200), LedgerWrite::Recorded);
    assert_eq!(l.record_rug(C, TokenId(3), 1_300), LedgerWrite::Recorded);
    assert_eq!(l.classify_as_of(C, 1_300), CreatorTrack::Toxic);
}

#[test]
fn serial_burst_outranks_proven_but_not_toxic() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    launch_and_migrate(&mut l, 2, 100);
    // Third launch inside the 10 000-slot window trips the serial gate.
    assert_eq!(l.record_launch(C, TokenId(3), 2_000), LedgerWrite::Recorded);
    assert_eq!(l.classify_as_of(C, 2_000), CreatorTrack::Serial);

    // Add a rug: toxic outranks serial.
    assert_eq!(l.record_rug(C, TokenId(3), 2_100), LedgerWrite::Recorded);
    assert_eq!(l.classify_as_of(C, 2_100), CreatorTrack::Toxic);
}

#[test]
fn track_ordinals_are_stable_and_round_trip() {
    for (t, o) in [
        (CreatorTrack::Unknown, 0u8),
        (CreatorTrack::Proven, 1),
        (CreatorTrack::Toxic, 2),
        (CreatorTrack::Serial, 3),
    ] {
        assert_eq!(t.ordinal(), o);
        assert_eq!(CreatorTrack::from_ordinal(o), Some(t));
    }
    assert_eq!(CreatorTrack::from_ordinal(4), None);
}

// ---------------------------------------------------------------------------
// Small-n / fail-closed refusal
// ---------------------------------------------------------------------------

#[test]
fn unknown_creator_is_unknown_not_proven() {
    let l = CreatorLedger::new(cfg());
    assert!(l.summary_as_of(WalletId(999), u64::MAX).is_none());
    assert_eq!(
        l.classify_as_of(WalletId(999), u64::MAX),
        CreatorTrack::Unknown
    );
}

#[test]
fn one_survivor_is_below_the_proven_gate() {
    assert_eq!(CREATOR_MIN_SURVIVED_FOR_PROVEN, 2);
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    let s = l.summary_as_of(C, 10_000).expect("tracked");
    assert_eq!(s.survived, 1);
    assert_eq!(
        l.classify_as_of(C, 10_000),
        CreatorTrack::Unknown,
        "one survivor is luck, not a track record"
    );
}

#[test]
fn launches_that_never_migrated_are_not_survivors() {
    let mut l = CreatorLedger::new(cfg());
    for t in 1..=2u64 {
        assert_eq!(l.record_launch(C, TokenId(t), t), LedgerWrite::Recorded);
    }
    let s = l.summary_as_of(C, 1_000_000).expect("tracked");
    assert_eq!(s.launches, 2);
    assert_eq!(s.migrated, 0);
    assert_eq!(s.survived, 0);
    assert_eq!(l.classify_as_of(C, 1_000_000), CreatorTrack::Unknown);
}

#[test]
fn truncated_history_refuses_the_optimistic_label() {
    // A tiny per-creator ring: the fourth launch evicts the first, so a rug in
    // the evicted past could exist. Proven is withheld; Unknown is the answer.
    let mut c = cfg();
    c.max_launches_per_creator = 3;
    c.serial_min_launches = 100; // keep the serial gate out of the way
    let mut l = CreatorLedger::new(c);
    for t in 1..=4u64 {
        launch_and_migrate(&mut l, t, t * 10);
    }
    let s = l.summary_as_of(C, 100_000).expect("tracked");
    assert!(s.truncated, "history lost a launch to the bound");
    assert!(s.survived >= 2, "survivors alone would qualify");
    assert_eq!(
        l.classify_as_of(C, 100_000),
        CreatorTrack::Unknown,
        "incomplete evidence must not yield the optimistic label"
    );
}

#[test]
fn invalid_config_never_yields_proven() {
    let bad = CreatorLedgerConfig {
        min_survived_for_proven: 0,
        ..cfg()
    };
    assert!(!bad.is_valid());
    let s = CreatorTrackSummary {
        launches: 5,
        launches_in_window: 0,
        migrated: 5,
        survived: 5,
        rugged: 0,
        truncated: false,
    };
    assert_eq!(classify_track(&s, &bad), CreatorTrack::Unknown);
    assert_eq!(classify_track(&s, &cfg()), CreatorTrack::Proven);
}

#[test]
fn facts_about_launches_the_ledger_does_not_hold_are_rejected() {
    let mut l = CreatorLedger::new(cfg());
    assert_eq!(
        l.record_migration(C, TokenId(1), 5),
        LedgerWrite::UnknownLaunch
    );
    assert_eq!(l.record_rug(C, TokenId(1), 5), LedgerWrite::UnknownLaunch);
    assert!(l.summary_as_of(C, 5).is_none(), "nothing was invented");
}

// ---------------------------------------------------------------------------
// §20 point-in-time safety
// ---------------------------------------------------------------------------

#[test]
fn a_later_rug_cannot_make_a_creator_toxic_earlier() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    launch_and_migrate(&mut l, 2, 100);
    let before = l.classify_as_of(C, 1_101);
    assert_eq!(before, CreatorTrack::Proven);

    // Token 2 rugs much later.
    assert_eq!(l.record_rug(C, TokenId(2), 5_000), LedgerWrite::Recorded);

    assert_eq!(
        l.classify_as_of(C, 1_101),
        before,
        "a rug at slot 5000 leaked into the slot-1101 verdict"
    );
    assert_eq!(l.summary_as_of(C, 1_101).expect("tracked").rugged, 0);
    // And at the later slot it does count.
    assert_eq!(l.classify_as_of(C, 5_000), CreatorTrack::Toxic);
}

#[test]
fn a_later_migration_cannot_make_a_creator_proven_earlier() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0);
    assert_eq!(l.record_launch(C, TokenId(2), 100), LedgerWrite::Recorded);
    // At slot 1 101 only one launch has survived.
    assert_eq!(l.classify_as_of(C, 1_101), CreatorTrack::Unknown);

    // The second launch migrates later.
    assert_eq!(
        l.record_migration(C, TokenId(2), 4_000),
        LedgerWrite::Recorded
    );
    assert_eq!(
        l.classify_as_of(C, 1_101),
        CreatorTrack::Unknown,
        "a migration at slot 4000 leaked into the slot-1101 verdict"
    );
    // It only counts once its own horizon has elapsed.
    assert_eq!(l.classify_as_of(C, 4_999), CreatorTrack::Unknown);
    assert_eq!(l.classify_as_of(C, 5_000), CreatorTrack::Proven);
}

#[test]
fn survival_horizon_boundary_is_inclusive_and_not_early() {
    let mut l = CreatorLedger::new(cfg());
    launch_and_migrate(&mut l, 1, 0); // migrated at slot 1
    let rec = l
        .launches(C)
        .expect("tracked")
        .first()
        .copied()
        .expect("one");
    assert_eq!(rec.migrated_slot, Some(1));
    assert!(!rec.survived_as_of(1_000, 1_000), "one slot short");
    assert!(rec.survived_as_of(1_001, 1_000), "exactly at the horizon");
    assert!(rec.survived_as_of(1_002, 1_000));
}

#[test]
fn a_launch_is_never_counted_before_its_own_slot() {
    let mut l = CreatorLedger::new(cfg());
    assert_eq!(l.record_launch(C, TokenId(1), 500), LedgerWrite::Recorded);
    assert_eq!(l.summary_as_of(C, 499).expect("tracked").launches, 0);
    assert_eq!(l.summary_as_of(C, 500).expect("tracked").launches, 1);
}

#[test]
fn terminal_facts_cannot_move_backwards_or_be_restated() {
    let mut l = CreatorLedger::new(cfg());
    assert_eq!(l.record_launch(C, TokenId(1), 100), LedgerWrite::Recorded);
    // Before the launch: refused.
    assert_eq!(l.record_migration(C, TokenId(1), 99), LedgerWrite::Refused);
    assert_eq!(l.record_rug(C, TokenId(1), 99), LedgerWrite::Refused);
    // First observation counts; a restatement cannot move the survival clock.
    assert_eq!(
        l.record_migration(C, TokenId(1), 200),
        LedgerWrite::Recorded
    );
    assert_eq!(l.record_migration(C, TokenId(1), 150), LedgerWrite::Refused);
    assert_eq!(
        l.launches(C)
            .expect("tracked")
            .first()
            .expect("one")
            .migrated_slot,
        Some(200)
    );
    // Duplicate launch of the same token is refused, not double-counted.
    assert_eq!(l.record_launch(C, TokenId(1), 300), LedgerWrite::Refused);
    assert_eq!(l.summary_as_of(C, 10_000).expect("tracked").launches, 1);
}

// ---------------------------------------------------------------------------
// Boundary / monotonicity
// ---------------------------------------------------------------------------

#[test]
fn proven_gate_is_at_the_threshold_not_above_it() {
    let base = CreatorTrackSummary {
        launches: 9,
        launches_in_window: 0,
        migrated: 9,
        survived: 1,
        rugged: 0,
        truncated: false,
    };
    let c = cfg();
    assert_eq!(classify_track(&base, &c), CreatorTrack::Unknown);
    let at = CreatorTrackSummary {
        survived: 2,
        ..base
    };
    assert_eq!(classify_track(&at, &c), CreatorTrack::Proven);
    let above = CreatorTrackSummary {
        survived: 3,
        ..base
    };
    assert_eq!(classify_track(&above, &c), CreatorTrack::Proven);
}

#[test]
fn serial_window_is_a_lookback_not_a_lifetime_count() {
    let mut l = CreatorLedger::new(cfg()); // 10 000-slot window, 3 launches
    for t in 1..=3u64 {
        assert_eq!(
            l.record_launch(C, TokenId(t), t * 1_000),
            LedgerWrite::Recorded
        );
    }
    assert_eq!(l.classify_as_of(C, 3_000), CreatorTrack::Serial);
    // Far in the future the same three launches have aged out of the window.
    let s = l.summary_as_of(C, 100_000).expect("tracked");
    assert_eq!(s.launches, 3);
    assert_eq!(s.launches_in_window, 0);
    assert_eq!(l.classify_as_of(C, 100_000), CreatorTrack::Unknown);
}

#[test]
fn survivor_count_is_monotone_non_decreasing_in_the_query_slot() {
    let mut l = CreatorLedger::new(cfg());
    for t in 1..=4u64 {
        launch_and_migrate(&mut l, t, t * 200);
    }
    let mut prev = 0u32;
    for slot in (0..8_000).step_by(250) {
        let s = l.summary_as_of(C, slot).expect("tracked");
        assert!(
            s.survived >= prev,
            "survived went backwards at slot {slot}: {} < {prev}",
            s.survived
        );
        prev = s.survived;
    }
    assert_eq!(prev, 4);
}

#[test]
fn default_config_horizon_is_the_documented_named_const() {
    let l = CreatorLedger::with_defaults();
    assert_eq!(
        l.config().survival_horizon_slots,
        CREATOR_SURVIVAL_HORIZON_SLOTS
    );
    assert!(l.config().is_valid());
    // Saturated slots must not panic or wrap.
    let mut l = CreatorLedger::with_defaults();
    assert_eq!(
        l.record_launch(WalletId(1), TokenId(1), u64::MAX),
        LedgerWrite::Recorded
    );
    assert_eq!(
        l.record_migration(WalletId(1), TokenId(1), u64::MAX),
        LedgerWrite::Recorded
    );
    // migrated_slot + horizon overflows the slot axis => never survives.
    assert_eq!(
        l.classify_as_of(WalletId(1), u64::MAX),
        CreatorTrack::Unknown
    );
}

// ---------------------------------------------------------------------------
// Bounded capacity / churn
// ---------------------------------------------------------------------------

#[test]
fn creator_capacity_is_bounded_and_evicts_least_recently_active() {
    let mut c = cfg();
    c.max_creators = 2;
    let mut l = CreatorLedger::new(c);
    l.record_launch(WalletId(1), TokenId(10), 100);
    l.record_launch(WalletId(2), TokenId(20), 500);
    assert_eq!(l.len(), 2);

    // A third creator evicts WalletId(1) (oldest last_slot).
    l.record_launch(WalletId(3), TokenId(30), 600);
    assert_eq!(l.len(), 2);
    assert_eq!(l.creator_evictions(), 1);
    assert!(l.launches(WalletId(1)).is_none());
    assert!(l.launches(WalletId(2)).is_some());
    assert!(l.launches(WalletId(3)).is_some());
    // An evicted creator reads Unknown, not Proven.
    assert_eq!(
        l.classify_as_of(WalletId(1), u64::MAX),
        CreatorTrack::Unknown
    );
}

#[test]
fn churn_never_exceeds_either_bound() {
    let mut c = cfg();
    c.max_creators = 4;
    c.max_launches_per_creator = 4;
    let mut l = CreatorLedger::new(c);
    for i in 0..400u64 {
        l.record_launch(WalletId(i % 16), TokenId(i), i * 10);
        assert!(l.len() <= 4, "creator bound breached at {i}");
        for w in 0..16u64 {
            if let Some(rec) = l.launches(WalletId(w)) {
                assert!(rec.len() <= 4, "launch bound breached for creator {w}");
            }
        }
    }
    assert_eq!(l.len(), 4);
    assert!(l.creator_evictions() > 0);
}

#[test]
fn per_creator_history_evicts_oldest_and_flags_truncation() {
    let mut c = cfg();
    c.max_launches_per_creator = 3;
    let mut l = CreatorLedger::new(c);
    for t in 1..=5u64 {
        assert_eq!(
            l.record_launch(C, TokenId(t), t * 100),
            LedgerWrite::Recorded
        );
    }
    let recs = l.launches(C).expect("tracked");
    assert_eq!(recs.len(), 3);
    assert_eq!(
        recs.first().expect("first").token,
        TokenId(3),
        "oldest evicted"
    );
    assert_eq!(l.dropped_launches(C), 2);
    assert!(l.summary_as_of(C, u64::MAX).expect("tracked").truncated);
}

#[test]
fn creators_are_independent() {
    let mut l = CreatorLedger::new(cfg());
    // Creator A: two survivors. Creator B: one rug.
    for t in 1..=2u64 {
        assert_eq!(
            l.record_launch(WalletId(1), TokenId(t), 0),
            LedgerWrite::Recorded
        );
        assert_eq!(
            l.record_migration(WalletId(1), TokenId(t), 1),
            LedgerWrite::Recorded
        );
    }
    assert_eq!(
        l.record_launch(WalletId(2), TokenId(9), 0),
        LedgerWrite::Recorded
    );
    assert_eq!(
        l.record_rug(WalletId(2), TokenId(9), 5),
        LedgerWrite::Recorded
    );

    assert_eq!(l.classify_as_of(WalletId(1), 2_000), CreatorTrack::Proven);
    assert_eq!(l.classify_as_of(WalletId(2), 2_000), CreatorTrack::Toxic);
    // A fact recorded against the wrong creator is not silently accepted.
    assert_eq!(
        l.record_rug(WalletId(1), TokenId(9), 10),
        LedgerWrite::UnknownLaunch
    );
    assert_eq!(l.classify_as_of(WalletId(1), 2_000), CreatorTrack::Proven);
}
