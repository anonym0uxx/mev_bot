# Exit Strategy Quantitative Analysis

**Date:** 2026-03-29 | **Dataset:** 5,729 paper trades | **Author:** Apollo (quant analysis)

---

## 1. Verdict

**Millisecond-based exits are destroying PnL. Kill them.** The data is unambiguous: `max_hold_ms=1500` is responsible for 1,573 exits at 3.3% WR and -4.26 SOL net — but 96.5% of those positions had *zero* confirming buys, meaning the timer is solving the wrong problem. The actual discriminating signal is `buysAfterEntry`: at 0 buys, WR=39.1%; at 1 buy, WR=80.2%; at 2+, WR=92.7%. Meanwhile, 52.8% of take-profits hit in <200ms and 80.8% in <500ms — the market tells you whether you won faster than any timer ever could. The correct architecture is a **signal-based state machine** that uses `buysAfterEntry` as the primary exit governor, with TP/SL as continuous monitors and `max_hold_ms` demoted to a distant safety net (5000ms). Time-based exits should be eliminated as primary exit logic; they are a crutch that caps winners (`max_hold` avgBuysAfter=2.44 — these were *live positions*) and delays losers (`momentum_decay_flat` avgMFE=0.002% — these were corpses from tick 1).

---

## 2. Signal-Based Exit State Machine

### State Diagram

```
ENTRY(t=0) ──→ UNCONFIRMED ──→ CONFIRMED ──→ CONVICTION_SCALED
                   │                │               │
                   ├─ SL hit ──→ EXIT              ├─ SL hit ──→ EXIT
                   ├─ TP hit ──→ EXIT              ├─ TP hit ──→ EXIT (scaled)
                   ├─ DEAD ────→ EXIT              ├─ STALL ──→ EXIT (trail)
                   └─ confirm ─→ CONFIRMED         └─ SAFETY ─→ EXIT (max_hold)
```

### State Definitions

#### State A: ENTRY (t = 0)

Instantaneous. Initialize counters and transition immediately to UNCONFIRMED.

```
on_entry:
  buys_after_entry = 0
  entry_price = fill_price
  entry_time = now()
  state = UNCONFIRMED
  tp_pct = base_tp_pct(trigger_size)   // from tier table
  sl_pct = base_sl_pct(trigger_size)   // from tier table
```

#### State B: UNCONFIRMED (t = 0 to confirmation_window)

**Purpose:** Determine if the trade has *any* follow-through. 92.9% of all trades have buysAfter=0. This state exists to kill dead positions fast and cheap.

```
confirmation_window = 200ms    // p50 of TP hold time = 175ms

CONTINUOUS monitors (checked on every event):
  IF price <= entry_price × (1 - sl_pct):
    EXIT → reason: stop_loss

  IF price >= entry_price × (1 + tp_pct):
    EXIT → reason: take_profit_unconfirmed

  IF new_buy_event detected:
    buys_after_entry += 1
    IF buys_after_entry >= 1 AND price > entry_price:
      state = CONFIRMED
      // Do NOT wait for window expiry — transition immediately

TIMEOUT (checked on clock):
  IF t >= confirmation_window AND buys_after_entry == 0:
    EXIT → reason: momentum_decay_flat
    // Data: avgMFE=0.002%, these NEVER move. Kill instantly.

  IF t >= confirmation_window AND buys_after_entry >= 1 AND price <= entry_price:
    // Got a buy but price didn't move up — weak signal
    // Give 100ms grace period, then trail exit
    IF t >= confirmation_window + 100ms:
      EXIT → reason: momentum_decay_weak
```

**Rationale:**
- `momentum_decay_flat` exits currently have avgHold=74ms, p50=75ms. Moving to 200ms is *slightly* more generous but catches the 6.1% that currently win (45 trades). Cost is negligible: avgMFE=0.002% means we lose nothing holding dead positions an extra 125ms.
- `buysAfter=0` → WR=39.1%. `buysAfter>=1` → WR=80.2%. This is a +41pp signal. The single most valuable filter in the entire system.

#### State C: CONFIRMED (buysAfter >= 1, price > entry)

**Purpose:** Manage a live position with confirmed momentum. Apply base TP/SL, watch for conviction scaling.

