//! §99 bound: StreamState.gaps is capped; open gaps are never evicted so gap
//! detection stays correct under heavy reconnect churn.
#![allow(clippy::bool_comparison)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn gaps_are_capacity_bounded() {
    let cap = 4;
    let mut s = StreamState::with_gap_capacity(cap);
    assert_eq!(s.gap_capacity(), cap);
    // Many disconnect/reconnect cycles (each closes its gap, opens a new epoch).
    for i in 0..50u64 {
        on_disconnect(&mut s, i * 10);
        s.reconnect(i * 10 + 5);
    }
    assert!(s.gaps.len() <= cap, "gaps never exceed the cap");
}

#[test]
fn open_gap_survives_eviction_and_stays_detectable() {
    let cap = 3;
    let mut s = StreamState::with_gap_capacity(cap);
    // Open then immediately close several gaps to fill the buffer with closed ones.
    for i in 0..cap as u64 {
        on_disconnect(&mut s, i * 100);
        s.reconnect(i * 100 + 10);
    }
    // Now open one final gap and leave it open; it must not be evicted by the next
    // over-cap push, and the seqs inside it must still read as gapped.
    on_disconnect(&mut s, 10_000);
    assert!(
        s.is_gapped(10_050),
        "the open gap is retained and detectable"
    );
    // Force more overflow with closed gaps; the open gap still survives.
    for i in 0..cap as u64 {
        on_disconnect(&mut s, 20_000 + i); // opens a new gap...
        s.reconnect(20_000 + i + 1); // ...and closes it
    }
    assert!(
        s.is_gapped(10_050),
        "open gap never evicted even under continued churn"
    );
}
