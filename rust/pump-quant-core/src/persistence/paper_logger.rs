//! JSONL paper trade logger.
//!
//! Appends closed positions as single JSON lines to a file,
//! matching the TypeScript `mev_paper_trades.jsonl` camelCase schema exactly.

use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{Context, Result};
use serde_json::json;

use crate::engine::positions::{ClosedPosition, ExitReason};

/// Appends paper trade results as JSONL (one JSON object per line).
/// Output schema matches the TS paper-trade-logger.ts exactly (camelCase keys).
pub struct PaperTradeLogger {
    file: std::fs::File,
    path: String,
    paper_mode: bool,
    config_version: String,
    // ── Golden segment thresholds (for strategyTag computation at log time) ──
    golden_min_buys_1s: u16,
    golden_min_hour_utc: u8,
    golden_max_hour_utc: u8,
    golden_min_vsol: f64,
    golden_max_vsol: f64,
    // ── Scaled entry config (SPEC 3 stub) ───────────────────────────
    scaled_entry_enabled: bool,
    scaled_entry_initial_pct: f64,
}

impl PaperTradeLogger {
    /// Create a new logger, opening (or creating) the file at `path` in append mode.
    ///
    /// `golden_thresholds`: (min_buys_1s, min_hour_utc, max_hour_utc, min_vsol_sol, max_vsol_sol)
    /// Used to compute `strategyTag` at log time without modifying ClosedPosition.
    pub fn new(
        path: &str,
        paper_mode: bool,
        config_version: String,
        golden_thresholds: (u16, u8, u8, f64, f64),
        scaled_entry_enabled: bool,
        scaled_entry_initial_pct: f64,
    ) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open paper trade log: {path}"))?;

        tracing::info!(config_version = %config_version, "PaperTradeLogger writing to: {path}");

