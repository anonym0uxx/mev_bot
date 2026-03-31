//! JSONL paper trade logger for graduation arbitrage positions.
//!
//! Appends one JSON line per closed graduation arb position to a file.
//! Thread model: called from the logger thread (separate from engine thread).
//! Performance: uses `BufWriter<File>` with explicit flush per record.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};

use anyhow::{Context, Result};
use serde_json::json;

use crate::arb::graduation::GradArbClosedPosition;

/// Compile-time constant strategy tag for graduation arb trades.
const STRATEGY_TAG: &str = "graduation_arb";

/// Compile-time engine version identifier.
const ENGINE_VERSION: &str = "grad-v1";

/// Conversion factor: lamports to SOL.
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Appends closed graduation arb positions as JSONL (one JSON object per line).
///
/// Output schema matches ARCHITECTURE_V2.md Section 3.5 with graduation arb
/// specific fields (poolType, detectionSource, spreadPct, etc.).
pub struct GradArbPaperLogger {
    writer: BufWriter<std::fs::File>,
    path: String,
    config_version: String,
}

impl GradArbPaperLogger {
    /// Create a new logger, opening (or creating) the file at `path` in append mode.
    ///
    /// `config_version` is embedded in every record for offline analysis
    /// (e.g. `"grad-v0.30sol_5000ms"`).
    pub fn new(path: &str, config_version: String) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open grad arb paper log: {path}"))?;

        tracing::info!(
            config_version = %config_version,
            "GradArbPaperLogger writing to: {path}"
        );

