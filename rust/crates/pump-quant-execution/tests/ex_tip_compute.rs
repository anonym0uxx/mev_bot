#![allow(unused_imports)]
use pump_quant_execution::ex_tip_compute::*;

fn reference(base: u64, congestion_bps: u32, urgency: u8) -> u64 {
    let cf = 10_000u128 + congestion_bps as u128;
    let uf = 10_000u128 + urgency as u128 * 5_000u128;
    let mut t = base as u128;
    t = t * cf / 10_000;
    t = t * uf / 10_000;
    if t > u64::MAX as u128 {
        u64::MAX
    } else {
        t as u64
    }
}

#[test]
fn zero_congestion_zero_urgency_returns_base() {
    assert_eq!(compute_tip(10_000, 0, 0), 10_000);
    assert_eq!(compute_tip(10_000, 0, 0), reference(10_000, 0, 0));
}

#[test]
fn congestion_scales_linearly() {
    // base 10_000, congestion 5000 bps (=+50%), urgency 0 -> 15_000
    assert_eq!(compute_tip(10_000, 5_000, 0), 15_000);
    assert_eq!(compute_tip(10_000, 5_000, 0), reference(10_000, 5_000, 0));
}

#[test]
fn urgency_adds_fifty_percent_per_level() {
    // base 10_000, congestion 0, urgency 2 -> factor 1 + 2*0.5 = 2.0 -> 20_000
    assert_eq!(compute_tip(10_000, 0, 2), 20_000);
    assert_eq!(compute_tip(10_000, 0, 2), reference(10_000, 0, 2));
}

#[test]
fn combined_factors_compose() {
    // base 8_000, congestion 2500 (=1.25), urgency 1 (=1.5)
    // 8000 * 12500/10000 = 10_000 ; 10_000 * 15000/10000 = 15_000
    assert_eq!(compute_tip(8_000, 2_500, 1), 15_000);
    assert_eq!(compute_tip(8_000, 2_500, 1), reference(8_000, 2_500, 1));
}

#[test]
fn tip_never_below_base() {
    for base in [0u64, 1, 500, 10_000, 1_000_000] {
        for cong in [0u32, 100, 9_999, 50_000] {
            for urg in [0u8, 1, 4] {
                let got = compute_tip(base, cong, urg);
                assert!(got >= base, "base={base} cong={cong} urg={urg} got={got}");
                assert_eq!(got, reference(base, cong, urg));
            }
        }
    }
}

#[test]
fn saturates_instead_of_overflowing() {
    // Huge base with large multipliers saturates to u64::MAX.
    let got = compute_tip(u64::MAX, 50_000, 4);
    assert_eq!(got, u64::MAX);
    assert_eq!(got, reference(u64::MAX, 50_000, 4));
}
