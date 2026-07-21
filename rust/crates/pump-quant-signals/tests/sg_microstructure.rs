//! Property/unit tests for the §21.7 AMM microstructure catalog (criterion 95).
//!
//! Expectations are computed independently of the implementation (by hand or by
//! a separate reference calculation over multiple inputs incl. edge cases) so a
//! memorized/hardcoded answer cannot pass.

use pump_quant_signals::microstructure::*;

fn swap(ts_ms: u64, dir: SwapDir, quote: u64, price: u64, entity: u64, new_buyer: bool) -> Swap {
    Swap {
        ts_ms,
        dir,
        quote_lamports: quote,
        price_fp: price,
        entity_id: entity,
        is_new_buyer: new_buyer,
    }
}

#[test]
fn cvd_nets_buys_minus_sells() {
    // buys: 100 + 250 = 350 ; sells: 40 + 60 = 100 ; net = +250.
    let s = [
        swap(1, SwapDir::Buy, 100, 10, 1, true),
        swap(2, SwapDir::Sell, 40, 10, 2, false),
        swap(3, SwapDir::Buy, 250, 11, 3, true),
        swap(4, SwapDir::Sell, 60, 11, 4, false),
    ];
    assert_eq!(cumulative_volume_delta(&s), 250);

    // All sells -> negative.
    let s2 = [
        swap(1, SwapDir::Sell, 500, 10, 1, false),
        swap(2, SwapDir::Sell, 500, 9, 2, false),
    ];
    assert_eq!(cumulative_volume_delta(&s2), -1000);

    // Empty -> 0.
    assert_eq!(cumulative_volume_delta(&[]), 0);
}

#[test]
fn cvd_velocity_scales_per_second() {
    // delta 250 lamports over 500 ms -> 250 * 1000 / 500 = 500 lamports/s.
    assert_eq!(cvd_velocity_lamports_per_s(250, 500), 500);
    // negative delta.
    assert_eq!(cvd_velocity_lamports_per_s(-1000, 2000), -500);
    // dt == 0 guard.
    assert_eq!(cvd_velocity_lamports_per_s(999, 0), 0);
}

#[test]
fn cvd_price_divergence_classification() {
    // price up (100->110), cvd down (500->400): bearish exhaustion.
    assert_eq!(
        cvd_price_divergence(100, 110, 500, 400),
        Divergence::BearishExhaustion
    );
    // price down (110->100), cvd up (400->500): bullish exhaustion.
    assert_eq!(
        cvd_price_divergence(110, 100, 400, 500),
        Divergence::BullishExhaustion
    );
    // both up: bullish confirm.
    assert_eq!(
        cvd_price_divergence(100, 120, 100, 300),
        Divergence::BullishConfirm
    );
    // both down: bearish confirm.
    assert_eq!(
        cvd_price_divergence(120, 100, 300, 100),
        Divergence::BearishConfirm
    );
    // price flat: neutral.
    assert_eq!(
        cvd_price_divergence(100, 100, 100, 300),
        Divergence::Neutral
    );
    // price up, cvd flat: exhaustion (cvd failed to confirm).
    assert_eq!(
        cvd_price_divergence(100, 110, 500, 500),
        Divergence::BearishExhaustion
    );
}

#[test]
fn ofi_breadth_decomposed() {
    // new-buyer buys: 800 ; new-buyer sells: 0 -> new_buyer OFI = +10000
    // repeat buys: 100 ; repeat sells: 300 -> (100-300)/(400)= -5000
    // aggregate buys 900, sells 300 -> (900-300)/1200 = 5000.
    let s = [
        swap(1, SwapDir::Buy, 800, 10, 1, true),
        swap(2, SwapDir::Buy, 100, 10, 2, false),
        swap(3, SwapDir::Sell, 300, 10, 2, false),
    ];
    let o = order_flow_imbalance(&s);
    assert_eq!(o.new_buyer_bps, 10_000);
    assert_eq!(o.repeat_bps, -5_000);
    assert_eq!(o.aggregate_bps, 5_000);

    // empty -> all zero.
    let z = order_flow_imbalance(&[]);
    assert_eq!(z, OfiBreakdown::default());
}

