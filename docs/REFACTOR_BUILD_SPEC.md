# Engine Abstraction Refactor — Build Spec

**Authors:** 4x Opus 4.6 Architects (A, B, C, D)  
**Date:** 2026-04-03  
**Status:** Ready for implementation by 5 parallel Rust engineers  
**Goal:** Refactor `MomentumEngine` from load-bearing monolith into one of N pluggable `TradingEngine` implementations, feature-flagged via config.

---

## Overview

### What we're building

```
Before:
  main.rs (481 lines) → MomentumEngine (6828 lines, owns all infra)

After:
  main.rs (~150 lines, pure orchestration)
    → ExecutionContext (shared Jito/Nozomi/blockhash/wallet/RPC)
    → FeedRouter (event fan-out to N engines + health monitor)
       → MomentumEngine (implements TradingEngine trait)
       → SniperEngine   (implements TradingEngine trait, disabled stub)
```

### New files
| File | Purpose |
|------|---------|
| `src/engine/trading_engine.rs` | `TradingEngine` trait + `GraduationEvent` type |
| `src/engine/registry.rs` | `EngineRegistry` — holds all engines, fan-out dispatch |
| `src/engine/feed_router.rs` | `FeedRouter` — single dispatch authority, owns health monitor |
| `src/tx/execution_context.rs` | `ExecutionContext` — shared infra (Jito, Nozomi, wallet, etc.) |
| `src/sniper/mod.rs` | `SniperEngine` stub |
| `src/sniper/config.rs` | `SniperConfig` |
| `src/test_utils.rs` | `MockEngine` for tests |

### Modified files
| File | Change |
|------|--------|
| `src/main.rs` | 481 → ~150 lines. Pure orchestration only. |
| `src/momentum/mod.rs` | Remove 9 infra fields, implement `TradingEngine` trait |
| `src/momentum/config.rs` | No change |
| `config/canary.json` | Add `sniper` section |
| `src/lib.rs` | Add new module declarations |

---

## ARCHITECT A — TradingEngine Trait & main.rs

### 1. The `TradingEngine` Trait

**File:** `rust/pump-quant-core/src/engine/trading_engine.rs`

```rust
//! Core trait for all trading engines.
//!
//! Object-safe via `async_trait`. Each engine is wrapped in `Arc` and
//! dispatched to by FeedRouter. Engines receive events in parallel (fan-out).

use async_trait::async_trait;
use crate::feeds::MigrationSource;
use crate::momentum::types::GradEnrichment;

/// Snapshot of engine health/stats for the API layer.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "engine")]
pub enum EngineHealthSnapshot {
    Momentum(crate::momentum::MomentumStats),
    Sniper(serde_json::Value),
    Unknown(serde_json::Value),
}

/// Unified graduation event — wraps both `Migration` and `PumpSwapGraduationDirect`.
/// Engines get one method (`on_graduation`) instead of two.
#[derive(Debug, Clone)]
pub struct GraduationEvent {
    pub mint: [u8; 32],
    pub sig: [u8; 64],
    pub ts_ms: u64,
    pub source: MigrationSource,
    pub enrichment: GradEnrichment,
    /// Pre-extracted vault accounts from Helius Enhanced WS.
    /// `None` for legacy `Migration` events (engine resolves via RPC).
    pub pumpswap_vaults: Option<PumpSwapVaults>,
}

#[derive(Debug, Clone, Copy)]
pub struct PumpSwapVaults {
    pub coin_vault: [u8; 32],
    pub pc_vault: [u8; 32],
}

#[async_trait]
pub trait TradingEngine: Send + Sync + 'static {
    /// Human-readable name, unique across all registered engines.
    fn name(&self) -> &'static str;

    /// Whether this engine is in paper mode (log trades, no real TXs).
    fn paper_mode(&self) -> bool;

    /// Whether this engine is enabled. Checked by registry before dispatch.
    fn enabled(&self) -> bool;

    /// Called on every `FeedEvent::TokenCreated`. Non-async, must be fast.
    fn on_token_created(&self, mint: [u8; 32], ts_ms: u64);

    /// Called on every graduation event. Cold path (~10-50/day for momentum).
    /// Spawned in tokio::spawn by registry — may do RPC calls.
    async fn on_graduation(&self, event: GraduationEvent);

    /// Called every tick (~50ms). Hot path — must return quickly.
    async fn on_tick(&self, ts_ms: u64);

    /// Engine health/stats snapshot for API.
    fn health(&self) -> EngineHealthSnapshot;

    /// Post-startup recovery hook. Called once, 5s after init. Default: no-op.
    async fn on_startup_recovery(&self) {}

    /// Graceful shutdown. Default: no-op.
    async fn shutdown(&self) {}
}
```

**Object safety:** `async_trait` desugars to `-> Pin<Box<dyn Future>>` — fully object-safe. `Arc<dyn TradingEngine>` compiles cleanly. Add to `Cargo.toml` if not present: `async-trait = "0.1"`.

