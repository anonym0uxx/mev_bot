//! Axum HTTP server — health, stats, control, and positions endpoints.
//!
//! Port 9421 (Rust daemon) to avoid conflicts with the TS daemon on 9420.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use tracing::{error, info};

use crate::engine::health::HealthMonitor;
use crate::feeds::FeedSource;

/// Default listen port for the Rust engine API.
const DEFAULT_PORT: u16 = 9421;

// ─── Engine Stats ──────────────────────────────────────────────────

/// Shared engine statistics, updated by the engine hot-path and read by the API.
#[derive(Default, Clone, Serialize)]
pub struct EngineStats {
    pub trades_seen: u64,
    pub gates_passed: u64,
    pub positions_opened: u64,
    pub positions_closed: u64,
    pub wins: u64,
    pub losses: u64,
    pub total_pnl_lamports: i64,
    pub paused: bool,
    pub uptime_s: u64,
    pub started_at: u64,
    // Stream event counters (CoreCast/Bitquery)
    pub migrations_seen: u64,
    pub lp_removals_seen: u64,
    pub creator_sells_seen: u64,
    // Graduation arb stats (SPEC 4)
    pub graduation_arb_enabled: bool,
    pub graduation_arb_trades: u64,
    pub graduation_arb_net_sol: f64,
    // Detailed graduation arb counters
    pub grad_arb_migrations: u64,
    pub grad_arb_entries: u64,
    pub grad_arb_timeouts: u64,
    pub grad_arb_pool_not_found: u64,
    pub grad_arb_no_spread: u64,
    pub grad_arb_exits_tp: u64,
    pub grad_arb_exits_sl: u64,
    pub grad_arb_exits_max_hold: u64,
    pub grad_arb_net_sol: f64,
}

// ─── Open Position (for /api/positions) ────────────────────────────

/// Serializable representation of an open position for the API.
#[derive(Clone, Serialize)]
pub struct OpenPositionInfo {
    pub mint_b58: String,
    pub entry_vsol: u64,
    pub current_vsol: u64,
    pub size_sol: u64,
    pub entry_ts_ms: u64,
    pub pnl_pct: f64,
    pub score: f64,
}

// ─── Shared API State ──────────────────────────────────────────────

/// State shared between the API server and the engine.
#[derive(Clone)]
pub struct ApiState {
    pub stats: Arc<Mutex<EngineStats>>,
    pub positions: Arc<Mutex<Vec<OpenPositionInfo>>>,
    pub health_monitor: Option<Arc<HealthMonitor>>,
}

impl ApiState {
    /// Create a new ApiState with default (zeroed) stats and empty positions.
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let stats = EngineStats {
            started_at: now,
            ..Default::default()
        };

        Self {
            stats: Arc::new(Mutex::new(stats)),
            positions: Arc::new(Mutex::new(Vec::new())),
            health_monitor: None,
        }
    }

    /// Create ApiState with an attached HealthMonitor.
    pub fn with_health(health_monitor: Arc<HealthMonitor>) -> Self {
        let mut state = Self::new();
        state.health_monitor = Some(health_monitor);
        state
    }
}

impl Default for ApiState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Handlers ──────────────────────────────────────────────────────

async fn health(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    if let Some(ref monitor) = state.health_monitor {
        let paused = !monitor.is_trading_allowed();
        let pp_last = monitor.last_event_ms(FeedSource::PumpPortal);
        let hel_last = monitor.last_event_ms(FeedSource::Helius);
        let stale_threshold = monitor.stale_threshold_ms();

        let pp_age_s = if pp_last > 0 {
            now_ms.saturating_sub(pp_last) / 1000
        } else {
            0
        };
        let hel_age_s = if hel_last > 0 {
            now_ms.saturating_sub(hel_last) / 1000
        } else {
            0
        };

        let pp_status = if pp_last == 0 {
            "not_started"
        } else if pp_age_s * 1000 > stale_threshold {
            "stale"
        } else {
            "healthy"
        };

        let hel_status = if hel_last == 0 {
            "not_started"
        } else if hel_age_s * 1000 > stale_threshold {
            "stale"
        } else {
            "healthy"
        };

        let overall = if paused { "degraded" } else { "healthy" };

        Json(serde_json::json!({
            "status": "ok",
            "data": {
                "overall": overall,
                "trading_paused": paused,
                "stale_threshold_s": stale_threshold / 1000,
                "feeds": {
                    "pumpportal": { "status": pp_status, "age_s": pp_age_s },
                    "helius": { "status": hel_status, "age_s": hel_age_s }
                }
            }
        }))
    } else {
        Json(serde_json::json!({
            "status": "ok",
            "data": {
                "overall": "healthy",
                "trading_paused": false,
                "feeds": {}
            }
        }))
    }
}

