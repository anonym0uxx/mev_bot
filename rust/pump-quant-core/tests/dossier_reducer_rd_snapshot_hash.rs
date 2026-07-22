// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'reducer' component (leaf 'rd_snapshot_hash').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_hash_order_stable() {
    let mut w1 = WorldState::new();
    let mut w2 = WorldState::new();
    for i in 0..100u64 { w1.upsert_market(i, MarketState::test_with(i)); }
    for i in (0..100u64).rev() { w2.upsert_market(i, MarketState::test_with(i)); }
    assert_eq!(state_hash(&w1), state_hash(&w2)); // insertion order irrelevant
    assert_eq!(state_hash(&w1), state_hash(&w1)); // stable within process
}
