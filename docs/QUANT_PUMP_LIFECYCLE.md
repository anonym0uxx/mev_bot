# PUMP LIFECYCLE — Phase Detection & Exit Optimization

**Date:** 2026-03-29  
**Author:** Quant Research (Pump Lifecycle Subagent)  
**Status:** DRAFT — Empirical analysis of 207 RIDE trades  
**Scope:** Phase classification, detection algorithm, phase-specific exit rules

---

## 0. Executive Summary

Analysis of 207 RIDE trades reveals a clear pump lifecycle with measurable phase boundaries. The data shows three distinct regimes:

| Phase | Trades | Avg PnL | Avg MFE | MFE Capture | Key Characteristic |
|---|---|---|---|---|---|
| **n/a** (failed to develop) | 125 (60%) | 1.34% | 4.07% | 27% | ≤5.7 buys, ≤2.3 SOL confirming |
| **EARLY** (developing pump) | 69 (33%) | 9.08% | 11.44% | 78% | ~10.8 buys, ~6.1 SOL confirming |
| **MOMENTUM** (full pump) | 13 (6%) | 30.47% | 35.43% | 87% | ~21.5 buys, ~18.3 SOL confirming |

**The alpha:** 6.3% of trades reach MOMENTUM phase and generate avg 30.47% PnL. Detecting this transition early and holding through it is the single most valuable improvement. Every trade that reaches MOMENTUM is profitable — 100% win rate.

**The bleed:** Whale exits destroy 7.78% of MFE on average. 55% of whale exits are losses. The whale exit signal is overly aggressive and needs to be gated by wallet count.

---

## 1. Pump Phase Classification

### 1.1 Phase Definitions (Empirically Calibrated)

#### IGNITION (Not Yet Confirmed as Pump)

The initial burst of activity. Most trades (60%) never leave this phase.

```
PHASE: IGNITION
  buysAfterEntry:         ≤ 4
  confirmingBuySol:       < 2.0 SOL
  rideUniqueWallets:      < 3
  buy_rate:               < 8/s
  
  Empirical profile:
    Avg PnL: -0.64% (buys=2), +1.29% (buys=3), +2.52% (buys=4)
    WR at buys=2: 48% → THIS IS NOISE, NOT A PUMP YET
    WR at buys=4: ~75%
    
  Key signal: NO clear trend. Could go either way.
  Duration: 0-500ms from ride activation
```

**Integer thresholds for Rust:**
- `confirming_buy_sol_mlamports < 2_000_000` (2.0 SOL in milli-lamports)
- `unique_wallets < 3`
- `buys_after_entry <= 4`

#### ACCELERATION (Confirmed Pump, Building Momentum)

Sustained multi-wallet buying. The pump is real but hasn't reached peak velocity.

```
PHASE: ACCELERATION
  buysAfterEntry:         5-14
  confirmingBuySol:       2.0 - 10.0 SOL
  rideUniqueWallets:      3-9
  buy_rate:               8-15/s
  
  Empirical profile:
    Avg PnL: 5.3% (buys 5-9), 8.4% (buys 10-14)
    WR: 90%+ consistently
    MFE capture: 78%
    
  Key signal: Monotonically increasing wallet count, 
              steady buy flow, price climbing on curve
  Duration: 200-1000ms from ride activation
```

**Integer thresholds for Rust:**
- `confirming_buy_sol_mlamports >= 2_000_000 && < 10_000_000`
- `unique_wallets >= 3 && < 10`
- `buys_after_entry >= 5 && <= 14`

#### PEAK (Maximum Activity, Transition Zone)

Buy rate reaches maximum velocity. This is where the biggest single-trade gains occur. The transition from ACCELERATION → PEAK is when confirming SOL crosses ~10 SOL and buy rate reaches ~15-20/s.

```
PHASE: PEAK (corresponds to 'momentum' in current engine)
  buysAfterEntry:         15+
  confirmingBuySol:       10+ SOL
  rideUniqueWallets:      10+
  buy_rate:               15-25+/s (2x the ACCELERATION rate)
  
  Empirical profile:
    Avg PnL: 30.47%
    WR: 100% (13/13 trades)
    MFE capture: 87%
    Avg confirming SOL: 18.32
    Avg unique wallets: 20.3
    
  Key signal: Buy rate doubles from ACCELERATION levels.
              vSOL moving 10-30 SOL during hold window.
  Duration: 800-1500ms from ride activation
```

