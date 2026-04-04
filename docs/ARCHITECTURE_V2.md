# ARCHITECTURE V2 — pump-quant Engine Refactor

**Date:** 2026-03-29  
**Authors:** Rust/Tokio Architect, Solana MEV Engineer, Systems Engineer  
**Status:** APPROVED — Ready for implementation

---

## 0. Executive Summary

Refactor pump-quant-core from a single backrun engine with tangled graduation arb stubs into two clean, independent logical engines sharing one binary:

1. **BackrunEngine** — existing momentum-backrun pipeline, cleaned up (remove golden/standard split)
2. **GraduationArbEngine** — new engine for pump.fun graduation migration arbitrage (paper mode first)

Key changes:
- Eliminate `strategyTag` golden/standard split → single `"backrun"` tag
- Extract graduation arb into fully independent engine: own config, JSONL, stats, exit logic
- Add Helius-based graduation detection (faster than Bitquery by ~50ms)
- Drop CoreCast stream 4 (new token pre-warm) — redundant with PumpPortal
- Main loop dispatches migration events to both engines inline (no broadcast channel needed)

---

## 1. Clean Separation Design

### 1.1 Process Architecture

```
pump-quant binary (single tokio runtime, single OS process)
│
├── Feeds (shared, single connections)
│   ├── PumpPortal WS → crossbeam → EventJoiner
│   ├── Helius WS → crossbeam → EventJoiner
│   ├── ShredStream gRPC → crossbeam → EventJoiner (optional)
│   └── CoreCast/Bitquery WS → engine_tx direct (3 streams, was 4)
│
├── EventJoiner thread → engine_tx (crossbeam, capacity 1024)
│
├── Engine Loop (main thread, single-threaded)
│   ├── Reads from engine_rx
│   ├── Dispatches to BackrunEngine (inline, synchronous — zero alloc hot path)
│   ├── Dispatches to GraduationArbEngine (async via tokio::spawn)
│   └── Drains closed positions from both engines → logger threads
│
├── BackrunEngine (existing HotPath, cleaned up)
│   ├── config: canary.json → mev.backrun { ... }
│   ├── events: Trade, PreWarm, CreatorSell, Migration(force-exit), LpRemoval, Tick
│   ├── JSONL: data/backrun_paper_trades.jsonl
│   ├── SQLite: data/backrun_paper_trades.sqlite
│   ├── stats: trades_seen, gates_passed, positions_opened, wins, losses, pnl
│   └── exit: TP, SL, NB, MaxHold, MomentumDecay, IntraHoldTrail, MigrationForceExit
│
└── GraduationArbEngine (new)
    ├── config: canary.json → mev.graduation_arb { ... }
    ├── events: Migration(entry trigger), Tick(position management)
    ├── JSONL: data/graduation_paper_trades.jsonl
    ├── stats: migrations_detected, arb_entries, arb_exits, wins, losses, pnl, timeouts
    └── exit: TP(3%), SL(2%), MaxHold(5000ms)
```

### 1.2 Tokio Runtime Sharing

Single `#[tokio::main]` runtime. Both engines share it.

- **BackrunEngine (HotPath):** Synchronous on the main engine loop thread. Zero async. Unchanged.
- **GraduationArbEngine:** Uses `tokio::spawn` for async operations (RPC calls for pool lookup/reserves). Engine state in `Arc`-wrapped structures.
- **Feed tasks:** Each feed runs as a spawned tokio task. Unchanged.

### 1.3 Migration Signal Flow

Migration events originate from two sources:
1. **Helius `logsSubscribe`** — detects Raydium `initialize2` CPI within pump.fun transactions (~50ms faster)
2. **CoreCast stream 2** — detects Raydium AMM DEX trades for pump.fun tokens (confirmation/fallback)

```
FeedEvent::Migration arrives at engine_rx
    │
    ├─→ BackrunEngine.on_migration(mint, ts_ms)
    │     └── Force-close any open backrun position for this mint
    │     └── Mark creator_sell_at_ms to block re-entry
    │
    └─→ GraduationArbEngine.on_migration(mint, ts_ms, source, sig)
          └── Dedup check (DashMap<mint, (ts_ms, source)>, 10s TTL)
          └── If first detection → tokio::spawn with 200ms timeout:
                1. Resolve pool address (derive PDA or getTransaction)
                2. Fetch pool reserves via getAccountInfo
                3. Calculate spread vs BC terminal price
                4. If spread >= min_spread_pct → open paper position
                5. Log entry to graduation_paper_trades.jsonl
```

### 1.4 Event Dispatch (No Broadcast Channel)

The current crossbeam channel architecture is correct for the latency-critical backrun path. Adding `tokio::broadcast` would require `Clone` on `FeedEvent` (expensive: 64-byte sig field) and introduces lagged-receiver failure modes.

The main engine loop dispatches inline:

```rust
Ok(FeedEvent::Migration { mint, ts_ms, source, sig }) => {
    // BackrunEngine: synchronous force-exit (~100ns)
    hot_path.on_migration(&mint, ts_ms);
    drain_closed_positions(&closed_rx, &mut hot_path, &logger_tx, &telegram_alerter);

    // GraduationArbEngine: async entry evaluation (spawned task)
    if grad_arb_engine.enabled() {
        grad_arb_engine.on_migration(mint, ts_ms, source, sig);
    }
}
```

### 1.5 Stats for API

`/api/stats` returns two distinct sections plus a combined summary:

```json
{
  "data": {
    "backrun": {
      "trades_seen": 150000,
      "gates_passed": 420,
      "positions_opened": 380,
      "wins": 200,
      "losses": 175,
      "win_rate": 0.533,
      "pnl_sol": 0.042
    },
    "graduation_arb": {
      "enabled": true,
      "mode": "paper",
      "migrations_detected": 47,
      "arb_entries": 12,
      "arb_exits": 12,
      "wins": 7,
      "losses": 5,
      "win_rate": 0.583,
      "net_sol": 0.008,
      "avg_spread_pct": 4.2,
      "avg_hold_ms": 1840,
      "timeouts": 3
    },
    "combined": {
      "total_positions": 392,
      "total_pnl_sol": 0.050,
      "uptime_s": 86400
    }
  }
}
```

Two stat structs (`BackrunStats` in `Arc<Mutex<>>`, `GradArbStats` using `Arc<AtomicU64>` counters). API handler reads both and merges at request time.

---

## 2. Helius Graduation Detection

### 2.1 Current State

`helius.rs` parses ONLY trade events (Buy/Sell). It does NOT detect graduation events.

Current behavior:
- Subscribes to `logsSubscribe` on pump.fun program `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`
- Detects `"Program log: Instruction: Buy"` and `"Program log: Instruction: Sell"`
- Emits `PreWarmEvent` with `mint=[0u8; 32]` (logsSubscribe provides no accountKeys)
- Does NOT look for Raydium program invocations in logs

### 2.2 Graduation Log Signature

When a pump.fun token graduates, the migration transaction invokes both programs. The `logsSubscribe` on pump.fun will see:

```
"Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]"
"Program log: Instruction: Withdraw"
"Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [2]"  ← Raydium CPI
"Program log: initialize2"
"Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 success"
```

**Critical limitation:** `logsSubscribe` does NOT provide `accountKeys`. We get `signature` and `logs[]` but NOT the mint address or pool address.

### 2.3 Detection Strategy

**Phase 1 (paper mode): logsSubscribe + `getTransaction` follow-up.**

1. `parse_helius_log()` detects graduation: check for Raydium AMM program ID in log lines
2. Emit `FeedEvent::Migration { mint: [0u8;32], sig: Some(sig), source: HeliusLogs }`
3. GraduationArbEngine receives it, spawns async task:
   - Call `getTransaction(sig, { encoding: "jsonParsed" })` to get full account list
   - Extract mint + pool address from transaction accounts
   - Proceed with arb evaluation
4. Adds ~50-100ms HTTP latency, but we still detect ~50ms before Bitquery, so net ~0-50ms advantage

**Phase 2 (future upgrade): Helius `transactionSubscribe`** (enhanced WebSocket) provides full parsed transactions with account keys. Eliminates the `getTransaction` round-trip entirely.

### 2.4 Helius Parser Changes

Add graduation detection to `parse_helius_log()`:

```rust
const RAYDIUM_AMM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

fn parse_helius_log(text: &str) -> Option<FeedEvent> {
    // ... existing parsing ...

    let mut is_pump_trade = false;
    let mut is_graduation = false;
    let mut is_buy = true;

    if let Some(logs) = value.get("logs").and_then(|l| l.as_array()) {
        for log_entry in logs {
            if let Some(log_str) = log_entry.as_str() {
                if log_str.starts_with("Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke") {
                    is_pump_trade = true;
                }
                if log_str.contains("Instruction: Buy") { is_buy = true; }
                if log_str.contains("Instruction: Sell") { is_buy = false; }

                // NEW: Detect graduation (Raydium pool initialization)
                if log_str.contains(RAYDIUM_AMM_PROGRAM) || log_str.contains("initialize2") {
                    is_graduation = true;
                }
            }
        }
    }

    if !is_pump_trade { return None; }

    if is_graduation {
        return Some(FeedEvent::Migration {
            mint: [0u8; 32],  // Unknown from logsSubscribe
            ts_ms: now_ms,
            source: MigrationSource::HeliusLogs,
            sig: Some(sig),
        });
    }

    // Existing PreWarm emission for buy/sell trades
    Some(FeedEvent::PreWarm(PreWarmEvent { /* ... existing ... */ }))
}
```

### 2.5 Extended FeedEvent::Migration

