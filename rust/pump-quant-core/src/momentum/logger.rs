//! JSONL paper trade logger for momentum engine positions.
//!
//! Uses a dedicated writer thread with `crossbeam_channel` (bounded 1024)
//! and `BufWriter<File>` with explicit flush per record — same pattern
//! as `persistence/grad_arb_logger.rs`.

use crossbeam_channel::{bounded, Sender};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};

/// A completed momentum position, ready for JSONL serialization.
#[derive(Debug, serde::Serialize)]
pub struct MomentumClosedPosition {
    /// Strategy identifier — always "momentum".
    pub strategy_tag: &'static str,
    /// Token mint address (base58).
    pub mint: String,
    /// Pool type string (e.g. "raydium_amm_v4", "pump_swap").
    pub pool_type: &'static str,
    /// Graduation score at entry (0-100).
    pub grad_score: u8,
    /// Final grad score at close — may differ from entry if recovery enriched.
    pub grad_score_final: u8,
    /// Time from token creation to graduation (seconds).
    pub grad_speed_s: u64,
    /// Total SOL volume during bonding curve phase.
    pub grad_volume_sol: f64,
    /// Number of buys in the 5 seconds before graduation.
    pub pre_grad_buys_5s: u32,
    /// Position size in SOL (always populated, never null).
    pub size_sol: f64,
    /// Position size in lamports.
    pub size_lamports: u64,
    /// Delay between graduation and entry (ms).
    pub entry_delay_ms: u64,
    /// Entry price in lamports per 1,000,000 token atoms (fp unit).
    pub entry_price_lamports: u64,
    /// Exit price in lamports per 1,000,000 token atoms (fp unit, same as entry_price_lamports).
    pub exit_price_lamports: u64,
    /// Bonding curve terminal price in fp units (lamports per 1,000,000 token atoms).
    pub bc_terminal_price_fp: u64,
    /// Structural discount at entry vs BC terminal (%).
    /// Positive = entry above terminal (token pumping), negative = entry below.
    pub structural_discount_pct: f64,
    /// Entry timestamp (unix ms).
    pub entry_timestamp_ms: u64,
    /// Exit timestamp (unix ms).
    pub exit_timestamp_ms: u64,
    /// Hold duration (ms).
    pub hold_ms: u64,
    /// Exit reason string.
    pub exit_reason: &'static str,
    /// Raw gain in bps before sanity clamp.
    pub raw_gain_bps: i32,
    /// Gross PnL in SOL (before fees).
    pub gross_pnl_sol: f64,
    /// Total fees in SOL.
    pub fee_sol: f64,
    /// Total fees in SOL (alias for analysis scripts that expect plural name).
    pub fees_sol: f64,
    /// Net PnL in SOL (after fees).
    pub net_pnl_sol: f64,
    /// Price samples as bps offset from entry price.
    pub price_samples_bps: Vec<i32>,
    /// Number of price samples recorded at close time.
    pub price_sample_count: u8,
    /// WS notification count from price feed at close time.
    pub ws_notif_count_at_close: u64,
    /// Whether this was a paper trade.
    pub is_paper: bool,
    /// Config version string for offline analysis.
    pub config_version: String,
}

/// Async-safe paper logger for momentum positions.
///
/// Sends `MomentumClosedPosition` records through a bounded crossbeam channel
/// to a dedicated writer thread. The writer thread uses `BufWriter<File>` with
/// explicit flush per record — one JSON line per position.
pub struct MomentumPaperLogger {
    sender: Sender<MomentumClosedPosition>,
}

