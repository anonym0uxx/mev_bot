#![allow(unused_imports)]
use pump_quant_execution::ex_route_policy::*;

fn base_ctx() -> RouteCtx {
    RouteCtx {
        entry_edge_bps: 500,
        trade_size_lamports: 1_000_000_000, // 1 SOL
        opportunity_half_life_ms: 10_000,
        is_sell: false,
        is_forced_exit: false,
        current_slippage_bps: 200,
        promotion_enabled: true,
        half_life_threshold_ms: 5_000,
        min_edge_for_nozomi_bps: 300,
        min_edge_for_jito_bps: 400,
        exit_slippage_trigger_bps: 1_000,
        nozomi_healthy: true,
        jito_healthy: true,
        rpc_latency_ms: 800,
        nozomi_latency_ms: 300,
        jito_latency_ms: 600,
        rpc_fail_bps: 500,
        nozomi_fail_bps: 300,
        jito_fail_bps: 200,
        nozomi_tip_lamports: 50_000,
        jito_tip_lamports: 100_000,
    }
}

// Independent EV reference (mirrors the documented integer formula).
fn ev(mode: Route, c: &RouteCtx) -> i128 {
    let size = c.trade_size_lamports as i128;
    let gross = c.entry_edge_bps as i128 * size / 10_000;
    let (fee, lat, fail) = match mode {
        Route::Rpc => (0i128, c.rpc_latency_ms, c.rpc_fail_bps),
        Route::Nozomi => (
            c.nozomi_tip_lamports as i128,
            c.nozomi_latency_ms,
            c.nozomi_fail_bps,
        ),
        Route::JitoBundle => (
            c.jito_tip_lamports as i128,
            c.jito_latency_ms,
            c.jito_fail_bps,
        ),
    };
    let slip = if matches!(mode, Route::JitoBundle) {
        (c.current_slippage_bps as i128 * size / 10_000) / 2
    } else {
        0
    };
    let latency_decay = lat as i128 * size / 1_000_000;
    let failure_cost = fail as i128 * size / (10_000 * 20);
    gross - fee + slip - latency_decay - failure_cost
}

#[test]
fn ev_matches_reference() {
    let c = base_ctx();
    assert_eq!(route_ev_lamports(Route::Rpc, &c), ev(Route::Rpc, &c));
    assert_eq!(route_ev_lamports(Route::Nozomi, &c), ev(Route::Nozomi, &c));
    assert_eq!(
        route_ev_lamports(Route::JitoBundle, &c),
        ev(Route::JitoBundle, &c)
    );
}

#[test]
fn promotion_disabled_forces_rpc() {
    let mut c = base_ctx();
    c.promotion_enabled = false;
    assert_eq!(choose_route(c), Route::Rpc);
}

#[test]
fn forced_exit_high_slippage_uses_jito() {
    let mut c = base_ctx();
    c.is_forced_exit = true;
    c.current_slippage_bps = 1_500; // > trigger 1000
    assert_eq!(choose_route(c), Route::JitoBundle);
}

#[test]
fn forced_exit_prefers_faster_nozomi() {
    let mut c = base_ctx();
    c.is_forced_exit = true;
    c.current_slippage_bps = 100; // below trigger
    c.nozomi_latency_ms = 300;
    c.rpc_latency_ms = 800; // 300*10=3000 < 800*7=5600 -> nozomi
    assert_eq!(choose_route(c), Route::Nozomi);
}

#[test]
fn forced_exit_falls_back_to_rpc_when_nozomi_not_faster() {
    let mut c = base_ctx();
    c.is_forced_exit = true;
    c.current_slippage_bps = 100;
    c.jito_healthy = false;
    c.nozomi_latency_ms = 700;
    c.rpc_latency_ms = 800; // 7000 < 5600 false -> rpc
    assert_eq!(choose_route(c), Route::Rpc);
}

#[test]
fn nozomi_not_promoted_when_half_life_above_threshold() {
    let mut c = base_ctx();
    c.opportunity_half_life_ms = 9_000; // >= threshold 5000 -> nozomi ineligible
                                        // Ensure jito also not clearly better: make jito unhealthy so RPC wins.
    c.jito_healthy = false;
    // Manually: only RPC candidate -> RPC.
    assert_eq!(choose_route(c), Route::Rpc);
}

#[test]
fn best_ev_route_is_selected() {
    // Construct a case where Nozomi is eligible and has the strictly highest EV.
    let mut c = base_ctx();
    c.opportunity_half_life_ms = 1_000; // < threshold -> nozomi eligible
    c.entry_edge_bps = 1_000; // > nozomi min 300 and > jito min 400
    c.jito_healthy = false; // drop jito from contention
    c.nozomi_tip_lamports = 0; // make nozomi cheap
    c.nozomi_latency_ms = 0;
    c.nozomi_fail_bps = 0;
    // Reference: compute both eligible EVs and expect the argmax with RPC tie-break.
    let rpc = ev(Route::Rpc, &c);
    let noz = ev(Route::Nozomi, &c);
    let expected = if noz > rpc { Route::Nozomi } else { Route::Rpc };
    assert_eq!(choose_route(c), expected);
    assert!(noz > rpc); // sanity: nozomi should win here
}

#[test]
fn sell_makes_jito_eligible_even_below_edge_min() {
    let mut c = base_ctx();
    c.is_sell = true;
    c.entry_edge_bps = 100; // below jito min 400, but sell qualifies
    c.opportunity_half_life_ms = 9_000; // nozomi ineligible
                                        // Give jito a strong EV edge: cheap tip, low latency/fail, big slippage adj.
    c.jito_tip_lamports = 0;
    c.jito_latency_ms = 0;
    c.jito_fail_bps = 0;
    c.current_slippage_bps = 5_000;
    let rpc = ev(Route::Rpc, &c);
    let jito = ev(Route::JitoBundle, &c);
    let expected = if jito > rpc {
        Route::JitoBundle
    } else {
        Route::Rpc
    };
    assert_eq!(choose_route(c), expected);
    assert!(jito > rpc);
}

#[test]
fn tie_resolves_to_rpc() {
    // Make every route EV identical to RPC by zeroing differentiators and
    // matching RPC's latency/fail so no candidate strictly exceeds RPC.
    let mut c = base_ctx();
    c.opportunity_half_life_ms = 1_000;
    c.entry_edge_bps = 1_000;
    c.current_slippage_bps = 0; // no jito slippage advantage
    c.nozomi_tip_lamports = 0;
    c.jito_tip_lamports = 0;
    c.nozomi_latency_ms = c.rpc_latency_ms;
    c.jito_latency_ms = c.rpc_latency_ms;
    c.nozomi_fail_bps = c.rpc_fail_bps;
    c.jito_fail_bps = c.rpc_fail_bps;
    assert_eq!(ev(Route::Rpc, &c), ev(Route::Nozomi, &c));
    assert_eq!(ev(Route::Rpc, &c), ev(Route::JitoBundle, &c));
    assert_eq!(choose_route(c), Route::Rpc);
}
