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
}

impl PaperTradeLogger {
    /// Create a new logger, opening (or creating) the file at `path` in append mode.
    pub fn new(path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open paper trade log: {path}"))?;

        tracing::info!("PaperTradeLogger writing to: {path}");

        Ok(Self {
            file,
            path: path.to_string(),
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
            "engineVersion": "v5-rust",
            "dataVersion": 2,
            "is_paper": true,
            "excludeFromAnalysis": false,
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