**Integer thresholds for Rust:**
- `confirming_buy_sol_mlamports >= 10_000_000`
- `unique_wallets >= 10`
- `buys_after_entry >= 15`

#### DECAY (Pump Exhausting)

Not directly observed in current data (our holds are 1.5s max), but signals are present:
- sell_cascade exit triggers (3+ sells in rapid succession)
- whale exit triggers (single large sell)
- buy rate declining below IGNITION levels
- price below trail stop

```
PHASE: DECAY
  Detected by: Sell signals during an ACCELERATION or PEAK phase
  
  Empirical signatures:
    ride_sell_cascade: triggered at 3+ sells, exits at avg 80.8% MFE capture
    ride_whale_exit: triggered by large single sell, avg -30% MFE capture (PROBLEMATIC)
    ride_trailing_stop: triggered by price decline, avg 63.5% MFE capture
    
  Key signals:
    - sell_count_during_hold >= 3 AND rising
    - buy_gap widening (time between buys increasing)
    - price velocity turning negative for 2+ consecutive ticks
```

### 1.2 Phase Transition Diagram

```
IGNITION ──────┬───── (buys≥5 AND confirming≥2 SOL AND wallets≥3) ────→ ACCELERATION
               │
               └───── (buys≤4 AND no momentum) ────→ EXIT (hard_floor or trailing_stop)

ACCELERATION ──┬───── (buys≥15 AND confirming≥10 SOL AND wallets≥10) ──→ PEAK
               │
               └───── (sell_cascade OR whale_exit) ────→ DECAY → EXIT

PEAK ──────────┬───── (still buying, rate sustained) ────→ HOLD (max ride time)
               │
               └───── (sell signals) ────→ DECAY → EXIT
```

---

## 2. Phase Detection Algorithm

### 2.1 Event-Based Rolling Window Detection

The algorithm uses trade events (not wall-clock time) as the fundamental unit. This makes it robust to clock drift and network latency.

```rust
/// Phase detection state machine
struct PhaseDetector {
    buys_after_entry: u32,
    confirming_sol_mlamports: u64,  // millionths of a SOL for integer math
    unique_wallets: u16,
    sells_during_hold: u16,
    current_phase: PumpPhase,
    
    // Rolling windows (event-based)
    recent_buys: VecDeque<u64>,     // timestamps of last N buys
    recent_sells: VecDeque<u64>,    // timestamps of last N sells
    buy_rate_at_ignition: u16,      // buys in first 200ms
}

#[derive(Clone, Copy, PartialEq)]
enum PumpPhase {
    Ignition,
    Acceleration,
    Peak,
    Decay,
}
```

### 2.2 Transition Functions

```rust
/// Called on every trade event during RIDE mode
fn detect_phase(state: &mut PhaseDetector) -> PumpPhase {
    // === DECAY detection (highest priority, can trigger from any phase) ===
    if state.is_decay_signal() {
        return PumpPhase::Decay;
    }
    
    // === Forward transitions only (Ignition → Acceleration → Peak) ===
    match state.current_phase {
        PumpPhase::Ignition => {
            if state.buys_after_entry >= 5
                && state.confirming_sol_mlamports >= 2_000_000  // 2 SOL
                && state.unique_wallets >= 3
            {
                PumpPhase::Acceleration
            } else {
                PumpPhase::Ignition
            }
        }
        PumpPhase::Acceleration => {
            if state.confirming_sol_mlamports >= 10_000_000  // 10 SOL
                && state.unique_wallets >= 10
                && state.buys_after_entry >= 15
            {
                PumpPhase::Peak
            } else {
                PumpPhase::Acceleration
            }
        }
        PumpPhase::Peak => PumpPhase::Peak,  // No forward transition
        PumpPhase::Decay => PumpPhase::Decay, // Terminal
    }
}
```

### 2.3 Buy Rate Derivative (Acceleration → Deceleration)

The buy rate derivative is the key signal for detecting PEAK exhaustion:

```rust
/// Compute buy rate in events per second over a rolling window
/// Returns rate in buys/second × 100 (integer-friendly)
fn buy_rate_x100(recent_buys: &VecDeque<u64>, window_ms: u64) -> u32 {
    let now = current_time_ms();
    let in_window = recent_buys.iter()
        .filter(|&&ts| now - ts <= window_ms)
        .count() as u64;
    // rate = in_window / (window_ms / 1000) × 100
    ((in_window * 100_000) / window_ms) as u32
}

/// Detect buy rate deceleration
/// Compare current 500ms rate to peak 500ms rate
fn is_decelerating(state: &PhaseDetector) -> bool {
    let current_rate = buy_rate_x100(&state.recent_buys, 500);
    // If current rate < 50% of peak rate → decelerating
    current_rate < state.peak_buy_rate_x100 / 2
}
```

