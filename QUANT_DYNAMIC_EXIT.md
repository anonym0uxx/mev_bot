# Dynamic Exit Signal System — Quantitative Specification

## Executive Summary

Replace the static 3-phase trailing stop (EARLY/MOMENTUM/TIGHTEN at fixed time boundaries) with a **continuous, signal-driven exit engine** that computes trail distance as a function of real-time order flow health. The system monitors a vector of 5 integer-computable signals on every trade event, combines them into a composite score, and maps that score to a trail distance in milli-vSOL basis points. No floating-point math on the hot path. All parameters derived from 207 historical RIDE trades.

---

## 1. Data-Driven Context

### 1.1 Dataset Summary

| Metric | Value |
|--------|-------|
| Total trades | 207 |
| Win rate | 67.6% (140W / 67L) |
| Net PnL (sum) | 0.8945 SOL |
| Avg winner | 0.00801 SOL |
| Avg loser | −0.00339 SOL |
| Median hold | 1028 ms |
| Mean MFE% | 8.50% |
| Mean capture ratio | 47.7% (median 67.0%) |

### 1.2 Critical Findings

**Finding 1: Max-hold exits left massive money on the table.**
- 35 trades hit max_hold (1.5s cutoff), avg MFE 9.52%, capture 97.7%.
- Top max_hold trade: MFE=53.5%, 20 buys at 13.1/s, 20 unique wallets, 28.65 SOL flow — this was an absolute rocket that got chopped at 1.5s.
- These 35 trades contributed 0.300 SOL (33.5% of total PnL). With longer holds, they likely would have contributed 2-3x that.

**Finding 2: Flow rate is the single strongest predictor of PnL.**
- Q4 buy rate (>14/s): avg PnL 0.00726, 70% win rate
- Q1 buy rate (<3.9/s): avg PnL 0.00131, 59% win rate
- Flow ≥5 SOL: 59 trades, avg PnL 0.01529, **98% win rate**
- Flow ≥10 SOL: 19 trades, avg PnL 0.02980, **100% win rate**

**Finding 3: Sell ratio is the strongest negative signal.**
- Sell ratio <10%: avg PnL 0.01004, n=43
- Sell ratio ≥30%: avg PnL −0.00078, n=55
- Inflection point at ~25%: above this, edge vanishes

**Finding 4: Momentum phase trades are the jackpot.**
- 13 momentum trades: avg PnL 0.034 SOL (7.9x average)
- Profile: 22+ buys, 16+ SOL flow, 20+ unique wallets
- These are the trades we must hold longer

**Finding 5: Hard floor exits are always losses.**
- 30 trades, 100% losers, avg loss −0.00241 SOL
- These fire fast (avg 761ms hold) — the entry was wrong, exit fast

**Finding 6: Trailing stop captures only 63.5% of MFE.**
- 14/50 trailing stop exits captured <50% of MFE — trail was too tight for the move size

**Finding 7: MAE > 0 always means loss.**
- 29 trades with any drawdown → 100% were losers
- Implication: any price decline from entry is a strong exit signal on bonding curves

---

## 2. Real-Time Signal Vector

The exit engine maintains 5 signals, updated on every trade event. All use integer arithmetic.

### 2.1 Signal Definitions

#### S1: Buy Rate (`buy_rate_1k` — buys per second × 1000)

```
Maintained as: ring buffer of last N buy timestamps (N=32)

On each buy event:
  count = buys in last WINDOW_MS (default 2000ms)
  buy_rate_1k = count * 1000 * 1000 / WINDOW_MS   // buys/sec * 1000

Example: 14 buys in 2000ms → buy_rate_1k = 14 * 1_000_000 / 2000 = 7000 (= 7.0/sec)
```

**Data-derived thresholds:**
- Excellent: ≥10,000 (10/s) — Q3+ territory, avg PnL 0.00728
- Good: 5,000–10,000 (5-10/s) — Q2-Q3, avg PnL 0.00573
- Weak: 2,000–5,000 (2-5/s) — Q1-Q2, avg PnL 0.00139
- Dead: <2,000 (2/s) — avg PnL −0.00011

#### S2: Flow Momentum (`flow_rate_mlamports` — milli-lamports per second)

