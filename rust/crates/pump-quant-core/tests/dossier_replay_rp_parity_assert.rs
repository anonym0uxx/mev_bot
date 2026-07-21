#![allow(unused_imports)]
use pump_quant_core::replay::*;
#[test]
fn prop_parity_first_divergence() {
    let evs: Vec<_> = (0..50).map(|i| CanonEvent::test(i, 0, 0, i)).collect();
    let mut st = WorldState::new();
    let mut cps = vec![];
    for (i, e) in evs.iter().enumerate() {
        st = apply_world(&st, e);
        if i % 10 == 9 { cps.push((i, state_hash(&st))); }
    }
    assert!(matches!(replay_assert(&evs, &cps), ReplayVerdict::Match));
    let mut bad = cps.clone();
    bad[2].1[0] ^= 0xFF;
    match replay_assert(&evs, &bad) {
        ReplayVerdict::Diverged { at_event, .. } => assert_eq!(at_event, bad[2].0),
        _ => panic!("must diverge at the third checkpoint"),
    }
}