```rust
// feeds/mod.rs — modify existing Migration variant:

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MigrationSource {
    HeliusLogs,    // From logsSubscribe — fast, no mint (needs getTransaction)
    CoreCast,      // From Bitquery stream 2 — slower, has mint
}

pub enum FeedEvent {
    // ... existing ...
    Migration {
        mint: [u8; 32],              // [0u8;32] if from Helius (needs resolution)
        ts_ms: u64,
        source: MigrationSource,
        sig: [u8; 64],               // Always populated (for dedup + getTransaction)
    },
    // ... existing ...
}
```

**Backward compat for CoreCast:** CoreCast's `parse_amm_migration()` currently emits `Migration { mint, ts_ms }`. Update to include `source: MigrationSource::CoreCast` and `sig: [0u8; 64]` (CoreCast doesn't provide full sig, use the 64-byte sig from the Bitquery payload's `Transaction.Signature` field — decode from base58).

### 2.6 Dedup Map

```rust
// arb/dedup.rs

use dashmap::DashMap;

/// Deduplicates migration events across sources (Helius, CoreCast).
/// Keyed by mint address. Helius events resolve mint via getTransaction
/// before reaching the dedup layer.
pub struct MigrationDedup {
    seen: DashMap<[u8; 32], (u64, MigrationSource)>,
}

impl MigrationDedup {
    pub fn new() -> Self {
        Self { seen: DashMap::with_capacity(256) }
    }

    /// Returns true if first detection. False if duplicate within TTL.
    pub fn try_insert(&self, mint: &[u8; 32], ts_ms: u64, source: MigrationSource) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.seen.entry(*mint) {
            Entry::Vacant(e) => { e.insert((ts_ms, source)); true }
            Entry::Occupied(e) => {
                let (first_ts, _) = *e.get();
                if ts_ms.saturating_sub(first_ts) > 10_000 {
                    drop(e);
                    self.seen.insert(*mint, (ts_ms, source));
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Evict entries older than 10s. Call every ~200 ticks from on_tick.
    pub fn evict_stale(&self, now_ms: u64) {
        self.seen.retain(|_, (ts, _)| now_ms.saturating_sub(*ts) < 10_000);
    }
}
```

---

## 3. GraduationArbEngine Design (Paper Mode)

### 3.1 Config Struct

```rust
// arb/graduation.rs

/// Parsed from canary.json mev.graduation_arb_* fields.
pub struct GradArbConfig {
    pub enabled: bool,
    pub max_sol: f64,             // Default: 0.30
    pub min_spread_pct: f64,      // Default: 3.0
    pub tp_pct: f64,              // Default: 0.03
    pub sl_pct: f64,              // Default: 0.02
    pub max_hold_ms: u64,         // Default: 5000
    pub jito_tip_sol: f64,        // Default: 0.003
    pub arb_timeout_ms: u64,      // Default: 200 (async pipeline budget)
}
```

Already exists in canary.json. Config parsing already works in `config.rs` — just needs to be routed to the new engine struct instead of EngineConfig flat fields.

### 3.2 Position Struct

```rust
#[derive(Debug, Clone)]
pub struct GradArbPosition {
    pub mint: [u8; 32],
    pub pool_address: [u8; 32],
    pub entry_ts_ms: u64,
    pub entry_price_sol: f64,          // Raydium opening price
    pub bc_terminal_price: f64,        // Bonding curve terminal price
    pub spread_pct: f64,
    pub size_sol: f64,
    pub detection_source: MigrationSource,
    pub detection_latency_ms: u64,
    pub peak_price: f64,
    pub trough_price: f64,
    pub price_feed_available: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum GradArbExitReason {
    TakeProfit,
    StopLoss,
    MaxHold,
    NoArbFound,
}
```

### 3.3 Engine Core

```rust
pub struct GraduationArbEngine {
    config: GradArbConfig,
    positions: Arc<DashMap<[u8; 32], GradArbPosition>>,
    dedup: Arc<MigrationDedup>,
    stats: Arc<GradArbStats>,
    closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>,
    rpc_client: Arc<reqwest::Client>,
    helius_rpc_url: String,
}
```

**Key methods:**

- `fn enabled(&self) -> bool` — check config flag
- `fn on_migration(&self, mint, ts_ms, source, sig)` — dedup + spawn async eval task
- `fn on_tick(&self, ts_ms: u64)` — check open positions for TP/SL/MaxHold exits
- `fn close_position(&self, pos, reason, ts_ms)` — compute paper PnL, send to logger

### 3.4 Async Evaluation Pipeline

```rust
// Spawned inside tokio::spawn with timeout:
tokio::time::timeout(Duration::from_millis(200), async {
    // 1. If mint==[0;32] (Helius): call getTransaction(sig) → extract mint + pool
    // 2. If mint known (CoreCast): derive pool PDA or extract from event
    // 3. Call getAccountInfo(pool_address) → parse Raydium AMM account → get reserves
    // 4. bc_terminal_price = 85.0 / 206_900_000.0  (fixed at graduation)
    // 5. ray_opening_price = pool_sol_reserves / pool_token_reserves
    // 6. spread_pct = |bc_price - ray_price| / bc_price * 100
    // 7. If spread >= min_spread_pct → return Some(GradArbPosition)
    // 8. Else return None
})
```

