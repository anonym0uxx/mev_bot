# Momentum Engine Architecture

**Date:** 2026-03-29 | **Status:** Implementation-ready | **Depends on:** MOMENTUM_ENGINE_SPEC.md

---

## 1. Module Structure & File Layout

```
rust/pump-quant-core/src/
  momentum/
    mod.rs           — MomentumEngine, on_graduation(), on_tick()  (~400 LOC)
    position.rs      — MomentumPosition (256B), PendingEntry, exit FSM  (~280 LOC)
    scorer.rs        — GraduationScore, integer-only scoring  (~130 LOC)
    price_feed.rs    — PriceFeedManager, Helius accountSubscribe WS  (~350 LOC)
    logger.rs        — Re-export + helpers  (~50 LOC)
    config.rs        — MomentumConfig + serde defaults  (~130 LOC)
  arb/
    pool_resolver.rs — NEW: extracted shared PoolInfo + resolve fn  (~220 LOC)
    mod.rs           — MODIFIED: add pool_resolver re-exports
    graduation.rs    — MODIFIED: delegate pool calls to pool_resolver
  persistence/
    momentum_logger.rs — NEW: JSONL BufWriter thread  (~150 LOC)
  engine/config.rs   — MODIFIED: add momentum_* fields  (+60 LOC)
  lib.rs             — MODIFIED: add `pub mod momentum;`  (+1 LOC)
  main.rs            — MODIFIED: wire engine + logger thread  (+50 LOC)
```

**New files:** 8 | **Modified files:** 5 | **Estimated new LOC:** ~1,760

---

## 2. Core Data Structures

### 2.1 MomentumPosition — exactly 256 bytes, cache-line aligned

```rust
#[repr(C, align(64))]
pub struct MomentumPosition {
    pub mint: [u8; 32],              //  32  offset 0
    pub entry_ts_ms: u64,            //   8  offset 32
    pub entry_price_atoms: u64,      //   8  offset 40  — lamports/1M atoms
    pub size_lamports: u64,          //   8  offset 48
    pub peak_price_atoms: AtomicU64, //   8  offset 56  — lock-free trailing stop
    pub last_sample_ts_ms: u64,      //   8  offset 64
    pub price_samples: [u16; 30],    //  60  offset 72  — biased bps (val+10000)
    pub remaining_bps: u16,          //   2  offset 132 — 10000=100%
    pub grad_speed_s: u16,           //   2  offset 134
    pub grad_volume_sol_x10: u16,    //   2  offset 136
    pub pre_grad_buys_5s: u8,        //   1  offset 138
    pub sample_count: u8,            //   1  offset 139
    pub pool_type: u8,               //   1  offset 140 — 0=raydium, 1=pumpswap
    pub grad_score: u8,              //   1  offset 141
    pub tp_state: u8,                //   1  offset 142 — bitmask b0=TP1,b1=TP2,b2=TP3
    pub exit_reason: u8,             //   1  offset 143
    pub _pad: [u8; 112],             // 112  offset 144
}
// TOTAL: 32+8+8+8+8+8+60+2+2+2+1+1+1+1+1+1+112 = 256 ✓
const _: () = assert!(size_of::<MomentumPosition>() == 256);
const _: () = assert!(align_of::<MomentumPosition>() == 64);
```

**Price sample encoding:** `encoded = (actual_bps + 10000) as u16`. Range: -100% to +555%. Resolution: 1 bps. Samples at 10s intervals, 30 slots = 300s max hold.

### 2.2 PendingEntry — ring buffer for delayed entries

```rust
#[derive(Clone, Copy)]
pub struct PendingEntry {
    pub mint: [u8; 32],            //  32
    pub trigger_ts_ms: u64,        //   8  — grad_ts + entry_delay_ms
    pub graduation_ts_ms: u64,     //   8
    pub coin_vault: [u8; 32],      //  32  — resolved at grad time
    pub pc_vault: [u8; 32],        //  32  — resolved at grad time
    pub reserve_sol_lamports: u64, //   8
    pub reserve_token_atoms: u64,  //   8
    pub grad_speed_s: u16,         //   2
    pub grad_volume_sol_x10: u16,  //   2
    pub pre_grad_buys_5s: u8,      //   1
    pub pool_type: u8,             //   1
    pub grad_score: u8,            //   1  — without recovery (added at entry)
    pub active: bool,              //   1
}
// 64-slot ring buffer. Mutex-protected. <30 events/day → zero contention.
const PENDING_RING_SIZE: usize = 64;
```

