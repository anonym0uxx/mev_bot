//! Reflection-enhances-discovery contract (§71) with governance bounds (§56).

use pump_quant_app::config::Config;
use pump_quant_app::reflect::reflect;
use pump_quant_watchlist::candidate::Lane;
use pump_quant_watchlist::lane_performance::LanePerformance;
use pump_quant_watchlist::rank::LaneWeights;

#[test]
fn profitable_lane_gains_weight_unprofitable_loses() {
    let cfg = Config::dev_portable();
    let mut w = LaneWeights::from_defaults();
    let mut perf = LanePerformance::new();
    // Scalp lane makes money; sniper lane bleeds.
    perf.record(Lane::ActiveMarketScalp, 5_000);
    perf.record(Lane::CreationSniper, -5_000);

    let before_scalp = w.get(Lane::ActiveMarketScalp);
    let before_sniper = w.get(Lane::CreationSniper);
    let deltas = reflect(&perf, &mut w, &cfg);

    assert!(w.get(Lane::ActiveMarketScalp) > before_scalp, "winner up");
    assert!(w.get(Lane::CreationSniper) < before_sniper, "loser down");
    // Step is bounded by the envelope.
    assert_eq!(
        w.get(Lane::ActiveMarketScalp) - before_scalp,
        cfg.reflect_weight_step_bp
    );
    assert_eq!(deltas.len(), Lane::COUNT);
}

#[test]
fn zero_net_lane_is_unchanged() {
    let cfg = Config::dev_portable();
    let mut w = LaneWeights::from_defaults();
    let perf = LanePerformance::new(); // all lanes zero
    let before: Vec<u32> = Lane::ALL.iter().map(|&l| w.get(l)).collect();
    reflect(&perf, &mut w, &cfg);
    let after: Vec<u32> = Lane::ALL.iter().map(|&l| w.get(l)).collect();
    assert_eq!(before, after);
}

#[test]
fn weight_never_leaves_the_envelope() {
    let cfg = Config::dev_portable();
    let mut w = LaneWeights::from_defaults();
    let mut perf = LanePerformance::new();
    // Hammer one lane negative many times; it must never fall below the floor.
    for _ in 0..1_000 {
        perf.record(Lane::CreationSniper, -1);
        reflect(&perf, &mut w, &cfg);
        assert!(w.get(Lane::CreationSniper) >= cfg.reflect_weight_floor_bp);
    }
    assert_eq!(w.get(Lane::CreationSniper), cfg.reflect_weight_floor_bp);

    // And hammer another positive; never above the ceiling.
    for _ in 0..1_000 {
        perf.record(Lane::ActiveMarketScalp, 1);
        reflect(&perf, &mut w, &cfg);
        assert!(w.get(Lane::ActiveMarketScalp) <= cfg.reflect_weight_ceiling_bp);
    }
    assert_eq!(
        w.get(Lane::ActiveMarketScalp),
        cfg.reflect_weight_ceiling_bp
    );
}
