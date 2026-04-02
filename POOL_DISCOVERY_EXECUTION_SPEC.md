# Pool Discovery & Execution Path — Architecture Spec

**Author:** Quant Architect #2  
**Date:** 2026-04-01  
**Scope:** On-demand pool resolution, caching, and TX execution for momentum-traded PumpSwap tokens

---

## 1. Problem Statement

The bot currently resolves PumpSwap pools **only** during graduation events (`on_migration()` / `on_pumpswap_graduation_direct()`). For momentum trading on **existing tokens** — those that already graduated hours/days ago and are now showing fresh momentum signals — there is no pool resolution path. The `pumpswap_pools` DashMap is empty for these tokens, so the engine cannot build buy/sell TXs.

### What Must Change

1. When a momentum signal fires for a token **not** in `pumpswap_pools`, resolve its pool on-demand
2. Cache resolved pools so subsequent signals/ticks skip RPC entirely
3. Execute buy/sell TXs through the existing `build_pumpswap_buy_tx()` / `build_pumpswap_sell_tx()` path
4. Handle Raydium V4 dead pools gracefully (verdict: abandon, route through Jupiter)

---

## 2. Pool Cache Design

### 2.1 Data Structure

Reuse the existing `pumpswap_pools: DashMap<[u8;32], PumpSwapPoolAccounts>` — it already has the right shape. Add a metadata wrapper for TTL/eviction:

```rust
// In momentum/mod.rs — NEW struct

/// Cached pool entry with resolution metadata for TTL and staleness detection.
#[derive(Debug, Clone)]
pub struct CachedPool {
    /// The resolved pool accounts (ready for TX building).
    pub accounts: crate::tx::pumpswap::PumpSwapPoolAccounts,
    /// Epoch ms when this pool was resolved.
    pub resolved_at_ms: u64,
    /// Epoch ms of last successful trade using this pool (0 if never traded).
    pub last_traded_ms: u64,
    /// Number of times this pool was used for a trade (monotonic counter).
    pub trade_count: u32,
    /// SOL reserves at resolution time (lamports). Used for staleness heuristic.
    pub initial_reserve_sol: u64,
}
```

**Replace** the existing field:
```rust
// OLD
pumpswap_pools: DashMap<[u8; 32], crate::tx::pumpswap::PumpSwapPoolAccounts>,

// NEW
pumpswap_pools: DashMap<[u8; 32], CachedPool>,
```

### 2.2 TTL & Eviction Strategy

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| **Max cache size** | 200 entries | ~200 × 256 bytes ≈ 50KB. Bounded memory. |
| **Soft TTL** | 30 minutes | After 30 min, re-validate reserves before trading (1 cheap RPC call). |
| **Hard TTL** | 4 hours | Evict entirely. Pool layout can't change, but reserves may be drained. |
| **Eviction trigger** | Every 60 seconds in `on_tick()` | Cheap — iterate DashMap, remove entries past hard TTL. |
| **LRU bias** | Evict oldest `resolved_at_ms` first when at capacity | Prefer keeping recently-resolved pools. |

**Eviction function** (added to `drain_scored_tokens()` or its own periodic call):
```rust
/// Evict stale pool cache entries. Called every ~60s from on_tick().
fn evict_stale_pools(&self, now_ms: u64) {
    const HARD_TTL_MS: u64 = 4 * 3600 * 1000; // 4 hours
    const MAX_CACHE_SIZE: usize = 200;

    // Hard TTL eviction
    self.pumpswap_pools.retain(|_mint, cached| {
        now_ms.saturating_sub(cached.resolved_at_ms) < HARD_TTL_MS
    });

    // Capacity eviction: if still over limit, remove oldest
    while self.pumpswap_pools.len() > MAX_CACHE_SIZE {
        let oldest = self.pumpswap_pools.iter()
            .min_by_key(|e| e.value().resolved_at_ms)
            .map(|e| *e.key());
        if let Some(key) = oldest {
            self.pumpswap_pools.remove(&key);
        } else {
            break;
        }
    }
}
```

### 2.3 Reserve Validation (Soft TTL)

When a cached pool is **older than 30 minutes** and we're about to trade it, refresh reserves with one cheap `getMultipleAccounts` call (~50ms):

