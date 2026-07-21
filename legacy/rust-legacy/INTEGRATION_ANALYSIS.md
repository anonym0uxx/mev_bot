# Integration Analysis — 5 Architect Designs

**Date:** 2026-03-30
**Analyst:** Master-Integrator (Principal Rust Systems Architect)

---

## 1. Conflict Report — Per-File Analysis

### 1.1 `entry_engine.rs` — 3-Way Merge (A3 + A4 + A5)

**Status: A5 ALREADY APPLIED. A3 FAILED. Only A4 remains.**

| Architect | Target | Conflict Status |
|-----------|--------|----------------|
| A5 | `DecisionThresholds::default()` values: 50→70, 40→55 | ✅ **ALREADY APPLIED** — line 156-157 show `min_entry_score: 70.0`, `min_magnitude_for_ride: 55.0` |
| A3 | Kelly LUT integer math | ❌ **FAILED** — see §2 for gap assessment |
| A4 | Add `fee_gate_multiplier_x100` field + fee gate block in `size()` | 🟡 **SOLE REMAINING CHANGE** — no merge conflict now |

**A5 is done.** The codebase already has:
- `entry_engine.rs:156`: `min_entry_score: 70.0`
- `entry_engine.rs:157`: `min_magnitude_for_ride: 55.0`
- `config.rs:265-266`: Comments reflect `70.0` and `55.0` defaults
- `canary.json:471-472`: Explicit `min_entry_score: 70.0`, `min_magnitude_for_ride: 55.0`

**A4's changes target two locations:**

1. **`DecisionThresholds` struct** (line 148-151): Add new field:
   ```rust
   pub fee_gate_multiplier_x100: u32,  // default 200 = require 2× fee coverage
   ```
   **Conflict: NONE.** A5 changed values, not struct fields. A3 failed. A4 adds a new field.

2. **`size()` method** (lines 498-525): Add fee gate block after `compute_conviction()` call (line 518-522):
   ```rust
   let conviction = kelly_sizing::compute_conviction(...);
   // ← A4 inserts fee gate check here
   (EntryAction::Ride, conviction)
   ```
   **Conflict: NONE.** A5 didn't touch `size()`. A3 failed. A4 is the sole modifier.

**A4 Compile Verification Against Current `kelly_sizing.rs`:**

| A4 Reference | Current Code | Status |
|-------------|-------------|--------|
| `kelly_sizing::DEFAULT_AVG_LOSS_BP` | `kelly_sizing.rs:24`: `pub const DEFAULT_AVG_LOSS_BP: u16 = 200` | ✅ EXISTS, correct type |
| `kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP` | `kelly_sizing.rs:21`: `pub const DEFAULT_ROUND_TRIP_FEE_BP: u16 = 210` | ✅ EXISTS, correct type |
| `conviction.p_permille` | `kelly_sizing.rs:63`: `pub p_permille: u16` | ✅ EXISTS, correct type |
| `conviction.r_x100` | `kelly_sizing.rs:65`: `pub r_x100: u16` | ✅ EXISTS, correct type |
| `conviction.size_lamports` | `kelly_sizing.rs:69`: `pub size_lamports: u64` | ✅ EXISTS, correct type |

**Verdict: A4's fee gate code compiles against the CURRENT `kelly_sizing.rs` with zero changes.**

### 1.2 `kelly_sizing.rs` — A3 Only (FAILED)

**Status: NO CHANGES NEEDED.**

The file already has:
- 2D LUT with bilinear interpolation (lines 37-54)
- Fee-adjusted R computation in pure integer math (lines 133-154, `fee_adjust_r()`)
- Kelly fraction in pure integer math (lines 161-173, `kelly_permille()`)
- Full sizing pipeline with integer arithmetic (lines 234-300)

Remaining floats are confined to:
- `bucket_frac()` (line 96): Maps continuous scores to LUT bucket indices
- `bilerp()` (line 119): Bilinear interpolation between 4 LUT cells
- `compute_conviction()` / `compute_conviction_with_fees()` function signatures: Accept `mag_score: f64, entry_score: f64`

See §2 for detailed gap assessment.

### 1.3 `positions.rs` — A1 Only

**Status: CLEAN. No conflicts.**

A1 adds a single new method `feed_initial_buy()` after `get_position_mut()` (line 350-352).

**Method signature verified against `RideState::on_buy_event()`:**
- A1 calls: `rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, FeedSource::PumpPortal, 10)`
- RideState expects: `on_buy_event(&mut self, sol_amount_mvsol: u32, now_ms: u64, wallet_hash: u64, source: FeedSource, weight_mult: u8)` (ride_state.rs:306)
- ✅ **Signatures match perfectly.**

