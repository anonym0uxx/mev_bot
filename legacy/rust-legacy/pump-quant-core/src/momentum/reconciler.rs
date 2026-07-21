//! On-chain trade log reconciliation engine.
//!
//! Compares trade log entries (estimated P&L from price feeds) against
//! actual on-chain transaction data via `getSignatureStatuses` and
//! `getTransaction` RPC calls. Runs as a background task every 15 seconds.
//!
//! ## Problem Solved
//!
//! The momentum trade logger records P&L based on ESTIMATED prices from the
//! price feed — NOT actual on-chain fills. This produced phantom trades,
//! wrong P&L, and unreliable data. The reconciler verifies every trade
//! against the blockchain and computes actual P&L from confirmed transactions.
//!
//! ## Architecture
//!
//! - `record_buy_tx()` / `record_sell_tx()` — called on hot path after TX submission
//! - `run()` — background loop (15s interval) that checks pending trades
//! - `check_tx_status()` → `fetch_sol_delta()` — RPC pipeline for confirmation + SOL extraction
//! - JSONL audit trail written to `data/reconciliation.jsonl`
//! - Rate-limited to ≤5 RPC requests per second (reconciliation is NOT hot path)

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use super::logger::MomentumClosedPosition;

// ── Time helper ──────────────────────────────────────────────────────────────

#[inline(always)]
fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Types ────────────────────────────────────────────────────────────────────

/// Reconciliation status for a single trade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReconcileStatus {
    Pending,
    Reconciled,
    BuyNotConfirmed,
    SellNotConfirmed,
    Discrepancy,
}

/// On-chain confirmed trade data, compared against log-reported P&L.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnChainTrade {
    pub mint: String,
    pub buy_signature: Option<String>,
    pub sell_signature: Option<String>,
    pub buy_confirmed: bool,
    pub sell_confirmed: bool,
    pub buy_sol_spent: Option<f64>,
    pub sell_sol_received: Option<f64>,
    pub onchain_pnl_sol: Option<f64>,
    pub log_pnl_sol: f64,
    pub pnl_discrepancy_sol: Option<f64>,
    pub reconciled_at_ms: u64,
    pub status: ReconcileStatus,
    pub created_at_ms: u64,
    pub check_count: u32,
}

/// Result of checking a TX signature on-chain.
#[derive(Debug)]
enum TxStatus {
    Confirmed(f64),
    Failed,
    Pending,
}

/// Summary statistics for the /api/status endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationSummary {
    pub total_reconciled: u64,
    pub total_discrepancies: u64,
    pub total_stuck: u64,
    pub total_buy_failed: u64,
    pub pending_count: u64,
    pub onchain_pnl_sol: f64,
    pub log_pnl_sol: f64,
    pub pnl_discrepancy_sol: f64,
    pub stuck_mints: Vec<String>,
    pub phantom_mints: Vec<String>,
    pub avg_discrepancy_sol: f64,
}

// ── Reconciler ───────────────────────────────────────────────────────────────

pub struct Reconciler {
    pending: DashMap<String, OnChainTrade>,
    completed: DashMap<String, OnChainTrade>,
    rpc_url: String,
    http_client: reqwest::Client,
    log_path: String,
    total_reconciled: AtomicU64,
    total_discrepancies: AtomicU64,
    total_stuck: AtomicU64,
    total_buy_failed: AtomicU64,
    onchain_total_pnl_lamports: AtomicI64,
    log_total_pnl_lamports: AtomicI64,
    sum_abs_discrepancy_lamports: AtomicU64,
    wallet_pubkey_b58: String,
    stale_timeout_ms: u64,
    discrepancy_tolerance_sol: f64,
}

impl Reconciler {
    pub fn new(rpc_url: String, wallet_pubkey_b58: String, log_path: String) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(2)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        tracing::info!(
            rpc_url = %rpc_url,
            wallet = %wallet_pubkey_b58,
            log_path = %log_path,
            "[reconciler] initialized"
        );

