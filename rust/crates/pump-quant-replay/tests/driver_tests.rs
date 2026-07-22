//! Integration tests for the §19 replay driver: execution modes, step
//! granularities, break conditions, and resume-from-checkpoint byte-equivalence.

use pump_quant_clock::EventKey;
use pump_quant_replay::{
    BreakCondition, BreakSet, EventKind, ReplayDriver, ReplayEvent, ReplayMode, StopReason,
};

/// Build one event: ts doubles as monotonic; source 0.
fn ev(ts: u64, seq: u64, wall: u64, slot: u64, kind: EventKind) -> ReplayEvent {
    ReplayEvent::new(EventKey::new(ts, 0, seq), wall, slot, kind)
}

/// The canonical 6-event fixture used across most tests.
///
/// slots:   10  10   11   11   12    12
/// kinds:   Obs Mint Obs  Dec  Entry Exit
fn fixture() -> Vec<ReplayEvent> {
    vec![
        ev(100, 0, 1000, 10, EventKind::Observation),
        ev(200, 1, 1100, 10, EventKind::Mint),
        ev(300, 2, 1200, 11, EventKind::Observation),
        ev(400, 3, 1300, 11, EventKind::Decision),
        ev(500, 4, 1400, 12, EventKind::Entry),
        ev(600, 5, 1500, 12, EventKind::Exit),
    ]
}

#[test]
fn tie_break_sort_orders_out_of_order_input() {
    // Feed reversed; driver must sort into (ts, source, seq) order.
    let mut events = fixture();
    events.reverse();
    let mut d = ReplayDriver::new(events, ReplayMode::StepByObservation, BreakSet::none());
    let mut seen = Vec::new();
    while !d.is_exhausted() {
        let s = d.step();
        for e in s.emitted {
            seen.push(e.monotonic_ns());
        }
    }
    assert_eq!(seen, vec![100, 200, 300, 400, 500, 600]);
}

#[test]
fn step_by_observation_emits_one_at_a_time() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepByObservation, BreakSet::none());
    for expected_ts in [100, 200, 300, 400, 500, 600] {
        let s = d.step();
        assert_eq!(s.emitted.len(), 1);
        assert_eq!(s.emitted[0].monotonic_ns(), expected_ts);
        assert_eq!(s.pacing_delay_ns, 0, "step modes never pace");
    }
    assert!(d.is_exhausted());
    let last = d.step();
    assert!(last.emitted.is_empty());
    assert!(last.exhausted);
}

#[test]
fn step_by_canonical_event_stops_at_next_canonical() {
    let mut d = ReplayDriver::new(
        fixture(),
        ReplayMode::StepByCanonicalEvent,
        BreakSet::none(),
    );
    // Obs(100) then Mint(200) -> one canonical unit.
    let s1 = d.step();
    assert_eq!(
        s1.emitted
            .iter()
            .map(ReplayEvent::monotonic_ns)
            .collect::<Vec<_>>(),
        vec![100, 200]
    );
    // Obs(300) then Decision(400).
    let s2 = d.step();
    assert_eq!(
        s2.emitted
            .iter()
            .map(ReplayEvent::monotonic_ns)
            .collect::<Vec<_>>(),
        vec![300, 400]
    );
    // Entry(500) is itself canonical -> single event.
    let s3 = d.step();
    assert_eq!(
        s3.emitted
            .iter()
            .map(ReplayEvent::monotonic_ns)
            .collect::<Vec<_>>(),
        vec![500]
    );
    // Exit(600).
    let s4 = d.step();
    assert_eq!(
        s4.emitted
            .iter()
            .map(ReplayEvent::monotonic_ns)
            .collect::<Vec<_>>(),
        vec![600]
    );
    assert!(d.is_exhausted());
}