**Why unified `GraduationEvent`:** `Migration` and `PumpSwapGraduationDirect` differ only in whether vault accounts are pre-extracted. Single trait method, engine branches internally on `pumpswap_vaults.is_some()`.

---

### 2. The `EngineRegistry`

**File:** `rust/pump-quant-core/src/engine/registry.rs`

```rust
use std::sync::Arc;
use super::trading_engine::{TradingEngine, GraduationEvent};

pub struct EngineRegistry {
    engines: Vec<Arc<dyn TradingEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self { Self { engines: Vec::new() } }

    pub fn register(&mut self, engine: Arc<dyn TradingEngine>) {
        tracing::info!(
            engine = engine.name(),
            enabled = engine.enabled(),
            paper_mode = engine.paper_mode(),
            "Registered trading engine"
        );
        self.engines.push(engine);
    }

    pub fn engines(&self) -> &[Arc<dyn TradingEngine>] { &self.engines }

    fn enabled(&self) -> impl Iterator<Item = &Arc<dyn TradingEngine>> {
        self.engines.iter().filter(|e| e.enabled())
    }

    /// Synchronous fan-out — non-async, must be fast.
    pub fn dispatch_token_created(&self, mint: [u8; 32], ts_ms: u64) {
        for engine in self.enabled() {
            engine.on_token_created(mint, ts_ms);
        }
    }

    /// Each engine gets its own tokio::spawn — one slow engine doesn't block another.
    pub fn dispatch_graduation(&self, event: GraduationEvent) {
        for engine in self.enabled() {
            let engine = Arc::clone(engine);
            let event = event.clone();
            tokio::spawn(async move {
                engine.on_graduation(event).await;
            });
        }
    }

    /// Sequential tick dispatch. Each engine internally throttles (returns in <1ms when idle).
    /// No tokio::spawn overhead — 50ms tick, N engines all return immediately when idle.
    pub async fn dispatch_tick(&self, ts_ms: u64) {
        for engine in self.enabled() {
            engine.on_tick(ts_ms).await;
        }
    }

    /// Trigger post-startup recovery for all enabled engines.
    pub async fn trigger_startup_recovery(&self) {
        for engine in &self.engines {
            if engine.enabled() {
                let engine = Arc::clone(engine);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    engine.on_startup_recovery().await;
                });
            }
        }
    }

    pub async fn shutdown_all(&self) {
        for engine in &self.engines {
            engine.shutdown().await;
        }
    }
}
```

**Design:** Registry is NOT a trait — one concrete struct, no over-abstraction. `dispatch_graduation` clones `GraduationEvent` per engine (~170 bytes, cheap). `dispatch_tick` is sequential, not spawned (engines self-throttle).

---

### 3. Refactored `main.rs` Skeleton

```rust
//! pump-quant-core — Multi-engine trading daemon.
//! Engines: momentum (post-graduation), sniper (bonding curve, future).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Init: env, TLS, tracing ──────────────────────────────────── (unchanged)

    // ── Load config ──────────────────────────────────────────────── (unchanged)
    let engine_config = load_config(&config_path)?;

    // ── Engine state persistence ─────────────────────────────────── (unchanged)

    // ── Health monitor ───────────────────────────────────────────── (unchanged)
    let health_monitor = HealthMonitor::new(&engine_config.health);
    let telegram_alerter = TelegramAlerter::new();

    // ── Feed channels + spawning ─────────────────────────────────── (unchanged — feeds are not engine-specific)
    // PumpPortal, Helius, ShredStream, CoreCast spawned here

    // ── ExecutionContext ─────────────────────────────────────────── (NEW — see Arch B)
    let exec_ctx = Arc::new(build_execution_context(&engine_config).await);

    // ── Build + register engines ─────────────────────────────────── (NEW)
    let mut registry = EngineRegistry::new();

    if engine_config.momentum.enabled || true { // always build, enabled() gates dispatch
        let (momentum, ..) = MomentumEngine::new(
            Arc::new(engine_config.momentum.clone()),
            momentum_wss_url,
            &momentum_log_path,
            Arc::clone(&exec_ctx),
        );
        registry.register(Arc::new(momentum));
    }

    // Future: sniper
    // if engine_config.sniper.enabled || true {
    //     registry.register(Arc::new(SniperEngine::new(sniper_config, Arc::clone(&exec_ctx))));
    // }

    registry.trigger_startup_recovery().await;

    // ── Build FeedRouter ─────────────────────────────────────────── (NEW — see Arch C)
    let mut router = FeedRouter::new(health_monitor, api_state.stats.clone(), telegram_alerter);
    // Router holds Arc<EngineRegistry> for dispatch

    // ── Main event loop ───────────────────────────────────────────── (NEW — 5 lines)
    loop {
        match engine_rx.recv() {
            Ok(event) => {
                if !router.dispatch(event, &registry).await {
                    break; // Shutdown
                }
            }
            Err(_) => break,
        }
    }

    registry.shutdown_all().await;
    Ok(())
}
```

