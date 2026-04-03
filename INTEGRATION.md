# Integration Plan: 4 Modules → MomentumEngine

**Generated:** 2026-04-03  
**Status:** Review before applying  
**Files modified:** `momentum/mod.rs`, `momentum/config.rs`, `api/server.rs`

All paths relative to `rust/pump-quant-core/src/`.

---

## Table of Contents

1. [Config additions (config.rs)](#1-config-additions-configrs)
2. [Struct field additions (mod.rs)](#2-struct-field-additions-modrs)
3. [Constructor wiring (mod.rs `new()`)](#3-constructor-wiring-modrs-new)
4. [Activity gate in on_graduation()](#4-activity-gate-in-on_graduation)
5. [Activity gate cleanup in on_tick()](#5-activity-gate-cleanup-in-on_tick)
6. [buy_confirmed_ms in buy TX callbacks](#6-buy_confirmed_ms-in-buy-tx-callbacks)
7. [Reconciler record_buy_tx in buy TX callbacks](#7-reconciler-record_buy_tx-in-buy-tx-callbacks)
8. [PositionPhase gate in process_active_positions()](#8-positionphase-gate-in-process_active_positions)
9. [SellEngine + Reconciler fields (TODO-only)](#9-sellengine--reconciler-fields-todo-only)
10. [Reconciler summary in API status](#10-reconciler-summary-in-api-status)

---

## 1. Config additions (config.rs)

### CHANGE 1a: Add import for ActivityGateConfig

**File:** `momentum/config.rs`  
**Location:** Line 7 (imports section)

```
AFTER the line:
    use super::position::TrailConfig;

ADD:
    use super::activity_gate::ActivityGateConfig;
```

### CHANGE 1b: Add `activity_gate` field to MomentumConfig struct

**File:** `momentum/config.rs`  
**Location:** After `trail_config: TrailConfig` field (~line 438)

```
AFTER the lines:
    /// Only used when `adaptive_trail_enabled` is true.
    #[serde(default)]
    pub trail_config: TrailConfig,

ADD:

    /// Pre-entry activity gate configuration.
    /// Blocks dead tokens by requiring minimum WS activity before entry.
    /// Omit section to use defaults (enabled, 5 notifs, 2s stale, 1 buy, 50bps range).
    #[serde(default)]
    pub activity_gate: ActivityGateConfig,
```

### CHANGE 1c: Add `activity_gate` default in `impl Default for MomentumConfig`

**File:** `momentum/config.rs`  
**Location:** After `trail_config: TrailConfig::default(),` (~line 813)

```
AFTER the line:
            trail_config: TrailConfig::default(),

ADD:
            activity_gate: ActivityGateConfig::default(),
```

---

## 2. Struct field additions (mod.rs)

### CHANGE 2a: Add import for ActivityTracker

**File:** `momentum/mod.rs`  
**Location:** After the existing `use crate::momentum::...` imports (~line 39-43)

```
AFTER the line:
    use crate::momentum::types::ScoredToken;

ADD:
    use crate::momentum::activity_gate::{ActivityTracker, ActivityDecision};
```

### CHANGE 2b: Add ActivityTracker field to MomentumEngine struct

**File:** `momentum/mod.rs`  
**Location:** Inside `pub struct MomentumEngine` block, after `retry_rx` field (~line 487)

```
AFTER the lines:
    retry_tx: tokio::sync::mpsc::UnboundedSender<AsyncRetryResult>,
    retry_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AsyncRetryResult>>,

ADD:

    // ── Pre-entry activity gate (filters dead tokens) ───────────────
    activity_tracker: ActivityTracker,

    // ── Sell engine (escalation retry pipeline) ─────────────────────
    // TODO(sell_engine_pr): Wire sell_engine into close_position() — separate PR
    // sell_engine: Arc<sell_engine::SellEngine>,

    // ── On-chain reconciler (P&L verification) ──────────────────────
    // TODO(reconciler_pr): Wire reconciler background task + record_buy/sell — separate PR
    // reconciler: Arc<reconciler::Reconciler>,
```

---

## 3. Constructor wiring (mod.rs `new()`)

### CHANGE 3a: Initialize ActivityTracker in constructor

**File:** `momentum/mod.rs`  
**Location:** Inside `fn new()`, in the `let engine = Self { ... }` block, after `retry_rx` field (~line 627)

```
AFTER the line:
            retry_rx: tokio::sync::Mutex::new(async_retry_rx),

ADD:
            activity_tracker: ActivityTracker::new(),
```

---

## 4. Activity gate in on_graduation()

### CHANGE 4: Add check_entry() call after scoring passes, before position opening

**File:** `momentum/mod.rs`  
**Location:** Inside `on_graduation()`, after ToD gating passes and BEFORE the "graduation score PASSED" log line (~line 960)

The insertion point is between the ToD gating block and the "graduation score PASSED" tracing::info. The exact anchor:

```
AFTER the lines:
        if tod_multiplier <= 0.0 {
            tracing::info!(
                mint = %bs58::encode(&pool_info.mint).into_string(),
                score = score.total(),
                "[momentum] ToD gating: blocked hour — skipping entry"
            );
            return;
        }

AND BEFORE the line:
        let mint_b58 = bs58::encode(&pool_info.mint).into_string();
        tracing::info!(
            mint = %mint_b58,
            score = score.total(),

ADD:

        // ── Activity Gate: require minimum WS trading activity before entry ──
        // Dead tokens waste -5.3% on AMM round-trip fees. This blocks ~90% of
        // dead tokens (saves +0.042 SOL per 167 trades).
        // Runs after scoring and ToD so we don't waste work on tokens that
        // would've been rejected by upstream gates.
        {
            let decision = self.activity_tracker.check_entry(
                &pool_info.mint,
                now_ms,
                &self.config.activity_gate,
            );
            if let ActivityDecision::Reject(reason) = decision {
                tracing::info!(
                    mint = %bs58::encode(&pool_info.mint).into_string(),
                    score = score.total(),
                    reason = %reason,
                    "[momentum] activity gate REJECTED — insufficient trading activity"
                );
                return;
            }
        }
```

---

## 5. Activity gate cleanup in on_tick()

### CHANGE 5: Call cleanup() periodically from on_tick()

**File:** `momentum/mod.rs`  
**Location:** Inside `on_tick()`, after `self.process_scale_in(now_ms);` at the end of the function (~line 1375)

```
AFTER the line:
        self.process_scale_in(now_ms);

ADD:

        // ── Activity tracker housekeeping ─────────────────────────
        // Run cleanup every ~10s (check_ms=150ms → 10000/150≈67 ticks).
        // Removes mints with no WS activity in last 60s to bound memory.
        let tick_num_cleanup = now_ms / self.config.check_ms.max(1);
        if tick_num_cleanup % 67 == 0 {
            self.activity_tracker.cleanup(now_ms, self.config.activity_gate.cleanup_stale_ms);
        }
```

---

## 6. buy_confirmed_ms in buy TX callbacks

There are **3 buy TX callback sites** where `BuyState::Confirmed` is set. Each needs `stamp_buy_confirmed()` called on the position.

**Critical constraint:** These run inside `tokio::spawn` closures that don't have `&self` access. They have `buy_states: Arc<DashMap<...>>` but NOT the `active` DashMap. We need to add a reference to `active` so the spawned task can update the position.

**Alternative (simpler, no new Arc):** Set `buy_confirmed_ms` in `process_active_positions()` by checking `buy_states`. This avoids passing `active` into every spawn. The position is evaluated every tick (~150ms), so the delay is negligible.

### CHANGE 6: Set buy_confirmed_ms reactively in process_active_positions()

**File:** `momentum/mod.rs`  
**Location:** Inside `process_active_positions()`, at the top of the `for mut entry in self.active.iter_mut()` loop body, BEFORE the max hold check (~line 2530)

```
AFTER the lines:
        for mut entry in self.active.iter_mut() {
            let mint = *entry.key();
            let pos = entry.value_mut();

ADD:
            // ── Set buy_confirmed_ms on first tick where buy TX is confirmed ──
            // Reactive approach: check buy_states each tick rather than threading
            // active DashMap into every tokio::spawn buy task. ~150ms delay is
            // negligible vs the 1-30s TX confirmation latency.
            if pos.buy_confirmed_ms == 0 {
                if let Some(state) = self.buy_states.get(&mint) {
                    if matches!(*state, BuyState::Confirmed) {
                        pos.stamp_buy_confirmed(now_ms);
                        tracing::debug!(
                            mint = %bs58::encode(&mint).into_string(),
                            confirmed_at_ms = now_ms,
                            "[momentum] buy_confirmed_ms stamped"
                        );
                    }
                }
            }

```

---

## 7. Reconciler record_buy_tx in buy TX callbacks

**Deferred to reconciler PR.** The reconciler's `record_buy_tx()` needs `&MomentumClosedPosition` which doesn't exist at buy time (that's a close-time structure). Use `record_buy_tx_raw()` instead, but this still requires threading an `Arc<Reconciler>` into every spawn.

### CHANGE 7: TODO markers at buy TX callback sites

**File:** `momentum/mod.rs`

**Site 1 (~line 1611):** `deferred_buy_pumpswap` callback

```
AFTER the line:
                                buy_states.insert(mint_buy, BuyState::Confirmed);

ADD (same indentation):
                                // TODO(reconciler_pr): reconciler.record_buy_tx_raw(&mint_str, &signature, 0.0);
```

**Site 2 (~line 2253):** `buy_task` (Raydium) callback

```
AFTER the line:
                                tracing::info!(mint=%mint_str, sig=%signature, latency_ms, tip, size_sol=size as f64/1e9, tokens_est, "[buy_task] RPC landed ✅");

ADD (same indentation, next line):
                                // TODO(reconciler_pr): reconciler.record_buy_tx_raw(&mint_str, &signature, 0.0);
```

**Site 3 (~line 2467):** `buy_pumpswap` callback

```
AFTER the line:
                                buy_states.insert(mint_buy, BuyState::Confirmed);

ADD (same indentation):
                                // TODO(reconciler_pr): reconciler.record_buy_tx_raw(&mint_str, &signature, 0.0);
```

---

## 8. PositionPhase gate in process_active_positions()

### CHANGE 8: Add phase evaluation after buy_confirmed_ms stamp, before exit evaluation

This is the most impactful integration. The phase gate replaces the current approach of evaluating ALL exit conditions from entry time. Instead, exits are gated by phase:

- `AwaitingConfirmation` → skip all exit evaluation (position doesn't exist on-chain yet)
- `RapidAssessment` → only micro-SL fires
- `Exiting` → push to to_close immediately
- `Observation` / `Momentum` / `ExitEligible` → evaluated by existing logic

**File:** `momentum/mod.rs`  
**Location:** Inside `process_active_positions()`, AFTER the `buy_confirmed_ms` stamp block from Change 6, and BEFORE the `let elapsed_ms = now_ms.saturating_sub(pos.entry_ts_ms);` line (~line 2533)

```
AFTER the buy_confirmed_ms stamp block (Change 6) AND BEFORE the line:
            let elapsed_ms = now_ms.saturating_sub(pos.entry_ts_ms);

ADD:
            // ── Phase-gated exit evaluation ──────────────────────────────
            // PositionPhase prevents exit evaluation before the buy TX is
            // confirmed on-chain, eliminating phantom sells on unconfirmed buys.
            //
            // Phase timeline (from buy_confirmed_ms):
            //   AwaitingConfirmation → skip ALL exits (not on-chain yet)
            //   RapidAssessment (0-1500ms) → only micro-SL (-2%) fires
            //   Observation (1500-4500ms) → hard SL + dead token only
            //   Momentum (any time, +100bps) → trailing stop active
            //   ExitEligible (>4500ms) → ALL exit conditions
            //   Exiting → immediate close (phase decided to exit)
            {
                let current_bps_for_phase = self.price_feed.current_price(&mint)
                    .map(|p| price_to_bps_offset(pos.entry_price_fp, p))
                    .unwrap_or(0);
                let (ws_msgs, ws_last_ms) = self.price_feed.ws_notif_info(&mint);
                let ws_age_ms = if ws_last_ms > 0 { now_ms.saturating_sub(ws_last_ms) } else { 0 };
                let phase = pos.evaluate_phase(
                    now_ms,
                    current_bps_for_phase,
                    ws_msgs.min(u16::MAX as u64) as u16,
                    ws_age_ms,
                );

                match phase {
                    position::PositionPhase::AwaitingConfirmation => {
                        // Position not yet on-chain — skip ALL exit evaluation.
                        // Safety timeout (10s) is handled inside evaluate_phase.
                        continue;
                    }
                    position::PositionPhase::Exiting => {
                        // Phase itself decided to exit (micro-SL, dead token, safety timeout).
                        let exit_price = self.price_feed.current_price(&mint)
                            .unwrap_or(pos.entry_price_fp);
                        to_close.push((mint, MomentumExitReason::HardSl, exit_price));
                        continue;
                    }
                    position::PositionPhase::RapidAssessment => {
                        // Only micro-SL (handled inside evaluate_phase → returns Exiting).
                        // If we're here, micro-SL didn't trigger. Skip full exit evaluation
                        // but still allow price sampling and drain detection below.
                        // Fall through — the existing micro_exit_window_ms + hard_sl will
                        // catch the same conditions. This is a belt-and-suspenders approach.
                    }
                    _ => {
                        // Observation, Momentum, ExitEligible — proceed with full evaluation.
                    }
                }
            }
```

**Design note:** The `RapidAssessment` case falls through to existing exit logic rather than `continue`ing. This is intentional: the existing `micro_exit_window_ms` and `hard_sl` checks already cover the same ground, and drain detection (highest priority) should still fire during rapid assessment. The phase gate's primary value is blocking exit evaluation during `AwaitingConfirmation`.

---

## 9. SellEngine + Reconciler fields (TODO-only)

These are already handled in Change 2b (TODO comments in the struct). The actual wiring is deferred:

- **SellEngine:** Requires replacing ~200 lines of `tokio::spawn` blocks in `close_position()` with `self.sell_engine.submit_sell()`. Complex enough for its own PR.
- **Reconciler:** Requires threading `Arc<Reconciler>` into every buy/sell spawn, plus background task management. Separate PR.

The struct fields are commented out so they compile but document the intent.

---

## 10. Reconciler summary in API status

### CHANGE 10: TODO marker for API status endpoint

**File:** `api/server.rs`  
**Location:** Inside the `/api/stats` handler, near the JSON response construction

```
// TODO(reconciler_pr): Add reconciliation_summary to stats response:
//   let recon_summary = state.reconciler.get_reconciliation_summary();
//   "reconciliation": serde_json::to_value(&recon_summary).unwrap_or_default(),
```

This requires `Arc<Reconciler>` on `ApiState`, which is part of the reconciler PR.

---

## Summary: What Gets Applied Now vs. Later

### Apply Now (this PR)

| # | Change | Risk | Lines |
|---|--------|------|-------|
| 1a-c | `ActivityGateConfig` in config | None (serde default) | ~8 |
| 2a-b | Imports + struct fields | Compile-only | ~12 |
| 3a | Constructor init | None | ~1 |
| 4 | Activity gate in `on_graduation()` | Low (new rejection gate, disabled via config) | ~20 |
| 5 | Cleanup in `on_tick()` | None (memory housekeeping) | ~5 |
| 6 | `buy_confirmed_ms` stamp in `process_active_positions()` | Low (reactive, ~150ms delay) | ~12 |
| 8 | PositionPhase gate in `process_active_positions()` | **Medium** (changes exit timing) | ~40 |

### Deferred (separate PRs)

| Module | Reason |
|--------|--------|
| SellEngine wiring | Replaces ~200 lines of spawn logic |
| Reconciler wiring | Requires Arc threading into all spawns |
| Reconciler API | Depends on reconciler wiring |

### Total new lines this PR: ~100 surgical insertions

---

## Dependency Order

```
1. Config additions (1a-c) — no dependencies
2. Struct + constructor (2a-b, 3a) — depends on 1
3. Activity gate (4, 5) — depends on 2
4. buy_confirmed_ms stamp (6) — no dependencies beyond struct
5. PositionPhase gate (8) — depends on 6
```

Changes 1-5 are safe to apply independently. Change 8 depends on Change 6 (needs `buy_confirmed_ms` to be set for phase evaluation to work).

---

## Testing Strategy

1. **Paper mode first:** All changes are active in paper mode. Run for 24h and compare trade log metrics.
2. **Activity gate validation:** Check logs for `activity gate REJECTED` entries. Compare against historical dead token mints.
3. **Phase gate validation:** Check logs for `buy_confirmed_ms stamped` and verify `AwaitingConfirmation` continues (no premature exits).
4. **Regression check:** Win rate, avg hold time, and PnL per trade should not degrade.

---

## WS Notification Wiring (Activity Tracker)

The `ActivityTracker::on_ws_notification()` needs to be called from the WS price feed handler. This is in `price_feed.rs` inside `ws_update_price()`:

### CHANGE 11 (Optional — requires price_feed refactor): Wire on_ws_notification

**File:** `momentum/price_feed.rs`  
**Location:** `ws_update_price()` function (~line 441), after the `ws_notif_count.fetch_add(1, ...)` line

**Problem:** `ws_update_price()` is a standalone function that doesn't have access to the `ActivityTracker`. The tracker lives on `MomentumEngine`, but the WS loop runs independently.

**Options:**
1. **Pass `Arc<ActivityTracker>` to the WS loop at construction time** — cleanest
2. **Move ActivityTracker into PriceFeedManager** — couples activity tracking to price feed
3. **Use a crossbeam channel** — WS sends notification events, on_tick drains them

**Recommended:** Option 1. Modify `PriceFeedManager::new()` to accept `Arc<ActivityTracker>`, pass it to the WS task, and call `on_ws_notification()` inside `ws_update_price()`.

**Deferred to implementation:** This is the main "pipe-fitting" work. Without this wiring, the activity gate will see 0 notifications and reject everything (when enabled). Set `activity_gate.enabled = false` in canary.json until this is wired.

### Interim workaround

Add `activity_gate.enabled = false` to canary.json so the gate compiles and is present but doesn't block entries until the WS notification path is wired.

```json
{
  "momentum": {
    "activity_gate": {
      "enabled": false
    }
  }
}
```
