# ARCHITECT REVIEW V2 — Exit State Machine & SL Strategy

**Date:** 2026-03-29  
**Reviewer:** Principal Architect (Solana MEV / Pump.fun Microstructure)  
**Files reviewed:** `exit_machine.rs`, `positions.rs`, `config.rs`, `canary.json`

---

## Section 1: SL Strategy Verdict

### 1.1 The Data Says Tight Fixed SL Is Correct — But Only for Dead Positions

**Key empirical finding: SL exits are NOT noise-clipped winners. They are correctly identified losers.**

Evidence:
- SL exits (n=914): **0% win rate**, avgMFE = 0.526%, avgHold = 723ms
- **97.6% had buysAfterEntry = 0** — meaning no confirming buyer ever arrived
- Positions that hit SL *never* recovered — the 0% WR is definitive
- The 0.526% avgMFE means they briefly ticked up before the inevitable reversal

This is the critical insight: **tight SL is not clipping good setups.** Good setups are identified by `buysAfterEntry ≥ 1` (WR 80.2%+). Bad setups are `buysAfterEntry = 0` (WR 39.1% but this includes flat/timeout exits). The SL is catching the subset of dead positions that drift adversely before the confirmation window expires.

### 1.2 Bonding Curve Microstructure Analysis

Pump.fun bonding curve tokens use a constant-product AMM (`x * y = k`). Key implications:

1. **Price impact is deterministic** — unlike orderbook markets, there's no "noise" from limit order book microstructure. Each trade moves the curve predictably. A 1% drop means real SOL was sold.

2. **The "dump" is informed flow** — per Karbalaii (2025), insider liquidation follows tranche strategies. When we see sell pressure in the first 200ms, it's not noise — it's potentially the setup dissolving (creator sells, first buyers taking profit, or the flow simply stopped).

3. **Constant product amplifies adverse moves** — as vSOL drains, each marginal sell has MORE price impact (convex slippage). A 1% drop can become 3% faster than on a linear AMM. Tight SL protects against this convexity.

4. **Autocorrelated volatility** (Easley et al. 2024) means that early adverse movement predicts continued adverse movement. The regime identification is baked into our confirmation signal: `buysAfterEntry = 0` IS the adverse regime.

### 1.3 Evaluation of Alternative SL Approaches

| Approach | Verdict | Reasoning |
|---|---|---|
| **Fixed % SL (current)** | ✅ **KEEP** | 0% WR on SL exits proves it's correctly identifying losers |
| **Signal-based (sell event detection)** | ❌ Reject | Sell events are lagging — by the time a large sell hits our feed, price has already moved. The price tick IS the signal. |
| **Time-gated (no SL during 200ms)** | ⚠️ **Partially adopt** | See recommendation below. Data shows flat exits (n=733) die in 74ms — no SL needed because they never move. But SL exits average 723ms hold, meaning most SL triggers happen AFTER the confirmation window anyway. |
| **Volatility-adjusted** | ❌ Reject | Adds complexity without improving WR. The tier system already adjusts SL by trigger size (proxy for volatility regime). |
| **Conviction-scaled (current spec)** | ✅ **KEEP** | Unconfirmed=1.0%, Confirmed=1.5% is correct. Wider SL for confirmed positions lets winners develop. |

### 1.4 Definitive Recommendation: HYBRID TIME-GATED + CONVICTION-SCALED

**Keep the current approach with ONE modification:**

**CHANGE: During UNCONFIRMED state, use a 2-tier SL:**
- **First 100ms**: SL = 1.5% (wider — allow bonding curve settlement noise)
- **After 100ms with no buy**: SL = 1.0% (tighter — position is likely dead)
- **On confirmation (buy arrives)**: SL = 1.5% (current confirmed SL)

**Rationale from the data:**
- Flat exits (n=733) have avgHold = 74ms and avgMFE = 0.002%. These positions are DOA — they never move at all. SL doesn't help or hurt these; they exit via `MomentumDecayFlat` at 200ms.
- SL exits (n=914) have avgHold = 723ms — most fire well after the confirmation window. Only a small fraction would fire in the first 100ms.
- The 0.526% avgMFE of SL exits means they DO move briefly in our favor. A 1.0% SL during the first 100ms might clip a rare setup that takes 80ms to get its first confirming buy.
- But at 100ms+ with no buy, tightening to 1.0% is correct — the position is almost certainly dead.