```
Maintained as: ring buffer of last N trades with (timestamp, sol_amount)

On each buy event:
  total_lamports = sum of buy amounts in WINDOW_MS (as lamports, u64)
  flow_rate_mlamports = total_lamports * 1000 / WINDOW_MS   // lamports/sec

Example: 4.5 SOL in 2s → flow_rate_mlamports = 4_500_000_000 * 1000 / 2000 = 2_250_000_000_000
         (= 2.25 SOL/sec)
```

**Data-derived thresholds (in SOL/sec equivalents):**
- Rocket: ≥5.0 SOL/s — these trades are 98% winners
- Strong: 2.0–5.0 SOL/s — solid momentum
- Fading: 0.5–2.0 SOL/s — edge is thinning
- Dead: <0.5 SOL/s — exit

For integer representation: use `flow_rate_mlam = lamports_per_sec * 1000` to avoid precision loss. Thresholds become:
- Rocket: ≥5_000_000_000_000 mlamports/s
- Strong: ≥2_000_000_000_000
- Fading: ≥500_000_000_000
- Dead: <500_000_000_000

#### S3: Sell Pressure (`sell_ratio_1k` — sells / total_trades × 1000)

```
Maintained as: counters of buys and sells in rolling window

  total = buys_in_window + sells_in_window
  sell_ratio_1k = if total > 0 { sells_in_window * 1000 / total } else { 500 }

Example: 8 buys, 2 sells → sell_ratio_1k = 2000 / 10 = 200 (= 20%)
```

**Data-derived thresholds:**
- Clean: <100 (10%) — avg PnL 0.01004, 86% win rate
- Healthy: 100–250 (10-25%) — avg PnL 0.00783
- Contested: 250–350 (25-35%) — avg PnL 0.00021, edge evaporating
- Toxic: ≥350 (35%+) — avg PnL −0.00010, negative expectancy

#### S4: Price Velocity (`vsol_velocity_1k` — milli-vSOL per second × 1000)

```
Maintained as: entry_mvsol (at position open) and current peak_mvsol

  elapsed_ms = now - entry_timestamp_ms
  // Current velocity from entry (smoothed by time)
  vsol_velocity_1k = if elapsed_ms > 0 {
    (peak_mvsol - entry_mvsol) * 1_000_000 / elapsed_ms
  } else { 0 }

Example: entry 51000 mvsol, peak 55000 mvsol, 1000ms elapsed
  vsol_velocity_1k = 4000 * 1_000_000 / 1000 = 4_000_000 (= 4.0 mvsol/sec)
```

**Data-derived thresholds:**
- Winners avg velocity: 5.75 mvsol/s → 5_750_000 in 1k units
- Losers avg velocity: −2.97 mvsol/s → negative
- Any negative velocity is a strong exit signal (recall: MAE>0 → 100% loss)

#### S5: Gap Pressure (`gap_ms` — milliseconds since last buy)

```
Maintained as: timestamp of most recent buy event

  gap_ms = now - last_buy_timestamp_ms
```

**Data-derived thresholds:**
- Active: <200ms — trades flowing continuously
- Slowing: 200–500ms — normal rhythm for decent trades
- Warning: 500–1000ms — momentum clearly fading
- Stale: >1000ms — virtually no edge remaining

Key stat: average exit gap (holdMs − rideHoldMs) is 205ms mean, 97ms median. Most exits happen within ~100ms of last trade.

### 2.2 Window Parameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `RATE_WINDOW_MS` | 2000 | Median hold is 1028ms. 2s window captures enough history without going stale. |
| `FLOW_WINDOW_MS` | 2000 | Same window for consistency. |
| `SELL_WINDOW_MS` | 3000 | Slightly wider — sells are sparser, need more data. |
| `RING_BUFFER_SIZE` | 64 | Max trades in window. 99th percentile is ~60 buys at 30/s × 2s. |

---

## 3. Signal-to-Trail Mapping

### 3.1 Composite Score

Each signal maps to a **score component** (0–255 range, u8). The composite is a weighted sum.

