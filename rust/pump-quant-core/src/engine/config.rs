//! Configuration loader for the MEV engine.
//!
//! Reads canary.json, extracts the `mev` section, and maps it into
//! `GateConfig`, `ScoreConfig`, and `PositionConfig` structs.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::gates::GateConfig;
use super::health::HealthConfig;
use super::positions::{PositionConfig, SizeTier, TpSlTier};
use super::scorer::ScoreConfig;
use crate::feeds::FeedSource;

// ── JSON schema for the `mev` section of canary.json ─────────────────────────

#[derive(Deserialize, Debug)]
pub struct MevJsonConfig {
    pub enabled: Option<bool>,
    pub paper_mode: Option<bool>,

    // Gate thresholds (SOL floats → lamports)
    pub trigger_min_buy_sol: Option<f64>,
    pub trigger_max_buy_sol: Option<f64>,
    pub min_vsol_in_curve: Option<f64>,
    pub max_vsol_in_curve: Option<f64>,
    pub max_token_age_s: Option<u64>,
    pub min_unique_buyers: Option<u16>,

    // Pre-trigger gates
    pub pre_trigger_min_buys_1s: Option<u16>,
    pub pre_trigger_min_buys_2s: Option<u16>,
    pub pre_trigger_min_buys_5s: Option<u16>,
    pub pre_trigger_max_gap_ms: Option<u64>,
    pub pre_trigger_min_vsol_accel: Option<f64>,
    pub pre_trigger_min_sell_count_5s: Option<u16>,
    pub pre_trigger_max_vsol_delta_3s: Option<f64>,
    pub pre_trigger_min_volume_5s: Option<f64>,
    pub max_trigger_isolation: Option<f64>,

    // Score threshold
    pub trigger_min_score: Option<f64>,

    // Position management
    pub max_hold_ms: Option<u64>,
    pub max_concurrent_positions: Option<usize>,
    pub entry_size_sol: Option<f64>,
    pub max_entry_size_sol: Option<f64>,
    pub take_profit_pct: Option<f64>,
    pub stop_loss_pct: Option<f64>,
    pub size_variance_pct: Option<f64>,
    pub jito_tip_lamports: Option<u64>,

    // Next-buyer exit
    pub next_buyer_exit: Option<bool>,
    pub next_buyer_aggregate_flow_ratio: Option<f64>,
    pub next_buyer_count_threshold: Option<u32>,
    pub next_buyer_single_buy_ratio: Option<f64>,
    pub next_buyer_profit_exit_pct: Option<f64>,

    // Momentum decay
    pub momentum_decay_check_ms: Option<u64>,
    pub momentum_decay_min_mfe_pct: Option<f64>,
    pub momentum_decay_max_drawdown_pct: Option<f64>,

    // Intra-hold trailing stop
    pub intra_hold_trailing_stop_pct: Option<f64>,
    pub intra_hold_trailing_stop_min_mfe_pct: Option<f64>,

    // Tiers
    pub tp_tiers: Option<Vec<TpSlTierJson>>,
    pub size_tiers: Option<Vec<SizeTierJson>>,

    // ToD config
    pub tod_config: Option<TodConfigJson>,

    // Blocked sources
    pub blocked_trigger_sources: Option<Vec<String>>,

    // Logging
    pub log_file: Option<String>,

    // Safety / circuit breakers
    pub daily_loss_cap_sol: Option<f64>,
    pub paper_daily_loss_cap_sol: Option<f64>,
    pub live_daily_loss_cap_sol: Option<f64>,
    pub consecutive_stop_pause_count: Option<u32>,
    pub consecutive_stop_pause_ms: Option<u64>,

    // Min hold before NB exit (ms)
    pub min_hold_before_exit_ms: Option<u64>,

    // Creator sell TTL (ms)
    pub creator_sell_ttl_ms: Option<u64>,

    // Master toggle for TOD gate. When false, blocked_hours_utc is ignored.
    // Use false in paper mode to collect data 24/7.
    pub tod_gate_enabled: Option<bool>,