### 2.3 GraduationScore — 8 bytes, integer-only

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GraduationScore {
    pub speed_score: u8,    // 0-25
    pub volume_score: u8,   // 0-25
    pub velocity_score: u8, // 0-25
    pub recovery_score: u8, // 0-25 (deferred to entry time)
    pub total: u8,          // sum 0-100
    pub _pad: [u8; 3],
}
```

### 2.4 PoolInfo — shared pool resolution result

```rust
/// Used by both GraduationArbEngine and MomentumEngine.
/// Lives in arb/pool_resolver.rs.
pub struct PoolInfo {
    pub mint: [u8; 32],
    pub pool_address: [u8; 32],
    pub pool_type: PoolType,
    pub coin_vault: [u8; 32],   // SPL token account (base token)
    pub pc_vault: [u8; 32],     // SPL token account (WSOL)
    pub reserve_token_atoms: u64,
    pub reserve_sol_lamports: u64,
}
```

### 2.5 MomentumExitReason

```rust
#[repr(u8)]
pub enum MomentumExitReason {
    Open = 0, Tp1 = 1, Tp2 = 2, TpCeiling = 3,
    TrailingStop = 4, HardSl = 5, TimeSl = 6, MaxHold = 7,
    DailyLossCap = 8, ScoreTooLow = 9, PoolFailed = 10,
}
```

---

## 3. Price Feed Design (WebSocket accountSubscribe)

### 3.1 Architecture

```
Helius WSS ──accountSubscribe──▶ PriceFeedManager ──AtomicU64──▶ MomentumEngine.on_tick()
  (persistent)                   (dedicated tokio task)          (main event loop)
```

### 3.2 PriceFeedManager

```rust
pub struct PriceFeedManager {
    cmd_tx: mpsc::Sender<PriceFeedCmd>,
    vault_subs: Arc<DashMap<[u8; 32], (u64, u64)>>,  // mint → (coin_sub, pc_sub)
    pub prices: Arc<DashMap<[u8; 32], PriceState>>,
}

pub struct PriceState {
    pub coin_reserve_atoms: AtomicU64,
    pub sol_reserve_lamports: AtomicU64,
    pub last_update_ms: AtomicU64,
}

