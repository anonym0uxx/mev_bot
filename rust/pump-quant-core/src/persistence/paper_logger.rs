//! JSONL paper trade logger.
//!
//! Appends closed positions as single JSON lines to a file,
//! matching the TypeScript `backrun_paper_trades.jsonl` camelCase schema exactly.

use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{Context, Result};
use serde_json::json;

use crate::engine::positions::{ClosedPosition, ExitReason};

/// Compile-time constant strategy tag — one backrunner, one tag, zero allocation.
const STRATEGY_TAG: &str = "backrun";

/// Convert ride_phase u8 to a human-readable string.
fn ride_phase_name(phase: u8) -> &'static str {
    match phase {
        0 => "n/a",
        1 => "early",
        2 => "momentum",
        3 => "tighten",
        _ => "unknown",
    }
}

/// Appends paper trade results as JSONL (one JSON object per line).
/// Output schema matches the TS paper-trade-logger.ts exactly (camelCase keys).
pub struct PaperTradeLogger {
    file: std::fs::File,
    path: String,
    paper_mode: bool,
    config_version: String,
}

impl PaperTradeLogger {
    /// Create a new logger, opening (or creating) the file at `path` in append mode.
    pub fn new(
        path: &str,
        paper_mode: bool,
        config_version: String,
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
        })
    }

    /// Append a closed position as one JSON line with camelCase keys
    /// matching the TS `paper-trade-logger.ts` schema.
    ///
    /// `entry_mint_b58` is the base58-encoded mint address string.
    #[inline(always)]
    pub fn log(&mut self, pos: &ClosedPosition, entry_mint_b58: &str) -> Result<()> {
        let exit_str = match pos.exit_reason {
            ExitReason::TakeProfit => "take_profit",
            ExitReason::StopLoss => "stop_loss",
            ExitReason::NextBuyer => "next_buyer",
            ExitReason::MaxHold => "max_hold",
            ExitReason::IntraHoldTrail => "intra_hold_trail",
            ExitReason::MomentumDecayFlat => "momentum_decay_flat",
            ExitReason::MomentumDecayFade => "momentum_decay_fade",
            ExitReason::TakeProfitScaled => "take_profit_scaled",
            ExitReason::MomentumStall => "momentum_stall",
            ExitReason::RideTrailingStop  => "ride_trailing_stop",
            ExitReason::RideHardFloor     => "ride_hard_floor",
            ExitReason::RideWhaleExit     => "ride_whale_exit",
            ExitReason::RideBuyGapTimeout => "ride_buy_gap_timeout",
            ExitReason::RideSellCascade   => "ride_sell_cascade",
            ExitReason::RideCreatorSell   => "ride_creator_sell",
            ExitReason::RideMaxHold       => "ride_max_hold",
            ExitReason::RideSignalExit    => "ride_signal_exit",
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
            // RIDE mode fields
            "exitMode": pos.exit_mode_str,
            "ridePhase": ride_phase_name(pos.ride_phase),
            "ridePeakMvsol": pos.ride_peak_mvsol,
            "rideHoldMs": pos.ride_hold_ms,
            "rideUniqueWallets": pos.ride_unique_wallets,
            // V2 EntryEngine fields (Kelly sizing + magnitude prediction)
            "magnitudeScore": pos.magnitude_estimate,
            "kellySizeLamports": pos.kelly_size_lamports,
            "entryAction": pos.entry_action,
            "confirmingBuySol": pos.confirming_buy_sol as f64 / 1_000_000_000.0,
            "sellsDuringHold": pos.sells_during_hold,
            // Signal v2 fields (RideState composite scoring)
            "signalScoreAtExit": pos.signal_score_at_exit,
            "signalStateAtExit": pos.signal_state_at_exit,
            "peakSignalScore": pos.peak_signal_score,
            "uniqueWalletsSeen": pos.unique_wallets_seen,
            // Strategy classification — single backrunner, compile-time constant
            "strategyTag": STRATEGY_TAG,
            // Metadata
            "engineVersion": "v5-rust",
            "configVersion": self.config_version,
            "dataVersion": 6,
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