```
// Normalize each signal to 0-255 range

fn buy_rate_score(buy_rate_1k: u64) -> u8 {
    // Linear ramp from 2000 (dead) to 15000 (excellent)
    // Below 2000 → 0, above 15000 → 255
    clamp((buy_rate_1k.saturating_sub(2000) * 255) / 13000, 0, 255) as u8
}

fn flow_rate_score(flow_rate_mlam: u64) -> u8 {
    // SOL/sec: 0.5 → 0, 5.0 → 255
    // In mlamports: 500B → 0, 5000B → 255
    let floor = 500_000_000_000u64;
    let ceil = 5_000_000_000_000u64;
    clamp((flow_rate_mlam.saturating_sub(floor) * 255) / (ceil - floor), 0, 255) as u8
}

fn sell_pressure_score(sell_ratio_1k: u64) -> u8 {
    // Inverted: 0 sells → 255, 350+ → 0
    if sell_ratio_1k >= 350 { return 0; }
    ((350 - sell_ratio_1k) * 255 / 350) as u8
}

fn velocity_score(vsol_velocity_1k: i64) -> u8 {
    // Negative → 0. 0 → 64 (neutral). 8000+ → 255
    if vsol_velocity_1k <= 0 { return 0; }
    clamp((vsol_velocity_1k as u64 * 255) / 8_000, 0, 255) as u8
}

fn gap_score(gap_ms: u64) -> u8 {
    // 0ms → 255, 1000ms → 0
    if gap_ms >= 1000 { return 0; }
    ((1000 - gap_ms) * 255 / 1000) as u8
}
```

**Composite weighted sum:**

```
WEIGHT_BUY_RATE   = 3    // Strong predictor (Q4 vs Q1 = 5.5x PnL difference)
WEIGHT_FLOW_RATE  = 4    // Strongest predictor (flow≥5 → 98% win rate)
WEIGHT_SELL_PRESS = 3    // Strong negative signal (clean<10% → 0.010 vs toxic≥35% → -0.001)
WEIGHT_VELOCITY   = 2    // Confirming signal (winners 5.75 vs losers -2.97)
WEIGHT_GAP        = 3    // Critical for exit timing (median exit gap 97ms)

WEIGHT_SUM = 15

composite_score = (
    buy_rate_score    * WEIGHT_BUY_RATE +
    flow_rate_score   * WEIGHT_FLOW_RATE +
    sell_press_score  * WEIGHT_SELL_PRESS +
    velocity_score    * WEIGHT_VELOCITY +
    gap_score         * WEIGHT_GAP
) / WEIGHT_SUM

// Result: 0-255 range, integer
```

### 3.2 Composite → Trail Distance

Map composite score to trail in milli-vSOL basis points (mvsol_bp), where 1000 bp = 100% of entry vSOL.

The current system uses:
- EARLY: 80 bp (8% trail) at entryVSol ~55000 mvsol → trail = 4400 mvsol
- MOMENTUM: 60 bp (6%) → trail = 3300 mvsol
- TIGHTEN: 40 bp (4%) → trail = 2200 mvsol

**Dynamic mapping:**

```
// Trail ranges in basis points (per 1000 of entry_mvsol):
TRAIL_MAX_BP = 100   // 10% trail — widest (very strong signals)
TRAIL_MIN_BP = 25    // 2.5% trail — tightest (very weak signals, about to exit)

fn composite_to_trail_bp(composite: u8) -> u16 {
    // Linear interpolation: score 0 → MIN, score 255 → MAX
    TRAIL_MIN_BP + ((composite as u32 * (TRAIL_MAX_BP - TRAIL_MIN_BP)) / 255) as u16
}

fn trail_mvsol(trail_bp: u16, entry_mvsol: u32) -> u32 {
    // Convert basis points to absolute mvsol distance
    (entry_mvsol as u64 * trail_bp as u64 / 1000) as u32
}
```

**Why 10% max / 2.5% min:**
- Current 8% trail at EARLY phase already produces 80.8% capture on sell_cascade exits (the best captures)
- Widening to 10% for strong-signal trades lets us ride harder (momentum trades hit 53.5% MFE)
- 2.5% minimum gives a tight leash when signals are dying — tighter than current 4% TIGHTEN
- At entryVSol=55000: trail ranges from 1375 mvsol (tight) to 5500 mvsol (wide)

### 3.3 Complete Trail Computation (Hot Path)

```
fn compute_trail(state: &RideState, now_ms: u64) -> u32 {
    let s1 = buy_rate_score(state.buy_rate_1k);
    let s2 = flow_rate_score(state.flow_rate_mlam);
    let s3 = sell_pressure_score(state.sell_ratio_1k);
    let s4 = velocity_score(state.vsol_velocity_1k);
    let s5 = gap_score(now_ms.saturating_sub(state.last_buy_ms));

    let composite: u32 = s1 as u32 * 3
        + s2 as u32 * 4
        + s3 as u32 * 3
        + s4 as u32 * 2
        + s5 as u32 * 3;
    let composite_norm: u8 = (composite / 15).min(255) as u8;

    let trail_bp: u16 = 25 + ((composite_norm as u32 * 75) / 255) as u16;
    let trail_mvsol: u32 = (state.entry_mvsol as u64 * trail_bp as u64 / 1000) as u32;

    trail_mvsol
}
```