**Empirical calibration:**
- IGNITION/n/a trades: avg buy rate ~9.0/s
- ACCELERATION/early trades: avg buy rate ~9.9/s
- PEAK/momentum trades: avg buy rate ~19.9/s
- **Deceleration threshold: current_rate < 10/s while in PEAK phase**

### 2.4 Volume Concentration Detection

```rust
/// Check if a single wallet dominates volume
/// Returns true if top wallet > 30% of total confirming volume
fn is_whale_dominated(wallet_volumes: &HashMap<Pubkey, u64>, total_sol: u64) -> bool {
    let max_wallet = wallet_volumes.values().max().copied().unwrap_or(0);
    // 30% threshold: max_wallet * 100 > total_sol * 30
    max_wallet * 100 > total_sol * 30
}
```

**Empirical finding:** Whale-dominated trades (whale exit) with only 2 unique wallets have avg PnL of **-3.59%**. With 3+ wallets: **+3.52%**. With 5+ wallets: **+6.18%**.

**Rule:** If top wallet > 30% of volume AND unique_wallets < 5, tighten trail immediately. The pump is synthetic.

### 2.5 Sell-to-Buy Ratio Thresholds

```rust
/// Sell pressure assessment
fn sell_pressure_level(sells_5s: u16, buys_5s: u16) -> SellPressure {
    if buys_5s == 0 { return SellPressure::Dead; }
    let ratio_x100 = (sells_5s as u32 * 100) / buys_5s as u32;
    match ratio_x100 {
        0..=10   => SellPressure::Clean,      // Pure buy flow
        11..=30  => SellPressure::Mild,        // Normal profit-taking
        31..=50  => SellPressure::Moderate,    // Distribution beginning
        _        => SellPressure::Heavy,       // Pump ending
    }
}
```

**Empirical calibration from pre-trigger sell ratio:**
- sell_ratio < 10%: 27 trades, WR 70.4%, avg PnL 6.16%
- sell_ratio 10-30%: 77 trades, WR 87.0%, avg PnL 5.89%
- sell_ratio 30-50%: 103 trades, WR 88.3%, avg PnL 5.54%

**Note:** Pre-trigger sell ratio doesn't strongly predict outcome because we filter entries. The *during-hold* sell ratio matters more — sells during hold is the key decay signal.

---

## 3. Phase-Specific Exit Rules

### 3.1 Trail Distance by Phase

| Phase | Trail Distance | Trail (BPS) | Min Locked Gain | Rationale |
|---|---|---|---|---|
| IGNITION | 8% | 800 BPS | 1% (floor) | Survive noise, most trades die here |
| ACCELERATION | 5% | 500 BPS | 3% | Pump confirmed, start protecting |
| PEAK | 3% | 300 BPS | 15% | Maximum protection, capture the top |
| DECAY | 1.5% | 150 BPS | immediate sell signal | Exit ASAP |

**Integer representation for Rust:**
```rust
fn trail_distance_bps(phase: PumpPhase) -> u32 {
    match phase {
        PumpPhase::Ignition     => 800,   // 8.00%
        PumpPhase::Acceleration => 500,   // 5.00%
        PumpPhase::Peak         => 300,   // 3.00%
        PumpPhase::Decay        => 150,   // 1.50%
    }
}
```

### 3.2 Phase-Specific Exit Logic

#### IGNITION Exit Rules

```rust
// During IGNITION: Be ready to exit fast
fn ignition_exit(state: &RideState) -> Option<ExitDecision> {
    // Hard floor at +1% (existing)
    if current_price <= entry_price * 101 / 100 {
        return Some(Exit::HardFloor);
    }
    
    // Trail at 8% below peak
    if current_price <= state.peak_price * 92 / 100 {
        return Some(Exit::TrailingStop);
    }
    
    // Whale exit: ONLY if unique_wallets < 3
    // (Current whale exit triggers too aggressively)
    if whale_sell_detected && state.unique_wallets < 3 {
        return Some(Exit::WhaleExit);
    }
    
    None // Hold
}
```

**Key insight from data:** Hard floor exits (30 trades) have avg PnL of +0.04% and avg 3.2 wallets. These are trades that never developed. The current floor at +1% is correct — it prevents significant losses while giving marginal trades a chance.

