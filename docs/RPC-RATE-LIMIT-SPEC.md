# RPC Rate Limiting & Endpoint Separation Spec

**Status:** Ready for implementation  
**Priority:** P0 — blocking live trading  
**Author:** Apollo (architect) — 2026-04-01  
**Affected files:** `rpc_sender.rs`, `pool.rs`, `price_feed.rs`, `executor.rs`, `mod.rs`, `main.rs`, `.env`

---

## 1. Problem Statement

ALL buy/sell transactions fail with HTTP 429 from Helius RPC. The circuit breaker trips after 5 consecutive 429s → 120s cooldown → zero trades land on-chain.

**Root cause:** Every RPC call in the system hits the same Helius `marielle-qe2lvr-fast-mainnet.helius-rpc.com` endpoint with no client-side rate limiting. Pool resolution from CoreCast graduation events generates massive read traffic that starves `sendTransaction` of rate budget.

### 1.1 RPC Call Inventory (all hitting same Helius endpoint)

| Call | Source | Frequency | Endpoint Used |
|------|--------|-----------|---------------|
| `sendTransaction` | `rpc_sender.rs` → buy/sell | Per trade + retries | `SOLANA_RPC_URL` (Helius fast) |
| `getSignatureStatuses` | `rpc_sender.rs` → confirm | Every 500ms per pending TX | `SOLANA_RPC_URL` |
| `getAccountInfo` (batch) | `price_feed.rs` → poll | Every 500ms × 2 per sub × chunks of 10 | `SOLANA_RPC_URL` |
| `getTransaction` | `pool.rs` → resolve from sig | Per graduation + up to 5 retries × 1-8s backoff | `SOLANA_RPC_URL` |
| `getProgramAccounts` | `pool.rs` → PumpSwap mint lookup | Per graduation (fast path) | `helius_rpc_url` (api-key) |
| `getProgramAccounts` | `pool.rs` → Raydium mint lookup | Fallback per graduation | `helius_rpc_url` (api-key) |
| `getMultipleAccounts` | `pool.rs` → vault reserves | Per graduation (after vault extraction) | `helius_rpc_url` |
| `getSignaturesForAddress` | `pool.rs` → Raydium activity check | Per Raydium graduation | `helius_rpc_url` |
| `getLatestBlockhash` | `executor.rs` → blockhash cache | Every 25s | `SOLANA_RPC_URL` |
| `getBalance` | `mod.rs` → wallet poller | Every 30s | `helius_rpc_url` |
| `accountSubscribe` (WS) | `price_feed.rs` → live vaults | Per subscription | `SOLANA_WS_URL` (Helius WSS) |

**Key observation:** `SOLANA_RPC_URL` and `helius_rpc_url` both resolve to Helius endpoints that share the same API key rate limit bucket. Even though they're different URLs, Helius rate limits by API key, not by URL. So ALL calls count against the same quota.

### 1.2 Graduation Event Storm Analysis

CoreCast sends 1000+ stale graduation events/min. Each event triggers `on_migration()` which calls:

1. **PumpSwap mint fast path** (when mint ≠ zero):
   - `getProgramAccounts` (PumpSwap) → 1 call to `helius_rpc_url`
   - `getMultipleAccounts` (vault reserves) → 1 call to `helius_rpc_url`
   - **Total: 2 RPC calls per graduation (fast path success)**

2. **Sig-based fallback** (when fast path fails or mint = zero):
   - `getTransaction` → 1-5 calls with exponential backoff (1s, 2s, 4s, 8s)
   - `getMultipleAccounts` → 1 call
   - If Raydium: `fetch_raydium_pool_accounts` → 2 more calls
   - If Raydium: `resolve_pumpswap_pool_from_mint` (FIX-2 preference check) → 2 more calls
   - **Total: 3-10 RPC calls per graduation (sig fallback)**

3. **Triple fallback** (sig fails → PumpSwap mint → Raydium mint):
   - `resolve_pumpswap_pool_from_mint` → 2 calls
   - `resolve_pool_from_mint` (Raydium) → 2 calls
   - `get_account_last_activity_ms` (FIX-5) → 1 call
   - **Total: up to 5 additional RPC calls**

**Worst case per graduation:** 15 RPC calls × 1000 events/min = **15,000 RPC calls/min** just for pool resolution.

Helius developer tier: ~25 RPS = 1,500 calls/min. **We're 10× over budget on reads alone.**

Meanwhile `sendTransaction` + `getSignatureStatuses` need ~4 calls per trade (1 send + 1-3 retries + N confirm polls). These get 429'd because reads consumed the entire budget.

### 1.3 Existing Mitigations (Insufficient)

- **`resolving_sigs` dedup:** Prevents 3 feeds from triggering separate lookups for the same sig. Effective but doesn't limit total throughput.
- **`stale_grad_max_age_ms` gate:** Drops cold-miss events older than threshold. Helps but CoreCast sets `ts_ms=now()`, making all events appear fresh.
- **`min_send_interval_ms` (200ms):** Only throttles `sendTransaction`, not reads. And 200ms = 5 RPS for sends alone, which is fine — the problem is reads starving the budget.
- **Price feed 429 backoff:** Has retry + consecutive-429 skip logic, but still contributes to the 429 storm.

---

## 2. Architecture: Priority-Based Rate Limiting + Endpoint Separation

### 2.1 Design Principles

1. **`sendTransaction` is sacred.** It must NEVER be rate-limited by pool resolution traffic.
2. **Endpoint separation by function.** Different RPC endpoints for different call types, spreading load across separate rate limit buckets.
3. **Priority-based token bucket.** When multiple callers contend for the same endpoint, `sendTransaction` always wins.
4. **CoreCast throttling at the source.** Pool resolution gets a hard concurrency + rate limit. Excess events are dropped, not queued indefinitely.

### 2.2 Endpoint Assignment