**Performance: 5 lookups + 15 multiplies + 5 additions + 2 divisions = ~20 integer ops. Well under 100ns.**

---

## 4. Dynamic Exit Triggers

### 4.1 Primary Exit: Trailing Stop (Existing, Improved)

The trailing stop logic stays the same mechanically — track `peak_mvsol`, exit when `peak_mvsol - current_mvsol > trail_mvsol`. What changes is that `trail_mvsol` is now dynamic instead of phase-switched.

```
if peak_mvsol - current_mvsol > compute_trail(state, now_ms) {
    exit(TrailingStop)
}
```

### 4.2 Signal Death Exit (Replaces max_hold)

Instead of a fixed time cutoff, exit when the composite score drops below a threshold AND stays below for a confirmation window.

```
SIGNAL_DEATH_THRESHOLD = 30     // Out of 255 — roughly bottom 12%
SIGNAL_DEATH_CONFIRM_MS = 200   // Must stay dead for 200ms

// On each tick:
if composite_norm < SIGNAL_DEATH_THRESHOLD {
    if state.signal_death_start_ms == 0 {
        state.signal_death_start_ms = now_ms;
    } else if now_ms - state.signal_death_start_ms >= SIGNAL_DEATH_CONFIRM_MS {
        exit(SignalDeath)
    }
} else {
    state.signal_death_start_ms = 0;  // Reset: signal recovered
}
```

**Derivation of threshold = 30:**
- composite_norm = 30 means roughly: buy_rate < 3/s, flow < 1 SOL/s, sell_ratio > 25%, velocity near 0, gap > 700ms
- At this profile, empirical avg PnL is negative (Q1 composite2: avg PnL = −0.00074)
- The 200ms confirmation prevents exit on momentary gaps between block batches

### 4.3 Gap Decay Exit (Replaces static gap timeout)

Instead of "exit if gap > 10s", the gap interacts with signal strength. A trade with strong prior flow can tolerate longer gaps than a weak trade.

```
// gap_pressure increases over time since last buy
// It's checked against the current trail — if gap pressure exceeds trail, exit
GAP_DECAY_RATE = 4   // mvsol of trail consumed per ms of gap (integer)

fn gap_exit_check(state: &RideState, now_ms: u64) -> bool {
    let gap = now_ms.saturating_sub(state.last_buy_ms);
    let gap_pressure = gap * GAP_DECAY_RATE;  // in mvsol units

    // Strong signals get a bonus buffer
    let signal_buffer = (state.composite_norm as u64) * state.entry_mvsol as u64 / 25_500;
    // At composite=255, buffer = entry_mvsol/100 = 1% of entry → ~550 mvsol
    // At composite=0, buffer = 0

    let current_trail = compute_trail(state, now_ms) as u64;
    
    gap_pressure > current_trail + signal_buffer
}
```

**Derivation of GAP_DECAY_RATE = 4:**
- Median exit gap is 97ms. At rate=4: 97 * 4 = 388 mvsol pressure
- Tightest trail (composite=0): trail = 25bp of 55000 = 1375 mvsol
  - Gap tolerance: 1375/4 = 344ms before gap alone triggers exit (matches data: trades with 300ms+ gaps are marginal)
- Widest trail (composite=255): trail = 100bp of 55000 = 5500 mvsol + 550 buffer = 6050
  - Gap tolerance: 6050/4 = 1513ms — strong trades can survive 1.5s gaps (matches: momentum trades with pauses)

### 4.4 Sell Cascade Exit (Keep Existing, Enhance)

The existing sell_cascade detection is already the best exit reason (80.8% capture ratio, 0.00861 avg PnL). Keep it, but modulate sensitivity:

```
// Instead of fixed "3 sells = exit", scale with signal strength
SELL_CASCADE_BASE = 2       // Minimum sells to trigger
SELL_CASCADE_SIGNAL_BONUS = 2  // Extra sells tolerated at max composite

fn sell_cascade_threshold(composite_norm: u8) -> u8 {
    SELL_CASCADE_BASE + (composite_norm as u32 * SELL_CASCADE_SIGNAL_BONUS as u32 / 255) as u8
}

// At composite=0: exit after 2 sells (weak trade, bail fast)
// At composite=255: exit after 4 sells (strong trade, can absorb selling)
```