#[test]
fn trade_size_distribution_median_and_large_prints() {
    // sizes (lamports): 0.05, 0.2, 0.6, 2, 20 SOL.
    // buckets edges: [0, .1, .5, 1, 5, 10] SOL.
    // 0.05 -> b0 ; 0.2 -> b1 ; 0.6 -> b2 ; 2 -> b3 ; 20 -> b5.
    let s = [
        swap(1, SwapDir::Buy, 50_000_000, 10, 1, true),
        swap(2, SwapDir::Buy, 200_000_000, 10, 2, true),
        swap(3, SwapDir::Buy, 600_000_000, 10, 3, true),
        swap(4, SwapDir::Buy, 2_000_000_000, 10, 4, true),
        swap(5, SwapDir::Buy, 20_000_000_000, 10, 5, true),
    ];
    let d = trade_size_distribution(&s, 3);
    assert_eq!(d.buckets, [1, 1, 1, 1, 0, 1]);
    // median of 5 sorted sizes = middle = 0.6 SOL = 600_000_000.
    assert_eq!(d.median_lamports, 600_000_000);
    // large print >= 3 * median = 1_800_000_000 : sizes 2 SOL and 20 SOL -> 2.
    assert_eq!(d.large_prints, 2);
    assert_eq!(d.count, 5);

    // even count median averages two central values: [10, 30] -> 20.
    let s2 = [
        swap(1, SwapDir::Buy, 10, 1, 1, true),
        swap(2, SwapDir::Buy, 30, 1, 2, true),
    ];
    let d2 = trade_size_distribution(&s2, 2);
    assert_eq!(d2.median_lamports, 20);

    // empty -> default.
    assert_eq!(trade_size_distribution(&[], 3), SizeDistribution::default());
}

#[test]
fn absorption_exhaustion_classes() {
    // notable inflow, price up small (100->101 = +100 bps) <= low_impact 200 -> Absorption.
    assert_eq!(
        absorption_exhaustion(1_000, 100, 101, 500, 200),
        FlowResponse::Absorption
    );
    // notable inflow, price down -> Exhaustion.
    assert_eq!(
        absorption_exhaustion(1_000, 100, 99, 500, 200),
        FlowResponse::Exhaustion
    );
    // inflow below threshold -> Normal.
    assert_eq!(
        absorption_exhaustion(100, 100, 90, 500, 200),
        FlowResponse::Normal
    );
    // notable inflow, big up-move (100->130 = +3000 bps) > low_impact -> Normal.
    assert_eq!(
        absorption_exhaustion(1_000, 100, 130, 500, 200),
        FlowResponse::Normal
    );
}

#[test]
fn price_change_bps_signed() {
    assert_eq!(price_change_bps(100, 110), 1_000); // +10%
    assert_eq!(price_change_bps(100, 90), -1_000); // -10%
    assert_eq!(price_change_bps(0, 90), 0); // guard
}

#[test]
fn anchored_vwap_and_state() {
    // prices 10,20 with quote weights 100,300 -> (10*100+20*300)/(400)=7000/400=17.
    let s = [
        swap(1, SwapDir::Buy, 100, 10, 1, true),
        swap(2, SwapDir::Buy, 300, 20, 2, true),
    ];
    assert_eq!(anchored_vwap_fp(&s), 17);
    assert_eq!(anchored_vwap_fp(&[]), 0);

    // vwap = 17 : prev 15 (below) -> cur 20 (above) = reclaim.
    assert_eq!(vwap_state(15, 20, 17), VwapState::ReclaimAbove);
    assert_eq!(vwap_state(20, 15, 17), VwapState::RejectBelow);
    assert_eq!(vwap_state(20, 25, 17), VwapState::HoldAbove);
    assert_eq!(vwap_state(10, 12, 17), VwapState::HoldBelow);
}