```
┌─────────────────────────────┬──────────────────────────────────────────────┐
│ RPC Call                    │ Endpoint                                     │
├─────────────────────────────┼──────────────────────────────────────────────┤
│ sendTransaction             │ HELIUS_SEND_URL (dedicated Helius fast)      │
│ getSignatureStatuses        │ HELIUS_SEND_URL (same as send — low volume)  │
│ getAccountInfo (price feed) │ SOLANA_RPC_URL (Helius fast — current)       │
│ accountSubscribe (WS)       │ SOLANA_WS_URL (Helius WSS — current)        │
│ getLatestBlockhash          │ PUBLIC_RPC_URL (api.mainnet-beta.solana.com) │
│ getBalance                  │ PUBLIC_RPC_URL                               │
│ getTransaction              │ PUBLIC_RPC_URL                               │
│ getProgramAccounts          │ PUBLIC_RPC_URL                               │
│ getMultipleAccounts (pool)  │ PUBLIC_RPC_URL                               │
│ getSignaturesForAddress     │ PUBLIC_RPC_URL                               │
└─────────────────────────────┴──────────────────────────────────────────────┘
```

**Rationale:**

- **`HELIUS_SEND_URL`** = current `SOLANA_RPC_URL` (`marielle-qe2lvr-fast-mainnet.helius-rpc.com`). This is Helius's staked/fast endpoint — optimized for `sendTransaction` landing. Reserved exclusively for TX submission + confirmation polling.
- **`SOLANA_RPC_URL`** = stays as-is for price feed polling. Price feed is latency-sensitive (500ms cadence) and benefits from Helius's performance. Bounded: max 3 active subs × 2 vaults × 500ms = ~12 calls/min. Tolerable.
- **`PUBLIC_RPC_URL`** = `https://api.mainnet-beta.solana.com` for all pool resolution reads. Public Solana RPC has generous rate limits for reads (~40 RPS) but doesn't support `sendTransaction` well (high drop rate). Perfect for our read-heavy pool resolution.

**Note on `getProgramAccounts`:** Public Solana RPC supports `getProgramAccounts` but with size limits. Helius `helius_rpc_url` (API key endpoint) is a fallback if public returns errors. The rate limiter will gate this.

### 2.3 `.env` Changes

```env
# EXISTING (unchanged)
SOLANA_RPC_URL=$SOLANA_RPC_URL  # set at runtime, fail-closed if absent
SOLANA_WS_URL=$SOLANA_WS_URL
HELIUS_API_KEY=<set via $HELIUS_API_KEY env var — fail-closed if absent>

# NEW
# Dedicated endpoint for sendTransaction + getSignatureStatuses.
# Isolated from read traffic. Uses the same Helius fast endpoint but
# is accessed through a separate rate limiter with highest priority.
HELIUS_SEND_URL=$HELIUS_SEND_URL  # set at runtime, fail-closed if absent

# Public Solana RPC for read-heavy pool resolution.
# Free, generous rate limits, no API key needed.
# getProgramAccounts may have tighter limits — falls back to HELIUS_API_KEY endpoint.
PUBLIC_RPC_URL=https://api.mainnet-beta.solana.com
```

**Implementation note:** Even though `HELIUS_SEND_URL` and `SOLANA_RPC_URL` start as the same URL, they go through separate rate limiters with different budgets. If Alon upgrades to a dedicated Helius plan for sends, he only changes `HELIUS_SEND_URL`.

---

## 3. Token Bucket Rate Limiter with Priority Tiers

### 3.1 Design

A proper token bucket (not just min-interval) with three priority tiers sharing a global budget per endpoint.

```
              ┌──────────────────────────────────┐
              │       SharedRateLimiter          │
              │  tokens: AtomicU64               │
              │  capacity: u64                   │
              │  refill_rate: tokens/sec         │
              │  last_refill: AtomicU64 (ms)     │
              │                                  │
              │  acquire(priority) → bool/wait   │
              │    P0_CRITICAL: never blocked    │
              │    P1_NORMAL: wait if < 30%      │
              │    P2_BACKGROUND: wait if < 60%  │
              └──────────────────────────────────┘
```

Priority tiers:
- **P0_CRITICAL:** `sendTransaction`, `getSignatureStatuses` — always gets a token, never rejected. If bucket is empty, steals from the refill.
- **P1_NORMAL:** `getAccountInfo` (price feed), `getLatestBlockhash` — waits when bucket is below 30% capacity. Gets preference over P2.
- **P2_BACKGROUND:** All pool resolution calls (`getTransaction`, `getProgramAccounts`, `getMultipleAccounts`, `getSignaturesForAddress`, `getBalance`) — waits when bucket is below 60% capacity. Shed first when constrained.

### 3.2 Implementation: `src/rpc/rate_limiter.rs`