### 3.5 JSONL Schema

File: `data/graduation_paper_trades.jsonl`

One JSON line per closed grad arb position:

```json
{
  "mint": "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU",
  "engineVersion": "grad-v1",
  "detectionSource": "helius",
  "detectionLatencyMs": 82,
  "spreadPct": 4.7,
  "bcTerminalPrice": 0.000000410,
  "rayOpeningPrice": 0.000000391,
  "poolAddress": "3nMFwZXwY1s1M3sNPGrF...",
  "entryVSol": 85.0,
  "entrySizeSol": 0.30,
  "entryTimestampMs": 1711670400000,
  "exitTimestampMs": 1711670401240,
  "exitReason": "max_hold",
  "holdMs": 1240,
  "pnlSol": 0.000,
  "netPnlSol": -0.003,
  "jitoBundleSubmitted": false,
  "priceFeedAvailable": true,
  "configVersion": "grad-v0.30sol_5000ms",
  "dataVersion": 1,
  "is_paper": true,
  "recordedAt": 1711670401300
}
```

### 3.6 Raydium Pool Address Derivation

**Recommended approach: Option A — Extract from migration transaction accounts.**

Rationale:
- Option B (PDA derivation) requires knowing the OpenBook/Serum market ID as a seed, which is itself a PDA that depends on the pool creation order. Not deterministic from mint alone.
- Option C (`getProgramAccounts`) is too slow (~500ms).
- Option A is reliable: the migration tx's `initialize2` instruction contains the pool address as an explicit account. We already need `getTransaction` for Helius-sourced events (to resolve the mint), so extracting the pool address from the same response costs zero additional latency.

**Implementation:**

```rust
/// Extract mint and pool address from a graduation/migration transaction.
/// Uses Helius RPC `getTransaction` with `jsonParsed` encoding.
async fn resolve_pool_from_transaction(
    client: &reqwest::Client,
    rpc_url: &str,
    sig: &[u8; 64],
) -> Option<([u8; 32], [u8; 32])> {  // Returns (mint, pool_address)
    let sig_b58 = bs58::encode(sig).into_string();

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [sig_b58, {"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0}]
    });

    let resp = client.post(rpc_url)
        .json(&body)
        .timeout(std::time::Duration::from_millis(150))
        .send().await.ok()?
        .json::<serde_json::Value>().await.ok()?;

    let tx = resp.get("result")?;
    let account_keys = tx.pointer("/transaction/message/accountKeys")?
        .as_array()?;

    // The Raydium initialize2 instruction's account layout (AMM v4):
    // Account 0: SPL Token Program
    // Account 1: System Program
    // Account 2: Rent Sysvar
    // Account 3: AMM ID (pool address) ← THIS IS WHAT WE WANT
    // Account 4: AMM Authority
    // Account 5: AMM Open Orders
    // Account 6: AMM LP Mint
    // Account 7: Coin Mint (base token = pump.fun token) ← MINT
    // Account 8: PC Mint (quote token = SOL/WSOL)
    // ...
    //
    // Strategy: Find the inner instruction that invokes Raydium AMM program,
    // then extract accounts[3] (pool) and accounts[7] (mint).

    let inner_instructions = tx.pointer("/meta/innerInstructions")?
        .as_array()?;

    for inner_group in inner_instructions {
        if let Some(instructions) = inner_group.get("instructions").and_then(|i| i.as_array()) {
            for ix in instructions {
                let program_id = ix.get("programId").and_then(|p| p.as_str()).unwrap_or("");
                if program_id == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
                    if let Some(accounts) = ix.get("accounts").and_then(|a| a.as_array()) {
                        if accounts.len() >= 9 {
                            let pool_b58 = accounts[3].as_str()?;
                            let mint_b58 = accounts[7].as_str()?;
                            let pool = decode_bs58_32(pool_b58)?;
                            let mint = decode_bs58_32(mint_b58)?;
                            return Some((mint, pool));
                        }
                    }
                }
            }
        }
    }

    None
}
```

**For CoreCast-sourced events (mint known but no pool address):**

Use `getTransaction` on the signature from Bitquery's `Transaction.Signature` field. Same logic as above but we already have the mint — just need the pool address.

Alternatively, for CoreCast where we have the mint but NOT the pool address, use a lightweight `getAccountInfo` on the known bonding curve address to check `complete` status, then scan recent Raydium transactions. But the simpler approach is: CoreCast's stream 2 fires on Raydium trades, meaning the pool already exists. We can use `getProgramAccounts` with `memcmp` filter on the mint within the Raydium AMM program — this is slow (~200-500ms) but within our 200ms async budget if Helius RPC is fast. If it exceeds budget, the timeout fires and we log `arb_timeout`.