**MomentumEngine trait implementation** (in `momentum/mod.rs`):
```rust
#[async_trait]
impl TradingEngine for MomentumEngine {
    fn name(&self) -> &'static str { "momentum" }
    fn paper_mode(&self) -> bool { self.config.paper_mode }
    fn enabled(&self) -> bool { self.config.enabled }

    fn on_token_created(&self, mint: [u8; 32], ts_ms: u64) {
        self.record_token_created(mint, ts_ms); // existing method, unchanged
    }

    async fn on_graduation(&self, event: GraduationEvent) {
        let enrichment = event.enrichment;
        if let Some(vaults) = event.pumpswap_vaults {
            self.on_pumpswap_graduation_direct(
                event.mint, event.sig, event.ts_ms,
                vaults.coin_vault, vaults.pc_vault,
                event.source, enrichment,
            ).await;
        } else {
            self.on_migration(event.mint, event.ts_ms, event.sig, enrichment).await;
        }
    }

    async fn on_tick(&self, ts_ms: u64) {
        self.on_tick(ts_ms).await; // existing method, rename to on_tick_internal if collision
    }

    fn health(&self) -> EngineHealthSnapshot {
        EngineHealthSnapshot::Momentum(self.stats_snapshot())
    }

    async fn on_startup_recovery(&self) {
        self.recover_orphan_positions().await;
    }
}
```

**Feature flag config:**
```json
{
  "momentum": {
    "enabled": true,
    "paper_mode": true,
    "...": "all existing momentum fields"
  },
  "sniper": {
    "enabled": false,
    "paper_mode": true,
    "max_position_sol": 0.05,
    "max_grad_age_s": 60,
    "min_social_score": 2
  }
}
```

---

## ARCHITECT B — ExecutionContext & Shared Infrastructure

### 1. `ExecutionContext` Struct

**File:** `rust/pump-quant-core/src/tx/execution_context.rs`

```rust
//! Shared transaction infrastructure for all trading engines.
//! Created once in main.rs, Arc'd to every engine.

use std::sync::Arc;
use parking_lot::Mutex;

pub struct WalletKeys {
    keypair_bytes: [u8; 64],
    pubkey: [u8; 32],
}

impl WalletKeys {
    pub fn load_from_path(path: &str) -> Option<Self> {
        if path.is_empty() { return None; }
        let bytes = std::fs::read(path).ok()?;
        let arr: Vec<u8> = serde_json::from_slice(&bytes).ok()?;
        if arr.len() != 64 {
            tracing::error!(len = arr.len(), "[WalletKeys] invalid keypair length");
            return None;
        }
        let mut keypair_bytes = [0u8; 64];
        keypair_bytes.copy_from_slice(&arr);
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&arr[32..64]);
        Some(Self { keypair_bytes, pubkey })
    }

    pub fn pubkey(&self) -> [u8; 32] { self.pubkey }
    pub fn keypair_bytes(&self) -> [u8; 64] { self.keypair_bytes }

    pub fn to_keypair(&self) -> solana_sdk::signature::Keypair {
        solana_sdk::signature::Keypair::from_bytes(&self.keypair_bytes)
            .expect("WalletKeys: validated 64-byte keypair")
    }
}

impl Clone for WalletKeys {
    fn clone(&self) -> Self {
        Self { keypair_bytes: self.keypair_bytes, pubkey: self.pubkey }
    }
}

pub struct ExecutionContext {
    // TX submission
    pub jito_grpc: Option<Arc<JitoGrpcClient>>,
    pub nozomi_client: Option<Arc<NozomiClient>>,

    // Blockhash
    pub blockhash_cache: Arc<BlockhashCache>,

    // Wallet (pre-loaded once — replaces per-trade fs::read)
    pub wallet: Option<WalletKeys>,

    // Tip engine
    pub tip_engine: Arc<Mutex<TipEngine>>,

    // RPC
    pub rpc_sender: Arc<RpcSender>,
    pub rpc_fallback_client: reqwest::Client,
    pub rpc_fallback_url: Arc<String>,

    // URLs
    pub helius_rpc_url: Arc<String>,
    pub public_rpc_url: Arc<String>,
}

impl ExecutionContext {
    pub fn blockhash_sync(&self) -> Option<[u8; 32]> {
        self.blockhash_cache.get_sync()
    }

    pub fn compute_tip(&self, req: &TipRequest) -> u64 {
        self.tip_engine.lock().compute_tip(req)
    }

    pub fn wallet_pubkey(&self) -> Option<[u8; 32]> {
        self.wallet.as_ref().map(|w| w.pubkey())
    }

    /// Panics if called in paper mode. Always gate on !paper_mode.
    pub fn keypair(&self) -> solana_sdk::signature::Keypair {
        self.wallet.as_ref()
            .expect("keypair() called in paper mode")
            .to_keypair()
    }
}
```

### 2. Construction in `main.rs`

