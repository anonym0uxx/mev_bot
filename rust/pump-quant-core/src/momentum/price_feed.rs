//! Hybrid price feed for Raydium/PumpSwap vault accounts.
//!
//! Two parallel paths update the same `PriceState` atomics:
//! 1. **WebSocket accountSubscribe** (primary, ~50-100ms) — Helius WSS
//! 2. **HTTP RPC getAccountInfo polling** (fallback/correction, 500ms)
//!
//! Previous attempt used `wss://mainnet.helius-rpc.com/?api-key=...` with
//! commitment=confirmed — silently delivered zero accountNotifications.
//! Fix: use dedicated endpoint (SOLANA_WS_URL) with commitment=processed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing;

// ── Fixed-point price type ───────────────────────────────────────────────────

pub type PriceFp = u64;

/// `price_fp = (reserve_sol * 1_000_000) / reserve_token`
#[inline(always)]
pub fn price_from_reserves(reserve_sol: u64, reserve_token: u64) -> PriceFp {
    if reserve_token == 0 {
        return 0;
    }
    ((reserve_sol as u128).saturating_mul(1_000_000) / reserve_token as u128) as u64
}

// ── Subscription types ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VaultSubscription {
    pub mint: [u8; 32],
    pub coin_vault: String,
    pub pc_vault: String,
}

pub enum PriceFeedCommand {
    Subscribe(VaultSubscription),
    Unsubscribe([u8; 32]),
    Shutdown,
}

// ── Shared price state ───────────────────────────────────────────────────────

pub struct PriceState {
    pub price_fp: AtomicU64,
    pub last_update_ms: AtomicU64,
    pub reserve_sol: AtomicU64,
    pub reserve_token: AtomicU64,
    /// Count of WS accountSubscribe notifications received for this mint's vaults.
    /// Each notification = a Raydium/PumpSwap swap occurred. Saturates at u64::MAX.
    pub ws_notif_count: AtomicU64,
    /// Timestamp of last WS accountSubscribe notification (epoch ms). 0 if none received.
    pub ws_notif_last_ms: AtomicU64,
}

impl PriceState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            price_fp: AtomicU64::new(0),
            last_update_ms: AtomicU64::new(0),
            reserve_sol: AtomicU64::new(0),
            reserve_token: AtomicU64::new(0),
            ws_notif_count: AtomicU64::new(0),
            ws_notif_last_ms: AtomicU64::new(0),
        })
    }
}

// ── Vault type tracking ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaultType { Coin, Pc }

// ── PriceFeedManager ─────────────────────────────────────────────────────────

pub struct PriceFeedManager {
    pub prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    active_subs: Arc<DashMap<[u8; 32], VaultSubscription>>,
    cmd_tx: mpsc::Sender<PriceFeedCommand>,
}

impl PriceFeedManager {
    /// Create manager and spawn WS + RPC polling tasks.
    ///
    /// `ws_url`: Helius WSS for accountSubscribe (primary). Empty = disable.
    pub fn new(rpc_url: String, ws_url: String, poll_interval_ms: u64) -> (Self, tokio::task::JoinHandle<()>) {
        let prices: Arc<DashMap<[u8; 32], Arc<PriceState>>> = Arc::new(DashMap::new());
        let active_subs: Arc<DashMap<[u8; 32], VaultSubscription>> = Arc::new(DashMap::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(256);

        if !ws_url.is_empty() {
            let p = prices.clone();
            let s = active_subs.clone();
            tokio::spawn(async move {
                ws_price_loop(ws_url, cmd_rx, p, s).await;
            });
        } else {
            tracing::warn!("[price_feed] WS URL empty — accountSubscribe disabled");
        }

        let p2 = prices.clone();
        let s2 = active_subs.clone();
        let poll_handle = tokio::spawn(async move {
            price_feed_poll_loop(rpc_url, s2, p2, poll_interval_ms).await;
        });

        (Self { prices, active_subs, cmd_tx }, poll_handle)
    }

    pub async fn subscribe(&self, sub: VaultSubscription) {
        tracing::info!(
            mint = %bs58::encode(&sub.mint).into_string(),
            coin_vault = %sub.coin_vault,
            pc_vault = %sub.pc_vault,
            "[price_feed] subscribing to vaults"
        );
        self.prices.entry(sub.mint).or_insert_with(PriceState::new);
        self.active_subs.insert(sub.mint, sub.clone());
        let _ = self.cmd_tx.send(PriceFeedCommand::Subscribe(sub)).await;
    }

    pub async fn unsubscribe(&self, mint: [u8; 32]) {
        self.unsubscribe_sync(&mint);
    }

    pub fn unsubscribe_sync(&self, mint: &[u8; 32]) {
        self.active_subs.remove(mint);
        self.prices.remove(mint);
        let _ = self.cmd_tx.try_send(PriceFeedCommand::Unsubscribe(*mint));
        tracing::debug!(
            mint = %bs58::encode(mint).into_string(),
            remaining_subs = self.active_subs.len(),
            "[price_feed] unsubscribed mint"
        );
    }

    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(PriceFeedCommand::Shutdown).await;
    }