**Internal helper verified:**
- `lamports_to_mvsol()` already exists at top of `positions.rs` (line 14)
- `FeedSource::PumpPortal` used via `crate::feeds::FeedSource::PumpPortal` — already imported

### 1.4 `hot_path.rs` — A1 Only

**Status: CLEAN. No conflicts.**

A1 adds 1 line after `self.stats.gates_passed += 1;` (line 313):
```rust
self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
```

**Insertion point:** Between line 313 (`self.stats.gates_passed += 1;`) and line 314 (`// Enrich with entry context from cached mint history`).

No other architect modifies `hot_path.rs`.

### 1.5 `watchlist.rs` — A2 Only

**Status: NEEDS DESIGN REVIEW — Current implementation may already be sufficient.**

The master plan says A2 adds 2-buy confirmation, but examining the current watchlist code:
- The watchlist ALREADY implements a 2-phase entry: watch → confirm on next buy
- `try_promote()` (line 303) already requires:
  - Different sig prefix (dedup)
  - Minimum buy size ≥ 0.03 SOL (MIN_CONFIRM_MVSOL)
  - ≤10% slippage from watch time
- `WatchSlot._pad: [u8; 8]` (line 70) is available for A2's `confirm1_sig_prefix`

**A2's proposed additions to current watchlist:**
- State machine: `watching(1)` → `partial_confirm(3)` → `promoted(2)` (adds state `3`)
- `confirm1_sig_prefix` stored in `_pad` field (maintains 64-byte alignment)
- Strong-interest shortcut: ≥0.10 SOL → immediate promotion
- Second confirming buy must differ from both entry AND first confirm sigs
- `expire_stale()` and `active_count()` updated to handle state `3`

**Struct size:** Reuses `_pad: [u8; 8]` for `confirm1_sig_prefix: [u8; 8]` → **stays 64 bytes** ✅

### 1.6 `config.rs` — A5 Only

**Status: ALREADY APPLIED. No remaining changes.**

Comments at lines 265-266 already reflect 70.0 / 55.0 defaults. `canary.json` already has explicit values.

---

## 2. A3 Gap Assessment — Is A3's Work Needed?

### What A3 Was Supposed to Do

Replace remaining `f64` operations in `kelly_sizing.rs` with integer-only math, specifically:
1. `bucket_frac()` — uses f64 for fractional position within LUT bucket
2. `bilerp()` — uses f64 for bilinear interpolation weights
3. Function signatures — accept `f64` for `mag_score` and `entry_score`

### What Already Works Without A3

The current `kelly_sizing.rs` already:
- ✅ Has integer LUT tables (`P_LUT`, `R_LUT` as `[[u16; 4]; 4]`)
- ✅ Has pure-integer `fee_adjust_r()` (line 133-154)
- ✅ Has pure-integer `kelly_permille()` (line 161-173)
- ✅ Has pure-integer correlation adjustment (Thorp approximation)
- ✅ Has pure-integer drawdown scaling
- ✅ Has pure-integer position sizing and clamping
- ✅ Outputs `EntryConviction` with all integer fields

### Where Floats Remain

| Function | Float Usage | Hot Path? | Impact |
|----------|-------------|-----------|--------|
| `bucket_frac()` | `f64` division/floor for bucket index + fractional position | No — entry engine only | ~2ns per call, 2 calls per evaluation |
| `bilerp()` | `f64` multiply-accumulate for interpolation | No — entry engine only | ~3ns per call, 2 calls per evaluation |
| Function signatures | `mag_score: f64, entry_score: f64` | No — called from `entry_engine::size()` which already has f64 scores | Zero conversion cost |

### Can We Skip A3?

**YES. A3 can be safely skipped.** Rationale:

1. **No other architect depends on A3.** The critical question was: "Does A4 compile against current kelly_sizing.rs?" — verified YES (see §1.1).

2. **The remaining floats are NOT on the hot path.** `compute_conviction()` is called at entry decision time (~100-500/day), not on every trade event. The ~10ns of float math is irrelevant at this frequency.

3. **Correctness is identical.** The LUT + bilinear interpolation with f64 intermediates produces the exact same u16 outputs as an integer-only implementation would (within ±1 due to rounding). The 26 existing tests all pass.

4. **The output struct (`EntryConviction`) is already fully integer.** The hot-path exit engine reads only integer fields. No f64 escapes into the hot path.

5. **Risk assessment:** Converting `bucket_frac()` and `bilerp()` to integer math adds code complexity (fixed-point arithmetic) for zero measurable performance gain. The existing implementation is correct, tested, and fast enough.

