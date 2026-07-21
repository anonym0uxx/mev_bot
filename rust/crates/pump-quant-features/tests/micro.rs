//! Leaf tests for the AMM microstructure catalog (constitution 21.7), with
//! independently-computed expectations and a cross-check property test for the
//! streaming rolling window against from-scratch recomputation.

use pump_quant_features::micro::{
    anchored_vwap_fp, arrival_intensity, classify_divergence, cumulative_volume_delta,
    cvd_velocity_per_sec, large_print_count, order_flow_imbalance_base, order_flow_imbalance_bps,
    to_price_fp, trade_size_histogram, CvdDivergence, RollingFlowWindow,
};
use pump_quant_features::types::{FeatureError, Side, TradeEvent};

/// Deterministic test-only LCG (no RNG in library logic).
struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn in_range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo)
    }
}

fn t(id: u64, ts: u64, price: i128, base: u64, quote: u64, side: Side) -> TradeEvent {
    TradeEvent {
        event_id: id,
        ts_ns: ts,
        price_fp: to_price_fp(price),
        base_qty: base,
        quote_qty: quote,
        side,
    }
}

fn sample() -> Vec<TradeEvent> {
    vec![
        t(1, 1, 10, 10, 100, Side::Buy),
        t(2, 2, 11, 4, 44, Side::Sell),
        t(3, 3, 10, 6, 60, Side::Buy),
    ]
}

#[test]
fn cvd_and_ofi() {
    let s = sample();
    // CVD = +100 - 44 + 60 = 116.
    assert_eq!(cumulative_volume_delta(&s), 116);
    // OFI base = +10 - 4 + 6 = 12.
    assert_eq!(order_flow_imbalance_base(&s), 12);
    // buy base 16, sell base 4, net 12, total 20 -> 6000 bps.
    assert_eq!(order_flow_imbalance_bps(&s), Some(6000));
    // Empty and zero-volume cases are undefined -> None.
    assert_eq!(order_flow_imbalance_bps(&[]), None);
}

#[test]
fn cvd_velocity() {
    // delta 500 over 2 seconds -> 250 quote/sec.
    assert_eq!(cvd_velocity_per_sec(0, 0, 500, 2_000_000_000), Some(250));
    // Non-positive time base -> None.
    assert_eq!(cvd_velocity_per_sec(0, 5, 10, 5), None);
    assert_eq!(cvd_velocity_per_sec(0, 6, 10, 5), None);
    // Negative delta over 1 second.
    assert_eq!(
        cvd_velocity_per_sec(100, 0, -900, 1_000_000_000),
        Some(-1000)
    );
}

#[test]
fn vwap_and_anchored() {
    let s = sample();
    // VWAP num = 10*10 + 11*4 + 10*6 = 204 (in price units), den 20 -> 10.2.
    assert_eq!(vwap_expected(&s), 10_200_000_000);
    // Anchor at ts>=2 keeps trades 2 and 3: (11*4 + 10*6)/10 = 10.4.
    assert_eq!(anchored_vwap_fp(&s, 2), Some(10_400_000_000));
    // Anchor past all trades -> None.
    assert_eq!(anchored_vwap_fp(&s, 100), None);
}

fn vwap_expected(s: &[TradeEvent]) -> i128 {
    pump_quant_features::micro::vwap_fp(s).unwrap()
}

#[test]
fn large_prints_and_histogram() {
    let s = sample();
    // base sizes {10,4,6}; threshold 6 -> {10,6} qualify.
    assert_eq!(large_print_count(&s, 6), 2);
    assert_eq!(large_print_count(&s, 11), 0);
    // edges [5,10]: bucket0 <5 -> {4}; bucket1 [5,10) -> {6}; bucket2 >=10 -> {10}.
    assert_eq!(trade_size_histogram(&s, &[5, 10]), vec![1, 1, 1]);
    // Histogram counts always sum to trade count.
    let h = trade_size_histogram(&s, &[5, 10]);
    assert_eq!(h.iter().sum::<u32>(), s.len() as u32);
}

#[test]
fn arrival_intensity_window() {
    let s = sample(); // ts 1,2,3
                      // now=3 window=2 -> (1,3] -> ts 2,3 -> 2.
    assert_eq!(arrival_intensity(&s, 3, 2), 2);
    // now=3 window=3 -> (0,3] -> all 3.
    assert_eq!(arrival_intensity(&s, 3, 3), 3);
    // window 0 -> empty.
    assert_eq!(arrival_intensity(&s, 3, 0), 0);
}