### 4.5 Hard Floor (Keep Existing)

Hard floor exits are 100% losers (avg −0.00241 SOL). The mechanism is correct — if price drops below entry, the bonding curve sell is guaranteed to be a loss. Keep the existing hard floor as-is. This is not time-dependent.

### 4.6 Whale Exit (Keep Existing, Tighten Trail)

Whale exits (38 trades, 14W/24L) are the most unreliable. When a single large sell is detected:

```
// On whale sell detection: immediately tighten trail to 50% of current
fn on_whale_sell(state: &mut RideState) {
    state.trail_override_bp = compute_trail_bp(state) / 2;
    state.trail_override_expires_ms = state.now_ms + 500; // Override lasts 500ms
}

// In trail computation, use min(dynamic_trail, override) if active
```

### 4.7 Emergency Max Hold (Safety Net)

Even with signal-driven exits, keep a safety-net max hold to bound tail risk. But set it high enough that it almost never fires:

```
EMERGENCY_MAX_HOLD_MS = 30_000  // 30 seconds
// Only 5 trades in data held >5s. At 30s, this is purely catastrophic protection.
```

---

## 5. State Machine (Replaces Phase Transitions)

The old system: `time < 15s → EARLY | time < 60s → MOMENTUM | else → TIGHTEN`

The new system has no phases. Instead, the exit engine maintains a **continuous state**:

```
struct RideExitState {
    // Entry
    entry_mvsol: u32,
    entry_timestamp_ms: u64,
    
    // Peak tracking
    peak_mvsol: u32,
    
    // Signal ring buffers
    buy_timestamps: RingBuffer<u64, 64>,
    buy_amounts_lam: RingBuffer<u64, 64>,     // lamports per buy
    sell_count_window: u16,
    trade_count_window: u16,
    
    // Derived signals (updated on each trade)
    buy_rate_1k: u64,
    flow_rate_mlam: u64,
    sell_ratio_1k: u64,
    vsol_velocity_1k: i64,
    last_buy_ms: u64,
    
    // Composite
    composite_norm: u8,
    
    // Signal death tracking
    signal_death_start_ms: u64,
    
    // Whale override
    trail_override_bp: u16,
    trail_override_expires_ms: u64,
}
```

### 5.1 Update Logic (On Every Trade Event)

```
fn on_trade(state: &mut RideExitState, trade: &TradeEvent, now_ms: u64) {
    if trade.is_buy {
        state.buy_timestamps.push(now_ms);
        state.buy_amounts_lam.push(trade.amount_lamports);
        state.last_buy_ms = now_ms;
        
        // Update peak
        if trade.current_mvsol > state.peak_mvsol {
            state.peak_mvsol = trade.current_mvsol;
        }
    }
    
    // Evict stale entries from ring buffers (older than window)
    state.buy_timestamps.evict_before(now_ms - RATE_WINDOW_MS);
    state.buy_amounts_lam.evict_before(now_ms - FLOW_WINDOW_MS);
    
    // Recompute signals
    state.buy_rate_1k = state.buy_timestamps.len() * 1_000_000 / RATE_WINDOW_MS;
    state.flow_rate_mlam = state.buy_amounts_lam.sum() * 1000 / FLOW_WINDOW_MS;
    
    // Sell ratio from wider window
    let (buys_w, sells_w) = count_trades_in_window(SELL_WINDOW_MS);
    let total = buys_w + sells_w;
    state.sell_ratio_1k = if total > 0 { sells_w * 1000 / total } else { 500 };
    
    // Velocity
    let elapsed = now_ms.saturating_sub(state.entry_timestamp_ms);
    state.vsol_velocity_1k = if elapsed > 0 {
        ((state.peak_mvsol as i64 - state.entry_mvsol as i64) * 1_000_000) / elapsed as i64
    } else { 0 };
    
    // Composite
    state.composite_norm = compute_composite(state, now_ms);
    
    // Check exits
    check_trailing_stop(state, trade.current_mvsol, now_ms);
    check_signal_death(state, now_ms);
    check_gap_decay(state, now_ms);
    check_sell_cascade(state, trade);
    check_hard_floor(state, trade.current_mvsol);
    check_emergency_max_hold(state, now_ms);
}
```

### 5.2 Tick Logic (On Timer, Between Trades)