pub enum PriceFeedCmd {
    Subscribe { mint: [u8; 32], coin_vault: [u8; 32], pc_vault: [u8; 32] },
    Unsubscribe { mint: [u8; 32] },
    Shutdown,
}
```

### 3.3 WS I/O task — dedicated tokio::spawn

- Persistent Helius WSS connection
- On `Subscribe`: `accountSubscribe` for both vault SPL accounts, `commitment: confirmed`
- On WS message: parse SPL amount = `u64::from_le_bytes(data[64..72])`, store via `AtomicU64::store(Relaxed)`
- Reconnect with exponential backoff: 100ms → 200ms → 400ms → cap 5s
- Zero lock contention: tick loop uses `DashMap::get()` + `AtomicU64::load(Relaxed)`

### 3.4 Price computation

```rust
#[inline(always)]
pub fn price_from_reserves(sol_lamports: u64, token_atoms: u64) -> u64 {
    if token_atoms == 0 { return 0; }
    ((sol_lamports as u128 * 1_000_000) / token_atoms as u128) as u64
}
```

### 3.5 Subscription lifecycle

| Phase | Time | Action |
|-------|------|--------|
| Graduation | T=0 | Subscribe to coin_vault + pc_vault |
| Entry check | T+15s | Read price, compute recovery score, decide entry |
| Position open | T+15s..T+315s | on_tick reads price every 150ms |
| Position close | varies | Unsubscribe from both vaults |
| Rejected entry | T+15s | Unsubscribe (score too low) |

Max concurrent: 3 positions × 2 + 5 pending × 2 = 16 account subscriptions.

---

## 4. Tick Loop Design (150ms intervals)

### 4.1 on_tick — branch-ordered for CPU prediction

```rust
#[inline(always)]
pub fn on_tick(&self, now_ms: u64) {
    if !self.config.enabled { return; }                          // predict: not-taken
    let last = self.last_tick_ms.load(Relaxed);
    if now_ms.wrapping_sub(last) < self.config.check_ms { return; } // skip 2/3
    self.last_tick_ms.store(now_ms, Relaxed);
    if self.over_daily_loss_cap() { return; }
    let has_pos = !self.positions.is_empty();
    let has_pending = self.pending_ring_count() > 0;
    if !has_pos && !has_pending { return; }
    if has_pending { self.process_pending_entries(now_ms); }     // cold
    if has_pos { self.process_active_positions(now_ms); }        // hot
}
```

### 4.2 TP/SL state machine (priority order)

Exit checks on every tick, ordered by urgency:

1. **Hard SL** (-12%): immediate full exit
2. **TP ceiling** (+50%): dump remaining position
3. **TP2** (+15%): partial exit (30%), activate trailing stop
4. **TP1** (+5%): partial exit (30%)
5. **Trailing stop** (8% below peak, active after TP2): full remaining exit
6. **Time SL** (60s elapsed, PnL < -2%): full exit
7. **Max hold** (300s): full exit

TP1/TP2 are partial exits — decrement `remaining_bps`, set `tp_state` bits. Position fully closes when `remaining_bps == 0` or a full-exit reason triggers.

### 4.3 Price sample recording

Every 10s: `maybe_record_sample()` writes biased bps to `price_samples[sample_count]`, increments `sample_count`. Uses `last_sample_ts_ms` to gate — no timer, just check `now_ms - last >= 10000`.

---

## 5. Graduation Scoring (integer arithmetic)

```rust
#[inline(always)]
pub fn score_graduation(speed_s: u16, volume_x10: u16, buys_5s: u8) -> GraduationScore {
    let speed = match speed_s {
        0..=59    => 25u8,
        60..=299  => 20,
        300..=899 => 15,
        900..=1799 => 10,
        1800..=3599 => 5,
        _ => 0,
    };
    let volume = std::cmp::min(volume_x10 / 200, 25) as u8;
    let velocity = std::cmp::min(buys_5s, 25);
    GraduationScore { speed_score: speed, volume_score: volume,
        velocity_score: velocity, recovery_score: 0,
        total: speed + volume + velocity, _pad: [0; 3] }
}
```

**Recovery score** (computed at T+15s entry time):
```rust
#[inline(always)]
pub fn recovery_score(current_price: u64, bc_terminal_price: u64) -> u8 {
    if bc_terminal_price == 0 { return 0; }
    let discount_bps = if current_price >= bc_terminal_price { 0i64 }
        else { ((bc_terminal_price - current_price) as i128 * 10000
                / bc_terminal_price as i128) as i64 };
    match discount_bps { 0..=500 => 25, 501..=1000 => 15, 1001..=2000 => 5, _ => 0 }
}
```

Total score at entry = speed + volume + velocity + recovery (0-100). Gate: `>= config.min_grad_score`.

---

## 6. Integration Points

### 6.1 Decision: Option B — parallel dispatch from main.rs

Both engines are `tokio::spawn`'d from the same `FeedEvent::Migration` handler:

```rust
Ok(FeedEvent::Migration { mint, ts_ms, source, sig }) => {
    hot_path.on_migration(&mint, ts_ms);
    // ... existing drain ...

    if engine_config.graduation_arb_enabled {
        let e = Arc::clone(&grad_arb_engine);
        tokio::spawn(async move { e.on_migration(mint, ts_ms, source, sig).await; });
    }
    if engine_config.momentum_enabled {  // NEW
        let e = Arc::clone(&momentum_engine);
        tokio::spawn(async move { e.on_graduation(mint, ts_ms, source, sig).await; });
    }
}
```

**Why Option B over Option A (nesting inside GraduationArbEngine):**

1. **Independent lifecycles**: momentum can run with arb disabled (and vice versa)
2. **Zero latency cost**: both are `tokio::spawn` — parallel, not sequential
3. **Clean testing**: each engine testable in isolation
4. **Own dedup**: momentum needs longer TTL (accounts for 15s entry delay)
5. **Shared infra via pool_resolver.rs**: no coupling needed for shared code

### 6.2 on_graduation cold path

```rust
#[inline(never)] #[cold]
pub async fn on_graduation(&self, mint: [u8;32], ts_ms: u64, source: MigrationSource, sig: [u8;64]) {
    if !self.config.enabled { return; }
    // 1. Dedup (own instance)
    // 2. Pool resolution via shared pool_resolver (timeout 500ms)
    // 3. Score: speed + volume + velocity (recovery deferred)
    // 4. If score >= min_score (excl. recovery): schedule pending entry
    // 5. Subscribe to price feed immediately
    // 6. Insert into pending ring with trigger_ts = ts_ms + entry_delay_ms
}
```

---

## 7. Shared Pool Resolver (arb/pool_resolver.rs)

### 7.1 Extraction from graduation.rs

Functions to extract into `arb/pool_resolver.rs`:
- `resolve_pool_from_transaction()` → renamed `resolve_pool()`
- `resolve_pool_inner()`
- `extract_vaults_from_tx_response()`
- `parse_spl_token_amount()`
- `fetch_vault_reserves()`
- `extract_fallback_mint()`
- `decode_bs58_32()`
- `make_pool_resolution_client()`
- `PoolType` enum (already public)
- `PoolResolution` struct → renamed/extended to `PoolInfo` with vault addresses

### 7.2 New PoolInfo (superset of old PoolResolution)

```rust
pub struct PoolInfo {
    pub mint: [u8; 32],
    pub pool_address: [u8; 32],
    pub pool_type: PoolType,
    pub coin_vault: [u8; 32],   // NEW: needed by momentum price feed
    pub pc_vault: [u8; 32],     // NEW: needed by momentum price feed
    pub reserve_token_atoms: u64,
    pub reserve_sol_lamports: u64,
    pub bc_terminal_vsol: f64,  // kept for arb engine compatibility
}
```

### 7.3 graduation.rs changes

Replace internal pool resolution calls with:
```rust
use super::pool_resolver::{resolve_pool, PoolInfo};
```

No behavior change — pure refactor. All 28 existing graduation tests must still pass.

---

## 8. JSONL Schema

### 8.1 Per-completed-position record

```json
{
  "strategyTag": "momentum",
  "engineVersion": "mom-v1",
  "mint": "7mHC...pump",
  "poolType": "raydium_amm_v4",
  "gradScore": 72,
  "gradSpeedS": 45,
  "gradVolumeSol": 87.3,
  "preGradBuys5s": 12,
  "speedScore": 25,
  "volumeScore": 20,
  "velocityScore": 12,
  "recoveryScore": 15,
  "entryDelayMs": 15000,
  "entryPriceLamportsPerMAtoms": 381900,
  "bcTerminalPriceLamportsPerMAtoms": 410880,
  "structuralDiscountPct": 7.07,
  "entryTimestampMs": 1711700000000,
  "exitTimestampMs": 1711700023400,
  "holdMs": 23400,
  "exitReason": "tp2",
  "remainingBps": 4000,
  "sizeLamports": 300000000,
  "grossPnlSol": 0.045,
  "feeSol": 0.0015,
  "netPnlSol": 0.0435,
  "priceSamplesBps": [10000, 10250, 10800, 11200, 10900],
  "sampleCount": 5,
  "tpState": 3,
  "isPaper": true,
  "configVersion": "mom-v0.35sol_15000ms"
}
```

### 8.2 Logger thread

Mirrors `persistence/grad_arb_logger.rs` pattern:
- Dedicated thread (not tokio task — matches existing pattern)
- `crossbeam_channel::Receiver<MomentumClosedPosition>`
- `BufWriter<File>` with explicit flush per record
- File: `data/momentum_paper_trades.jsonl`

---

## 9. Config Schema

### 9.1 MomentumConfig struct

```rust
#[derive(Debug, Clone)]
pub struct MomentumConfig {
    pub enabled: bool,              // false
    pub paper_mode: bool,           // true
    pub entry_delay_ms: u64,        // 15000
    pub min_grad_score: u8,         // 40 (0-100, excl recovery at filter time)
    pub position_size_sol: f64,     // 0.30
    pub max_concurrent: u8,         // 3
    pub tp1_pct: f64,               // 5.0
    pub tp1_exit_pct: f64,          // 0.30
    pub tp2_pct: f64,               // 15.0
    pub tp2_exit_pct: f64,          // 0.30
    pub tp3_pct: f64,               // 50.0 (ceiling)
    pub tp3_exit_pct: f64,          // 1.0 (dump remaining)
    pub trailing_stop_pct: f64,     // 8.0
    pub hard_sl_pct: f64,           // 12.0
    pub time_sl_ms: u64,            // 60000
    pub max_hold_ms: u64,           // 300000
    pub check_ms: u64,              // 150
    pub daily_loss_cap_sol: f64,    // 2.0
    pub raydium_fee_bps: u32,       // 25
    pub pumpswap_fee_bps: u32,      // 100
    pub skip_pumpswap: bool,        // true (Raydium-only initially)
    pub dedup_ttl_ms: u64,          // 30000 (longer than arb: accounts for 15s delay)
    pub rpc_timeout_ms: u64,        // 500
}
```

### 9.2 canary.json additions

```json
{
  "mev": {
    "momentum_enabled": false,
    "momentum_paper_mode": true,
    "momentum_entry_delay_ms": 15000,
    "momentum_min_grad_score": 40,
    "momentum_position_size_sol": 0.30,
    "momentum_max_concurrent": 3,
    "momentum_tp1_pct": 5.0,
    "momentum_tp1_exit_pct": 0.30,
    "momentum_tp2_pct": 15.0,
    "momentum_tp2_exit_pct": 0.30,
    "momentum_tp3_pct": 50.0,
    "momentum_trailing_stop_pct": 8.0,
    "momentum_hard_sl_pct": 12.0,
    "momentum_time_sl_ms": 60000,
    "momentum_max_hold_ms": 300000,
    "momentum_check_ms": 150,
    "momentum_daily_loss_cap_sol": 2.0
  }
}
```

### 9.3 EngineConfig additions

Add to `MevJsonConfig` (raw JSON) and `EngineConfig` (parsed) in `engine/config.rs`:
- 18 new `Option<T>` fields in `MevJsonConfig`
- 18 new non-optional fields in `EngineConfig` with defaults
- Builder pattern in `EngineConfig::from()` mapping each field

---

## 10. Machine-Level Optimization Checklist

| # | Optimization | Where | Status |
|---|-------------|-------|--------|
| 1 | `#[repr(C, align(64))]` on MomentumPosition | position.rs | Required |
| 2 | `#[inline(always)]` on on_tick, read_current_price, check_exit, price_from_reserves, score_graduation | mod.rs, position.rs, scorer.rs, price_feed.rs | Required |
| 3 | `#[inline(never)] #[cold]` on on_graduation, process_pending, close_position, JSONL write | mod.rs | Required |
| 4 | AtomicU64 for peak_price (no Mutex on price read) | position.rs, price_feed.rs | Required |
| 5 | PendingEntry ring buffer (64 slots, no heap alloc) | position.rs | Required |
| 6 | Branch ordering: !enabled → throttle → loss cap → empty | mod.rs on_tick | Required |
| 7 | `[u8;32]` keys in DashMap (not String) | mod.rs, price_feed.rs | Required |
| 8 | `u64::from_le_bytes(data[64..72])` for SPL parse | price_feed.rs | Required |
| 9 | No Arc cloning on hot path (hold &self refs) | mod.rs on_tick | Required |
| 10 | Separate tokio tasks: WS I/O, tick (main loop), JSONL writer | price_feed.rs, main.rs | Required |
| 11 | ArrayVec<8> for to_close list (no heap) | mod.rs process_active | Required |
| 12 | u128 intermediate in price calc (overflow-safe) | price_feed.rs | Required |
| 13 | Biased u16 for price samples (not f64) | position.rs | Required |
| 14 | Integer-only scoring (no f64 in scorer) | scorer.rs | Required |
| 15 | Compile-time size/align assertions | position.rs | Required |