#[test]
fn divergence_classification() {
    assert_eq!(classify_divergence(5, 5), CvdDivergence::Confirmation);
    assert_eq!(classify_divergence(5, 0), CvdDivergence::Bearish);
    assert_eq!(classify_divergence(5, -3), CvdDivergence::Bearish);
    assert_eq!(classify_divergence(-5, -5), CvdDivergence::Confirmation);
    assert_eq!(classify_divergence(-5, 0), CvdDivergence::Bullish);
    assert_eq!(classify_divergence(-5, 2), CvdDivergence::Bullish);
    assert_eq!(classify_divergence(0, 9), CvdDivergence::Neutral);
    assert_eq!(classify_divergence(0, 0), CvdDivergence::Neutral);
}

#[test]
fn rolling_window_time_eviction() {
    let mut w = RollingFlowWindow::new(100, 10).unwrap();
    w.push(t(1, 0, 10, 10, 100, Side::Buy)).unwrap();
    w.push(t(2, 50, 11, 4, 44, Side::Sell)).unwrap();
    // Pushing at ts=100 evicts everything with ts <= 100-100 = 0, i.e. trade 1.
    w.push(t(3, 100, 10, 6, 60, Side::Buy)).unwrap();
    assert_eq!(w.len(), 2);
    // CVD over {sell 44, buy 60} = -44 + 60 = 16.
    assert_eq!(w.cvd(), 16);
    assert_eq!(w.ofi_base(), 2);
    assert_eq!(w.buy_base(), 6);
    assert_eq!(w.sell_base(), 4);
    assert_eq!(w.ofi_bps(), Some(2000));
}

#[test]
fn rolling_window_capacity_eviction() {
    // Hard capacity 2: a third trade in-window evicts the oldest.
    let mut w = RollingFlowWindow::new(1_000_000, 2).unwrap();
    w.push(t(1, 1, 10, 10, 100, Side::Buy)).unwrap();
    w.push(t(2, 2, 10, 5, 50, Side::Sell)).unwrap();
    w.push(t(3, 3, 10, 7, 70, Side::Buy)).unwrap();
    assert_eq!(w.len(), 2);
    // Retained {sell 50, buy 70} -> cvd = 20.
    assert_eq!(w.cvd(), 20);
}

#[test]
fn rolling_window_rejects_non_monotonic() {
    let mut w = RollingFlowWindow::new(100, 10).unwrap();
    w.push(t(1, 10, 10, 1, 10, Side::Buy)).unwrap();
    let err = w.push(t(2, 9, 10, 1, 10, Side::Buy)).unwrap_err();
    assert_eq!(
        err,
        FeatureError::NonMonotonicTimestamp {
            previous_ns: 10,
            offending_ns: 9,
        }
    );
}

#[test]
fn invalid_window_config() {
    assert_eq!(
        RollingFlowWindow::new(0, 10).unwrap_err(),
        FeatureError::InvalidConfiguration
    );
    assert_eq!(
        RollingFlowWindow::new(100, 0).unwrap_err(),
        FeatureError::InvalidConfiguration
    );
}

/// PROPERTY: the streaming window's incremental aggregates always equal a
/// from-scratch recomputation over its currently-retained trades, across many
/// generated ascending-timestamp streams and window/capacity settings.
#[test]
fn property_window_aggregates_match_recompute() {
    for seed in 0..300u64 {
        let mut rng = Lcg(seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(7));
        let window_ns = rng.in_range(1, 500);
        let capacity = rng.in_range(1, 20) as usize;
        let mut w = RollingFlowWindow::new(window_ns, capacity).unwrap();

        let n = rng.in_range(1, 60);
        let mut ts = 0u64;
        for i in 0..n {
            ts += rng.in_range(0, 60); // non-decreasing timestamps
            let base = rng.in_range(1, 1000);
            let quote = rng.in_range(1, 100_000);
            let side = if rng.next_u64().is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            w.push(t(i, ts, 10, base, quote, side)).unwrap();

            // Independent recompute over retained trades.
            let retained: Vec<TradeEvent> = w.trades().copied().collect();
            assert!(retained.len() <= capacity, "capacity bound violated");
            let exp_cvd = cumulative_volume_delta(&retained);
            let exp_ofi = order_flow_imbalance_base(&retained);
            let exp_bps = order_flow_imbalance_bps(&retained);
            assert_eq!(w.cvd(), exp_cvd, "seed {seed} step {i}: cvd");
            assert_eq!(w.ofi_base(), exp_ofi, "seed {seed} step {i}: ofi");
            assert_eq!(w.ofi_bps(), exp_bps, "seed {seed} step {i}: bps");

            // Time-window invariant: every retained trade is within window of newest.
            let newest = ts;
            let lo = newest.saturating_sub(window_ns);
            for r in &retained {
                assert!(r.ts_ns > lo, "seed {seed} step {i}: stale trade retained");
            }
        }
    }
}
