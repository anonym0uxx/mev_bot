# LATENCY_REVIEW.md — pump-quant v5-rust Latency Audit

_Produced by principal Solana MEV / Rust architect. All citations reference actual code._

---

## 1. CRITICAL PATH LATENCY BUDGET

### Current estimated end-to-end latency: ~150–220ms

```
Stage                                     Current         Theoretical Min   Delta
─────────────────────────────────────────────────────────────────────────────────
1. Network: trade on-chain → PumpPortal   ~80–130ms       ~20ms (ShredStream) ~80ms
2. PumpPortal WS frame → parsed           ~0.5–2ms        ~0.1ms              ~1ms
3. Channel hop: PumpPortal → hot_path     ~0.3–1ms        ~0.05ms             ~0.5ms
4. hot_path.on_trade() total              ~2–8μs          ~0.5–1μs            ~5μs
   a. now_ms() clock                      ~5–8ns          ~3ns                ~3ns
   b. mint_map get_or_insert              ~30–80ns        ~20ns               ~30ns
   c. mint_map second get()               ~25–60ns        0 (eliminate)       ~40ns
   d. excluded_mints.contains()           ~15–40ns        ~5ns (bloom)        ~20ns
   e. regime/graduation check             ~10–20ns        ~2ns (u64 cmp)      ~15ns
   f. Gate stack evaluate() 18 gates      ~50–150ns       ~20–40ns            ~80ns
      - blocked_hours Vec scan            ~8–20ns         ~1ns (bitmask)      ~15ns
      - MaxCurveProgress float math       ~10–20ns        ~2ns (u64 cmp)      ~15ns
      - ratio gate f64 division           ~8–15ns         ~2ns (int multiply) ~10ns
   g. scorer.compute() 6 components       ~80–200ns       ~40–80ns            ~80ns
   h. position_manager.open_position()    ~200–500ns      ~100–200ns          ~200ns
5. TX build (builder.rs)                  ~5–20μs         ~1–2μs              ~15μs
6. Jito bundle HTTP submit                ~50–150ms       ~10–30ms (gRPC)     ~60ms
─────────────────────────────────────────────────────────────────────────────────
TOTAL                                     ~150–220ms      ~30–55ms            ~120ms
```

### Top 3 bottlenecks by impact:

1. **Network delivery lag** (~80ms): PumpPortal relays events after they land on-chain. ShredStream gets shreds before finalization — 20–30ms total latency. **+80ms savings.**
2. **Jito HTTP vs gRPC** (~60–100ms): REST bundle submission adds round-trip overhead. Persistent gRPC stream to block engine cuts this to ~10–30ms. **+40–70ms savings.**  
3. **Hot path micro-waste** (~200–400ns per trade): Individually small, but at 1000+ trades/sec = 200–400μs/s of wasted CPU = cache thrashing + branch mispredicts compounding. Listed below.

---

## 2. HOT PATH MICRO-OPTIMIZATIONS

### A. `now_ms()` — hot_path.rs

**Current code:**
```rust
fn now_ms(&self) -> u64 {
    let elapsed = self.clock.now().duration_since(self.start_instant);
    self.start_epoch_ms + elapsed.as_millis() as u64
}
```

**Problem:** `duration_since` does a signed subtraction with overflow check. `.as_millis()` does an integer division by 1,000,000 (compiler may optimize with reciprocal multiply, but not guaranteed).

**Fix:**
```rust
// In HotPath struct, add:
start_raw: u64,           // raw RDTSC ticks at startup
ns_per_tick_recip: f64,   // precomputed: 1e6 / ticks_per_ms

// In HotPath::new():
let start_raw = clock.raw();
let ns_per_tick_recip = 1_000_000.0 / clock.hz() as f64; // ms per tick * 1e6

// now_ms():
#[inline(always)]
fn now_ms(&self) -> u64 {
    let ticks = self.clock.raw().wrapping_sub(self.start_raw);
    self.start_epoch_ms + (ticks as f64 * self.ns_per_tick_recip) as u64
}
```
**Savings: ~3–5ns per call.** Called on every trade + every tick = significant at high throughput.

---

### B. Hour bitmasks — gates.rs GateConfig + GateStack::evaluate()