async fn stats(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let mut s = state.stats.lock().unwrap().clone();

    // Compute live uptime
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    s.uptime_s = now.saturating_sub(s.started_at);

    // Derived metrics
    let total_closed = s.wins + s.losses;
    let win_rate = if total_closed > 0 {
        s.wins as f64 / total_closed as f64
    } else {
        0.0
    };
    let pnl_sol = s.total_pnl_lamports as f64 / 1_000_000_000.0;

    Json(serde_json::json!({
        "status": "ok",
        "data": {
            "trades_seen": s.trades_seen,
            "gates_passed": s.gates_passed,
            "positions_opened": s.positions_opened,
            "positions_closed": s.positions_closed,
            "wins": s.wins,
            "losses": s.losses,
            "win_rate": win_rate,
            "pnl_sol": pnl_sol,
            "total_pnl_lamports": s.total_pnl_lamports,
            "paused": s.paused,
            "uptime_s": s.uptime_s,
            "migrations_seen": s.migrations_seen,
            "lp_removals_seen": s.lp_removals_seen,
            "creator_sells_seen": s.creator_sells_seen,
            "graduation_arb": {
                "enabled": s.graduation_arb_enabled,
                "mode": "paper",
                "migrations_detected": s.grad_arb_migrations,
                "arb_entries": s.grad_arb_entries,
                "arb_timeouts": s.grad_arb_timeouts,
                "pool_not_found": s.grad_arb_pool_not_found,
                "no_arb_spread": s.grad_arb_no_spread,
                "exits_tp": s.grad_arb_exits_tp,
                "exits_sl": s.grad_arb_exits_sl,
                "exits_max_hold": s.grad_arb_exits_max_hold,
                "net_sol": s.grad_arb_net_sol
            }
        }
    }))
}

async fn control_pause(State(state): State<ApiState>) -> Json<serde_json::Value> {
    {
        let mut s = state.stats.lock().unwrap();
        s.paused = true;
    }
    info!("[api] trading PAUSED");
    Json(serde_json::json!({
        "status": "ok",
        "data": { "paused": true }
    }))
}

async fn control_resume(State(state): State<ApiState>) -> Json<serde_json::Value> {
    {
        let mut s = state.stats.lock().unwrap();
        s.paused = false;
    }
    info!("[api] trading RESUMED");
    Json(serde_json::json!({
        "status": "ok",
        "data": { "paused": false }
    }))
}

async fn positions(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let pos = state.positions.lock().unwrap().clone();
    Json(serde_json::json!({
        "status": "ok",
        "data": {
            "count": pos.len(),
            "positions": pos
        }
    }))
}

// ─── Server Startup ────────────────────────────────────────────────

/// Build the axum router with all API routes.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/control/pause", post(control_pause))
        .route("/api/control/resume", post(control_resume))
        .route("/api/positions", get(positions))
        .with_state(state)
}

/// Start the API server on `0.0.0.0:9421`.
///
/// This is an async function — spawn it as a tokio task.
/// It runs until the process exits or the listener errors.
pub async fn start_server(state: ApiState) {
    let port = std::env::var("RUST_API_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", port);

    info!("[api] starting HTTP server on {}", addr);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("[api] failed to bind {}: {}", addr, e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        error!("[api] server error: {}", e);
    }
}
