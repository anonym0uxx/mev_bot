// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'driver' component (leaf 'break_set_with_arms_only_target').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_replay::driver::*;

#[test]
fn break_set_with_arms_only_target() {
    let all = [
        BreakCondition::OnMint,
        BreakCondition::OnDecision,
        BreakCondition::OnEntry,
        BreakCondition::OnExit,
    ];
    for &c in &all {
        let s = BreakSet::none().with(c);
        // Exactly the target is armed.
        assert!(s.contains(c));
        assert!(!s.is_empty());
        // No other condition is armed.
        let mut armed = 0u32;
        for &other in &all {
            if s.contains(other) {
                armed += 1;
                assert_eq!(other, c);
            }
        }
        assert_eq!(armed, 1);
        // Arming the same condition twice is idempotent.
        assert_eq!(s.with(c), s);
    }
    // Concrete cross-check: mint armed, exit not.
    let m = BreakSet::none().with(BreakCondition::OnMint);
    assert!(m.contains(BreakCondition::OnMint));
    assert!(!m.contains(BreakCondition::OnExit));
}