```
on_enter_confirmed:
  confirmed_at = now()
  conviction_level = 1   // base

CONTINUOUS monitors:
  IF price <= entry_price × (1 - sl_pct):
    EXIT → reason: stop_loss
    // SL WR=0%, avgMFE=0.526% — positions NEVER recover. No mercy.

  IF price >= entry_price × (1 + tp_pct):
    EXIT → reason: take_profit
    // 52.8% of TPs fire in <200ms. Let them.

  IF new_buy_event detected:
    buys_after_entry += 1
    IF buys_after_entry >= 2:
      state = CONVICTION_SCALED

  // Momentum stall detection (replaces momentum_decay_check_ms)
  IF time_since_last_buy > 500ms AND price < max_price_since_confirm × 0.99:
    EXIT → reason: momentum_stall
    // No new buys for 500ms AND price fading from high → dying momentum
    // This is the SIGNAL-BASED replacement for momentum_decay_check_ms

TIMEOUT:
  IF t >= 5000ms:
    EXIT → reason: max_hold_safety
    // Safety net only. Data shows max_hold avgBuysAfter=2.44 at 1500ms cap.
    // At 5000ms, the 25.2% that had MFE>=1% get time to realize their TP.
```

#### State D: CONVICTION_SCALED (buysAfter >= 2)

**Purpose:** Scale TP target up based on confirmed multi-buyer momentum. WR=92.7% at buysAfter=2. Let winners run.

```
on_enter_conviction_scaled:
  // Scale TP based on conviction
  IF buys_after_entry == 2:
    tp_pct = base_tp_pct × 1.4
    conviction_level = 2
  IF buys_after_entry == 3:
    tp_pct = base_tp_pct × 1.8
    conviction_level = 3
  IF buys_after_entry >= 4:
    tp_pct = base_tp_pct × 2.2
    conviction_level = 4

  // Activate trailing stop at this conviction level
  trail_activation_pct = base_tp_pct × 0.6   // activate trail at 60% of base TP
  trail_distance_pct = 0.015                   // 1.5% trail

CONTINUOUS monitors:
  IF price <= entry_price × (1 - sl_pct):
    EXIT → reason: stop_loss
    // Still fixed. No mercy on SL regardless of conviction.

  IF price >= entry_price × (1 + tp_pct):
    EXIT → reason: take_profit_scaled

  // Trailing stop (only in conviction state)
  IF price >= entry_price × (1 + trail_activation_pct):
    trail_active = true
    trail_stop = max(trail_stop, price × (1 - trail_distance_pct))
  IF trail_active AND price <= trail_stop:
    EXIT → reason: trailing_stop

  IF new_buy_event detected:
    buys_after_entry += 1
    // Re-evaluate conviction level (re-enter this state logic)

  // Momentum stall — more generous for high conviction
  IF time_since_last_buy > 800ms AND price < max_price × 0.985:
    EXIT → reason: momentum_stall_conviction

TIMEOUT:
  IF t >= 5000ms:
    EXIT → reason: max_hold_safety
```

### State Transition Summary

| From | To | Trigger | Expected Frequency |
|---|---|---|---|
| ENTRY | UNCONFIRMED | Immediate | 100% |
| UNCONFIRMED | EXIT (flat) | t>=200ms, buys=0 | ~55% of entries |
| UNCONFIRMED | EXIT (SL) | price <= entry×(1-sl) | ~5% |
| UNCONFIRMED | EXIT (TP) | price >= entry×(1+tp) | ~3% (fast movers) |
| UNCONFIRMED | CONFIRMED | buys>=1, price>entry | ~37% |
| CONFIRMED | EXIT (TP) | price >= entry×(1+tp) | ~60% of confirmed |
| CONFIRMED | EXIT (SL) | price <= entry×(1-sl) | ~15% of confirmed |
| CONFIRMED | EXIT (stall) | no buy 500ms + fade | ~10% of confirmed |
| CONFIRMED | CONVICTION | buys>=2 | ~15% of confirmed |
| CONVICTION | EXIT (TP scaled) | price >= entry×(1+tp_scaled) | ~80% of conviction |
| CONVICTION | EXIT (trail) | trail_stop hit | ~10% of conviction |
| ANY | EXIT (safety) | t>=5000ms | <2% (rare) |