**Recommendation:** Always prefer extracting pool from the graduation transaction. For CoreCast events, attempt to get the sig from the Bitquery payload and use the same `resolve_pool_from_transaction` path. If no sig available, skip the arb (log `no_sig_available`).

### 3.7 Paper Trade Price Feed

**Phase 1 (paper mode):** No real-time Raydium price feed. Positions exit only via MaxHold. Set `price_feed_available: false` when pool reserves couldn't be fetched.

**Phase 2:** Subscribe to Raydium pool account via Helius `accountSubscribe`. Parse AMM account data on each update to get real-time reserves → derive price → run TP/SL logic. This requires one additional WebSocket subscription per open grad arb position (max 1-2 concurrent).

---

## 4. CoreCast Stream Cleanup

### 4.1 Current Streams

| ID | Query | Purpose | Status |
|----|-------|---------|--------|
| 1 | pump.fun DEX trades | Creator sell detection → force-exit | **KEEP** |
| 2 | Raydium AMM trades | Migration detection → force-exit + grad arb entry | **KEEP** |
| 3 | Token supply updates | LP removal / rug detection → force-exit | **KEEP** |
| 4 | pump.fun create instructions | Pre-warm creator_map | **DROP** |

### 4.2 Stream 4 Analysis

`FeedEvent::NewToken` from stream 4 is used for:
1. Pre-warming `creator_map` (mint → creator wallet) in `corecast.rs` `parse_new_token()`
2. Incrementing `hot_path.stats.new_tokens` counter
3. Writing to `shared_creator_map` in main.rs

**Redundancy check:** PumpPortal's `subscribeNewToken` already delivers `txType="create"` events with the creator wallet. The `pumpportal.rs` feed writes to `creator_map` on every create event. Bitquery's stream 4 occasionally fires before PumpPortal (by ~500ms-2s), but this pre-warming provides minimal value because:
- The backrunner doesn't enter positions until multiple buys accumulate (1-5s after creation minimum)
- By then, PumpPortal has always delivered the create event
- Creator sell detection only matters for tokens that have accumulated enough activity to trigger entry — PumpPortal will have populated creator_map long before then

**Decision: DROP stream 4.** Frees one Bitquery subscription slot (3 streams instead of 4, well within the 5-stream cap). Simplifies corecast.rs.

### 4.3 Implementation

In `corecast.rs`:
- Remove `SUB_ID_NEW_TOKEN`, `GQL_NEW_TOKEN`, `parse_new_token()`
- Remove stream 4 from the subscription array
- Remove stream 4 match arm from the read loop
- Remove `stats.new_tokens` field from `StreamStats`

In `main.rs`:
- Remove `FeedEvent::NewToken` match arm (the event type stays in `feeds/mod.rs` for potential future use, but nothing emits it)
- Remove the `shared_creator_map.write()` call in the NewToken handler
- Remove `hot_path.on_new_token()` call

In `hot_path.rs`:
- Mark `on_new_token()` and `stats.new_tokens` as `#[allow(dead_code)]` or remove entirely

In API `server.rs`:
- Remove `new_tokens_seen` from stats response (or keep as 0 for backward compat)

---

## 5. Rust/Tokio Architecture Specifics

### 5.1 Memory Layout

**BackrunEngine (HotPath) — unchanged:**
- `MintHistoryMap`: `hashbrown::HashMap<[u8;32], MintHistory>` (capacity 4096)
- `PositionManager`: `HashMap<[u8;32], OpenPosition>` + `Sender<ClosedPosition>`
- `excluded_mints`: `hashbrown::HashSet<[u8;32]>` (capacity 256)
- All on the main thread. No Arc, no Mutex. Single-threaded access.

**GraduationArbEngine — new:**
- `positions: Arc<DashMap<[u8; 32], GradArbPosition>>` — concurrent access from spawned tokio tasks
- `dedup: Arc<MigrationDedup>` — `DashMap<[u8;32], (u64, MigrationSource)>`
- `stats: Arc<GradArbStats>` — atomic counters, zero contention
- `closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>` — bounded(64)

**DashMap justification:** Grad arb positions are created in spawned tokio tasks (async RPC calls) and read/closed from the main thread's tick handler. DashMap provides lock-free concurrent read/write without blocking the main thread. Expected capacity: 1-3 concurrent positions (graduation events are infrequent, ~5-20 per hour).

### 5.2 Shared Resources

```rust
// Shared between both engines:
Arc<HealthMonitor>           // Read-only from hot path, written by feed events
Arc<RwLock<HashMap<...>>>    // creator_map — read by CoreCast, written by PumpPortal
Arc<reqwest::Client>         // HTTP client for RPC calls (grad arb only)

// NOT shared (engine-specific):
HotPath                      // BackrunEngine — main thread only, no Arc
GraduationArbEngine          // Owned by main, dispatches via &self methods
```

