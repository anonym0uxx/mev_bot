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

// ── E5: the market-anchored bid ─────────────────────────────────────────────────

#[test]
fn a_missing_market_bids_exactly_what_the_old_law_did() {
    for (floor, cong, urg) in [
        (5_000u64, 0u32, 0u8),
        (5_000, 10_000, 0),
        (5_000, 0, 2),
        (200_000, 5_000, 1),
    ] {
        assert_eq!(
            bid_per_send(floor, None, 7_500, cong, urg),
            compute_tip(floor, cong, urg),
            "no market signal must not change what we pay"
        );
    }
}

#[test]
fn the_anchor_interpolates_between_the_reported_percentiles() {
    let m = TipMarket {
        p50: 10_000,
        p75: 20_000,
        p90: 60_000,
    };
    assert_eq!(observed_anchor(&m, 0), 10_000);
    assert_eq!(observed_anchor(&m, 5_000), 10_000);
    assert_eq!(observed_anchor(&m, 7_500), 20_000);
    assert_eq!(observed_anchor(&m, 10_000), 60_000);
    assert_eq!(observed_anchor(&m, 6_250), 15_000, "halfway p50 -> p75");
    assert_eq!(observed_anchor(&m, 20_000), 60_000, "clamped at p90");
}

#[test]
fn a_hot_market_raises_the_bid_and_a_cold_one_cannot_push_it_below_the_floor() {
    let hot = TipMarket {
        p50: 50_000,
        p75: 80_000,
        p90: 90_000,
    };
    assert_eq!(
        bid_per_send(5_000, Some(&hot), 7_500, 0, 0),
        80_000,
        "p75 of the landed market"
    );

    let cold = TipMarket {
        p50: 1,
        p75: 2,
        p90: 3,
    };
    assert_eq!(
        bid_per_send(5_000, Some(&cold), 7_500, 0, 0),
        5_000,
        "under the venue floor nothing lands, however cheap the market looks"
    );
}

#[test]
fn a_stale_or_absurd_market_read_cannot_blow_the_bid_up() {
    // A corrupt p75/p90 next to a sane median: the read is self-consistency checked
    // against its own p50, so the absurd percentiles cannot become an absurd tip.
    let wild = TipMarket {
        p50: 5_000,
        p75: u64::MAX,
        p90: u64::MAX,
    };
    assert_eq!(
        bid_per_send(5_000, Some(&wild), 10_000, 0, 0),
        5_000 * MAX_ANCHOR_MULTIPLE,
        "capped at 8x the observed median"
    );
    // ...and a hot-but-coherent market IS allowed above that: the cap is not 8x the floor.
    let hot = TipMarket {
        p50: 200_000,
        p75: 400_000,
        p90: 800_000,
    };
    // 9,000 bps is 60% of the way from p75 to p90: 400_000 + 0.6 * 400_000 = 640_000.
    assert_eq!(bid_per_send(5_000, Some(&hot), 9_000, 0, 0), 640_000);
    // A degenerate read (zero median) leaves the floor in force.
    let empty = TipMarket {
        p50: 0,
        p75: 0,
        p90: 0,
    };
    assert_eq!(bid_per_send(5_000, Some(&empty), 7_500, 0, 0), 5_000);
}