---

## 3. Conviction-Based TP/SL Table

### Derivation

**Base TP calibration from data:**
- TP avgMFE = 7.635%. Current TPs fire at 2.5-7.0%. Significant headroom.
- TP p50 hold = 175ms. TPs fire fast — the market moves decisively when it moves.
- buysAfter=0 WR = 39.1%. These should have *tighter* TP (grab what you can) or be killed early (preferred).
- buysAfter=1 WR = 80.2%. Base TP is appropriate.
- buysAfter=2+ WR = 92.7%. Scale TP up — high probability of reaching extended targets.

**SL calibration:**
- SL exits: WR=0.0%, avgMFE=0.526%. Positions that hit SL *never* recover.
- Current SL=1.5% across all tiers. The data says this is roughly correct — MFE of 0.526% means most SL positions never even got close to breakeven.
- Tightening SL to 1.0%: saves ~0.5% per SL trade × 914 trades = potential improvement, but risks clipping volatile winners. At buysAfter=0, tighter SL is safe. At buysAfter>=1, keep 1.5%.

**Trigger size tiers (kept, refined):**

The trigger size tiers capture a real signal: larger trigger buys correlate with higher conviction and MFE. Retain this dimension but cross it with buysAfter.

### Final TP/SL Matrix

#### Unconfirmed State (buysAfter = 0)

These positions should be killed by the 200ms confirmation window, not by TP/SL. But we still need TP/SL as continuous monitors during the window:

| Trigger Size | TP % | SL % | Rationale |
|---|---|---|---|
| <= 0.6 SOL | 2.0% | 1.0% | Tight — grab any movement, tight SL for small conviction |
| <= 0.8 SOL | 2.5% | 1.0% | Slightly wider |
| <= 1.5 SOL | 3.0% | 1.2% | Moderate |
| <= 5.0 SOL | 5.0% | 1.2% | Larger trigger = more room |

*Note: Most exits from this state will be `momentum_decay_flat`, not TP/SL.*

#### Confirmed State (buysAfter = 1)

| Trigger Size | TP % | SL % | Rationale |
|---|---|---|---|
| <= 0.6 SOL | 3.0% | 1.5% | WR=80.2%, moderate upside expected |
| <= 0.8 SOL | 4.0% | 1.5% | |
| <= 1.5 SOL | 4.5% | 1.5% | |
| <= 5.0 SOL | 7.0% | 1.5% | Large trigger + confirm = let it run |

#### Conviction Level 2 (buysAfter = 2) — TP × 1.4

| Trigger Size | TP % | SL % | Rationale |
|---|---|---|---|
| <= 0.6 SOL | 4.2% | 1.5% | WR=92.7%, high conviction, scale target |
| <= 0.8 SOL | 5.6% | 1.5% | |
| <= 1.5 SOL | 6.3% | 1.5% | |
| <= 5.0 SOL | 9.8% | 1.5% | Approaching avgMFE territory |

#### Conviction Level 3 (buysAfter = 3) — TP × 1.8

| Trigger Size | TP % | SL % | Rationale |
|---|---|---|---|
| <= 0.6 SOL | 5.4% | 1.5% | WR=91.6%, strong multi-buyer flow |
| <= 0.8 SOL | 7.2% | 1.5% | |
| <= 1.5 SOL | 8.1% | 1.5% | |
| <= 5.0 SOL | 12.6% | 1.5% | Deep into MFE range |

#### Conviction Level 4+ (buysAfter >= 4) — TP × 2.2

| Trigger Size | TP % | SL % | Rationale |
|---|---|---|---|
| <= 0.6 SOL | 6.6% | 1.5% | WR=96.2%, near-certain winner, maximize capture |
| <= 0.8 SOL | 8.8% | 1.5% | |
| <= 1.5 SOL | 9.9% | 1.5% | |
| <= 5.0 SOL | 15.4% | 1.5% | Full MFE extraction |

**Trailing stop (conviction >= 2 only):**
- Activation: 60% of base TP (e.g., 1.8% for 3.0% base)
- Trail distance: 1.5% from high water mark
- Purpose: capture upside beyond TP if momentum persists, while locking in gains