```rust
/// Validate a cached pool's reserves are still sufficient.
/// Returns updated (reserve_token, reserve_sol) or None if pool is dead/drained.
async fn validate_pool_reserves(
    &self,
    mint: &[u8; 32],
    cached: &CachedPool,
) -> Option<(u64, u64)> {
    let coin_vault_b58 = bs58::encode(&cached.accounts.pool_base_token_account).into_string();
    let pc_vault_b58 = bs58::encode(&cached.accounts.pool_quote_token_account).into_string();

    // For reversed pools (token_is_base=false), base=WSOL, quote=token
    // fetch_vault_reserves expects (coin=token, pc=WSOL) ordering
    let (cv, pv) = if cached.accounts.token_is_base {
        (coin_vault_b58, pc_vault_b58)
    } else {
        (pc_vault_b58, coin_vault_b58)
    };

    let (reserve_token, reserve_sol) = crate::momentum::pool::fetch_vault_reserves(
        &self.http_client, &self.public_rpc_url, &cv, &pv
    ).await?;

    if reserve_sol < crate::momentum::pool::MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
        tracing::warn!(
            mint = %bs58::encode(mint).into_string(),
            reserve_sol,
            "[pool_cache] pool drained below 30 SOL — evicting"
        );
        self.pumpswap_pools.remove(mint);
        return None;
    }

    Some((reserve_token, reserve_sol))
}
```

---

## 3. On-Demand Pool Resolution

### 3.1 Signal Flow

```
ScoredToken arrives via crossbeam channel
        │
        ▼
drain_scored_tokens() inserts into scored_tokens DashMap
        │
        ▼
on_graduation() / process_pending_entries() checks pumpswap_pools
        │
        ├─ HIT (cached) ──► validate if soft TTL expired ──► build TX
        │
        └─ MISS ──► spawn resolve_pool_for_signal() task
                          │
                          ▼
                    resolve_pumpswap_pool_from_mint()
                          │ (getProgramAccounts ~500ms)
                          ▼
                    insert into pumpswap_pools cache
                          │
                          ▼
                    re-evaluate signal (entry or skip)
```

### 3.2 New Entry Point: `resolve_pool_for_momentum()`

This is the **key new function**. Called when a momentum signal fires for a token that isn't in the pool cache.

```rust
// In momentum/mod.rs — NEW function

/// Resolve a PumpSwap pool on-demand for a momentum signal.
/// Returns the CachedPool if resolution succeeds.
///
/// Rate-limited: respects the existing 5-concurrent semaphore.
/// Budget-aware: tracks calls/minute to stay within 60 RPC calls/min.
async fn resolve_pool_for_momentum(
    &self,
    mint: &[u8; 32],
) -> Option<CachedPool> {
    let mint_b58 = bs58::encode(mint).into_string();

    // Fast path: already cached
    if let Some(cached) = self.pumpswap_pools.get(mint) {
        return Some(cached.clone());
    }

    // Rate budget check (see §3.3)
    if !self.pool_resolution_budget.try_consume() {
        tracing::debug!(
            mint = %mint_b58,
            "[momentum] pool resolution rate budget exhausted — skipping"
        );
        return None;
    }

    tracing::info!(
        mint = %mint_b58,
        "[momentum] resolving PumpSwap pool on-demand for momentum signal"
    );

    let resolution = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
        &self.http_client, mint, &self.public_rpc_url, &self.helius_rpc_url
    ).await?;

    // Extract pool accounts
    let ps_pool = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution)?;
    let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let cached = CachedPool {
        accounts: ps_accts,
        resolved_at_ms: now_ms,
        last_traded_ms: 0,
        trade_count: 0,
        initial_reserve_sol: resolution.reserve_sol_lamports,
    };

    self.pumpswap_pools.insert(*mint, cached.clone());

    tracing::info!(
        mint = %mint_b58,
        pool = %bs58::encode(&resolution.pool_address).into_string(),
        reserve_sol = resolution.reserve_sol_lamports,
        "[momentum] pool cached on-demand for momentum trading"
    );

    Some(cached)
}
```

### 3.3 Rate Budget: Token Bucket

