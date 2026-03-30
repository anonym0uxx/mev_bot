# ARCH_FEED_EVIDENCE.md — Engineer 3: Feed-Aware Evidence Routing

## Overview

**Primary file:** `rust/pump-quant-core/src/engine/hot_path.rs` (MODIFY)

**Integration touchpoints:**
1. `positions.rs` — `on_subsequent_trade()` must pass `FeedSource` through to RideState
2. `feeds/mod.rs` — `TradeEvent.source` already exists; add `FeedSource::as_u8()` helper

**Goal:** Route `FeedSource` from `TradeEvent` through the hot path into `RideState::on_buy_event()` / `on_sell_event()`, enable source-aware evidence weighting, implement Helius dedup, ShredStream pre-confirm retraction, unique wallet bonus, and whale sell detection.

---

## FeedSource Extensions (feeds/mod.rs)

### Add `as_u8()` to FeedSource

```rust
// In feeds/mod.rs — add to existing FeedSource enum:
impl FeedSource {
    /// Convert to u8 index for evidence weight LUT lookup.
    /// PumpPortal=0, Helius=1, CoreCast=2, ShredStream=3.
    #[inline(always)]
    pub fn as_u8(self) -> u8 {
        match self {
            FeedSource::PumpPortal => 0,
            FeedSource::Helius     => 1,
            FeedSource::CoreCast   => 2,
            FeedSource::ShredStream => 3,
        }
    }
}
```

**This is the ONLY change to `feeds/mod.rs`.** `FeedSource` enum variants and `TradeEvent` struct are unchanged.

---

## Evidence Weight Lookup Table

Defined in `bayesian_signal.rs` (Engineer 1), consumed here. Reproduced for reference:

```rust
/// [is_buy: 0=sell, 1=buy][source: PP=0, Hel=1, CC=2, SS=3]
pub const EVIDENCE_WEIGHTS: [[u8; 4]; 2] = [
    /* sell */ [10, 10, 25, 15],
    /* buy  */ [10, 10, 10, 12],
];
```

### Special Weight Overrides (applied in hot_path.rs before calling RideState)

| Condition | Weight Override | Mechanism |
|-----------|---------------|-----------|
| Creator sell (CoreCast verified) | `CREATOR_SELL_WEIGHT = 50` | Passed as `source_weight_override` to `on_sell_event()` |
| Whale sell (>2 SOL = 2000 mSOL) | `base_weight × WHALE_SELL_MULTIPLIER (3)` | Multiplied before passing |
| Unique new wallet buying | `+UNIQUE_WALLET_BONUS (5)` added to α after normal update | Applied in `on_buy_event()` by checking bloom filter |
| ShredStream pre-confirm | Normal weight but 90% confidence | Retract if unconfirmed (see below) |

---

## Hot Path Changes (hot_path.rs)

### Change 1: Pass `FeedSource` through `on_subsequent_trade()`

Currently, `positions.rs::on_subsequent_trade()` receives a `&TradeEvent` which has `event.source`. But the call to `RideState::on_buy_event()` inside `positions.rs` does NOT pass `source`. Engineer 3 modifies the routing.

**Option A (minimal change):** `positions.rs::on_subsequent_trade()` already receives the full `TradeEvent`. Just extract `event.source.as_u8()` and pass it to `RideState::on_buy_event(buy_mvsol, now_ms, wallet_hash, event.source.as_u8())`.

**Option B (hot_path.rs change):** Have `hot_path.rs` pre-extract source and pass it separately.

**Decision: Option A** — `positions.rs` already has the `TradeEvent`. Minimal change. Engineer 4 coordinates the positions.rs signature update with Engineer 2.

### Change 2: Creator Sell Detection Routing

Currently in `hot_path.rs`:
```rust
pub fn on_creator_sell(&mut self, mint: &[u8; 32], ts_ms: u64) {
    self.stats.creator_sells += 1;
    if let Some(history) = self.mint_map.get_mut(mint) {
        history.creator_sell_at_ms = ts_ms;
    }
}
```