Between trades, the gap score degrades. Run a lightweight check every ~50ms:

```
fn on_tick(state: &mut RideExitState, now_ms: u64) {
    // Only need to update gap-dependent signals
    let gap = now_ms.saturating_sub(state.last_buy_ms);
    
    // Recompute composite with updated gap
    state.composite_norm = compute_composite(state, now_ms);
    
    // Check gap-based exits
    check_signal_death(state, now_ms);
    check_gap_decay(state, now_ms);
    check_emergency_max_hold(state, now_ms);
}
```

---

## 6. Parameter Table

All tunable parameters in one place, with data-derived initial values:

| Parameter | Value | Unit | Derivation |
|-----------|-------|------|------------|
| `RATE_WINDOW_MS` | 2000 | ms | Median hold 1028ms; 2x for stability |
| `FLOW_WINDOW_MS` | 2000 | ms | Same as rate window |
| `SELL_WINDOW_MS` | 3000 | ms | Wider for sparser sell events |
| `TRAIL_MAX_BP` | 100 | bp/1000 (=10%) | Wider than current 8% to ride momentum |
| `TRAIL_MIN_BP` | 25 | bp/1000 (=2.5%) | Tighter than current 4% for dying trades |
| `WEIGHT_BUY_RATE` | 3 | - | Q4/Q1 PnL ratio = 5.5x |
| `WEIGHT_FLOW_RATE` | 4 | - | Strongest predictor, flow≥5 = 98% WR |
| `WEIGHT_SELL_PRESS` | 3 | - | Clean(<10%) vs toxic(>35%) = 0.011 spread |
| `WEIGHT_VELOCITY` | 2 | - | Confirming but lagging |
| `WEIGHT_GAP` | 3 | - | Critical for exit timing |
| `SIGNAL_DEATH_THRESHOLD` | 30 | /255 | Bottom 12%, negative EV |
| `SIGNAL_DEATH_CONFIRM_MS` | 200 | ms | Prevent false triggers from block gaps |
| `GAP_DECAY_RATE` | 4 | mvsol/ms | Derived from median gap 97ms vs trail sizes |
| `SELL_CASCADE_BASE` | 2 | count | Minimum sells to trigger |
| `SELL_CASCADE_SIGNAL_BONUS` | 2 | count | Extra sells tolerated at max composite |
| `EMERGENCY_MAX_HOLD_MS` | 30000 | ms | Safety net only, 99.9% trades exit before this |

---

## 7. Expected Behavioral Changes vs Current System

### 7.1 Scenario: Rocket Trade (Current: Chopped at 1.5s)

**Profile:** 13 buys/s, 5 SOL/s flow, 0% sells, velocity 8 mvsol/s, 0ms gap

```
Scores: buy_rate=216, flow_rate=255, sell_press=255, velocity=255, gap=255
Composite = (216*3 + 255*4 + 255*3 + 255*2 + 255*3) / 15 = 250
Trail = 25 + (250 * 75 / 255) = 98 bp → 5390 mvsol at entry=55000

Old system: exits at 1500ms regardless
New system: keeps riding with 9.8% trail. If flow continues, holds for 5-30s.
Estimated improvement: captures 20-50% more MFE on these trades.
```

### 7.2 Scenario: Fading Trade (Current: Holds to 1.5s Unnecessarily)

**Profile:** 2 buys/s, 0.8 SOL/s flow, 30% sells, velocity -1 mvsol/s, 500ms gap

```
Scores: buy_rate=0, flow_rate=17, sell_press=36, velocity=0, gap=127
Composite = (0*3 + 17*4 + 36*3 + 0*2 + 127*3) / 15 = 37
Trail = 25 + (37 * 75 / 255) = 36 bp → 1980 mvsol at entry=55000

Old system: still in EARLY phase with 8% (4400 mvsol) trail, waiting for max_hold
New system: tight 3.6% trail, signal death check active, gap decay consuming trail
Likely exits within 200-500ms when signals clearly show momentum is gone.
```

### 7.3 Scenario: Recovering Trade (Current: Tightens at Phase Boundary)

**Profile at t=14s:** 8 buys/s, 3 SOL/s flow, 15% sells, velocity 3 mvsol/s, 100ms gap

```
Scores: buy_rate=117, flow_rate=141, sell_press=145, velocity=95, gap=229
Composite = (117*3 + 141*4 + 145*3 + 95*2 + 229*3) / 15 = 148
Trail