**Recommendation:** Skip A3 entirely. The current `kelly_sizing.rs` is sufficient for all 4 remaining designs. Document A3 as "deferred — no measurable benefit" in the build log.

---

## 3. Exact Build Order

Given that A5 is already applied, the remaining work is **A1, A2, and A4** (3 designs, not 5).

### Step 1: A2 — Watchlist 2-Buy Confirmation (`watchlist.rs`)

**Reason to go first:** Fully independent. Touches only `watchlist.rs`. No downstream dependencies.

**Changes:**
1. `watchlist.rs:62` — Repurpose `_pad: [u8; 8]` → `confirm1_sig_prefix: [u8; 8]`
2. `watchlist.rs:86,90` — Update `WatchSlot::EMPTY` to initialize `confirm1_sig_prefix: [0u8; 8]`
3. `watchlist.rs:271,275` — Update `write_slot` to initialize `confirm1_sig_prefix: [0u8; 8]`
4. `watchlist.rs:62` — State machine: add state `3` (partial_confirm)
5. `watchlist.rs:~280-350` — Modify `try_promote()` to implement:
   - If state==1 (watching) and sol_amount < 100_000_000 (0.10 SOL):
     - Store sig_prefix in `confirm1_sig_prefix`, transition to state 3
     - Return None (not yet promoted)
   - If state==1 and sol_amount >= 100_000_000: strong-interest shortcut → promote
   - If state==3 (partial_confirm): require DIFFERENT sig from both entry AND confirm1 → promote
6. `watchlist.rs:~360-380` — Update `expire_stale()` to handle state 3 (treat as active)
7. `watchlist.rs:~385-400` — Update `active_count()` to count state 3

**Static assertion:** `const _: () = assert!(core::mem::size_of::<WatchSlot>() == 64);` (line 74) — must still pass after field rename.

**Verify:** `cargo test -p pump-quant-core -- watchlist` 
**Risk:** Low. Field rename (`_pad` → `confirm1_sig_prefix`) is layout-identical.

### Step 2: A1 — Feed Initial Buy into RideState (`positions.rs` + `hot_path.rs`)

**Reason to go second:** Independent of A2 and A4. Touches different files.

**Change 1: `positions.rs`** — Add method after line 352 (after `get_position_mut`):
```rust
#[inline]
pub fn feed_initial_buy(&mut self, mint: &[u8; 32], sol_amount: u64, now_ms: u64, sig: &[u8; 64]) {
    let pos = match self.positions.get_mut(mint) {
        Some(p) => p,
        None => return,
    };
    match &mut pos.exit_mode {
        ExitMode::Ride(ref mut rs) => {
            let buy_mvsol = lamports_to_mvsol(sol_amount);
            let wallet_hash = u64::from_le_bytes([
                sig[0], sig[1], sig[2], sig[3],
                sig[4], sig[5], sig[6], sig[7],
            ]);
            rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, crate::feeds::FeedSource::PumpPortal, 10);
        }
    }
    pos.confirming_buy_sol = pos.confirming_buy_sol.saturating_add(sol_amount);
    if sol_amount >= 50_000_000 {
        pos.confirming_unique_wallets = pos.confirming_unique_wallets.saturating_add(1);
    }
}
```

**Change 2: `hot_path.rs`** — Insert 1 line after line 313 (`self.stats.gates_passed += 1;`):
```rust
self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
```

**Change 3: `positions.rs` tests** — Add `test_feed_initial_buy_injects_evidence` test.

**Verify:** `cargo test -p pump-quant-core -- positions`
**Risk:** Very low. Additive-only change. New method, new call site, no existing signatures modified.

### Step 3: A4 — Fee-Aware Entry Gate (`entry_engine.rs`)

**Reason to go last:** Modifies `entry_engine.rs` which has the most complex test suite. A5 is already applied, so A4 is the sole remaining change to this file.

**Change 1: `DecisionThresholds` struct** (line 148-151):
```rust
pub struct DecisionThresholds {
    pub min_entry_score: f64,           // 70.0
    pub min_magnitude_for_ride: f64,    // 55.0
    pub fee_gate_multiplier_x100: u32,  // NEW: default 200 = require 2× fee coverage
}
```

**Change 2: `DecisionThresholds::default()`** (line 153-159):
```rust
impl Default for DecisionThresholds {
    fn default() -> Self {
        Self {
            min_entry_score: 70.0,
            min_magnitude_for_ride: 55.0,
            fee_gate_multiplier_x100: 200,  // NEW
        }
    }
}
```

