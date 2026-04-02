# RPC Sender Wiring Plan — Momentum Engine

> **Status:** PLAN — do not implement until reviewed  
> **Date:** 2026-04-01  
> **Author:** Apollo (subagent: engineer-4-wiring)  
> **File:** `rust/pump-quant-core/src/momentum/mod.rs` (3466 lines)  

---

## Table of Contents

1. [Overview](#1-overview)
2. [Current Architecture](#2-current-architecture)
3. [RpcSender API Reference](#3-rpcsender-api-reference)
4. [Structural Changes (MomentumEngine)](#4-structural-changes-momentumengine)
5. [Site 1: buy_task (Raydium buy)](#5-site-1-buy_task-raydium-buy)
6. [Site 2: buy_pumpswap (PumpSwap buy)](#6-site-2-buy_pumpswap-pumpswap-buy)
7. [Site 3: sell_raydium (Raydium sell)](#7-site-3-sell_raydium-raydium-sell)
8. [Site 4: sell_pumpswap (PumpSwap sell)](#8-site-4-sell_pumpswap-pumpswap-sell)
9. [Fields to Remove](#9-fields-to-remove)
10. [New Imports](#10-new-imports)
11. [Config Reconciliation Note](#11-config-reconciliation-note)
12. [Testing Plan](#12-testing-plan)

---

## 1. Overview

Wire `RpcSender` (already implemented in `momentum/rpc_sender.rs`) into the 4 transaction submission sites in `momentum/mod.rs`. The flow changes from:

```
Current:  Build TX → Jito bundle → (on failure) rpc_fallback_send()
New:      Build TX → RpcSender.submit_tx() → (JitoFallback) Jito bundle → (sell: last-resort RPC)
```

**Key invariant for sells:** Sells MUST land. Tokens in wallet = open risk. The sell paths use a 3-tier escalation:
1. `RpcSender.submit_tx()` (3 internal retries + circuit breaker)
2. Jito bundle (existing path)
3. Last-resort raw RPC with elevated priority fee

**Key behavior for buys:** Buys are best-effort. Timeout is acceptable (position is already tracked; the buy may still land).

---

## 2. Current Architecture

### 2.1 The `rpc_fallback_send()` free function (lines 124-157)

```rust
// Line 124-157
async fn rpc_fallback_send(
    client: &reqwest::Client,
    rpc_url: &str,
    tx_bytes: &[u8],
    mint_b58: &str,
    label: &str,
) {
    // base64-encodes tx_bytes, POSTs sendTransaction to rpc_url
    // Logs success/failure, fire-and-forget (no confirmation)
}
```

This function will be **kept temporarily** as a last-resort sell fallback, but its callers will be removed. Sells use it as tier-3. Buys no longer need it.

### 2.2 Four Jito submission sites

| # | Label | Lines (spawn block) | Pool Type | Submit Line | Fallback Line |
|---|-------|---------------------|-----------|-------------|---------------|
| 1 | `[buy_task]` | 1122-1159 | Raydium | 1150 | 1158 |
| 2 | `[buy_pumpswap]` | 1197-1240 | PumpSwap | 1225 | 1239 |
| 3 | `[sell_raydium]` | 2290-2335 | Raydium | 2322 (JitoOnly), 2333 (Nozomi) | 2326 |
| 4 | `[sell_pumpswap]` | 2377-2422 | PumpSwap | 2409 (JitoOnly), 2420 (Nozomi) | 2413 |

### 2.3 Fields used for fallback (to be replaced)

| Field | Struct line | Constructor line | Used at |
|-------|-------------|------------------|---------|
| `rpc_fallback_client: reqwest::Client` | 229 | 306, 331 | 1120, 1195, 2288, 2375 |
| `rpc_fallback_url: Arc<String>` | 231 | 302-304, 332 | 1121, 1196, 2289, 2376 |

---

## 3. RpcSender API Reference

From `momentum/rpc_sender.rs`:

```rust
pub struct RpcSender {
    client: reqwest::Client,
    rpc_url: String,
    metrics: Arc<RwLock<SubmissionMetrics>>,
    circuit: Arc<RwLock<CircuitState>>,
    config: RpcSenderConfig,  // from rpc_sender.rs (not config.rs — see §11)
}

pub enum SubmitResult {
    Landed { signature: String, latency_ms: u64 },
    TimedOut { signature: String },
    Failed { error: String },
    JitoFallback { bundle_id: Option<String> },
}

impl RpcSender {
    pub fn new(rpc_url: String, config: RpcSenderConfig) -> Self;
    pub async fn submit_tx(&self, tx_bytes: &[u8], mint_str: &str, label: &str) -> SubmitResult;
    pub async fn metrics(&self) -> SubmissionMetrics;
    pub async fn record_jito_landed(&self, tip_lamports: u64);
}
```

**Important behavior:**
- `submit_tx()` checks circuit breaker first; if OPEN → returns `JitoFallback` immediately
- Retries up to `max_send_retries` (default 3) on retryable errors (blockhash not found, rate limit, etc.)
- Polls `getSignatureStatuses` for confirmation up to `confirm_timeout_ms` (default 30s)
- On consecutive failures ≥ `circuit_breaker_threshold` (default 5) → trips breaker

---

## 4. Structural Changes (MomentumEngine)

### 4.1 Add `rpc_sender` field to struct (line ~231)

**Current (lines 228-232):**
```rust
    // ── RPC fallback for sendTransaction when Jito/Nozomi fail ──────
    /// Shared reqwest::Client for RPC fallback (created once, reused).
    rpc_fallback_client: reqwest::Client,
    /// RPC URL for fallback sendTransaction (SOLANA_RPC_URL or default).
    rpc_fallback_url: Arc<String>,
```

**Replacement:**
```rust
    // ── RPC primary sender (replaces rpc_fallback_client + rpc_fallback_url) ──
    /// RPC transaction sender with retry, confirmation, and circuit breaker.
    /// Wraps sendTransaction + getSignatureStatuses with configurable retry/backoff.
    rpc_sender: Arc<crate::momentum::rpc_sender::RpcSender>,
```

### 4.2 Update constructor `new()` (lines 258-357)

**Add parameter (line 266, after `blockhash_cache`):**
```rust
    pub fn new(
        config: Arc<MomentumConfig>,
        rpc_url: Arc<String>,
        helius_wss_url: String,
        log_path: &str,
        jito_grpc: Option<Arc<crate::tx::jito_grpc::JitoGrpcClient>>,
        nozomi_client: Option<Arc<crate::tx::nozomi::NozomiClient>>,
        wallet_pubkey: Option<[u8; 32]>,
        blockhash_cache: Arc<crate::tx::executor::BlockhashCache>,
        rpc_sender: Arc<crate::momentum::rpc_sender::RpcSender>,  // ← NEW
    ) -> (Self, crossbeam_channel::Sender<ScoredToken>, tokio::task::JoinHandle<()>, std::thread::JoinHandle<()>) {
```

**Remove old fallback construction (lines 301-306):**
```rust
        // REMOVE these lines:
        let rpc_fallback_url = Arc::new(
            std::env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        );
        let rpc_fallback_client = reqwest::Client::new();
```

**Update struct initialization (lines 331-332):**
```rust
        // REMOVE:
        //     rpc_fallback_client,
        //     rpc_fallback_url,
        // REPLACE with:
            rpc_sender,
```

### 4.3 Caller update (main.rs or wherever `MomentumEngine::new()` is called)

The caller must construct the `RpcSender` and pass it in:

```rust
// In main.rs (or wherever the engine is constructed):
use pump_quant_core::momentum::rpc_sender::{RpcSender, RpcSenderConfig as RpcSenderRsConfig};

let rpc_sender_config = RpcSenderRsConfig {
    priority_fee_microlamports: momentum_config.rpc_sender.priority_fee_microlamports,
    max_send_retries: momentum_config.rpc_sender.max_send_retries,
    retry_delay_ms: momentum_config.rpc_sender.retry_delay_ms,
    confirm_timeout_ms: momentum_config.rpc_sender.confirm_timeout_ms,
    skip_preflight: momentum_config.rpc_sender.skip_preflight,
    circuit_breaker_threshold: momentum_config.rpc_sender.circuit_breaker_threshold,
    circuit_breaker_cooldown_ms: momentum_config.rpc_sender.circuit_breaker_cooldown_ms,
    jito_fallback_tip: momentum_config.rpc_sender.jito_fallback_tip,
};
let rpc_url = std::env::var("SOLANA_RPC_URL")
    .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
let rpc_sender = Arc::new(RpcSender::new(rpc_url, rpc_sender_config));

let (engine, scored_tx, poll_handle, logger_handle) = MomentumEngine::new(
    config,
    rpc_url_arc,
    helius_wss_url,
    log_path,
    jito_grpc,
    nozomi_client,
    wallet_pubkey,
    blockhash_cache,
    rpc_sender,  // ← NEW
);
```

---

## 5. Site 1: buy_task (Raydium buy)

### 5.1 Available variables at submission point

Inside the `tokio::spawn(async move { ... })` block starting at line 1122:
- `tx_bytes: Vec<u8>` — serialized signed transaction (from `build_raydium_buy_tx`)
- `tx_b58: String` — base58 encoding (for Jito)
- `mint_buy: [u8; 32]` — mint pubkey bytes
- `jg: Arc<JitoGrpcClient>` — Jito gRPC client
- `rpc_fb_client: reqwest::Client` — ← REMOVING
- `rpc_fb_url: Arc<String>` — ← REMOVING
- `tip: u64`, `size: u64`, `tokens_est: u64` — for logging

### 5.2 Current code (lines 1118-1159)

```rust
                    // Line 1120-1121
                    let rpc_fb_client = self.rpc_fallback_client.clone();
                    let rpc_fb_url = self.rpc_fallback_url.clone();
                    // Line 1122
                    tokio::spawn(async move {
                        // ... keypair loading (lines 1123-1136) ...
                        // ... build tx (lines 1137-1148) ...
                        // Line 1149: base58 encode
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        // Lines 1150-1159: Jito submit + fallback
                        match jg.submit_bundle(&tx_b58).await {
                            Ok(id) => {
                                tracing::info!(mint=%bs58::encode(&mint_buy).into_string(), bundle_id=%id, tip, size_sol=size as f64/1e9, tokens_est, "[buy_task] Jito submitted");
                            }
                            Err(e) => {
                                tracing::warn!(mint=%bs58::encode(&mint_buy).into_string(), err=?e, "[buy_task] Jito FAILED — trying RPC fallback");
                                rpc_fallback_send(&rpc_fb_client, &rpc_fb_url, &tx_bytes, &bs58::encode(&mint_buy).into_string(), "buy_task").await;
                            }
                        }
                    });
```

### 5.3 Replacement code

**Replace lines 1120-1121** (clone preparation before `tokio::spawn`):
```rust
                    let rpc_sender = self.rpc_sender.clone();
```

**Replace lines 1149-1159** (inside the spawn, after `tx_bytes` is built):

```rust
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        let mint_str = bs58::encode(&mint_buy).into_string();

                        // ── Tier 1: RPC sender (3 retries + circuit breaker) ──
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "buy_task").await {
                            crate::momentum::rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(
                                    mint=%mint_str, %signature, latency_ms, tip,
                                    size_sol=size as f64/1e9, tokens_est,
                                    "[buy_task] RPC LANDED"
                                );
                            }
                            crate::momentum::rpc_sender::SubmitResult::TimedOut { signature } => {
                                // Buy timeout is acceptable — tx may still land
                                tracing::warn!(
                                    mint=%mint_str, %signature, tip,
                                    size_sol=size as f64/1e9, tokens_est,
                                    "[buy_task] RPC timed out (may still land)"
                                );
                            }
                            crate::momentum::rpc_sender::SubmitResult::JitoFallback { .. } => {
                                // Circuit breaker open → fall back to Jito
                                tracing::info!(mint=%mint_str, "[buy_task] circuit breaker → Jito fallback");
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => {
                                        tracing::info!(
                                            mint=%mint_str, bundle_id=%id, tip,
                                            size_sol=size as f64/1e9, tokens_est,
                                            "[buy_task] Jito submitted (fallback)"
                                        );
                                        rpc_sender.record_jito_landed(tip).await;
                                    }
                                    Err(e) => {
                                        tracing::error!(mint=%mint_str, err=?e, "[buy_task] Jito fallback also FAILED");
                                    }
                                }
                            }
                            crate::momentum::rpc_sender::SubmitResult::Failed { error } => {
                                // RPC exhausted retries → try Jito as last resort
                                tracing::warn!(mint=%mint_str, %error, "[buy_task] RPC FAILED → trying Jito");
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => {
                                        tracing::info!(
                                            mint=%mint_str, bundle_id=%id, tip,
                                            size_sol=size as f64/1e9, tokens_est,
                                            "[buy_task] Jito submitted (RPC failed)"
                                        );
                                        rpc_sender.record_jito_landed(tip).await;
                                    }
                                    Err(e2) => {
                                        tracing::error!(mint=%mint_str, err=?e2, "[buy_task] ALL submission paths FAILED");
                                    }
                                }
                            }
                        }
                    });
```

---

## 6. Site 2: buy_pumpswap (PumpSwap buy)

### 6.1 Available variables at submission point

Inside the `tokio::spawn(async move { ... })` block starting at line 1197:
- `tx_bytes: Vec<u8>` — from `build_pumpswap_buy_tx`
- `tx_b58: String` — base58 for Jito
- `mint_buy: [u8; 32]` — mint pubkey
- `jg: Arc<JitoGrpcClient>`
- `rpc_fb_client` / `rpc_fb_url` — ← REMOVING
- `tip: u64`, `size: u64` — for logging
- `fee_idx: usize` — PumpSwap fee account index

### 6.2 Current code (lines 1195-1240)

```rust
                    // Lines 1195-1196
                    let rpc_fb_client = self.rpc_fallback_client.clone();
                    let rpc_fb_url = self.rpc_fallback_url.clone();
                    // Line 1197
                    tokio::spawn(async move {
                        // ... keypair loading (lines 1198-1211) ...
                        // ... build tx (lines 1212-1222) ...
                        // Line 1223: base58 encode
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        // Lines 1225-1240: Jito submit + fallback
                        match jg.submit_bundle(&tx_b58).await {
                            Ok(id) => tracing::info!(
                                mint=%bs58::encode(&mint_buy).into_string(),
                                bundle_id=%id, tip, size_sol=size as f64/1e9,
                                "[buy_pumpswap] Jito submitted"
                            ),
                            Err(e) => {
                                tracing::warn!(
                                    mint=%bs58::encode(&mint_buy).into_string(),
                                    err=?e, "[buy_pumpswap] Jito FAILED — trying RPC fallback"
                                );
                                rpc_fallback_send(&rpc_fb_client, &rpc_fb_url, &tx_bytes, &bs58::encode(&mint_buy).into_string(), "buy_pumpswap").await;
                            }
                        }
                    });
```

### 6.3 Replacement code

**Replace lines 1195-1196** (clone preparation):
```rust
                    let rpc_sender = self.rpc_sender.clone();
```

**Replace lines 1223-1240** (inside spawn, after `tx_bytes` built):

```rust
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        let mint_str = bs58::encode(&mint_buy).into_string();

                        // ── Tier 1: RPC sender (3 retries + circuit breaker) ──
                        match rpc_sender.submit_tx(&tx_bytes, &mint_str, "buy_pumpswap").await {
                            crate::momentum::rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(
                                    mint=%mint_str, %signature, latency_ms, tip,
                                    size_sol=size as f64/1e9,
                                    "[buy_pumpswap] RPC LANDED"
                                );
                            }
                            crate::momentum::rpc_sender::SubmitResult::TimedOut { signature } => {
                                tracing::warn!(
                                    mint=%mint_str, %signature, tip,
                                    size_sol=size as f64/1e9,
                                    "[buy_pumpswap] RPC timed out (may still land)"
                                );
                            }
                            crate::momentum::rpc_sender::SubmitResult::JitoFallback { .. } => {
                                tracing::info!(mint=%mint_str, "[buy_pumpswap] circuit breaker → Jito fallback");
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => {
                                        tracing::info!(
                                            mint=%mint_str, bundle_id=%id, tip,
                                            size_sol=size as f64/1e9,
                                            "[buy_pumpswap] Jito submitted (fallback)"
                                        );
                                        rpc_sender.record_jito_landed(tip).await;
                                    }
                                    Err(e) => {
                                        tracing::error!(mint=%mint_str, err=?e, "[buy_pumpswap] Jito fallback also FAILED");
                                    }
                                }
                            }
                            crate::momentum::rpc_sender::SubmitResult::Failed { error } => {
                                tracing::warn!(mint=%mint_str, %error, "[buy_pumpswap] RPC FAILED → trying Jito");
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => {
                                        tracing::info!(
                                            mint=%mint_str, bundle_id=%id, tip,
                                            size_sol=size as f64/1e9,
                                            "[buy_pumpswap] Jito submitted (RPC failed)"
                                        );
                                        rpc_sender.record_jito_landed(tip).await;
                                    }
                                    Err(e2) => {
                                        tracing::error!(mint=%mint_str, err=?e2, "[buy_pumpswap] ALL submission paths FAILED");
                                    }
                                }
                            }
                        }
                    });
```

---

## 7. Site 3: sell_raydium (Raydium sell)

### 7.1 Available variables at submission point

Inside the `tokio::spawn(async move { ... })` block starting at line 2290:
- `tx_bytes: Vec<u8>` — from `build_raydium_sell_tx`
- `tx_b64: String` — base64 for Nozomi
- `tx_b58: String` — base58 for Jito
- `mint_copy: [u8; 32]` — mint pubkey
- `jg: Arc<JitoGrpcClient>`
- `noz: Option<Arc<NozomiClient>>`
- `rpc_fb_client` / `rpc_fb_url` — ← REMOVING
- `landing: LandingPath` — from `route_exit()`
- `reason_str: String`, `gain: i64`, `noz_ok: bool`
- `tip: u64`, `tokens: u64`, `min_sol_out: u64`

### 7.2 Current code (lines 2288-2335)

```rust
                    // Lines 2288-2289
                    let rpc_fb_client = self.rpc_fallback_client.clone();
                    let rpc_fb_url = self.rpc_fallback_url.clone();
                    // Line 2290
                    tokio::spawn(async move {
                        // ... keypair loading (lines 2291-2304) ...
                        // ... build tx (lines 2305-2315) ...
                        // Lines 2316-2317: base64 + base58 encode
                        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        // Lines 2319-2335: LandingPath routing
                        let landing = route_exit(&reason_str, gain, noz_ok);
                        match landing {
                            LandingPath::JitoOnly => {
                                match jg.submit_bundle(&tx_b58).await {
                                    Ok(id) => tracing::info!(..., "[sell_raydium] Jito submitted"),
                                    Err(e) => {
                                        tracing::warn!(..., "[sell_raydium] Jito FAILED — trying RPC fallback");
                                        rpc_fallback_send(&rpc_fb_client, &rpc_fb_url, &tx_bytes, ..., "sell_raydium").await;
                                    }
                                }
                            }
                            LandingPath::NozomiOnly | LandingPath::DualPath => {
                                if let Some(ref n) = noz {
                                    match n.send_transaction(&tx_b64).await {
                                        Ok(_) => tracing::info!(..., "[sell_raydium] Nozomi OK"),
                                        Err(e) => { tracing::warn!(...); let _ = jg.submit_bundle(&tx_b58).await; }
                                    }
                                }
                            }
                        }
                    });
```

### 7.3 Replacement code

**CRITICAL: Sells must succeed.** Three-tier escalation:
1. RpcSender (3 retries internally)
2. Jito bundle
3. Last-resort raw RPC (reuse the existing `rpc_fallback_send` free function with the `RpcSender`'s internal client, or a dedicated last-resort call)

Since `rpc_fallback_send` needs a `reqwest::Client` and URL, and we're removing those fields, we have two options:
- **Option A:** Keep `rpc_fallback_send` but pass `RpcSender`'s internal fields (would require making them `pub`)
- **Option B:** Add a `pub fn raw_send_once(&self, tx_bytes, mint_str, label)` method to `RpcSender` that does a single fire-and-forget sendTransaction without retries/confirmation — a cleaner API

**Recommendation: Option B** — add `raw_send_once()` to `RpcSender`. This preserves encapsulation and gives sells a last-resort path. (See §7.4 below.)

**Replace lines 2288-2289** (clone preparation):
```rust
                    let rpc_sender = self.rpc_sender.clone();
```

**Replace lines 2316-2335** (inside spawn, after `tx_bytes` built):

```rust
                        use base64::Engine as _;
                        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);
                        let tx_b58 = bs58::encode(&tx_bytes).into_string();
                        let mint_str = bs58::encode(&mint_copy).into_string();

                        // ── Sell Tier 1: RPC sender (3 retries + circuit breaker) ──
                        let rpc_result = rpc_sender.submit_tx(&tx_bytes, &mint_str, "sell_raydium").await;

                        let mut landed = false;
                        match rpc_result {
                            crate::momentum::rpc_sender::SubmitResult::Landed { signature, latency_ms } => {
                                tracing::info!(
                                    mint=%mint_str, %signature, latency_ms,
                                    reason=%reason_str, gain_bps=gain,
                                    "[sell_raydium] RPC LANDED"
                                );
                                landed = true;
                            }
                            crate::momentum::rpc_sender::SubmitResult::TimedOut { signature } => {
                                // Sell timeout is DANGEROUS — tokens still in wallet
                                tracing::warn!(
                                    mint=%mint_str, %signature,
                                    reason=%reason_str, gain_bps=gain,
                                    "[sell_raydium] RPC TIMED OUT — escalating to Jito"
                                );
                            }
                            crate::momentum::rpc_sender::SubmitResult::JitoFallback { .. } => {
                                tracing::info!(mint=%mint_str, "[sell_raydium] circuit breaker → Jito fallback");
                            }
                            crate::momentum::rpc_sender::SubmitResult::Failed { error } => {
                                tracing::warn!(
                                    mint=%mint_str, %error,
                                    "[sell_raydium] RPC FAILED — escalating to Jito"
                                );
                            }
                        }

                        // ── Sell Tier 2: Jito bundle (if RPC didn't land) ──
                        if !landed {
                            let landing = route_exit(&reason_str, gain, noz_ok);
                            let jito_ok = match landing {
                                LandingPath::NozomiOnly | LandingPath::DualPath => {
                                    // Try Nozomi first, Jito as backup
                                    if let Some(ref n) = noz {
                                        match n.send_transaction(&tx_b64).await {
                                            Ok(_) => {
                                                tracing::info!(mint=%mint_str, "[sell_raydium] Nozomi OK (tier 2)");
                                                true
                                            }
                                            Err(e) => {
                                                tracing::warn!(err=?e, "[sell_raydium] Nozomi failed → trying Jito");
                                                match jg.submit_bundle(&tx_b58).await {
                                                    Ok(id) => {
                                                        tracing::info!(mint=%mint_str, bundle_id=%id, "[sell_raydium] Jito submitted (tier 2)");
                                                        rpc_sender.record_jito_landed(tip).await;
                                                        true
                                                    }
                                                    Err(e2) => {
                                                        tracing::warn!(mint=%mint_str, err=?e2, "[sell_raydium] Jito also failed (tier 2)");
                                                        false
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        // No Nozomi client, try Jito directly
                                        match jg.submit_bundle(&tx_b58).await {
                                            Ok(id) => {
                                                tracing::info!(mint=%mint_str, bundle_id=%id, "[sell_raydium] Jito submitted (tier 2)");
                                                rpc_sender.record_jito_landed(tip).await;
                                                true
                                            }
                                            Err(e) => { tracing::warn!(mint=%mint_str, err=?e, "[sell_raydium] Jito failed (tier 2)"); false }
                                        }
                                    }
                                }
                                LandingPath::JitoOnly => {
                                    match jg.submit_bundle(&tx_b58).await {
                                        Ok(id)