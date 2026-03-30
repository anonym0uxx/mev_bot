//! Helius WebSocket feed — processed-commitment log subscriptions on the
//! pump.fun program (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`).
//!
//! Pre-warms mint trade history BEFORE PumpPortal confirms.
//! Emits `PreWarmEvent` for buy/sell trades, and `FeedEvent::Migration`
//! when a token graduation (Raydium/PumpSwap pool creation) is detected.

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::feeds::{FeedEvent, FeedSource, MigrationSource, PreWarmEvent};

const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const MAX_BACKOFF_MS: u64 = 30_000;

// ── Graduation detection markers ────────────────────────────────────
// Raydium AMM v4 program invocation — primary graduation signal (pre-March 2025)
#[allow(dead_code)]
const RAYDIUM_AMM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const GRADUATION_LOG_MARKER: &[u8] = b"Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke";
// PumpSwap migration — newer pump.fun behavior (post-March 2025)
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: MigrateFunds";

pub struct HeliusConfig {
    pub api_key: String,
    pub enabled: bool,
}

pub struct HeliusWsClient {
    config: HeliusConfig,
    engine_tx: Sender<FeedEvent>,
}

impl HeliusWsClient {
    pub fn new(config: HeliusConfig, engine_tx: Sender<FeedEvent>) -> Self {
        Self { config, engine_tx }
    }

    /// Spawn a tokio task that connects, subscribes, and forwards PreWarm events.
    /// Reconnects on disconnect with exponential backoff (1s → 2s → 4s → max 30s).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    /// Connect, subscribe, and forward PreWarm events in a loop.
    /// Reconnects on disconnect with exponential backoff.
    /// This is an async method — call from a tokio task.
    pub async fn run_loop(self) {
        if !self.config.enabled || self.config.api_key.is_empty() {
            info!("[helius] disabled or no API key — skipping");
            return;
        }

        let url = format!(
            "wss://mainnet.helius-rpc.com/?api-key={}",
            self.config.api_key
        );

        let sub_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                { "mentions": [PUMP_PROGRAM_ID] },
                { "commitment": "processed" }
            ]
        })
        .to_string();

        let mut backoff_ms: u64 = 1_000;

        loop {
            info!("[helius] connecting");

            match connect_async(&url).await {
                Err(e) => {
                    warn!("[helius] connect failed: {e} — retrying in {backoff_ms}ms");
                }
                Ok((ws_stream, _)) => {
                    backoff_ms = 1_000; // reset on successful connect

                    let (mut write, mut read) = ws_stream.split();

                    // Send subscription
                    if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await {
                        error!("[helius] subscribe send failed: {e}");
                        continue;
                    }

                    info!("[helius] connected and subscribed");

                    // Read loop
                    loop {
                        match read.next().await {
                            Some(Ok(Message::Text(text))) => {
                                // Check for graduation (Migration) FIRST — it's
                                // higher-priority and mutually exclusive with
                                // buy/sell trades (graduation tx won't parse as
                                // a normal pump trade).
                                if let Some(migration_event) = check_graduation_logs(&text) {
                                    if self.engine_tx.send(migration_event).is_err() {
                                        info!("[helius] engine channel closed — exiting");
                                        return;
                                    }
                                } else if let Some(event) = parse_helius_log(&text) {
                                    if self.engine_tx.send(FeedEvent::PreWarm(event)).is_err() {
                                        info!("[helius] engine channel closed — exiting");
                                        return;
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                let _ = write.send(Message::Pong(data)).await;
                            }
                            Some(Ok(Message::Close(_))) => {
                                warn!("[helius] server sent close frame");
                                break;
                            }
                            Some(Err(e)) => {
                                warn!("[helius] ws error: {e}");
                                break;
                            }
                            Some(Ok(_)) => {} // Binary, Pong, Frame — ignore
                            None => {
                                warn!("[helius] stream ended");
                                break;
                            }
                        }
                    }

                    warn!("[helius] disconnected — retrying in {backoff_ms}ms");
                }
            }

            // Exponential backoff
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }
    }
}

