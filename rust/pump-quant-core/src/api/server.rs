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
        }
    }
}

impl Default for ApiState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Handlers ──────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "data": {
            "overall": "healthy"
        }
    }))
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
            "uptime_s": s.uptime_s
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