---

## 11. Build Plan

### Task 1: Shared pool_resolver.rs module
- **Files:** Create `arb/pool_resolver.rs`. Modify `arb/mod.rs`, `arb/graduation.rs`
- **Key functions:** `resolve_pool()`, `PoolInfo` struct (superset of `PoolResolution`)
- **What:** Extract ~200 lines of pool resolution from graduation.rs. Add `coin_vault`/`pc_vault` fields to output. Update graduation.rs to import from pool_resolver. All 28 existing graduation tests must pass.
- **Dependencies:** None (pure refactor)
- **Complexity:** **M** — careful extraction, must preserve all test behavior
- **Est. LOC:** 220 new + 30 changed

### Task 2: momentum/config.rs + momentum/mod.rs skeleton
- **Files:** Create `momentum/config.rs`, `momentum/mod.rs`. Modify `engine/config.rs`, `lib.rs`
- **Key functions:** `MomentumConfig::default()`, `MomentumEngine::new()`, stub `on_tick()`, stub `on_graduation()`
- **What:** Config struct with all 18+ fields, serde deserialization, defaults. Engine skeleton with DashMap + AtomicU64 stats. Wire into config loader. Add `pub mod momentum` to lib.rs.
- **Dependencies:** None
- **Complexity:** **S** — boilerplate config + skeleton
- **Est. LOC:** 260 new + 60 changed