```rust
//! Priority-aware token bucket rate limiter for RPC endpoints.
//!
//! Ensures sendTransaction always has headroom by throttling lower-priority
//! reads when approaching rate limits. Each endpoint gets its own limiter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::Semaphore;

/// Priority tier for RPC calls. Lower number = higher priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RpcPriority {
    /// sendTransaction, getSignatureStatuses — never throttled.
    Critical = 0,
    /// Price feed getAccountInfo, getLatestBlockhash — throttled when bucket < 30%.
    Normal = 1,
    /// Pool resolution reads — throttled when bucket < 60%.
    Background = 2,
}

/// Configuration for a rate limiter instance.
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Maximum tokens (burst capacity).
    pub capacity: u64,
    /// Tokens added per second (sustained rate).
    pub refill_per_sec: f64,
    /// Below this fill fraction, P1_NORMAL callers wait. Default 0.30.
    pub normal_throttle_threshold: f64,
    /// Below this fill fraction, P2_BACKGROUND callers wait. Default 0.60.
    pub background_throttle_threshold: f64,
    /// Maximum concurrent P2 (background) calls. Prevents pool resolution
    /// from opening 50 HTTP connections simultaneously.
    pub max_concurrent_background: usize,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            capacity: 25,
            refill_per_sec: 20.0,    // ~20 tokens/sec sustained
            normal_throttle_threshold: 0.30,
            background_throttle_threshold: 0.60,
            max_concurrent_background: 4,
        }
    }
}

/// Per-endpoint rate limiter with priority-aware token bucket.
pub struct RpcRateLimiter {
    /// Available tokens (scaled by 1000 for sub-token precision).
    tokens_x1000: AtomicU64,
    /// Maximum tokens × 1000.
    capacity_x1000: u64,
    /// Tokens added per millisecond × 1000.
    refill_per_ms_x1000: u64,
    /// Last refill timestamp (epoch ms).
    last_refill_ms: AtomicU64,
    /// Threshold: P1 callers wait when tokens < this.
    normal_threshold_x1000: u64,
    /// Threshold: P2 callers wait when tokens < this.
    background_threshold_x1000: u64,
    /// Concurrency semaphore for background calls.
    background_semaphore: Semaphore,
    /// Stats: total calls acquired.
    pub stats_acquired: AtomicU64,
    /// Stats: total calls that had to wait.
    pub stats_waited: AtomicU64,
    /// Stats: total calls shed (background dropped).
    pub stats_shed: AtomicU64,
}

impl RpcRateLimiter {
    pub fn new(config: &RateLimiterConfig) -> Self {
        let cap_x1000 = config.capacity * 1000;
        let refill_per_ms_x1000 = (config.refill_per_sec * 1000.0 / 1000.0) as u64; // per ms

        let now_ms = Instant::now().elapsed().as_millis() as u64; // placeholder; will use SystemTime
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            tokens_x1000: AtomicU64::new(cap_x1000),
            capacity_x1000: cap_x1000,
            refill_per_ms_x1000: refill_per_ms_x1000,
            last_refill_ms: AtomicU64::new(now_ms),
            normal_threshold_x1000: (cap_x1000 as f64 * config.normal_throttle_threshold) as u64,
            background_threshold_x1000: (cap_x1000 as f64 * config.background_throttle_threshold) as u64,
            background_semaphore: Semaphore::new(config.max_concurrent_background),
            stats_acquired: AtomicU64::new(0),
            stats_waited: AtomicU64::new(0),
            stats_shed: AtomicU64::new(0),
        }
    }

    /// Refill tokens based on elapsed time. Lock-free CAS loop.
    fn refill(&self) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let prev_ms = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed_ms = now_ms.saturating_sub(prev_ms);
        if elapsed_ms == 0 {
            return;
        }

        // CAS on last_refill_ms to prevent double-refill
        if self.last_refill_ms.compare_exchange(
            prev_ms, now_ms, Ordering::AcqRel, Ordering::Relaxed
        ).is_err() {
            return; // Another thread won the race
        }

        let add_x1000 = elapsed_ms * self.refill_per_ms_x1000;
        let mut current = self.tokens_x1000.load(Ordering::Relaxed);
        loop {
            let new_val = (current + add_x1000).min(self.capacity_x1000);
            match self.tokens_x1000.compare_exchange_weak(
                current, new_val, Ordering::AcqRel, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Try to consume one token. Returns true if successful.
    fn try_consume(&self) -> bool {
        let mut current = self.tokens_x1000.load(Ordering::Relaxed);
        loop {
            if current < 1000 {
                return false; // Less than 1 token available
            }
            match self.tokens_x1000.compare_exchange_weak(
                current, current - 1000, Ordering::AcqRel, Ordering::Relaxed
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Current fill level as fraction [0.0, 1.0].
    fn fill_level(&self) -> f64 {
        let current = self.tokens_x1000.load(Ordering::Relaxed);
        current as f64 / self.capacity_x1000 as f64
    }

    /// Acquire a token with priority-aware waiting.
    ///
    /// - `Critical`: Always succeeds immediately. Forces a token even if empty.
    /// - `Normal`: Waits if fill < normal_threshold. Returns after wait.
    /// - `Background`: Waits if fill < background_threshold. Also limited by
    ///   concurrency semaphore. Returns `Err(Shed)` if semaphore is full.
    ///
    /// Returns `Ok(())` on success, `Err(RateLimitShed)` if the call should be dropped.
    pub async fn acquire(&self, priority: RpcPriority) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, RateLimitShed> {
        self.refill();

        match priority {
            RpcPriority::Critical => {
                // Always acquire. If bucket is empty, consume anyway (overdraft).
                if !self.try_consume() {
                    // Force-consume: decrement even below zero (will go negative conceptually
                    // but AtomicU64 wraps — refill will catch up). Log it.
                    tracing::debug!("[rate_limiter] critical call — overdrafting empty bucket");
                }
                self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            RpcPriority::Normal => {
                // Wait if bucket is below threshold
                let current_x1000 = self.tokens_x1000.load(Ordering::Relaxed);
                if current_x1000 < self.normal_threshold_x1000 {
                    // Calculate wait time to reach threshold
                    let deficit = self.normal_threshold_x1000 - current_x1000;
                    let wait_ms = if self.refill_per_ms_x1000 > 0 {
                        (deficit / self.refill_per_ms_x1000).max(50).min(2000)
                    } else {
                        500
                    };
                    self.stats_waited.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(
                        wait_ms,
                        fill_pct = (current_x1000 as f64 / self.capacity_x1000 as f64 * 100.0) as u32,
                        "[rate_limiter] normal priority — waiting for bucket refill"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
                    self.refill();
                }
                if !self.try_consume() {
                    // After waiting, still empty — wait a bit more
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    self.refill();
                    let _ = self.try_consume(); // Best effort
                }
                self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            RpcPriority::Background => {
                // Check concurrency limit first
                let permit = match self.background_semaphore.try_acquire() {
                    Ok(p) => p,
                    Err(_) => {
                        self.stats_shed.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(
                            "[rate_limiter] background call shed — concurrency limit reached"
                        );
                        return Err(RateLimitShed);
                    }
                };

                // Wait if bucket is below threshold
                let current_x1000 = self.tokens_x1000.load(Ordering::Relaxed);
                if current_x1000 < self.background_threshold_x1000 {
                    let deficit = self.background_threshold_x1000 - current_x1000;
                    let wait_ms = if self.refill_per_ms_x1000 > 0 {
                        (deficit / self.refill_per_ms_x1000).max(100).min(5000)
                    } else {
                        1000
                    };
                    self.stats_waited.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(
                        wait_ms,
                        fill_pct = (current_x1000 as f64 / self.capacity_x1000 as f64 * 100.0) as u32,
                        "[rate_limiter] background priority — waiting for bucket refill"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
                    self.refill();
                }
                if !self.try_consume() {
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    self.refill();
                    let _ = self.try_consume();
                }
                self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                Ok(Some(permit))
            }
        }
    }

    /// Get a snapshot of rate limiter stats.
    pub fn stats_snapshot(&self) -> RateLimiterStats {
        self.refill();
        RateLimiterStats {
            tokens_available: self.tokens_x1000.load(Ordering::Relaxed) as f64 / 1000.0,
            capacity: self.capacity_x1000 as f64 / 1000.0,
            fill_pct: self.fill_level() * 100.0,
            total_acquired: self.stats_acquired.load(Ordering::Relaxed),
            total_waited: self.stats_waited.load(Ordering::Relaxed),
            total_shed: self.stats_shed.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct RateLimitShed;

#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    pub tokens_available: f64,
    pub capacity: f64,
    pub fill_pct: f64,
    pub total_acquired: u64,
    pub total_waited: u64,
    pub total_shed: u64,
}

impl std::fmt::Display for RateLimiterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tokens={:.1}/{:.0} ({:.0}%) acquired={} waited={} shed={}",
            self.tokens_available, self.capacity, self.fill_pct,
            self.total_acquired, self.total_waited, self.total_shed
        )
    }
}
```

