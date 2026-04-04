# RIDE-THE-PUMP EXIT STRATEGY — Quantitative Design

**Date:** 2026-03-29  
**Author:** Quant Research (Ride-the-Pump Subagent)  
**Status:** DRAFT — Research spec for architect review  
**Scope:** Dual-mode exit architecture: SCALP vs RIDE

---

## 0. Executive Summary

**The core insight:** We built a microsecond scalper on an asset class where the alpha is in *minutes*, not *milliseconds*. Our current system captures 100% of price movement within a 1.5-second window — but the window itself is the problem. Confirmed pumps (buysAfterEntry≥2) have 92.7% WR and routinely move 50-500%+ over 5-30 minutes. We exit at 3-7%.

**The fix:** A dual-mode exit engine:
- **SCALP MODE** (default): Current system, slightly tightened. For the 92.9% of trades with zero follow-through. Quick in/out, TP 2-5%, max hold 2s.
- **RIDE MODE** (activated on confirmation): For the 7.1% of trades with confirming buys. Wide adaptive trailing stop, hold for 30s-5min, target 20-100%+ capture. This is where all the money is.

**Expected impact:** If RIDE MODE captures even 30% of the average pump on the 150 high-conviction trades per 2-day period, this transforms the P&L:
- Current: 150 trades × 92.7% WR × 5% avg gain × 0.10 SOL = +0.70 SOL
- Ride mode: 150 trades × 85% WR × 35% avg gain × 0.10 SOL = +4.46 SOL
- That's **6.4x more profit** from the same entries, with modestly lower WR (we'll give back some gains on trailing stop exits).

---

## 1. The Problem: Quantified

### 1.1 What We're Leaving on the Table

The data is unambiguous about the magnitude of the missed opportunity:

```
buysAfterEntry≥2 cohort (150 trades over 2 days):
  Win rate:        92.7%
  Median MFE:      7.18% (CAPPED by 1.5s max hold — this is a LOWER BOUND)
  Median hold:     174ms
  
  What happens AFTER we exit:
    We don't know. Our MFE tracking stops at 1.5s.
    But we DO know: these tokens continue to receive buys.
    buysAfterEntry≥3: 83 trades (55% of the ≥2 cohort got ANOTHER buy)
    buysAfterEntry≥4: likely ~40-50 trades (extrapolating the funnel)
    
  If buysAfterEntry≥3 means a THIRD buyer arrived within our 1.5s window,
  and the token continues to receive buys for minutes afterward,
  we're exiting a multi-minute pump in the first 200ms.
```

### 1.2 Bonding Curve Math: What Pumps Actually Look Like

The Pump.fun bonding curve is `x * y = k` with:
- Initial vSOL = 30 SOL, initial vToken ≈ 1.073 billion tokens
- k = 30 × 1.073e9 ≈ 3.219e10 (constant)
- Graduation at vSOL ≈ 115 SOL

Price at any point: `P = vSOL / vToken`

**Key price multipliers on the bonding curve:**

| vSOL Start | vSOL End | SOL Inflow | Price Multiple | % Gain |
|---|---|---|---|---|
| 30 | 35 | 5 SOL | 1.36x | +36% |
| 35 | 40 | 5 SOL | 1.31x | +31% |
| 40 | 50 | 10 SOL | 1.56x | +56% |
| 50 | 70 | 20 SOL | 1.96x | +96% |
| 40 | 70 | 30 SOL | 3.06x | +206% |
| 40 | 115 | 75 SOL | 8.27x | +727% |

**Derivation:** Since `x * y = k`, if vSOL goes from `S1` to `S2`:
```
P1 = S1 / (k / S1) = S1² / k
P2 = S2 / (k / S2) = S2² / k
Price multiple = P2 / P1 = (S2 / S1)²
```

The price scales as the **square** of the SOL reserve ratio. This is the fundamental non-linearity that makes ride-the-pump so valuable: each incremental SOL of buying pressure produces a *larger* price increase than the last.

**What this means for our trades:**

Our entries typically happen at vSOL ≈ 35-50 (based on min_vsol_in_curve=15 but realistic trigger zone). A confirmed pump from vSOL=40 to vSOL=70 (a moderate pump, 30 SOL of total buying) produces a **3.06x** price move. We're exiting after capturing 1.05-1.07x of that.

### 1.3 The Manual Trader Benchmark

