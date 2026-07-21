//! Leaf `ex_route_policy`: MEV-aware route selection.
//!
//! Ported from `execution/route-policy.ts` (`selectRoute` / `selectForcedExitRoute`
//! / `computeRouteEV`), adapted to the three routes this bot actually ships and
//! to integer lamport / basis-point math (constitution §22 — the TS original
//! scored routes in floating-point SOL).
//!
//! ## Route mapping
//! The legacy `RouteMode` set (`local`, `lightning`, `private`, `jito`/`bundle`)
//! collapses onto the three concrete senders here:
//! - [`Route::Rpc`] ← `local`: the RPC-primary path. Per the legacy
//!   `rpc_sender.rs`, RPC is the default and there is **no** blind Jito fallback,
//!   so `Rpc` is the default and the tie-break winner.
//! - [`Route::Nozomi`] ← `lightning`: a low-latency alternate sender, promoted
//!   only when the opportunity is short-lived and the edge justifies its tip.
//! - [`Route::JitoBundle`] ← `private`/`bundle`: MEV-protected submission, used
//!   for sells and high-edge/high-slippage exits.
//!
//! ## Responsibility
//! Choose the route that maximizes integer expected net value (in lamports),
//! with a dedicated fast path for forced exits that never waits.
//!
//! ## Constitution refs
//! - §22: edges/slippage/fail-rates in basis points; sizes/EV in lamports;
//!   all EV math in `i128`.
//! - Deterministic: pure function of [`RouteCtx`]; no clock, no RNG.

/// One whole unit in basis points (`1.0 == 10_000 bps`).
pub const BPS_ONE: i128 = 10_000;

/// The concrete submission routes available to the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// RPC-primary `sendTransaction` path (default).
    Rpc,
    /// Jito MEV-protected bundle submission.
    JitoBundle,
    /// Nozomi low-latency alternate sender.
    Nozomi,
}

/// All inputs required to choose a route. Health and configuration are folded in
/// as explicit fields so the decision is a pure function (the legacy class kept
/// this as mutable internal state updated from execution history).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteCtx {
    // ── Trade / opportunity ────────────────────────────────────────────────
    /// Expected entry edge in basis points of trade size.
    pub entry_edge_bps: u32,
    /// Trade size in lamports.
    pub trade_size_lamports: u64,
    /// Opportunity half-life in milliseconds (smaller ⇒ more time pressure).
    pub opportunity_half_life_ms: u64,
    /// Whether this is a sell (exit) rather than a buy.
    pub is_sell: bool,
    /// Whether this is a forced exit (stop/liquidation): fastest healthy route.
    pub is_forced_exit: bool,
    /// Current observed slippage in basis points.
    pub current_slippage_bps: u32,

    // ── Policy switches ────────────────────────────────────────────────────
    /// Whether non-default route promotion is enabled at all.
    pub promotion_enabled: bool,
    /// Half-life threshold (ms) below which Nozomi promotion is considered.
    pub half_life_threshold_ms: u64,
    /// Minimum edge (bps) to consider Nozomi.
    pub min_edge_for_nozomi_bps: u32,
    /// Minimum edge (bps) to consider Jito for a buy (sells always qualify).
    pub min_edge_for_jito_bps: u32,
    /// Slippage (bps) above which a forced exit prefers the private Jito route.
    pub exit_slippage_trigger_bps: u32,

    // ── Route health ───────────────────────────────────────────────────────
    /// Whether the Nozomi route is currently healthy.
    pub nozomi_healthy: bool,
    /// Whether the Jito route is currently healthy.
    pub jito_healthy: bool,
    /// Recent average landing latency per route, in milliseconds.
    pub rpc_latency_ms: u64,
    pub nozomi_latency_ms: u64,
    pub jito_latency_ms: u64,
    /// Recent failure rate per route, in basis points.
    pub rpc_fail_bps: u32,
    pub nozomi_fail_bps: u32,
    pub jito_fail_bps: u32,

    // ── Fees ───────────────────────────────────────────────────────────────
    /// Nozomi (lightning) tip/fee in lamports.
    pub nozomi_tip_lamports: u64,
    /// Jito tip in lamports.
    pub jito_tip_lamports: u64,
}