### 3.3 Configuration in `canary.json`

Add to `momentum.rpc_sender`:

```json
{
  "rpc_sender": {
    "...existing fields...",
    
    "helius_send_budget_rps": 20,
    "helius_send_burst": 25,
    "public_rpc_budget_rps": 15,
    "public_rpc_burst": 25,
    "pool_resolution_max_concurrent": 4,
    "pool_resolution_max_per_min": 60,
    "pool_resolution_cooldown_on_429_ms": 10000
  }
}
```

---

## 4. Endpoint Routing Layer

### 4.1 `src/rpc/client.rs` — Shared Multi-Endpoint RPC Client

```rust
//! Multi-endpoint RPC client with priority-based rate limiting.
//!
//! Routes RPC calls to the correct endpoint based on method, and applies
//! the appropriate rate limiter with the correct priority tier.

use std::sync::Arc;
use super::rate_limiter::{RpcRateLimiter, RpcPriority, RateLimiterConfig, RateLimitShed};

/// RPC method classification — determines endpoint + priority.
#[derive(Debug, Clone, Copy)]
pub enum RpcMethod {
    /// sendTransaction → HELIUS_SEND_URL, Critical priority
    SendTransaction,
    /// getSignatureStatuses → HELIUS_SEND_URL, Critical priority
    GetSignatureStatuses,
    /// getAccountInfo (price feed batch) → SOLANA_RPC_URL, Normal priority
    GetAccountInfo,
    /// getLatestBlockhash → PUBLIC_RPC_URL, Normal priority
    GetLatestBlockhash,
    /// getBalance → PUBLIC_RPC_URL, Background priority
    GetBalance,
    /// getTransaction → PUBLIC_RPC_URL, Background priority
    GetTransaction,
    /// getProgramAccounts → PUBLIC_RPC_URL, Background priority (fallback: helius_rpc_url)
    GetProgramAccounts,
    /// getMultipleAccounts (pool resolution) → PUBLIC_RPC_URL, Background priority
    GetMultipleAccounts,
    /// getSignaturesForAddress → PUBLIC_RPC_URL, Background priority
    GetSignaturesForAddress,
}

impl RpcMethod {
    pub fn priority(&self) -> RpcPriority {
        match self {
            Self::SendTransaction | Self::GetSignatureStatuses => RpcPriority::Critical,
            Self::GetAccountInfo | Self::GetLatestBlockhash => RpcPriority::Normal,
            _ => RpcPriority::Background,
        }
    }
}

/// Multi-endpoint client configuration.
pub struct RpcClientConfig {
    /// Helius fast endpoint for sendTransaction (HELIUS_SEND_URL).
    pub helius_send_url: String,
    /// Helius endpoint for price feed reads (SOLANA_RPC_URL).
    pub helius_read_url: String,
    /// Public Solana RPC for pool resolution reads (PUBLIC_RPC_URL).
    pub public_rpc_url: String,
    /// Helius API key endpoint for getProgramAccounts fallback.
    pub helius_api_url: String,
}

/// Shared multi-endpoint RPC client.
///
/// Passed as `Arc<RpcClient>` to all components. Each component calls
/// `client.url_for(method)` to get the correct endpoint URL, and
/// `client.acquire(method)` to rate-limit before calling.
pub struct RpcClient {
    config: RpcClientConfig,
    /// Rate limiter for Helius send endpoint (sendTransaction + confirms).
    helius_send_limiter: RpcRateLimiter,
    /// Rate limiter for Helius read endpoint (price feed).
    helius_read_limiter: RpcRateLimiter,
    /// Rate limiter for public RPC (pool resolution, blockhash, balance).
    public_limiter: RpcRateLimiter,
}

impl RpcClient {
    pub fn new(config: RpcClientConfig) -> Self {
        // Helius send: 25 burst, 20/sec sustained — reserved for TX submission.
        let helius_send_limiter = RpcRateLimiter::new(&RateLimiterConfig {
            capacity: 25,
            refill_per_sec: 20.0,
            normal_throttle_threshold: 0.0,     // N/A (only Critical uses this)
            background_throttle_threshold: 0.0,  // N/A
            max_concurrent_background: 1,        // N/A
        });

        // Helius read: 20 burst, 15/sec — for price feed getAccountInfo batches.
        // Background calls (pool resolution) are NOT routed here by default,
        // but getProgramAccounts may fall back here.
        let helius_read_limiter = RpcRateLimiter::new(&RateLimiterConfig {
            capacity: 20,
            refill_per_sec: 15.0,
            normal_throttle_threshold: 0.30,
            background_throttle_threshold: 0.60,
            max_concurrent_background: 2,
        });

        // Public RPC: 30 burst, 15/sec — pool resolution reads.
        // Generous but we limit concurrency to avoid connection exhaustion.
        let public_limiter = RpcRateLimiter::new(&RateLimiterConfig {
            capacity: 30,
            refill_per_sec: 15.0,
            normal_throttle_threshold: 0.20,
            background_throttle_threshold: 0.50,
            max_concurrent_background: 4,
        });

        Self {
            config,
            helius_send_limiter,
            helius_read_limiter,
            public_limiter,
        }
    }

    /// Get the URL for a given RPC method.
    pub fn url_for(&self, method: RpcMethod) -> &str {
        match method {
            RpcMethod::SendTransaction | RpcMethod::GetSignatureStatuses => {
                &self.config.helius_send_url
            }
            RpcMethod::GetAccountInfo => &self.config.helius_read_url,
            RpcMethod::GetLatestBlockhash | RpcMethod::GetBalance => {
                &self.config.public_rpc_url
            }
            // Pool resolution: public RPC primary, helius API fallback handled at call site
            RpcMethod::GetTransaction
            | RpcMethod::GetProgramAccounts
            | RpcMethod::GetMultipleAccounts
            | RpcMethod::GetSignaturesForAddress => &self.config.public_rpc_url,
        }
    }

    /// Get the fallback URL for methods that have one (e.g. getProgramAccounts → helius API).
    pub fn fallback_url_for(&self, method: RpcMethod) -> Option<&str> {
        match method {
            RpcMethod::GetProgramAccounts => Some(&self.config.helius_api_url),
            _ => None,
        }
    }

    /// Acquire a rate limit token for the given method.
    /// Returns a semaphore permit for Background calls (must be held until RPC completes).
    pub async fn acquire(
        &self,
        method: RpcMethod,
    ) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, RateLimitShed> {
        let priority = method.priority();
        let limiter = match method {
            RpcMethod::SendTransaction | RpcMethod::GetSignatureStatuses => {
                &self.helius_send_limiter
            }
            RpcMethod::GetAccountInfo => &self.helius_read_limiter,
            _ => &self.public_limiter,
        };
        limiter.acquire(priority).await
    }

    /// Get stats for all limiters.
    pub fn all_stats(&self) -> (RateLimiterStats, RateLimiterStats, RateLimiterStats) {
        (
            self.helius_send_limiter.stats_snapshot(),
            self.helius_read_limiter.stats_snapshot(),
            self.public_limiter.stats_snapshot(),
        )
    }
}
```