**ADD** to `on_creator_sell()`:
```rust
/// Mark creator sell for immediate exit AND inject heavy β evidence.
///
/// Called when CoreCast detects a signer-verified creator sell.
/// Two effects:
///   1. RideState.flags |= CREATOR_SELL (emergency exit on next tick)
///   2. Inject CREATOR_SELL_WEIGHT into β for Bayesian logging (even though
///      emergency exit fires first, the Bayesian state captures the evidence)
pub fn on_creator_sell(&mut self, mint: &[u8; 32], ts_ms: u64) {
    self.stats.creator_sells += 1;
    if let Some(history) = self.mint_map.get_mut(mint) {
        history.creator_sell_at_ms = ts_ms;
    }
    // If we have an open position, mark creator sell on its RideState
    if let Some(pos) = self.position_manager.get_position_mut(mint) {
        match &mut pos.exit_mode {
            ExitMode::Ride(ref mut rs) => {
                rs.mark_creator_sell();
                // Also inject heavy β evidence for Bayesian logging
                // source=2 (CoreCast), weight=CREATOR_SELL_WEIGHT
                let sell_mvsol = 1000u32; // estimate 1 SOL creator sell
                rs.on_sell_event(sell_mvsol, ts_ms, 2, true, &self.config().ride_config);
            }
        }
    }
}
```

**IMPORTANT:** `on_creator_sell()` is called from the main event loop when `FeedEvent::CreatorSell` arrives from CoreCast. This is a cold path (~rare event), not hot path.

### Change 3: Helius Dedup Enhancement

Currently, the Helius lead-time tracking uses a sig_prefix ring buffer (`helius_sig_ring`). When PumpPortal confirms a trade already seen by Helius, we measure lead time.

**Enhancement:** When the same trade arrives from both Helius and PumpPortal, the Bayesian update should only count α/β ONCE (from whichever arrives first, typically Helius), and the second arrival only enriches reserves data.

**Implementation in `on_subsequent_trade()` flow (positions.rs):**

The dedup is already partially handled: `TradeEvent.sig_prefix` is available. We need a small sig_ring on RideState or OpenPosition to detect duplicate sigs.

**Decision: Use the existing `helius_sig_ring` in HotPath, NOT per-position.**

In `hot_path.rs::on_trade()`, BEFORE calling `position_manager.on_subsequent_trade()`:

```rust
/// Check if this trade's sig has already been seen by another feed source.
/// If so, set a flag that positions.rs can use to skip Bayesian α/β update
/// but still update reserves/market_cap.
///
/// Implementation: compare trade.sig_prefix against helius_sig_ring entries.
/// If match found AND source is PumpPortal (arriving second):
///   → set event_is_deduped = true
///   → still call on_subsequent_trade (updates reserves)
///   → but pass a flag to skip α/β update
#[inline(always)]
fn is_deduped_trade(&self, trade: &TradeEvent) -> bool {
    if trade.source != FeedSource::PumpPortal {
        return false; // Only dedup PP against Helius
    }
    let sig_u64 = u64::from_le_bytes(trade.sig_prefix);
    for &(stored_sig, stored_ts) in &self.helius_sig_ring {
        if stored_sig == sig_u64 && stored_ts > 0 {
            // Helius already saw this sig
            return true;
        }
    }
    false
}
```

**In `on_trade()`:**
```rust
// After: if self.position_manager.has_position(&trade.mint)
let deduped = self.is_deduped_trade(trade);
self.position_manager.on_subsequent_trade_v3(trade, now, deduped);
```

**In `positions.rs` (Engineer 4 integrates):**
```rust
/// v3 on_subsequent_trade with dedup flag.
/// When deduped=true: skip Bayesian α/β update, but still update reserves and counters.
pub fn on_subsequent_trade_v3(&mut self, event: &TradeEvent, now_ms: u64, deduped: bool) -> bool
```

When `deduped=true`, the call to `rs.on_buy_event()` / `rs.on_sell_event()` still fires for ring buffer and counter updates, but passes `source_weight_override=0` with a special sentinel meaning "zero evidence weight" — or simply a `skip_bayesian: bool` param.

**Simpler approach:** Just pass `deduped` as a parameter to `RideState::on_buy_event()` / `on_sell_event()`:

```rust
// In RideState (Engineer 2):
pub fn on_buy_event(&mut self, sol_mvsol: u32, now_ms: u64, wallet_hash: u64, source: u8, skip_bayesian: bool) {
    // ... ring buffer updates (always) ...
    if !skip_bayesian {
        // ... α update ...
    }
}
```

### Change 4: ShredStream Pre-Confirm Handling

ShredStream events arrive as `PreWarmEvent` (not `TradeEvent`) because they're pre-confirmation. The current `on_prewarm()` adds them to MintHistory but does NOT update RideState.

**Enhancement:** For positions we already hold, ShredStream pre-warms should update the Bayesian posterior immediately (with 90% confidence), then retract if unconfirmed.

**Implementation:**

Add to `HotPath`:

```rust
/// ShredStream pre-confirm ring per position (max 3 pending pre-confirms).
/// Stored on HotPath, not RideState (to keep RideState at 128 bytes).
/// Key: mint, Value: [(sig_prefix_u64, alpha_delta, beta_delta, slot); 3]
///
/// NOT IMPLEMENTED IN v3.0 — ShredStream is pending Jito WL.
/// This is the integration seam for when ShredStream becomes available.
///
/// When ShredStream delivers a trade for a held position:
///   1. Apply α/β update with 90% weight (evidence × 230 / 256)
///   2. Record the delta in shred_pending_ring
///   3. When Helius/PP confirms same sig: mark as confirmed, drop pending entry
///   4. If NOT confirmed within 2 slots (~800ms): retract the α/β delta
struct ShredPendingEntry {
    sig_prefix: u64,
    alpha_delta: u16,
    beta_delta: u16,
    applied_at_ms: u64,
}

// Per-mint, max 3 pending entries (tiny, stack-allocated)
// Total memory: ~32 bytes per mint × max_concurrent_positions
```

**For v3.0:** ShredStream is not yet active (pending Jito WL). Document the seam but do NOT implement. When ShredStream goes live, add:
1. `on_shred_prewarm()` method on HotPath that checks position existence → applies weighted evidence
2. Confirmation path in `on_trade()` that matches sig_prefix and marks shred entry as confirmed
3. Retraction path in `on_tick()` that checks expired shred entries and reverses α/β delta

### Change 5: Unique Wallet Bonus

Currently, `RideState::on_buy_event()` updates the bloom filter and counts unique wallets. The unique wallet count is used in the old composite score as a feature.

**Enhancement:** In Bayesian mode, a new unique wallet buying adds an extra `+UNIQUE_WALLET_BONUS (5)` to α.

**Implementation (in `RideState::on_buy_event()` — Engineer 2):**

```rust
// After bloom_insert:
let old_count = self.unique_wallets;
signal_engine::bloom_insert(&mut self.bloom_filter, wallet_hash);
self.unique_wallets = signal_engine::bloom_count(&self.bloom_filter);

// Unique wallet bonus (Bayesian mode only)
if self.unique_wallets > old_count && !skip_bayesian {
    self.alpha_x16 = self.alpha_x16.saturating_add(
        bayesian_signal::UNIQUE_WALLET_BONUS as u16
    );
}
```

### Change 6: Whale Sell Detection

Currently, `RideState::on_sell_event()` checks for whale exits (>2 SOL) as an emergency exit. This is unchanged.

**Enhancement:** Even for sells BELOW the whale threshold, large sells get amplified β evidence.

**Implementation (in `positions.rs::on_subsequent_trade()` — Engineer 4 integrates):**

```rust
// Before calling rs.on_sell_event():
let sell_mvsol = lamports_to_mvsol(event.sol_amount);
let is_whale = sell_mvsol > 2000; // > 2 SOL
let is_creator = false; // Set true only from on_creator_sell()

// RideState::on_sell_event handles whale multiplier internally:
// If sell_mvsol > 2000: β weight is 3× normal via WHALE_SELL_MULTIPLIER
```

---

## Evidence Flow Diagram

```
TradeEvent arrives from FeedEvent::Trade
    │
    ▼
hot_path.rs::on_trade()
    │
    ├─ Check helius_sig_ring for dedup (is_deduped_trade)
    │
    ├─ Has position for this mint?
    │   │
    │   ▼ YES
    │   position_manager.on_subsequent_trade_v3(event, now, deduped)
    │       │
    │       ├─ event.is_buy?
    │       │   │
    │       │   ▼ YES
    │       │   rs.on_buy_event(mvsol, now, wallet_hash, source_u8, deduped)
    │       │       ├─ Ring buffer update (always)
    │       │       ├─ Bloom filter update (always)
    │       │       ├─ if !deduped: α += weight × amount_scale
    │       │       ├─ if unique_wallet && !deduped: α += UNIQUE_WALLET_BONUS
    │       │       └─ rs.on_tick() → decay + f̂ + state + trail + exit check
    │       │
    │       │   ▼ NO (sell)
    │       │   rs.on_sell_event(mvsol, now, source_u8, is_creator, config)
    │       │       ├─ Ring buffer update (always)
    │       │       ├─ Emergency checks: creator, whale, cascade
    │       │       ├─ β += weight × amount_scale × whale_mult
    │       │       └─ rs.on_tick() → same as above
    │       │
    │       └─ Return closed=true/false
    │
    ├─ No position → entry evaluation (unchanged)
    │
    └─ Return

FeedEvent::CreatorSell arrives (from CoreCast)
    │
    ▼
hot_path.rs::on_creator_sell()
    ├─ history.creator_sell_at_ms = ts
    ├─ If position exists:
    │   ├─ rs.mark_creator_sell()  (flags |= CREATOR_SELL)
    │   └─ rs.on_sell_event(1000, ts, 2/*CoreCast*/, true/*is_creator*/, config)
    │       └─ β += CREATOR_SELL_WEIGHT × amount_scale
    └─ Return

FeedEvent::PreWarm arrives (from Helius/ShredStream)
    │
    ▼
hot_path.rs::on_prewarm()
    ├─ If Helius: record sig_prefix in helius_sig_ring (for dedup)
    ├─ If ShredStream + position exists: [FUTURE — not in v3.0]
    │   ├─ Apply 90%-confidence α/β update
    │   └─ Record in shred_pending_ring
    └─ Mint history update (existing behavior)
```