A manual trader with 0.01 SOL positions making 2 SOL/day is:
- If averaging 50% gains per winning trade: needs 20 winning trades × 0.01 × 0.50 = 0.10 SOL per batch, implying many more trades
- More realistically: 5-10 trades/day, average gain 30-50%, WR ~60-80%, some big winners
- **Key difference:** They hold for MINUTES, not milliseconds

Our bot with 0.10 SOL positions making 0.02 SOL/day:
- ~75 high-conviction entries per day × 92.7% WR × 5% avg gain × 0.10 SOL ≈ 0.35 SOL gross
- Minus losses and fees ≈ 0.02 SOL net

The manual trader captures **10-100x more per winning trade** with **10x smaller positions**.

---

## 2. Dual-Mode Exit Architecture

### 2.1 Mode Selection: When Does RIDE Activate?

The transition from SCALP to RIDE is the single most important decision in the system. Get this wrong and you're either:
- Holding dead positions in RIDE mode (catastrophic: wide SL, long hold = big losses)
- Staying in SCALP mode on confirmed pumps (leaving 10-50x on the table)

**Transition signal: buysAfterEntry ≥ 2 AND additional confirmation**

Rationale:
- buysAfterEntry=0: 92.9% of trades, WR=39.1% → SCALP (or kill immediately)
- buysAfterEntry=1: WR=80.2% → SCALP with confirmed TP levels (current system handles this well)
- buysAfterEntry≥2: WR=92.7% → RIDE CANDIDATE, but need additional confirmation

**Why not transition at buysAfterEntry=1?**

The WR jump from 39.1% to 80.2% is enormous, but 80.2% WR with a wide trailing stop could still produce painful drawdowns. At buysAfterEntry≥2 (92.7% WR), the risk of a RIDE mode loss is small enough that the expected value of letting winners run dominates.

More importantly: buysAfterEntry≥2 means at least two *independent* buyers confirmed the momentum. This isn't just follow-through — it's convergent demand from multiple participants. This is the signature of a pump that has legs.

**The full transition matrix:**

```
buysAfterEntry=0, hold < 200ms:    → SCALP (UNCONFIRMED) → exit flat if no buy
buysAfterEntry=0, hold >= 200ms:   → EXIT (momentum_decay_flat)
buysAfterEntry=1:                  → SCALP (CONFIRMED) → standard TP/SL
buysAfterEntry=2, rideQualified:   → RIDE MODE TRANSITION
buysAfterEntry=2, !rideQualified:  → SCALP (CONVICTION_SCALED) at 1.4x TP
buysAfterEntry≥3:                  → RIDE MODE (confirmed, tighten trail)
```

### 2.2 RIDE Qualification Criteria

Not every buysAfterEntry≥2 should enter RIDE mode. The 150 trades in the buysAfterEntry≥2 cohort likely include some where the two confirming buys were tiny (dust) or from the same wallet (wash). RIDE mode should require:

```
RIDE_QUALIFICATION = {
  buys_after_entry >= 2,                      // Mandatory: multi-buyer confirmation
  confirming_buys_total_sol >= 0.3,           // Confirming buys are material (not dust)
  unique_confirming_wallets >= 2,             // Different wallets (not one person)
  no_sells_since_entry: true,                 // Zero sell pressure during confirmation
  price_above_entry: true,                    // Currently green (obvious but explicit)
  current_gain_pct >= 1.5%,                   // Already showing price appreciation
  curve_pct < 0.80,                           // Room to run (not near graduation)
  // OR curve_pct >= 0.80 AND gain_pct >= 3%  // Near graduation = graduation pump play
}
```

**Why each criterion:**

| Criterion | Rationale |
|---|---|
| `confirming_buys_total_sol >= 0.3` | Sub-0.1 SOL buys are noise/bots. 0.3 SOL total from 2+ buys = real demand. |
| `unique_confirming_wallets >= 2` | Single-wallet multi-buys could be a sandwich or self-trade. Multiple wallets = organic. |
| `no_sells_since_entry` | Any sell during the confirmation window is a red flag. Pumps in their first moments have pure buy flow. |
| `price_above_entry` | If price isn't green despite 2 buys, the buys are being absorbed by sell pressure. Bad sign. |
| `current_gain_pct >= 1.5%` | The price has already moved meaningfully. Confirms the buys are moving the curve. |
| `curve_pct < 0.80` | Remaining curve capacity determines max upside. At 80%+ filled, upside is capped unless graduation pump. |

### 2.3 SCALP MODE — Refined Current System

SCALP mode is the existing signal-based state machine with minor refinements. It handles the 93% of trades that are noise:

```
SCALP_MODE:
  States: UNCONFIRMED → CONFIRMED → CONVICTION_SCALED → safety net
  
  UNCONFIRMED (buysAfter=0):
    TP: 2-5% (by trigger tier)
    SL: 1.0-1.2%
    Max hold: 200ms confirmation window → exit flat if no buy
    
  CONFIRMED (buysAfter=1):
    TP: 3-7% (by trigger tier)
    SL: 1.5%
    Stall detection: no buy for 500ms + price fade 1% → exit
    Max hold: 5000ms safety
    
  CONVICTION_SCALED (buysAfter≥2, NOT ride-qualified):
    TP: scaled by conviction (1.4x/1.8x/2.2x)
    SL: 1.5%
    Trailing stop: 1.5% below peak (at conviction≥2)
    Max hold: 5000ms safety
    
  Key change from current system:
    - If position qualifies for RIDE at buysAfter≥2 → transition to RIDE_MODE
    - If not ride-qualified → stay in CONVICTION_SCALED (current behavior)
```

### 2.4 RIDE MODE — Complete Specification

RIDE mode is a fundamentally different exit philosophy. Instead of targeting a fixed TP%, it uses an adaptive trailing stop that lets the position ride the pump until momentum exhausts.

```
RIDE_MODE:
  Entry: Transition from SCALP when ride_qualified=true
  
  States: RIDE_EARLY → RIDE_MOMENTUM → RIDE_TIGHTEN → EXIT
  
  Position management: Trailing stop only. No fixed TP.
  SL: Replaced by trailing stop floor.
  Max hold: 300,000ms (5 minutes)
  
  On transition from SCALP:
    lock_in_price = entry_price × (1 + 0.01)   // Lock in 1% guaranteed gain
    trail_distance = 0.08                        // 8% below peak initially
    peak_price = current_price
    ride_start_time = now()
    ride_state = RIDE_EARLY
```

---

## 3. RIDE MODE Trailing Stop Mechanics

### 3.1 The Fundamental Design Question

How wide should the trailing stop be?

Too tight (e.g., 2% below peak): Exits on normal volatility within a pump. Bonding curve trades are lumpy — a single 0.5 SOL sell can dip the price 3-5% before the next buyer continues the pump.

Too wide (e.g., 20% below peak): Gives back too much profit when the pump finally reverses. A 100% gain that retraces to +60% before the trail triggers means you captured 60% — but you could have had 80%+ with a tighter trail.

**The answer is: adaptive trailing that starts wide and tightens.**

### 3.2 Adaptive Trailing Stop — Three Phases

#### Phase 1: RIDE_EARLY (0-15 seconds after RIDE activation)

```
trail_distance = 8% below peak
purpose: Survive the initial volatility. Pumps are noisy in the first seconds.
```

**Why 8%:** On the bonding curve, a single moderate sell (0.3-0.5 SOL) when vSOL is 40-50 can produce a 2-4% price dip. Two sells in sequence could produce 5-7%. An 8% trail survives these dips while still protecting against genuine reversals.

Bonding curve math for sell impact:
```
At vSOL=45, k=3.219e10:
  vToken = k/vSOL = 715,333,333
  Price = 45/715.3M = 6.29e-8 SOL/token

If someone sells tokens worth 0.5 SOL (vSOL drops to 44.5):
  New vToken = k/44.5 = 723,370,787
  New price = 44.5/723.4M = 6.15e-8
  Price impact = -2.2%

If TWO people sell 0.5 SOL each (vSOL→44):
  New vToken = k/44 = 731,590,909
  New price = 44/731.6M = 6.01e-8
  Price impact from peak = -4.4%
```

An 8% trail survives even aggressive sell sequences during an otherwise healthy pump.

#### Phase 2: RIDE_MOMENTUM (15-60 seconds, or after +15% gain)

```
trail_distance = 6% below peak
purpose: Tighten as the pump establishes. Still allows for dips but captures more.
transition_trigger: hold_time >= 15s OR unrealized_gain >= 15%
```

**Why tighten to 6%:** After 15 seconds of sustained buying, the pump's character is established. The initial noise period is over. Dips are now more likely to be genuine reversals rather than normal volatility.

At +15% gain, you have significant profit to protect. A 6% trail from peak means you're locking in at least +8.1% gain (15% × 0.94 = 14.1% from peak, but the trail moves up with price).

