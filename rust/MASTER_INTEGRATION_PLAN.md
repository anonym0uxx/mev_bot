# Master Integration Plan — 5 Architect Designs

## Overview

Five parallel architect subagents designed changes to the Pump.fun quant bot's Rust engine. This document compiles all 5 designs and identifies integration concerns, ordering, and conflicts.

## Files Touched Summary

| Architect | Files Modified | Files Created |
|-----------|---------------|---------------|
| A1: Exit Confirm | `positions.rs`, `hot_path.rs` | — |
| A2: Watchlist 2-Buy | `watchlist.rs` | — |
| A3: Kelly LUT | `kelly_sizing.rs`, `entry_engine.rs` | — |
| A4: Fee Gate | `entry_engine.rs` | — |
| A5: Score Threshold | `entry_engine.rs`, `config.rs`, `canary.json` | — |

### Conflict Matrix

| File | Architects | Conflict? |
|------|-----------|-----------|
| `entry_engine.rs` | A3, A4, A5 | **YES — all three touch DecisionThresholds and/or size()** |
| `kelly_sizing.rs` | A3 | No conflict |
| `positions.rs` | A1 | No conflict |
| `hot_path.rs` | A1 | No conflict |
| `watchlist.rs` | A2 | No conflict |
| `config.rs` | A5 | No conflict |
| `canary.json` | A5 | No conflict |

---

## Design 1: Exit Confirm — Feed Initial Buy into RideState (A1)

### Problem
When a watchlist entry is promoted, the confirming buy becomes `trigger_sig` in `open_position()`. Since `on_subsequent_trade` skips events matching `trigger_sig`, the confirming buy's evidence is NEVER injected into the Bayesian model. Result: `buys_after_entry=0`, empty bloom/ring, faster decay, premature exits.

Note: `ExitStateMachine` is DEAD CODE. The live exit path is `RideState` exclusively.

### Changes
1. **`positions.rs`** — New method `PositionManager::feed_initial_buy()` (~25 lines)
   - Takes mint, sol_amount, now_ms, sig
   - Gets mutable ref to position, extracts RideState
   - Calls `rs.on_buy_event(buy_mvsol, now_ms, wallet_hash, FeedSource::PumpPortal, 10)`
   - Updates `pos.confirming_buy_sol` and `pos.confirming_unique_wallets`

2. **`hot_path.rs`** — 1 line added after `open_position()` in step 2b:
   ```rust
   self.position_manager.feed_initial_buy(&trade.mint, trade.sol_amount, now, &trade.sig);
   ```

### Test
- Verifies buys_after_entry goes 0→1, unique_wallets ≥1, confirming_vol_msol > 0 after call

---

## Design 2: Watchlist 2-Buy Confirmation (A2)

### Problem
Single-buy watchlist promotion lets noise through. Require 2 distinct confirming buys (or 1 large buy ≥0.10 SOL) before promotion.

### Changes
**`watchlist.rs`** — WatchlistEntry struct and logic:
- Reuses existing 8-byte `_pad` field for `confirm1_sig_prefix: [u8; 8]` — struct stays 64 bytes
- New state machine: `watching(1)` → first buy → `partial_confirm(3)` → second buy → `promoted(2)`
- Strong-interest shortcut: single buy ≥0.10 SOL → immediate promotion
- Dedup: second confirming buy must have different sig prefix from both entry trade AND first confirm
- `expire_stale()` and `active_count()` updated to handle state 3
- Extracted `finalize_promote()` helper for code reuse

### Test
- Multi-buy promotion path
- Strong-interest shortcut
- Dedup rejection (same sig prefix)

---

## Design 3: Kelly LUT — Integer Lookup Tables (A3)

### ⚠️ PENDING — Architect-3 still running at time of document creation

**Expected changes:**
- `kelly_sizing.rs` — Replace floating-point Kelly fraction computation with integer LUT
- `entry_engine.rs` — Possibly update how conviction is consumed (LUT output format)
- Expected to export constants like `DEFAULT_AVG_LOSS_BP` and `DEFAULT_ROUND_TRIP_FEE_BP` that A4 depends on

**Integration concern:** A4's fee gate code references `kelly_sizing::DEFAULT_AVG_LOSS_BP` and `kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP`. If A3 renames or restructures these constants, A4 needs adjustment.

---

## Design 4: Fee-Aware Entry Gate (A4)

### Problem
Kelly can produce non-zero `f_permille` for low-edge trades. Min size clamp forces 0.05 SOL positions. No check that expected edge exceeds fee cost.