/// Choose the best route for the given context.
///
/// Faithful port of `selectRoute`:
/// 1. Forced exits take the fast path (see [`select_forced_exit_route`]).
/// 2. If promotion is disabled, always use [`Route::Rpc`].
/// 3. Otherwise score every *eligible* route by integer EV and pick the highest;
///    ties resolve to [`Route::Rpc`] (the default), which is why RPC is scored
///    first and only strictly-greater EV displaces it.
pub fn choose_route(ctx: RouteCtx) -> Route {
    if ctx.is_forced_exit {
        return select_forced_exit_route(&ctx);
    }

    if !ctx.promotion_enabled {
        return Route::Rpc;
    }

    // RPC is always a candidate and is the default/tie-break winner.
    let mut best_route = Route::Rpc;
    let mut best_ev = route_ev_lamports(Route::Rpc, &ctx);

    // Nozomi (lightning): only when the opportunity is short-lived, the edge is
    // high enough, and the route is healthy.
    if ctx.opportunity_half_life_ms < ctx.half_life_threshold_ms
        && ctx.entry_edge_bps > ctx.min_edge_for_nozomi_bps
        && ctx.nozomi_healthy
    {
        let ev = route_ev_lamports(Route::Nozomi, &ctx);
        if ev > best_ev {
            best_ev = ev;
            best_route = Route::Nozomi;
        }
    }

    // Jito (private): for sells, or buys whose edge clears the private minimum.
    if ctx.jito_healthy && (ctx.is_sell || ctx.entry_edge_bps > ctx.min_edge_for_jito_bps) {
        let ev = route_ev_lamports(Route::JitoBundle, &ctx);
        if ev > best_ev {
            best_route = Route::JitoBundle;
        }
    }

    best_route
}

/// Fast path for forced exits — prioritize speed and reliability, never wait.
///
/// Port of `selectForcedExitRoute`:
/// - If Jito (private) is healthy and current slippage exceeds the exit
///   trigger, use [`Route::JitoBundle`] to shield the exit.
/// - Else if Nozomi is healthy and materially faster than RPC
///   (`nozomi_latency < 0.7 * rpc_latency`, expressed as `n*10 < r*7`), use it.
/// - Else fall back to [`Route::Rpc`].
pub fn select_forced_exit_route(ctx: &RouteCtx) -> Route {
    if ctx.jito_healthy && ctx.current_slippage_bps > ctx.exit_slippage_trigger_bps {
        return Route::JitoBundle;
    }
    if ctx.nozomi_healthy
        && ctx.nozomi_latency_ms.saturating_mul(10) < ctx.rpc_latency_ms.saturating_mul(7)
    {
        return Route::Nozomi;
    }
    Route::Rpc
}

/// Compute the integer expected net value (lamports) for a route.
///
/// Port of `computeRouteEV`, with each floating-point term re-expressed in
/// integer lamports:
/// ```text
/// gross          = entry_edge_bps * trade_size / 10_000
/// fee            = route tip (0 for RPC)
/// slippage_adj   = (current_slippage_bps * trade_size / 10_000) / 2   (Jito only)
/// latency_decay  = latency_ms * trade_size / 1_000_000
/// failure_cost   = fail_bps * trade_size / (10_000 * 20)
/// EV = gross - fee + slippage_adj - latency_decay - failure_cost
/// ```
/// All terms are computed in `i128`.
pub fn route_ev_lamports(mode: Route, ctx: &RouteCtx) -> i128 {
    let size = i128::from(ctx.trade_size_lamports);
    let gross = i128::from(ctx.entry_edge_bps) * size / BPS_ONE;

    let (fee, latency_ms, fail_bps) = match mode {
        Route::Rpc => (0i128, ctx.rpc_latency_ms, ctx.rpc_fail_bps),
        Route::Nozomi => (
            i128::from(ctx.nozomi_tip_lamports),
            ctx.nozomi_latency_ms,
            ctx.nozomi_fail_bps,
        ),
        Route::JitoBundle => (
            i128::from(ctx.jito_tip_lamports),
            ctx.jito_latency_ms,
            ctx.jito_fail_bps,
        ),
    };

    // Private (Jito) landing reduces MEV extraction by ~50% of current slippage.
    let slippage_adj = if matches!(mode, Route::JitoBundle) {
        (i128::from(ctx.current_slippage_bps) * size / BPS_ONE) / 2
    } else {
        0
    };

    let latency_decay = i128::from(latency_ms) * size / 1_000_000;
    let failure_cost = i128::from(fail_bps) * size / (BPS_ONE * 20);

    gross - fee + slippage_adj - latency_decay - failure_cost
}