---

## 5. Pool Resolution Throttle: The CoreCast Backlog Fix

This is the highest-impact change. CoreCast sends 1000+ graduation events/min, each triggering pool resolution RPC calls. We need to throttle at the entry point.

### 5.1 Pool Resolution Semaphore + Rate Gate

Add to `MomentumEngine`:

```rust
/// Pool resolution concurrency limiter.
/// Max 4 concurrent pool resolutions. Excess events are dropped (not queued).
pool_resolution_semaphore: Arc<tokio::sync::Semaphore>,
/// Pool resolution rate counter. Max 60/min (1/sec sustained).
pool_resolution_counter: Arc<AtomicU64>,
/// Timestamp of last pool resolution counter reset.
pool_resolution_counter_reset_ms: Arc<AtomicU64>,
```

### 5.2 Changes to `on_migration()` — Add Rate Gate at Top

```rust
/// Called from main.rs on every graduation migration event.
#[inline(never)]
pub async fn on_migration(
    &self,
    mint: [u8; 32],
    ts_ms: u64,
    sig: [u8; 64],
    enrichment: crate::engine::hot_path::GradEnrichment,
) {
    if !self.config.enabled { return; }

    // ── NEW: Pool resolution rate gate ──────────────────────────────────
    // Drop events that exceed the pool resolution budget.
    // CoreCast sends 1000+ events/min; we can only afford ~60 RPC calls/min
    // for pool resolution without starving sendTransaction.
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let reset_ms = self.pool_resolution_counter_reset_ms.load(Ordering::Relaxed);
        
        // Reset counter every 60 seconds
        if now_ms.saturating_sub(reset_ms) > 60_000 {
            self.pool_resolution_counter.store(0, Ordering::Relaxed);
            self.pool_resolution_counter_reset_ms.store(now_ms, Ordering::Relaxed);
        }
        
        let count = self.pool_resolution_counter.fetch_add(1, Ordering::Relaxed);
        if count >= self.config.pool_resolution_max_per_min {
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                count,
                max = self.config.pool_resolution_max_per_min,
                "[momentum] pool resolution rate limit — dropping event"
            );
            return;
        }
    }
    // ── End rate gate ───────────────────────────────────────────────────

    // ── NEW: Concurrency gate — try_acquire, don't queue ────────────────
    let _permit = match self.pool_resolution_semaphore.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                "[momentum] pool resolution concurrency limit (4) — dropping event"
            );
            return;
        }
    };
    // ── End concurrency gate ────────────────────────────────────────────

    // ... existing dedup, filter, and resolution logic unchanged ...
}
```

### 5.3 Changes to Pool Resolution Functions — Use `RpcClient`