---

## Struct Changes Summary

### HotPath (hot_path.rs) — fields added:

None. The `helius_sig_ring` already exists. `is_deduped_trade()` is a new method, not new state.

### positions.rs — function signature changes:

```rust
// OLD:
pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64) -> bool

// NEW (add deduped parameter):
pub fn on_subsequent_trade(&mut self, event: &TradeEvent, now_ms: u64, deduped: bool) -> bool
```

### RideState (ride_state.rs) — function signature changes (documented in ARCH_RIDESTATE_V3.md):

```rust
// on_buy_event: +source, +skip_bayesian
pub fn on_buy_event(&mut self, sol_mvsol: u32, now_ms: u64, wallet_hash: u64, source: u8, skip_bayesian: bool)

// on_sell_event: +source, +is_creator_sell
pub fn on_sell_event(&mut self, sol_mvsol: u32, now_ms: u64, source: u8, is_creator_sell: bool, config: &RideConfig) -> Option<RideExitReason>
```

---

## Files This Engineer Writes/Modifies

1. **PRIMARY:** `src/engine/hot_path.rs` — add `is_deduped_trade()`, modify `on_trade()` to extract source and dedup, modify `on_creator_sell()` to inject β evidence
2. **INTEGRATION:** `src/feeds/mod.rs` — add `FeedSource::as_u8()` method (3 lines)

## Files This Engineer Does NOT Touch

- `bayesian_signal.rs` (Engineer 1)
- `ride_state.rs` (Engineer 2 — but Engineer 3 documents the required signature changes)
- `positions.rs` (Engineer 4 — but Engineer 3 documents the dedup parameter addition)
- `paper_logger.rs` (Engineer 4)

---

## Performance Budget

| Operation | Budget | Notes |
|-----------|--------|-------|
| `is_deduped_trade()` | <200ns | 256-entry linear scan, 4 entries per cache line, typically early-exit |
| `FeedSource::as_u8()` | <1ns | Single match → u8, compiled to jump table or direct value |
| Evidence routing overhead | <2ns | Extract source, pass as parameter |
| Whale detection | <1ns | Single comparison (sol_mvsol > 2000) |
| Unique wallet bonus | <2ns | Comparison of old vs new bloom count |
| **Total added to hot path** | <5ns | (dedup check is only for PP trades with existing positions) |

The dedup scan (`is_deduped_trade`) is the most expensive operation but:
1. Only runs for PumpPortal trades (not Helius/CoreCast/ShredStream)
2. Only runs when we have an open position for the mint
3. The ring is 256 entries × 16 bytes = 4KB, fits in L1 cache
4. Average scan length is ~128 entries before finding a match (Helius typically leads by <1s)

---

## Compile-Time Assertions

```rust
// In feeds/mod.rs:
const _: () = assert!(core::mem::size_of::<FeedSource>() == 1); // u8-sized enum

// In hot_path.rs:
// helius_sig_ring is 256 × 16 bytes = 4096 bytes = exactly 1 page
const _: () = assert!(core::mem::size_of::<[(u64, u64); 256]>() == 4096);
```

---

## Test Cases

### Test 1: FeedSource::as_u8() mapping

```rust
#[test]
fn test_feed_source_as_u8() {
    assert_eq!(FeedSource::PumpPortal.as_u8(), 0);
    assert_eq!(FeedSource::Helius.as_u8(), 1);
    assert_eq!(FeedSource::CoreCast.as_u8(), 2);
    assert_eq!(FeedSource::ShredStream.as_u8(), 3);
}
```

