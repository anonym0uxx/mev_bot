# Master Build Plan v2 — Signal-to-Exit Optimization + Jito ShredStream WL

**Date:** 2026-03-30
**Engineer:** Apollo (Opus 4.6 bare-metal Rust)
**Scope:** Full pipeline audit from feed ingestion → entry → exit

---

## Executive Summary

After auditing all 26K lines of the Rust engine, I've identified 8 optimization phases targeting the full signal-to-exit pipeline. Priorities ranked by **expected PnL impact** (not just latency).

The Jito ShredStream whitelist approval gives us ~80ms latency advantage over PumpPortal — but only if we wire it correctly as a **primary trigger** (not just pre-warm).

---

## Phase 1: ShredStream Primary Trigger (Jito WL)

**Impact: HIGH (80ms latency advantage = first-mover on entries)**
**Risk: MEDIUM**
**Files: `shredstream.rs`, `event_joiner.rs`, `hot_path.rs`, `feeds/mod.rs`**

### Current State
- ShredStream exists but only emits `FeedEvent::PreWarm` (no vSOL reserves, no trader, no sig)
- PreWarm events are used for dedup correlation only — NOT for triggering entries
- The actual entry trigger comes from PumpPortal (~80ms later)

### Problem
With Jito WL, we get **full transaction data** from ShredStream (not just discriminator+mint+sol). The current parse_trade() only extracts 3 fields from raw shreds. With WL access, we get gRPC `SubscribeTransactionUpdate` with full decoded transaction including all account keys, vSOL reserves, signatures.

### Changes

1. **`shredstream.rs`** — New `parse_full_transaction()` for Jito gRPC decoded tx:
   - Extract: mint, trader, sig (64 bytes), sol_amount, vsol_reserves, vtoken_reserves, bonding_curve, assoc_bonding_curve
   - Emit `FeedEvent::Trade(TradeEvent)` instead of `FeedEvent::PreWarm`
   - Keep UDP fallback with current `parse_trade()` → PreWarm for non-WL mode

2. **`event_joiner.rs`** — ShredStream priority:
   - When ShredStream emits `Trade` events (not PreWarm), it becomes the primary trigger
   - PumpPortal becomes the confirming feed (for dedup and gap-fill)
   - Helius stays as pre-warmer for graduation detection

3. **`hot_path.rs`** — ShredStream dedup window:
   - Add 200ms dedup window: if ShredStream already triggered an entry, PumpPortal's duplicate is suppressed
   - Use sig_prefix[0..8] matching (already exists for Helius dedup)

4. **`feeds/mod.rs`** — Add `PreWarmEvent.has_full_data: bool` flag to distinguish WL vs non-WL ShredStream events

### Test Plan
- Unit: parse_full_transaction with mock gRPC payload
- Integration: ShredStream Trade → entry trigger → PumpPortal dedup
- Regression: all existing feed tests pass

---

## Phase 2: Helius → simd_json (Feed Parsing Optimization)

**Impact: LOW-MEDIUM (~2-5µs per message, ~100-500/sec)**
**Risk: LOW**
**Files: `helius.rs`**

### Current State
There's literally a TODO in the code:
```
/// TODO(perf): Swap serde_json → simd_json for SIMD-accelerated parsing.
```

### Changes
1. Replace `serde_json::from_str(text)` with `simd_json::to_borrowed_value(bytes)` in:
   - `parse_helius_log()` — buy/sell detection
   - `check_graduation_logs()` — graduation detection
2. Use `unsafe { text.as_bytes_mut() }` for in-place parsing (same pattern as PumpPortal)
3. Access fields via `.get_str()` / `.get_u64()` instead of `.get()?.as_str()?`

### Test Plan
- All existing Helius tests pass with simd_json backend
- Benchmark: parse 10K mock messages, verify < 3µs avg

---

## Phase 3: Hot Path — Eliminate Remaining Float Math

**Impact: LOW (but principle: zero f64 in hot path)**
**Risk: LOW**
**Files: `entry_engine.rs`, `hot_path.rs`, `positions.rs`**

### Current State
The hot path is mostly integer, but there are f64 operations in:
- `entry_engine::score()` — 8 weighted feature computations use f64 multiplication
- `entry_engine::magnitude()` — 7 weighted feature computations use f64 multiplication
- `positions.rs` — PnL calculation uses f64 for delta/ratio
- `hot_path.rs` — ToD multiplier is f64

### Changes
1. **Score/Magnitude weights** — Convert to fixed-point u16 (weight × 1000):
   - `w_buy_burst: 0.30` → `w_buy_burst_x1000: 300`
   - Each feature score (0.0..1.0) becomes u16 (0..1000)
   - Weighted sum: `Σ(w_x1000[i] * feature_x1000[i]) / 1000` = score × 1000
   - Final: `score_x1000 * 100 / 1000` → 0..100 scale

2. **PnL** — Already integer in positions.rs (i128 arithmetic). Just audit and clean up any remaining f64 casts.

3. **ToD multiplier** — Convert to u16 (multiplier × 100): `125` = 1.25×

### Test Plan
- Cross-validate: compute scores with both f64 and integer paths on test inputs, verify ≤ ±1 on 0-100 scale
- All existing entry_engine tests pass

---

## Phase 4: MintMap / MintHistory Cache Line Optimization

**Impact: MEDIUM (MintMap is accessed on EVERY trade event)**
**Risk: LOW**
**Files: `core/mint_map.rs`**

### Current State
MintMap uses `HashMap<[u8; 32], MintHistory>`. Every trade event does a HashMap lookup (one per event, ~20-50ns depending on load factor). MintHistory struct size needs audit for cache alignment.