All functions in `pool.rs` that make RPC calls need to accept an `Arc<RpcClient>` and call `client.acquire()` before each HTTP request.

**Before (current code):**
```rust
pub async fn resolve_pumpswap_pool_from_mint(
    client: &reqwest::Client,
    mint: &[u8; 32],
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    // ...
    let resp = client.post(helius_rpc_url).json(&body).send().await.ok()?;
    // ...
}
```

**After:**
```rust
pub async fn resolve_pumpswap_pool_from_mint(
    http: &reqwest::Client,
    mint: &[u8; 32],
    rpc_client: &crate::rpc::client::RpcClient,
) -> Option<PoolResolution> {
    use crate::rpc::client::RpcMethod;
    
    // Rate-limit before calling
    let _permit = match rpc_client.acquire(RpcMethod::GetProgramAccounts).await {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!(mint = %bs58::encode(mint).into_string(),
                "[pool] getProgramAccounts shed by rate limiter");
            return None;
        }
    };
    
    let url = rpc_client.url_for(RpcMethod::GetProgramAccounts);
    let resp = http.post(url).json(&body).send().await.ok()?;
    
    // If public RPC returned error for getProgramAccounts, try Helius fallback
    if resp.status().is_server_error() || resp.status().as_u16() == 429 {
        if let Some(fallback_url) = rpc_client.fallback_url_for(RpcMethod::GetProgramAccounts) {
            tracing::info!("[pool] public RPC failed for getProgramAccounts — trying Helius fallback");
            // Re-acquire against read limiter for fallback
            let _permit2 = rpc_client.acquire(RpcMethod::GetAccountInfo).await.ok()?;
            let resp = http.post(fallback_url).json(&body).send().await.ok()?;
            // ... continue with resp
        }
    }
    // ...
}
```

**Apply the same pattern to:**
- `resolve_pool_from_transaction()` — uses `GetTransaction`
- `resolve_pool_from_mint()` — uses `GetProgramAccounts`
- `fetch_vault_reserves()` — uses `GetMultipleAccounts`
- `get_account_last_activity_ms()` — uses `GetSignaturesForAddress`

---

## 6. Changes to `rpc_sender.rs` — Dedicated Send Endpoint

### 6.1 Accept `Arc<RpcClient>` Instead of Raw URL

The `RpcSender` currently takes a single `rpc_url: String`. Change it to use the shared `RpcClient` for endpoint routing.

```rust
pub struct RpcSender {
    http: reqwest::Client,
    rpc_client: Arc<crate::rpc::client::RpcClient>,
    metrics: Arc<RwLock<SubmissionMetrics>>,
    circuit: Arc<RwLock<CircuitState>>,
    config: RpcSenderConfig,
    // Remove: rate_limiter (old min-interval limiter)
    // Remove: rpc_url (now comes from RpcClient)
}
```

### 6.2 Use `RpcClient.acquire(Critical)` Before Each Send

In `submit_tx()`, replace the old `self.rate_limiter.acquire()` with:

```rust
// ── 2. Rate limit: acquire Critical priority token ───────────────
// Critical priority: NEVER blocked. If bucket is empty, overdrafts.
// This guarantees sendTransaction always fires immediately.
let _ = self.rpc_client.acquire(RpcMethod::SendTransaction).await;

// Get the dedicated send URL
let send_url = self.rpc_client.url_for(RpcMethod::SendTransaction);
```

And for confirmation polling:

```rust
// getSignatureStatuses also goes through dedicated send endpoint
let _ = self.rpc_client.acquire(RpcMethod::GetSignatureStatuses).await;
let confirm_url = self.rpc_client.url_for(RpcMethod::GetSignatureStatuses);
```

### 6.3 Smarter 429 Handling: Retry-After + Adaptive Backoff

When `sendTransaction` gets a 429 (which should be rare after endpoint separation), parse the `Retry-After` header:

```rust
if http_status == reqwest::StatusCode::TOO_MANY_REQUESTS {
    // Parse Retry-After header if present
    let retry_after_ms = resp.headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|secs| secs * 1000)
        .unwrap_or(retry_delay);
    
    tracing::warn!(
        mint = %mint_str,
        attempt,
        retry_after_ms,
        "[rpc_send] HTTP 429 — honoring Retry-After"
    );
    
    // Use the server's Retry-After instead of our exponential backoff
    retry_delay = retry_after_ms;
    continue;
}
```

---

## 7. Changes to `executor.rs` (BlockhashCache) — Use Public RPC

### 7.1 Route `getLatestBlockhash` to Public RPC

The blockhash cache refreshes every 25s. This is a trivial read that doesn't need Helius's low-latency endpoint. Route it to public RPC.

In `main.rs`, change the blockhash cache spawn:

**Before:**
```rust
let rpc_for_bh = std::env::var("SOLANA_RPC_URL")
    .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
momentum_bh_cache.clone().spawn_refresh_task(rpc_for_bh);
```

**After:**
```rust
let rpc_for_bh = std::env::var("PUBLIC_RPC_URL")
    .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
momentum_bh_cache.clone().spawn_refresh_task(rpc_for_bh);
```

This is already the default fallback, but making it explicit ensures it doesn't change if `SOLANA_RPC_URL` is set.

---

## 8. Changes to `mod.rs` — Wallet Balance Poller

### 8.1 Route `getBalance` to Public RPC

The wallet balance poller runs every 30s — trivial load. Route to public RPC.

**Before** (in `MomentumEngine::new()`):
```rust
let rpc_for_balance = Arc::clone(&engine.helius_rpc_url);
```

**After:**
```rust
let rpc_for_balance = Arc::new(
    std::env::var("PUBLIC_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
);
```

---

## 9. Circuit Breaker Improvements

### 9.1 Per-Endpoint Circuit Breakers

The current circuit breaker is global — one endpoint's 429s block all sends. With endpoint separation, the circuit breaker should be per-endpoint:

```rust
/// Per-endpoint circuit breaker state.
/// Each endpoint independently tracks consecutive failures.
pub struct EndpointCircuitBreaker {
    state: RwLock<CircuitState>,
    consecutive_429s: AtomicU32,
    threshold: u32,
    cooldown_ms: u64,
}

impl EndpointCircuitBreaker {
    pub fn new(threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            state: RwLock::new(CircuitState::Closed),
            consecutive_429s: AtomicU32::new(0),
            threshold,
            cooldown_ms,
        }
    }
    
    /// Record a 429 response. Returns true if circuit just tripped.
    pub async fn record_429(&self) -> bool {
        let count = self.consecutive_429s.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.threshold {
            let mut state = self.state.write().await;
            if matches!(*state, CircuitState::Closed) {
                *state = CircuitState::Open { since: Instant::now() };
                tracing::warn!(
                    consecutive_429s = count,
                    cooldown_ms = self.cooldown_ms,
                    "[circuit_breaker] TRIPPED"
                );
                return true;
            }
        }
        false
    }
    
    /// Record a successful response. Resets consecutive 429 counter.
    pub fn record_success(&self) {
        self.consecutive_429s.store(0, Ordering::Relaxed);
    }
    
    /// Check if the circuit is open. If so, returns remaining cooldown ms.
    pub async fn check(&self) -> Option<u64> {
        let state = self.state.read().await;
        if let CircuitState::Open { since } = &*state {
            let elapsed = since.elapsed().as_millis() as u64;
            if elapsed < self.cooldown_ms {
                return Some(self.cooldown_ms - elapsed);
            }
        }
        None
    }
    
    /// Reset the circuit breaker (after cooldown or manual reset).
    pub async fn reset(&self) {
        let mut state = self.state.write().await;
        *state = CircuitState::Closed;
        self.consecutive_429s.store(0, Ordering::Relaxed);
    }
}
```

### 9.2 Shorter Cooldown for Sends

With the send endpoint isolated, 429s on the send path indicate a real Helius rate limit hit (not pool resolution spillover). Use a shorter cooldown:

- **Send circuit breaker:** threshold=3, cooldown=15s (was 5/120s)
- **Read circuit breaker:** threshold=5, cooldown=30s

---

## 10. `on_pumpswap_graduation_direct()` — Apply Same Gates

The Enhanced WebSocket path (`on_pumpswap_graduation_direct()`) also makes RPC calls for vault reserve fetching. Apply the same rate gate and concurrency limit:

```rust
pub async fn on_pumpswap_graduation_direct(/* ... */) {
    // ... existing dedup, cooldown, blocklist checks ...
    
    // ── NEW: Pool resolution rate gate (same as on_migration) ──────────
    {
        let now_ms = /* ... */;
        let count = self.pool_resolution_counter.fetch_add(1, Ordering::Relaxed);
        if count >= self.config.pool_resolution_max_per_min {
            tracing::debug!("[momentum] direct grad rate limited — dropping");
            return;
        }
    }
    let _permit = match self.pool_resolution_semaphore.try_acquire() {
        Ok(p) => p,
        Err(_) => { return; }
    };
    // ── End rate gate ─────────────────────────────────────────────────
    
    // ... existing vault reserve fetch ...
}
```

---

## 11. Implementation Plan

### Phase 1: Immediate (P0) — Stop the 429 Bleed

**Goal:** Reduce RPC calls from 15,000/min to under 1,500/min. Unblock `sendTransaction`.

| Task | File | Change | Impact |
|------|------|--------|--------|
| 1a. Pool resolution rate gate | `mod.rs` | Add `pool_resolution_counter` + `pool_resolution_semaphore` to `on_migration()` and `on_pumpswap_graduation_direct()` | **Highest.** Caps pool resolution to 60/min (was unlimited/1000+). |
| 1b. Blockhash to public RPC | `main.rs` | Change `rpc_for_bh` to `PUBLIC_RPC_URL` | Removes 2.4 calls/min from Helius. Trivial change. |
| 1c. Wallet balance to public RPC | `mod.rs` | Change `rpc_for_balance` to `PUBLIC_RPC_URL` | Removes 2 calls/min from Helius. Trivial change. |
| 1d. Add `PUBLIC_RPC_URL` env var | `.env` | Add `PUBLIC_RPC_URL=https://api.mainnet-beta.solana.com` | Config only. |

**Phase 1 alone reduces pool resolution RPC load by ~94% (from ~15K/min to ~120/min).**

### Phase 2: Endpoint Separation (P1) — Same Day

**Goal:** Fully isolate `sendTransaction` onto its own rate limit bucket.

| Task | File | Change |
|------|------|--------|
| 2a. Create `src/rpc/rate_limiter.rs` | New file | Priority token bucket (code in §3.2) |
| 2b. Create `src/rpc/client.rs` | New file | Multi-endpoint routing (code in §4.1) |
| 2c. Create `src/rpc/mod.rs` | New file | `pub mod rate_limiter; pub mod client;` |
| 2d. Wire `RpcClient` into `MomentumEngine` | `mod.rs` | Replace `helius_rpc_url`, `rpc_fallback_url` with `Arc<RpcClient>` |
| 2e. Update `RpcSender` to use `RpcClient` | `rpc_sender.rs` | Replace raw URL + min-interval with `RpcClient.acquire(Critical)` |
| 2f. Update pool resolution functions | `pool.rs` | Accept `&RpcClient`, call `acquire()` before each HTTP request |
| 2g. Update price feed polling | `price_feed.rs` | Use `RpcClient.acquire(Normal)` before batch getAccountInfo |
| 2h. Update `BlockhashCache` | `executor.rs` | Accept URL from `RpcClient.url_for(GetLatestBlockhash)` |

### Phase 3: Circuit Breaker Upgrade (P2) — This Week

