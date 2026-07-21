# Enforced Probe Hold Time — mod.rs Integration Guide

## Overview

This document describes the exact changes needed in `mod.rs` to integrate the
`PositionPhase` / `buy_confirmed_ms` system from `position.rs`.

There are **3 integration points**:

1. **Stamp `buy_confirmed_ms` when BuyState transitions to Confirmed**
2. **Phase gate at the top of `process_active_positions`**
3. **Map `PositionPhase::Exiting` to the correct exit reason**

---

## Integration Point 1: Stamp buy_confirmed_ms

### Location: `process_active_positions`, right after getting `current_price_fp`

The buy TX lands asynchronously. The `BuyState` DashMap tracks whether it's
Pending/Confirmed/Failed. On the first tick where `BuyState::Confirmed` and
`buy_confirmed_ms == 0`, stamp the confirmation time.

```rust
// In process_active_positions, right after:
//   let current_price_fp = match self.price_feed.current_price(&mint) { ... };
// ADD THIS BLOCK:

// ── Enforced Hold: Stamp buy confirmation time ────────────────────
// On the first tick after the async buy TX callback sets BuyState::Confirmed,
// record the confirmation timestamp. This is the reference point for all
// phase-gated exit evaluation.
if pos.buy_confirmed_ms == 0 {
    if let Some(state) = self.buy_states.get(&mint) {
        if matches!(*state, BuyState::Confirmed) {
            pos.stamp_buy_confirmed(now_ms);
            tracing::debug!(
                mint = %bs58::encode(&mint).into_string(),
                decision_ms = pos.entry_ts_ms,
                confirmed_ms = now_ms,
                latency_ms = now_ms.saturating_sub(pos.entry_ts_ms),
                "[momentum] buy TX confirmed on-chain — stamped buy_confirmed_ms"
            );
        }
    }
}
// ── End Enforced Hold stamp ───────────────────────────────────────
```

### Why here and not in the async callback?

The async buy task captures `buy_states: Arc<DashMap>` but does NOT have access
to `self.active` (which is a plain `DashMap`, not Arc). The cleanest integration
is to check `buy_states` on the next tick and stamp the position. This adds at
most one tick (~50ms) of delay, which is negligible compared to the 600-1200ms
TX propagation time.

---

## Integration Point 2: Phase Gate

### Location: `process_active_positions`, right after the buy_confirmed_ms stamp

This is the **core enforcement mechanism**. It evaluates `PositionPhase` and
gates ALL downstream exit evaluation.

```rust
// ── Enforced Hold: Phase gate ─────────────────────────────────────
// Evaluate position phase from on-chain confirmation time.
// This MUST run before any exit evaluation (TP, SL, trail, etc.)
let current_bps = price_to_bps_offset(pos.entry_price_fp, current_price_fp);
let (ws_count, ws_last_ms) = self.price_feed.ws_notif_info(&mint);
let ws_age_ms = if ws_last_ms > 0 {
    now_ms.saturating_sub(ws_last_ms)
} else {
    now_ms.saturating_sub(pos.entry_ts_ms) // fallback: age from decision
};

let phase = pos.evaluate_phase(
    now_ms,
    current_bps,
    ws_count.min(u16::MAX as u64) as u16,
    ws_age_ms,
);

match phase {
    PositionPhase::AwaitingConfirmation => {
        // Buy TX not yet confirmed. Skip ALL exit evaluation.
        // No price tracking, no TP, no SL. Position doesn't exist on-chain yet.
        continue;
    }
    PositionPhase::RapidAssessment => {
        // 0-1500ms post-confirmation. Micro-SL already handled inside
        // evaluate_phase (returns Exiting for -200 bps).
        // Only update peak price for future trailing stop reference.
        if current_price_fp > pos.peak_price_fp {
            pos.peak_price_fp = current_price_fp;
        }
        continue;
    }
    PositionPhase::Exiting => {
        // Determine exit reason based on state
        let exit_reason = if pos.buy_confirmed_ms == 0 {
            // Never confirmed → BuyTimeout
            MomentumExitReason::BuyTimeout
        } else {
            let hold = now_ms.saturating_sub(pos.buy_confirmed_ms);
            if current_bps <= -200 {
                MomentumExitReason::MicroSl
            } else if hold < 4500 {
                // Dead token in observation
                MomentumExitReason::DeadOnArrival
            } else {
                MomentumExitReason::HardSl // fallback
            }
        };
        to_close.push((mint, exit_reason, current_price_fp));
        continue;
    }
    PositionPhase::Observation => {
        // 1500-4500ms: limited exit evaluation.
        // Update peak price. Update WS notif counts.
        // Hard SL and dead token are handled by evaluate_phase → Exiting.
        // Allow drain detection to run (it's below this block).
        // But skip TP, trailing stop, velocity exit, time_sl.
        //
        // Fall through to drain detection only, then continue.
        // We handle this by letting the drain detection block run,
        // then adding a `continue` after it for Observation phase.
    }
    PositionPhase::Momentum => {
        // Price running: trailing stop evaluation only.
        // Fall through — the existing trailing stop logic handles this.
        // Skip time_sl, dead zone, etc. — only trail matters when running.
    }
    PositionPhase::ExitEligible => {
        // Full exit evaluation. All existing logic runs unchanged.
        // This is the normal path for positions held >4.5s.
    }
}
// ── End Phase gate ────────────────────────────────────────────────
```

### Detailed Integration: Wrapping Existing Exit Blocks

The cleanest approach is to wrap the existing exit evaluation blocks with
phase-aware guards:

```rust
// After the phase gate match above, the remaining code paths are:
// - Observation: drain detection only, then continue
// - Momentum: trailing stop only, then continue  
// - ExitEligible: everything (existing behavior)

// [EXISTING: max_hold handling]
// ADD GUARD: only runs in ExitEligible
if phase.allows_full_exit() && elapsed_ms >= self.config.max_hold_ms {
    // ... existing max_hold logic unchanged
}

// [EXISTING: trailing-stop-at-maturity]
// ADD GUARD: only ExitEligible
if phase.allows_full_exit() && self.config.max_hold_trail_activation_ms > 0 ... {
    // ... unchanged
}

// [EXISTING: Get current price] — already done above, skip redundant fetch

// [EXISTING: Fix E first-tick sample] — runs for all phases (need data)
// NO GUARD NEEDED

// [EXISTING: Fix A sample interval] — runs for all phases
// NO GUARD NEEDED

// [EXISTING: Peak price update] — runs for all phases
// NO GUARD NEEDED

// [EXISTING: Drain detection] — runs for Observation, Momentum, ExitEligible
// NO GUARD NEEDED (drain is emergency exit at any phase)

// After drain detection, for Observation phase, skip all remaining checks:
if matches!(phase, PositionPhase::Observation) {
    continue;
}

// [EXISTING: Top detection]
// ADD GUARD: only ExitEligible (top detection needs mature price data)
if phase.allows_full_exit() && pos.sample_count >= 2 && pos.tp_flags & 0x1 != 0 {
    // ... unchanged
}

// [EXISTING: micro exit velocity]
// ADD GUARD: only ExitEligible
if phase.allows_full_exit() && hold_ms <= self.config.micro_exit_window_ms {
    // ... unchanged
}

// [EXISTING: Dump signal s[0]]
// ADD GUARD: ExitEligible or Momentum
if !matches!(phase, PositionPhase::Observation) && pos.tp_flags & 0x8 != 0 {
    // ... unchanged
}

// [EXISTING: Hard SL]
// ADD GUARD: ExitEligible only (Observation hard SL is in evaluate_phase)
if phase.allows_full_exit() {
    let hard_sl_bps = (self.config.hard_sl_pct * 100.0) as u32;
    if pos.hard_sl_hit(current_price_fp, hard_sl_bps) {
        // ... unchanged
    }
}

// [EXISTING: Trailing stop — active after TP1]
// RUNS FOR: Momentum + ExitEligible
if matches!(phase, PositionPhase::Momentum | PositionPhase::ExitEligible)
    && pos.tp_flags & 0x1 != 0
    && pos.sample_count >= self.config.trailing_stop_min_samples
{
    // ... unchanged trailing stop logic
}

// For Momentum phase, skip everything after trailing stop
if phase.is_momentum() {
    continue;
}

// [EXISTING: Velocity exit]
// ADD GUARD: ExitEligible only
if phase.allows_full_exit() && self.config.velocity_exit_enabled ... {
    // ... unchanged
}

// [EXISTING: Adaptive dead zone]
// ADD GUARD: ExitEligible only
if phase.allows_full_exit() {
    // ... unchanged dead zone logic
}

// [EXISTING: Momentum decay]
// ADD GUARD: ExitEligible only
if phase.allows_full_exit() && hold_ms >= self.config.momentum_decay_min_hold_ms {
    // ... unchanged
}

// ... remaining phases 5, 5B, etc. all guarded by phase.allows_full_exit()
```

---

## Integration Point 3: close_position — Handle BuyTimeout

### Location: `close_position()`, in the `should_sell` logic

```rust
// In close_position(), after computing should_sell:
// ADD: If exit reason is BuyTimeout, never attempt sell
let should_sell = match exit_reason {
    MomentumExitReason::BuyTimeout => false,  // No on-chain position exists
    _ => should_sell,  // existing logic
};
```

---

## Integration Point 4: JSONL Logging

### Location: `close_position()` JSONL output

Add `buy_confirmed_ms` and `confirmed_hold_ms` to the trade close log:

```rust
// In the JSONL trade log struct, add:
buy_confirmed_ms: pos.buy_confirmed_ms,
confirmed_hold_ms: if pos.buy_confirmed_ms > 0 {
    now_ms.saturating_sub(pos.buy_confirmed_ms)
} else {
    0
},
position_phase: phase.as_str(),  // capture at close time
```

---

## Paper Mode Compatibility

For paper mode (no real TX), `buy_confirmed_ms` should be stamped immediately
at position creation since there's no async TX to wait for:

```rust
// In process_pending_entries, after creating MomentumPosition::new():
if self.config.paper_mode {
    pos.buy_confirmed_ms = now_ms; // Paper mode: instant "confirmation"
}
```

This ensures paper mode behavior is unchanged — positions enter ExitEligible
at the existing timing.

---

## Summary of Behavioral Changes

| Phase | Duration | Exit Conditions | Previously |
|-------|----------|----------------|------------|
| AwaitingConfirmation | 0 - TX landing (~800ms) | None (10s safety timeout) | Full exit eval ran |
| RapidAssessment | TX+0 to TX+1500ms | Micro-SL (-2%) only | Full exit eval ran |
| Observation | TX+1500 to TX+4500ms | Hard SL (-2%) + dead token | Full exit eval ran |
| Momentum | Any (when +100bps) | Trailing stop only | Full exit eval ran |
| ExitEligible | TX+4500ms+ | All (unchanged) | Full exit eval ran |

**Net effect**: Positions are now held a minimum of ~4.5s from on-chain buy
confirmation before full exit evaluation runs. This aligns with the on-chain
data showing 50% win rate for 3-5s holds vs 13% for 0-1s holds.