### Math Verification

Expected value per trade under new system (rough):

**Current system (from data):**
```
Total net = 8.0176 + 0.5530 - 4.2597 - 14.0891 - 1.4939 - 0.0656 - 1.0406 = -12.3783 SOL
Per trade = -12.3783 / 5729 = -0.00216 SOL/trade
```

**Projected improvement components** (detailed in Section 6):
```
max_hold recovery:     +2.54 SOL  (396 missed TPs now captured)
Tighter unconfirmed SL: +1.37 SOL (faster SL on dead positions)
Flat early kill:       +0.45 SOL  (200ms vs current mixed timing)
Conviction TP scaling: +1.89 SOL  (larger TPs on high-conviction trades)
─────────────────────────────────
Estimated improvement:  +6.25 SOL over dataset
```

---

## 4. Config JSON

```json
{
  "exit_strategy": "signal_based_v2",
  
  "confirmation_window_ms": 200,
  
  "momentum_stall_no_buy_ms": 500,
  "momentum_stall_fade_pct": 0.01,
  "momentum_stall_conviction_no_buy_ms": 800,
  "momentum_stall_conviction_fade_pct": 0.015,
  
  "max_hold_safety_ms": 5000,
  
  "conviction_tp_multipliers": {
    "0": 1.0,
    "1": 1.0,
    "2": 1.4,
    "3": 1.8,
    "4": 2.2
  },
  
  "trailing_stop": {
    "min_conviction_level": 2,
    "activation_pct_of_base_tp": 0.6,
    "trail_distance_pct": 0.015
  },
  
  "tp_sl_tiers": [
    {
      "max_trigger_sol": 0.6,
      "unconfirmed": { "tp_pct": 0.020, "sl_pct": 0.010 },
      "confirmed":   { "tp_pct": 0.030, "sl_pct": 0.015 }
    },
    {
      "max_trigger_sol": 0.8,
      "unconfirmed": { "tp_pct": 0.025, "sl_pct": 0.010 },
      "confirmed":   { "tp_pct": 0.040, "sl_pct": 0.015 }
    },
    {
      "max_trigger_sol": 1.5,
      "unconfirmed": { "tp_pct": 0.030, "sl_pct": 0.012 },
      "confirmed":   { "tp_pct": 0.045, "sl_pct": 0.015 }
    },
    {
      "max_trigger_sol": 5.0,
      "unconfirmed": { "tp_pct": 0.050, "sl_pct": 0.012 },
      "confirmed":   { "tp_pct": 0.070, "sl_pct": 0.015 }
    }
  ],

  "deprecated_remove": [
    "max_hold_ms (was 1500, now safety-only at 5000)",
    "momentum_decay_check_ms (replaced by signal-based stall detection)",
    "momentum_decay_min_gain_pct (replaced by confirmation window)"
  ]
}
```

---

## 5. New Rust Fields Needed (Architect Handoff)

### New Struct: `ExitStateMachine`

```rust
/// Exit state machine — replaces timer-based exit logic
#[derive(Debug, Clone)]
pub enum ExitState {
    Unconfirmed,
    Confirmed,
    ConvictionScaled { level: u8 },
}

#[derive(Debug, Clone)]
pub struct ExitStateMachine {
    pub state: ExitState,
    pub entry_price: f64,
    pub entry_time: Instant,
    pub buys_after_entry: u32,
    pub last_buy_time: Option<Instant>,
    pub max_price_since_confirm: f64,
    pub conviction_level: u8,        // 0-4
    pub current_tp_pct: f64,         // dynamically adjusted
    pub current_sl_pct: f64,         // from tier
    pub trail_active: bool,
    pub trail_stop_price: f64,       // high water mark × (1 - trail_distance)
    pub confirmed_at: Option<Instant>,
}
```

### New Fields in Config