        Ok(Self {
            writer: BufWriter::new(file),
            path: path.to_string(),
            config_version,
        })
    }

    /// Append a closed graduation arb position as one JSON line.
    ///
    /// `mint_b58` is the pre-encoded base58 mint address string.
    #[inline(always)]
    pub fn log(&mut self, cp: &GradArbClosedPosition, mint_b58: &str) -> Result<()> {
        let pool_type_str = cp.pool_type.as_str();
        let detection_source_str = cp.detection_source.as_str();
        let exit_reason_str = cp.exit_reason.as_str();

        // Convert lamports to SOL for human-readable output
        let entry_size_sol = cp.size_lamports as f64 / LAMPORTS_PER_SOL;
        let pnl_sol = cp.pnl_lamports as f64 / LAMPORTS_PER_SOL;
        let net_pnl_sol = cp.net_pnl_lamports as f64 / LAMPORTS_PER_SOL;
        let mfe_sol = cp.mfe_lamports as f64 / LAMPORTS_PER_SOL;
        let mae_sol = cp.mae_lamports as f64 / LAMPORTS_PER_SOL;

        // Cap spread at 50% in output — values above this are bad price data.
        // Use -1.0 to signal "invalid/uncalculable" for downstream analysis.
        let spread_pct_capped = if !cp.spread_pct.is_finite() || cp.spread_pct > 50.0 {
            -1.0
        } else {
            cp.spread_pct
        };

        let line = json!({
            "mint": mint_b58,
            "engineVersion": ENGINE_VERSION,
            "strategyTag": STRATEGY_TAG,
            "poolType": pool_type_str,
            "detectionSource": detection_source_str,
            "detectionLatencyMs": cp.detection_latency_ms,
            "spreadPct": spread_pct_capped,
            "bcTerminalPrice": cp.bc_terminal_price,
            "rayOpeningPrice": cp.ray_opening_price,
            "entrySizeSol": entry_size_sol,
            "entryTimestampMs": cp.entry_ts_ms,
            "exitTimestampMs": cp.exit_ts_ms,
            "exitReason": exit_reason_str,
            "holdMs": cp.hold_ms,
            "pnlSol": pnl_sol,
            "netPnlSol": net_pnl_sol,
            "mfeSol": mfe_sol,
            "maeSol": mae_sol,
            "configVersion": self.config_version,
        });

        let mut line_str =
            serde_json::to_string(&line).context("failed to serialize grad arb trade to JSON")?;
        line_str.push('\n');

        self.writer
            .write_all(line_str.as_bytes())
            .with_context(|| format!("failed to write to grad arb paper log: {}", self.path))?;
        self.writer
            .flush()
            .with_context(|| format!("failed to flush grad arb paper log: {}", self.path))?;

        Ok(())
    }

    /// Get the path of the log file.
    pub fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb::graduation::{GradArbClosedPosition, GradArbExitReason, PoolType};
    use crate::feeds::MigrationSource;
    use std::io::Read;

    fn make_test_closed_position() -> GradArbClosedPosition {
        GradArbClosedPosition {
            mint: [0xAA; 32],
            pool_address: [0xBB; 32],
            pool_type: PoolType::RaydiumAmmV4,
            entry_price_lamports: 1_000_000,
            exit_price_lamports: 1_030_000,
            size_lamports: 300_000_000, // 0.3 SOL
            bc_terminal_price: 0.000001234,
            ray_opening_price: 0.000001176,
            spread_pct: 4.7,
            detection_source: MigrationSource::HeliusLogs,
            detection_latency_ms: 82,
            entry_ts_ms: 1_700_000_000_000,
            exit_ts_ms: 1_700_000_001_240,
            hold_ms: 1_240,
            exit_reason: GradArbExitReason::TakeProfit,
            pnl_lamports: 12_000_000,
            fee_lamports: 4_000_000,
            net_pnl_lamports: 8_000_000,
            mfe_lamports: 14_000_000,
            mae_lamports: 2_000_000,
        }
    }

    #[test]
    fn grad_arb_logger_writes_valid_json() {
        let dir = std::env::temp_dir();
        let path = dir
            .join(format!("grad_arb_test_{}.jsonl", std::process::id()))
            .to_string_lossy()
            .to_string();

        let config_version = "grad-v0.30sol_5000ms".to_string();
        let mut logger = GradArbPaperLogger::new(&path, config_version.clone()).unwrap();

        let cp = make_test_closed_position();
        let mint_b58 = bs58::encode(&cp.mint).into_string();
        logger.log(&cp, &mint_b58).unwrap();

        // Read back and parse
        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        let line = contents.lines().next().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();

        // Assert all fields present and correct
        assert_eq!(parsed["mint"], mint_b58);
        assert_eq!(parsed["engineVersion"], "grad-v1");
        assert_eq!(parsed["strategyTag"], "graduation_arb");
        assert_eq!(parsed["poolType"], "raydium_amm_v4");
        assert_eq!(parsed["detectionSource"], "helius");
        assert_eq!(parsed["detectionLatencyMs"], 82);
        assert_eq!(parsed["spreadPct"], 4.7);
        assert_eq!(parsed["bcTerminalPrice"], 0.000001234);
        assert_eq!(parsed["rayOpeningPrice"], 0.000001176);
        assert_eq!(parsed["entrySizeSol"], 0.3);
        assert_eq!(parsed["entryTimestampMs"], 1_700_000_000_000u64);
        assert_eq!(parsed["exitTimestampMs"], 1_700_000_001_240u64);
        assert_eq!(parsed["exitReason"], "take_profit");
        assert_eq!(parsed["holdMs"], 1_240);
        assert_eq!(parsed["pnlSol"], 0.012);
        assert_eq!(parsed["netPnlSol"], 0.008);
        assert_eq!(parsed["mfeSol"], 0.014);
        assert_eq!(parsed["maeSol"], 0.002);
        assert_eq!(parsed["configVersion"], "grad-v0.30sol_5000ms");

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn grad_arb_logger_appends_multiple_records() {
        let dir = std::env::temp_dir();
        let path = dir
            .join(format!("grad_arb_multi_{}.jsonl", std::process::id()))
            .to_string_lossy()
            .to_string();

        let mut logger =
            GradArbPaperLogger::new(&path, "grad-v0.30sol_5000ms".to_string()).unwrap();

        for i in 0..3 {
            let mut cp = make_test_closed_position();
            cp.mint[0] = i;
            cp.hold_ms = 1000 + i as u64 * 100;
            let mint_b58 = bs58::encode(&cp.mint).into_string();
            logger.log(&cp, &mint_b58).unwrap();
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);

        // Each line should be valid JSON
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["strategyTag"], "graduation_arb");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn grad_arb_logger_pump_swap_pool_type() {
        let dir = std::env::temp_dir();
        let path = dir
            .join(format!("grad_arb_ps_{}.jsonl", std::process::id()))
            .to_string_lossy()
            .to_string();

        let mut logger =
            GradArbPaperLogger::new(&path, "grad-v0.30sol_5000ms".to_string()).unwrap();

        let mut cp = make_test_closed_position();
        cp.pool_type = PoolType::PumpSwap;
        cp.detection_source = MigrationSource::CoreCastStream2;
        cp.exit_reason = GradArbExitReason::StopLoss;

        let mint_b58 = bs58::encode(&cp.mint).into_string();
        logger.log(&cp, &mint_b58).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(parsed["poolType"], "pump_swap");
        assert_eq!(parsed["detectionSource"], "corecast");
        assert_eq!(parsed["exitReason"], "stop_loss");

        let _ = std::fs::remove_file(&path);
    }
}