impl MomentumPaperLogger {
    /// Create a new logger writing JSONL to `log_path`.
    ///
    /// Returns the logger and a join handle for the dedicated writer thread.
    /// The writer thread exits cleanly when all senders are dropped (channel disconnected).
    pub fn new(log_path: &str) -> (Self, std::thread::JoinHandle<()>) {
        let (sender, receiver) = bounded::<MomentumClosedPosition>(1024);

        let path = log_path.to_string();
        let handle = std::thread::Builder::new()
            .name("momentum-logger".into())
            .spawn(move || {
                let file = match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("momentum logger: failed to open {}: {}", path, e);
                        return;
                    }
                };
                let mut writer = BufWriter::new(file);

                while let Ok(position) = receiver.recv() {
                    match serde_json::to_string(&position) {
                        Ok(mut line) => {
                            line.push('\n');
                            if let Err(e) = writer.write_all(line.as_bytes()) {
                                tracing::error!("momentum logger: write failed: {}", e);
                                continue;
                            }
                            if let Err(e) = writer.flush() {
                                tracing::error!("momentum logger: flush failed: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("momentum logger: serialize failed: {}", e);
                        }
                    }
                }

                // Channel disconnected — flush and exit
                let _ = writer.flush();
                tracing::info!("momentum logger thread exiting");
            })
            .expect("failed to spawn momentum logger thread");

        (Self { sender }, handle)
    }

    /// Send a closed position record to the writer thread.
    ///
    /// Non-blocking: uses `try_send` — drops the record if the channel is full
    /// (1024 pending records). This should never happen in practice since
    /// graduations are rare (~10/day).
    #[inline(always)]
    pub fn log(&self, position: MomentumClosedPosition) {
        let _ = self.sender.try_send(position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn make_test_closed_position() -> MomentumClosedPosition {
        MomentumClosedPosition {
            strategy_tag: "momentum",
            mint: "7mHCxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_string(),
            pool_type: "raydium_amm_v4",
            grad_score: 72,
            grad_score_final: 72,
            grad_speed_s: 45,
            grad_volume_sol: 87.3,
            pre_grad_buys_5s: 12,
            size_sol: 0.3,
            size_lamports: 300_000_000,
            entry_delay_ms: 15000,
            entry_price_lamports: 381_900,
            exit_price_lamports: 420_000,
            bc_terminal_price_fp: 410,
            structural_discount_pct: 7.07,
            entry_timestamp_ms: 1_711_700_000_000,
            exit_timestamp_ms: 1_711_700_023_400,
            hold_ms: 23_400,
            exit_reason: "tp2",
            raw_gain_bps: 997,
            gross_pnl_sol: 0.045,
            fee_sol: 0.0015,
            fees_sol: 0.0015,
            net_pnl_sol: 0.0435,
            price_samples_bps: vec![0, 250, 800, 1200, 900],
            price_sample_count: 5,
            ws_notif_count_at_close: 12,
            is_paper: true,
            config_version: "mom-v0.30sol_15000ms".to_string(),
        }
    }

    #[test]
    fn test_momentum_closed_position_serializes() {
        let pos = make_test_closed_position();
        let json = serde_json::to_string(&pos).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["strategy_tag"], "momentum");
        assert_eq!(parsed["grad_score"], 72);
        assert_eq!(parsed["hold_ms"], 23_400);
        assert_eq!(parsed["exit_reason"], "tp2");
        assert_eq!(parsed["is_paper"], true);

        // price_samples_bps should be an array of 5 elements
        let samples = parsed["price_samples_bps"].as_array().unwrap();
        assert_eq!(samples.len(), 5);
        assert_eq!(samples[0], 0);
        assert_eq!(samples[3], 1200);
    }

    #[test]
    fn test_momentum_logger_sends_record() {
        let dir = std::env::temp_dir();
        let path = dir
            .join(format!("momentum_test_{}.jsonl", std::process::id()))
            .to_string_lossy()
            .to_string();

        let (logger, handle) = MomentumPaperLogger::new(&path);

        let pos = make_test_closed_position();
        logger.log(pos);

        // Drop sender to signal the writer thread to exit
        drop(logger);
        handle.join().expect("logger thread panicked");

        // Read back and verify
        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["strategy_tag"], "momentum");
        assert_eq!(parsed["mint"], "7mHCxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        assert_eq!(parsed["grad_score"], 72);
        assert_eq!(parsed["exit_reason"], "tp2");
        assert!((parsed["net_pnl_sol"].as_f64().unwrap() - 0.0435).abs() < 1e-6);

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