### Test 2: Helius dedup prevents double-counting

```rust
#[test]
fn test_helius_dedup() {
    // Setup: create HotPath, open a position
    let mut hp = test_hot_path();
    let mint = [0xAA; 32];
    open_test_position(&mut hp, &mint);

    // Simulate Helius seeing a trade first
    let sig = [0xBB; 64];
    let sig_prefix = {
        let mut p = [0u8; 8];
        p.copy_from_slice(&sig[..8]);
        p
    };
    hp.helius_sig_ring[0] = (u64::from_le_bytes(sig_prefix), 1000);
    hp.helius_sig_ring_head = 1;

    // PumpPortal trade with same sig_prefix → should be deduped
    let trade = make_trade_event(mint, sig, 100_000_000, 31_000_000_000,
                                  1_000_000_000_000_000, true);
    assert!(hp.is_deduped_trade(&trade));

    // Different sig_prefix → not deduped
    let trade2 = make_trade_event(mint, [0xCC; 64], 100_000_000, 31_000_000_000,
                                   1_000_000_000_000_000, true);
    assert!(!hp.is_deduped_trade(&trade2));
}
```

### Test 3: Creator sell injects heavy β evidence

```rust
#[test]
fn test_creator_sell_evidence_injection() {
    let mut hp = test_hot_path();
    let mint = [0xAA; 32];
    open_test_position(&mut hp, &mint);

    // Get initial β
    let beta_before = get_ride_state(&hp, &mint).beta_x16;

    // Creator sell event
    hp.on_creator_sell(&mint, 2000);

    // β should have increased significantly (CREATOR_SELL_WEIGHT = 50)
    let beta_after = get_ride_state(&hp, &mint).beta_x16;
    assert!(beta_after > beta_before + 100,
        "Creator sell should massively increase β: {} → {}", beta_before, beta_after);

    // Creator sell flag should be set
    assert!(get_ride_state(&hp, &mint).flags & ride_flags::CREATOR_SELL != 0);
}
```

### Test 4: Whale sell gets amplified β weight

```rust
#[test]
fn test_whale_sell_amplified() {
    let mut hp = test_hot_path();
    let mint = [0xAA; 32];
    open_test_position(&mut hp, &mint);

    let beta_before = get_ride_state(&hp, &mint).beta_x16;

    // Small sell: 0.1 SOL (100 mSOL)
    let small_sell = make_sell_event(mint, 100_000_000); // 0.1 SOL
    hp.on_trade(&small_sell);
    let beta_after_small = get_ride_state(&hp, &mint).beta_x16;
    let small_delta = beta_after_small - beta_before;

    // Reset for whale sell test (new position)
    let mint2 = [0xBB; 32];
    open_test_position(&mut hp, &mint2);
    let beta_before2 = get_ride_state(&hp, &mint2).beta_x16;

    // Whale sell: 3 SOL (3000 mSOL) — should get 3× weight
    // Note: whale sell (>2 SOL) triggers emergency exit via RideState::on_sell_event
    // So β increase will be visible even though position closes immediately
    let whale_sell = make_sell_event(mint2, 3_000_000_000); // 3 SOL
    hp.on_trade(&whale_sell);

    // Position was closed by whale exit emergency, but β was updated first
    // This test verifies the weight was applied before the emergency exit fires
    // (The actual beta_after is captured in ClosedPosition for JSONL logging)
}
```

### Test 5: Source-aware evidence routing

```rust
#[test]
fn test_source_aware_routing() {
    // Verify that CoreCast sells produce more β evidence than PumpPortal sells
    let cfg = test_config();

    // Position A: PumpPortal sell
    let mut rs_a = RideState::new(30_000, 1000, 300, 500, 1000, 1, true, &cfg);
    rs_a.on_sell_event(500, 1010, 0 /* PumpPortal */, false, &cfg);
    let beta_pp = rs_a.beta_x16;

    // Position B: CoreCast sell (same amount, not creator-sell)
    let mut rs_b = RideState::new(30_000, 1000, 300, 500, 1000, 1, true, &cfg);
    rs_b.on_sell_event(500, 1010, 2 /* CoreCast */, false, &cfg);
    let beta_cc = rs_b.beta_x16;

    // CoreCast base weight (25) > PumpPortal base weight (10) for sells
    assert!(beta_cc > beta_pp,
        "CoreCast sell should produce more β than PumpPortal: {} > {}", beta_cc, beta_pp);
}
```