**However:** The marginal improvement from this 2-tier approach is small (maybe 10-20 of 914 SL exits affected). The current flat 1.0% unconfirmed SL is already performing well. **If implementation complexity is a concern, the current spec is fine.**

### 1.5 Exact Parameters

```
KEEP as-is:
  unconfirmed_sl: 1.0% (per tier)
  confirmed_sl:   1.5% (per tier)
  
OPTIONAL refinement (P2 — next session):
  unconfirmed_sl_early_ms: 100      // first 100ms after entry
  unconfirmed_sl_early_pct: 1.5%    // wider during settlement
  unconfirmed_sl_late_pct: 1.0%     // tighter after 100ms, no buy
```

**Bottom line: The current SL strategy is correct. Don't change it for V1. The data proves it.**

---

## Section 2: exit_machine.rs Review Findings

### CRITICAL Issues

#### C1: `_confirmed_sl_pct` tier lookup is fragile — can return wrong SL

**Location:** `_confirmed_sl_pct()`, lines 149-163

**Problem:** The method identifies the entry tier by matching `base_confirmed_tp_fp == t.confirmed_tp_fp`. This breaks if two tiers share the same `confirmed_tp_fp` value. Currently no tiers collide, but this is a time bomb.

**Fix:** Store `tier_index: u8` in the struct at entry time. Replace the linear scan with a direct index:

```rust
// In ExitStateMachine struct, replace base_confirmed_tp_fp: u32 with:
pub tier_index: u8,
pub base_confirmed_tp_fp: u32,
// tier_index fits in the existing _pad byte (currently [u8; 1])

// In _confirmed_sl_pct:
fn _confirmed_sl_pct(&self, config: &ExitConfig) -> f64 {
    let tier = &config.tp_sl_tiers[self.tier_index as usize];
    tier.confirmed_sl_fp as f64 / 100_000.0
}
```

**Severity:** CRITICAL — silent wrong SL if tiers are reconfigured.

#### C2: `on_buy_event` always confirms, even if price is below entry

**Location:** `on_buy_event()`, Unconfirmed match arm

**Problem:** A buy event for the same token arrives, but current price could be significantly below entry (e.g., we entered at the local peak, price dropped, then someone buys at a lower price). The state machine transitions to CONFIRMED and WIDENS the SL from 1.0% to 1.5%, giving a losing position MORE room to lose.

**Fix:** Add a price guard — only confirm if price is at or above entry:

```rust
ExitState::Unconfirmed => {
    // Only confirm if we're not underwater.
    // A buy while underwater may indicate price discovery, not momentum.
    // Keep SL tight until price recovers to entry.
    // NOTE: This requires current_vsol to be passed in. See Section 3.
    self.last_buy_time_ms = now_ms;
    // For now, always confirm (existing behavior). Engineer should add
    // price check once on_buy_event receives current_vsol parameter.
    self.conviction_level = 1;
    self.state = ExitState::Confirmed;
    ...
}
```

**Recommendation:** Add `current_vsol: f64` parameter to `on_buy_event`. Only confirm if `current_vsol >= self.entry_price_vsol * 0.995` (within 0.5% of entry). This is a P1 change — not blocking next run but should be added this session.

**Severity:** CRITICAL (logic correctness), but low probability in practice because SL usually fires first if price is below entry by >1%.

### IMPORTANT Issues

#### I1: Division on hot path in trailing stop calculation

**Location:** `on_price_tick()`, ConvictionScaled trailing stop block

**Problem:** Two divisions per tick when trail is active:
```rust
let activation_pct = base_tp_pct * config.trail_activation_pct_of_base_tp as f64 / 100.0;
let trail_pct = config.trail_distance_fp as f64 / 100_000.0;
```

The first division is by 100.0, the second by 100_000.0. These are f64 divisions which are ~20-25 cycles each on x86. Not a correctness issue but violates the ≤100ns budget when both fire.