### 5.3 Graduation Arb Task Hygiene

```rust
// Each migration event spawns ONE tokio task:
tokio::spawn(async move {
    // Budget: 200ms total
    // Step 1: getTransaction RPC call (~50-100ms)
    // Step 2: getAccountInfo for pool reserves (~30-50ms)
    // Step 3: Spread calculation (~0.1ms)
    // Step 4: Position creation (~0.1ms)
    // If timeout fires: task is cancelled, stats.arb_timeouts incremented
});
```

- Tasks are fire-and-forget. No JoinHandle stored.
- Each task captures only `Arc` clones (cheap: pointer-sized).
- No task holds a lock across an await point.
- If the engine is disabled mid-flight, spawned tasks will still complete and insert positions, but the next tick will close them. This is fine for paper mode.

### 5.4 Backrun Strategy Tag Cleanup

**Current:** `paper_logger.rs` computes `strategyTag` as either `"backrun_golden"` or `"backrun_standard"` at log time based on buys_1s, hour_utc, and vsol range thresholds.

**Change:** Replace with a single `"backrun"` tag.

Remove from `PaperTradeLogger`:
- `golden_min_buys_1s`, `golden_min_hour_utc`, `golden_max_hour_utc`, `golden_min_vsol`, `golden_max_vsol` fields
- The `strategy_tag` computation block in `log()`
- The `golden_thresholds` parameter from `new()`

Replace `"strategyTag"` value with hardcoded `"backrun"` string.

In `main.rs`:
- Remove `_golden_min_buys_1s`, `_golden_min_vsol_sol`, `_golden_max_vsol_sol` captures
- Remove `logger_golden_thresholds` tuple construction
- Simplify `PaperTradeLogger::new()` call

This removes ~30 lines of dead complexity from the logger.

---

## 6. Implementation Plan

### Phase 1: Cleanup (can be parallelized)

| # | Task | Size | Depends On | Done Criteria |
|---|------|------|------------|---------------|
| 1 | **Drop strategyTag golden/standard split** | S | — | `paper_logger.rs`: all trades log `"strategyTag": "backrun"`. `PaperTradeLogger::new()` signature simplified. Golden threshold fields removed. Tests pass. |
| 2 | **Drop CoreCast stream 4** | S | — | `corecast.rs`: 3 subscriptions sent (IDs 1,2,3). `GQL_NEW_TOKEN` removed. `main.rs`: `FeedEvent::NewToken` handler removed. `hot_path.rs`: `on_new_token()` removed. Log line confirms "3 streams subscribed". `cargo test` passes. |
| 3 | **Add `MigrationSource` to `FeedEvent::Migration`** | S | — | `feeds/mod.rs`: `Migration` variant carries `source: MigrationSource` and `sig: [u8; 64]`. `corecast.rs`: sets `source: CoreCast`, extracts sig from Bitquery payload. `hot_path.rs` and `main.rs`: updated to destructure new fields (ignoring source/sig for backrun force-exit). Compiles, all tests pass. |
| 4 | **Add Helius graduation detection** | M | 3 | `helius.rs`: `parse_helius_log()` checks for Raydium AMM program ID in logs. On detection, emits `FeedEvent::Migration { mint: [0;32], source: HeliusLogs, sig }`. Log line: `"[helius] graduation detected, sig=..."`. Unit test: pass known graduation log lines → assert `Migration` event emitted. |

### Phase 2: GraduationArbEngine (sequential)

| # | Task | Size | Depends On | Done Criteria |
|---|------|------|------------|---------------|
| 5 | **Create `arb/dedup.rs`** | S | — | `MigrationDedup` struct with `try_insert()`, `evict_stale()`. Unit tests: insert returns true first time, false second time within TTL, true after TTL. |
| 6 | **Create `arb/graduation.rs` v2 — config + structs** | M | — | `GradArbConfig`, `GradArbPosition`, `GradArbExitReason`, `GradArbClosedPosition`, `GradArbStats` structs. `GraduationArbEngine::new()` constructor. Config loaded from existing `mev.graduation_arb_*` fields via `EngineConfig`. Compiles. |
| 7 | **Create `persistence/grad_arb_logger.rs`** | M | 6 | `GradArbPaperLogger` struct. Opens `data/graduation_paper_trades.jsonl` in append mode. `log()` method writes one JSON line per `GradArbClosedPosition`. Schema matches Section 3.5. Test: create logger, log one entry, verify JSON line. |
| 8 | **Implement `resolve_pool_from_transaction()`** | M | 3 | Async function in `arb/graduation.rs`. Takes sig, calls Helius `getTransaction`, extracts mint + pool address from Raydium `initialize2` inner instruction accounts. Integration test with a known graduation tx signature. |
| 9 | **Implement `GraduationArbEngine::on_migration()`** | L | 5, 6, 8 | Full async pipeline: dedup → spawn task → timeout → resolve pool → fetch reserves → calc spread → paper position or skip. Log lines visible for each step. Test with mock RPC returning known pool data. |
| 10 | **Implement `GraduationArbEngine::on_tick()`** | M | 6, 9 | Iterates open positions, checks MaxHold exit. Calls `close_position()` → sends to logger channel. Position removed from DashMap. Log line: `"[grad_arb] paper position closed, reason=max_hold"`. |
| 11 | **Wire GraduationArbEngine into main.rs** | M | 9, 10, 7 | Engine constructed in main. Migration events dispatched to both engines. Tick events dispatched to grad arb engine. Closed positions drained to logger. Grad arb logger thread spawned (separate from backrun logger). `cargo run` shows `"[grad_arb] engine initialized"` at startup. |