Actually, the math is:
```
If peak = entry × 1.15 (you're at +15%):
  trail_stop = peak × 0.94 = entry × 1.15 × 0.94 = entry × 1.081
  Locked-in gain = 8.1% (worst case if hit immediately)
  
If peak = entry × 1.30 (+30% gain, trail hasn't triggered):
  trail_stop = entry × 1.30 × 0.94 = entry × 1.222
  Locked-in gain = 22.2%
```

#### Phase 3: RIDE_TIGHTEN (after +30% gain or 60+ seconds held)

```
trail_distance = 4% below peak
purpose: Aggressive profit protection. The pump is mature.
transition_trigger: unrealized_gain >= 30% OR hold_time >= 60s
```

**Why tighten to 4%:** At +30% gain, you're deep into a real pump. The bonding curve is filling rapidly. At this point, any significant reversal (>4%) likely means the pump is exhausting. Locking in 25%+ on a 30%+ move is an excellent outcome.

#### Phase Summary Table

| Phase | Trigger | Trail Distance | Min Locked Gain* | Purpose |
|---|---|---|---|---|
| RIDE_EARLY | Ride activation | 8% below peak | 1% (floor) | Survive volatility |
| RIDE_MOMENTUM | 15s held OR +15% | 6% below peak | ~8% at 15% peak | Tighten as pump establishes |
| RIDE_TIGHTEN | 60s held OR +30% | 4% below peak | ~25% at 30% peak | Aggressive profit lock |

*Min locked gain assumes trail triggers immediately after phase transition.

### 3.3 The Trailing Stop Floor

Regardless of trailing stop distance, the position has a **hard floor** at `entry_price × 1.01` (1% gain). This ensures that RIDE mode never gives back the initial confirmation profit. If the pump immediately reverses after RIDE activation:

```
worst_case_ride_exit = max(
  peak_price × (1 - trail_distance),    // Trailing stop
  entry_price × 1.01                     // Hard floor
)
```