    // Entry randomizer config (anti-fingerprinting)
    pub jitter_ms_min: Option<u32>,
    pub jitter_ms_max: Option<u32>,
    // size_variance_pct already declared above (position management section)

    // ── Scaled entry config (SPEC 3) ────────────────────────────────
    // When enabled, golden segment entries use a two-phase scaled entry:
    // Phase 1: enter at initial_pct of full size, wait for confirmation buy.
    // Phase 2: on confirmation, scale up to full size; on timeout, keep partial.
    // TODO: Full implementation deferred pending PositionManager API extension.
    // Currently stub-only: config fields parsed, JSONL schema emitted, logic is no-op.
    pub scaled_entry_enabled: Option<bool>,
    pub scaled_entry_initial_pct: Option<f64>,
    pub scaled_entry_confirmation_window_ms: Option<u64>,
    pub scaled_entry_confirmation_min_sol: Option<f64>,

    // ── Graduation arb config (SPEC 4) ──────────────────────────────
    // Infrastructure for graduation arbitrage between bonding curve terminal
    // price and Raydium AMM opening price. Disabled by default — requires
    // ShredStream for competitive latency.
    pub graduation_arb_enabled: Option<bool>,
    pub graduation_arb_max_sol: Option<f64>,
    pub graduation_arb_min_spread_pct: Option<f64>,
    pub graduation_arb_tp_pct: Option<f64>,
    pub graduation_arb_sl_pct: Option<f64>,
    pub graduation_arb_max_hold_ms: Option<u64>,
    pub graduation_arb_jito_tip_sol: Option<f64>,
}

#[derive(Deserialize, Debug)]
pub struct TpSlTierJson {
    pub trigger_max_sol: f64,
    pub tp_pct: f64,
    pub sl_pct: f64,
}

#[derive(Deserialize, Debug)]
pub struct SizeTierJson {
    pub trigger_max_sol: f64,
    pub size_sol: f64,
}

#[derive(Deserialize, Debug)]
pub struct TodConfigJson {
    pub blocked_hours_utc: Option<Vec<u8>>,
    pub boosted_hours_utc: Option<Vec<u8>>,
    pub reduced_hours_utc: Option<Vec<u8>>,
}

// ── Parsed engine config ─────────────────────────────────────────────────────

/// All engine configuration, parsed from the `mev` section.
pub struct EngineConfig {
    pub gate: GateConfig,
    pub score: ScoreConfig,
    pub position: PositionConfig,
    pub health: HealthConfig,
    pub paper_mode: bool,
    pub log_file: String,
    /// Daily loss cap in lamports (mode-aware: paper vs live).
    pub daily_loss_cap_lamports: u64,
    /// Number of consecutive stop-loss exits before pausing.
    pub consecutive_stop_pause_count: u32,
    /// Duration (ms) to pause after consecutive stop breaker fires.
    pub consecutive_stop_pause_ms: u64,
    /// UTC hours that get ToD boost (loaded from config).
    pub boosted_hours_utc: Vec<u8>,
    /// ToD boost multiplier for boosted hours (default 1.25).
    pub tod_boost_multiplier: f64,
    /// Entry randomizer config (anti-fingerprinting for live mode).
    pub randomizer: super::entry_randomizer::RandomizerConfig,

    // ── Scaled entry (SPEC 3) — stub config, logic deferred ─────────
    /// Master toggle for scaled entry on golden segment trades.
    pub scaled_entry_enabled: bool,
    /// Fraction of entry_size_sol for the initial (unconfirmed) position (0.0–1.0).
    pub scaled_entry_initial_pct: f64,
    /// Milliseconds to wait for a follow-on confirmation buy before keeping partial size.
    pub scaled_entry_confirmation_window_ms: u64,
    /// Minimum SOL of the follow-on buy to count as confirmation.
    pub scaled_entry_confirmation_min_sol: f64,