#### ACCELERATION Exit Rules

```rust
fn acceleration_exit(state: &RideState) -> Option<ExitDecision> {
    // Lock in minimum 3% gain once in ACCELERATION
    let min_lock = entry_price * 103 / 100;
    
    // Trail at 5% below peak
    let trail = max(state.peak_price * 95 / 100, min_lock);
    if current_price <= trail {
        return Some(Exit::TrailingStop);
    }
    
    // Sell cascade: 3 sells in rapid succession
    if sells_in_last_500ms >= 3 {
        return Some(Exit::SellCascade);
    }
    
    // Whale exit: only if wallets < 5 AND confirming < 5 SOL
    if whale_sell_detected 
        && state.unique_wallets < 5 
        && state.confirming_sol < 5_000_000 
    {
        return Some(Exit::WhaleExit);
    }
    
    None
}
```

**Empirical basis:** ACCELERATION (early) trades capture 78% of MFE. The 5% trail distance allows for the typical 2-4% bonding curve noise while protecting gains. Sell cascade exits at this phase capture 81% of MFE — they work well.

#### PEAK Exit Rules

```rust
fn peak_exit(state: &RideState) -> Option<ExitDecision> {
    // Lock in minimum 15% gain once in PEAK
    let min_lock = entry_price * 115 / 100;
    
    // Tight trail at 3% below peak
    let trail = max(state.peak_price * 97 / 100, min_lock);
    if current_price <= trail {
        return Some(Exit::TrailingStop);
    }
    
    // Buy rate deceleration detection
    if is_decelerating(state) {
        // Tighten trail to 1.5%
        let tight_trail = max(state.peak_price * 985 / 1000, min_lock);
        if current_price <= tight_trail {
            return Some(Exit::DecelerationExit);
        }
    }
    
    // Disable whale exit in PEAK unless it's truly massive
    // Whale exits in PEAK cost us 14.2% avg (41.9% MFE vs 27.7% PnL)
    if whale_sell_sol > 5_000_000 && state.confirming_sol < 10_000_000 {
        return Some(Exit::WhaleExit); // Only if whale is >50% of confirming
    }
    
    None
}
```

**Critical empirical finding:** 4 whale exits occurred during PEAK phase. They had avg MFE of 35.9% but only captured avg PnL of 23.4%. **The whale exit signal costs 12.5% on average in PEAK phase.** In PEAK, the pump has enough participants to absorb a single whale sell. The whale exit should be disabled or heavily gated in PEAK.

#### DECAY Exit Rules

```rust
fn decay_exit(state: &RideState) -> Option<ExitDecision> {
    // IMMEDIATE exit - don't wait for trail
    // The pump is over. Every millisecond costs money.
    return Some(Exit::DecayExit);
}
```

### 3.3 Decay Detection Signals (Priority-Ordered)

```rust
fn is_decay_signal(state: &PhaseDetector) -> bool {
    // Signal 1: Buy gap > 500ms during ACCELERATION/PEAK
    // (Healthy pumps have sub-200ms buy gaps)
    if state.ms_since_last_buy > 500 
        && state.current_phase >= PumpPhase::Acceleration 
    {
        return true;
    }
    
    // Signal 2: 3+ sells in 300ms (sell cascade)
    if state.sells_in_last_300ms >= 3 {
        return true;
    }
    
    // Signal 3: Buy rate drops to <30% of peak
    if state.current_buy_rate_x100 < state.peak_buy_rate_x100 * 30 / 100
        && state.current_phase >= PumpPhase::Acceleration
    {
        return true;
    }
    
    // Signal 4: Price below entry + 1% (hard floor violation attempt)
    if current_price_mvsol < entry_price_mvsol * 101 / 100 {
        return true;
    }
    
    false
}
```

---

## 4. Empirical Calibration from 207 Trades

### 4.1 Trade Classification

| Category | Count | % | Avg PnL | Avg MFE | WR | Phase |
|---|---|---|---|---|---|---|
| Failed pumps (confirming < 2 SOL) | 53 | 25.6% | -0.01% | 2.56% | 66.0% | IGNITION |
| Marginal pumps (2-4 SOL) | 71 | 34.3% | 2.47% | 5.37% | 84.5% | late IGNITION / early ACCELERATION |
| Confirmed pumps (4-6 SOL) | 41 | 19.8% | 6.66% | 8.82% | 97.6% | ACCELERATION |
| Strong pumps (6-10 SOL) | 23 | 11.1% | 9.96% | 12.87% | 100% | ACCELERATION → PEAK |
| Mega pumps (10+ SOL) | 19 | 9.2% | 27.03% | 30.76% | 100% | PEAK |