**Current code (Gate 0a):**
```rust
if c.tod_gate_enabled && !c.blocked_hours_utc.is_empty() {
    let hour_utc = ((now_ms / 3_600_000) % 24) as u8;
    if c.blocked_hours_utc.contains(&hour_utc) {
        return Err(GateRejectReason::BlockedHour);
    }
}
```

**Problem:** `Vec<u8>::contains` = linear scan, up to 24 comparisons = ~8–20ns.

**Fix — GateConfig:**
```rust
// Replace Vec<u8> fields with bitmasks:
pub blocked_hours_bitmask: u32,   // bit N set = hour N blocked
pub boosted_hours_bitmask: u32,   // bit N set = hour N boosted

// GateStack::new() precompute:
let blocked_hours_bitmask = config.blocked_hours_utc
    .iter().fold(0u32, |acc, &h| acc | (1u32 << h));
let boosted_hours_bitmask = config.boosted_hours_utc
    .iter().fold(0u32, |acc, &h| acc | (1u32 << h));
```

**Fix — evaluate() Gate 0a:**
```rust
if c.tod_gate_enabled && c.blocked_hours_bitmask != 0 {
    let hour_utc = ((now_ms / 3_600_000) % 24) as u32;
    if (c.blocked_hours_bitmask >> hour_utc) & 1 == 1 {
        return Err(GateRejectReason::BlockedHour);
    }
}
```

**Fix — HotPath::get_tod_multiplier():**
```rust
#[inline(always)]
fn get_tod_multiplier(&self, hour_utc: u8) -> f64 {
    if (self.boosted_hours_bitmask >> hour_utc) & 1 == 1 {
        self.tod_boost_multiplier
    } else {
        1.0
    }
}
// Add boosted_hours_bitmask: u32 field to HotPath, precompute in new()
```
**Savings: ~15–20ns per trade.** Called every buy event.

---

### C. Gate 3b `MaxCurveProgress` — eliminate float math

**Current code (gates.rs Gate 3b):**
```rust
if c.max_curve_progress < 1.0 && event.vtoken_reserves > 0 {
    let progress = crate::engine::regime::compute_bonding_curve_progress(
        event.vtoken_reserves,
        crate::engine::regime::INITIAL_VIRTUAL_TOKENS,
    );
    if progress > c.max_curve_progress {
        return Err(GateRejectReason::MaxCurveProgress);
    }
}
```

**Problem:** `compute_bonding_curve_progress` does float division on every trade that reaches Gate 3b. With 11.6 events/s throughput this fires frequently.

**Fix — precompute threshold in GateStack::new():**
```rust
// GateConfig: add field
pub max_vtoken_threshold: u64,  // precomputed, replaces max_curve_progress float

// GateStack::new():
// INITIAL_VIRTUAL_TOKENS = 1_073_000_000_000_000u64
// progress = (INITIAL - vtoken) / INITIAL
// progress > max_curve_progress ↔ vtoken < INITIAL * (1 - max_curve_progress)
let max_vtoken_threshold = if config.max_curve_progress < 1.0 {
    (regime::INITIAL_VIRTUAL_TOKENS as f64 * (1.0 - config.max_curve_progress)) as u64
} else {
    0 // disabled
};

// Gate 3b in evaluate():
if self.config.max_vtoken_threshold > 0 
    && event.vtoken_reserves < self.config.max_vtoken_threshold 
{
    return Err(GateRejectReason::MaxCurveProgress);
}
```
**Savings: ~10–20ns per trade.** Float division → single u64 comparison.

---

### D. Gate 14b `min_buy_sell_ratio_5s` — eliminate f64 division

**Current code (gates.rs Gate 14b):**
```rust
if c.min_buy_sell_ratio_5s > 0.0 && sell_count_5s > 0 {
    let ratio = buy_count_5s as f64 / sell_count_5s as f64;
    if ratio < c.min_buy_sell_ratio_5s {
        return Err(GateRejectReason::SellPressure);
    }
}
```

**Fix — GateConfig: add integer field:**
```rust
pub min_buy_sell_ratio_x10: u16,  // ratio * 10 as integer (2.5 → 25)
// Keep min_buy_sell_ratio_5s: f64 for JSON config parsing,
// but precompute min_buy_sell_ratio_x10 in GateStack::new()
```