### Phase 3: API + Stats Integration

| # | Task | Size | Depends On | Done Criteria |
|---|------|------|------------|---------------|
| 12 | **Update `/api/stats` for dual-engine stats** | M | 11 | API returns `backrun` and `graduation_arb` sections. `GradArbStats` synced to API state. `curl localhost:9421/api/stats` shows both sections. |
| 13 | **Rename JSONL file: `mev_paper_trades.jsonl` → `backrun_paper_trades.jsonl`** | S | 1 | Config default updated. Existing data not migrated (new file going forward). Log file path updated in canary.json. |

### Phase 4: Config Restructure (optional, low priority)

| # | Task | Size | Depends On | Done Criteria |
|---|------|------|------------|---------------|
| 14 | **Restructure canary.json: `mev.backrun` + `mev.graduation_arb` sections** | M | 11 | Current flat `mev.*` fields moved into `mev.backrun.*`. Graduation arb fields stay at `mev.graduation_arb.*`. Config loader updated. Backward compat: still reads flat fields if nested ones absent. |

### Dependency Graph

```
Phase 1 (parallel):  [1] [2] [3]
                              │
Phase 2 (sequential):    [4]──┤
                         [5]  │
                         [6]──┼──[9]──[11]
                         [7]  │        │
                         [8]──┘   [10]─┘
                                       │
Phase 3:                          [12]─┤
                              [13]─────┘
Phase 4 (optional):           [14]
```

**Critical path:** 3 → 4 → 8 → 9 → 11 → 12

**Estimated total:** 2-3 engineering days (one person) or 1-2 days (two people parallelizing Phase 1 + Phase 2 scaffolding).

---

## 7. Files to Create / Modify

### NEW Files

| File | Purpose |
|------|---------|
| `src/arb/dedup.rs` | Migration event dedup map (`MigrationDedup`) |
| `src/persistence/grad_arb_logger.rs` | JSONL logger for graduation arb paper trades |

### MODIFIED Files

| File | Changes |
|------|---------|
| `src/feeds/mod.rs` | Add `MigrationSource` enum. Extend `Migration` variant with `source` and `sig` fields. |
| `src/feeds/helius.rs` | Add graduation detection in `parse_helius_log()`: check for Raydium AMM program ID in log lines. Emit `FeedEvent::Migration` for detected graduations. |
| `src/feeds/corecast.rs` | **Remove** stream 4 (new token): delete `SUB_ID_NEW_TOKEN`, `GQL_NEW_TOKEN`, `parse_new_token()`. Update subscription array to 3 streams. Update `parse_amm_migration()` to include `source: MigrationSource::CoreCast` and extract sig from Bitquery `Transaction.Signature`. Remove `stats.new_tokens` from `StreamStats`. |
| `src/arb/graduation.rs` | **Major rewrite:** Replace stub with full `GraduationArbEngine` struct, `GradArbConfig`, `GradArbPosition`, `GradArbExitReason`, `GradArbClosedPosition`, `GradArbStats`. Implement `on_migration()` (async), `on_tick()`, `close_position()`, `evaluate_arb()`, `resolve_pool_from_transaction()`. |
| `src/arb/mod.rs` | Update re-exports for new types. Add `pub mod dedup;`. |
| `src/engine/config.rs` | Extract `GradArbConfig` struct construction from `EngineConfig` fields (the fields already exist, just need to be bundled into the new struct). Add `arb_timeout_ms` field (default: 200). |
| `src/engine/hot_path.rs` | Remove `on_new_token()` method and `stats.new_tokens` field. Update `on_migration()` to accept new `FeedEvent::Migration` fields (ignore `source`/`sig`). |
| `src/persistence/paper_logger.rs` | Remove golden segment fields (`golden_min_buys_1s`, `golden_min_hour_utc`, `golden_max_hour_utc`, `golden_min_vsol`, `golden_max_vsol`). Remove `strategy_tag` computation block. Hardcode `"strategyTag": "backrun"`. Simplify `new()` constructor signature. |
| `src/persistence/mod.rs` | Add `pub mod grad_arb_logger;`. |
| `src/main.rs` | (1) Remove golden threshold captures and `logger_golden_thresholds` tuple. (2) Simplify `PaperTradeLogger::new()` call. (3) Remove `FeedEvent::NewToken` handler. (4) Construct `GraduationArbEngine` at startup. (5) Dispatch `Migration` events to both engines. (6) Dispatch `Tick` events to grad arb engine. (7) Spawn grad arb logger thread. (8) Update `sync_stats_to_api()` for dual-engine stats. |
| `src/api/server.rs` | Restructure `EngineStats` into `BackrunStats` + `GradArbStats` (or keep `EngineStats` and add a separate `GradArbApiStats`). Update `/api/stats` handler to return dual-section response. Remove `new_tokens_seen` field (or keep at 0). |
| `config/canary.json` | Rename `log_file` from `data/mev_paper_trades.jsonl` to `data/backrun_paper_trades.jsonl`. Add `graduation_arb_timeout_ms: 200` field. No other config changes needed (graduation arb fields already exist). |

