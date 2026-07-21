// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'scalp_position' component (leaf 'sp_state_apply').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.

#[test]
fn prop_peak_never_freezes() {
    let mut s = ScalpPositionState::open(1_000, 0);
    for i in 0..10_000u64 {  // far beyond any internal buffer
        let ev = SwapEvent::test(1_000 + i, i * 1_000);
        s = apply_swap(&s, &ev);
    }
    assert_eq!(s.peak_price_fp, 1_000 + 9_999);
    let down = SwapEvent::test(500, 10_000_000);
    s = apply_swap(&s, &down);
    assert_eq!(s.peak_price_fp, 1_000 + 9_999); // monotone
    assert_eq!(s.last_price_fp, 500);
}