        Ok(Self {
            file,
            path: path.to_string(),
            paper_mode,
            config_version,
            golden_min_buys_1s: golden_thresholds.0,
            golden_min_hour_utc: golden_thresholds.1,
            golden_max_hour_utc: golden_thresholds.2,
            golden_min_vsol: golden_thresholds.3,
            golden_max_vsol: golden_thresholds.4,
            scaled_entry_enabled,
            scaled_entry_initial_pct,
        })
    }

    /// Append a closed position as one JSON line with camelCase keys
    /// matching the TS `paper-trade-logger.ts` schema.
    ///
    /// `entry_mint_b58` is the base58-encoded mint address string.
    pub fn log(&mut self, pos: &ClosedPosition, entry_mint_b58: &str) -> Result<()> {
        let exit_str = match pos.exit_reason {
            ExitReason::TakeProfit => "take_profit",
            ExitReason::StopLoss => "stop_loss",
            ExitReason::NextBuyer => "next_buyer",
            ExitReason::MaxHold => "max_hold",
            ExitReason::IntraHoldTrail => "intra_hold_trail",
            ExitReason::MomentumDecayFlat => "momentum_decay_flat",
            ExitReason::MomentumDecayFade => "momentum_decay_fade",
        };

        // Convert lamports to SOL for human-readable output
        let size_sol = pos.size_sol as f64 / 1_000_000_000.0;
        let gross_pnl_sol = pos.gross_pnl_sol as f64 / 1_000_000_000.0;
        let net_pnl_sol = pos.net_pnl_sol as f64 / 1_000_000_000.0;
        let fees_sol = pos.fees_sol as f64 / 1_000_000_000.0;
        let entry_vsol = pos.entry_vsol as f64 / 1_000_000_000.0;
        let exit_vsol = pos.exit_vsol as f64 / 1_000_000_000.0;

        // pnlPct = gross PnL / size (same as TS: trade.pnlPct)
        let pnl_pct = if size_sol > 0.0 {
            gross_pnl_sol / size_sol
        } else {
            0.0
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // MFE/MAE computation (from peak/trough vSol vs entry vSol)
        let mfe_sol = if pos.peak_vsol > pos.entry_vsol {
            (pos.peak_vsol - pos.entry_vsol) as f64 / 1_000_000_000.0
        } else {
            0.0
        };
        let mae_sol = if pos.entry_vsol > pos.trough_vsol {
            (pos.entry_vsol - pos.trough_vsol) as f64 / 1_000_000_000.0
        } else {
            0.0
        };
        let mfe_pct = if entry_vsol > 0.0 { mfe_sol / entry_vsol } else { 0.0 };
        let mae_pct = if entry_vsol > 0.0 { mae_sol / entry_vsol } else { 0.0 };

        let trigger_buy_sol = pos.trigger_sol as f64 / 1_000_000_000.0;
        let vsol_delta_3s_sol = pos.vsol_delta_3s as f64 / 1_000_000_000.0;
        let volume_5s_sol = pos.volume_5s as f64 / 1_000_000_000.0;

        // Trigger hour UTC (from entry timestamp)
        let trigger_hour_utc = ((pos.entry_ts_ms / 3_600_000) % 24) as u8;

        // Bonding curve progress at entry
        let curve_pct = if pos.current_vtokens > 0 {
            crate::engine::regime::compute_bonding_curve_progress(
                pos.current_vtokens,
                crate::engine::regime::INITIAL_VIRTUAL_TOKENS,
            )
        } else {
            0.0
        };

        // Auto-flag anomalous trades (>90% loss = data bug)
        let exclude = gross_pnl_sol < -0.9 * size_sol;

        // ── Compute strategyTag (SPEC 1/5) ──────────────────────────
        // Derived at log time from entry context fields on ClosedPosition.
        // This avoids modifying ClosedPosition struct (positions.rs is read-only).
        let strategy_tag = {
            let golden_buys = pos.pre_trigger_buys_1s >= self.golden_min_buys_1s;
            let golden_hours = trigger_hour_utc >= self.golden_min_hour_utc
                && trigger_hour_utc <= self.golden_max_hour_utc;
            let golden_vsol = entry_vsol >= self.golden_min_vsol
                && entry_vsol <= self.golden_max_vsol;

            if golden_buys && golden_hours && golden_vsol {
                "backrun_golden"
            } else {
                "backrun_standard"
            }
        };

        // ── Scaled entry fields (SPEC 3 — stub) ────────────────────
        // TODO: scaled entry logic — full impl requires PositionManager API extension.
        // Currently always false/0.40 since actual scaled entry is not yet wired.
        // When PositionManager carries scaled_confirmed, these will reflect real state.
        let scaled_entry = false; // stub: no trades use scaled entry yet
        let scaled_confirmed = false; // stub: no confirmations possible yet
        let scaled_initial_pct = self.scaled_entry_initial_pct;

        let line = json!({
            "mint": entry_mint_b58,
            "entryVSol": entry_vsol,
            "exitVSol": exit_vsol,
            "entryTimestampMs": pos.entry_ts_ms,
            "exitTimestampMs": pos.exit_ts_ms,
            "holdMs": pos.hold_ms,
            "sizeSol": size_sol,
            "pnlSol": gross_pnl_sol,
            "pnlPct": pnl_pct,
            "exitReason": exit_str,
            "score": pos.score,
            "netPnlSol": net_pnl_sol,
            "feesSol": fees_sol,
            // MFE/MAE — essential for TP/SL calibration
            "mfeSol": mfe_sol,
            "maeSol": mae_sol,
            "mfePct": mfe_pct,
            "maePct": mae_pct,
            // Trigger context
            "triggerBuySol": trigger_buy_sol,
            "triggerHourUtc": trigger_hour_utc,
            "curvePct": curve_pct,
            "uniqueBuyerCount": pos.unique_buyers,
            // Pre-trigger gate signals
            "preTriggerBuys1s": pos.pre_trigger_buys_1s,
            "preTriggerBuys2s": pos.pre_trigger_buys_2s,
            "preTriggerBuys5s": pos.pre_trigger_buys_5s,
            "preTriggerVSolDelta3s": vsol_delta_3s_sol,
            "preTriggerVolume5s": volume_5s_sol,
            "preTriggerSellCount5s": pos.sell_count_5s,
            // Next-buyer flow data
            "tradesAfterEntry": pos.trades_after_entry,
            "buysAfterEntry": pos.buys_after_entry,
            "flowAfterEntrySol": pos.flow_after_entry as f64 / 1_000_000_000.0,
            // Sizing context
            "todMultiplier": pos.tod_multiplier,
            // Strategy classification (SPEC 1/5)
            "strategyTag": strategy_tag,
            // Scaled entry fields (SPEC 3 — stub, always false/defaults until full impl)
            "scaledEntry": scaled_entry,
            "scaledConfirmed": scaled_confirmed,
            "scaledInitialPct": scaled_initial_pct,
            // Metadata
            "engineVersion": "v5-rust",
            "configVersion": self.config_version,
            "dataVersion": 4,
            "is_paper": self.paper_mode,
            "excludeFromAnalysis": exclude,
            "recordedAt": now_ms,
        });

        let mut line_str = serde_json::to_string(&line)
            .context("failed to serialize paper trade to JSON")?;
        line_str.push('\n');

        self.file
            .write_all(line_str.as_bytes())
            .with_context(|| format!("failed to write to paper trade log: {}", self.path))?;

        Ok(())
    }

    /// Get the path of the log file.
    pub fn path(&self) -> &str {
        &self.path
    }
}