```rust
// Build once, Arc to all engines
let exec_ctx = Arc::new(ExecutionContext {
    jito_grpc,           // Option<Arc<JitoGrpcClient>> — None if paper_mode
    nozomi_client,       // Option<Arc<NozomiClient>> — None if paper_mode or no key
    blockhash_cache,     // Arc<BlockhashCache> — always present
    wallet,              // Option<WalletKeys> — None if paper_mode
    tip_engine,          // Arc<Mutex<TipEngine>>
    rpc_sender,          // Arc<RpcSender>
    rpc_fallback_client, // reqwest::Client
    rpc_fallback_url,
    helius_rpc_url,
    public_rpc_url,
});
```

### 3. MomentumEngine: Fields Removed

These 9 fields are **removed** from `MomentumEngine` and accessed via `self.exec_ctx.*`:

| Removed field | Access via |
|---|---|
| `jito_grpc: Option<Arc<JitoGrpcClient>>` | `self.exec_ctx.jito_grpc` |
| `nozomi_client: Option<Arc<NozomiClient>>` | `self.exec_ctx.nozomi_client` |
| `wallet_pubkey: Option<[u8; 32]>` | `self.exec_ctx.wallet_pubkey()` |
| `blockhash_cache: Arc<BlockhashCache>` | `self.exec_ctx.blockhash_cache` |
| `tip_engine: Arc<Mutex<TipEngine>>` | `self.exec_ctx.tip_engine` |
| `rpc_sender: Arc<RpcSender>` | `self.exec_ctx.rpc_sender` |
| `rpc_fallback_client: reqwest::Client` | `self.exec_ctx.rpc_fallback_client` |
| `rpc_fallback_url: Arc<String>` | `self.exec_ctx.rpc_fallback_url` |
| `helius_rpc_url: Arc<String>` | `self.exec_ctx.helius_rpc_url` |

**New field added:**
```rust
pub struct MomentumEngine {
    exec_ctx: Arc<ExecutionContext>,  // ← NEW
    // ... all momentum-specific fields remain unchanged
}
```

**New `new()` signature:**
```rust
pub fn new(
    config: Arc<MomentumConfig>,
    helius_wss_url: String,     // WSS URL for price feed (momentum-specific)
    log_path: &str,
    exec_ctx: Arc<ExecutionContext>,
) -> (Self, ...)
```

### 4. Access Pattern Changes (Before/After)

**Before:**
```rust
let jg = match self.jito_grpc.clone() {
    Some(j) => j,
    None => { tracing::warn!(...); return; }
};
let bh = self.blockhash_cache_sync().unwrap_or([0u8; 32]);
let tip = self.tip_engine.lock().compute_tip(&tip_req);
```

**After:**
```rust
let jg = match self.exec_ctx.jito_grpc.clone() {
    Some(j) => j,
    None => { tracing::warn!(...); return; }
};
let bh = self.exec_ctx.blockhash_sync().unwrap_or([0u8; 32]);
let tip = self.exec_ctx.compute_tip(&tip_req);
```

### 5. Wallet Keypair: Per-trade `fs::read` Eliminated

**Before** (in every buy/sell async task):
```rust
let kp_bytes = match std::fs::read(&kp_path) {
    Ok(b) => b,
    Err(e) => { tracing::error!(err=?e, "keypair load failed"); return; }
};
let kp_arr: Vec<u8> = serde_json::from_slice(&kp_bytes).unwrap();
// ... 8 more lines of error handling
let keypair = solana_sdk::signature::Keypair::from_bytes(&kb).unwrap();
```

**After** (via `ExecutionContext`):
```rust
// In the async task, clone the pre-loaded WalletKeys (64-byte copy, no I/O)
let wallet = self.exec_ctx.wallet.clone()
    .expect("live mode requires wallet");
let keypair = wallet.to_keypair();
```

Eliminates ~15 lines of repeated disk I/O + error handling from every buy and sell task.

---

## ARCHITECT C — FeedRouter & Health Monitoring

### 1. `FeedRouter` Struct

**File:** `rust/pump-quant-core/src/engine/feed_router.rs`