The Helius `getProgramAccounts` calls are expensive. We have a 60 calls/min budget. With the existing graduation traffic, budget ~20 calls/min for momentum pool resolution (the other 40 for graduations + other reads).

```rust
// In momentum/mod.rs — NEW struct

/// Simple token-bucket rate limiter for pool resolution RPC calls.
/// Replenishes `rate_per_min` tokens per minute, up to `burst` max.
pub struct ResolutionBudget {
    tokens: AtomicU64,
    last_refill_ms: AtomicU64,
    rate_per_min: u64,
    burst: u64,
}

impl ResolutionBudget {
    pub fn new(rate_per_min: u64, burst: u64) -> Self {
        Self {
            tokens: AtomicU64::new(burst),
            last_refill_ms: AtomicU64::new(0),
            rate_per_min,
            burst,
        }
    }

    /// Try to consume one token. Returns false if bucket is empty.
    pub fn try_consume(&self) -> bool {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Refill: add tokens proportional to elapsed time
        let last = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed_ms = now_ms.saturating_sub(last);
        if elapsed_ms >= 1000 {
            let new_tokens = (elapsed_ms * self.rate_per_min) / 60_000;
            if new_tokens > 0 {
                let current = self.tokens.load(Ordering::Relaxed);
                let refilled = current.saturating_add(new_tokens).min(self.burst);
                self.tokens.store(refilled, Ordering::Relaxed);
                self.last_refill_ms.store(now_ms, Ordering::Relaxed);
            }
        }

        // Try consume
        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current == 0 { return false; }
            if self.tokens.compare_exchange(
                current, current - 1, Ordering::Relaxed, Ordering::Relaxed
            ).is_ok() {
                return true;
            }
        }
    }
}
```

**Initialization** in `MomentumEngine::new()`:
```rust
pool_resolution_budget: ResolutionBudget::new(20, 5),
// 20 resolutions/min sustained, burst of 5
```

### 3.4 Integration with Entry Pipeline

Currently, `process_pending_entries()` at line ~1100 checks `self.pumpswap_pools.get(&entry.mint)`. We need to **add the on-demand resolve** when pool is missing:

```rust
// MODIFIED: process_pending_entries() — around line 1210

} else if let Some(ps_pool) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
    // Existing path: pool already cached from graduation
    // ... existing PumpSwap buy logic ...

} else {
    // NEW: On-demand pool resolution for momentum signals
    // This token came through scored_tokens (not graduation), so no pool is cached.
    let mint = entry.mint;
    let mint_b58 = bs58::encode(&mint).into_string();

    tracing::info!(
        mint = %mint_b58,
        "[momentum] no cached pool — attempting on-demand resolution"
    );

    if let Some(cached) = self.resolve_pool_for_momentum(&mint).await {
        // Pool resolved! Subscribe to price feed and re-queue the entry.
        let pool_info = PoolInfo {
            coin_vault: cached.accounts.pool_base_token_account, // adjusted for ordering
            pc_vault: cached.accounts.pool_quote_token_account,
            reserve_token: 0, // will be fetched by price feed
            reserve_sol: cached.initial_reserve_sol,
            pool_type: PoolType::PumpSwap,
            mint,
        };

        // Subscribe price feed to the vaults
        let (cv_b58, pv_b58) = if cached.accounts.token_is_base {
            (bs58::encode(&cached.accounts.pool_base_token_account).into_string(),
             bs58::encode(&cached.accounts.pool_quote_token_account).into_string())
        } else {
            (bs58::encode(&cached.accounts.pool_quote_token_account).into_string(),
             bs58::encode(&cached.accounts.pool_base_token_account).into_string())
        };

        self.price_feed.subscribe(VaultSubscription {
            mint,
            coin_vault: cv_b58,
            pc_vault: pv_b58,
        });

        // Now proceed with PumpSwap buy (same as existing path)
        // ... build_pumpswap_buy_tx() ...
    } else {
        tracing::warn!(
            mint = %mint_b58,
            "[momentum] on-demand pool resolution FAILED — skipping entry"
        );
    }
}
```

---

## 4. Execution Latency Analysis

### 4.1 Signal → TX Submitted Timeline