        Self {
            pending: DashMap::new(),
            completed: DashMap::new(),
            rpc_url,
            http_client,
            log_path,
            total_reconciled: AtomicU64::new(0),
            total_discrepancies: AtomicU64::new(0),
            total_stuck: AtomicU64::new(0),
            total_buy_failed: AtomicU64::new(0),
            onchain_total_pnl_lamports: AtomicI64::new(0),
            log_total_pnl_lamports: AtomicI64::new(0),
            sum_abs_discrepancy_lamports: AtomicU64::new(0),
            wallet_pubkey_b58,
            stale_timeout_ms: 120_000,
            discrepancy_tolerance_sol: 0.0001,
        }
    }

    // ── Hot-path API ─────────────────────────────────────────────────────

    pub fn record_buy_tx(&self, mint: &str, signature: &str, log_entry: &MomentumClosedPosition) {
        let now = current_epoch_ms();
        let trade = OnChainTrade {
            mint: mint.to_string(),
            buy_signature: Some(signature.to_string()),
            sell_signature: None,
            buy_confirmed: false,
            sell_confirmed: false,
            buy_sol_spent: None,
            sell_sol_received: None,
            onchain_pnl_sol: None,
            log_pnl_sol: log_entry.net_pnl_sol,
            pnl_discrepancy_sol: None,
            reconciled_at_ms: 0,
            status: ReconcileStatus::Pending,
            created_at_ms: now,
            check_count: 0,
        };
        tracing::debug!(mint=%mint, sig=%signature, "[reconciler] recorded buy TX");
        self.pending.insert(mint.to_string(), trade);
    }

    pub fn record_buy_tx_raw(&self, mint: &str, signature: &str, log_pnl_sol: f64) {
        let now = current_epoch_ms();
        let trade = OnChainTrade {
            mint: mint.to_string(),
            buy_signature: Some(signature.to_string()),
            sell_signature: None,
            buy_confirmed: false,
            sell_confirmed: false,
            buy_sol_spent: None,
            sell_sol_received: None,
            onchain_pnl_sol: None,
            log_pnl_sol,
            pnl_discrepancy_sol: None,
            reconciled_at_ms: 0,
            status: ReconcileStatus::Pending,
            created_at_ms: now,
            check_count: 0,
        };
        tracing::debug!(mint=%mint, sig=%signature, "[reconciler] recorded buy TX (raw)");
        self.pending.insert(mint.to_string(), trade);
    }

    pub fn record_sell_tx(&self, mint: &str, signature: &str) {
        if let Some(mut trade) = self.pending.get_mut(mint) {
            trade.sell_signature = Some(signature.to_string());
            tracing::debug!(mint=%mint, sig=%signature, "[reconciler] recorded sell TX");
        }
    }

    pub fn update_log_pnl(&self, mint: &str, log_pnl_sol: f64) {
        if let Some(mut trade) = self.pending.get_mut(mint) {
            trade.log_pnl_sol = log_pnl_sol;
        }
    }

    // ── Background loop ──────────────────────────────────────────────────

    pub async fn run(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!("[reconciler] background loop started (15s interval)");

        loop {
            interval.tick().await;
            if self.pending.is_empty() {
                continue;
            }
            tracing::debug!(
                pending = self.pending.len(),
                reconciled = self.total_reconciled.load(Ordering::Relaxed),
                "[reconciler] tick"
            );
            self.reconcile_pending().await;
        }
    }

    async fn reconcile_pending(&self) {
        let now = current_epoch_ms();
        let mints: Vec<String> = self.pending.iter().map(|e| e.key().clone()).collect();

        let mut rpc_calls: u32 = 0;
        const MAX_RPC_PER_TICK: u32 = 10;

        for mint in mints {
            if rpc_calls >= MAX_RPC_PER_TICK {
                tracing::debug!("[reconciler] RPC budget exhausted this tick");
                break;
            }

            let snap = match self.pending.get(&mint) {
                Some(t) => t.clone(),
                None => continue,
            };

            // Stale timeout
            let age_ms = now.saturating_sub(snap.created_at_ms);
            if age_ms > self.stale_timeout_ms {
                if !snap.buy_confirmed {
                    self.total_buy_failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(mint=%mint, age_ms, "[reconciler] BUY NOT CONFIRMED — phantom trade");
                    self.finalize_trade(&mint, ReconcileStatus::BuyNotConfirmed, now).await;
                } else if !snap.sell_confirmed {
                    self.total_stuck.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(mint=%mint, age_ms, buy_sol=?snap.buy_sol_spent, "[reconciler] SELL NOT CONFIRMED — token stuck!");
                    self.finalize_trade(&mint, ReconcileStatus::SellNotConfirmed, now).await;
                }
                continue;
            }

            // Check buy TX
            if !snap.buy_confirmed {
                if let Some(ref sig) = snap.buy_signature {
                    rpc_calls += 1;
                    match self.check_tx_status(sig).await {
                        TxStatus::Confirmed(sol_delta) => {
                            rpc_calls += 1; // getTransaction call
                            if let Some(mut t) = self.pending.get_mut(&mint) {
                                t.buy_confirmed = true;
                                t.buy_sol_spent = Some(sol_delta.abs());
                                t.check_count += 1;
                                tracing::info!(mint=%mint, buy_sol=format!("{:.6}", sol_delta.abs()), "[reconciler] buy CONFIRMED ✅");
                            }
                        }
                        TxStatus::Failed => {
                            self.total_buy_failed.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(mint=%mint, sig=%sig, "[reconciler] buy TX FAILED — phantom");
                            self.finalize_trade(&mint, ReconcileStatus::BuyNotConfirmed, now).await;
                            continue;
                        }
                        TxStatus::Pending => {
                            if let Some(mut t) = self.pending.get_mut(&mint) { t.check_count += 1; }
                        }
                    }
                }
            }

            // Re-read
            let snap = match self.pending.get(&mint) {
                Some(t) => t.clone(),
                None => continue,
            };

            // Check sell TX
            if snap.buy_confirmed && !snap.sell_confirmed {
                if let Some(ref sig) = snap.sell_signature {
                    rpc_calls += 1;
                    match self.check_tx_status(sig).await {
                        TxStatus::Confirmed(sol_delta) => {
                            rpc_calls += 1;
                            if let Some(mut t) = self.pending.get_mut(&mint) {
                                t.sell_confirmed = true;
                                t.sell_sol_received = Some(sol_delta.abs());
                                t.check_count += 1;
                                tracing::info!(mint=%mint, sell_sol=format!("{:.6}", sol_delta.abs()), "[reconciler] sell CONFIRMED ✅");
                            }
                        }
                        TxStatus::Failed => {
                            self.total_stuck.fetch_add(1, Ordering::Relaxed);
                            tracing::error!(mint=%mint, sig=%sig, "[reconciler] sell TX FAILED — stuck!");
                            self.finalize_trade(&mint, ReconcileStatus::SellNotConfirmed, now).await;
                            continue;
                        }
                        TxStatus::Pending => {
                            if let Some(mut t) = self.pending.get_mut(&mint) { t.check_count += 1; }
                        }
                    }
                }
            }

            // Re-read
            let snap = match self.pending.get(&mint) {
                Some(t) => t.clone(),
                None => continue,
            };

            // Compute P&L
            if snap.buy_confirmed && snap.sell_confirmed {
                if let (Some(spent), Some(recv)) = (snap.buy_sol_spent, snap.sell_sol_received) {
                    let onchain_pnl = recv - spent;
                    let disc = onchain_pnl - snap.log_pnl_sol;
                    let abs_disc = disc.abs();

                    let final_status = if abs_disc > self.discrepancy_tolerance_sol {
                        ReconcileStatus::Discrepancy
                    } else {
                        ReconcileStatus::Reconciled
                    };

                    if let Some(mut t) = self.pending.get_mut(&mint) {
                        t.onchain_pnl_sol = Some(onchain_pnl);
                        t.pnl_discrepancy_sol = Some(disc);
                        t.reconciled_at_ms = now;
                        t.status = final_status.clone();
                    }

                    self.onchain_total_pnl_lamports.fetch_add((onchain_pnl * 1e9) as i64, Ordering::Relaxed);
                    self.log_total_pnl_lamports.fetch_add((snap.log_pnl_sol * 1e9) as i64, Ordering::Relaxed);
                    self.sum_abs_discrepancy_lamports.fetch_add((abs_disc * 1e9) as u64, Ordering::Relaxed);
                    self.total_reconciled.fetch_add(1, Ordering::Relaxed);

                    if final_status == ReconcileStatus::Discrepancy {
                        self.total_discrepancies.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            mint=%mint,
                            onchain=format!("{:.6}", onchain_pnl),
                            log=format!("{:.6}", snap.log_pnl_sol),
                            disc=format!("{:.6}", disc),
                            "[reconciler] ⚠️ DISCREPANCY"
                        );
                    } else {
                        tracing::info!(
                            mint=%mint,
                            onchain=format!("{:.6}", onchain_pnl),
                            log=format!("{:.6}", snap.log_pnl_sol),
                            "[reconciler] ✅ RECONCILED"
                        );
                    }

                    self.finalize_and_complete(&mint, now).await;
                }
            }
        }
    }

    // ── RPC helpers ──────────────────────────────────────────────────────

    async fn check_tx_status(&self, signature: &str) -> TxStatus {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSignatureStatuses",
            "params": [[signature], {"searchTransactionHistory": true}]
        });

        let resp = match self.rpc_post(&body).await {
            Some(v) => v,
            None => return TxStatus::Pending,
        };

        let status = &resp["result"]["value"][0];
        if status.is_null() {
            return TxStatus::Pending;
        }

        if let Some(err) = status.get("err") {
            if !err.is_null() {
                return TxStatus::Failed;
            }
        }

        match status["confirmationStatus"].as_str().unwrap_or("") {
            "confirmed" | "finalized" => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                self.fetch_sol_delta(signature).await
            }
            _ => TxStatus::Pending,
        }
    }

    async fn fetch_sol_delta(&self, signature: &str) -> TxStatus {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": [
                signature,
                {
                    "encoding": "jsonParsed",
                    "commitment": "confirmed",
                    "maxSupportedTransactionVersion": 0
                }
            ]
        });

        let tx_json = match self.rpc_post(&body).await {
            Some(v) => v,
            None => return TxStatus::Pending,
        };

        let result = &tx_json["result"];
        if result.is_null() {
            return TxStatus::Pending;
        }

        let meta = &result["meta"];
        if let Some(err) = meta.get("err") {
            if !err.is_null() {
                return TxStatus::Failed;
            }
        }

        let pre_balances = match meta["preBalances"].as_array() {
            Some(a) => a,
            None => return TxStatus::Pending,
        };
        let post_balances = match meta["postBalances"].as_array() {
            Some(a) => a,
            None => return TxStatus::Pending,
        };

        // Build the full ordered list of account keys for this TX.
        // Versioned TXs (v0) use staticAccountKeys + loadedAddresses.
        // Legacy TXs use message.accountKeys as string array.
        let mut account_keys: Vec<String> = Vec::new();

        // Try versioned format first (staticAccountKeys)
        if let Some(static_keys) = result["transaction"]["message"]["accountKeys"].as_array() {
            for key in static_keys {
                // Can be object {pubkey: "...", signer: bool, writable: bool} or string
                if let Some(pk) = key.as_str() {
                    account_keys.push(pk.to_string());
                } else if let Some(pk) = key["pubkey"].as_str() {
                    account_keys.push(pk.to_string());
                }
            }
        }

        // Append loaded addresses (address lookup tables, v0 only)
        if let Some(loaded) = meta.get("loadedAddresses") {
            if let Some(writable) = loaded["writable"].as_array() {
                for addr in writable {
                    if let Some(s) = addr.as_str() {
                        account_keys.push(s.to_string());
                    }
                }
            }
            if let Some(readonly) = loaded["readonly"].as_array() {
                for addr in readonly {
                    if let Some(s) = addr.as_str() {
                        account_keys.push(s.to_string());
                    }
                }
            }
        }

        if account_keys.is_empty() {
            tracing::warn!(signature=%signature, "[reconciler] no account keys in TX");
            return TxStatus::Pending;
        }

        // Find our wallet index in the account list
        let wallet_idx = account_keys.iter().position(|k| k == &self.wallet_pubkey_b58);
        let wallet_idx = match wallet_idx {
            Some(i) => i,
            None => {
                tracing::warn!(
                    signature=%signature,
                    wallet=%self.wallet_pubkey_b58,
                    n_accounts=account_keys.len(),
                    "[reconciler] wallet pubkey not found in TX accounts"
                );
                // Wallet not in TX — might be a different TX or ALT not loaded.
                // Return a zero delta as confirmed (the TX landed, but we can't
                // extract SOL change).
                return TxStatus::Confirmed(0.0);
            }
        };

        let pre_lamports = pre_balances
            .get(wallet_idx)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let post_lamports = post_balances
            .get(wallet_idx)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let delta_lamports = post_lamports as i64 - pre_lamports as i64;
        let delta_sol = delta_lamports as f64 / 1e9;

        tracing::debug!(
            signature=%signature,
            pre_lamports,
            post_lamports,
            delta_sol=format!("{:.6}", delta_sol),
            "[reconciler] SOL delta extracted"
        );

        TxStatus::Confirmed(delta_sol)
    }

    /// Generic JSON-RPC POST with error handling and 429 backoff.
    async fn rpc_post(&self, body: &serde_json::Value) -> Option<serde_json::Value> {
        let resp = match self
            .http_client
            .post(&self.rpc_url)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(err=%e, "[reconciler] RPC HTTP error");
                return None;
            }
        };

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .map(|s| (s * 1000.0) as u64)
                .unwrap_or(2000);
            tracing::warn!(retry_after_ms=retry_after, "[reconciler] 429 — backing off");
            tokio::time::sleep(Duration::from_millis(retry_after)).await;
            return None;
        }

        match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                if v.get("error").map_or(false, |e| !e.is_null()) {
                    tracing::warn!(err=%v["error"], "[reconciler] RPC error response");
                    return None;
                }
                Some(v)
            }
            Err(e) => {
                tracing::warn!(err=%e, "[reconciler] RPC response parse error");
                None
            }
        }
    }

    // ── Finalization helpers ─────────────────────────────────────────────

    /// Move a trade from pending to completed with the given status.
    /// Writes JSONL audit log entry.
    async fn finalize_trade(&self, mint: &str, status: ReconcileStatus, now: u64) {
        if let Some(mut trade) = self.pending.get_mut(mint) {
            trade.status = status;
            trade.reconciled_at_ms = now;
        }
        if let Some((_, trade)) = self.pending.remove(mint) {
            self.write_jsonl(&trade);
            self.completed.insert(mint.to_string(), trade);
        }
    }

    /// Move a fully reconciled trade from pending to completed.
    /// Writes JSONL audit log entry.
    async fn finalize_and_complete(&self, mint: &str, _now: u64) {
        if let Some((_, trade)) = self.pending.remove(mint) {
            self.write_jsonl(&trade);
            self.completed.insert(mint.to_string(), trade);
        }
    }

    /// Append a single trade record to the JSONL audit log.
    fn write_jsonl(&self, trade: &OnChainTrade) {
        let file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(err=%e, path=%self.log_path, "[reconciler] failed to open JSONL log");
                return;
            }
        };
        let mut writer = BufWriter::new(file);
        match serde_json::to_string(trade) {
            Ok(mut line) => {
                line.push('\n');
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    tracing::error!(err=%e, "[reconciler] JSONL write failed");
                }
                if let Err(e) = writer.flush() {
                    tracing::error!(err=%e, "[reconciler] JSONL flush failed");
                }
            }
            Err(e) => {
                tracing::error!(err=%e, "[reconciler] JSONL serialize failed");
            }
        }
    }

    // ── API endpoint ─────────────────────────────────────────────────────

    pub fn get_reconciliation_summary(&self) -> ReconciliationSummary {
        let total_reconciled = self.total_reconciled.load(Ordering::Relaxed);
        let onchain_pnl_sol =
            self.onchain_total_pnl_lamports.load(Ordering::Relaxed) as f64 / 1e9;
        let log_pnl_sol =
            self.log_total_pnl_lamports.load(Ordering::Relaxed) as f64 / 1e9;
        let avg_disc = if total_reconciled > 0 {
            self.sum_abs_discrepancy_lamports.load(Ordering::Relaxed) as f64
                / 1e9
                / total_reconciled as f64
        } else {
            0.0
        };

        let stuck_mints: Vec<String> = self
            .completed
            .iter()
            .filter(|e| e.value().status == ReconcileStatus::SellNotConfirmed)
            .map(|e| e.key().clone())
            .chain(
                self.pending
                    .iter()
                    .filter(|e| e.value().status == ReconcileStatus::SellNotConfirmed)
                    .map(|e| e.key().clone()),
            )
            .collect();

        let phantom_mints: Vec<String> = self
            .completed
            .iter()
            .filter(|e| e.value().status == ReconcileStatus::BuyNotConfirmed)
            .map(|e| e.key().clone())
            .collect();

        ReconciliationSummary {
            total_reconciled,
            total_discrepancies: self.total_discrepancies.load(Ordering::Relaxed),
            total_stuck: self.total_stuck.load(Ordering::Relaxed),
            total_buy_failed: self.total_buy_failed.load(Ordering::Relaxed),
            pending_count: self.pending.len() as u64,
            onchain_pnl_sol,
            log_pnl_sol,
            pnl_discrepancy_sol: onchain_pnl_sol - log_pnl_sol,
            stuck_mints,
            phantom_mints,
            avg_discrepancy_sol: avg_disc,
        }
    }

    /// Get a specific trade's reconciliation data (for debugging).
    pub fn get_trade(&self, mint: &str) -> Option<OnChainTrade> {
        self.pending
            .get(mint)
            .map(|t| t.clone())
            .or_else(|| self.completed.get(mint).map(|t| t.clone()))
    }

    /// Get all completed trades (for detailed reporting).
    pub fn get_all_completed(&self) -> Vec<OnChainTrade> {
        self.completed.iter().map(|e| e.value().clone()).collect()
    }

    /// Number of trades currently pending reconciliation.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────