```rust
use std::sync::Arc;
use crate::alerts::telegram::{self, TelegramAlerter};
use crate::api::EngineStats;
use crate::engine::health::{HealthMonitor, HealthStatus};
use crate::engine::registry::EngineRegistry;
use crate::feeds::{FeedEvent, FeedSource};
use crate::momentum::types::GradEnrichment;
use crate::engine::trading_engine::{GraduationEvent, PumpSwapVaults};

pub struct FeedRouter {
    health_monitor: Arc<HealthMonitor>,
    shared_stats: Arc<std::sync::Mutex<EngineStats>>,
    telegram_alerter: Option<TelegramAlerter>,
    counters: EventCounters,
    health_check_interval: u64,  // ticks, default 100
    stats_sync_interval: u64,    // ticks, default 200
    stats_log_interval: u64,     // trades, default 1000
}

struct EventCounters {
    trades_seen: u64,
    ticks: u64,
    migrations: u64,
    creator_sells: u64,
}

impl FeedRouter {
    /// Returns false on Shutdown — caller should break event loop.
    pub async fn dispatch(&mut self, event: FeedEvent, registry: &EngineRegistry) -> bool {
        match event {
            FeedEvent::Trade(trade) => {
                self.health_monitor.record_event(trade.source, trade.timestamp_ms);
                self.counters.trades_seen += 1;
                if self.counters.trades_seen % self.stats_log_interval == 0 {
                    tracing::info!(
                        trades = self.counters.trades_seen,
                        ticks = self.counters.ticks,
                        migrations = self.counters.migrations,
                        "engine stats"
                    );
                }
            }
            FeedEvent::PreWarm(prewarm) => {
                self.health_monitor.record_event(prewarm.source, prewarm.timestamp_ms);
            }
            FeedEvent::Tick { ts_ms } => {
                self.counters.ticks += 1;

                // Engine tick dispatch
                registry.dispatch_tick(ts_ms).await;

                // Health check every 100 ticks (~5s)
                if self.counters.ticks % self.health_check_interval == 0 {
                    let (status, recovered) = self.health_monitor.check(ts_ms);
                    self.handle_health_status(status, recovered, ts_ms);
                }

                // Stats sync every 200 ticks (~10s)
                if self.counters.ticks % self.stats_sync_interval == 0 {
                    if let Ok(mut stats) = self.shared_stats.lock() {
                        stats.trades_seen = self.counters.trades_seen;
                        stats.migrations_seen = self.counters.migrations;
                        stats.creator_sells_seen = self.counters.creator_sells;
                    }
                }
            }
            FeedEvent::TokenCreated(tc) => {
                registry.dispatch_token_created(tc.mint, tc.ts_ms);
            }
            FeedEvent::CreatorSell { ts_ms, .. } => {
                self.health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                self.counters.creator_sells += 1;
            }
            FeedEvent::Migration { mint, ts_ms, source, sig } => {
                if matches!(source, crate::feeds::MigrationSource::CoreCastStream2) {
                    self.health_monitor.record_event(FeedSource::CoreCast, ts_ms);
                }
                self.counters.migrations += 1;
                let sig_bytes = Self::sig_to_bytes(sig);
                registry.dispatch_graduation(GraduationEvent {
                    mint,
                    sig: sig_bytes,
                    ts_ms,
                    source,
                    enrichment: GradEnrichment::UNKNOWN,
                    pumpswap_vaults: None,
                });
            }
            FeedEvent::PumpSwapGraduationDirect { mint, sig, ts_ms, coin_vault, pc_vault, source } => {
                self.counters.migrations += 1;
                registry.dispatch_graduation(GraduationEvent {
                    mint,
                    sig,
                    ts_ms,
                    source,
                    enrichment: GradEnrichment::UNKNOWN,
                    pumpswap_vaults: Some(PumpSwapVaults { coin_vault, pc_vault }),
                });
            }
            FeedEvent::LpRemoval { .. } => {} // engines handle exits internally
            FeedEvent::Shutdown => return false,
        }
        true
    }

    fn handle_health_status(
        &self,
        status: crate::engine::health::HealthStatus,
        recovered: Vec<String>,
        ts_ms: u64,
    ) {
        if let HealthStatus::Degraded { ref stale_feeds } = status {
            for feed in stale_feeds {
                let source = match feed.as_str() {
                    "Helius" => FeedSource::Helius,
                    _ => FeedSource::PumpPortal,
                };
                let last_ms = self.health_monitor.last_event_ms(source);
                let stale_s = if last_ms > 0 { ts_ms.saturating_sub(last_ms) / 1000 } else { 0 };
                tracing::warn!(feed = %feed, stale_s, "Feed stale — trading paused");
                if let Some(ref tg) = self.telegram_alerter {
                    tg.try_send_blocking(&telegram::format_feed_stale_alert(feed, stale_s));
                }
            }
        }
        for feed in &recovered {
            tracing::info!(feed = %feed, "Feed recovered — trading resumed");
            if let Some(ref tg) = self.telegram_alerter {
                tg.try_send_blocking(&telegram::format_feed_recovered_alert(feed));
            }
        }
    }
}
```

### 2. Event Routing Table

| FeedEvent | HealthMonitor | Counter | Engine method | All engines? |
|---|---|---|---|---|
| `Trade` | `record_event` | `trades_seen++` | — | N/A |
| `PreWarm` | `record_event` | — | — | N/A |
| `Tick` | `check()` every 100 ticks | `ticks++` | `on_tick()` | ✅ ALL |
| `TokenCreated` | — | — | `on_token_created()` | ✅ ALL |
| `CreatorSell` | `record_event(CoreCast)` | `creator_sells++` | — | N/A |
| `Migration` | `record_event` if CoreCast | `migrations++` | `on_graduation()` | ✅ ALL |
| `PumpSwapGraduationDirect` | — | `migrations++` | `on_graduation()` | ✅ ALL |
| `LpRemoval` | — | — | — | N/A |
| `Shutdown` | — | — | — (returns false) | N/A |

### 3. New Main Loop (5 lines)

```rust
loop {
    match engine_rx.recv() {
        Ok(event) => {
            if !router.dispatch(event, &registry).await {
                break;
            }
        }
        Err(_) => break,
    }
}
```

---

## ARCHITECT D — Migration Strategy, Tests & SniperEngine Stub