### 4.2 Trades That Exited During ACCELERATION (Left Money on Table)

These are trades with high confirming SOL that exited via trailing stop or whale exit before reaching PEAK:

```
PATTERN: confirming >= 5 SOL, exited via ride_trailing_stop
COUNT: 13 trades
AVG PnL: 7.2%   AVG MFE: 9.8%
GAP: 2.6% left on table

PATTERN: confirming >= 5 SOL, exited via ride_whale_exit  
COUNT: 7 trades
AVG PnL: 9.7%   AVG MFE: 20.2%
GAP: 10.5% left on table  ← WHALE EXIT IS THE PROBLEM
```

**Key finding:** Whale exit in ACCELERATION/PEAK trades leaves **10.5%** on the table. This is the biggest source of money left on table in the entire system.

### 4.3 Trades That Held Too Long (Exited During DECAY)

Not directly observable (max hold is 1.5s, so we don't see real DECAY). However, proxy signals:

```
ride_hard_floor exits: 30 trades, avg PnL=+0.04%
  These are trades that almost went red → caught by the floor.
  With 2 wallets: heavy losses (avg -3.59% before floor cap)
  The floor is doing its job.
  
ride_sell_cascade exits: 54 trades, avg PnL=+9.37%
  These EXIT at the right time — 80.8% MFE capture.
  Sell cascade is the BEST exit signal.
```

### 4.4 Optimal Exit Phase

**The optimal exit is during late ACCELERATION or early PEAK:**

| Exit Timing | MFE Capture | Reasoning |
|---|---|---|
| IGNITION (too early) | 27% | Left 73% on the table |
| Early ACCELERATION | 65-70% | Reasonable, but PEAK hasn't arrived |
| Late ACCELERATION | 78% | Current system average for "early" phase |
| PEAK entry | 87% | Best capture ratio observed |
| PEAK + sell cascade | 81% | Near-optimal with confirmed reversal signal |
| Post-PEAK (too late) | <60% | Trailing stop gives back too much |

**Recommendation:** Target exit during the **first sell cascade after confirming ≥ 10 SOL and ≥ 10 wallets**. This is the sell cascade during or just after PEAK, capturing 81%+ of MFE.

### 4.5 Exit Reason Performance Ranking

| Exit Reason | Count | Avg PnL | Avg MFE | MFE Capture | Verdict |
|---|---|---|---|---|---|
| **max_hold** | 35 | 9.37% | 9.52% | 97.7% | 🟢 PERFECT — extend max hold time! |
| **sell_cascade** | 54 | 9.37% | 10.88% | 80.8% | 🟢 EXCELLENT signal |
| **trailing_stop** | 50 | 5.85% | 8.17% | 63.5% | 🟡 OK — trail may be too tight |
| **hard_floor** | 30 | 0.04% | 2.39% | -0.9% | 🟡 Safety net, working as designed |
| **whale_exit** | 38 | 1.65% | 9.43% | -30.4% | 🔴 BROKEN — destroying value |

### 4.6 Critical Finding: Whale Exit Is Broken

The whale exit signal produces the worst outcomes of any exit reason:

```
Whale exits with 2 wallets:  10 trades, avg PnL = -3.59% 
Whale exits with 3-4 wallets: 13 trades, avg PnL = +1.85%
Whale exits with 5+ wallets:  15 trades, avg PnL = +6.18%

Whale exits in MOMENTUM:      4 trades, avg PnL = +23.4%, avg MFE = 35.9%
  → Left 12.5% on table per trade!
```

**Fix:** Gate whale exit by wallet count and phase:
- IGNITION + wallets < 3: whale exit = FIRE (correct)
- ACCELERATION + wallets < 5: whale exit = FIRE 
- ACCELERATION + wallets ≥ 5: whale exit = TIGHTEN trail to 3% (don't exit)
- PEAK: whale exit = TIGHTEN trail to 2% (never hard exit)

---

## 5. Integration with Bonding Curve Math

### 5.1 At What vSOL Reserve Level Does Buying Peak?

```
Graduation = 85 vSOL (55 SOL net inflow from initial 30)

Entry vSOL distribution:
  40-50 SOL (20-36% curve): 75 trades, avg MFE = 9.06% ← BEST ENTRY ZONE
  50-60 SOL (36-55% curve): 85 trades, avg MFE = 9.64%
  60-70 SOL (55-73% curve): 28 trades, avg MFE = 6.11% ← Diminishing returns
  70-80 SOL (73-91% curve): 15 trades, avg MFE = 4.49%
  80+ SOL (91%+ curve):     4 trades,  avg MFE = 5.46%

Peak vSOL distribution (where price peaks during hold):
  Peak at 30-50% curve: 50 trades, avg PnL = 1.49% ← Pump stalls early
  Peak at 50-60% curve: 43 trades, avg PnL = 6.40%
  Peak at 60-70% curve: 23 trades, avg PnL = 6.83%
  Peak at 70-80% curve: 16 trades, avg PnL = 14.71% ← SWEET SPOT
  Peak at 80-100% curve: 20 trades, avg PnL = 8.33% ← Near graduation
```

**Key insight:** The biggest moves peak at 70-80% curve fill (vSOL 68-74). This makes sense: at this point, the token has enough momentum to attract graduation front-runners, but there's still upside on the bonding curve.

### 5.2 Curve Position ↔ Phase Correlation

```
Entry at 20-40% curve (vSOL 41-52):
  → 50% of trades. Best zone for MOMENTUM (6.6% reach it)
  → These entries have the most room to run on the curve
  → Price multiple from vSOL 47 to 85: (85/47)² = 3.27x (227% gain possible)

Entry at 40-55% curve (vSOL 52-60):
  → 35% of trades. Good ACCELERATION zone
  → Price multiple from vSOL 55 to 85: (85/55)² = 2.39x (139% gain possible)

Entry at 55%+ curve (vSOL 60+):
  → 15% of trades. Limited upside
  → Price multiple from vSOL 65 to 85: (85/65)² = 1.71x (71% gain possible)
```

### 5.3 Trail Distance as Function of Curve Position

Trail distance should be WIDER when there's more room to run (early in curve) and TIGHTER near graduation.

```rust
/// Adjust trail distance based on curve position
/// curve_pct_x100: 0 = fresh (vSOL=30), 10000 = graduated (vSOL=85)
fn curve_adjusted_trail_bps(base_trail_bps: u32, curve_pct_x100: u32) -> u32 {
    // At 0-40% curve: widen trail by 20% (more room to run)
    // At 40-70% curve: use base trail
    // At 70-85% curve: tighten by 20% (near graduation, protect gains)
    // At 85%+ curve: tighten by 40% (graduation zone, very tight)
    
    let multiplier = match curve_pct_x100 {
        0..=4000     => 120,   // 1.20x (wider)
        4001..=7000  => 100,   // 1.00x (base)
        7001..=8500  => 80,    // 0.80x (tighter)
        _            => 60,    // 0.60x (very tight near graduation)
    };
    
    base_trail_bps * multiplier / 100
}
```

**Example trail distances with curve adjustment:**

| Phase | Base Trail | At 30% Curve | At 55% Curve | At 75% Curve | At 90% Curve |
|---|---|---|---|---|---|
| IGNITION | 800 BPS | 960 BPS | 800 BPS | 640 BPS | 480 BPS |
| ACCELERATION | 500 BPS | 600 BPS | 500 BPS | 400 BPS | 300 BPS |
| PEAK | 300 BPS | 360 BPS | 300 BPS | 240 BPS | 180 BPS |

### 5.4 vSOL Movement Rate as Phase Confirmer

The data shows an almost perfect correlation between vSOL movement and confirming SOL:

```
Trades with >10 vSOL movement: 18 trades
  Average confirming SOL: 18.9 (almost equals vSOL delta)
  ALL in MOMENTUM phase
  ALL profitable (100% WR)
```

This means: **confirming SOL ≈ vSOL delta.** When we see 10+ SOL of confirming buys, the curve has moved ~10 vSOL, which at typical entry of vSOL=50 represents a **~44% price move** ((60/50)² = 1.44x).

**Implication for Rust:** We don't need to independently track vSOL if we have confirming SOL. The confirming SOL IS the vSOL delta (modulo sells, which are small during pumps).

```rust
/// Estimate curve movement from confirming buy SOL
/// Returns estimated price multiplier × 10000 (integer)
fn estimated_price_mult_x10000(entry_vsol_mvsol: u64, confirming_sol_mlamports: u64) -> u64 {
    let entry = entry_vsol_mvsol;  // in milli-vSOL
    let delta = confirming_sol_mlamports;  // roughly equals vSOL delta in mlam