**Fix — Gate 14b:**
```rust
if c.min_buy_sell_ratio_x10 > 0 && sell_count_5s > 0 {
    // buy/sell >= ratio ↔ buy * 10 >= ratio_x10 * sell (no division)
    if (buy_count_5s as u32) * 10 < (c.min_buy_sell_ratio_x10 as u32) * (sell_count_5s as u32) {
        return Err(GateRejectReason::SellPressure);
    }
}
```
**Savings: ~8–15ns per trade.** f64 int conversion + division → 2 integer multiplies.

---

### E. MintHistoryMap double-lookup — hot_path.rs

**Current code:**
```rust
// First lookup:
let history = self.mint_map.get_or_insert(&trade.mint, now);
history.push(record, now);

// Position check (uses position_manager, not mint_map) ...

// Second lookup (30+ lines later):
let history = self.mint_map.get(&trade.mint).unwrap();
let history_age_ms = now.saturating_sub(history.first_seen_ms);
let unique_buyers_30s = history.cached_unique_buyers_30s;
// ... reads 8 more fields
```

**Fix:** `get_or_insert` should return `&mut MintHistory` directly. Store reference, use throughout:
```rust
// MintHistoryMap::get_or_insert() return type: &mut MintHistory
let history = self.mint_map.get_or_insert_mut(&trade.mint, now);
history.push(record, now);

// ... check excluded_mints, graduation boundary, health ...

// Then use same reference (if has_position check doesn't consume it):
let history_age_ms = now.saturating_sub(history.first_seen_ms);
// etc — no second lookup needed
```
**Savings: ~25–50ns per trade** (one hashmap lookup eliminated).

---

### F. Gate reordering — gates.rs evaluate()

Current order vs optimal (cheapest/highest-reject first):

| Current Gate | Reason | Cost | Optimal Position |
|---|---|---|---|
| 0a BlockedHour | Vec scan | ~15ns | 1st (but use bitmask → ~1ns) |
| 0b SourceBlocked | Vec scan | ~5ns | 2nd |
| 1 NotBuy | bool | ~1ns | 3rd |
| 2 TriggerSize | 2× u64 cmp | ~2ns | 4th |
| **3b MaxCurveProgress** | float div | ~15ns | **5th (after fix: ~2ns)** |
| 3 VSolOutOfRange | 2× u64 cmp | ~2ns | 6th |
| 4 TokenTooOld | u64 cmp | ~2ns | 7th |
| ... | ... | ... | ... |
| 14b BuySellRatio | f64 div | ~12ns | Move earlier (after fix: ~3ns) |
| 17 ScoreTooLow | f64 cmp | ~3ns | Last (score computed before) |

**Key move:** MaxCurveProgress (after fix to u64 compare) should be Gate 2b — right after trigger size check — since it rejects ~78% of late-curve tokens. This eliminates all subsequent gate processing for those tokens.

---

### G. `excluded_mints` HashSet — hot_path.rs

**Current:** `hashbrown::HashSet<[u8; 32]>` — AHash on 32-byte key ~15–40ns.

**Upgrade:** Add Bloom filter pre-check. At typical 100–500 excluded mints, a 512-bit Bloom (2 hash functions) gives <1% false positive rate with ~2ns check:
```rust
// Add to HotPath:
excluded_bloom: [u64; 8],  // 512-bit bloom filter, stack-allocated

// on_token_created() when adding to excluded_mints:
// Also set bloom bits

// on_trade() fast-path:
let h1 = fast_hash_low(&trade.mint);
let h2 = fast_hash_high(&trade.mint);
if (self.excluded_bloom[h1 & 7] >> (h1 >> 3 & 63)) & 1 == 1
    && (self.excluded_bloom[h2 & 7] >> (h2 >> 3 & 63)) & 1 == 1 {
    // May be excluded — do full hashset lookup
    if self.excluded_mints.contains(&trade.mint) { ... }
}
// Otherwise skip hashset lookup entirely
```
**Savings: ~10–35ns** for the common case (not excluded). Bloom check ~2ns vs hashset ~15–40ns.

---

## 3. FEED PIPELINE

### Channel hops: PumpPortal → hot_path

From corecast.rs and main.rs analysis:
```
PumpPortal WebSocket frame
  → tokio-tungstenite receive (~0.3ms)
  → simd-json parse (✅ already using simd-json)
  → crossbeam_channel::Sender<FeedEvent> (✅ already crossbeam)
  → main.rs event loop recv()
  → hot_path.on_trade() direct call (same thread)
```