| Step | Time (ms) | Notes |
|------|-----------|-------|
| ScoredToken received via crossbeam | ~0 | Lock-free channel |
| Pool cache lookup (DashMap) | ~0.001 | O(1) hash lookup |
| **Cache HIT path:** | | |
| Soft TTL check | ~0 | Compare timestamps |
| Reserve validation (if >30min old) | ~50 | getMultipleAccounts |
| Build TX | ~1 | CPU-only, pre-computed accounts |
| Submit TX (RPC) | ~200–500 | Helius sendTransaction |
| **TOTAL (cache hit, fresh)** | **~201** | |
| **TOTAL (cache hit, stale)** | **~251** | +50ms reserve check |
| **Cache MISS path:** | | |
| getProgramAccounts (offset 43) | ~500 | Helius gPA, may need offset 75 too |
| getMultipleAccounts (vault reserves) | ~50 | Confirm reserves |
| Extract + cache | ~0.1 | CPU-only |
| Build TX | ~1 | |
| Submit TX (RPC) | ~200–500 | |
| **TOTAL (cache miss)** | **~751–1051** | First trade on a token |

### 4.2 Optimization: Pre-resolve on Watchlist

The `ScoredToken` pipeline in `hot_path.rs` has a **watchlist** phase before the token is scored. We can pre-resolve pools when a token enters the watchlist, so by the time the momentum signal fires, the pool is already cached:

```rust
// In engine/hot_path.rs — when promoting to watchlist (around line 475/616)

// NEW: Fire-and-forget pool pre-resolution
if let Some(pool_tx) = &self.pool_pre_resolve_tx {
    let _ = pool_tx.try_send(scored.mint);
}
```

The momentum engine listens on the other end:
```rust
// In MomentumEngine — new background task

async fn pool_pre_resolve_loop(
    rx: mpsc::Receiver<[u8; 32]>,
    http_client: reqwest::Client,
    public_rpc_url: Arc<String>,
    helius_rpc_url: Arc<String>,
    pool_cache: Arc<DashMap<[u8; 32], CachedPool>>,
    budget: Arc<ResolutionBudget>,
) {
    while let Some(mint) = rx.recv().await {
        // Skip if already cached
        if pool_cache.contains_key(&mint) { continue; }
        if !budget.try_consume() { continue; }

        tokio::spawn({
            let client = http_client.clone();
            let pub_url = public_rpc_url.clone();
            let hel_url = helius_rpc_url.clone();
            let cache = pool_cache.clone();
            async move {
                if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                    &client, &mint, &pub_url, &hel_url
                ).await {
                    if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                        let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        cache.insert(mint, CachedPool {
                            accounts: ps_accts,
                            resolved_at_ms: now_ms,
                            last_traded_ms: 0,
                            trade_count: 0,
                            initial_reserve_sol: resolution.reserve_sol_lamports,
                        });
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            "[pre-resolve] pool pre-cached from watchlist promotion"
                        );
                    }
                }
            }
        });
    }
}
```

This brings the effective latency for momentum signals down to the **cache hit** path (~200ms) in most cases.

---

## 5. Raydium V4 Pool Feasibility

### 5.1 Verdict: Dead, Cannot Trade

Old Raydium AMM V4 pools where `getAccountInfo(amm_id)` returns 0 bytes are **truly dead**. Here's why:

1. **The AMM state account is closed.** Raydium V4's `swap` instruction requires reading the AMM state account (`amm_id`) as the first account. If it returns 0 bytes (closed/deallocated), the program will abort with `AccountNotFound` or `InvalidAccountData`.

2. **Vaults still having balances is expected.** SPL token accounts aren't automatically closed when the AMM account is closed. The vault tokens are effectively locked — nobody can execute a swap to drain them because the AMM program can't process the instruction.

3. **No Raydium V3 path.** Raydium V3 (the old CLMM before Raydium V4) is different from the current CLMM (Raydium Concentrated Liquidity). Old V3 pools are similarly dead. The current CLMM pools are separate deployments with different program IDs and pool structures.

### 5.2 Alternative: Jupiter Aggregator

For tokens that **only** have dead Raydium V4 pools and no PumpSwap pool, the options are:

1. **Skip them.** Since April 2026, 100% of pump.fun graduations go to PumpSwap. Tokens with only Raydium pools are pre-April legacy tokens — probably not showing fresh momentum anyway.

2. **Jupiter API as fallback.** Jupiter aggregates across all DEXes. One API call returns a swap instruction:
   ```
   GET https://quote-api.jup.ag/v6/quote?inputMint=So11...112&outputMint={TOKEN}&amount={LAMPORTS}&slippageBps=100
   POST https://quote-api.jup.ag/v6/swap
   ```
   - Adds ~200-500ms latency (API call + Jupiter route optimization)
   - Requires trusting Jupiter's TX construction (we can't inspect every instruction)
   - Not worth building for 1.49 SOL bankroll — complexity vs. reward ratio is wrong

### 5.3 Recommendation

**Kill the `raydium_pools` DashMap entirely.** Replace with a comment:
```rust
// Raydium V4 pools are dead since April 2026 — all pump.fun graduations go to PumpSwap.
// Jupiter aggregator fallback not implemented (complexity vs. 1.49 SOL bankroll).
// If a token only has a Raydium pool, skip it.
```

This simplifies the codebase (remove ~200 lines of Raydium pool storage/lookup), reduces DashMap memory, and eliminates the dead code path that will never execute.

---

## 6. Specific Rust Changes

### 6.1 File: `momentum/mod.rs`

**New fields on `MomentumEngine`:**
```rust
/// Rate limiter for on-demand pool resolution RPC calls.
pool_resolution_budget: ResolutionBudget,

/// Receiver for pool pre-resolution requests from hot_path watchlist.
pool_pre_resolve_rx: Option<mpsc::Receiver<[u8; 32]>>,

/// Timestamp of last pool cache eviction run.
last_pool_evict_ms: AtomicU64,
```

**New struct `CachedPool`** (see §2.1)

**New struct `ResolutionBudget`** (see §3.3)

**New methods on `MomentumEngine`:**
```rust
async fn resolve_pool_for_momentum(&self, mint: &[u8; 32]) -> Option<CachedPool>;
fn evict_stale_pools(&self, now_ms: u64);
async fn validate_pool_reserves(&self, mint: &[u8; 32], cached: &CachedPool) -> Option<(u64, u64)>;
```

**Modified methods:**
- `on_tick()` — add `evict_stale_pools()` call every 60s
- `process_pending_entries()` — add on-demand resolve fallback for missing pools
- All existing `self.pumpswap_pools.insert(mint, ps_accts)` calls → wrap in `CachedPool`
- All existing `self.pumpswap_pools.get(&mint)` calls → unwrap `.accounts` from `CachedPool`
- `new()` — initialize `pool_resolution_budget`, `last_pool_evict_ms`

**Deletions:**
- Remove `raydium_pools: DashMap<[u8; 32], RaydiumPoolAccounts>` and all Raydium pool insert/lookup code
- Remove Raydium buy/sell TX building from `process_pending_entries()` and exit handlers

### 6.2 File: `momentum/pool.rs`

No changes needed. The existing `resolve_pumpswap_pool_from_mint()` and `extract_pumpswap_pool_accounts()` are already perfectly suited for on-demand resolution. The 5-concurrent semaphore provides the right backpressure.

### 6.3 File: `tx/pumpswap.rs`

No changes needed. `build_pumpswap_buy_tx()` and `build_pumpswap_sell_tx()` already accept `PumpSwapPoolAccounts` which the `CachedPool` wraps.

### 6.4 File: `engine/hot_path.rs`

**New field:**
```rust
/// Channel to request pool pre-resolution for watchlist-promoted tokens.
pool_pre_resolve_tx: Option<mpsc::Sender<[u8; 32]>>,
```

**New method:**
```rust
pub fn set_pool_pre_resolve_tx(&mut self, tx: mpsc::Sender<[u8; 32]>);
```

**Modified:** Add `pool_pre_resolve_tx.try_send(mint)` where `ScoredToken` is published (~lines 475, 616).

### 6.5 File: `tx/raydium.rs`

**Candidate for deletion** (or at minimum, stop importing/using). Keep the file if needed for historical reference, but remove all active code paths that reference it from `momentum/mod.rs`.