    pub fn cmd_sender(&self) -> mpsc::Sender<PriceFeedCommand> {
        self.cmd_tx.clone()
    }

    pub fn active_subs(&self) -> &Arc<DashMap<[u8; 32], VaultSubscription>> {
        &self.active_subs
    }

    #[inline(always)]
    pub fn current_price(&self, mint: &[u8; 32]) -> Option<u64> {
        self.prices.get(mint).and_then(|s| {
            let p = s.price_fp.load(Ordering::Relaxed);
            // Only return a price when both vaults have contributed (price_fp > 0).
            // A price_fp of 0 means only one vault has been seen — not a valid price.
            if p > 0 { Some(p) } else { None }
        })
    }

    #[inline(always)]
    pub fn price_state(&self, mint: &[u8; 32]) -> Option<Arc<PriceState>> {
        self.prices.get(mint).map(|s| Arc::clone(s.value()))
    }

    /// Returns (ws_notif_count, ws_notif_last_ms) for the given mint's price state.
    /// Returns (0, 0) if no price state exists yet.
    pub fn ws_notif_info(&self, mint: &[u8; 32]) -> (u64, u64) {
        self.prices.get(mint).map(|s| (
            s.ws_notif_count.load(Ordering::Relaxed),
            s.ws_notif_last_ms.load(Ordering::Relaxed),
        )).unwrap_or((0, 0))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// WebSocket accountSubscribe loop (primary, low-latency)
// ═══════════════════════════════════════════════════════════════════════════════

async fn ws_price_loop(
    url: String,
    mut cmd_rx: mpsc::Receiver<PriceFeedCommand>,
    prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    active_subs: Arc<DashMap<[u8; 32], VaultSubscription>>,
) {
    use futures_util::{SinkExt, StreamExt};
    use std::collections::HashMap;
    use tokio_tungstenite::tungstenite::Message;

    let mut backoff_ms: u64 = 500;
    const MAX_BACKOFF: u64 = 30_000;

    loop {
        tracing::info!(url = %url, "[price_feed_ws] connecting");

        let ws_stream = match tokio_tungstenite::connect_async(&url).await {
            Ok((s, _)) => { backoff_ms = 500; s }
            Err(e) => {
                tracing::warn!(error = %e, backoff_ms, "[price_feed_ws] connect failed");
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let (mut ws_tx, mut ws_rx) = ws_stream.split();
        let mut sub_id_map: HashMap<u64, ([u8; 32], VaultType)> = HashMap::new();
        let mut mint_sub_ids: HashMap<[u8; 32], (Option<u64>, Option<u64>)> = HashMap::new();
        let mut pending_rpc: HashMap<u64, ([u8; 32], VaultType)> = HashMap::new();
        let mut next_rpc_id: u64 = 1;
        let mut notif_count: u64 = 0;
        let t0 = std::time::Instant::now();

        // Resubscribe all active vaults on reconnect
        let subs: Vec<VaultSubscription> = active_subs.iter().map(|e| e.value().clone()).collect();
        let n = subs.len();
        let mut ok = true;
        for sub in subs {
            if ws_send_sub(&mut ws_tx, &mut next_rpc_id, &mut pending_rpc, &sub).await.is_err() {
                ok = false;
                break;
            }
        }
        if !ok {
            tracing::warn!("[price_feed_ws] resubscribe failed");
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF);
            continue;
        }
        if n > 0 {
            tracing::info!(count = n, "[price_feed_ws] resubscribed active vaults");
        }

        let mut ping_iv = tokio::time::interval(std::time::Duration::from_secs(30));
        ping_iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ping_iv.tick().await;

        let mut disc = false;

        loop {
            tokio::select! {
                biased;

                msg = ws_rx.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Some((mint, vt, amt)) = ws_parse_notif(&text, &sub_id_map) {
                                notif_count += 1;
                                ws_update_price(&prices, &mint, vt, amt);
                            } else {
                                ws_handle_confirm(&text, &mut pending_rpc, &mut sub_id_map, &mut mint_sub_ids);
                            }
                        }
                        Some(Ok(Message::Ping(d))) => { let _ = ws_tx.send(Message::Pong(d)).await; }
                        Some(Ok(Message::Close(_))) => { tracing::warn!("[price_feed_ws] close frame"); disc = true; break; }
                        Some(Err(e)) => { tracing::warn!(error = %e, "[price_feed_ws] error"); disc = true; break; }
                        None => { tracing::warn!("[price_feed_ws] stream ended"); disc = true; break; }
                        _ => {}
                    }
                }

                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(PriceFeedCommand::Subscribe(sub)) => {
                            if ws_send_sub(&mut ws_tx, &mut next_rpc_id, &mut pending_rpc, &sub).await.is_err() {
                                disc = true; break;
                            }
                        }
                        Some(PriceFeedCommand::Unsubscribe(mint)) => {
                            if let Some((c, p)) = mint_sub_ids.remove(&mint) {
                                for sid in [c, p].into_iter().flatten() {
                                    let uid = next_rpc_id; next_rpc_id += 1;
                                    let m = format!(r#"{{"jsonrpc":"2.0","id":{},"method":"accountUnsubscribe","params":[{}]}}"#, uid, sid);
                                    let _ = ws_tx.send(Message::Text(m.into())).await;
                                    sub_id_map.remove(&sid);
                                }
                            }
                        }
                        Some(PriceFeedCommand::Shutdown) | None => { tracing::info!("[price_feed_ws] shutdown"); return; }
                    }
                }

                _ = ping_iv.tick() => {
                    if ws_tx.send(Message::Ping(vec![].into())).await.is_err() { disc = true; break; }
                    let ac = active_subs.len();
                    let el = t0.elapsed().as_secs();
                    if ac > 0 && notif_count == 0 && el >= 30 {
                        tracing::warn!(active_subs = ac, elapsed_s = el, "[price_feed_ws] WATCHDOG: 0 notifications");
                    }
                }
            }
        }