### Changes
1. **Audit MintHistory size** — ensure it fits in 1-2 cache lines (64-128 bytes)
2. **Consider `hashbrown::HashMap` with pre-hashed keys** — MintMap keys are [u8;32], but we already compute `u64` hash in watchlist. Use the same hash for MintMap lookups.
3. **Prefault MintHistory pool** — pre-allocate capacity for ~10K mints to avoid mid-session resizing

### Test Plan
- Benchmark: 100K random mint lookups before/after
- Size assertions in tests

---

## Phase 5: Watchlist → Entry Pipeline Tightening

**Impact: HIGH (directly reduces false entries)**
**Risk: MEDIUM**
**Files: `watchlist.rs`, `hot_path.rs`**

### Current State (post-A2)
2-buy confirmation is live. But the watchlist expiry is 2000ms — generous for a memecoin that moves in <500ms.

### Changes
1. **Reduce expiry to 1500ms** — if no 2nd confirm in 1.5s, the token is probably dead
2. **Add vSOL velocity check** — on promotion, compute `(current_vsol - entry_vsol) / elapsed_ms`. If negative (price falling since watch), reject even with 2 confirms. Falling-knife protection.
3. **Strong-interest threshold tuning** — currently 0.10 SOL. Consider raising to 0.15 SOL to reduce noise.

### Test Plan
- Update test_expiry with 1500ms
- New test: vSOL velocity rejection

---

## Phase 6: Exit Engine — Decay + Trail Refinement

**Impact: HIGH (exit quality directly affects realized PnL)**
**Risk: MEDIUM**
**Files: `ride_state.rs`, `bayesian_signal.rs`**

### Current State
57% of trades exit as `momentum_decay_flat` in <100ms. These are dead-on-arrival tokens. The Bayesian model starts with a prior that's too weak — it allows exit before even 1 confirming buy arrives.

### Changes
1. **Stronger prior for MED/HIGH conviction** — increase `PRIOR_STRENGTH[1]` from 9→12, `[2]` from 13→18. This makes the model harder to push into Exit state before real evidence arrives.
2. **Early phase minimum hold** — in `RidePhase::Early`, suppress `SignalExit` for the first 500ms. The model needs at least 1-2 events before it can meaningfully evaluate.
3. **Dynamic trail based on confirming volume** — if `confirming_vol_msol > 0`, tighten trail; if no confirms, widen trail (give it room to breathe while waiting for evidence).

### Test Plan
- Backtest on paper trade data: compare exit timing before/after
- Regression: all existing ride_state tests pass

---

## Phase 7: Transaction Execution Optimization

**Impact: MEDIUM (faster tx = better fill rate)**
**Risk: LOW**
**Files: `tx/skeleton.rs`, `tx/tip_engine.rs`, `tx/executor.rs`**

### Current State
Transaction skeleton is pre-built with patched fields. Good. But the Jito bundle submission could be parallelized with position tracking.

### Changes
1. **Pre-sign skeleton** — keep a warmed EdDSA keypair and pre-sign the immutable parts of the tx
2. **Parallel Jito submission** — fire-and-forget the bundle via a dedicated tokio task, don't block the hot path
3. **Adaptive tip** — tip_engine already exists. Wire it to actual landing rate data from Jito WL (ShredStream provides confirmation times)

### Test Plan
- Existing tx tests pass
- Benchmark: skeleton patch + sign time

---

## Phase 8: Dead Code Removal + Binary Size

**Impact: LOW (cleanliness, faster compile)**
**Risk: VERY LOW**
**Files: multiple**

### Current State
17 compiler warnings about unused imports, dead code, etc. ExitStateMachine is confirmed dead. Legacy scorer/gate_stack removed but some remnants.

### Changes
1. Remove `exit_machine.rs` (confirmed dead code)
2. Remove `scorer.rs` if fully replaced by entry_engine
3. Clean up unused imports identified by compiler
4. Remove unused fields in HotPath (boosted_hours_utc, boosted_hours_bitmask, etc.)

### Test Plan
- `cargo test` passes with zero warnings
- Binary size audit before/after

---

## Build Order

| Priority | Phase | Impact | Risk | Est. Time |
|----------|-------|--------|------|-----------|
| 🔴 1 | P1: ShredStream Primary Trigger | HIGH | MED | 2-3h |
| 🟠 2 | P6: Exit Engine Refinement | HIGH | MED | 1-2h |
| 🟡 3 | P5: Watchlist Tightening | HIGH | MED | 30min |
| 🟢 4 | P2: Helius simd_json | LOW-MED | LOW | 30min |
| 🟢 5 | P3: Float Elimination | LOW | LOW | 1h |
| 🟢 6 | P4: MintMap Optimization | MED | LOW | 45min |
| 🔵 7 | P7: TX Execution | MED | LOW | 1h |
| ⚪ 8 | P8: Dead Code Cleanup | LOW | VERY LOW | 30min |

**Phases 1, 6, 5 are PnL-critical.** Everything else is engineering quality.

---

## Zero-Regression Guarantees

Every phase MUST:
1. Pass `cargo test -p pump-quant-core` (403+ tests)
2. Pass `cargo clippy -p pump-quant-core -- -D warnings` (zero warnings)
3. Not change any existing Kelly LUT values, scoring weights, or Bayesian signal constants
4. Not change feed event types or TradeEvent struct layout
5. Not break PumpPortal/Helius/CoreCast/ShredStream feed parsing
6. Not change Jito tip amounts or bundle submission behavior

Existing Kelly sizing, conviction computation, and Bayesian posterior math are FROZEN unless explicitly approved.