### 6.6 Migration Path (All Existing Callsites)

Every place that currently does:
```rust
self.pumpswap_pools.insert(resolution.mint, ps_accts);
```

Must become:
```rust
let now_ms = /* current epoch ms */;
self.pumpswap_pools.insert(resolution.mint, CachedPool {
    accounts: ps_accts,
    resolved_at_ms: now_ms,
    last_traded_ms: 0,
    trade_count: 0,
    initial_reserve_sol: resolution.reserve_sol_lamports,
});
```

Every place that currently does:
```rust
if let Some(ps_pool) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
```

Must become:
```rust
if let Some(cached) = self.pumpswap_pools.get(&entry.mint).map(|r| r.clone()) {
    let ps_pool = cached.accounts;
```

There are **12 insert sites** and **~6 get sites** based on the grep (lines 2629, 2817, 2828, 2979, 3185, 3206, 3272 for inserts; lines 1133, 1210, 2301, 2377, 2463 for gets).

---

## 7. Complete Execution Flow (End-to-End)

### 7.1 Momentum Signal → Buy TX

```
1. hot_path scores token → ScoredToken published
2. hot_path also fires pool_pre_resolve_tx.try_send(mint)
3. [Background] resolve_pumpswap_pool_from_mint() → CachedPool stored
4. [150ms tick] drain_scored_tokens() ingests ScoredToken
5. on_graduation() evaluates ScoredToken as entry candidate
6. process_pending_entries() dequeues after entry_delay_ms
7. pumpswap_pools.get(mint) → HIT (from step 3)
   - If soft TTL expired: validate_pool_reserves() → 50ms
   - If cache miss (pre-resolve failed): resolve_pool_for_momentum() → 500ms
8. build_pumpswap_buy_tx() → signed TX bytes
9. rpc_sender.submit_tx() → Helius sendTransaction → poll confirmation
```

### 7.2 Position Exit → Sell TX

No change from existing flow. The pool is already cached from the buy path. `process_active_positions()` already handles PumpSwap sell via `self.pumpswap_pools.get(&mint)`.

### 7.3 Error Recovery

| Failure | Recovery |
|---------|----------|
| getProgramAccounts timeout | `resolve_pumpswap_pool_from_mint()` returns None → entry skipped |
| Pool has <30 SOL reserves | Resolution succeeds but pool rejected by MIN_PUMPSWAP_SOL_RESERVES check |
| Pool drained between cache and trade | TX fails on-chain (insufficient funds) → circuit breaker absorbs; validate_pool_reserves on next attempt catches it |
| Semaphore full (5 concurrent) | Resolution dropped → entry retried on next tick if signal persists |
| Rate budget exhausted (20/min) | Resolution deferred → signal may expire from scored_tokens TTL (10 min) |
| getProgramAccounts returns multiple pools | Existing code takes `accounts[0]` → always the first match. This is correct: PumpSwap creates exactly one pool per token. |

---

## 8. Configuration Additions

```rust
// In momentum/config.rs — new fields on MomentumConfig

/// Maximum pool resolution calls per minute for momentum signals (default: 20).
/// Separate from graduation resolution budget. 0 = disabled.
#[serde(default = "default_momentum_pool_budget")]
pub momentum_pool_budget_per_min: u64,

/// Pool cache soft TTL in ms. Cached pools older than this trigger
/// a reserve validation before trading (default: 1_800_000 = 30 min).
#[serde(default = "default_pool_soft_ttl_ms")]
pub pool_soft_ttl_ms: u64,

/// Pool cache hard TTL in ms. Pools older than this are evicted entirely
/// (default: 14_400_000 = 4 hours).
#[serde(default = "default_pool_hard_ttl_ms")]
pub pool_hard_ttl_ms: u64,

/// Enable pre-resolution of pools when tokens enter watchlist (default: true).
#[serde(default = "default_pre_resolve_enabled")]
pub pool_pre_resolve_enabled: bool,

fn default_momentum_pool_budget() -> u64 { 20 }
fn default_pool_soft_ttl_ms() -> u64 { 1_800_000 }
fn default_pool_hard_ttl_ms() -> u64 { 14_400_000 }
fn default_pre_resolve_enabled() -> bool { true }
```