**Good news:** Already using `crossbeam-channel` (not tokio mpsc). 1 channel hop. Hot path called directly — no additional dispatch overhead.

**Remaining issue:** The event loop processes events sequentially. If tick processing (position management) takes >1ms, incoming trade events queue up. Consider:
- Separate tick thread: positions.tick() on its own 10ms timer thread
- Hot path thread: dedicated to on_trade() only, no tick processing in same loop

### Thread pinning

Not observed in codebase. Add to main.rs:
```rust
// Pin hot-path thread to CPU core 1 (reserve core 0 for OS/interrupts)
use libc::{cpu_set_t, sched_setaffinity, CPU_SET, CPU_ZERO};
unsafe {
    let mut cpuset: cpu_set_t = std::mem::zeroed();
    CPU_ZERO(&mut cpuset);
    CPU_SET(1, &mut cpuset);
    sched_setaffinity(0, std::mem::size_of::<cpu_set_t>(), &cpuset);
}
```
**Savings: 5–20% reduction in cache miss rate** from dedicated core.

---

## 4. TX PIPELINE

### builder.rs — static pre-serialization

Transaction structure has static parts (program IDs, account keys, instruction discriminators) and dynamic parts (amount, blockhash, slot). 

**Current:** Full transaction built on each trade signal.

**Fix:** Pre-serialize static instruction layout. Only patch:
1. `amount` field at known byte offset
2. `recent_blockhash` (32 bytes at fixed offset in serialized tx)
3. Signature (64 bytes)

This eliminates repeated `bincode::serialize` overhead. ~3–8μs savings per trade.

### executor.rs — BlockhashCache

**Good news:** Already implemented — `BlockhashCache` refreshes every 25s, eliminating per-trade `getLatestBlockhash` RPC (~200ms saved). ✅

### jito.rs — HTTP vs gRPC

**Current:** REST HTTP POST to Jito block engine per bundle.

**Problem:** Each bundle submission = new HTTP connection + TLS handshake = ~50–150ms.

**Fix:** Jito provides a gRPC endpoint (`searcher.proto`). Persistent bidirectional stream:
- Connect once at startup
- Stream bundles as gRPC messages
- Eliminates connection overhead per bundle
- Latency: ~10–30ms vs ~50–150ms

**Implementation:** Add `tonic` dependency, implement `BundleStream` that wraps gRPC channel. Keep HTTP as fallback.

**Savings: ~40–120ms per bundle submission.** Biggest single TX pipeline improvement.

---

## 5. SHREDSTREAM ACTIVATION

### Current state (feeds/shredstream.rs)

The file exists but implements a stub/skeleton. It defines the connection infrastructure but is not wired into the event routing in main.rs or corecast.rs.

### Technical architecture

ShredStream delivers transaction shreds from Jito's validator network **before block finalization**. For our use case:

**The mint address problem:**
- Standard Helius `logsSubscribe`: does NOT include `accountKeys` → can't get mint. Known limitation.  
- ShredStream provides **raw transaction shreds** with full account key data → mint address IS available.
- Decoding: parse `ShredVariant::LegacyData`/`MerkleData` → reassemble transaction → decode pump.fun instruction → extract mint from account index.

### Wiring plan

```rust
// 1. feeds/shredstream.rs: implement parse_pump_trade(shred: &[u8]) -> Option<TradeEvent>
//    - reassemble shred fragments by nonce
//    - deserialize transaction
//    - find pump.fun program instruction (discriminator: buy/sell)
//    - extract accounts[1] = mint, reserves from instruction data

// 2. feeds/corecast.rs or new feeds/shredstream_router.rs:
//    - Priority routing: ShredStream event for mint X suppresses PumpPortal
//      event for same sig_prefix within 200ms window
//    - Use existing helius_sig_ring pattern in hot_path.rs for dedup

// 3. main.rs: spawn shredstream feed task alongside pumpportal
if config.shredstream_enabled {
    let ss_tx = hot_path_tx.clone();
    tokio::spawn(feeds::shredstream::run(config.shredstream_endpoint, ss_tx));
}
```

### Fallback
If ShredStream silent for >5s → fall back to PumpPortal as primary.

**Expected savings: 50–120ms end-to-end latency** (20–30ms ShredStream vs 80–130ms PumpPortal).

