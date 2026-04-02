# Wallet Balance Safety System — Production Spec

**Author:** Apollo (Citadel-grade quant systems design)  
**Date:** 2026-04-01  
**Status:** Ready for implementation  
**Priority:** CRITICAL — blocks live deployment

---

## Table of Contents

1. [Algorithm Design](#1-algorithm-design)
2. [Config Parameters](#2-config-parameters)
3. [Engineer 1 Spec: Balance Cache + Background Poller](#3-engineer-1-balance-cache--background-poller)
4. [Engineer 2 Spec: Pre-Entry Balance Gate](#4-engineer-2-pre-entry-balance-gate)
5. [Engineer 3 Spec: Config Additions](#5-engineer-3-config-additions)
6. [Engineer 4 Spec: Kelly Sizing Integration](#6-engineer-4-kelly-sizing-integration)
7. [Integration Order + Dependencies](#7-integration-order--dependencies)
8. [Pre-Live Testing Checklist](#8-pre-live-testing-checklist)

---

## 1. Algorithm Design

### 1.1 Data Structures

#### New fields on `MomentumEngine`:

```rust
// ── Wallet Balance Safety System ────────────────────────────────
/// Cached wallet SOL balance in lamports. Updated by background poller.
/// AtomicU64 for lock-free reads on the hot path (on_graduation, on_tick).
wallet_balance_lamports: Arc<AtomicU64>,

/// Trading paused flag. Set when balance < min_wallet_balance_lamports.
/// AtomicBool for lock-free reads. Auto-clears when balance recovers.
trading_paused: Arc<AtomicBool>,

/// Timestamp (ms) of last successful balance fetch. Used for staleness detection.
balance_last_updated_ms: Arc<AtomicU64>,

/// Handle to the background balance poller task (for graceful shutdown).
balance_poller_handle: Option<tokio::task::JoinHandle<()>>,
```

#### Why `Arc<AtomicU64>` not just `AtomicU64`

The balance cache background task runs in a separate tokio spawn. It needs a shared reference to update the value. `Arc<AtomicU64>` is cloned into the spawned task. The engine reads via `Ordering::Relaxed` — we don't need sequential consistency, just "recent enough" (within 10s).

### 1.2 Balance Cache Polling Logic

```
┌─────────────────────────────────────────────────────────────┐
│ STARTUP (synchronous, before event loop)                     │
│                                                              │
│ 1. rpc.getBalance(wallet_pubkey) → balance_lamports          │
│ 2. wallet_balance_lamports.store(balance_lamports)           │
│ 3. log: "wallet balance: {balance_lamports / 1e9:.4} SOL"   │
│ 4. if balance < min_wallet_balance_lamports:                 │
│      trading_paused.store(true)                              │
│      log WARN: "balance below minimum, trading paused"       │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ BACKGROUND TASK (tokio::spawn, runs forever)                 │
│                                                              │
│ loop {                                                       │
│   sleep(balance_poll_interval_s)  // default: 10s            │
│                                                              │
│   match rpc.getBalance(wallet_pubkey) {                      │
│     Ok(new_balance) => {                                     │
│       wallet_balance_lamports.store(new_balance)             │
│       balance_last_updated_ms.store(now_ms)                  │
│                                                              │
│       // Circuit breaker: auto-pause / auto-resume           │
│       if new_balance < min_wallet_balance_lamports {         │
│         if !trading_paused.load() {                          │
│           trading_paused.store(true)                         │
│           log WARN: "CIRCUIT BREAKER: balance {new_balance}  │
│                      < min {min}, trading HALTED"            │
│         }                                                    │
│       } else {                                               │
│         if trading_paused.load() {                           │
│           trading_paused.store(false)                        │
│           log INFO: "balance recovered to {new_balance},     │
│                      trading RESUMED"                        │
│         }                                                    │
│       }                                                      │
│                                                              │
│       // Warning threshold                                   │
│       if new_balance < balance_warning_lamports {            │
│         log WARN: "balance low: {new_balance / 1e9:.4} SOL" │
│       }                                                      │
│     }                                                        │
│     Err(e) => {                                              │
│       // DON'T panic. DON'T pause. Use last known balance.   │
│       log WARN: "balance poll failed: {e}, using cached"     │
│       // After 5 consecutive failures, log ERROR             │
│       consecutive_failures += 1;                             │
│       if consecutive_failures >= 5 {                         │
│         log ERROR: "balance poll failed 5x consecutive"      │
│       }                                                      │
│     }                                                        │
│   }                                                          │
│ }                                                            │
└─────────────────────────────────────────────────────────────┘
```

**Key design decisions:**

- **10s poll interval**: `getBalance` is a lightweight RPC call (~5ms). At 10s interval, that's 8,640 calls/day — well within any rate limit. The worst-case staleness is 10s, during which at most 1 graduation event could slip through (graduations are ~10-20/day = one every 4-8 minutes).
- **On error: keep last balance**: RPC flap should NOT halt trading. The balance changes slowly (only on our own buys/sells). A 30s stale balance is fine.
- **Auto-resume**: When the poller sees balance recover above min, it clears `trading_paused`. This means depositing SOL into the wallet auto-resumes without operator intervention.

### 1.3 Pre-Entry Balance Gate Formula

This is the critical safety check. It runs in the `process_pending_entries()` method, AFTER `if !self.config.paper_mode` and BEFORE the buy tx submission.

```
// All values in lamports (u64)

size_lamports        = position size (from compute_size_lamports or probe_size_sol)
tip_lamports         = tip_engine.compute_tip(TipRequest { context: Entry, ... })
tx_fee_lamports      = 5_000                     // Solana base fee (5000 lamports = 0.000005 SOL)
ata_rent_lamports    = 2_039_280                  // ATA creation rent (worst case — token account may not exist)

raw_required         = size_lamports + tip_lamports + tx_fee_lamports + ata_rent_lamports

// 10% safety margin on top (covers fee variance, blockhash retry, unforeseen priority fee spikes)
safety_margin        = raw_required / 10          // integer division, 10% buffer

total_required       = raw_required + safety_margin

// The gate:
wallet_balance       = wallet_balance_lamports.load(Relaxed)

if wallet_balance < total_required {
    log WARN: "[balance_gate] insufficient balance for entry: have={wallet_balance/1e9:.4} SOL, \
               need={total_required/1e9:.4} SOL (size={size_lamports/1e9:.4}, tip={tip_lamports}, \
               fee={tx_fee_lamports}, ata={ata_rent_lamports}, margin={safety_margin})"
    // CLEANUP: undo the position that was just inserted into active
    active.remove(&entry.mint);
    momentum_zones.remove(&entry.mint);
    reserve_sol_ctx.remove(&entry.mint);
    price_feed.unsubscribe_sync(&entry.mint);
    entries_opened.fetch_sub(1, Relaxed);
    continue;  // skip to next pending entry
}
```

**Exact constants:**

| Component | Lamports | SOL | Rationale |
|-----------|----------|-----|-----------|
| `TX_FEE_BASE` | 5,000 | 0.000005 | Solana base tx fee (1 signature) |
| `ATA_RENT_EXEMPT` | 2,039,280 | ~0.00204 | Worst case: creating a new token account |
| `SAFETY_MARGIN_PCT` | 10% | — | Covers fee variance + retry overhead |

**Why ATA rent?** The buy tx creates an Associated Token Account (ATA) for the purchased token if the wallet doesn't already have one. This costs rent-exempt minimum (~0.00204 SOL). It's a one-time cost per token mint but must be budgeted.

**Tip estimation:** Use the same `TipEngine::compute_tip()` with `TipContext::Entry` that the buy path already uses. The tip engine returns a value based on the `TipConfig::entry_tip` (100 μSOL base) + proportional rate. For a 0.05 SOL position, this gives ~250k lamports max tip.

**Worked example at 0.05 SOL probe size:**

```
size_lamports   =  50,000,000  (0.05 SOL)
tip_lamports    =     250,000  (entry tip for 0.05 SOL position)
tx_fee_lamports =       5,000
ata_rent        =   2,039,280

raw_required    =  52,294,280  (0.05229 SOL)
safety_margin   =   5,229,428  (10%)

total_required  =  57,523,708  (0.05752 SOL)
```

So with 5 concurrent positions at probe size: `5 × 0.0575 ≈ 0.288 SOL` needed.

### 1.4 Kelly Integration Formula

The existing `kelly_sizing.rs` has a complete implementation with:
- 2D LUT bilinear interpolation for win probability `p` and reward ratio `R`
- Half-Kelly fraction with correlation adjustment (Thorp approximation, ρ=0.25)
- Drawdown scaling
- Fee-adjusted R via `fee_adjust_r()`
- Position sizing: `f × wallet_balance / 1000`
- Clamp to `[MIN_SIZE_LAMPORTS, MAX_SIZE_LAMPORTS]`

The `BankrollSource::Live` variant already exists with `AtomicU64` for cached balance.

**Bootstrap logic:**

```
kelly_sizing_enabled: bool     // config flag (default: false)
kelly_min_trades: u32          // minimum trades before Kelly activates (default: 30)

fn determine_position_size(mint, grad_score, wallet_balance) -> u64 {
    if !config.kelly_sizing_enabled {
        // Fixed sizing — current behavior
        return compute_size_lamports(mint, grad_score);
    }

    let completed_trades = count_completed_trades();  // from paper_logger or trade counter
    
    if completed_trades < config.kelly_min_trades {
        // Bootstrap phase: use fixed probe size
        // Not enough data for reliable Kelly estimates
        return (config.probe_size_sol * 1e9) as u64;
    }

    // Compute Kelly conviction from recent trade statistics
    let stats = compute_recent_trade_stats();  // from data/momentum_paper_trades.jsonl
    let conviction = kelly_sizing::compute_conviction(
        stats.magnitude_score,   // avg magnitude of recent winning trades
        stats.entry_score,       // avg entry quality score
        wallet_balance,          // cached wallet balance
        active.len() as u8,     // current open positions
        compute_drawdown_pct(), // current drawdown from HWM
    );

    // Clamp to momentum-specific bounds
    let min_lamports = (config.kelly_min_probe_sol * 1e9) as u64;
    let max_lamports = (config.kelly_max_probe_sol * 1e9) as u64;
    
    conviction.size_lamports.clamp(min_lamports, max_lamports)
}
```

**Win rate computation from trade history:**

```rust
struct RecentTradeStats {
    trade_count: u32,
    win_rate: f64,          // wins / total (win = net_pnl_sol > 0)
    avg_win_bps: f64,       // average gain_bps on winning trades
    avg_loss_bps: f64,      // average |gain_bps| on losing trades
    magnitude_score: f64,   // proxy for Kelly magnitude input
    entry_score: f64,       // proxy for Kelly entry_score input
}

fn compute_recent_trade_stats(trades: &[ClosedTrade], lookback: usize) -> RecentTradeStats {
    let recent = &trades[trades.len().saturating_sub(lookback)..];
    let wins: Vec<_> = recent.iter().filter(|t| t.net_pnl_sol > 0.0).collect();
    let losses: Vec<_> = recent.iter().filter(|t| t.net_pnl_sol <= 0.0).collect();
    
    RecentTradeStats {
        trade_count: recent.len() as u32,
        win_rate: wins.len() as f64 / recent.len().max(1) as f64,
        avg_win_bps: wins.iter().map(|t| t.raw_gain_bps as f64).sum::<f64>() / wins.len().max(1) as f64,
        avg_loss_bps: losses.iter().map(|t| t.raw_gain_bps.abs() as f64).sum::<f64>() / losses.len().max(1) as f64,
        // Map win_rate → score range that kelly_sizing LUT expects
        magnitude_score: (win_rate * 100.0).clamp(40.0, 80.0),
        entry_score: 60.0,  // neutral default; could be refined
    }
}
```

### 1.5 Ghost Position Detection

**On startup (live mode only):**

```
fn detect_ghost_positions(
    active: &DashMap<[u8; 32], MomentumPosition>,
    wallet_pubkey: &Pubkey,
    rpc_url: &str,
) -> Vec<[u8; 32]> {
    // Fetch all token accounts owned by wallet
    let token_accounts = rpc.getTokenAccountsByOwner(
        wallet_pubkey,
        TokenAccountsFilter::ProgramId(spl_token::id()),
    );
    
    // Build set of mints where we hold > 0 tokens
    let held_mints: HashSet<[u8; 32]> = token_accounts
        .iter()
        .filter(|acct| acct.token_amount.amount > 0)
        .map(|acct| acct.mint.to_bytes())
        .collect();
    
    // Any active position whose mint is NOT in held_mints is a ghost
    let ghosts: Vec<[u8; 32]> = active
        .iter()
        .filter(|entry| !held_mints.contains(entry.key()))
        .map(|entry| *entry.key())
        .collect();
    
    ghosts
}
```

**Cleanup:**

```
for ghost_mint in ghosts {
    log WARN: "[ghost_detect] ghost position found: mint={ghost_mint_b58} — \
               active in engine but 0 tokens held on-chain. Force-closing at 0 PnL."
    // Force close at entry price (0 PnL) to keep books clean
    if let Some((_, pos)) = active.remove(&ghost_mint) {
        close_position(ghost_mint, MomentumExitReason::TimeSl, pos.entry_price_fp, now_ms);
    }
}
```

**Periodic check (every 60s during on_tick):**

```
// In on_tick(), every ~60s (400 ticks at 150ms):
if !config.paper_mode && tick_num % 400 == 0 && active.len() > 0 {
    tokio::spawn(ghost_detection_sweep(...));
}
```

This is conservative (60s interval) because `getTokenAccountsByOwner` is a heavier RPC call.

---

## 2. Config Parameters

| Field | Type | Default | Validation | Description |
|-------|------|---------|------------|-------------|
| `balance_poll_interval_s` | `u64` | `10` | `>= 5` | Seconds between wallet balance RPC polls |
| `min_wallet_balance_lamports` | `u64` | `100_000_000` | `>= 10_000_000` | Circuit breaker floor (0.1 SOL default) |
| `balance_warning_lamports` | `u64` | `500_000_000` | `> min_wallet_balance_lamports` | Log warning when below this (0.5 SOL) |
| `balance_safety_margin_pct` | `u8` | `10` | `1..=50` | Safety margin percentage on top of required lamports |
| `balance_gate_enabled` | `bool` | `true` | — | Master toggle for balance gate (disable for testing) |
| `ata_rent_lamports` | `u64` | `2_039_280` | — | ATA creation rent-exempt minimum |
| `tx_fee_base_lamports` | `u64` | `5_000` | — | Solana base transaction fee |
| `kelly_sizing_enabled` | `bool` | `false` | — | Enable Kelly criterion position sizing |
| `kelly_min_trades` | `u32` | `30` | `>= 10` | Minimum completed trades before Kelly activates |
| `kelly_min_probe_sol` | `f64` | `0.01` | `> 0.0` | Kelly floor: minimum position size |
| `kelly_max_probe_sol` | `f64` | `0.10` | `> kelly_min_probe_sol` | Kelly ceiling: maximum position size |
| `kelly_lookback_trades` | `u32` | `100` | `>= kelly_min_trades` | Number of recent trades for win rate computation |
| `ghost_detection_enabled` | `bool` | `true` | — | Enable ghost position detection (live mode only) |
| `ghost_detection_interval_ticks` | `u64` | `400` | `>= 100` | Ticks between ghost detection sweeps (~60s at 150ms) |

---

## 3. Engineer 1: Balance Cache + Background Poller

### File: `momentum/balance_cache.rs` (NEW)

**Purpose:** Self-contained balance cache with background polling, usable by `MomentumEngine`.

```rust
//! Wallet balance cache with background RPC polling.
//!
//! Provides a lock-free AtomicU64 balance readable from the hot path.
//! Background task polls getBalance every N seconds.
//! On RPC failure: uses last known balance (no trading disruption).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Shared balance state — cloned into both the engine and the poller task.
#[derive(Clone)]
pub struct BalanceCache {
    /// Cached SOL balance in lamports.
    balance_lamports: Arc<AtomicU64>,
    /// Whether trading is paused due to low balance.
    trading_paused: Arc<AtomicBool>,
    /// Epoch-ms of last successful balance fetch.
    last_updated_ms: Arc<AtomicU64>,
}

impl BalanceCache {
    /// Create a new BalanceCache with balance 0, not paused.
    pub fn new() -> Self {
        Self {
            balance_lamports: Arc::new(AtomicU64::new(0)),
            trading_paused: Arc::new(AtomicBool::new(false)),
            last_updated_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read current cached balance (lock-free, Relaxed ordering).
    #[inline(always)]
    pub fn balance(&self) -> u64 {
        self.balance_lamports.load(Ordering::Relaxed)
    }

    /// Check if trading is paused due to low balance.
    #[inline(always)]
    pub fn is_trading_paused(&self) -> bool {
        self.trading_paused.load(Ordering::Relaxed)
    }

    /// Epoch-ms of last successful balance update.
    #[inline(always)]
    pub fn last_updated_ms(&self) -> u64 {
        self.last_updated_ms.load(Ordering::Relaxed)
    }

    /// Update the cached balance (called by poller or startup).
    pub fn update(&self, new_balance: u64, now_ms: u64) {
        self.balance_lamports.store(new_balance, Ordering::Relaxed);
        self.last_updated_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Set or clear the trading_paused flag.
    pub fn set_trading_paused(&self, paused: bool) {
        self.trading_paused.store(paused, Ordering::Relaxed);
    }
}
```

**Startup sync fetch function:**

```rust
/// Fetch wallet balance once synchronously at startup.
/// Blocks until the RPC responds or times out (5s).
/// Returns the balance in lamports, or an error.
pub async fn fetch_balance_once(
    http_client: &reqwest::Client,
    rpc_url: &str,
    wallet_pubkey_b58: &str,
) -> anyhow::Result<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBalance",
        "params": [wallet_pubkey_b58, {"commitment": "confirmed"}]
    });

    let resp = http_client
        .post(rpc_url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;

    // Parse response: { "result": { "value": <lamports> } }
    let json: serde_json::Value = resp.json().await?;
    let lamports = json["result"]["value"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("getBalance: missing result.value"))?;

    Ok(lamports)
}
```

**Background poller task:**

```rust
/// Spawn the background balance poller. Returns a JoinHandle for shutdown.
pub fn spawn_balance_poller(
    cache: BalanceCache,
    rpc_url: String,
    wallet_pubkey_b58: String,
    poll_interval_s: u64,
    min_balance_lamports: u64,
    warning_balance_lamports: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut consecutive_failures: u32 = 0;
        let interval = tokio::time::Duration::from_secs(poll_interval_s);

        loop {
            tokio::time::sleep(interval).await;

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            match fetch_balance_once(&client, &rpc_url, &wallet_pubkey_b58).await {
                Ok(balance) => {
                    consecutive_failures = 0;
                    cache.update(balance, now_ms);

                    // Circuit breaker logic
                    if balance < min_balance_lamports {
                        if !cache.is_trading_paused() {
                            cache.set_trading_paused(true);
                            tracing::warn!(
                                balance_sol = balance as f64 / 1e9,
                                min_sol = min_balance_lamports as f64 / 1e9,
                                "[balance_cache] CIRCUIT BREAKER: balance below minimum — trading HALTED"
                            );
                        }
                    } else if cache.is_trading_paused() {
                        cache.set_trading_paused(false);
                        tracing::info!(
                            balance_sol = balance as f64 / 1e9,
                            "[balance_cache] balance recovered — trading RESUMED"
                        );
                    }

                    // Warning threshold
                    if balance < warning_balance_lamports && balance >= min_balance_lamports {
                        tracing::warn!(
                            balance_sol = balance as f64 / 1e9,
                            "[balance_cache] wallet balance LOW"
                        );
                    }

                    tracing::debug!(
                        balance_sol = balance as f64 / 1e9,
                        "[balance_cache] balance updated"
                    );
                }
                Err(e) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= 5 {
                        tracing::error!(
                            err = ?e,
                            consecutive_failures,
                            "[balance_cache] balance poll failed 5+ times — using stale cache"
                        );
                    } else {
                        tracing::warn!(
                            err = ?e,
                            "[balance_cache] balance poll failed — using cached value"
                        );
                    }
                }
            }
        }
    })
}
```

### Changes to `MomentumEngine` (`momentum/mod.rs`):

1. **Add field to struct:**
   ```rust
   balance_cache: BalanceCache,
   ```

2. **In `MomentumEngine::new()`:**
   - Accept `balance_cache: BalanceCache` parameter
   - Store it in the engine struct
   - The caller (`main.rs`) is responsible for:
     a. Creating the `BalanceCache`
     b. Calling `fetch_balance_once()` before constructing the engine
     c. Calling `spawn_balance_poller()` after constructing the engine

3. **Add `pub mod balance_cache;` to `momentum/mod.rs`**

### Changes to `main.rs` (startup sequence):

```rust
// After loading config, before creating MomentumEngine:
let balance_cache = BalanceCache::new();

if !momentum_config.paper_mode {
    // Sync fetch at startup — block until we know the wallet balance
    let wallet_pubkey_b58 = bs58::encode(&wallet_pubkey.unwrap()).into_string();
    let initial_balance = balance_cache::fetch_balance_once(
        &http_client, &rpc_url, &wallet_pubkey_b58
    ).await
    .expect("FATAL: cannot fetch wallet balance at startup");
    
    let now_ms = /* current epoch ms */;
    balance_cache.update(initial_balance, now_ms);
    
    tracing::info!(
        balance_sol = initial_balance as f64 / 1e9,
        "[startup] wallet balance loaded"
    );
    
    if initial_balance < momentum_config.min_wallet_balance_lamports {
        balance_cache.set_trading_paused(true);
        tracing::warn!(
            "[startup] wallet balance below minimum — trading starts PAUSED"
        );
    }
}

// Create engine with balance_cache
let (engine, scored_tx, ws_handle, logger_handle) = MomentumEngine::new(
    config, rpc_url, helius_wss, log_path,
    jito_grpc, nozomi, wallet_pubkey, blockhash_cache,
    balance_cache.clone(),  // <-- NEW PARAMETER
);

// Spawn background poller (live mode only)
if !momentum_config.paper_mode {
    let poller_handle = balance_cache::spawn_balance_poller(
        balance_cache.clone(),
        rpc_url.to_string(),
        wallet_pubkey_b58,
        momentum_config.balance_poll_interval_s,
        momentum_config.min_wallet_balance_lamports,
        momentum_config.balance_warning_lamports,
    );
    // Store handle for graceful shutdown if needed
}
```

### Testing for Engineer 1:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balance_cache_read_write() {
        let cache = BalanceCache::new();
        assert_eq!(cache.balance(), 0);
        cache.update(1_000_000_000, 12345);
        assert_eq!(cache.balance(), 1_000_000_000);
        assert_eq!(cache.last_updated_ms(), 12345);
    }

    #[test]
    fn test_trading_paused_flag() {
        let cache = BalanceCache::new();
        assert!(!cache.is_trading_paused());
        cache.set_trading_paused(true);
        assert!(cache.is_trading_paused());
        cache.set_trading_paused(false);
        assert!(!cache.is_trading_paused());
    }

    #[tokio::test]
    async fn test_fetch_balance_once_integration() {
        // Requires HELIUS_API_KEY and network access — mark #[ignore] for CI
        // Test against devnet or mainnet with a known wallet
    }
}
```

---

## 4. Engineer 2: Pre-Entry Balance Gate

### Insertion Point

In `momentum/mod.rs`, method `process_pending_entries()`, at approximately line 869 (the exact location within the live buy block).

**Current code (simplified):**
```rust
// Live mode: submit buy tx via Raydium AMM V4 + Jito
if !self.config.paper_mode {
    if let Some(pool) = self.raydium_pools.get(&entry.mint).map(|r| r.clone()) {
        // ... build and submit tx (NO BALANCE CHECK) ...
    }
}
```

**Modified code — insert BEFORE the pool lookup:**

```rust
// Live mode: submit buy tx via Raydium AMM V4 + Jito
if !self.config.paper_mode {
    // ── BALANCE GATE ────────────────────────────────────────
    // Check wallet has sufficient balance before attempting buy.
    // Prevents ghost positions from silently-failing on-chain txs.
    if self.config.balance_gate_enabled {
        let wallet_bal = self.balance_cache.balance();

        // Compute tip using same logic as the buy path
        let tip_req = crate::tx::tip_engine::TipRequest {
            context: crate::tx::tip_engine::TipContext::Entry,
            size_lamports: size_lamports,
            gain_bps: 0,
            grad_score: entry.grad_score as f64,
        };
        let estimated_tip = self.tip_engine.lock().compute_tip(&tip_req);

        let raw_required = size_lamports
            + estimated_tip
            + self.config.tx_fee_base_lamports
            + self.config.ata_rent_lamports;

        let safety_margin = raw_required / (100 / self.config.balance_safety_margin_pct as u64).max(1);
        let total_required = raw_required + safety_margin;

        if wallet_bal < total_required {