    // ── Graduation arb config (SPEC 4) ──────────────────────────────
    /// Whether graduation arb is enabled (default: false).
    pub graduation_arb_enabled: bool,
    /// Max SOL per arb trade (default: 0.30).
    pub graduation_arb_max_sol: f64,
    /// Min spread % between BC terminal price and Raydium opening price (default: 3.0).
    pub graduation_arb_min_spread_pct: f64,
    /// Take-profit % for arb positions (default: 0.03).
    pub graduation_arb_tp_pct: f64,
    /// Stop-loss % for arb positions (default: 0.02).
    pub graduation_arb_sl_pct: f64,
    /// Max hold time in ms for arb positions (default: 5000).
    pub graduation_arb_max_hold_ms: u64,
    /// Jito tip in SOL for arb bundles (default: 0.003).
    pub graduation_arb_jito_tip_sol: f64,
}

impl EngineConfig {
    /// Returns the time-of-day size multiplier for the given UTC hour.
    /// Returns `tod_boost_multiplier` (e.g. 1.25) if the hour is in `boosted_hours_utc`,
    /// otherwise returns 1.0.
    pub fn get_tod_multiplier(&self, hour_utc: u8) -> f64 {
        if self.boosted_hours_utc.contains(&hour_utc) {
            self.tod_boost_multiplier
        } else {
            1.0
        }
    }
}

// ── Loader ───────────────────────────────────────────────────────────────────

fn sol_to_lamports(sol: f64) -> u64 {
    (sol * 1_000_000_000.0) as u64
}