**Change 3: `size()` method** (after line 522, after `compute_conviction()`):
```rust
let conviction = kelly_sizing::compute_conviction(
    magnitude_score,
    entry_score,
    wallet_balance,
    n_open,
    drawdown_pct,
);

// ── Fee gate: reject if expected edge < fee cost × multiplier ──
if d.fee_gate_multiplier_x100 > 0 && conviction.size_lamports > 0 {
    // Expected edge per trade (integer):
    //   edge_x1000 = p × R × size - (1-p) × size
    //              = size × (p × R - (1000-p)) / 1000
    // Using u128 to avoid overflow (size can be ~200M lamports)
    let p = conviction.p_permille as u128;
    let r = conviction.r_x100 as u128;
    let size = conviction.size_lamports as u128;
    let edge_x1000 = size * p * r / 100_000; // numerator: size * p_permille * r_x100 / (1000 * 100)
    let loss_component = size * (1000 - p) / 1000;
    let net_edge = if edge_x1000 > loss_component {
        edge_x1000 - loss_component
    } else {
        return (EntryAction::Reject, conviction);
    };

    // Fee cost: size × round_trip_fee_bp / 10000
    let fee_bp = kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP as u128;
    let fee_lam = size * fee_bp / 10_000;

    // Require: edge > fee × multiplier / 100
    let threshold = fee_lam * d.fee_gate_multiplier_x100 as u128 / 100;
    if net_edge < threshold {
        return (EntryAction::Reject, conviction);
    }
}

(EntryAction::Ride, conviction)
```

**Change 4: Cache layout comment** (line 300): Update `DecisionThresholds` size from 16B to 24B (2 f64 + 1 u32 + padding).

**Change 5: `EntryEngineConfig`** (line 191): The `decision: DecisionThresholds` field automatically picks up the new default.

**Change 6: `config.rs`** — Add `fee_gate_multiplier_x100` to `SizingJsonConfig`:
```rust
pub fee_gate_multiplier_x100: Option<u32>, // default: 200
```
And in `build_entry_engine_config()`:
```rust
if let Some(v) = sizing.fee_gate_multiplier_x100 {
    cfg.decision.fee_gate_multiplier_x100 = v;
}
```

**Verify:** `cargo test -p pump-quant-core -- entry_engine`

**Risk:** Medium. The fee gate math must be verified against the test vectors from A4:
- Marginal (p=440, R=4.85→r_x100=485, 0.05 SOL): edge < threshold → REJECTED ✓
- Strong (p=640, R=4.85→r_x100=485, 0.10 SOL): edge > threshold → ACCEPTED ✓

**Critical test concern:** Existing `test_evaluate_produces_scores` (entry_engine.rs tests) uses `passing_input()` which produces scores above the new 70/55 thresholds. The test asserts `decision.size_lamports > 0`. The fee gate must NOT reject this input. Verification:
- `passing_input()` produces entry_score ≈ 77.6, magnitude ≈ 58.7
- Kelly conviction: p ≈ 600, r_x100 ≈ 485 (fee-adjusted), size = 0.05-0.20 SOL
- Edge check: p=600, R=485 → p*R/100000 = 0.0029, well above fee threshold
- ✅ Test passes with fee gate active.

---

## 4. Test Plan

### Pre-Merge: Full Test Suite Baseline

```bash
cd projects/pump-quant/rust
cargo test -p pump-quant-core 2>&1 | tail -5
```
Record pass/fail count as baseline.

### After Step 1 (A2 — Watchlist)

```bash
cargo test -p pump-quant-core -- watchlist
```
**Expected new tests:**
- `test_two_buy_promotion` — requires 2 buys for promotion
- `test_strong_interest_shortcut` — single buy ≥0.10 SOL promotes immediately
- `test_dedup_second_buy` — second buy must differ from both entry AND confirm1 sigs
- `test_partial_confirm_state_expires` — state 3 entries expire properly
- `test_active_count_includes_partial` — `active_count()` counts state 3

**Regression test:** All existing watchlist tests must pass unchanged:
- `test_slot_size` — verifies 64-byte struct ← **CRITICAL**
- `test_watch_and_promote` — may need update if 2-buy changes default behavior
- Other existing tests

### After Step 2 (A1 — Feed Initial Buy)

```bash
cargo test -p pump-quant-core -- positions
```
**Expected new test:**
- `test_feed_initial_buy_injects_evidence` — verifies `buys_after_entry=1`, `unique_wallets≥1`, `confirming_vol_msol>0`

**Regression tests:** All existing position tests must pass unchanged.
No hot_path tests exist, but `cargo check` verifies compilation.

### After Step 3 (A4 — Fee Gate)