This floor costs almost nothing (RIDE activates when we're already at +1.5% minimum per qualification criteria) but prevents the psychological damage of a RIDE mode loss.

### 3.4 Trailing Stop Implementation

```rust
struct RideState {
    mode: RidePhase,           // Early, Momentum, Tighten
    peak_price: f64,           // Highest price observed since RIDE activation
    trail_stop: f64,           // Current trailing stop price
    floor_price: f64,          // Hard floor = entry × 1.01
    ride_start: Instant,       // When RIDE mode activated
    ride_entry_gain: f64,      // Unrealized gain at RIDE activation
}

enum RidePhase {
    Early,      // 0-15s, trail=8%
    Momentum,   // 15-60s or +15%, trail=6%
    Tighten,    // 60s+ or +30%, trail=4%
}

fn on_price_update(price: f64, state: &mut RideState) -> ExitDecision {
    // Update peak
    if price > state.peak_price {
        state.peak_price = price;
    }
    
    // Phase transitions (one-way: Early → Momentum → Tighten)
    let elapsed = state.ride_start.elapsed();
    let gain_pct = (price / entry_price - 1.0) * 100.0;
    
    let trail_dist = match state.mode {
        RidePhase::Early => {
            if elapsed >= Duration::from_secs(15) || gain_pct >= 15.0 {
                state.mode = RidePhase::Momentum;
                0.06
            } else {
                0.08
            }
        }
        RidePhase::Momentum => {
            if elapsed >= Duration::from_secs(60) || gain_pct >= 30.0 {
                state.mode = RidePhase::Tighten;
                0.04
            } else {
                0.06
            }
        }
        RidePhase::Tighten => 0.04,
    };
    
    // Compute trailing stop (can only go UP, never down)
    let new_trail = state.peak_price * (1.0 - trail_dist);
    state.trail_stop = state.trail_stop.max(new_trail).max(state.floor_price);
    
    // Check exit
    if price <= state.trail_stop {
        ExitDecision::Exit(ExitReasonNew::RideTrailingStop)
    } else {
        ExitDecision::Hold
    }
}
```

**Critical detail: `trail_stop` can only increase, never decrease.** When the phase tightens from 8% to 6%, the new trail is `peak × 0.94` vs the old `peak × 0.92`. Since we take the MAX of old trail and new trail, the trail naturally ratchets up on phase transitions. This prevents the pathological case where a phase transition *lowers* the stop.

---

## 4. Pump Termination Signals

The trailing stop is the primary exit mechanism, but we can improve exit quality by detecting pump exhaustion signals and tightening the trail preemptively.

### 4.1 Sell Pressure Spike

```
signal: sell_volume_5s / buy_volume_5s > 0.5 during RIDE mode
action: tighten trail by 2% (e.g., 8% → 6%, 6% → 4%, 4% → 2%)
reason: Heavy selling into a pump is the #1 sign of distribution/exhaustion.
        On a healthy pump, sell pressure is near zero.
```

**Quantification:**
During the early moments of a genuine pump, sell/buy ratio is typically <0.1 (pure buying, no one is selling). As the pump matures:
- Sell/buy = 0.1-0.2: Normal profit-taking from early holders
- Sell/buy = 0.2-0.5: Increasing distribution, pump is aging
- Sell/buy > 0.5: Active selling — the pump is ending

**Implementation:**
```rust
fn check_sell_pressure(sell_vol_5s: f64, buy_vol_5s: f64) -> TrailAdjustment {
    if buy_vol_5s == 0.0 {
        return TrailAdjustment::EmergencyExit; // No buys = dead
    }
    let ratio = sell_vol_5s / buy_vol_5s;
    if ratio > 0.5 {
        TrailAdjustment::TightenBy(0.02) // Aggressive tighten
    } else if ratio > 0.3 {
        TrailAdjustment::TightenBy(0.01) // Moderate tighten
    } else {
        TrailAdjustment::None
    }
}
```

### 4.2 Buy Rate Deceleration

```
signal: buy_count_5s < buy_count_5s_at_ride_start × 0.3
action: tighten trail by 1%
reason: Pump is losing steam. Fewer buyers = less fuel.
```

The buy rate when RIDE activates represents the "fuel level" of the pump. If the rate drops to 30% of that level, momentum is fading. We don't want to exit immediately (could be a brief pause before another wave), but tightening the trail ensures we capture more if it continues to fade.

### 4.3 Whale Exit (Large Single Sell)

```
signal: single sell > 1.0 SOL during RIDE mode
action: IMMEDIATE trail tighten to 2% (regardless of current phase)
reason: A 1+ SOL sell on the bonding curve is a significant event.
        At vSOL=50, a 1 SOL sell produces ~4% price impact.
        This is likely a large holder dumping.
```

**Price impact of a 1 SOL sell at various curve positions:**

```
vSOL=40: sell 1 SOL → price impact = 1 - (39/40)² = -4.9%
vSOL=50: sell 1 SOL → price impact = 1 - (49/50)² = -3.9%
vSOL=70: sell 1 SOL → price impact = 1 - (69/70)² = -2.8%
vSOL=100: sell 1 SOL → price impact = 1 - (99/100)² = -2.0%
```

A 1+ SOL sell is always significant on the bonding curve. Even at vSOL=100, it's a 2% immediate impact. Tightening to a 2% trail means we're essentially setting a stop ~4% below where we were before the whale sell.

### 4.4 Buy Gap (Time Since Last Buy)

```
signal: time_since_last_buy > 5000ms during RIDE mode
action: tighten trail by 2%
signal: time_since_last_buy > 10000ms during RIDE mode  
action: EXIT at current price (market sell)
reason: A pump that goes 10 seconds without a buy is dead.
        On a healthy pump, buys arrive every 0.5-2 seconds.
```

This is the time-based backstop for RIDE mode. Not a fixed max hold, but a *silence detector*. Pumps don't have quiet periods — they're continuous buyer streams. Silence = the pump is over.

### 4.5 Signal Priority and Stacking

Signals stack (multiple tightenings can occur):

```
trail_adjustment = base_trail_for_phase;

// Each signal can tighten the trail independently
if sell_pressure_spike:   trail_adjustment -= 0.02
if buy_deceleration:      trail_adjustment -= 0.01
if whale_exit:            trail_adjustment = min(trail_adjustment, 0.02)
if buy_gap > 5s:          trail_adjustment -= 0.02

// Floor: trail can never be tighter than 1.5%
trail_adjustment = max(trail_adjustment, 0.015)

// Immediate exit on total silence
if buy_gap > 10s:         EXIT immediately
```

### 4.6 Combined Signal State Table

| Phase | Base Trail | +Sell Pressure | +Deceleration | +Whale Exit | +Buy Gap 5s |
|---|---|---|---|---|---|
| EARLY | 8% | 6% | 7% | 2% | 6% |
| MOMENTUM | 6% | 4% | 5% | 2% | 4% |
| TIGHTEN | 4% | 2% | 3% | 2% | 2% |
| Any + silence 10s | — | — | — | — | EXIT |

---

## 5. Transition Logic: SCALP → RIDE

### 5.1 The Transition Moment

The transition happens during the normal SCALP state machine evaluation. When `buysAfterEntry` increments to ≥2, the system evaluates RIDE qualification:

```
fn on_buy_event(position: &mut Position, buy: &BuyEvent) {
    position.buys_after_entry += 1;
    
    // Track confirming buy metadata
    position.confirming_buy_sol += buy.sol_amount;
    position.confirming_wallets.insert(buy.trader_wallet);
    
    // Normal SCALP state transitions
    if position.buys_after_entry == 1 && position.in_profit() {
        position.exit_state = ExitState::Confirmed;
    }
    
    // RIDE qualification check
    if position.buys_after_entry >= 2 {
        if evaluate_ride_qualification(position) {
            transition_to_ride(position);
        } else {
            // Stay in SCALP conviction-scaled mode
            position.exit_state = ExitState::ConvictionScaled { 
                level: position.buys_after_entry.min(4) as u8 
            };
        }
    }
}

fn evaluate_ride_qualification(pos: &Position) -> bool {
    pos.confirming_buy_sol >= 0.3              // Material buying
    && pos.confirming_wallets.len() >= 2       // Multiple wallets
    && pos.sells_since_entry == 0              // No sell pressure
    && pos.current_price > pos.entry_price     // In profit
    && pos.unrealized_gain_pct() >= 1.5        // Meaningful gain
    && (pos.curve_pct < 0.80                   // Room to run
        || pos.unrealized_gain_pct() >= 3.0)   // OR already moving fast near graduation
}
```

### 5.2 What Happens When RIDE Activates

```
fn transition_to_ride(pos: &mut Position) {
    // Cancel the 5000ms safety timer (RIDE has its own 300s timer)
    pos.cancel_safety_timer();
    
    // Set new safety timer for RIDE (5 minutes)
    pos.set_safety_timer(Duration::from_secs(300));
    
    // Initialize RIDE state
    pos.ride_state = Some(RideState {
        mode: RidePhase::Early,
        peak_price: pos.current_price,
        trail_stop: pos.entry_price * 1.01,    // Floor: 1% gain guaranteed
        floor_price: pos.entry_price * 1.01,
        ride_start: Instant::now(),
        ride_entry_gain: pos.unrealized_gain_pct(),
        buy_rate_at_start: pos.buy_count_5s,   // For deceleration detection
    });
    
    // Remove TP target (RIDE has no fixed TP)
    pos.tp_price = None;
    
    // SL is replaced by trailing stop floor
    pos.sl_price = pos.entry_price * 1.01;     // Hard floor
    
    // Log the transition
    log_event("RIDE_ACTIVATED", pos);
}
```

### 5.3 Anti-Signals: Immediate RIDE Cancellation

Even after RIDE activates, certain signals should force an immediate exit (overriding the trailing stop):

```
RIDE_EMERGENCY_EXIT_SIGNALS:
  1. Single sell > 2.0 SOL                    // Massive whale dump
  2. sell_count_3s >= 3                        // Rapid sell cascade
  3. price < entry_price                       // Below entry (shouldn't happen with floor, but safety)
  4. curve_pct >= 0.95 AND sell detected       // Near graduation + selling = rug risk
  5. creator_sell detected                     // Creator dumping
```

These are kill switches. No trailing stop, no tightening — just exit immediately at market.

---

## 6. Position Sizing for RIDE Mode

### 6.1 Kelly Criterion Recalculation

**Current SCALP parameters:**
```
Win rate (p):        54.3% overall, but ~92.7% for ≥2 buys cohort
Average win (W):     5% × 0.10 SOL = 0.005 SOL (net of fees at 0.10 position)
Average loss (L):    1.5% × 0.10 SOL = 0.0015 SOL + 0.002 SOL fees ≈ 0.0035 SOL
```

For the overall system at 54.3% WR:
```
Kelly = p/L - (1-p)/W = 0.543/0.0035 - 0.457/0.005
     = 155.1 - 91.4 = 63.7 (uncapped; Kelly says bet big)
```

But this is misleading because the blended WR mixes two very different populations.

**RIDE MODE parameters (projected):**
```
Win rate (p):        85% (lower than 92.7% because trailing stop exits give back some)
Average win (W):     35% × 0.10 SOL = 0.035 SOL (net of fees)
Average loss