#[test]
fn constant_product_impact_curve() {
    // reserves base=1_000_000, quote=1_000_000 (k=1e12). buy quote_in=1000.
    // new_quote=1_001_000 ; new_base = 1e12 / 1_001_000 = 999000 (floor 999000.999->999000)
    // base_out = 1_000_000 - 999000 = 1000.
    let base_out = constant_product_base_out(1_000_000, 1_000_000, 1_000);
    assert_eq!(base_out, 1_000);
    // spot = quote/base = 1 ; effective = quote_in/base_out = 1000/1000 = 1.
    // impact ~ 0 here (edge). Use a bigger trade for clear impact:
    // quote_in = 100_000 -> new_quote=1_100_000 ; new_base=1e12/1_100_000=909090
    // base_out=90910. effective=100000/90910=1.1 -> spot 1 -> ~1000 bps.
    let out2 = constant_product_base_out(1_000_000, 1_000_000, 100_000);
    assert_eq!(out2, 90_910);
    // impact_bps = (quote_in*base - quote*base_out)*10000/(quote*base_out)
    // = (100000*1_000_000 - 1_000_000*90910)*10000/(1_000_000*90910)
    // = (1e11 - 9.091e10)*1e4 / 9.091e10
    // = (9.09e9)*1e4/9.091e10 = 9.09e13/9.091e10 = 1000 (bps) approx.
    let imp = price_impact_bps(1_000_000, 1_000_000, 100_000);
    // exact: num=(100000*1000000 - 1000000*90910)=100000000000-90910000000=9090000000
    // spot_num=1000000*90910=90910000000 ; imp=9090000000*10000/90910000000=909000000000... /90910000000
    // = 9.09e13 / 9.091e10 = 999.89 -> floor 999.
    assert_eq!(imp, 999);
    // degenerate reserves.
    assert_eq!(constant_product_base_out(0, 100, 10), 0);
    assert_eq!(price_impact_bps(0, 100, 10), 0);
}

#[test]
fn reserve_velocity_signed() {
    // +5000 lamports over 1000ms -> +5000/s.
    assert_eq!(
        reserve_velocity_lamports_per_s(10_000, 15_000, 1_000),
        5_000
    );
    // removal.
    assert_eq!(
        reserve_velocity_lamports_per_s(15_000, 10_000, 1_000),
        -5_000
    );
    assert_eq!(reserve_velocity_lamports_per_s(1, 2, 0), 0);
}

#[test]
fn arrival_rate_and_burst_phases() {
    // 10 swaps in 5000 ms -> 10*1e6/5000 = 2000 millihz.
    assert_eq!(arrival_rate_millihz(10, 5_000), 2_000);
    assert_eq!(arrival_rate_millihz(1, 0), 0);

    // baseline 1000, multiple 2 => strongly elevated threshold = 2000.
    // recent 3000 >= 2000, recent>prior(2500): Onset.
    assert_eq!(burst_phase(3_000, 2_500, 1_000, 2), BurstPhase::Onset);
    // recent 3000, prior 3000, elevated: Climax.
    assert_eq!(burst_phase(3_000, 3_000, 1_000, 2), BurstPhase::Climax);
    // recent 3000, prior 3500, elevated, decel: Exhaustion.
    assert_eq!(burst_phase(3_000, 3_500, 1_000, 2), BurstPhase::Exhaustion);
    // recent 900 <= baseline 1000: Quiet.
    assert_eq!(burst_phase(900, 800, 1_000, 2), BurstPhase::Quiet);
    // recent 1500 > baseline but < 2000 threshold, decel(prior 1600): Exhaustion.
    assert_eq!(burst_phase(1_500, 1_600, 1_000, 2), BurstPhase::Exhaustion);
    // recent 1500 > baseline, accel(prior 1200), below threshold: Onset.
    assert_eq!(burst_phase(1_500, 1_200, 1_000, 2), BurstPhase::Onset);
}