        if disc {
            tracing::warn!(backoff_ms, notifications = notif_count, "[price_feed_ws] disconnected");
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF);
        }
    }
}

async fn ws_send_sub<S>(
    ws_tx: &mut futures_util::stream::SplitSink<S, tokio_tungstenite::tungstenite::Message>,
    next_id: &mut u64,
    pending: &mut std::collections::HashMap<u64, ([u8; 32], VaultType)>,
    sub: &VaultSubscription,
) -> Result<(), ()>
where S: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
{
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let cid = *next_id; *next_id += 1;
    let cm = format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"processed"}}]}}"#,
        cid, sub.coin_vault
    );
    if ws_tx.send(Message::Text(cm.into())).await.is_err() { return Err(()); }
    pending.insert(cid, (sub.mint, VaultType::Coin));

    let pid = *next_id; *next_id += 1;
    let pm = format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"accountSubscribe","params":["{}",{{"encoding":"base64","commitment":"processed"}}]}}"#,
        pid, sub.pc_vault
    );
    if ws_tx.send(Message::Text(pm.into())).await.is_err() { return Err(()); }
    pending.insert(pid, (sub.mint, VaultType::Pc));

    tracing::debug!(mint = %bs58::encode(&sub.mint).into_string(), "[price_feed_ws] sent accountSubscribe");
    Ok(())
}

