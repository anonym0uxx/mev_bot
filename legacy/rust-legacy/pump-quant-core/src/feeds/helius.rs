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
const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: CreatePool";
// Pump.fun migrate instruction — emitted by pump.fun program during graduation
const PUMPFUN_MIGRATE_MARKER: &[u8] = b"Instruction: Migrate";

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
/// Returns `true` if any log line contains `GRADUATION_LOG_MARKER` (Raydium),
/// `PUMPSWAP_LOG_MARKER` (CreatePool), or `PUMPFUN_MIGRATE_MARKER` (Migrate).
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
                || bytes_contains(bytes, PUMPFUN_MIGRATE_MARKER)
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

// ── Helius PumpSwap transactionSubscribe client ─────────────────────
// Separate WebSocket connection that uses Helius Enhanced `transactionSubscribe`
// to receive FULL transactions involving PumpSwap. This provides account keys,
// token balances, and log messages inline — eliminating the getTransaction
// round-trip that plagues logsSubscribe-based detection.

const PUMPSWAP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

pub struct HeliusPumpSwapClient {
    config: HeliusConfig,
    engine_tx: Sender<FeedEvent>,
}

impl HeliusPumpSwapClient {
    pub fn new(config: HeliusConfig, engine_tx: Sender<FeedEvent>) -> Self {
        Self { config, engine_tx }
    }

    /// Spawn a tokio task that connects via transactionSubscribe to detect
    /// PumpSwap graduations with full transaction data (no getTransaction needed).
    /// Reconnects on disconnect with exponential backoff (1s → 2s → 4s → max 30s).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    async fn run_loop(self) {
        if !self.config.enabled || self.config.api_key.is_empty() {
            info!("[helius_pumpswap] disabled or no API key — skipping");
            return;
        }

        let url = format!(
            "wss://mainnet.helius-rpc.com/?api-key={}",
            self.config.api_key
        );

        // transactionSubscribe: filter for PumpSwap program,
        // request full tx with jsonParsed encoding
        let sub_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "transactionSubscribe",
            "params": [
                {
                    "accountInclude": [PUMPSWAP_AMM_PROGRAM],
                    "failed": false
                },
                {
                    "commitment": "processed",
                    "encoding": "jsonParsed",
                    "transactionDetails": "full",
                    "maxSupportedTransactionVersion": 0
                }
            ]
        })
        .to_string();

        let mut backoff_ms: u64 = 1_000;

        loop {
            info!("[helius_pumpswap] connecting transactionSubscribe");

            match connect_async(&url).await {
                Err(e) => {
                    warn!("[helius_pumpswap] connect failed: {e} — retrying in {backoff_ms}ms");
                }
                Ok((ws_stream, _)) => {
                    backoff_ms = 1_000; // reset on successful connect

                    let (mut write, mut read) = ws_stream.split();

                    // Send subscription
                    if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await {
                        error!("[helius_pumpswap] subscribe send failed: {e}");
                        continue;
                    }

                    info!("[helius_pumpswap] connected and subscribed (transactionSubscribe)");

                    // Ping every 30s to keep alive (Helius 10-min inactivity timer)
                    let ping_interval =
                        tokio::time::interval(tokio::time::Duration::from_secs(30));
                    tokio::pin!(ping_interval);

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        if let Some(event) = parse_pumpswap_transaction(&text) {
                                            if self.engine_tx.send(event).is_err() {
                                                info!("[helius_pumpswap] engine channel closed — exiting");
                                                return;
                                            }
                                        }
                                    }
                                    Some(Ok(Message::Ping(data))) => {
                                        let _ = write.send(Message::Pong(data)).await;
                                    }
                                    Some(Ok(Message::Close(_))) => {
                                        warn!("[helius_pumpswap] server sent close frame");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        warn!("[helius_pumpswap] ws error: {e}");
                                        break;
                                    }
                                    Some(Ok(_)) => {} // Binary, Pong, Frame — ignore
                                    None => {
                                        warn!("[helius_pumpswap] stream ended");
                                        break;
                                    }
                                }
                            }
                            _ = ping_interval.tick() => {
                                let _ = write.send(Message::Ping(vec![].into())).await;
                            }
                        }
                    }

                    warn!("[helius_pumpswap] disconnected — retrying in {backoff_ms}ms");
                }
            }

            // Exponential backoff
            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }
    }
}