### Changes
1. **`entry_engine.rs` — `DecisionThresholds`** — New field:
   ```rust
   pub fee_gate_multiplier_x100: u32,  // default 200 = require 2× fee coverage
   ```

2. **`entry_engine.rs` — `size()` method** — New gate after `compute_conviction()`:
   - Computes `expected_edge_lamports` using p_permille, r_x100, size_lamports (all integer u128 math)
   - Computes `fee_lamports` from size × DEFAULT_ROUND_TRIP_FEE_BP
   - Rejects if `edge_x1000 < fee_lam × multiplier_x100 × 10`
   - Disabled when `fee_gate_multiplier_x100 = 0`

### Math (verified):
- Marginal (p=440, R=4.85, 0.05 SOL): edge 1.57B < threshold 2.1B → REJECTED ✓
- Strong (p=640, R=4.85, 0.10 SOL): edge 5.49B > threshold 4.2B → ACCEPTED ✓

### Dependencies
- References `kelly_sizing::DEFAULT_AVG_LOSS_BP` and `kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP`
- References `conviction.p_permille`, `conviction.r_x100`, `conviction.size_lamports`

---

## Design 5: Score Threshold Increase (A5)

### Problem
Old thresholds (50/40) let too many marginal trades through. Backtest shows trades scoring 50-70 had ~40.4% WR vs 43.4% average.

### Changes
1. **`entry_engine.rs` — `DecisionThresholds::default()`**:
   ```rust
   min_entry_score: 70.0,      // was 50.0
   min_magnitude_for_ride: 55.0, // was 40.0
   ```

2. **`config.rs`** — Updated comments to document new defaults

3. **`canary.json`** — Added explicit `entry_engine` section:
   ```json
   "entry_engine": {
       "position_sizing": {
           "min_entry_score": 70.0,
           "min_magnitude_for_ride": 55.0
       }
   }
   ```

### Test compatibility
- `passing_input()` produces entry ≈77.6, magnitude ≈58.7 — both pass new thresholds

---

## Integration Concerns & Ordering

### 1. DecisionThresholds Merge (A3 + A4 + A5)

All three touch `DecisionThresholds` in `entry_engine.rs`:
- **A5** changes default values for existing fields
- **A4** adds new field `fee_gate_multiplier_x100: u32`
- **A3** may add LUT-related fields or change how thresholds interact with sizing

**Resolution:** Merge all three into one unified struct definition. A5's value changes + A4's new field are non-conflicting. A3's changes TBD.

### 2. size() Method Ordering (A3 + A4)

Both A3 and A4 modify the `size()` method in `entry_engine.rs`:
- **A3** replaces `compute_conviction()` internals (or the call itself)
- **A4** adds fee gate AFTER conviction is computed

**Resolution:** A4's gate goes after whatever A3 does to conviction computation. The gate only reads `conviction.{p_permille, r_x100, size_lamports}` — it doesn't care how they were computed. Apply A3 first (conviction computation), then A4 (fee gate), then return.

### 3. Kelly Constants (A3 → A4 dependency)

A4 references `kelly_sizing::DEFAULT_AVG_LOSS_BP` and `kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP`. If A3 renames these, update A4's references.

### 4. Watchlist → Position Pipeline (A2 → A1)

A2 changes when promotion happens (2-buy confirmation). A1 changes what happens after promotion (feed initial buy). These are sequential in the pipeline — no conflict, but the "confirming buy" that A1 feeds into RideState is now specifically the SECOND confirming buy (or the single strong buy). This is correct — the second buy is the promotion trigger.

### Recommended Apply Order

1. **A2 (Watchlist 2-Buy)** — Independent, touches only `watchlist.rs`
2. **A1 (Exit Confirm)** — Independent, touches only `positions.rs` + `hot_path.rs`
3. **A3 (Kelly LUT)** — Touches `kelly_sizing.rs` + `entry_engine.rs` (conviction computation)
4. **A4 (Fee Gate)** — Depends on A3's constants, touches `entry_engine.rs` (post-conviction gate)
5. **A5 (Score Threshold)** — Simple value changes, apply last to avoid merge noise

### Build & Test Order

```bash
# After each step, run:
cargo check --lib  # fast compile check
cargo test         # full test suite

# Step 1: A2 (watchlist.rs)
# Step 2: A1 (positions.rs + hot_path.rs)  
# Step 3: A3 (kelly_sizing.rs + entry_engine.rs conviction)
# Step 4: A4 (entry_engine.rs fee gate — may need constant name fixup from A3)
# Step 5: A5 (entry_engine.rs thresholds + config.rs + canary.json)
# Final: cargo test -- --nocapture  (verify all new + existing tests pass)
```