**Fix:** Pre-compute `trail_distance_pct` and `trail_activation_pct` in `on_entry` and store as fields. OR, since these are config-derived constants that don't change per tick, use `mul` by reciprocal:

```rust
// In ExitConfig, add precomputed reciprocals:
pub trail_distance_pct_f64: f64,       // trail_distance_fp as f64 / 100_000.0 (precomputed)
pub trail_activation_pct_f64: f64,     // precomputed
```

Better yet: compute `trail_activation_price: f64` and `trail_distance_mult: f64` once in `on_entry` or on first confirmation, store in struct.

**Severity:** IMPORTANT — hot path latency. Each division is ~20ns, two = ~40ns, plus the multiplies. Total trailing stop block may be 60-80ns, leaving only 20-40ns for rest of function. At 5-10 concurrent positions this matters.

#### I2: `on_price_tick` does SL check before TP check — correct ordering?

**Location:** `on_price_tick()`, lines 105-115

**Problem:** SL is checked before TP. In a scenario where a massive buy pushes price from below SL to above TP in one trade (unlikely but possible with bonding curve mechanics), we'd check the post-trade price against SL (which it's now above) and pass, then check TP and exit correctly. This ordering is actually fine for the common case. BUT: if we receive stale/out-of-order trade events, we might process a low price before a high price that happened simultaneously. This is a Helius/feed ordering issue, not an exit_machine issue.

**Verdict:** Current ordering is correct. No change needed.

**Severity:** MINOR (no actual bug, noting for documentation).

#### I3: `on_tick` in positions.rs allocates Vec on every tick

**Location:** `positions.rs`, `on_tick()`, lines 215-220 and 228-233

**Problem:** Two `Vec::new()` allocations per tick:
```rust
let mut to_close: Vec<([u8; 32], ExitReason)> = Vec::new();
let mut sm_closes: Vec<([u8; 32], ExitReason)> = Vec::new();
```

At 50ms tick interval, this is 20 allocations/second. With max_concurrent_positions=5, these Vecs are almost always empty or length 1.

**Fix:** Use `SmallVec<[_; 4]>` or pre-allocate a fixed-size array on the stack:

```rust
let mut to_close: arrayvec::ArrayVec<([u8; 32], ExitReason), 8> = arrayvec::ArrayVec::new();
```

Or just iterate-and-collect into a stack buffer. Since `max_concurrent_positions` is typically ≤10, a `[Option<_>; 16]` array works too.

**Severity:** IMPORTANT — unnecessary heap allocation on the tick hot path.

#### I4: `on_tick` runs `on_price_tick` with stale `current_vsol` 

**Location:** `positions.rs`, `on_tick()`, line 234-238

**Problem:** When no trade events arrive for a position, `on_tick` calls `exit_sm.on_price_tick(current_vsol)` — but `current_vsol` hasn't changed since the last trade. This means the confirmation window expiry check (`MomentumDecayFlat`) works correctly (it only depends on time), but stall checks re-evaluate with the same price. This is harmless but wasteful.

**Fix:** Add an `unchanged` fast-path: if `current_vsol` hasn't changed since last tick, only check time-based conditions:

```rust
// In ExitStateMachine, add: pub last_ticked_vsol: f64
// In on_price_tick, early return Hold if current_vsol == self.last_ticked_vsol
//   EXCEPT for time-based checks (confirmation window, stall)
```

Actually this optimization isn't worth the complexity. The time-based checks need to run anyway. **Keep as-is.**

**Severity:** MINOR — no bug, mild inefficiency. No action needed.

#### I5: `_apply_conviction_tp` is not `#[inline(always)]`

**Location:** `_apply_conviction_tp()` and `_confirmed_sl_pct()`

**Problem:** These helper methods are `#[inline]` but not `#[inline(always)]`. The compiler may or may not inline them. On the hot path (on_buy_event calls both), a function call adds ~5ns.

**Fix:**
```rust
#[inline(always)]
fn _apply_conviction_tp(...)
#[inline(always)]
fn _confirmed_sl_pct(...)
```