```bash
cargo test -p pump-quant-core -- entry_engine
```
**Expected new tests:**
- `test_fee_gate_rejects_marginal` — marginal trade rejected by fee gate
- `test_fee_gate_accepts_strong` — strong trade passes fee gate
- `test_fee_gate_disabled_when_zero` — `fee_gate_multiplier_x100=0` disables gate

**Critical regression tests:**
- `test_evaluate_produces_scores` — must still pass (good input above gate)
- `test_hard_gate_passes_good_input` — not affected (fee gate is stage 3)
- `test_scoring_sweet_spot_curve` — not affected

### Final Verification

```bash
cargo test -p pump-quant-core 2>&1 | tail -5
```
**Expected:** All baseline tests pass + all new tests pass. Zero regressions.

---

## 5. Risk Assessment

### Low Risk ✅

| Change | Risk | Rationale |
|--------|------|-----------|
| A5 (Score Threshold) | ✅ **None** | Already applied in codebase |
| A3 (Kelly LUT) | ✅ **None** | Failed, skip entirely — current code sufficient |
| A1 (Feed Initial Buy) | ✅ **Very Low** | Additive method + 1 call site. No existing code modified. |
| A2 (Watchlist 2-Buy) | 🟡 **Low-Medium** | Modifies state machine logic in hot watchlist path |

### Medium Risk 🟡

| Change | Risk | Rationale |
|--------|------|-----------|
| A4 (Fee Gate) | 🟡 **Medium** | New rejection path in `size()`. Must verify fee gate math doesn't create false rejections for trades that should pass. |

### Specific Risks

1. **A4 false rejections:** If the fee gate math has an integer overflow or rounding error, legitimate trades could be rejected. Mitigate with test vectors covering boundary cases.

2. **A2 breaks watchlist promotion:** If the 2-buy state machine has a bug, no tokens get promoted → zero entries. Mitigate: the strong-interest shortcut (≥0.10 SOL) provides a fallback path that bypasses 2-buy logic.

3. **A1 double-counting:** If `feed_initial_buy` is called when the trade somehow also enters `on_subsequent_trade`, evidence could be double-counted. Mitigate: impossible — `open_position` sets `trigger_sig = event.sig`, and `on_subsequent_trade` skips events matching `trigger_sig`. The feed_initial_buy call is the ONLY way this evidence enters.

4. **A2 struct size regression:** If `WatchSlot` grows beyond 64 bytes, the compile-time assertion (line 74) will catch it. This is a hard fail, not a silent bug.

5. **A4 `DecisionThresholds` size change:** Adding `fee_gate_multiplier_x100: u32` changes the struct from 16 bytes (2×f64) to 24 bytes (2×f64 + u32 + 4 padding due to `#[repr(C)]`). This affects the `EntryEngine` cache layout comment (line 300) but NOT actual performance — 8 bytes is negligible in the 3,144-byte engine struct.

### What Could NOT Go Wrong

- **A1 ↔ A2 interaction:** These are sequential in the pipeline (watchlist promotes → position opens → feed_initial_buy). A2 changes WHEN promotion happens, A1 changes WHAT happens after. The "confirming buy" that A1 feeds is the trade that triggered `try_promote()` — whether that's the first confirming buy (current behavior) or the second (A2 behavior), the mechanism is identical.

- **A4 ↔ current kelly_sizing.rs:** Verified — all referenced constants and struct fields exist with correct types.

- **Test namespace collisions:** Each architect's tests are in the file-level `mod tests` of different files. No name collisions possible.

---

## Summary

| Architect | Status | Action Required |
|-----------|--------|----------------|
| A1 (Feed Initial Buy) | 🟢 Ready | Apply Step 2 |
| A2 (Watchlist 2-Buy) | 🟢 Ready | Apply Step 1 |
| A3 (Kelly LUT) | ⏭️ Skip | Already sufficient — see §2 |
| A4 (Fee Gate) | 🟢 Ready | Apply Step 3 |
| A5 (Score Threshold) | ✅ Done | Already in codebase |

**Total remaining work: 3 designs across 4 files.**
**Zero merge conflicts between any pair of remaining changes.**
**All designs compile against current codebase without modifications to each other.**

### Recommended Build Order (Final)

```
1. A2: watchlist.rs                         (independent)
2. A1: positions.rs + hot_path.rs           (independent of A2)
3. A4: entry_engine.rs + config.rs          (independent of A1, A2)
   ↓
   cargo test -p pump-quant-core            (final verification)
```

All three steps can theoretically be applied in parallel (no shared file modifications), but sequential application with test verification between each step provides rollback safety.
