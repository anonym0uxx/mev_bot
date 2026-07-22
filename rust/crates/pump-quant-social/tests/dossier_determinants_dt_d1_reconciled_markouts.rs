// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'determinants' component (leaf 'dt_d1_reconciled_markouts').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_social::determinants::*;

#[test]
fn dt_d1_markouts_prop() {
    const HL: u64 = 1_000_000_000;
    // Single call doubled at every horizon -> +10_000, n=1, conf=10000*1/21=476.
    let one = [MarkoutSample {
        price_at_call: 100,
        price_5m: 200,
        price_30m: 200,
        price_2h: 200,
        price_24h: 200,
        age_ns: 0,
    }];
    let s = d1_reconciled_markouts(&one, [1, 1, 1, 1], HL);
    assert_eq!(s.value_bps, 10_000);
    assert_eq!(s.sample_size, 1);
    assert_eq!(s.confidence_bps, 476);

    // +100% and -50% equal age -> mean(10000,-5000)=2500, n=2, conf=10000*2/22=909.
    let two = [
        MarkoutSample {
            price_at_call: 100,
            price_5m: 200,
            price_30m: 200,
            price_2h: 200,
            price_24h: 200,
            age_ns: 0,
        },
        MarkoutSample {
            price_at_call: 100,
            price_5m: 50,
            price_30m: 50,
            price_2h: 50,
            price_24h: 50,
            age_ns: 0,
        },
    ];
    let s2 = d1_reconciled_markouts(&two, [1, 1, 1, 1], HL);
    assert_eq!(s2.value_bps, 2_500);
    assert_eq!(s2.sample_size, 2);
    assert_eq!(s2.confidence_bps, 909);

    // Decay: fresh +100% weighs 10000, stale -50% (one half-life) weighs 5000.
    // (10000*10000 + (-5000)*5000)/15000 = 5000.
    let dec = [
        MarkoutSample {
            price_at_call: 100,
            price_5m: 200,
            price_30m: 200,
            price_2h: 200,
            price_24h: 200,
            age_ns: 0,
        },
        MarkoutSample {
            price_at_call: 100,
            price_5m: 50,
            price_30m: 50,
            price_2h: 50,
            price_24h: 50,
            age_ns: HL,
        },
    ];
    assert_eq!(
        d1_reconciled_markouts(&dec, [1, 1, 1, 1], HL).value_bps,
        5_000
    );

    // Rejection/edge: empty input -> empty score.
    let e = d1_reconciled_markouts(&[], [1, 1, 1, 1], HL);
    assert_eq!(e.value_bps, 0);
    assert_eq!(e.sample_size, 0);
    assert_eq!(e.confidence_bps, 0);
}