### Task 3: momentum/price_feed.rs (WebSocket accountSubscribe)
- **Files:** Create `momentum/price_feed.rs`
- **Key functions:** `PriceFeedManager::new()`, `PriceFeedManager::start()`, `price_feed_ws_loop()`, `PriceState`, `price_from_reserves()`
- **What:** Helius WSS connection, accountSubscribe/Unsubscribe for vault SPL accounts, AtomicU64 price storage, reconnect with exponential backoff, command channel (mpsc).
- **Dependencies:** Task 2 (config for WS URL)
- **Complexity:** **L** — WebSocket lifecycle, reconnection, async coordination
- **Est. LOC:** 350 new

### Task 4: momentum/scorer.rs + momentum/position.rs
- **Files:** Create `momentum/scorer.rs`, `momentum/position.rs`
- **Key functions:** `score_graduation()`, `recovery_score()`, `MomentumPosition` struct, `PendingEntry`, `PendingRing`, `MomentumExitReason`, `check_exit_conditions()`, `handle_tp_tier()`
- **What:** Integer-only scorer. 256-byte position struct with compile-time size assertions. Pending entry ring buffer (64 slots). TP/SL state machine with partial exit support.
- **Dependencies:** Task 2 (config for TP/SL thresholds)
- **Complexity:** **M** — position struct layout math, TP/SL FSM, scoring logic
- **Est. LOC:** 410 new