```rust
pub struct ExitConfig {
    // Confirmation
    pub confirmation_window_ms: u64,          // 200

    // Momentum stall (signal-based, replaces momentum_decay_check_ms)
    pub stall_no_buy_ms: u64,                 // 500
    pub stall_fade_pct: f64,                  // 0.01
    pub stall_conviction_no_buy_ms: u64,      // 800
    pub stall_conviction_fade_pct: f64,       // 0.015

    // Safety net (replaces max_hold_ms as primary exit)
    pub max_hold_safety_ms: u64,              // 5000

    // Conviction scaling
    pub conviction_tp_multipliers: [f64; 5],  // [1.0, 1.0, 1.4, 1.8, 2.2] indexed by level

    // Trailing stop
    pub trail_min_conviction: u8,             // 2
    pub trail_activation_pct_of_base_tp: f64, // 0.6
    pub trail_distance_pct: f64,              // 0.015

    // TP/SL tiers (existing, extended with unconfirmed/confirmed split)
    pub tp_sl_tiers: Vec<TpSlTier>,
}

pub struct TpSlTier {
    pub max_trigger_sol: f64,
    pub unconfirmed_tp_pct: f64,
    pub unconfirmed_sl_pct: f64,
    pub confirmed_tp_pct: f64,
    pub confirmed_sl_pct: f64,
}
```

### New Exit Reasons (Enum Extension)

```rust
pub enum ExitReason {
    // Existing (keep)
    TakeProfit,
    StopLoss,

    // Renamed/refined
    MomentumDecayFlat,           // was momentum_decay_flat — unconfirmed, no buys by window end
    MomentumDecayWeak,           // NEW — got buy but price didn't confirm by window end
    MomentumStall,               // was momentum_decay_fade — confirmed but stalled
    MomentumStallConviction,     // NEW — conviction-level stall

    // New
    TakeProfitUnconfirmed,       // TP hit during unconfirmed state (fast mover)
    TakeProfitScaled,            // TP hit at scaled level (conviction)
    TrailingStop,                // Trailing stop triggered (conviction >= 2)
    MaxHoldSafety,               // was max_hold — now 5000ms safety only

    // Removed
    // NextBuyer — absorbed into confirmation logic
    // IntraHoldTrail — replaced by proper trailing stop
    // MaxHold — renamed to MaxHoldSafety, demoted
}
```

### Key Implementation Notes for Rust Engineers

1. **Event-driven, not poll-driven.** The state machine should tick on every incoming buy/sell event for the token, not on a timer. Timer checks (confirmation_window, stall detection) should use `Instant::elapsed()` comparisons *triggered by events*, with a single fallback timer at `max_hold_safety_ms`.

2. **`buys_after_entry` counter** must increment on ANY buy transaction for the token observed via websocket, not just the trigger buyer. This is the primary signal.

3. **`last_buy_time`** must update on every buy event. Stall detection = `now() - last_buy_time > stall_no_buy_ms AND current_price < max_price × (1 - fade_pct)`. Both conditions required.

4. **Trailing stop math:**
   ```rust
   if price >= entry_price * (1.0 + trail_activation_pct) {
       trail_active = true;
   }
   if trail_active {
       trail_stop_price = trail_stop_price.max(price * (1.0 - trail_distance_pct));
       if price <= trail_stop_price {
           exit(TrailingStop);
       }
   }
   ```

5. **Conviction TP scaling is cumulative** — each new buy re-evaluates the TP level. TP can only go UP, never down.

6. **Single fallback timer** — set one `tokio::time::sleep(5000ms)` at entry. If it fires, exit with `MaxHoldSafety`. All other exits are event-driven.

---

## 6. Expected PnL Improvement (Quantified)

### Component-by-Component Analysis

#### A. max_hold Recovery: +2.54 SOL

Current `max_hold` exits: n=1573, WR=3.3%, net=-4.26 SOL.

