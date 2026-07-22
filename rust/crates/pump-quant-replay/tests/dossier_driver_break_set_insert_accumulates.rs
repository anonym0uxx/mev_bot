// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'driver' component (leaf 'break_set_insert_accumulates').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_replay::driver::*;

#[test]
fn break_set_insert_accumulates() {
    let all = [
        BreakCondition::OnMint,
        BreakCondition::OnDecision,
        BreakCondition::OnEntry,
        BreakCondition::OnExit,
    ];
    // insert matches the const `with` builder for a single condition.
    for &c in &all {
        let mut s = BreakSet::none();
        s.insert(c);
        assert_eq!(s, BreakSet::none().with(c));
    }
    // Inserting accumulates without clobbering earlier conditions.
    let mut s = BreakSet::none();
    let mut count = 0u32;
    for &c in &all {
        s.insert(c);
        count += 1;
        let mut armed = 0u32;
        for &prev in &all {
            if s.contains(prev) {
                armed += 1;
            }
        }
        assert_eq!(armed, count);
    }
    // All four armed; equals the fully-armed `with` chain.
    let full = BreakSet::none()
        .with(BreakCondition::OnMint)
        .with(BreakCondition::OnDecision)
        .with(BreakCondition::OnEntry)
        .with(BreakCondition::OnExit);
    assert_eq!(s, full);
    // Re-inserting an already-armed condition is a no-op.
    let before = s;
    s.insert(BreakCondition::OnMint);
    assert_eq!(s, before);
}