### Task 5: momentum/logger.rs + persistence/momentum_logger.rs
- **Files:** Create `momentum/logger.rs`, `persistence/momentum_logger.rs`. Modify `persistence/mod.rs`
- **Key functions:** `MomentumPaperLogger::new()`, `MomentumPaperLogger::log()`, `MomentumClosedPosition` struct
- **What:** JSONL writer mirroring grad_arb_logger.rs pattern. BufWriter + flush per record. MomentumClosedPosition with all fields from §8 schema. crossbeam_channel receiver.
- **Dependencies:** Task 4 (MomentumPosition for field mapping)
- **Complexity:** **S** — follows established pattern exactly
- **Est. LOC:** 200 new

### Task 6: MomentumEngine::on_tick() + TP/SL logic
- **Files:** Modify `momentum/mod.rs`
- **Key functions:** `on_tick()`, `process_active_positions()`, `process_pending_entries()`, `try_enter_position()`, `close_position()`, `read_current_price()`, `maybe_record_sample()`
- **What:** Full tick loop implementation. Branch ordering. Price read from AtomicU64. Exit condition checker with priority ordering. Partial exit handling (TP tiers). Price sample recording. Daily loss tracking.
- **Dependencies:** Task 3 (price feed), Task 4 (position + scorer), Task 5 (logger)
- **Complexity:** **L** — core engine logic, state machine, all the hot-path optimizations
- **Est. LOC:** 300 new (filling in mod.rs stubs from Task 2)

