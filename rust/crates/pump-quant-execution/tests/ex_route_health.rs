//! Tests for `ex_route_health`.
//!
//! The controls that matter here assert that an UNDER-SAMPLED route reports
//! nothing rather than reporting health. A tracker that answers "0% failures"
//! after three observations would satisfy the EV model and be wrong.

use pump_quant_execution::ex_route_health::*;
use pump_quant_execution::ex_route_policy::{route_health_is_measured, Route, RouteCtx};

fn landed(ms: u64) -> Attempt {
    Attempt {
        landed: true,
        latency_ms: ms,
    }
}
fn dropped(ms: u64) -> Attempt {
    Attempt {
        landed: false,
        latency_ms: ms,
    }
}

fn blank_ctx() -> RouteCtx {
    RouteCtx {
        entry_edge_bps: 1_000,
        trade_size_lamports: 1_000_000_000,
        opportunity_half_life_ms: 10_000,
        is_sell: false,
        is_forced_exit: false,
        current_slippage_bps: 0,
        promotion_enabled: true,
        half_life_threshold_ms: 2_000,
        min_edge_for_nozomi_bps: 500,
        min_edge_for_jito_bps: 500,
        exit_slippage_trigger_bps: 300,
        nozomi_healthy: false,
        jito_healthy: false,
        rpc_latency_ms: 0,
        nozomi_latency_ms: 0,
        jito_latency_ms: 0,
        rpc_fail_bps: 0,
        nozomi_fail_bps: 0,
        jito_fail_bps: 0,
        nozomi_tip_lamports: 1_000_000,
        jito_tip_lamports: 1_000_000,
    }
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_under_sampled_reports_nothing_not_health() {
    let mut h = RouteHealth::new();
    for _ in 0..(MIN_SAMPLES - 1) {
        h.record(landed(100));
    }
    assert_eq!(h.samples(), MIN_SAMPLES - 1);
    assert!(!h.has_measurement());
    assert_eq!(
        h.fail_bps(),
        None,
        "a perfect but tiny window must not report 0 bps"
    );
    assert_eq!(h.latency_ms(), None);
    assert!(
        !h.is_healthy(500),
        "unmeasured must fail closed, not default to healthy"
    );
}

#[test]
fn negative_control_empty_tracker_is_not_healthy() {
    let h = RouteHealth::new();
    assert!(!h.is_healthy(10_000));
    assert_eq!(h.fail_bps(), None);
    assert_eq!(h.total_attempts(), 0);
}

#[test]
fn negative_control_under_sampled_routes_do_not_touch_route_ctx() {
    // Three observations on every route: the EV model must remain blind, so
    // route_health_is_measured must still say false.
    let mut set = RouteHealthSet::new();
    for _ in 0..3 {
        set.record(Route::Rpc, landed(700));
        set.record(Route::JitoBundle, landed(300));
        set.record(Route::Nozomi, landed(250));
    }
    let mut ctx = blank_ctx();
    set.fill_route_ctx(&mut ctx, 500);

    assert_eq!(ctx.rpc_latency_ms, 0);
    assert_eq!(ctx.rpc_fail_bps, 0);
    assert!(!ctx.jito_healthy);
    assert!(!ctx.nozomi_healthy);
    assert!(
        !route_health_is_measured(&ctx),
        "three samples must not unblock the plan gate"
    );
    assert!(!set.all_legacy_measured());
}

// ────────────────────────────── measured behaviour ───────────────────────────

#[test]
fn one_sample_past_the_bar_starts_reporting() {
    let mut h = RouteHealth::new();
    for _ in 0..MIN_SAMPLES {
        h.record(landed(100));
    }
    assert!(h.has_measurement());
    assert_eq!(h.fail_bps(), Some(0));
    assert_eq!(h.latency_ms(), Some(100));
    assert!(h.is_healthy(0));
}

#[test]
fn fail_rate_is_exact_integer_basis_points() {
    let mut h = RouteHealth::new();
    // 4 failures in 16 = 25% = 2500 bps, exactly.
    for i in 0..16 {
        if i % 4 == 0 {
            h.record(dropped(900));
        } else {
            h.record(landed(200));
        }
    }
    assert_eq!(h.fail_bps(), Some(2_500));
    assert!(h.is_healthy(2_500), "tolerance is inclusive");
    assert!(!h.is_healthy(2_499));
}

#[test]
fn accepted_but_never_confirmed_counts_as_failure() {
    // The distinction the Attempt doc calls out: an endpoint accepting a
    // submission is not the same as the transaction landing.
    let mut h = RouteHealth::new();
    for _ in 0..MIN_SAMPLES {
        h.record(Attempt {
            landed: false,
            latency_ms: 50, // fast 200 OK, never included
        });
    }
    assert_eq!(h.fail_bps(), Some(10_000));
    assert!(!h.is_healthy(9_999));
}

#[test]
fn ewma_is_seeded_by_the_first_sample() {
    // Without seeding, an EWMA starting at 0 spends many observations climbing,
    // and would under-report latency for exactly the early window a new route
    // is being judged on.
    let mut h = RouteHealth::new();
    h.record(landed(800));
    for _ in 1..MIN_SAMPLES {
        h.record(landed(800));
    }
    assert_eq!(h.latency_ms(), Some(800));
}

#[test]
fn ewma_moves_toward_a_new_regime_without_jumping() {
    let mut h = RouteHealth::new();
    for _ in 0..MIN_SAMPLES {
        h.record(landed(100));
    }
    assert_eq!(h.latency_ms(), Some(100));
    // One slow sample moves the average by delta >> 3 = (900-100)/8 = 100.
    h.record(landed(900));
    assert_eq!(h.latency_ms(), Some(200));
}

#[test]
fn window_is_bounded_and_evicts_oldest() {
    let mut h = RouteHealth::new();
    for _ in 0..WINDOW {
        h.record(dropped(500));
    }
    assert_eq!(h.samples(), WINDOW);
    assert_eq!(h.fail_bps(), Some(10_000));
    // Fill the window with successes; the old failures must age out entirely.
    for _ in 0..WINDOW {
        h.record(landed(500));
    }
    assert_eq!(h.samples(), WINDOW, "window must stay bounded");
    assert_eq!(h.fail_bps(), Some(0));
    assert_eq!(h.total_attempts(), (WINDOW * 2) as u64);
}

#[test]
fn measured_set_populates_route_ctx_and_unblocks_the_gate() {
    let mut set = RouteHealthSet::new();
    for i in 0..WINDOW {
        // RPC: 25% drop, slow. Jito: clean, fast. Nozomi: clean, faster.
        set.record(
            Route::Rpc,
            if i % 4 == 0 {
                dropped(800)
            } else {
                landed(800)
            },
        );
        set.record(Route::JitoBundle, landed(400));
        set.record(Route::Nozomi, landed(250));
    }
    let mut ctx = blank_ctx();
    set.fill_route_ctx(&mut ctx, 500);

    assert_eq!(ctx.rpc_fail_bps, 2_500);
    assert_eq!(ctx.rpc_latency_ms, 800);
    assert_eq!(ctx.jito_fail_bps, 0);
    assert_eq!(ctx.jito_latency_ms, 400);
    assert!(ctx.jito_healthy);
    assert!(ctx.nozomi_healthy);
    assert!(
        route_health_is_measured(&ctx),
        "real measurements must unblock the plan gate"
    );
    assert!(set.all_legacy_measured());
}

#[test]
fn an_unhealthy_route_is_marked_unhealthy_not_merely_slow() {
    let mut set = RouteHealthSet::new();
    for _ in 0..WINDOW {
        set.record(Route::JitoBundle, dropped(400));
        set.record(Route::Nozomi, landed(400));
    }
    let mut ctx = blank_ctx();
    set.fill_route_ctx(&mut ctx, 500);
    assert!(!ctx.jito_healthy);
    assert!(ctx.nozomi_healthy);
}

#[test]
fn sender_health_is_tracked_separately_from_legacy_routes() {
    let mut set = RouteHealthSet::new();
    for _ in 0..WINDOW {
        set.record_sender(landed(380));
    }
    assert!(set.sender.has_measurement());
    assert_eq!(set.sender.latency_ms(), Some(380));
    assert_eq!(set.sender.fail_bps(), Some(0));
    // Recording Sender must not have touched the legacy routes.
    assert!(!set.rpc.has_measurement());
    assert!(!set.all_legacy_measured());
}