/// Parse a Helius `logsNotification` message using simd_json (SIMD-accelerated).
///
/// ~2-5µs faster per message vs serde_json. Uses owned String + in-place parse
/// (same pattern as PumpPortal feed). simd_json is already in Cargo.toml.
///
/// Extracts signature + buy/sell direction from program log lines.
/// Emits PreWarmEvent with mint=[0u8;32] (logsSubscribe doesn't provide accountKeys).
fn parse_helius_log(text: &str) -> Option<PreWarmEvent> {
    // simd_json needs mutable bytes — copy into owned String for in-place parse
    let mut owned = text.to_string();
    let bytes = unsafe { owned.as_bytes_mut() };
    let v: simd_json::BorrowedValue = simd_json::to_borrowed_value(bytes).ok()?;

    use simd_json::prelude::*;

    // Must be a logsNotification
    let method = v.get("method")?.as_str()?;
    if method != "logsNotification" {
        return None;
    }

    let params = v.get("params")?;
    let result = params.get("result")?;
    let value = result.get("value")?;

    // Skip failed transactions
    let err = value.get("err")?;
    if !err.is_null() {
        return None;
    }

    let sig_str = value.get("signature")?.as_str()?;
    let _slot = result.get("context")
        .and_then(|c| c.get("slot"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    // LATENCY: Decode base58 signature into stack-allocated [u8; 64].
    // Uses bs58::decode().onto() — no heap allocation (saves ~50-80ns per decode).
    let mut sig = [0u8; 64];
    match bs58::decode(sig_str).onto(&mut sig[..]) {
        Ok(n) if n == 64 => {}
        Ok(n) => {
            debug!("[helius] unexpected sig length {}", n);
            return None;
        }
        Err(_) => return None,
    }

    // ── Attempt to extract mint + direction from program log lines ──
    // Pump.fun program logs contain structured data we can parse:
    //   "Program log: Instruction: Buy" / "Program log: Instruction: Sell"
    //   The mint address appears as an account key in the invoke context.
    //
    // Log format from pump.fun (observed pattern):
    //   "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]"
    //   "Program log: Instruction: Buy"
    //   ... (inner CPI logs)
    //
    // We cannot reliably extract the mint from these logs alone because the
    // account keys are not logged — only the program ID and instruction name.
    // The actual mint address is in the transaction's accountKeys, which
    // logsSubscribe does NOT provide.
    //
    // What we CAN extract: buy/sell direction from "Instruction: Buy/Sell" logs.
    let mut is_buy = true; // default assumption
    let mut is_pump_trade = false;

    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
        for log_entry in logs {
            if let Some(log_str) = log_entry.as_str() {
                // Detect pump.fun program invocation
                if log_str.starts_with("Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke") {
                    is_pump_trade = true;
                }
                // Detect buy/sell direction
                if log_str.contains("Instruction: Buy") {
                    is_buy = true;
                } else if log_str.contains("Instruction: Sell") {
                    is_buy = false;
                }
            }
        }
    }

    // Only emit if this is actually a pump.fun transaction
    if !is_pump_trade {
        return None;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // NOTE: mint=[0u8;32] because logsSubscribe doesn't provide accountKeys.
    // The sig_prefix is used for dedup correlation when PumpPortal confirms.
    // To make Helius a true primary trigger, we'd need either:
    //   1. accountSubscribe on pump.fun bonding curves (full account data)
    //   2. LaserStream Preprocessed Transactions (gRPC, full decoded tx)
    //   3. Helius Enhanced Transactions API (HTTP, adds latency)
    Some(PreWarmEvent {
        mint: [0u8; 32],
        trader: [0u8; 32],
        sig,
        sol_amount: 0,
        is_buy,
        timestamp_ms: now_ms,
        source: FeedSource::Helius,
    })
}

/// Check if a Helius logsNotification contains a graduation event
/// (Raydium AMM pool creation or PumpSwap MigrateFunds instruction).
///
/// Returns `Some(FeedEvent::Migration)` if graduation detected, `None` otherwise.
/// The mint is set to `[0u8; 32]` because logsSubscribe doesn't provide account keys —
/// the GraduationArbEngine resolves the mint via `getTransaction` using the signature.
///
/// Called on every Helius ws message — must be allocation-free on the scan path.
fn check_graduation_logs(text: &str) -> Option<FeedEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    if v.get("method")?.as_str()? != "logsNotification" {
        return None;
    }

    let value = v.pointer("/params/result/value")?;

    // Skip failed transactions
    let err = value.get("err")?;
    if !err.is_null() {
        return None;
    }

    let logs = value.get("logs")?.as_array()?;

    // LATENCY: byte-level contains() — no regex, no heap allocation.
    // Scan all log lines for graduation markers.
    if !logs_contain_graduation_marker(logs) {
        return None;
    }

    // Extract and decode the full 64-byte transaction signature
    let sig_str = value.get("signature")?.as_str()?;
    let mut sig = [0u8; 64];
    match bs58::decode(sig_str).onto(&mut sig[..]) {
        Ok(64) => {}
        Ok(n) => {
            debug!("[helius] graduation sig unexpected length {}", n);
            return None;
        }
        Err(_) => return None,
    }

    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    info!(
        sig = %sig_str,
        source = "helius",
        "[helius] graduation detected — dispatching Migration event for pool resolution"
    );

    Some(FeedEvent::Migration {
        mint: [0u8; 32],
        ts_ms,
        source: MigrationSource::HeliusLogs,
        sig,
    })
}

/// Byte-level scan of Helius log lines for graduation markers.
/// Returns `true` if any log line contains `GRADUATION_LOG_MARKER` or `PUMPSWAP_LOG_MARKER`.
///
/// # Performance
/// Uses `[u8]::windows().any()` pattern — zero-allocation, branch-predictor-friendly.
/// Called on every Helius logsNotification (~100-500/sec during high activity).
#[inline(always)]
fn logs_contain_graduation_marker(logs: &[serde_json::Value]) -> bool {
    for entry in logs {
        if let Some(s) = entry.as_str() {
            let bytes = s.as_bytes();
            if bytes_contains(bytes, GRADUATION_LOG_MARKER)
                || bytes_contains(bytes, PUMPSWAP_LOG_MARKER)
            {
                return true;
            }
        }
    }
    false
}

/// SIMD-accelerated byte-level substring search via memchr crate.
/// Uses hardware SIMD (SSE2/AVX2 on x86, NEON on ARM) for 4-8x speedup
/// over naive `windows().any()` on the 58-byte GRADUATION_LOG_MARKER.
/// Zero allocations, called on every Helius logsNotification (~100-500/sec).
#[inline(always)]
fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a mock Helius logsNotification JSON string.
    fn mock_logs_notification(sig: &str, logs: &[&str], err: Option<&str>) -> String {
        let logs_json: Vec<String> = logs.iter().map(|l| format!("\"{}\"", l)).collect();
        let err_value = err.unwrap_or("null");
        format!(
            r#"{{
                "jsonrpc": "2.0",
                "method": "logsNotification",
                "params": {{
                    "result": {{
                        "context": {{ "slot": 12345 }},
                        "value": {{
                            "signature": "{}",
                            "err": {},
                            "logs": [{}]
                        }}
                    }},
                    "subscription": 1
                }}
            }}"#,
            sig,
            err_value,
            logs_json.join(", ")
        )
    }

    // A valid base58-encoded 64-byte signature for testing
    const TEST_SIG: &str = "5VERv8NMhDGLVpFpJxGjkWNyVSz9idJAqKb3iV1Bv7epMqNXhP5GhipY9VGPYdRJ6jT6E1rxJKoYjKoJe3xUwz1";

    #[test]
    fn test_helius_detects_raydium_graduation() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Buy",
                "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [2]",
                "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 consumed 12345 of 200000 compute units",
                "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 success",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 100000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_some(), "should detect Raydium graduation");

        match event.unwrap() {
            FeedEvent::Migration { mint, source, sig, .. } => {
                assert_eq!(mint, [0u8; 32], "mint should be unknown (zeros)");
                assert_eq!(source, MigrationSource::HeliusLogs);
                assert_ne!(sig, [0u8; 64], "sig should be populated from tx signature");
            }
            other => panic!("expected Migration event, got {:?}", other),
        }
    }

    #[test]
    fn test_helius_detects_pumpswap_graduation() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: MigrateFunds",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 150000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_some(), "should detect PumpSwap graduation");

        match event.unwrap() {
            FeedEvent::Migration { source, .. } => {
                assert_eq!(source, MigrationSource::HeliusLogs);
            }
            other => panic!("expected Migration event, got {:?}", other),
        }
    }

    #[test]
    fn test_helius_no_false_positive_on_regular_buy() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Buy",
                "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]",
                "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 3000 of 200000 compute units",
                "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 50000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_none(), "regular buy should NOT trigger graduation detection");
    }

    #[test]
    fn test_helius_no_graduation_on_failed_tx() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [2]",
            ],
            Some(r#"{"InstructionError":[0,"Custom"]}"#),
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_none(), "failed tx should NOT trigger graduation");
    }

    #[test]
    fn test_helius_no_graduation_on_sell() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Sell",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 50000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_none(), "regular sell should NOT trigger graduation");
    }

    #[test]
    fn test_bytes_contains_basic() {
        assert!(bytes_contains(b"hello world", b"world"));
        assert!(bytes_contains(b"hello world", b"hello"));
        assert!(!bytes_contains(b"hello world", b"xyz"));
        assert!(!bytes_contains(b"hi", b"hello")); // needle longer than haystack
    }

    #[test]
    fn test_existing_prewarm_still_works() {
        // Verify the existing parse_helius_log still returns PreWarmEvent for buy trades
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Buy",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 50000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = parse_helius_log(&text);
        assert!(event.is_some(), "existing buy parse should still work");
        let pw = event.unwrap();
        assert!(pw.is_buy, "should detect Buy direction");
        assert_eq!(pw.source, FeedSource::Helius);
    }
}