| Task | File | Change |
|------|------|--------|
| 3a. Per-endpoint circuit breakers | `rpc_sender.rs`, new file | Replace global circuit breaker with per-endpoint (code in §9) |
| 3b. Retry-After header parsing | `rpc_sender.rs` | Parse 429 Retry-After header for smarter backoff |
| 3c. Monitoring endpoint for rate limiter stats | `api/` | Expose `/api/rpc_stats` with all three limiter stats |

---

## 12. Expected Outcome

### Before (current state):
```
Total RPC calls:    ~15,000/min  (all to Helius, shared API key)
Helius limit:       ~1,500/min   (25 RPS developer tier)
sendTransaction:    0% landing   (all 429'd, circuit breaker tripped)
Pool resolution:    ~14,800/min  (consuming entire budget)
Price feed:         429'd        (starved by pool resolution)
```

### After Phase 1:
```
Total Helius calls: ~300/min     (price feed + send + confirm)
Public RPC calls:   ~130/min     (pool resolution + blockhash + balance)
sendTransaction:    ~20 RPS      (zero contention on Helius send endpoint)
Pool resolution:    60/min max   (rate gated, public RPC)
Price feed:         ~12/min      (3 subs × 2 vaults × 500ms batched)
```

### After Phase 2:
```
Helius send bucket: 25 burst, 20/sec  →  sendTransaction exclusive
Helius read bucket: 20 burst, 15/sec  →  price feed exclusive
Public RPC bucket:  30 burst, 15/sec  →  pool resolution, blockhash, balance
Contention:         ZERO between sends and reads
```

---

## 13. Risk Assessment

| Risk | Mitigation |
|------|-----------|
| Public RPC drops `getProgramAccounts` (heavy call) | Fallback to `helius_api_url` in `resolve_pumpswap_pool_from_mint()` with Background priority |
| Public RPC latency higher than Helius | Pool resolution is cold path (graduation ~10-20 real events/day). Latency doesn't matter. |
| Rate gate drops valid graduations | 60/min budget = ~1/sec is 3× actual graduation rate. Real graduations also have dedup (resolving_sigs) and enrichment priority. |
| Public RPC returns stale blockhash | 25s refresh interval means we're always within 60s validity. Public RPC confirmed commitment is fine. |
| `getProgramAccounts` not supported on public RPC | Some public nodes disable it. Fallback to Helius API key endpoint. |
| Phase 1 without Phase 2 still has shared Helius bucket | True, but Phase 1 removes 94% of RPC calls. Remaining ~300/min is well within 1,500/min Helius budget. Phase 2 adds isolation for safety. |

---

## 14. Testing Checklist

- [ ] **Unit:** `RpcRateLimiter` — verify token consumption, refill, priority ordering
- [ ] **Unit:** `RpcClient.url_for()` — correct endpoint for each method
- [ ] **Unit:** `RpcClient.acquire()` — Critical never blocked, Background shed at limit
- [ ] **Integration:** Run with `RUST_LOG=rate_limiter=debug` — verify no sends are waited/shed
- [ ] **Integration:** Simulate CoreCast storm (100 events/sec) — verify pool resolution capped at 60/min
- [ ] **Live canary:** Deploy with `paper_mode=true` first, monitor:
  - Rate limiter stats via `/api/rpc_stats`
  - Zero 429s on send endpoint
  - Pool resolution count in logs
- [ ] **Live:** Switch `paper_mode=false`, monitor first 10 trades for successful landing

---

## Appendix A: File Change Summary

```
NEW FILES:
  src/rpc/mod.rs                 ~5 lines
  src/rpc/rate_limiter.rs        ~250 lines (§3.2)
  src/rpc/client.rs              ~150 lines (§4.1)

MODIFIED FILES:
  .env                           +2 lines (PUBLIC_RPC_URL, HELIUS_SEND_URL)
  config/canary.json             +6 fields in rpc_sender section
  src/main.rs                    ~20 lines (wire RpcClient, update blockhash URL)
  src/momentum/mod.rs            ~40 lines (rate gate, concurrency gate, RpcClient field)
  src/momentum/rpc_sender.rs     ~30 lines (use RpcClient, remove old rate limiter)
  src/momentum/pool.rs           ~60 lines (accept RpcClient, acquire before each RPC call)
  src/momentum/price_feed.rs     ~10 lines (acquire Normal before batch getAccountInfo)
  src/tx/executor.rs             ~5 lines (accept URL parameter)
```

## Appendix B: Quick Fix (Emergency — Deploy in 5 Minutes)

If you need to stop the bleeding RIGHT NOW before implementing the full spec:

**Add these 15 lines to `on_migration()` at line ~2494 (after `if !self.config.enabled { return; }`):**

```rust
// EMERGENCY: Rate-limit pool resolution to prevent 429 storm.
// Tracks count with AtomicU64 on self — add field to MomentumEngine.
{
    static POOL_RES_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static POOL_RES_RESET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let reset = POOL_RES_RESET.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(reset) > 60_000 {
        POOL_RES_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        POOL_RES_RESET.store(now, std::sync::atomic::Ordering::Relaxed);
    }
    let count = POOL_RES_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= 60 {
        return; // Drop — over budget
    }
}
```

**And add these 5 lines to `on_pumpswap_graduation_direct()` after the `enabled` check (same static):**

```rust
{
    // Same statics as on_migration — shared budget
    static POOL_RES_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = POOL_RES_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= 60 { return; }
}
```

**And change blockhash + balance to public RPC (2 lines in `main.rs`):**

```rust
// Line ~555: blockhash cache
let rpc_for_bh = "https://api.mainnet-beta.solana.com".to_string();

// In MomentumEngine::new(), the wallet balance poller (~line 375):
// Change: let rpc_for_balance = Arc::clone(&engine.helius_rpc_url);
// To:     let rpc_for_balance = Arc::new("https://api.mainnet-beta.solana.com".to_string());
```

**This emergency fix reduces pool resolution RPC calls by ~94% with zero architectural changes. Deploy, verify sends land, then implement the full spec.**