#[test]
fn step_by_slot_emits_one_slot_run_per_step() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepBySlot, BreakSet::none());
    let s1 = d.step();
    assert_eq!(
        s1.emitted.iter().map(|e| e.slot).collect::<Vec<_>>(),
        vec![10, 10]
    );
    let s2 = d.step();
    assert_eq!(
        s2.emitted.iter().map(|e| e.slot).collect::<Vec<_>>(),
        vec![11, 11]
    );
    let s3 = d.step();
    assert_eq!(
        s3.emitted.iter().map(|e| e.slot).collect::<Vec<_>>(),
        vec![12, 12]
    );
    assert!(d.is_exhausted());
}

#[test]
fn break_on_mint_stops_after_the_mint() {
    let breaks = BreakSet::none().with(BreakCondition::OnMint);
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepByObservation, breaks);
    let r = d.run_to_break();
    assert_eq!(r.emitted, 2, "Observation(100) then Mint(200)");
    assert_eq!(r.stop, StopReason::Break(BreakCondition::OnMint));
    assert_eq!(d.position(), 2);
}

#[test]
fn break_conditions_fire_for_each_kind() {
    for (bc, expected_emitted) in [
        (BreakCondition::OnMint, 2),
        (BreakCondition::OnDecision, 4),
        (BreakCondition::OnEntry, 5),
        (BreakCondition::OnExit, 6),
    ] {
        let mut d = ReplayDriver::new(
            fixture(),
            ReplayMode::MaximumSpeed,
            BreakSet::none().with(bc),
        );
        let r = d.run_to_break();
        assert_eq!(r.stop, StopReason::Break(bc));
        assert_eq!(r.emitted, expected_emitted, "for {bc:?}");
    }
}

#[test]
fn break_mid_slot_step_interrupts_the_unit() {
    // StepBySlot would emit slot 10's [Obs, Mint] together, but break-on-mint
    // must interrupt the unit right after the mint.
    let breaks = BreakSet::none().with(BreakCondition::OnMint);
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepBySlot, breaks);
    let s = d.step();
    assert_eq!(s.emitted.len(), 2);
    assert_eq!(s.break_hit, Some(BreakCondition::OnMint));
    assert_eq!(d.position(), 2);
}

#[test]
fn run_to_break_with_no_breaks_runs_to_exhaustion() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::MaximumSpeed, BreakSet::none());
    let r = d.run_to_break();
    assert_eq!(r.stop, StopReason::Exhausted);
    assert_eq!(r.emitted, 6);
    assert!(d.is_exhausted());
}

#[test]
fn maximum_speed_never_paces() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::MaximumSpeed, BreakSet::none());
    let r = d.run_to_break();
    assert_eq!(r.pacing_delay_ns, 0);
}

#[test]
fn real_time_paces_by_monotonic_gaps() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::RealTime, BreakSet::none());
    let r = d.run_to_break();
    // First event pays 0; five gaps of 100 each.
    assert_eq!(r.pacing_delay_ns, 500);
}

#[test]
fn scaled_time_double_speed_halves_delay() {
    let mode = ReplayMode::ScaledTime {
        speed_num: 2,
        speed_den: 1,
    };
    let mut d = ReplayDriver::new(fixture(), mode, BreakSet::none());
    let r = d.run_to_break();
    assert_eq!(r.pacing_delay_ns, 250); // 500 / 2
}

#[test]
fn scaled_time_half_speed_doubles_delay() {
    let mode = ReplayMode::ScaledTime {
        speed_num: 1,
        speed_den: 2,
    };
    let mut d = ReplayDriver::new(fixture(), mode, BreakSet::none());
    let r = d.run_to_break();
    assert_eq!(r.pacing_delay_ns, 1000); // 500 * 2
}

#[test]
fn scaled_time_zero_numerator_falls_back_to_real_time() {
    let mode = ReplayMode::ScaledTime {
        speed_num: 0,
        speed_den: 5,
    };
    let mut d = ReplayDriver::new(fixture(), mode, BreakSet::none());
    let r = d.run_to_break();
    assert_eq!(r.pacing_delay_ns, 500);
}