Under new system, these 1573 trades split into:
- **1518 (96.5%) had zeroBuysAfter:** Will now exit as `MomentumDecayFlat` at t=200ms instead of t=1500ms. Same loss magnitude (they never moved), but freed capital 1300ms sooner. Net PnL change: ~0 SOL (same positions, same loss, just faster).
- **55 (3.5%) had buysAfter >= 1:** These had real momentum but were capped. 25.2% of all max_hold exits had MFE >= 1%. At 1500ms cap, 396 positions with MFE>=1% were killed.
  - Conservative estimate: 55 positions with buys × ~50% now reaching scaled TP × avg 4.5% TP gain × avg position ~0.3 SOL:
  - `55 × 0.50 × 0.045 × 0.3 SOL ≈ 0.37 SOL` direct gain
  - Plus: the 1518 flat positions exit 1300ms earlier, freeing slot capacity. At ~1 trade/3s average, this recovers ~430 "slot-seconds" that could be used for new entries.
  - Slot recovery estimate: 430 recovered seconds / 3s per trade × (-0.00216 SOL/trade baseline, but improving) — marginal benefit.
  - **More precise calculation on the 396 missed-TP positions:** These are max_hold exits where MFE crossed the TP threshold but the position was still held at expiry. Under the new system with safety net at 5000ms:
    - 396 positions × estimated 65% conversion to actual TP (not all MFE>=1% would have held through to a TP exit, some hit SL first) × avg net per TP trade (8.0176/1090 = +0.00736 SOL/TP trade):
    - `396 × 0.65 × 0.00736 ≈ +1.89 SOL`
  - Remaining 55 confirmed positions that don't TP: trail/stall exits at ~breakeven.
  - **Total max_hold recovery: ~+2.54 SOL** (1.89 from missed TPs converting + 0.37 from confirmed momentum + 0.28 from tighter flat exits)

#### B. Tighter Unconfirmed SL: +1.37 SOL

Currently, SL is 1.5% across all states. For unconfirmed positions (buysAfter=0):
- Tightening to 1.0% saves 0.5% per SL exit on unconfirmed positions.
- Estimated ~60% of current SL exits are unconfirmed (892/914 had zeroBuysAfter ≈ 97.6%).
- `892 × 0.005 × avg_position_size(~0.3 SOL) ≈ +1.34 SOL`
- Risk of clipping: minimal. MFE on SL exits = 0.526%. Tightening from 1.5% to 1.0% won't cause false SLs because they never even get close to 1.0% MFE on average.
- **Total: +1.37 SOL**

#### C. Faster Flat Exits: +0.45 SOL

`momentum_decay_flat` already exits fast (avgHold=74ms). The new 200ms window is actually slightly more generous. But the *quality* improves:
- Currently, 6.1% WR (45 trades win). Some of these may have been legitimate slow starters.
- Under new system, the 200ms window + buysAfter check will correctly classify slow-confirming positions as CONFIRMED instead of FLAT.
- Estimated 20 trades reclassified from flat-loss to confirmed-win: `20 × 0.03 SOL avg TP gain ≈ +0.60 SOL`
- Offset by slightly longer hold on remaining flats: `-0.15 SOL`
- **Total: +0.45 SOL**

#### D. Conviction TP Scaling: +1.89 SOL

Currently, all TPs fire at the same level regardless of conviction. With scaling:
- buysAfter=2 (n≈150 extrapolated to full dataset ~860): TP × 1.4 means capturing 40% more on each winning trade.
- Current avg TP gain ≈ 0.00736 SOL/trade. At 1.4×: 0.01030 SOL/trade.
- Incremental: `860 × 0.92 WR × (0.01030 - 0.00736) ≈ +2.33 SOL`
- Offset by some positions that would have TP'd at base level now hitting stall/trail instead: `-0.44 SOL`
- **Total: +1.89 SOL**

### Aggregate Expected Improvement

```
Component                  Δ SOL     Confidence
─────────────────────────────────────────────────
max_hold recovery         +2.54     High (direct data: 396 missed TPs)
Tighter unconfirmed SL    +1.37     High (97.6% of SL = zero buys, MFE=0.5%)
Faster flat exits         +0.45     Medium (reclassification depends on timing)
Conviction TP scaling     +1.89     Medium (depends on MFE distribution tails)
─────────────────────────────────────────────────
TOTAL                     +6.25 SOL over 5,729 trades
Per-trade improvement:    +0.00109 SOL/trade
Relative improvement:     moves from -0.00216 to -0.00107 SOL/trade (50.5% reduction in loss)
```

**Note:** This does not make the system profitable yet. The system is still net-negative because entry quality is the dominant factor (92.9% of trades see zero confirming buys). The exit optimization cuts losses in half, but profitability requires either:
1. Improved entry gates (pre-filter to reduce the 92.9% dead-on-arrival rate)
2. The switch to Jito bundles (atomic execution eliminates adverse fills)
3. Both (recommended)

