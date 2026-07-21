//! Leaf ec_cost: phase-asymmetric executable exit-cost model (criterion 101).

use pump_quant_strategy::exit_cost_model::{
    curve_exit_cost_bps, executable_exit_cost_bps, pool_exit_cost_bps, CurveExitInputs,
    PhaseModelError, PoolExitInputs,
};
use pump_quant_strategy::scalp_position::Phase;

fn curve(reserve: u64, size: u64, drift: u32, fail: u32, retry: u32) -> CurveExitInputs {
    CurveExitInputs {
        curve_reserve_lamports: reserve,
        size_lamports: size,
        latency_drift_bps: drift,
        failure_rate_bps: fail,
        retry_adder_bps: retry,
    }
}
fn pool(base: u64, quote: u64, size: u64, drift: u32) -> PoolExitInputs {
    PoolExitInputs {
        base_reserve_lamports: base,
        quote_reserve_lamports: quote,
        size_lamports: size,
        latency_drift_bps: drift,
    }
}

#[test]
fn curve_cost_matches_independent_formula() {
    // schedule = 1000*10000/100000 = 100; base = 100+50+20 = 170.
    // fail 2500: inflated = 170*10000/7500 = 226.
    assert_eq!(
        curve_exit_cost_bps(&curve(100_000, 1_000, 50, 2_500, 20)),
        Ok(226)
    );
    // fail 0: inflated = base = 170.
    assert_eq!(
        curve_exit_cost_bps(&curve(100_000, 1_000, 50, 0, 20)),
        Ok(170)
    );
}

#[test]
fn curve_certain_failure_rejected() {
    assert_eq!(
        curve_exit_cost_bps(&curve(100_000, 1_000, 50, 10_000, 20)),
        Err(PhaseModelError::CertainFailure)
    );
    assert_eq!(
        curve_exit_cost_bps(&curve(100_000, 1_000, 50, 12_000, 20)),
        Err(PhaseModelError::CertainFailure)
    );
}

#[test]
fn higher_failure_rate_raises_curve_cost() {
    let lo = curve_exit_cost_bps(&curve(100_000, 1_000, 50, 1_000, 20)).unwrap();
    let hi = curve_exit_cost_bps(&curve(100_000, 1_000, 50, 5_000, 20)).unwrap();
    assert!(hi > lo, "more failures should cost more: {hi} vs {lo}");
}

#[test]
fn pool_cost_matches_independent_formula() {
    // impact = 1000*10000/(9000+1000) = 1000; + drift 50 = 1050.
    assert_eq!(pool_exit_cost_bps(&pool(9_000, 0, 1_000, 50)), 1_050);
    // impact = 1000*10000/(1000+1000) = 5000; + 0.
    assert_eq!(pool_exit_cost_bps(&pool(1_000, 0, 1_000, 0)), 5_000);
}

#[test]
fn larger_size_raises_pool_impact() {
    let small = pool_exit_cost_bps(&pool(100_000, 0, 1_000, 0));
    let large = pool_exit_cost_bps(&pool(100_000, 0, 50_000, 0));
    assert!(large > small);
}

#[test]
fn dispatch_curve_phase_ok() {
    let c = curve(100_000, 1_000, 50, 0, 20);
    assert_eq!(
        executable_exit_cost_bps(Phase::Curve, Some(&c), None),
        Ok(170)
    );
}

#[test]
fn dispatch_pool_phase_ok() {
    let p = pool(9_000, 0, 1_000, 50);
    assert_eq!(
        executable_exit_cost_bps(Phase::Pool, None, Some(&p)),
        Ok(1_050)
    );
}

#[test]
fn curve_model_forbidden_in_pool_phase() {
    let c = curve(100_000, 1_000, 50, 0, 20);
    let p = pool(9_000, 0, 1_000, 50);
    // Curve model supplied in pool phase => rejected.
    assert_eq!(
        executable_exit_cost_bps(Phase::Pool, Some(&c), Some(&p)),
        Err(PhaseModelError::CurveModelMisapplied)
    );
}

#[test]
fn pool_model_forbidden_in_curve_phase() {
    let c = curve(100_000, 1_000, 50, 0, 20);
    let p = pool(9_000, 0, 1_000, 50);
    assert_eq!(
        executable_exit_cost_bps(Phase::Curve, Some(&c), Some(&p)),
        Err(PhaseModelError::PoolModelMisapplied)
    );
}

#[test]
fn missing_model_rejected() {
    assert_eq!(
        executable_exit_cost_bps(Phase::Curve, None, None),
        Err(PhaseModelError::CurveModelMisapplied)
    );
    assert_eq!(
        executable_exit_cost_bps(Phase::Pool, None, None),
        Err(PhaseModelError::PoolModelMisapplied)
    );
}