#[test]
fn clock_tracks_last_emitted_event_in_lockstep() {
    use pump_quant_clock::Clock;
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepByObservation, BreakSet::none());
    let _ = d.step(); // emit e0
    assert_eq!(d.clock().monotonic_ns(), 100);
    assert_eq!(d.clock().current_slot(), 10);
    let _ = d.step(); // emit e1
    let _ = d.step(); // emit e2
    assert_eq!(d.clock().monotonic_ns(), 300);
    assert_eq!(d.clock().current_slot(), 11);
}

#[test]
fn reset_reproduces_the_identical_run() {
    let mut d = ReplayDriver::new(fixture(), ReplayMode::MaximumSpeed, BreakSet::none());
    let first = d.run_to_break();
    d.reset();
    assert_eq!(d.position(), 0);
    assert_eq!(d.emitted(), 0);
    let second = d.run_to_break();
    assert_eq!(first.state_hash, second.state_hash);
    assert_eq!(first.emitted, second.emitted);
}

#[test]
fn resume_from_checkpoint_is_byte_equivalent() {
    // Reference: a full run from the top.
    let mut reference =
        ReplayDriver::new(fixture(), ReplayMode::StepByObservation, BreakSet::none());
    let full = reference.run_to_break();

    // Driver two: step three times, checkpoint, capture the tail, then resume
    // the checkpoint and run the tail again — both tails and the final hash
    // must match, and match the reference.
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepByObservation, BreakSet::none());
    for _ in 0..3 {
        let _ = d.step();
    }
    let cp = d.checkpoint();
    assert_eq!(cp.cursor(), 3);
    assert_eq!(cp.emitted(), 3);

    let tail_a = d.run_to_break();
    assert_eq!(tail_a.state_hash, full.state_hash);

    // Rewind to the checkpoint and replay the tail.
    d.resume(cp);
    assert_eq!(d.position(), 3);
    assert_eq!(d.emitted(), 3);
    assert_eq!(d.state_hash(), cp.state_hash());
    let tail_b = d.run_to_break();

    assert_eq!(tail_b.state_hash, tail_a.state_hash);
    assert_eq!(tail_b.emitted, tail_a.emitted);
    assert_eq!(tail_b.emitted, 3, "events 3,4,5 replayed");
}

#[test]
fn resume_re_syncs_the_clock() {
    use pump_quant_clock::Clock;
    let mut d = ReplayDriver::new(fixture(), ReplayMode::StepByObservation, BreakSet::none());
    // Capture a checkpoint at the very top before emitting anything.
    let top = d.checkpoint();
    // Advance to the end.
    let _ = d.run_to_break();
    assert_eq!(d.clock().monotonic_ns(), 600);
    // Resume at the very top.
    d.resume(top);
    assert_eq!(d.position(), 0);
    // Clock re-seated to first reading.
    assert_eq!(d.clock().monotonic_ns(), 100);
    assert_eq!(d.clock().current_slot(), 10);
}

#[test]
fn different_kinds_produce_different_hashes() {
    let a = fixture();
    let mut b = fixture();
    b[1] = ev(200, 1, 1100, 10, EventKind::Canonical); // was Mint
    let mut da = ReplayDriver::new(a, ReplayMode::MaximumSpeed, BreakSet::none());
    let mut db = ReplayDriver::new(b, ReplayMode::MaximumSpeed, BreakSet::none());
    let ra = da.run_to_break();
    let rb = db.run_to_break();
    assert_ne!(ra.state_hash, rb.state_hash);
}

#[test]
#[should_panic(expected = "non-empty")]
fn empty_sequence_panics() {
    let _ = ReplayDriver::new(Vec::new(), ReplayMode::MaximumSpeed, BreakSet::none());
}

#[test]
fn break_set_bitpacking() {
    let mut s = BreakSet::none();
    assert!(s.is_empty());
    s.insert(BreakCondition::OnEntry);
    assert!(s.contains(BreakCondition::OnEntry));
    assert!(!s.contains(BreakCondition::OnExit));
    let s2 = s.with(BreakCondition::OnExit);
    assert!(s2.contains(BreakCondition::OnEntry));
    assert!(s2.contains(BreakCondition::OnExit));
}
