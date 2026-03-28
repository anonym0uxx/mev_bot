# Rust Rewrite Build Plan — Pump.fun MEV Backrun Engine

**Author:** Architecture review based on full codebase analysis  
**Date:** 2026-03-28  
**Scope:** Complete rewrite of hot-path TypeScript → Rust for maximum latency reduction  
**Target:** Single binary, zero-GC, sub-100µs signal-to-decision pipeline

---

## Table of Contents

1. [Architecture Decision: Monolith](#1-architecture-decision-monolith)
2. [Critical Hot Path Components](#2-critical-hot-path-components)
3. [Memory Layout](#3-memory-layout)
4. [Timing Precision](#4-timing-precision)
5. [Feed Integration](#5-feed-integration)
6. [Concurrency Model](#6-concurrency-model)
7. [Expected Latency Improvements](#7-expected-latency-improvements)
8. [What to Keep in TypeScript](#8-what-to-keep-in-typescript)
9. [Build Plan — Ordered by Impact](#9-build-plan--ordered-by-impact)
10. [Crate Selection](#10-crate-selection)
11. [Risk Analysis](#11-risk-analysis)

---

## 1. Architecture Decision: Monolith

**Decision: Single binary, single process, multi-threaded tokio runtime.**

### Rationale

1. **IPC latency kills you.** Even Unix domain sockets add ~2-5µs per message. With a 100-400ms pump-dump window, every microsecond of IPC between "signal detected" and "bundle submitted" is wasted. Shared-memory IPC (e.g. `mmap` + futex) is possible but adds complexity equivalent to just running the same code in-process.

2. **The data is small.** Max 10 concurrent positions, ~500 active mints in ring buffer memory, ~10k dedup entries. This fits in L2 cache. No reason to scatter it across processes.

3. **The concurrency model is simple.** 3-4 WS feed tasks + 1 gRPC task funnel into 1 signal engine core. The signal engine is inherently sequential per-event (gate stack is a linear pipeline). Position monitoring is per-position, max 10. This is a single-runtime workload.

4. **Fault isolation is handled by restarts, not process separation.** The existing `run-daemon.sh` supervisor pattern works. A Rust panic in any subsystem → process restart. The 3-hour daily trading window means you lose at most seconds, not state.

### Binary Structure

```
pump-quant-engine (single binary)
├── main() → tokio::main multi-thread runtime
├── feed/ → WS + gRPC async tasks (tokio::spawn)
├── engine/ → signal detection, scoring (called synchronously from feed dispatch)
├── position/ → position state, hold monitoring, momentum decay
├── execution/ → Jito bundle builder, sell executor
├── api/ → HTTP health/control (axum on separate port)
└── persistence/ → JSONL logger, SQLite (background flusher)
```

### What About the HTTP API?

The existing Express.js health API on `:9420` becomes an `axum` server running on its own tokio task. It reads shared state via `Arc<AtomicU64>` counters and `Arc<RwLock<Stats>>` for aggregated stats. The health API is never on the hot path — reads happen at most once per second from a human or monitoring system.

---

## 2. Critical Hot Path Components

Priority order by latency impact (highest first):

### 2A. Feed Parsers — WebSocket JSON Deserialization

**Current:** `JSON.parse(data.toString())` on Node.js — 0.5-2ms per message, subject to GC pauses.

**Rust design:**

```rust
// PumpPortal message — only the fields we need, skip the rest
#[derive(Deserialize)]
struct PumpPortalTrade {
    signature: CompactString,        // ~88 bytes base58, avoid String alloc
    mint: CompactString,             // base58 pubkey
    #[serde(rename = "traderPublicKey")]
    trader_public_key: CompactString,
    #[serde(rename = "txType")]
    tx_type: TxType,                 // enum: buy/sell/create
    #[serde(rename = "solAmount")]
    sol_amount: f64,
    #[serde(rename = "vSolInBondingCurve")]
    v_sol_in_bonding_curve: f64,
    #[serde(rename = "vTokensInBondingCurve")]
    v_tokens_in_bonding_curve: f64,
    #[serde(rename = "bondingCurveKey")]
    bonding_curve_key: CompactString,
    #[serde(rename = "marketCapSol")]
    market_cap_sol: f64,
    #[serde(rename = "tokenAmount")]
    token_amount: f64,
}

#[derive(Deserialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TxType { Buy, Sell, Create }
```

**Crate:** `simd-json` v0.14 — uses SIMD (AVX2/SSE4.2) for JSON validation and structure detection. Falls back to `serde_json` on non-x86 but this runs on x86-64 VPS.

**Why `simd-json` over `serde_json`:** PumpPortal messages are ~500-800 bytes of JSON. `simd-json` processes the structural characters (braces, quotes, colons) 16 bytes at a time on AVX2. For a 600-byte message, that's ~37 SIMD iterations vs ~600 byte-by-byte comparisons. Benchmark expectation: **50-150µs per parse** vs `serde_json`'s 100-300µs.

**Zero-copy trick:** `simd-json` operates on `&mut [u8]` in-place. The WS frame arrives as `Vec<u8>` from `tokio-tungstenite`. We pass it directly to `simd_json::serde::from_slice_mut()` — no intermediate `String` allocation, no UTF-8 revalidation.

**Helius parsing:** Helius transactionNotification payloads are 2-10KB (full transaction JSON). We use `simd-json` with a **minimal extraction struct** — deserialize only the fields we need (signature, accountKeys[0], preBalances, postBalances, preTokenBalances, postTokenBalances). The rest is `#[serde(skip)]`.

```rust
#[derive(Deserialize)]
struct HeliusNotification {
    params: HeliusParams,
}

#[derive(Deserialize)]
struct HeliusParams {
    result: HeliusResult,
}

#[derive(Deserialize)]
struct HeliusResult {
    signature: CompactString,
    transaction: HeliusTxOuter,
}

#[derive(Deserialize)]
struct HeliusTxOuter {
    meta: HeliusMeta,
    transaction: HeliusTxInner,
}

#[derive(Deserialize)]
struct HeliusMeta {
    err: Option<serde_json::Value>,  // null = success
    #[serde(rename = "preBalances")]
    pre_balances: Vec<u64>,
    #[serde(rename = "postBalances")]
    post_balances: Vec<u64>,
    #[serde(rename = "preTokenBalances")]
    pre_token_balances: Vec<TokenBalance>,
    #[serde(rename = "postTokenBalances")]
    post_token_balances: Vec<TokenBalance>,
}
```

### 2B. Ring Buffer — Per-Mint Sliding Window Trade History

**Current:** `Map<string, { trades: TradeRecord[] }>` — dynamic array, GC-managed, `.filter()` on every gate check reconstructs arrays.

**Rust design: Fixed-capacity circular buffer, arena-allocated, O(1) insertion.**

```rust
const RING_CAP: usize = 256; // Max trades per mint in 60s window (generous)
const MAX_ACTIVE_MINTS: usize = 2048; // Slab capacity

/// Compact trade record — 40 bytes, fits in cache line pair
#[repr(C, packed)]
#[derive(Copy, Clone)]
struct TradeRecord {
    ts_ms: u64,           // 8 bytes — epoch ms
    sol_amount: f64,      // 8 bytes — SOL
    v_sol: f64,           // 8 bytes — vSol at time of trade
    trader: [u8; 12],     // 12 bytes — first 12 bytes of pubkey (sufficient for uniqueness check)
    tx_type: u8,          // 1 byte — 0=buy, 1=sell
    _pad: [u8; 3],        // 3 bytes padding → 40 bytes total
}

/// Fixed-size ring buffer — no heap allocation after init
struct MintRingBuffer {
    trades: [TradeRecord; RING_CAP],
    head: u16,             // write position (wraps at RING_CAP)
    len: u16,              // current count (max RING_CAP)
    first_seen_ms: u64,
    last_updated_ms: u64,
    creator_sell_at: u64,  // 0 = no creator sell detected
}

impl MintRingBuffer {
    /// Push trade — O(1), no allocation, overwrites oldest if full
    #[inline(always)]
    fn push(&mut self, trade: TradeRecord) {
        let idx = self.head as usize;
        self.trades[idx] = trade;
        self.head = ((self.head + 1) % RING_CAP as u16);
        if self.len < RING_CAP as u16 {
            self.len += 1;
        }
        self.last_updated_ms = trade.ts_ms;
    }

    /// Iterate trades from oldest to newest (yields valid entries only)
    #[inline]
    fn iter(&self) -> RingIter<'_> {
        RingIter { buf: self, pos: 0 }
    }

    /// Count buys in last N ms — single pass, no allocation
    #[inline]
    fn count_buys_since(&self, cutoff_ms: u64) -> u32 {
        let mut count = 0u32;
        for i in 0..self.len as usize {
            let idx = (self.head as usize + RING_CAP - self.len as usize + i) % RING_CAP;
            let t = &self.trades[idx];
            if t.ts_ms >= cutoff_ms && t.tx_type == 0 {
                count += 1;
            }
        }
        count
    }
}
```

**Mint lookup:** Use a `HashMap<Pubkey32, u16>` mapping mint pubkey → slab index, where `Pubkey32` is `[u8; 32]`. The slab is a pre-allocated `Vec<MintRingBuffer>` of size `MAX_ACTIVE_MINTS`. This avoids per-mint heap allocation. The `HashMap` uses `FxHashMap` from `rustc-hash` (non-cryptographic, fastest for fixed-size keys).

```rust
type Pubkey32 = [u8; 32];

struct MintHistoryStore {
    /// Slab of pre-allocated ring buffers
    slabs: Vec<MintRingBuffer>,        // capacity = MAX_ACTIVE_MINTS
    /// Pubkey → slab index
    index: FxHashMap<Pubkey32, u16>,
    /// Free list for slab reuse
    free_list: Vec<u16>,
}
```

**Why 12 bytes for `trader` instead of 32?** The trader pubkey is used ONLY for uniqueness counting (unique buyers). The first 12 bytes give 2^96 collision space — probability of a false uniqueness match in a 60s window with ~200 trades is negligible (~10^-23). This saves 20 bytes per record × 256 records × 2048 mints = 10MB of cache pressure.

### 2C. Gate Stack — Sequential Boolean Evaluation

**Current:** 13 gates evaluated sequentially with early-return. Each gate does `.filter()` and `.reduce()` over the trade array, reconstructing temporary arrays.

**Rust design: Single-pass aggregation + gate evaluation.**

The key insight is that most gates need the **same aggregate statistics** computed from the ring buffer. Rather than running 13 separate `.filter()` passes, we do ONE scan of the ring buffer that computes all aggregates simultaneously, then evaluate gates against the pre-computed aggregates.

```rust
/// Aggregated stats from a single ring buffer scan
#[derive(Default)]
struct TradeAggregates {
    buy_count_1s: u32,
    buy_count_2s: u32,
    buy_count_5s: u32,
    sell_count_5s: u32,
    buy_vol_5s: f64,
    sell_vol_5s: f64,
    unique_buyers_30s: u32,          // counted via inline bitset
    total_buys_30s: u32,
    last_buy_ts: u64,
    oldest_vsol_3s: f64,
    total_buy_vol_30s: f64,
    // Per-trader accumulator for concentration check (top wallet only)
    max_wallet_vol_30s: f64,
    // Unique buyer tracking: use 256-bit bloom filter for speed
    buyer_bloom: [u64; 4],           // 256-bit bloom, ~1% false positive for <50 entries
    buyer_exact_count: u32,          // exact count via separate small hashset
}

/// Single-pass ring buffer scan — computes ALL gate inputs in one loop
#[inline]
fn compute_aggregates(
    ring: &MintRingBuffer,
    now_ms: u64,
    trigger_trader: &[u8; 12],
) -> TradeAggregates {
    let mut agg = TradeAggregates::default();

    let cutoff_1s = now_ms.saturating_sub(1_000);
    let cutoff_2s = now_ms.saturating_sub(2_000);
    let cutoff_3s = now_ms.saturating_sub(3_000);
    let cutoff_5s = now_ms.saturating_sub(5_000);
    let cutoff_30s = now_ms.saturating_sub(30_000);

    // Track unique buyers with a small inline hashset (stack-allocated)
    // Max ~200 trades in 60s → max ~100 unique traders
    let mut seen_traders: ArrayVec<[u8; 12], 128> = ArrayVec::new();
    // Per-trader volume for concentration check
    let mut trader_vols: ArrayVec<([u8; 12], f64), 128> = ArrayVec::new();

    let mut oldest_3s_vsol_set = false;

    for i in 0..ring.len as usize {
        let idx = (ring.head as usize + RING_CAP - ring.len as usize + i) % RING_CAP;
        let t = &ring.trades[idx];

        let is_buy = t.tx_type == 0;
        let age = now_ms - t.ts_ms;

        if age <= 60_000 {
            // Unique buyer tracking (all trades in window)
            if !seen_traders.contains(&t.trader) {
                if seen_traders.len() < 128 {
                    seen_traders.push(t.trader);
                }
            }

            if is_buy {
                if age < 1_000 { agg.buy_count_1s += 1; }
                if age < 2_000 { agg.buy_count_2s += 1; }
                if age < 5_000 {
                    agg.buy_count_5s += 1;
                    agg.buy_vol_5s += t.sol_amount;
                }
                if age < 30_000 {
                    agg.total_buys_30s += 1;
                    agg.total_buy_vol_30s += t.sol_amount;

                    // Per-trader volume tracking
                    if let Some(entry) = trader_vols.iter_mut().find(|(k, _)| *k == t.trader) {
                        entry.1 += t.sol_amount;
                    } else if trader_vols.len() < 128 {
                        trader_vols.push((t.trader, t.sol_amount));
                    }
                }
                agg.last_buy_ts = agg.last_buy_ts.max(t.ts_ms);
            } else {
                if age < 5_000 {
                    agg.sell_count_5s += 1;
                    agg.sell_vol_5s += t.sol_amount;
                }
            }

            if age < 3_000 && t.v_sol > 0.0 && !oldest_3s_vsol_set {
                agg.oldest_vsol_3s = t.v_sol;
                oldest_3s_vsol_set = true;
            }
        }
    }

    agg.unique_buyers_30s = seen_traders.len() as u32;
    agg.buyer_exact_count = seen_traders.len() as u32;
    agg.max_wallet_vol_30s = trader_vols.iter().map(|(_, v)| *v).fold(0.0f64, f64::max);

    agg
}

/// Gate evaluation — all branches are branchless-friendly comparisons
#[inline]
fn evaluate_gates(
    event: &InternalTradeEvent,
    ring: &MintRingBuffer,
    agg: &TradeAggregates,
    cfg: &MevConfigCompact,
    now_ms: u64,
) -> Option<f64> {  // Returns score if all gates pass, None if rejected
    // Gate 1: must be buy
    if event.tx_type != TxType::Buy { return None; }

    // Gate 2: buy size range
    if event.sol_amount < cfg.trigger_min_buy_sol { return None; }
    if event.sol_amount > cfg.trigger_max_buy_sol { return None; }

    // Gate 3: vSol in range
    if event.v_sol < cfg.min_vsol || event.v_sol > cfg.max_vsol { return None; }

    // Gate 4: token age
    let age_s = (now_ms - ring.first_seen_ms) / 1000;
    if age_s > cfg.max_token_age_s as u64 { return None; }

    // Gate 5: unique buyers
    if agg.unique_buyers_30s < cfg.min_unique_buyers { return None; }

    // Gate 5b: large trigger concentration
    if event.sol_amount > 1.5 && agg.unique_buyers_30s < 5 { return None; }

    // Gate 6: pre-trigger momentum (all from pre-computed aggregates)
    let gap_ms = if agg.last_buy_ts > 0 { now_ms - agg.last_buy_ts } else { u64::MAX };
    if gap_ms > cfg.pre_trigger_max_gap_ms as u64 { return None; }

    if event.sol_amount < 0.5 {
        // Pre-trigger crowd gates use buy counts EXCLUDING trigger
        // (aggregates are computed from all trades before trigger was pushed)
        if agg.buy_count_2s < cfg.pre_trigger_min_buys_2s { return None; }
        if agg.buy_count_5s < cfg.pre_trigger_min_buys_5s { return None; }
    }

    let v_sol_delta_3s = (event.v_sol - agg.oldest_vsol_3s).max(0.0);
    if v_sol_delta_3s < cfg.pre_trigger_min_vsol_accel { return None; }

    if agg.buy_count_1s < cfg.pre_trigger_min_buys_1s { return None; }

    // Gate 6e: sell count
    if agg.sell_count_5s < cfg.pre_trigger_min_sell_count_5s { return None; }

    // Gate 6f: vSol delta cap
    if v_sol_delta_3s > cfg.pre_trigger_max_vsol_delta_3s { return None; }

    // Gate 6b: creator sell (30s TTL)
    if ring.creator_sell_at > 0 && (now_ms - ring.creator_sell_at) < 30_000 {
        return None;
    }

    // Gate 6c: net flow ratio
    let total_vol_5s = agg.buy_vol_5s + agg.sell_vol_5s;
    let net_flow_ratio = if total_vol_5s > 0.0 {
        (agg.buy_vol_5s - agg.sell_vol_5s) / total_vol_5s
    } else { 1.0 };
    if net_flow_ratio < 0.2 { return None; }

    // Gate 6d: trigger isolation
    let isolation = event.sol_amount / (agg.buy_vol_5s + event.sol_amount);
    if isolation > cfg.max_trigger_isolation { return None; }

    // Compute score (all f64 arithmetic, zero allocation)
    let score = compute_score_v5(event, agg, cfg, v_sol_delta_3s);

    // Gate 7: score threshold
    if score < cfg.trigger_min_score { return None; }

    Some(score)
}
```

### 2D. Score Engine — Float Arithmetic on Aggregates

**Current:** 6 weighted components + adversarial penalty. All f64. The TS version allocates Sets and arrays for diversity calculation.

**Rust design: Pure arithmetic, no allocations.**

```rust
#[inline]
fn compute_score_v5(
    event: &InternalTradeEvent,
    agg: &TradeAggregates,
    cfg: &MevConfigCompact,
    v_sol_delta_3s: f64,
) -> f64 {
    // 1. Momentum trend (10%)
    let older_1s = (agg.buy_count_2s - agg.buy_count_1s).max(1) as f64;
    let momentum_ratio = agg.buy_count_1s as f64 / older_1s;
    let momentum = ((momentum_ratio - 0.5) / 1.5).clamp(0.0, 1.0);

    // 2. Unique buyers banded (25%)
    let ub = agg.unique_buyers_30s;
    let buyer_score = if ub < 3 { 0.1 }
        else if ub <= 5 { 0.5 + (ub - 3) as f64 * 0.15 }
        else if ub <= 10 { 0.8 + (ub - 5) as f64 * 0.04 }
        else if ub <= 15 { 1.0 - (ub - 10) as f64 * 0.06 }
        else { 0.7 };

    // 3. Buyer diversity (10%)
    let unique_traders_30s = agg.buyer_exact_count as f64;
    let total_buys_30s = agg.total_buys_30s.max(1) as f64;
    let diversity = (unique_traders_30s / total_buys_30s * 1.5).min(1.0);

    // 4. Curve fill (20%)
    let range = cfg.max_vsol - cfg.min_vsol;
    let fill = (event.v_sol - cfg.min_vsol) / range;
    let curve_fill = (1.0 - fill).max(0.0);

    // 5. Crowd depth 5s (20%)
    let crowd_depth = (agg.buy_vol_5s / 5.0).min(1.0);

    // 6. Recent buyers 1s (15%)
    let recent_1s = (agg.buy_count_1s as f64 / 6.0).min(1.0);

    // Adversarial concentration penalty
    let concentration = if agg.total_buy_vol_30s > 0.0 {
        agg.max_wallet_vol_30s / agg.total_buy_vol_30s
    } else { 0.0 };
    let adversarial_penalty = if concentration > 0.6 { 0.5 } else { 1.0 };

    // Weighted sum
    let raw = momentum * 0.10
        + buyer_score * 0.25
        + diversity * 0.10
        + curve_fill * 0.20
        + crowd_depth * 0.20
        + recent_1s * 0.15;

    raw * adversarial_penalty
}
```

This is pure register arithmetic. On modern x86-64, this is ~20-50 CPU cycles. Estimated: **<100ns**.

### 2E. Position Monitor — Per-Position Tick Handler

**Current:** Called on every trade event for held mints. Does TP/SL checks, trailing stop, aggregate flow exit.

**Rust design:** Inline struct with all state, called from the main event dispatch loop.

```rust
const MAX_POSITIONS: usize = 16; // slightly above config's 10

#[repr(C)]
struct OpenPosition {
    mint: Pubkey32,                     // 32 bytes
    entry_v_sol: f64,                   // 8
    size_sol: f64,                      // 8
    entry_ts_ms: u64,                   // 8
    peak_v_sol: f64,                    // 8
    trough_v_sol: f64,                  // 8
    current_v_sol_lamports: u64,        // 8 (was bigint)
    current_v_tokens: u64,              // 8 (was bigint)
    flow_since_entry: f64,             // 8
    buys_since_entry: u32,             // 4
    trades_seen_after_entry: u32,      // 4
    trigger_sol: f64,                  // 8 — for tiered TP/SL
    trigger_sig: [u8; 64],             // 64 — raw signature bytes
    bonding_curve_key: Pubkey32,       // 32
    assoc_bonding_curve: Pubkey32,     // 32
    tokens_held: u64,                  // 8
    score: f64,                        // 8
    tp_pct: f64,                       // 8 — pre-computed from tier at open
    sl_pct: f64,                       // 8 — pre-computed from tier at open
    is_active: bool,                   // 1
    _pad: [u8; 7],                     // alignment
    // Opportunity snapshot (for PnL record)
    opportunity_snapshot: OpportunitySnapshot, // stored at open time
}
// Total: ~304 bytes — fits in 5 cache lines

struct PositionManager {
    positions: [OpenPosition; MAX_POSITIONS],
    active_count: u8,
    // Index: mint → position slot (for O(1) lookup on trade events)
    mint_to_slot: FxHashMap<Pubkey32, u8>,
}

impl PositionManager {
    #[inline]
    fn on_trade(&mut self, event: &InternalTradeEvent, now_ms: u64) -> Option<ExitSignal> {
        let slot = match self.mint_to_slot.get(&event.mint_bytes) {
            Some(&s) => s as usize,
            None => return None,
        };
        let pos = &mut self.positions[slot];
        if !pos.is_active { return None; }

        // Skip events with zero vSol (Helius fast lane)
        if event.v_sol <= 0.0 { return None; }

        // Update reserves
        pos.current_v_sol_lamports = (event.v_sol * 1e9) as u64;
        pos.current_v_tokens = (event.v_tokens * 1.0) as u64; // from event

        let current_v_sol = event.v_sol;

        // MFE/MAE tracking
        if current_v_sol > pos.peak_v_sol { pos.peak_v_sol = current_v_sol; }
        if current_v_sol < pos.trough_v_sol { pos.trough_v_sol = current_v_sol; }

        let pnl_pct = (current_v_sol - pos.entry_v_sol) / pos.entry_v_sol;

        // Aggregate flow
        if event.tx_type == TxType::Buy {
            pos.flow_since_entry += event.sol_amount;
            pos.buys_since_entry += 1;
        }

        // Intra-hold trailing stop
        let mfe_from_entry = (pos.peak_v_sol - pos.entry_v_sol) / pos.entry_v_sol;
        let drop_from_peak = (pos.peak_v_sol - current_v_sol) / pos.peak_v_sol;
        if mfe_from_entry >= 0.01 && drop_from_peak >= 0.025 {
            return Some(ExitSignal::IntraHoldTrail(current_v_sol));
        }

        // TP/SL
        if pnl_pct >= pos.tp_pct {
            return Some(ExitSignal::TakeProfit(current_v_sol));
        }
        if pnl_pct <= -pos.sl_pct {
            return Some(ExitSignal::StopLoss(current_v_sol));
        }

        // Skip trigger event
        // (signature check: compare first 8 bytes for speed)
        if event.sig_prefix == pos.trigger_sig[..8] { return None; }

        pos.trades_seen_after_entry += 1;
        let hold_ms = now_ms - pos.entry_ts_ms;
        if pos.trades_seen_after_entry < 2 || hold_ms < 500 { return None; }

        // Early profit exit
        if pnl_pct >= 0.015 && hold_ms >= 200 {
            return Some(ExitSignal::NextBuyer(current_v_sol));
        }

        // Aggregate next buyer exit
        if event.tx_type == TxType::Buy {
            let flow_ratio = 0.5;
            let count_threshold = 5;
            let single_threshold = pos.trigger_sol * 0.25;

            if pos.flow_since_entry >= pos.trigger_sol * flow_ratio
                || pos.buys_since_entry >= count_threshold
                || event.sol_amount >= single_threshold {
                return Some(ExitSignal::NextBuyer(current_v_sol));
            }
        }

        None
    }
}
```

### 2F. Momentum Decay — Recurring 50ms Check

**Current:** `setInterval(fn, 50)` with ±15ms jitter from Node.js event loop imprecision.

**Rust design:** Dedicated tokio task with `tokio::time::interval` for each open position — but improved.

The critical insight: momentum decay needs to check `current_v_sol_lamports` which is updated by the trade event handler. In the single-threaded model, the decay checker and the trade handler run on the same thread. We use a **single