**Severity:** MINOR — compiler likely inlines these anyway, but explicit is safer for the ≤100ns contract.

#### I6: No defensive check for `entry_vsol == 0`

**Location:** `on_entry()`, nowhere checks for zero entry_vsol

**Problem:** If `entry_vsol` is 0 (shouldn't happen, but bonding curve edge case), `current_sl_vsol = 0 * (1 - x) = 0` and `current_tp_vsol = 0 * (1 + x) = 0`. Price will always be >= 0 = TP, triggering an immediate TakeProfit exit on first tick.

**Fix:** Add a debug_assert or early return:

```rust
pub fn on_entry(...) -> Self {
    debug_assert!(entry_vsol > 0.0, "entry_vsol must be positive");
    // ... existing code
}
```

In positions.rs, `open_position` already guards against `vsol_reserves == 0` for TradeEvent. But the ExitStateMachine should be independently defensive.

**Severity:** MINOR — can't happen in practice due to upstream guards, but defense-in-depth.

#### I7: `conviction_tp_multipliers[level_idx]` — verify bounds

**Location:** `_apply_conviction_tp()`, line 139

**Problem:** `level_idx = level.min(4) as usize`. The array is `[u16; 5]` (indices 0-4). `level.min(4)` ensures max index = 4. This is correct.

**Verification:** ✅ No OOB possible. `level` is always 0-4 due to `(self.conviction_level + 1).min(4)` in `on_buy_event`.

**Severity:** NONE — verified correct.

### MINOR Issues

#### M1: `ExitState` enum size could be smaller

**Problem:** `ExitState::ConvictionScaled { level: ConvictionLevel }` makes the enum 2 bytes (discriminant + u8 payload). The `conviction_level` is also stored separately in the struct. This is redundant.

**Fix:** Consider making ExitState a simple 3-variant enum (Unconfirmed, Confirmed, ConvictionScaled) with no payload, using `self.conviction_level` everywhere. Saves 1 byte and simplifies match arms.

```rust
pub enum ExitState {
    Unconfirmed,
    Confirmed,
    ConvictionScaled,
}
// In match arms, use self.conviction_level instead of destructuring level
```

**Severity:** MINOR — no functional impact, slight code cleanup.

#### M2: `find_tier` is called once and is fine, but uses linear scan

**Location:** `find_tier()`, line 145

**Problem:** Linear scan over tiers (max 8). Called only in `on_entry` (cold path). No issue.

**Severity:** NONE — cold path, fine as-is.

#### M3: `on_safety_timeout` takes `&self` but doesn't use any fields

**Location:** `on_safety_timeout()`, line 130

**Problem:** Method could be a free function or associated function. Minor style issue.

**Severity:** MINOR — no functional impact.

---

## Section 3: New Rust Fields Needed

### 3.1 Store `tier_index` (fix C1)

```rust
// In ExitStateMachine struct:
// Replace _pad: [u8; 1] with:
pub tier_index: u8,

// Struct layout becomes:
//   state(1) + conviction_level(1) + trail_active(1) + tier_index(1) + base_confirmed_tp_fp(4) = 8
//   ... rest unchanged
// Total: still 56 bytes ≤ 64 ✅

// In on_entry(), after find_tier:
let tier_idx = Self::find_tier_index(
    &config.tp_sl_tiers[..config.tp_sl_tier_count as usize],
    trigger_lamports,
);
// ...
tier_index: tier_idx,

// Add:
#[inline]
fn find_tier_index(tiers: &[TpSlTierV2], trigger_lamports: u64) -> u8 {
    for (i, t) in tiers.iter().enumerate() {
        if trigger_lamports <= t.trigger_max_lamports {
            return i as u8;
        }
    }
    tiers.len().saturating_sub(1) as u8
}

// Fix _confirmed_sl_pct:
#[inline(always)]
fn _confirmed_sl_pct(&self, config: &ExitConfig) -> f64 {
    let tier = &config.tp_sl_tiers[self.tier_index as usize];
    tier.confirmed_sl_fp as f64 / 100_000.0
}
```

### 3.2 Add `current_vsol` to `on_buy_event` signature (fix C2)

```rust
// Change signature:
pub fn on_buy_event(&mut self, config: &ExitConfig, current_vsol: f64, now_ms: u64) -> ExitDecision {
    self.last_buy_time_ms = now_ms;

    match self.state {
        ExitState::Unconfirmed => {
            // Only confirm if price is near entry (not underwater)
            if current_vsol < self.entry_price_vsol * 0.995 {
                // Buy happened but we're underwater. Don't confirm.
                // SL will handle exit if needed.
                return ExitDecision::Hold;
            }
            self.conviction_level = 1;
            self.state = ExitState::Confirmed;
            // ... rest unchanged
        }
        // ... rest unchanged
    }
}
```

Update call site in `positions.rs`:
```rust
// In on_subsequent_trade, change:
let exit_decision = pos.exit_sm.on_buy_event(
    &self.config.exit_config,
    event.vsol_reserves as f64,  // NEW parameter
    now_ms,
);
```

### 3.3 Pre-compute trail constants (fix I1)

Option A (recommended — zero struct growth):

```rust
// In ExitConfig, add precomputed constants:
pub trail_distance_mult: f64,  // 1.0 - (trail_distance_fp / 100_000.0)

// Set during build_exit_config():
trail_distance_mult: 1.0 - (trail_distance_pct * 100_000.0) as f64 / 100_000.0,
```

Wait — `ExitConfig` is `Clone` + lives on the heap (in `PositionConfig`). Adding one f64 there is fine.

Actually better: just replace the division-per-tick with a multiply:

```rust
// In on_price_tick, ConvictionScaled trailing stop block:
// BEFORE:
let trail_pct = config.trail_distance_fp as f64 / 100_000.0;
let trail_stop = self.peak_price_vsol * (1.0 - trail_pct);

// AFTER (precompute in ExitConfig):
let trail_stop = self.peak_price_vsol * config.trail_keep_mult;
// where trail_keep_mult = 1.0 - trail_distance_fp / 100_000.0, computed once in build_exit_config
```

Add to `ExitConfig`:
```rust
pub trail_keep_mult: f64,           // 1.0 - trail_distance_pct
pub trail_activation_mult: f64,     // trail_activation_pct_of_base_tp / 100.0 (precomputed)
```

---

## Section 4: Config Changes

### 4.1 canary.json — No Changes Required for V1

The current `canary.json` configuration is correct for the existing exit state machine. The `tp_sl_tiers_v2` values match the intended behavior:

```json
"tp_sl_tiers_v2": [
    { "trigger_max_sol": 0.6,  "unconfirmed_sl_pct": 0.010, "confirmed_sl_pct": 0.015 },
    { "trigger_max_sol": 0.8,  "unconfirmed_sl_pct": 0.010, "confirmed_sl_pct": 0.015 },
    { "trigger_max_sol": 1.5,  "unconfirmed_sl_pct": 0.012, "confirmed_sl_pct": 0.015 },
    { "trigger_max_sol": 5.0,  "unconfirmed_sl_pct": 0.012, "confirmed_sl_pct": 0.015 }
]
```

These are validated as correct by the empirical data.

### 4.2 Future config additions (P2):

If implementing the optional 2-tier unconfirmed SL:
```json
"mev": {
    "unconfirmed_sl_early_ms": 100,
    "unconfirmed_sl_early_multiplier": 1.5
}
```

This would multiply the `unconfirmed_sl_pct` by 1.5 during the first 100ms, then revert to the configured value. **Defer to P2 — not needed for V1.**

---

## Section 5: Implementation Priority

### P0 — Fix Before Next Run

| # | Issue | What | Est. Effort |
|---|-------|------|-------------|
| P0-1 | C1: tier_index storage | Replace `_pad[0]` with `tier_index: u8`, update `on_entry` and `_confirmed_sl_pct` | 20 min |

### P1 — This Session

| # | Issue | What | Est. Effort |
|---|-------|------|-------------|
| P1-1 | C2: price guard on confirmation | Add `current_vsol: f64` param to `on_buy_event`, guard against underwater confirmation | 30 min |
| P1-2 | I1: precompute trail constants | Add `trail_keep_mult` and `trail_activation_mult` to ExitConfig, eliminate hot-path divisions | 15 min |
| P1-3 | I5: `#[inline(always)]` on helpers | Add to `_apply_conviction_tp` and `_confirmed_sl_pct` | 5 min |
| P1-4 | I6: debug_assert entry_vsol > 0 | One-line defensive check | 2 min |
| P1-5 | Update tests | Update test_buy_event_unconfirmed_to_confirmed and conviction tests to pass `current_vsol` | 15 min |

### P2 — Next Session

| # | Issue | What | Est. Effort |
|---|-------|------|-------------|
| P2-1 | I3: Vec allocation in on_tick | Replace `Vec::new()` with `ArrayVec<8>` or stack array in positions.rs | 20 min |
| P2-2 | M1: simplify ExitState enum | Remove level payload from ConvictionScaled, use struct field everywhere | 15 min |
| P2-3 | Optional 2-tier unconfirmed SL | Time-gated early/late SL during unconfirmed window | 45 min |
| P2-4 | SL data collection | Add JSONL field `sl_time_since_entry_ms` to measure when SL fires relative to entry, validate the 100ms boundary hypothesis | 15 min |

---

## Appendix: Integration Concerns (Section D answers)

### D1: Should on_buy_event only count post-confirmation buys?

**Answer:** "Any buy for this token after entry time" is sufficient. Here's why:

- Our entry is a simulated buy in the bonding curve. We don't wait for on-chain confirmation of OUR transaction — we optimistically track the position.
- Any buy event for the same mint that arrives after our `entry_ts_ms` is a signal of continued interest, regardless of whether our transaction has landed.
- If our transaction FAILS (execution failure), the position is cleaned up by the safety timer or by the execution layer, not by the exit state machine.

**However:** The trigger event itself MUST be skipped (already handled via `trigger_sig` check in `on_subsequent_trade`). ✅

### D2: Safety timer — tokio::spawn per position at scale?

**Answer:** Not a concern. 

- `max_concurrent_positions` in canary.json = 5 (config shows `"max_positions": 1` in risk, `"max_concurrent_positions": 5` in mev section)
- Even at 50 concurrent positions, 50 tokio tasks sleeping on a timer is negligible — tokio's timer wheel handles millions of entries
- The real concern is the `on_tick` method creating Vecs and iterating positions (addressed in I3)

### D3: Is on_price_tick called on EVERY trade event?

**Answer:** Yes — `on_subsequent_trade` in positions.rs calls `on_price_tick` for every trade event (buy AND sell) after updating reserves. This is correct because:

- Sell events move price DOWN — we need SL checks on sells
- Buy events move price UP — we need TP checks on buys  
- The state machine also receives buy events via `on_buy_event` for confirmation/conviction, THEN the same trade's price is fed via `on_price_tick`

This means a buy event causes TWO state machine calls: `on_buy_event` + `on_price_tick`. This is correct — the buy event handles state transitions, the price tick handles exit level checks. They're semantically different.

**Edge case:** If `on_buy_event` transitions to Confirmed AND the same price tick triggers TP (because the confirmed TP is lower than the unconfirmed TP... wait, confirmed TP is HIGHER). So this can't happen. Confirmed TP ≥ Unconfirmed TP by design. ✅

### E: SL During Confirmation Window — Definitive Answer

**KEEP SL active during UNCONFIRMED state. The data proves it's working correctly.**

- SL exits during unconfirmed: these are positions where no buy arrived AND price dropped >1%. The 0% WR proves they're correctly identified losers.
- If we removed SL during unconfirmed, these positions would instead exit via `MomentumDecayFlat` at 200ms — but by then they may have dropped 3-5%, increasing our loss per dead position.
- The 1.0% unconfirmed SL saves us ~0.5-2.0% per dead position versus waiting for the confirmation window to expire.
- The avgMFE of 0.526% on SL exits means they briefly touched +0.5% before reversing — this is consistent with bonding curve noise from our own buy impact (we pushed price up, then it reverted with no follow-through).

**Verdict: SL during unconfirmed is a critical risk management feature. Do not remove.**
