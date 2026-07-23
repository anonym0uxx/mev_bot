//! §47a per-mint reflection-cadence API — integration coverage.
use pump_quant_evaluator::reflection_cadence::{
    reflect_mints, MintId, MintSwaps, ReflectionCadence,
};

#[test]
fn versioned_criterion_pins_the_label() {
    let c1 = ReflectionCadence::new(1_000, 1);
    let c2 = ReflectionCadence::new(50, 2);
    let alive = c1.reflect(MintId(1), &[0, 100], 200);
    let dead = c2.reflect(MintId(1), &[0, 100], 200);
    assert!(!alive.is_dead());
    assert!(dead.is_dead());
    assert_eq!(alive.criterion_version, 1);
    assert_eq!(dead.criterion_version, 2);
}

#[test]
fn batch_sorted_by_mint_and_deterministic() {
    let cadence = ReflectionCadence::new(1_000, 5);
    let mints = vec![
        MintSwaps {
            mint: MintId(9),
            swap_ts_ns: vec![0],
            window_end_ns: 5_000,
        },
        MintSwaps {
            mint: MintId(2),
            swap_ts_ns: vec![0, 10, 20],
            window_end_ns: 30,
        },
    ];
    let out = reflect_mints(cadence, &mints);
    assert_eq!(out[0].mint, MintId(2));
    assert_eq!(out[1].mint, MintId(9));
    assert_eq!(out, reflect_mints(cadence, &mints));
}

#[test]
fn empty_fleet_is_empty() {
    let cadence = ReflectionCadence::new(1_000, 1);
    assert!(reflect_mints(cadence, &[]).is_empty());
}