/// Load canary.json from the given path, parse the `mev` section,
/// and return a fully-constructed `EngineConfig`.
pub fn load_config(path: &Path) -> Result<EngineConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;

    let root: serde_json::Value =
        serde_json::from_str(&raw).context("failed to parse canary.json as JSON")?;

    let mev_val = root
        .get("mev")
        .context("canary.json missing 'mev' section")?;

    let mev: MevJsonConfig =
        serde_json::from_value(mev_val.clone()).context("failed to deserialize 'mev' section")?;

    // ── Build GateConfig ────────────────────────────────────────────
    let blocked_sources: Vec<FeedSource> = mev
        .blocked_trigger_sources
        .as_ref()
        .map(|v| {
            v.iter()
                .filter_map(|s| match s.as_str() {
                    "corecast" | "Corecast" => None, // Not a FeedSource variant
                    "helius" | "Helius" => Some(FeedSource::Helius),
                    "pumpportal" | "PumpPortal" => Some(FeedSource::PumpPortal),
                    "shredstream" | "ShredStream" => Some(FeedSource::ShredStream),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let gate = GateConfig {
        trigger_min_buy_lamports: sol_to_lamports(mev.trigger_min_buy_sol.unwrap_or(0.1)),
        trigger_max_buy_lamports: sol_to_lamports(mev.trigger_max_buy_sol.unwrap_or(10.0)),
        min_vsol_lamports: sol_to_lamports(mev.min_vsol_in_curve.unwrap_or(3.0)),
        max_vsol_lamports: sol_to_lamports(mev.max_vsol_in_curve.unwrap_or(85.0)),
        max_token_age_ms: mev.max_token_age_s.unwrap_or(120) * 1000,
        min_unique_buyers: mev.min_unique_buyers.unwrap_or(3),
        pre_trigger_min_buys_1s: mev.pre_trigger_min_buys_1s.unwrap_or(1),
        pre_trigger_min_buys_2s: mev.pre_trigger_min_buys_2s.unwrap_or(2),
        pre_trigger_min_buys_5s: mev.pre_trigger_min_buys_5s.unwrap_or(3),
        pre_trigger_max_gap_ms: mev.pre_trigger_max_gap_ms.unwrap_or(3000),
        pre_trigger_min_vsol_accel: sol_to_lamports(
            mev.pre_trigger_min_vsol_accel.unwrap_or(0.1),
        ),
        pre_trigger_min_sell_count_5s: mev.pre_trigger_min_sell_count_5s.unwrap_or(0),
        pre_trigger_max_vsol_delta_3s: sol_to_lamports(
            mev.pre_trigger_max_vsol_delta_3s.unwrap_or(30.0),
        ),
        creator_sell_ttl_ms: mev.creator_sell_ttl_ms.unwrap_or(30_000),
        pre_trigger_min_volume_5s_lamports: sol_to_lamports(
            mev.pre_trigger_min_volume_5s.unwrap_or(0.5),
        ),
        max_trigger_isolation: mev.max_trigger_isolation.unwrap_or(0.5),
        trigger_min_score: mev.trigger_min_score.unwrap_or(0.35),
        blocked_sources,
        large_trigger_lamports: 1_500_000_000,
        large_trigger_min_unique_buyers: 5,
        blocked_hours_utc: mev
            .tod_config
            .as_ref()
            .and_then(|tod| tod.blocked_hours_utc.clone())
            .unwrap_or_default(),
        boosted_hours_utc: mev
            .tod_config
            .as_ref()
            .and_then(|tod| tod.boosted_hours_utc.clone())
            .unwrap_or_default(),
        tod_gate_enabled: mev.tod_gate_enabled.unwrap_or(true),
        regime_config: super::regime::RegimeConfig::default(),
    };

    // ── Build ScoreConfig (defaults — no JSON overrides yet) ────────
    let score = ScoreConfig::default();

    // ── Build PositionConfig ────────────────────────────────────────
    let tp_tiers: Vec<TpSlTier> = mev
        .tp_tiers
        .as_ref()
        .map(|tiers| {
            tiers
                .iter()
                .map(|t| TpSlTier {
                    trigger_max_lamports: sol_to_lamports(t.trigger_max_sol),
                    tp_pct: t.tp_pct,
                    sl_pct: t.sl_pct,
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![TpSlTier {
                trigger_max_lamports: u64::MAX,
                tp_pct: mev.take_profit_pct.unwrap_or(0.025),
                sl_pct: mev.stop_loss_pct.unwrap_or(0.015),
            }]
        });

    let size_tiers: Vec<SizeTier> = mev
        .size_tiers
        .as_ref()
        .map(|tiers| {
            tiers
                .iter()
                .map(|t| SizeTier {
                    trigger_max_lamports: sol_to_lamports(t.trigger_max_sol),
                    size_lamports: sol_to_lamports(t.size_sol),
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![SizeTier {
                trigger_max_lamports: u64::MAX,
                size_lamports: sol_to_lamports(mev.entry_size_sol.unwrap_or(0.1)),
            }]
        });

    let boosted_hours_utc = mev
        .tod_config
        .as_ref()
        .and_then(|tod| tod.boosted_hours_utc.clone())
        .unwrap_or_else(|| vec![14, 15]);

    let position = PositionConfig {
        max_hold_ms: mev.max_hold_ms.unwrap_or(1200),
        momentum_decay_check_ms: mev.momentum_decay_check_ms.unwrap_or(50),
        momentum_decay_min_mfe_pct: mev.momentum_decay_min_mfe_pct.unwrap_or(0.001),
        momentum_decay_max_drawdown_pct: mev.momentum_decay_max_drawdown_pct.unwrap_or(0.003),
        intra_hold_trailing_stop_pct: mev.intra_hold_trailing_stop_pct.unwrap_or(1.0),
        intra_hold_trailing_stop_min_mfe_pct: mev
            .intra_hold_trailing_stop_min_mfe_pct
            .unwrap_or(1.0),
        next_buyer_profit_exit_pct: mev.next_buyer_profit_exit_pct.unwrap_or(0.01),
        next_buyer_aggregate_flow_ratio: mev.next_buyer_aggregate_flow_ratio.unwrap_or(0.35),
        next_buyer_count_threshold: mev.next_buyer_count_threshold.unwrap_or(3),
        next_buyer_single_buy_ratio: mev.next_buyer_single_buy_ratio.unwrap_or(0.25),
        tp_tiers,
        size_tiers,
        max_concurrent_positions: mev.max_concurrent_positions.unwrap_or(10),
        max_entry_size_lamports: sol_to_lamports(mev.max_entry_size_sol.unwrap_or(0.25)),
        size_variance_pct: mev.size_variance_pct.unwrap_or(0.2),
        jito_tip_lamports: mev.jito_tip_lamports.unwrap_or(50_000),
        min_hold_before_exit_ms: mev.min_hold_before_exit_ms.unwrap_or(300),
        tod_boost_multiplier: 1.25,
        boosted_hours_utc,
    };

    let paper_mode = mev.paper_mode.unwrap_or(true);
    let log_file = mev
        .log_file
        .unwrap_or_else(|| "data/mev_paper_trades.jsonl".to_string());

    // ── Safety / circuit breaker config ─────────────────────────────
    // Daily loss cap: paper mode uses paper_daily_loss_cap_sol, live uses live_daily_loss_cap_sol,
    // both fall back to daily_loss_cap_sol, then to 5.0 SOL.
    let daily_loss_cap_sol = if paper_mode {
        mev.paper_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(5.0)
    } else {
        mev.live_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(0.18)
    };
    let daily_loss_cap_lamports = sol_to_lamports(daily_loss_cap_sol);

    let consecutive_stop_pause_count = mev.consecutive_stop_pause_count.unwrap_or(3);
    let consecutive_stop_pause_ms = mev.consecutive_stop_pause_ms.unwrap_or(180_000);

    // ── Build HealthConfig from top-level `health` section ──────────
    let health = if let Some(health_val) = root.get("health") {
        let market_feed_stale_s: u64 = health_val
            .get("market_feed_stale_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(45);
        let auto_pause_on_degraded: bool = health_val
            .get("auto_pause_on_degraded")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        HealthConfig {
            market_feed_stale_ms: market_feed_stale_s * 1000,
            auto_pause_on_degraded,
        }
    } else {
        HealthConfig::default()
    };

    // ── ToD multiplier config ──────────────────────────────────────
    let tod_boosted_hours = mev
        .tod_config
        .as_ref()
        .and_then(|tod| tod.boosted_hours_utc.clone())
        .unwrap_or_default();
    let tod_boost_multiplier = 1.25_f64; // hardcoded per spec; config override possible later

    Ok(EngineConfig {
        gate,
        score,
        position,
        health,
        paper_mode,
        log_file,
        daily_loss_cap_lamports,
        consecutive_stop_pause_count,
        consecutive_stop_pause_ms,
        boosted_hours_utc: tod_boosted_hours,
        tod_boost_multiplier,
        randomizer: super::entry_randomizer::RandomizerConfig {
            jitter_ms_min: mev.jitter_ms_min.unwrap_or(50),
            jitter_ms_max: mev.jitter_ms_max.unwrap_or(200),
            size_variance_pct: mev.size_variance_pct.unwrap_or(0.20),
            base_entry_lamports: sol_to_lamports(mev.entry_size_sol.unwrap_or(0.12)),
        },
        // Scaled entry (SPEC 3) — config parsed, logic is stub-only for now
        scaled_entry_enabled: mev.scaled_entry_enabled.unwrap_or(false),
        scaled_entry_initial_pct: mev.scaled_entry_initial_pct.unwrap_or(0.40),
        scaled_entry_confirmation_window_ms: mev.scaled_entry_confirmation_window_ms.unwrap_or(400),
        scaled_entry_confirmation_min_sol: mev.scaled_entry_confirmation_min_sol.unwrap_or(0.10),
        // Graduation arb (SPEC 4) — disabled by default, infrastructure only
        graduation_arb_enabled: mev.graduation_arb_enabled.unwrap_or(false),
        graduation_arb_max_sol: mev.graduation_arb_max_sol.unwrap_or(0.30),
        graduation_arb_min_spread_pct: mev.graduation_arb_min_spread_pct.unwrap_or(3.0),
        graduation_arb_tp_pct: mev.graduation_arb_tp_pct.unwrap_or(0.03),
        graduation_arb_sl_pct: mev.graduation_arb_sl_pct.unwrap_or(0.02),
        graduation_arb_max_hold_ms: mev.graduation_arb_max_hold_ms.unwrap_or(5000),
        graduation_arb_jito_tip_sol: mev.graduation_arb_jito_tip_sol.unwrap_or(0.003),
    })
}