### 1. Phased Migration Plan

Each phase = one PR. Each leaves engine deployable on VPS. Zero behavior change until Phase 5.

---

#### Phase 1 — Extract `ExecutionContext`
**What changes:** `src/tx/execution_context.rs` (new), `src/momentum/mod.rs` (remove 9 fields, add `exec_ctx: Arc<ExecutionContext>`, update `new()` sig), `src/main.rs` (build ExecutionContext, pass to MomentumEngine), `src/lib.rs` (add module), test helpers updated.

**Verify:**
```bash
cd rust && cargo build -p pump-quant-core 2>&1 | tail -3
cd rust && cargo test -p pump-quant-core 2>&1 | tail -5
```
**Go/no-go:** 530/530 tests pass. Paper trade JSONL output unchanged after 10-minute run.

---

#### Phase 2 — `TradingEngine` Trait
**What changes:** `src/engine/trading_engine.rs` (new), `src/momentum/mod.rs` (add `impl TradingEngine for MomentumEngine` block), `src/lib.rs`.

`main.rs` adds `use crate::engine::trading_engine::TradingEngine;` — no call-site changes yet.

Implement trait **directly on `MomentumEngine`**, not via adapter. Existing methods satisfy the trait; the impl block is thin delegation.

**Verify:**
```bash
cd rust && cargo build -p pump-quant-core 2>&1 | tail -3
cd rust && cargo test -p pump-quant-core 2>&1 | tail -5
# Verify Arc<dyn TradingEngine> compiles:// In a test: let _: Arc<dyn TradingEngine> = Arc::new(momentum_engine);
```
**Go/no-go:** 530 tests pass. `Arc<dyn TradingEngine + Send + Sync>` compiles. No main.rs changes.

---

#### Phase 3 — `FeedRouter` + `EngineRegistry`
**What changes:** `src/engine/feed_router.rs` (new), `src/engine/registry.rs` (new), `src/main.rs` (replace match block with `router.dispatch(event, &registry).await`).

**Verify:**
```bash
cd rust && cargo test -p pump-quant-core 2>&1 | tail -5
# Diff paper trade output before/after — should be identical
```
**Go/no-go:** 533 tests pass (3 new). Paper trades identical. main.rs match block fully removed.

---

#### Phase 4 — Slim `main.rs`
**What changes:** `src/main.rs` only. Remove all remaining direct `momentum_engine.*` calls. Add `router.health_all()`, `registry.shutdown_all()`.

**Verify:**
```bash
wc -l rust/pump-quant-core/src/main.rs  # ≤ 160
grep -c "momentum_engine\." rust/pump-quant-core/src/main.rs  # 0
cd rust && cargo test -p pump-quant-core 2>&1 | tail -5
```
**Go/no-go:** Zero direct momentum references in main.rs. 533 tests pass. 30-minute VPS run stable.

---

#### Phase 5 — `SniperEngine` Stub
**What changes:** `src/sniper/mod.rs` (new), `src/sniper/config.rs` (new), `src/main.rs` (conditional register if config.sniper.enabled), `config/canary.json` (add sniper section, enabled=false).

**Verify:**
```bash
cd rust && cargo test -p pump-quant-core 2>&1 | tail -5
grep '"enabled": false' config/canary.json  # sniper must be disabled
```
**Go/no-go:** 534 tests pass. Sniper absent from health output when disabled. Momentum behavior byte-identical.

---

### 2. Test Strategy

**Existing tests that guard the refactor** — all in `momentum/mod.rs`:
- `on_graduation` unit tests: verify scoring, gate logic, position entry. Constructor sig changes in Phase 1 (pass `Arc<ExecutionContext>`), bodies unchanged.
- `on_tick` tests: exit logic, trailing stops. Same constructor update.
- Scoring functions: pure, unaffected.
- Paper mode tests: no real TXs. Critical regression guard.

**Phase 1 migration of test helpers:**
```rust
// Add to test module:
fn test_exec_ctx() -> Arc<ExecutionContext> {
    Arc::new(ExecutionContext {
        jito_grpc: None,
        nozomi_client: None,
        blockhash_cache: Arc::new(BlockhashCache::new()),
        wallet: None,
        tip_engine: Arc::new(Mutex::new(TipEngine::new(TipConfig::default()))),
        rpc_sender: Arc::new(RpcSender::new_test()),
        rpc_fallback_client: reqwest::Client::new(),
        rpc_fallback_url: Arc::new("https://api.mainnet-beta.solana.com".into()),
        helius_rpc_url: Arc::new("https://test.helius.xyz".into()),
        public_rpc_url: Arc::new("https://api.mainnet-beta.solana.com".into()),
    })
}
// Before: MomentumEngine::new(rpc, jito, nozomi, wallet, ..., config)
// After:  MomentumEngine::new(config, wss_url, log_path, test_exec_ctx())
```

**3 new tests:**