fn ws_parse_notif(
    text: &str,
    map: &std::collections::HashMap<u64, ([u8; 32], VaultType)>,
) -> Option<([u8; 32], VaultType, u64)> {
    if !text.contains("accountNotification") { return None; }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("method")?.as_str()? != "accountNotification" { return None; }
    let p = v.get("params")?;
    let sid = p.get("subscription")?.as_u64()?;
    let (mint, vt) = map.get(&sid)?;
    let b64 = p.pointer("/result/value/data")?.as_array()?.first()?.as_str()?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let amt = parse_spl_amount(&bytes)?;
    Some((*mint, *vt, amt))
}

fn ws_handle_confirm(
    text: &str,
    pending: &mut std::collections::HashMap<u64, ([u8; 32], VaultType)>,
    map: &mut std::collections::HashMap<u64, ([u8; 32], VaultType)>,
    mints: &mut std::collections::HashMap<[u8; 32], (Option<u64>, Option<u64>)>,
) {
    if !text.contains("\"result\"") || text.contains("\"method\"") { return; }
    let v: serde_json::Value = match serde_json::from_str(text) { Ok(v) => v, Err(_) => return };
    let rid = match v.get("id").and_then(|x| x.as_u64()) { Some(x) => x, None => return };
    let sid = match v.get("result").and_then(|x| x.as_u64()) { Some(x) => x, None => return };
    if let Some((mint, vt)) = pending.remove(&rid) {
        map.insert(sid, (mint, vt));
        let e = mints.entry(mint).or_insert((None, None));
        match vt { VaultType::Coin => e.0 = Some(sid), VaultType::Pc => e.1 = Some(sid) }
        tracing::debug!(subscription_id = sid, vault = ?vt, mint = %bs58::encode(&mint).into_string(), "[price_feed_ws] confirmed");
    }
}