### Task 7: Integration into main.rs + GraduationArbEngine
- **Files:** Modify `main.rs`
- **Key functions:** MomentumEngine construction, tokio::spawn dispatch, logger thread spawn, stats sync
- **What:** Wire MomentumEngine into main event loop. Spawn price feed WS task. Spawn JSONL logger thread. Add `momentum_engine.on_tick(ts_ms)` to Tick handler. Add `momentum_engine.on_graduation()` to Migration handler (tokio::spawn). Stats integration with API.
- **Dependencies:** Task 1-6 (all momentum modules complete)
- **Complexity:** **M** — wiring, follows existing grad_arb pattern closely
- **Est. LOC:** 50 new in main.rs

### Task 8: Tests (≥20 new tests)
- **Files:** Add `#[cfg(test)]` blocks in each momentum/*.rs file
- **Test categories:**
  - `position.rs`: size/align assertions, biased bps encoding/decoding, pending ring FIFO, tp_state bitmask (5 tests)
  - `scorer.rs`: speed scoring brackets, volume scoring, velocity scoring, recovery scoring, combined score (5 tests)
  - `price_feed.rs`: price_from_reserves correctness, zero-guard, overflow safety (3 tests)
  - `mod.rs`: on_tick throttle, exit priority order, partial exit remaining_bps, daily loss cap, max concurrent (5 tests)
  - `pool_resolver.rs`: extraction parity (existing tests still pass), PoolInfo vault fields (2 tests)
  - `config.rs`: defaults, serde roundtrip (2 tests)
- **Dependencies:** Task 1-7 (need implementations to test)
- **Complexity:** **M** — 22+ test functions
- **Est. LOC:** 500 new
### Task 9: Config integration + canary.json defaults
- **Files:** Modify `config/canary.json`, restart daemon
- **Key functions:** Add `[momentum]` section to TOML/JSON config with all defaults
- **What:** Wire MomentumConfig into config loader, validate fields, add to startup dump log, restart daemon.
- **Dependencies:** Task 7 (integration complete)
- **Complexity:** **S** — config wiring + restart
- **Est. LOC:** 30 new + 20 changed

---

## 11. Build Summary & Parallelization

```
PHASE 1 (parallel — no deps):
  Task 1: pool_resolver.rs          [S] ~120 LOC
  Task 2: config.rs + mod.rs stub   [S] ~320 LOC

PHASE 2 (after Phase 1):
  Task 3: price_feed.rs             [L] ~350 LOC  — needs Task 2
  Task 4: scorer.rs + position.rs   [M] ~410 LOC  — needs Task 2
  Task 5: logger.rs                 [S] ~200 LOC  — needs Task 4

PHASE 3 (after Phase 2):
  Task 6: on_tick() + TP/SL        [L] ~300 LOC  — needs 3,4,5
  Task 8: Tests                     [M] ~500 LOC  — needs 3,4,5,6

PHASE 4 (final):
  Task 7: main.rs integration       [M] ~50 LOC   — needs 6
  Task 9: config + restart          [S] ~50 LOC   — needs 7

Total: ~2,300 new LOC, 130 new LOC changed
New tests: ≥22
```

**Engineer agent split (recommended):**
- Agent A: Tasks 1 + 2 + 5 (sequential, all S/M, low risk)
- Agent B: Tasks 3 + 4 (parallel with Agent A, L complexity, core data structures)
- Agent C: Tasks 6 + 7 + 8 + 9 (final integration — runs after A+B complete)