**Test 1 — FeedRouter dispatch (Phase 3):**
```rust
#[tokio::test]
async fn test_feed_router_dispatches_tick_to_enabled_engines() {
    let mock = Arc::new(MockEngine::new(true));
    let mut registry = EngineRegistry::new();
    registry.register(mock.clone());
    registry.dispatch_tick(1000).await;
    assert_eq!(mock.tick_count(), 1);
}

#[tokio::test]
async fn test_feed_router_skips_disabled_engine() {
    let mock = Arc::new(MockEngine::new(false)); // disabled
    let mut registry = EngineRegistry::new();
    registry.register(mock.clone());
    registry.dispatch_tick(1000).await;
    assert_eq!(mock.tick_count(), 0);
}
```

**Test 2 — Multi-engine fan-out (Phase 3):**
```rust
#[tokio::test]
async fn test_registry_fans_out_graduation_to_all_enabled() {
    let a = Arc::new(MockEngine::new(true));
    let b = Arc::new(MockEngine::new(true));
    let c = Arc::new(MockEngine::new(false)); // disabled
    let mut registry = EngineRegistry::new();
    registry.register(a.clone());
    registry.register(b.clone());
    registry.register(c.clone());
    registry.dispatch_graduation(GraduationEvent::test_fixture());
    tokio::time::sleep(Duration::from_millis(10)).await; // let spawns complete
    assert_eq!(a.graduation_count(), 1);
    assert_eq!(b.graduation_count(), 1);
    assert_eq!(c.graduation_count(), 0);
}
```

**Test 3 — SniperEngine trait compliance (Phase 5):**
```rust
#[tokio::test]
async fn test_sniper_engine_satisfies_trait() {
    let engine: Arc<dyn TradingEngine> = Arc::new(
        SniperEngine::new(Arc::new(SniperConfig::default()), test_exec_ctx())
    );
    assert_eq!(engine.name(), "sniper");
    assert!(!engine.enabled());
    assert!(engine.paper_mode());
    engine.on_token_created([0u8; 32], 1000);
    engine.on_graduation(GraduationEvent::test_fixture()).await;
    engine.on_tick(2000).await;
    // No panic = pass
}
```

**MockEngine helper:**
```rust
pub struct MockEngine {
    enabled: bool,
    tick_count: Arc<AtomicU32>,
    graduation_count: Arc<AtomicU32>,
}
impl MockEngine {
    pub fn new(enabled: bool) -> Self { ... }
    pub fn tick_count(&self) -> u32 { self.tick_count.load(Ordering::SeqCst) }
    pub fn graduation_count(&self) -> u32 { self.graduation_count.load(Ordering::SeqCst) }
}
#[async_trait]
impl TradingEngine for MockEngine {
    fn name(&self) -> &'static str { "mock" }
    fn enabled(&self) -> bool { self.enabled }
    fn paper_mode(&self) -> bool { true }
    fn on_token_created(&self, _: [u8; 32], _: u64) {}
    async fn on_graduation(&self, _: GraduationEvent) {
        self.graduation_count.fetch_add(1, Ordering::SeqCst);
    }
    async fn on_tick(&self, _: u64) {
        self.tick_count.fetch_add(1, Ordering::SeqCst);
    }
    fn health(&self) -> EngineHealthSnapshot { EngineHealthSnapshot::Unknown(serde_json::json!({})) }
}
```

---

### 3. SniperEngine Stub

**File:** `rust/pump-quant-core/src/sniper/mod.rs`

```rust
//! SniperEngine — bonding curve sniper (stub, not yet implemented).
//! All methods are no-ops. Disabled by default.

use std::sync::Arc;
use async_trait::async_trait;
use crate::engine::trading_engine::{TradingEngine, GraduationEvent, EngineHealthSnapshot};
use crate::tx::execution_context::ExecutionContext;
use super::config::SniperConfig;

pub struct SniperEngine {
    config: Arc<SniperConfig>,
    exec_ctx: Arc<ExecutionContext>,
}

impl SniperEngine {
    pub fn new(config: Arc<SniperConfig>, exec_ctx: Arc<ExecutionContext>) -> Self {
        Self { config, exec_ctx }
    }
}

#[async_trait]
impl TradingEngine for SniperEngine {
    fn name(&self) -> &'static str { "sniper" }
    fn enabled(&self) -> bool { self.config.enabled }
    fn paper_mode(&self) -> bool { self.config.paper_mode }

    fn on_token_created(&self, mint: [u8; 32], ts_ms: u64) {
        if self.config.enabled {
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                ts_ms,
                "[sniper] token created — stub, scoring not implemented"
            );
        }
    }

    async fn on_graduation(&self, _event: GraduationEvent) {
        // Sniper doesn't act on graduations — it acts on TokenCreated
    }

    async fn on_tick(&self, _ts_ms: u64) {
        // No active positions to manage in stub
    }

    fn health(&self) -> EngineHealthSnapshot {
        EngineHealthSnapshot::Unknown(serde_json::json!({
            "engine": "sniper",
            "enabled": self.config.enabled,
            "paper_mode": self.config.paper_mode,
            "status": "stub — not yet implemented",
            "active_positions": 0,
        }))
    }
}
```