---

## 9. Testing Strategy

### 9.1 Unit Tests (in `momentum/pool.rs`)

Already extensive (1876 lines includes 636+ lines of tests). No new tests needed for pool resolution itself.

### 9.2 Integration Tests (new `momentum/pool_cache_test.rs` or in mod.rs `#[cfg(test)]`)

1. **Cache insert/get round-trip** — resolve pool, cache it, retrieve it, verify accounts match
2. **Soft TTL triggers validation** — mock a 31-minute-old entry, verify `validate_pool_reserves` is called
3. **Hard TTL eviction** — insert entry, advance clock 4.1 hours, call `evict_stale_pools()`, verify removed
4. **Rate budget exhaustion** — consume all 5 burst tokens, verify 6th returns false
5. **CachedPool wrapping** — verify all 12 insert sites produce valid CachedPool structs

### 9.3 Paper Mode Validation

Run in paper mode with momentum signals enabled. Verify logs show:
- `[momentum] resolving PumpSwap pool on-demand for momentum signal`
- `[momentum] pool cached on-demand for momentum trading`
- `[pre-resolve] pool pre-cached from watchlist promotion`
- `[pool_cache] pool drained below 30 SOL — evicting`

---

## 10. Risk Assessment

### 10.1 RPC Cost

| Operation | Cost/call | Calls/min (budget) | Monthly cost (est.) |
|-----------|-----------|-------------------|---------------------|
| getProgramAccounts | 100 credits | 20/min | ~864K credits/day |
| getMultipleAccounts | 1 credit | 60/min (validation) | ~86K credits/day |

Helius free tier: 100K credits/day. **This exceeds the free tier.**
Helius Developer tier ($50/mo): 50M credits/day. Comfortable.

**Mitigation for free tier:** Reduce `momentum_pool_budget_per_min` to 5. This gives 7,200 gPA calls/day = 720K credits/day for gPA. Combined with other traffic, still tight. Consider caching aggressively (longer TTLs) and only resolving for high-conviction signals (score > 70).

### 10.2 Bankroll Risk

With 1.49 SOL, failed TXs cost only the TX fee (~0.0001 SOL priority + 0.000005 SOL base). 
Position sizes of 0.05–0.30 SOL mean 5–30 entries before bankroll is depleted.
The CachedPool reserve validation prevents trading into drained pools (the main failure mode that would waste a position-sized amount on a failed swap).

### 10.3 Concurrency Safety

- `DashMap` provides lock-free concurrent reads and sharded writes — safe for multi-task access.
- `ResolutionBudget` uses `AtomicU64` with CAS loop — wait-free for single-consumer (which this is).
- The existing `POOL_RESOLUTION_SEMAPHORE` (5 permits) prevents RPC connection exhaustion.
- Pre-resolve tasks are `tokio::spawn`'d — don't block the tick loop.

---

## 11. Summary of Deliverables

| # | Deliverable | Status |
|---|-------------|--------|
| 1 | Pool cache design | `CachedPool` wrapping `PumpSwapPoolAccounts` with TTL metadata. 30-min soft TTL (re-validate reserves), 4-hour hard TTL (evict). DashMap, 200 entry cap. |
| 2 | On-demand resolution | `resolve_pool_for_momentum()` + `pool_pre_resolve_loop()` background task. Token-bucket rate limiter at 20 calls/min. Pre-resolves from watchlist promotions. |
| 3 | Raydium feasibility | **Dead.** AMM accounts are closed, vaults are orphaned. Skip tokens with only Raydium pools. Remove `raydium_pools` DashMap. Jupiter not worth the complexity at current bankroll. |
| 4 | Execution latency | **Cache hit:** ~200ms (fresh) / ~250ms (stale + validation). **Cache miss:** ~750–1050ms. **With pre-resolve:** almost always cache hit. |
| 5 | Rust changes | `momentum/mod.rs`: +CachedPool, +ResolutionBudget, +3 methods, modify 18 callsites. `engine/hot_path.rs`: +pre-resolve channel. `momentum/config.rs`: +4 config fields. Delete Raydium paths. |