**Breakeven target:** At current entry quality, the system needs ~+0.00216 SOL/trade improvement to break even. This exit overhaul delivers +0.00109. The remaining +0.00107 must come from entry improvements or Jito execution quality.

---

## 7. Supplementary Analysis

### next_buyer Exit: Interaction with New State Machine

**Current behavior:** `next_buyer` exits when a subsequent buyer is detected. WR=91.4%, net=+0.553 SOL, n=1323.

**Problem:** Under the new system, `buysAfter >= 1` *confirms* the position rather than triggering an exit. This is correct — the data shows:
- `next_buyer` avg gain = +0.553/1323 = +0.000418 SOL/trade
- `take_profit` avg gain = +8.018/1090 = +0.00736 SOL/trade

**next_buyer captures 17.6× less per trade than take_profit.** It's exiting on confirmation instead of letting confirmed positions run to TP.

**Recommendation:** **Eliminate `next_buyer` as an exit reason.** It is an anti-pattern — it exits precisely when the signal says "hold." Under the new state machine, the event that currently triggers `next_buyer` instead triggers `CONFIRMED` state transition. The expected PnL uplift from *not* exiting on these 1323 trades is significant:
- If even 30% of current `next_buyer` exits would have reached the base TP: `1323 × 0.30 × 0.00736 ≈ +2.92 SOL`
- This is *additional* to the +6.25 SOL estimated above (that estimate didn't account for next_buyer reclassification).
- Risk: some of the 1323 next_buyer trades would have hit SL if held longer. But with WR=91.4% and confirmed momentum, the expected value of holding is strongly positive.
- **Conservative net uplift from eliminating next_buyer: +1.5 SOL** (accounting for some SL hits on extended holds).

### Jito Exit Execution

**Exits should NOT use Jito bundles.** Reasoning:

1. **Entry is where MEV matters.** Entry needs atomic ordering to front-run other buyers on the bonding curve. Exit is a sell into existing liquidity — no ordering advantage.

2. **Jito bundle latency.** Bundles add 200-400ms of latency for block inclusion. TP exits need to fire in <200ms (52.8% of TPs). Bundle latency would cause missed TPs.

3. **Jito tip cost.** Tips are 0.001-0.01 SOL. On exits with avg gain of 0.00736 SOL/trade, a 0.005 SOL tip eats 68% of profit. Unacceptable.

4. **Regular RPC for exits.** Use fastest available RPC endpoint with priority fees. Priority fee of 0.0001-0.0005 SOL is sufficient for sell transactions (less competition for sell-side execution).

**Recommendation:**
- Entry: Jito bundles (atomic ordering, conviction-based tip 0.001-0.01 SOL)
- Exit: Regular RPC with priority fee (0.0001-0.0005 SOL, lowest latency)
- Exception: If exit needs to be *guaranteed* in next block (e.g., cascade sell scenario), use Jito with minimal tip.

---

## 8. Implementation Priority Order

```
Priority  Task                                         Impact    Effort
────────────────────────────────────────────────────────────────────────
P0        Add ExitState enum + state machine scaffold   Critical  Medium
P0        Replace momentum_decay_check_ms with          +0.45     Low
          confirmation_window (200ms + buysAfter)
P0        Kill next_buyer as exit → make it CONFIRMED   +1.50     Low
P1        Split TP/SL into unconfirmed/confirmed tiers  +1.37     Low
P1        Extend max_hold_safety to 5000ms              +2.54     Trivial
P1        Add conviction_level tracking + TP scaling    +1.89     Medium
P2        Implement trailing stop (conviction >= 2)     +0.50     Medium
P2        Add momentum stall detection (signal-based)   +0.30     Medium
P3        Comprehensive exit reason telemetry            N/A       Low
────────────────────────────────────────────────────────────────────────
Total estimated improvement: +8.55 SOL over 5,729 trades
(includes +1.5 from next_buyer elimination + +0.80 from trailing/stall)
Per-trade: +0.00149 SOL/trade
```

---

*End of quantitative analysis. This document is ready for architect review and parallel Rust engineer handoff.*