**File:** `rust/pump-quant-core/src/sniper/config.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SniperConfig {
    #[serde(default)]
    pub enabled: bool,                   // default: false

    #[serde(default = "default_true")]
    pub paper_mode: bool,                // default: true

    #[serde(default = "default_position_sol")]
    pub max_position_sol: f64,           // default: 0.05

    #[serde(default = "default_grad_age")]
    pub max_grad_age_s: u32,             // default: 60 (only snipe tokens < 60s old)

    #[serde(default)]
    pub min_social_score: u8,            // default: 0 (0 = no social filter yet)
}

impl Default for SniperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            max_position_sol: 0.05,
            max_grad_age_s: 60,
            min_social_score: 0,
        }
    }
}

fn default_true() -> bool { true }
fn default_position_sol() -> f64 { 0.05 }
fn default_grad_age() -> u32 { 60 }
```

---

### 4. Config Schema After Refactor

```json
{
  "log_file": "data/momentum_paper_trades.jsonl",
  "min_grad_score": 50,

  "momentum": {
    "enabled": true,
    "paper_mode": true,
    "max_grad_age_s": 300,
    "... all existing momentum fields unchanged ..."
  },

  "sniper": {
    "enabled": false,
    "paper_mode": true,
    "max_position_sol": 0.05,
    "max_grad_age_s": 60,
    "min_social_score": 0
  },

  "health": {
    "... unchanged ..."
  }
}
```

**What moves to shared / stays engine-specific:**
- `log_file`, `min_grad_score`, `health` → stay at top level (shared)
- All `momentum.*` fields → stay under `momentum` (unchanged)
- `sniper.*` → new section, ignored if not present (serde defaults)
- No new top-level `execution` section needed — `ExecutionContext` is built from env vars, not config

---

### 5. Risk Register

| # | Risk | Mitigation |
|---|------|-----------|
| 1 | **`async_trait` object safety** — `Arc<dyn TradingEngine + Send + Sync>` fails to compile if any method returns `Self` or has generics | All trait methods use `&self`, no generics, no `Self` in return. `async_trait` crate handles async desugaring. Verify with explicit `let _: Arc<dyn TradingEngine + Send + Sync>` compile test in Phase 2. |
| 2 | **MomentumEngine tests break in Phase 1** — removing 9 struct fields breaks ~40 test constructors | Extract `test_exec_ctx()` helper before touching any test. Update all `MomentumEngine::new()` calls to new signature atomically in one commit. `cargo test` is the go/no-go gate. |
| 3 | **FeedRouter adds tick latency** — extra indirection in hot 50ms tick path | Profile shows `on_tick` returns in <1ms when throttled (engine checks `last_tick_ms`). FeedRouter adds one `for` loop iteration. Not measurable. If ever a concern, `dispatch_tick` can be inlined. |
| 4 | **`ExecutionContext` borrow issues in async tasks** — async move closures capturing `Arc<ExecutionContext>` fields | Always clone the `Arc<ExecutionContext>` into the async block, not individual fields. Pattern: `let ctx = Arc::clone(&self.exec_ctx); tokio::spawn(async move { ctx.jito_grpc... })`. All current async tasks already use this pattern with individual Arcs. |
| 5 | **Phase ordering dependency** — shipping Phase 3 (FeedRouter) before Phase 2 (trait impl) would break dispatch | Enforce strict PR order: 1 → 2 → 3 → 4 → 5. Each phase's `cargo build` gate catches the dependency. Phase 3 PR should have Phase 2 as explicit prerequisite in PR description. |

---

## Engineer Assignment (5 parallel engineers)

Once this spec is approved, assign phases to engineers as follows. Phases 1-2 are sequential (dependency). Phases 3-4 can overlap once Phase 2 merges.

| Engineer | Phase | Files | Est. complexity |
|---|---|---|---|
| Eng-1 | Phase 1 — ExecutionContext extraction | `execution_context.rs` (new), `momentum/mod.rs` (infra fields), `main.rs`, test helpers | Medium — mechanical but touches many call sites |
| Eng-2 | Phase 2 — TradingEngine trait | `trading_engine.rs` (new), `momentum/mod.rs` (trait impl block) | Low — mostly new code, minimal surgery |
| Eng-3 | Phase 3 — FeedRouter + Registry | `feed_router.rs` (new), `registry.rs` (new), `main.rs` (replace match block), test_utils MockEngine | Medium — new architecture, careful equivalence |
| Eng-4 | Phase 4 — Slim main.rs | `main.rs` only — remove direct momentum references | Low — cleanup, no new logic |
| Eng-5 | Phase 5 — SniperEngine stub | `sniper/mod.rs` (new), `sniper/config.rs` (new), `main.rs` (register), `canary.json` | Low — all new code, no surgery |

**Sequencing:** Eng-1 merges → Eng-2 starts. Eng-2 merges → Eng-3 + Eng-4 can work in parallel. Eng-3 merges → Eng-5 starts.

**All engineers:** Run `cd rust && cargo test -p pump-quant-core 2>&1 | tail -5` before every commit. 530 tests must pass at every phase boundary.