---

## 6. BUILD PROFILE (Cargo.toml)

**Current `[profile.release]`:**
```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

**Already optimal for most settings.** ✅ lto=fat, codegen-units=1, panic=abort are all set.

**Missing:**
```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
# ADD THESE:
overflow-checks = false   # Remove debug overflow checks in release (~2-5% perf gain)
```

**Build environment additions** (set in CI / run script):
```bash
export RUSTFLAGS="-C target-cpu=native -C target-feature=+avx2,+bmi1,+bmi2"
# target-cpu=native: enables SIMD instructions for this specific CPU
# avx2: 256-bit SIMD for any vectorizable loops in scorer/gate stack
# bmi1/bmi2: fast bit manipulation (popcount, trailing zeros — used in hashbrown)
```

**Expected gain from `target-cpu=native`: 5–15%** on hot path due to AVX2 in hashbrown AHash.

---

## 7. PRIORITIZED IMPLEMENTATION LIST

| Priority | Impact | File | Change | Est. ns/ms saved | Complexity |
|----------|--------|------|--------|------------------|------------|
| 1 | **~80ms** | feeds/shredstream.rs + main.rs | Activate ShredStream as primary trigger | 50–120ms/trade | 4 |
| 2 | **~60ms** | tx/jito.rs + tx/executor.rs | Jito gRPC persistent stream vs HTTP | 40–120ms/bundle | 4 |
| 3 | **~50ns** | engine/hot_path.rs | Eliminate second MintHistoryMap lookup | 25–50ns/trade | 2 |
| 4 | **~20ns** | engine/gates.rs + hot_path.rs | Hour bitmask (replace Vec<u8> scan) | 15–20ns/trade | 1 |
| 5 | **~20ns** | engine/gates.rs | MaxCurveProgress: u64 threshold vs float | 10–20ns/trade | 1 |
| 6 | **~15ns** | engine/gates.rs | Gate 14b: integer ratio vs f64 division | 8–15ns/trade | 1 |
| 7 | **~15ns** | engine/hot_path.rs | MaxCurveProgress gate: move to Gate 2b | 10–30ns/trade | 1 |
| 8 | **~15ns** | engine/hot_path.rs | Bloom filter for excluded_mints | 10–35ns/trade | 2 |
| 9 | **~8μs** | tx/builder.rs | Pre-serialize static tx portions | 3–8μs/trade | 3 |
| 10 | **~5ns** | engine/hot_path.rs | now_ms() raw tick arithmetic | 3–5ns/call | 2 |
| 11 | **5–15%** | Build env | RUSTFLAGS=-C target-cpu=native | Global perf | 1 |
| 12 | **~2%** | Cargo.toml | overflow-checks=false | Global perf | 1 |
| 13 | OS | main.rs | Thread pinning (sched_setaffinity) | Cache miss reduction | 2 |
| 14 | OS | main.rs | Separate tick thread from event thread | Eliminates tick jitter | 3 |

### Quick wins (ship immediately, Complexity ≤ 2):
- Items 3, 4, 5, 6, 7, 10, 11, 12 — all low complexity, combined ~100–150ns savings per trade

### High-leverage (ship next sprint):
- Items 1 (ShredStream) and 2 (gRPC Jito) — combined ~100–200ms savings per trade cycle

---

## IMPLEMENTATION NOTES FOR ENGINEER

### Files to touch for quick wins:
1. `engine/gates.rs`: GateConfig (add bitmask fields, threshold fields), GateStack::new() (precompute), evaluate() (use bitmasks, integer comparisons, reorder gates)
2. `engine/hot_path.rs`: add `boosted_hours_bitmask: u32`, `excluded_bloom: [u64; 8]`, eliminate second mint_map lookup, update now_ms()
3. `Cargo.toml`: add `overflow-checks = false`
4. Build script / systemd unit: add `RUSTFLAGS="-C target-cpu=native"`

### Files to touch for high-leverage (separate PRs):
5. `feeds/shredstream.rs`: implement real shred parsing
6. `tx/jito.rs` + `tx/executor.rs`: add gRPC bundle stream with HTTP fallback

### Do not change:
- `simd-json` is already in use ✅
- `crossbeam-channel` is already in use ✅  
- `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` already set ✅
- `quanta` RDTSC clock already in use ✅
- BlockhashCache already implemented ✅