#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_log_entry() -> MomentumClosedPosition {
        MomentumClosedPosition {
            strategy_tag: "momentum",
            mint: "TestMint1111111111111111111111111111111111".to_string(),
            pool_type: "pump_swap",
            grad_score: 72,
            grad_score_final: 72,
            grad_speed_s: 120,
            grad_volume_sol: 85.0,
            pre_grad_buys_5s: 8,
            size_sol: 0.05,
            size_lamports: 50_000_000,
            entry_delay_ms: 15000,
            entry_price_lamports: 381_900,
            exit_price_lamports: 400_000,
            bc_terminal_price_fp: 410,
            structural_discount_pct: 7.0,
            entry_timestamp_ms: 1_711_700_000_000,
            exit_timestamp_ms: 1_711_700_023_400,
            hold_ms: 23_400,
            exit_reason: "tp2",
            raw_gain_bps: 500,
            gross_pnl_sol: 0.0025,
            fee_sol: 0.0005,
            fees_sol: 0.0005,
            net_pnl_sol: 0.002,
            price_samples_bps: vec![0, 100, 300, 500],
            price_sample_count: 4,
            ws_notif_count_at_close: 8,
            is_paper: false,
            config_version: "mom-v0.05sol".to_string(),
        }
    }

    #[test]
    fn test_reconciler_record_buy_tx() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let entry = make_test_log_entry();
        r.record_buy_tx("TestMint1111111111111111111111111111111111", "BuySig123", &entry);
        assert_eq!(r.pending.len(), 1);

        let trade = r.pending.get("TestMint1111111111111111111111111111111111").unwrap();
        assert_eq!(trade.buy_signature, Some("BuySig123".to_string()));
        assert_eq!(trade.sell_signature, None);
        assert!(!trade.buy_confirmed);
        assert!(!trade.sell_confirmed);
        assert!((trade.log_pnl_sol - 0.002).abs() < 1e-9);
        assert_eq!(trade.status, ReconcileStatus::Pending);
    }

    #[test]
    fn test_reconciler_record_sell_tx() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let entry = make_test_log_entry();
        r.record_buy_tx("TestMint1111111111111111111111111111111111", "BuySig123", &entry);
        r.record_sell_tx("TestMint1111111111111111111111111111111111", "SellSig456");

        let trade = r.pending.get("TestMint1111111111111111111111111111111111").unwrap();
        assert_eq!(trade.sell_signature, Some("SellSig456".to_string()));
    }

    #[test]
    fn test_reconciler_record_sell_tx_no_buy() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        // Sell without buy — should be silently ignored
        r.record_sell_tx("NonexistentMint", "SellSig456");
        assert_eq!(r.pending.len(), 0);
    }

    #[test]
    fn test_reconciler_update_log_pnl() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let entry = make_test_log_entry();
        r.record_buy_tx("TestMint1111111111111111111111111111111111", "BuySig123", &entry);
        r.update_log_pnl("TestMint1111111111111111111111111111111111", -0.003);

        let trade = r.pending.get("TestMint1111111111111111111111111111111111").unwrap();
        assert!((trade.log_pnl_sol - (-0.003)).abs() < 1e-9);
    }

    #[test]
    fn test_reconciliation_summary_empty() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let summary = r.get_reconciliation_summary();
        assert_eq!(summary.total_reconciled, 0);
        assert_eq!(summary.total_discrepancies, 0);
        assert_eq!(summary.total_stuck, 0);
        assert_eq!(summary.total_buy_failed, 0);
        assert_eq!(summary.pending_count, 0);
        assert!((summary.onchain_pnl_sol).abs() < 1e-9);
        assert!((summary.log_pnl_sol).abs() < 1e-9);
    }

    #[test]
    fn test_reconciliation_summary_with_pending() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let entry = make_test_log_entry();
        r.record_buy_tx("Mint1", "Sig1", &entry);
        r.record_buy_tx("Mint2", "Sig2", &entry);

        let summary = r.get_reconciliation_summary();
        assert_eq!(summary.pending_count, 2);
        assert_eq!(summary.total_reconciled, 0);
    }

    #[test]
    fn test_onchain_trade_serialization() {
        let trade = OnChainTrade {
            mint: "TestMint".to_string(),
            buy_signature: Some("BuySig".to_string()),
            sell_signature: Some("SellSig".to_string()),
            buy_confirmed: true,
            sell_confirmed: true,
            buy_sol_spent: Some(0.05),
            sell_sol_received: Some(0.048),
            onchain_pnl_sol: Some(-0.002),
            log_pnl_sol: 0.002,
            pnl_discrepancy_sol: Some(-0.004),
            reconciled_at_ms: 1_711_700_000_000,
            status: ReconcileStatus::Discrepancy,
            created_at_ms: 1_711_699_990_000,
            check_count: 3,
        };
        let json = serde_json::to_string(&trade).unwrap();
        let parsed: OnChainTrade = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mint, "TestMint");
        assert_eq!(parsed.status, ReconcileStatus::Discrepancy);
        assert!((parsed.onchain_pnl_sol.unwrap() - (-0.002)).abs() < 1e-9);
    }

    #[test]
    fn test_reconcile_status_serialization() {
        let statuses = vec![
            ReconcileStatus::Pending,
            ReconcileStatus::Reconciled,
            ReconcileStatus::BuyNotConfirmed,
            ReconcileStatus::SellNotConfirmed,
            ReconcileStatus::Discrepancy,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let parsed: ReconcileStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn test_write_jsonl() {
        let path = format!("/tmp/test_recon_{}.jsonl", std::process::id());
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            path.clone(),
        );
        let trade = OnChainTrade {
            mint: "WriteMint".to_string(),
            buy_signature: Some("BuySig".to_string()),
            sell_signature: None,
            buy_confirmed: false,
            sell_confirmed: false,
            buy_sol_spent: None,
            sell_sol_received: None,
            onchain_pnl_sol: None,
            log_pnl_sol: 0.001,
            pnl_discrepancy_sol: None,
            reconciled_at_ms: 0,
            status: ReconcileStatus::Pending,
            created_at_ms: current_epoch_ms(),
            check_count: 0,
        };
        r.write_jsonl(&trade);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["mint"], "WriteMint");
        assert_eq!(parsed["status"], "Pending");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_get_trade() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        let entry = make_test_log_entry();
        r.record_buy_tx("Mint1", "Sig1", &entry);

        assert!(r.get_trade("Mint1").is_some());
        assert!(r.get_trade("Mint2").is_none());
    }

    #[test]
    fn test_record_buy_tx_raw() {
        let r = Reconciler::new(
            "https://example.com".to_string(),
            "WaLLeTpUbKeY111111111111111111111111111111".to_string(),
            "/tmp/test_recon.jsonl".to_string(),
        );
        r.record_buy_tx_raw("RawMint", "RawSig", -0.005);

        let trade = r.pending.get("RawMint").unwrap();
        assert_eq!(trade.buy_signature, Some("RawSig".to_string()));
        assert!((trade.log_pnl_sol - (-0.005)).abs() < 1e-9);
    }
}