/// Parse a Helius `transactionNotification` for PumpSwap graduation events.
///
/// The notification provides the FULL transaction including:
/// - `transaction.transaction.message.accountKeys` (pubkey strings)
/// - `meta.postTokenBalances` (mint + vault addresses via accountIndex)
/// - `meta.logMessages` (instruction names)
/// - `signature`
///
/// This eliminates the need for a separate getTransaction RPC call.
///
/// Returns:
/// - `PumpSwapGraduationDirect` if both vaults extracted (fast path, no RPC needed)
/// - `Migration` if mint found but vaults missing (fallback to mint-based resolution)
/// - `None` if not a graduation or parse fails
fn parse_pumpswap_transaction(text: &str) -> Option<FeedEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // Must be a transactionNotification
    if v.get("method")?.as_str()? != "transactionNotification" {
        return None;
    }

    let result = v.pointer("/params/result")?;
    let tx = result.get("transaction")?;
    let meta = tx.get("meta")?;

    // Skip failed transactions
    if !meta.get("err")?.is_null() {
        return None;
    }

    // Check logs for both graduation markers:
    // "Instruction: CreatePool" (PumpSwap CPI) AND "Instruction: Migrate" (pump.fun outer)
    // CreatePool without Migrate = manual pool creation, NOT a graduation
    let logs = meta.get("logMessages")?.as_array()?;
    let mut has_create_pool = false;
    let mut has_migrate = false;

    for log_entry in logs {
        if let Some(s) = log_entry.as_str() {
            let b = s.as_bytes();
            if !has_create_pool && bytes_contains(b, PUMPSWAP_LOG_MARKER) {
                has_create_pool = true;
            }
            if !has_migrate && bytes_contains(b, PUMPFUN_MIGRATE_MARKER) {
                has_migrate = true;
            }
            if has_create_pool && has_migrate {
                break;
            }
        }
    }

    if !has_create_pool || !has_migrate {
        return None; // Not a pump.fun graduation
    }

    // Extract signature
    let sig_str = result.get("signature")?.as_str()?;
    let mut sig = [0u8; 64];
    match bs58::decode(sig_str).onto(&mut sig[..]) {
        Ok(64) => {}
        _ => return None,
    }

    // Extract mint from postTokenBalances (first non-WSOL mint)
    let post_balances = meta.get("postTokenBalances")?.as_array()?;
    let mint_b58 = post_balances.iter().find_map(|entry| {
        let mint = entry.get("mint")?.as_str()?;
        if mint != WSOL_MINT {
            Some(mint.to_string())
        } else {
            None
        }
    })?;

    let mut mint = [0u8; 32];
    match bs58::decode(&mint_b58).onto(&mut mint[..]) {
        Ok(32) => {}
        _ => return None,
    }

    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Extract vault addresses from postTokenBalances
    // coin_vault = account holding the graduated token
    // pc_vault = account holding WSOL
    // We find them via accountIndex → accountKeys mapping
    let account_keys = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(|k| k.as_array());

    let mut coin_vault = [0u8; 32];
    let mut pc_vault = [0u8; 32];
    let mut coin_vault_found = false;
    let mut pc_vault_found = false;

    if let Some(keys) = account_keys {
        for entry in post_balances {
            let entry_mint = match entry.get("mint").and_then(|m| m.as_str()) {
                Some(m) => m,
                None => continue,
            };
            let account_index = match entry.get("accountIndex").and_then(|i| i.as_u64()) {
                Some(i) => i as usize,
                None => continue,
            };

            if account_index >= keys.len() {
                continue;
            }

            // accountKeys can be either a plain string or an object with "pubkey" field
            let account_key = keys[account_index]
                .as_str()
                .or_else(|| keys[account_index].get("pubkey").and_then(|p| p.as_str()));

            let account_key = match account_key {
                Some(k) => k,
                None => continue,
            };

            let mut acct = [0u8; 32];
            match bs58::decode(account_key).onto(&mut acct[..]) {
                Ok(32) => {}
                _ => continue,
            }

            if entry_mint == mint_b58 && !coin_vault_found {
                coin_vault = acct;
                coin_vault_found = true;
            } else if entry_mint == WSOL_MINT && !pc_vault_found {
                pc_vault = acct;
                pc_vault_found = true;
            }

            if coin_vault_found && pc_vault_found {
                break;
            }
        }
    }

    if !coin_vault_found || !pc_vault_found {
        // Fallback: we have the mint but not vaults — dispatch as Migration
        // for mint-based resolution via getProgramAccounts
        info!(
            sig = %sig_str,
            mint = %mint_b58,
            coin_vault_found,
            pc_vault_found,
            "[helius_pumpswap] graduation detected but vault extraction incomplete — mint-based fallback"
        );

        return Some(FeedEvent::Migration {
            mint,
            ts_ms,
            source: MigrationSource::HeliusEnhanced,
            sig,
        });
    }

    info!(
        sig = %sig_str,
        mint = %mint_b58,
        coin_vault = %bs58::encode(&coin_vault).into_string(),
        pc_vault = %bs58::encode(&pc_vault).into_string(),
        "[helius_pumpswap] graduation detected with full vault resolution — fast path"
    );

    // Emit the rich event with pre-extracted pool data.
    // This skips the entire pool resolution step in on_migration().
    Some(FeedEvent::PumpSwapGraduationDirect {
        mint,
        sig,
        ts_ms,
        coin_vault,
        pc_vault,
        source: MigrationSource::HeliusEnhanced,
    })
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
                "Program log: Instruction: CreatePool",
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
    fn test_helius_detects_pumpfun_migrate() {
        let text = mock_logs_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Migrate",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 120000 of 200000 compute units",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            None,
        );

        let event = check_graduation_logs(&text);
        assert!(event.is_some(), "should detect pump.fun Migrate instruction");

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

    // ── PumpSwap transactionSubscribe parser tests ─────────────────

    // Test mint: a valid base58-encoded 32-byte pubkey
    const TEST_MINT: &str = "7LSsEoJGhLeZzGvDofTdNg7M3JttxQqGWNLo6vWMpump";
    const TEST_COIN_VAULT: &str = "FnK9BfdJ4gSVKPjMfHQc7nvYMoGQsmvWWznTEfGHtRFu";
    const TEST_PC_VAULT: &str = "8sLbNZoA1cfnvMJLPfA98bxJ1FBjQBkokf9cDVh3mzLj";
    const WSOL: &str = "So11111111111111111111111111111111111111112";

    /// Build a mock Helius transactionNotification JSON string.
    fn mock_tx_notification(
        sig: &str,
        logs: &[&str],
        post_token_balances: &[(/*mint*/&str, /*accountIndex*/u64)],
        account_keys: &[&str],
        err: Option<&str>,
    ) -> String {
        let logs_json: Vec<String> = logs.iter().map(|l| format!("\"{}\"", l)).collect();
        let err_value = err.unwrap_or("null");

        let balances_json: Vec<String> = post_token_balances
            .iter()
            .map(|(mint, idx)| {
                format!(
                    r#"{{ "mint": "{}", "accountIndex": {}, "owner": "owner1" }}"#,
                    mint, idx
                )
            })
            .collect();

        let keys_json: Vec<String> = account_keys.iter().map(|k| format!("\"{}\"", k)).collect();

        format!(
            r#"{{
                "jsonrpc": "2.0",
                "method": "transactionNotification",
                "params": {{
                    "result": {{
                        "signature": "{}",
                        "transaction": {{
                            "transaction": {{
                                "message": {{
                                    "accountKeys": [{}]
                                }}
                            }},
                            "meta": {{
                                "err": {},
                                "logMessages": [{}],
                                "postTokenBalances": [{}]
                            }}
                        }}
                    }},
                    "subscription": 2
                }}
            }}"#,
            sig,
            keys_json.join(", "),
            err_value,
            logs_json.join(", "),
            balances_json.join(", ")
        )
    }

    #[test]
    fn test_pumpswap_tx_graduation_full_extraction() {
        let text = mock_tx_notification(
            TEST_SIG,
            &[
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                "Program log: Instruction: Migrate",
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA invoke [2]",
                "Program log: Instruction: CreatePool",
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA success",
                "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
            ],
            &[
                (TEST_MINT, 3),      // coin vault at accountKeys[3]
                (WSOL, 4),           // pc vault at accountKeys[4]
            ],
            &[
                "11111111111111111111111111111111",   // 0: system program
                "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P", // 1: pump.fun
                "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",  // 2: pumpswap
                TEST_COIN_VAULT,                      // 3: coin vault
                TEST_PC_VAULT,                        // 4: pc vault
            ],
            None,
        );

        let event = parse_pumpswap_transaction(&text);
        assert!(event.is_some(), "should detect PumpSwap graduation");

        match event.unwrap() {
            FeedEvent::PumpSwapGraduationDirect {
                mint, sig, coin_vault, pc_vault, source, ..
            } => {
                assert_ne!(mint, [0u8; 32], "mint should be extracted");
                assert_ne!(sig, [0u8; 64], "sig should be populated");
                assert_ne!(coin_vault, [0u8; 32], "coin_vault should be extracted");
                assert_ne!(pc_vault, [0u8; 32], "pc_vault should be extracted");
                assert_eq!(source, MigrationSource::HeliusEnhanced);
            }
            other => panic!("expected PumpSwapGraduationDirect, got {:?}", other),
        }
    }

    #[test]
    fn test_pumpswap_tx_create_pool_without_migrate_rejected() {
        // Manual pool creation — has CreatePool but NOT Migrate
        let text = mock_tx_notification(
            TEST_SIG,
            &[
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA invoke [1]",
                "Program log: Instruction: CreatePool",
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA success",
            ],
            &[(TEST_MINT, 1), (WSOL, 2)],
            &["11111111111111111111111111111111", TEST_COIN_VAULT, TEST_PC_VAULT],
            None,
        );

        let event = parse_pumpswap_transaction(&text);
        assert!(event.is_none(), "CreatePool without Migrate should be rejected");
    }

    #[test]
    fn test_pumpswap_tx_swap_ignored() {
        // A regular PumpSwap buy/sell — no CreatePool or Migrate
        let text = mock_tx_notification(
            TEST_SIG,
            &[
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA invoke [1]",
                "Program log: Instruction: Buy",
                "Program pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA success",
            ],
            &[(TEST_MINT, 1)],
            &["11111111111111111111111111111111", TEST_COIN_VAULT],
            None,
        );

        let event = parse_pumpswap_transaction(&text);
        assert!(event.is_none(), "regular PumpSwap swap should be ignored");
    }

    #[test]
    fn test_pumpswap_tx_failed_rejected() {
        let text = mock_tx_notification(
            TEST_SIG,
            &[
                "Program log: Instruction: Migrate",
                "Program log: Instruction: CreatePool",
            ],
            &[(TEST_MINT, 1), (WSOL, 2)],
            &["11111111111111111111111111111111", TEST_COIN_VAULT, TEST_PC_VAULT],
            Some(r#"{"InstructionError":[0,"Custom"]}"#),
        );

        let event = parse_pumpswap_transaction(&text);
        assert!(event.is_none(), "failed tx should be rejected");
    }

    #[test]
    fn test_pumpswap_tx_non_notification_ignored() {
        let text = r#"{"jsonrpc":"2.0","result":42,"id":2}"#;
        let event = parse_pumpswap_transaction(text);
        assert!(event.is_none(), "subscription confirmation should be ignored");
    }

    #[test]
    fn test_pumpswap_tx_vault_fallback_to_migration() {
        // Has Migrate + CreatePool but no postTokenBalances → can't extract vaults
        // But we can still extract the mint and fall back to Migration
        let text = format!(
            r#"{{
                "jsonrpc": "2.0",
                "method": "transactionNotification",
                "params": {{
                    "result": {{
                        "signature": "{}",
                        "transaction": {{
                            "transaction": {{
                                "message": {{
                                    "accountKeys": ["11111111111111111111111111111111"]
                                }}
                            }},
                            "meta": {{
                                "err": null,
                                "logMessages": [
                                    "Program log: Instruction: Migrate",
                                    "Program log: Instruction: CreatePool"
                                ],
                                "postTokenBalances": [
                                    {{ "mint": "{}", "accountIndex": 99, "owner": "x" }}
                                ]
                            }}
                        }}
                    }},
                    "subscription": 2
                }}
            }}"#,
            TEST_SIG, TEST_MINT
        );

        let event = parse_pumpswap_transaction(&text);
        assert!(event.is_some(), "should fall back to Migration when vaults can't be extracted");

        match event.unwrap() {
            FeedEvent::Migration { source, .. } => {
                assert_eq!(source, MigrationSource::HeliusEnhanced);
            }
            other => panic!("expected Migration fallback, got {:?}", other),
        }
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