### DELETED / Simplified

| File | Action |
|------|--------|
| No files deleted | Stream 4 code removed from `corecast.rs`, golden segment logic removed from `paper_logger.rs`, but files themselves remain. |

### Unchanged Files (explicitly)

| File | Why |
|------|-----|
| `src/feeds/pumpportal.rs` | No changes needed. Already writes to creator_map. |
| `src/feeds/shredstream.rs` | No changes needed. |
| `src/feeds/event_joiner.rs` | No changes needed. Migration events from CoreCast bypass the joiner (direct to engine_tx). Helius migration events flow through the joiner as `FeedEvent::Migration`. |
| `src/engine/gates.rs` | No changes. |
| `src/engine/positions.rs` | No changes. BackrunEngine exit logic unchanged. |
| `src/engine/scorer.rs` | No changes. |
| `src/engine/bonding_curve.rs` | No changes. |
| `src/tx/*.rs` | No changes for paper mode. Future live mode will need `JitoClient` wiring. |

---

## 8. PumpSwap Consideration

**Important note for 2025+:** Since March 2025, pump.fun introduced PumpSwap as their own DEX. Tokens now migrate to PumpSwap instead of Raydium. This means:

1. **CoreCast stream 2 (Raydium AMM trades) may stop firing** for new pump.fun graduations if all tokens go to PumpSwap instead of Raydium.
2. **The graduation arb opportunity changes**: instead of BC → Raydium spread, it's BC → PumpSwap spread.
3. **Helius detection still works**: the `logsSubscribe` on the pump.fun program will see the PumpSwap pool creation instruction instead of Raydium's `initialize2`.

**Recommendation:** Design the GraduationArbEngine to be pool-type agnostic. The `MigrationSource` enum and pool resolution logic should support both Raydium and PumpSwap pool types. For Phase 1 (paper mode), detect both and log which pool type was created. This gives us data to decide which arb path is more viable.

Add to config:
```json
"graduation_arb_pool_types": ["raydium", "pumpswap"]
```

Add to JSONL schema:
```json
"poolType": "raydium|pumpswap"
```

This doesn't change the implementation plan — just adds a `pool_type` field to the position struct and logger. The detection logic in Helius needs to check for both Raydium AMM program and PumpSwap program in the log lines.

---

## 9. Risk & Open Questions

1. **Helius logsSubscribe → getTransaction latency:** The 50-100ms added by `getTransaction` may consume most of the ~50ms advantage over Bitquery. Monitor `detection_latency_ms` in paper trades to validate.

2. **Raydium AMM account layout parsing:** The `getAccountInfo` response for a Raydium AMM pool account returns raw bytes. Need to parse the `AmmInfo` struct at known offsets to extract `pool_coin_amount` (base reserve) and `pool_pc_amount` (quote reserve). Offsets: coin_vault at +72, pc_vault at +104 (verify against Raydium SDK source).

3. **Graduation frequency:** pump.fun graduates ~5-20 tokens per hour. At this rate, the async pipeline is not contention-sensitive. DashMap with 256 capacity is more than sufficient.

4. **PumpSwap vs Raydium split:** Need to monitor which pool type is being used for new graduations. If PumpSwap dominates, CoreCast stream 2 becomes useless for graduation detection and Helius becomes the sole source.

5. **BC terminal price precision:** The "85 SOL / 206.9M tokens" approximation may not be precise enough. Better: read the actual `vSol` and `vTokens` from the bonding curve account before graduation. If the bonding curve is already closed, use the last known values from PumpPortal trade data (available in `MintHistory`).

---

*This document is the implementation blueprint. Each task in Section 6 is designed to be handed to an engineering agent with unambiguous acceptance criteria. Start with Phase 1 (tasks 1-3, parallel), then Phase 2 (tasks 4-11, critical path), then Phase 3 (tasks 12-13).*