/// Current epoch time in milliseconds.
#[inline(always)]
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn ws_update_price(
    prices: &DashMap<[u8; 32], Arc<PriceState>>,
    mint: &[u8; 32],
    vt: VaultType,
    amount: u64,
) {
    if let Some(state) = prices.get(mint) {
        let now = epoch_ms();
        // Track WS notification activity for adaptive dead zone
        state.ws_notif_count.fetch_add(1, Ordering::Relaxed);
        state.ws_notif_last_ms.store(now, Ordering::Relaxed);
        match vt {
            VaultType::Coin => state.reserve_token.store(amount, Ordering::Relaxed),
            VaultType::Pc => state.reserve_sol.store(amount, Ordering::Relaxed),
        }
        let sol = state.reserve_sol.load(Ordering::Relaxed);
        let tok = state.reserve_token.load(Ordering::Relaxed);
        if sol > 0 && tok > 0 {
            let price = price_from_reserves(sol, tok);
            if price > 0 {
                let prev = state.price_fp.load(Ordering::Relaxed);
                if prev >= 100 {
                    let hi = price.max(prev);
                    let lo = price.min(prev);
                    if lo > 0 && hi / lo > 100 {
                        tracing::warn!(
                            mint = %bs58::encode(mint).into_string(),
                            prev = prev, new = price,
                            "[price_feed_ws] spike rejected >100x"
                        );
                        return;
                    }
                }
                let was_zero = state.price_fp.swap(price, Ordering::Release) == 0;
                state.last_update_ms.store(now, Ordering::Relaxed);
                if was_zero {
                    tracing::info!(
                        mint = %bs58::encode(mint).into_string(),
                        price_fp = price, sol = sol, token = tok,
                        "[price_feed_ws] first price from accountSubscribe"
                    );
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// RPC Polling Loop (fallback/correction)
// ═══════════════════════════════════════════════════════════════════════════════

async fn price_feed_poll_loop(
    rpc_url: String,
    active_subs: Arc<DashMap<[u8; 32], VaultSubscription>>,
    prices: Arc<DashMap<[u8; 32], Arc<PriceState>>>,
    poll_interval_ms: u64,
) {
    if rpc_url.is_empty() {
        tracing::warn!("[price_feed] RPC URL not configured — polling disabled");
        return;
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut iv = tokio::time::interval(std::time::Duration::from_millis(poll_interval_ms));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(poll_interval_ms, url = %rpc_url, "[price_feed] RPC polling started");

    loop {
        iv.tick().await;

        let mut subs: Vec<([u8; 32], String, String)> = active_subs
            .iter()
            .map(|e| (*e.key(), e.value().coin_vault.clone(), e.value().pc_vault.clone()))
            .collect();

        if subs.is_empty() { continue; }
        if subs.len() > 50 {
            tracing::warn!(n = subs.len(), "[price_feed] large active_subs — capping at 50 for this tick");
            subs.truncate(50);
        }

        const CHUNK: usize = 10;
        let num_chunks = (subs.len() + CHUNK - 1) / CHUNK;
        let chunk_delay_ms = if num_chunks > 1 { poll_interval_ms / num_chunks as u64 } else { 0 };
        let mut consecutive_429s: u32 = 0;

        for chunk in subs.chunks(CHUNK) {
            let chunk_start = std::time::Instant::now();

            // If we've hit 3+ consecutive 429s this tick, skip remaining chunks
            if consecutive_429s >= 3 {
                tracing::warn!(
                    consecutive_429s,
                    "[price_feed] 3+ consecutive 429s — skipping remaining chunks this tick"
                );
                break;
            }

            let mut batch = Vec::with_capacity(chunk.len() * 2);
            for (i, (_, cv, pv)) in chunk.iter().enumerate() {
                batch.push(serde_json::json!({
                    "jsonrpc": "2.0", "id": i * 2,
                    "method": "getAccountInfo",
                    "params": [cv, {"encoding": "base64", "commitment": "confirmed"}]
                }));
                batch.push(serde_json::json!({
                    "jsonrpc": "2.0", "id": i * 2 + 1,
                    "method": "getAccountInfo",
                    "params": [pv, {"encoding": "base64", "commitment": "confirmed"}]
                }));
            }

            let results: Vec<serde_json::Value> = {
                let maybe = async {
                    let mut backoff_ms: u64 = 500;
                    const MAX_BACKOFF_MS: u64 = 30_000;
                    const MAX_RETRIES: u32 = 5;

                    for attempt in 0..=MAX_RETRIES {
                        let resp = http.post(&rpc_url).json(&batch).send().await
                            .map_err(|e| { tracing::warn!(error = %e, "[price_feed] RPC batch failed"); })?;

                        if resp.status().as_u16() == 429 {
                            if attempt == MAX_RETRIES {
                                tracing::warn!(
                                    attempts = MAX_RETRIES + 1,
                                    "[price_feed] HTTP 429 — max retries exhausted"
                                );
                                return Err(());
                            }
                            tracing::warn!(
                                attempt = attempt + 1,
                                backoff_ms,
                                "[price_feed] HTTP 429 — backing off"
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                            continue;
                        }

                        return resp.json().await.map_err(|e| {
                            tracing::warn!(error = %e, "[price_feed] parse failed");
                        });
                    }
                    Err(())
                }.await;
                match maybe {
                    Ok(r) => {
                        consecutive_429s = 0; // Reset on successful response
                        r
                    }
                    Err(()) => {
                        consecutive_429s += 1;
                        // Rate-limit even on failure: sleep remaining chunk delay
                        if chunk_delay_ms > 0 {
                            let elapsed = chunk_start.elapsed().as_millis() as u64;
                            if elapsed < chunk_delay_ms {
                                tokio::time::sleep(tokio::time::Duration::from_millis(
                                    chunk_delay_ms - elapsed,
                                )).await;
                            }
                        }
                        continue;
                    }
                }
            };

            for (i, (mint, _, _)) in chunk.iter().enumerate() {
                let cd = extract_account_data(&results, i * 2);
                let pd = extract_account_data(&results, i * 2 + 1);
                let (sr, tr) = match (cd, pd) {
                    (Some(c), Some(p)) => match (parse_spl_amount(&p), parse_spl_amount(&c)) {
                        (Some(s), Some(t)) => (s, t),
                        _ => continue,
                    },
                    _ => continue,
                };
                if sr == 0 || tr == 0 { continue; }
                let fp = price_from_reserves(sr, tr);
                if fp == 0 { continue; }

                if let Some(state) = prices.get(mint) {
                    let prev = state.price_fp.load(Ordering::Acquire);
                    if prev >= 100 {
                        let hi = fp.max(prev);
                        let lo = fp.min(prev);
                        if lo > 0 && hi / lo > 100 {
                            tracing::warn!(
                                mint = %bs58::encode(mint).into_string(),
                                prev = prev, new = fp, ratio = hi / lo,
                                "[price_feed] spike rejected >100x"
                            );
                            continue;
                        }
                    }
                    let was_zero = state.price_fp.swap(fp, Ordering::Release) == 0;
                    state.reserve_sol.store(sr, Ordering::Relaxed);
                    state.reserve_token.store(tr, Ordering::Relaxed);
                    state.last_update_ms.store(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_millis() as u64,
                        Ordering::Relaxed,
                    );
                    if was_zero {
                        tracing::info!(
                            mint = %bs58::encode(mint).into_string(),
                            price_fp = fp, sol = sr, token = tr,
                            "[price_feed] first price from RPC poll"
                        );
                    }
                }
            }

            // Spread chunks evenly across the poll interval to avoid request bursts
            if chunk_delay_ms > 0 {
                let elapsed = chunk_start.elapsed().as_millis() as u64;
                if elapsed < chunk_delay_ms {
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        chunk_delay_ms - elapsed,
                    )).await;
                }
            }
        }
    }
}

fn extract_account_data(results: &[serde_json::Value], id: usize) -> Option<Vec<u8>> {
    let entry = results.iter().find(|r| r.get("id").and_then(|i| i.as_u64()) == Some(id as u64))?;
    let arr = entry.pointer("/result/value/data")?.as_array()?;
    let b64 = arr.first()?.as_str()?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

fn parse_spl_amount(data: &[u8]) -> Option<u64> {
    if data.len() < 72 { return None; }
    Some(u64::from_le_bytes(data[64..72].try_into().ok()?))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_from_reserves_basic() {
        let price = price_from_reserves(79_000_000_000, 206_900_000_000_000);
        assert!(price >= 380 && price <= 383, "expected ~381, got {price}");
    }

    #[test]
    fn test_price_from_reserves_zero_token() {
        assert_eq!(price_from_reserves(79_000_000_000, 0), 0);
    }

    #[test]
    fn test_price_from_reserves_zero_sol() {
        assert_eq!(price_from_reserves(0, 206_900_000_000_000), 0);
    }

    #[test]
    fn test_price_from_reserves_overflow_safety() {
        let price = price_from_reserves(10_000_000_000_000, 1_000_000_000_000_000);
        assert_eq!(price, 10_000);
    }

    #[test]
    fn test_parse_spl_amount_valid() {
        let mut data = vec![0u8; 165];
        let amount: u64 = 1_234_567_890;
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        assert_eq!(parse_spl_amount(&data), Some(amount));
    }

    #[test]
    fn test_parse_spl_amount_too_short() {
        assert_eq!(parse_spl_amount(&vec![0u8; 71]), None);
    }

    #[tokio::test]
    async fn test_price_feed_manager_creates() {
        let (manager, handle) = PriceFeedManager::new(
            "https://invalid.example.com".to_string(),
            String::new(),
            500,
        );
        assert_eq!(manager.prices.len(), 0);
        assert!(manager.current_price(&[0u8; 32]).is_none());
        handle.abort();
    }
}
