# QUANT_UNIFIED_RIDE: Unified Dynamic Ride Engine — Architecture Specification

**Date:** 2026-03-29
**Author:** Systems Architect (Unified Integration)
**Status:** ARCHITECTURE SPEC — Integration design for 4 parallel quant analyses
**Scope:** Replace time-based RideState v1 (64B, 3 static phases) with signal-driven RideState v2 (≤128B, 4 dynamic states)

**Depends on:**
- `QUANT_RIDE_A.md` — Dual-mode exit architecture, RIDE mode trailing stop mechanics
- `QUANT_RIDE_B.md` — Pump magnitude predictor, Kelly criterion with variable payoff
- `QUANT_RIDE_C.md` — Optimal trail width math, vSOL-space integer computation, anti-rug detection

---

## 0. Executive Summary

### The Problem

The current RideState v1 uses **time-based phase progression**:
- EARLY (0–15s): 8% trail (408 vSOL bp)
- MOMENTUM (15–60s): 6% trail (305 vSOL bp)
- TIGHTEN (60s+): 4% trail (202 vSOL bp)

This is structurally wrong. Phases should track **signal state**, not wall clock time. A pump that receives 30 buys in 3 seconds is in MOMENTUM by any sane measure, but the system still runs an 8% trail because "only 3 seconds have elapsed." Conversely, a pump that goes silent after 20 seconds is in WEAKENING, but the system just tightened to 6% and will hold for another 40 seconds before tightening again.

### Evidence From 207 Trades

| Finding | Implication |
|---|---|
| **29/35 max_hold exits still pumping** (MFE≈PnL, avg +9.4%) | Time cap kills winners. Signal-based exit holds longer. |
| **38/38 whale exits had >2% more MFE** (avg 7.78% left on table) | Whale exit override is too aggressive. Signal context matters. |
| **28/30 hard-floor trades had MFE >1%** | Entered valid pumps but got shaken out. Wider early trail or better signal detection saves these. |
| **r(buysAfterEntry, PnL%) = 0.63** | Buy flow is the dominant continuation signal. |
| **r(confirmingBuySol, PnL%) = 0.93** | Flow magnitude even more predictive than count. |
| **Buy/sell ratio 5-10: 96% WR, 8.26% avg PnL** | Net flow ratio is the cleanest composite signal. |
| **Net flow rate ≥10/sec: 96% WR, 10.67% avg PnL** | High flow rate = strong pump. Trail should widen. |
| **67.6% WR current → target >90% WR** | Achievable by making whale exits & hard floors smarter. |

### The Solution

Replace time/gain-based phases with a **4-state signal-driven state machine**:

```
STRONG_PUMP → SUSTAINED → WEAKENING → EXIT
     ↑            ↕            ↓
     └── (bidirectional recovery) ──┘
```

Trail width is computed dynamically every event from three multiplicative factors:
```
trail_bp = (base_trail × kelly_mult × phase_mult) >> 8

where:
  base_trail:  from composite signal score (200–600 vSOL bp)
  kelly_mult:  from optimal f edge estimate (192–320, fixed-point 8.8)
  phase_mult:  from pump lifecycle detector (128–256, fixed-point 8.8)
```

All integer. Zero heap. ≤128 bytes. <100ns per event.

---

## 1. Unified RideState v2 — Memory Layout

### 1.1 Design Constraints

| Constraint | Value | Rationale |
|---|---|---|
| Max size | 128 bytes (2 cache lines) | L1 cache residency for hot-path processing |
| Alignment | 8 bytes (natural u64 align) | Single load instruction for any field |
| Heap allocations | Zero | No mallocs on the hot path, ever |
| Floating point | Zero | Integer-only; f64 division is 20ns+, u64 multiply+shift is 2ns |
| Copy trait | Required | State is value-type, passed/stored by copy |

### 1.2 Struct Layout (128 bytes, 2 cache lines)

**Cache Line 0 (bytes 0–63): HOT — accessed every event**

```
Offset  Size  Type    Field                   Description
──────  ────  ──────  ──────────────────────  ─────────────────────────────────
0       1     u8      signal_state            0=STRONG_PUMP, 1=SUSTAINED, 2=WEAKENING, 3=EXIT
1       1     u8      pump_phase              0=IGNITION, 1=ACCEL, 2=PEAK, 3=DECAY (lifecycle detector)
2       2     u16     trail_distance_bp       Current effective trail (vSOL basis points)
4       4     u32     entry_mvsol             Entry vSOL in milli-SOL
8       4     u32     peak_mvsol              High-water mark vSOL
12      4     u32     floor_mvsol             Hard floor: entry × 1.01
16      4     u32     trail_stop_mvsol        Current trail stop level (ratchet-up only)
20      4     u32     current_mvsol           Last observed vSOL (for delta computation)
24      8     u64     ride_start_ms           RIDE activation timestamp
32      8     u64     last_buy_ms             Last confirming buy timestamp
40      2     u16     composite_score         Composite hold/exit signal (0–1000, ×1000 FP)
42      2     u16     kelly_mult_fp8          Kelly multiplier, 8.8 fixed-point (256 = 1.0×)
44      2     u16     phase_mult_fp8          Phase multiplier, 8.8 fixed-point (256 = 1.0×)
46      2     u16     flags                   Bitflags (emergency signals)
48      4     u32     total_buy_msol          Cumulative buy volume during ride
52      4     u32     total_sell_msol         Cumulative sell volume during ride
56      2     u16     buy_count               Buy events during ride
58      2     u16     sell_count              Sell events during ride
60      2     u16     buy_rate_at_start       Buy rate (count/5s) at RIDE activation
62      2     u16     entry_gain_bp           Gain at RIDE activation (diagnostics)
```

**Cache Line 1 (bytes 64–127): WARM — accessed for signal computation + diagnostics**

```
Offset  Size  Type    Field                   Description
──────  ────  ──────  ──────────────────────  ─────────────────────────────────
64      4     u32     rolling_buy_msol_1s     Buy volume in last 1s window (mvsol)
68      4     u32     rolling_sell_msol_1s    Sell volume in last 1s window (mvsol)
72      4     u32     rolling_buy_msol_3s     Buy volume in last 3s window (mvsol)
76      4     u32     rolling_sell_msol_3s    Sell volume in last 3s window (mvsol)
80      2     u16     rolling_buy_count_1s    Buy count in last 1s
82      2     u16     rolling_sell_count_1s   Sell count in last 1s
84      2     u16     rolling_buy_count_3s    Buy count in last 3s
86      2     u16     rolling_sell_count_3s   Sell count in last 3s
88      8     u64     window_anchor_ms        Timestamp anchor for rolling windows
96      4     u32     prev_peak_mvsol         Previous peak (for peak rate detection)
100     2     u16     peak_rate_bp_per_s      Rate of new highs (vSOL bp/sec, for PEAK detection)
102     1     u8      cascade_count_2s        Recent sells in 2s window (for cascade detection)
103     1     u8      unique_wallets          Unique buying wallets observed
104     4     u32     last_sell_msol          Size of most recent sell (for whale detection)
108     4     u32     max_single_sell_msol    Largest single sell observed during ride
112     2     u16     signal_vector_raw       Raw composite signal before smoothing (diagnostics)
114     1     u8      state_transitions       Count of state transitions (diagnostics)
115     1     u8      _reserved0              Padding
116     4     u32     _reserved1              Reserved for future use
120     8     u64     cascade_window_start_ms Window start for cascade detection
```

**Total: 128 bytes exactly. 2 cache lines. Zero heap.**

### 1.3 Cache Locality Analysis

The hot path (`on_event` → `check_exit`) touches bytes 0–62 (cache line 0) for:
- State check (byte 0)
- Trail stop comparison (bytes 16, 20)
- Peak update (byte 8)
- Flag check (byte 46)
- Score read (byte 40)

Signal computation touches bytes 64–107 (cache line 1) for:
- Rolling window updates
- Composite score calculation

**Result:** Hot path = 1 cache miss (line 0 always resident). Signal compute = 1 additional cache miss (line 1). Both lines will be adjacent in memory → hardware prefetcher keeps line 1 warm.

### 1.4 Rolling Window Design: Exponential Decay Approximation

True rolling windows require ring buffers (heap) or sorted timestamp arrays. We can't afford that. Instead, use **timestamp-anchored exponential decay approximation**:

**Mechanism:**
- Maintain `window_anchor_ms` (last reset point)
- On each event, if `(now_ms - window_anchor_ms) > window_size`, decay existing values by 50% and advance anchor by half-window
- This gives O(1) updates with ~90% accuracy vs true rolling windows

**For 1s window (1000ms):**
```
if now_ms - window_anchor_ms > 500:
    rolling_buy_msol_1s >>= 1       // decay by 50%
    rolling_sell_msol_1s >>= 1
    rolling_buy_count_1s >>= 1
    rolling_sell_count_1s >>= 1
    window_anchor_ms += 500
    // Repeat if needed (handles gaps > 1s)
```

**For 3s window (3000ms):** Same mechanism on the 3s fields with 1500ms half-life.

**Why this works:** We don't need exact rolling sums. We need *relative magnitude and trend direction*. The exponential decay preserves these properties while fitting in fixed-size integers. The alternative (circular buffer of per-event records) would require ~200 bytes minimum for 10-20 events.

### 1.5 Flags Bitfield

```
Bit   Name                   Meaning
───   ────────────────────   ──────────────────────────────────────
0     SELL_PRESSURE_HIGH     sell_vol_1s > buy_vol_1s / 2
1     BUY_DECELERATION       buy_rate_1s < buy_rate_at_start / 3
2     WHALE_SELL_SEEN        Single sell > 1 SOL observed
3     BUY_GAP_5S             No buy for >5000ms
4     CREATOR_SELL           Creator wallet sold (INSTANT EXIT)
5     EMERGENCY_EXIT         Catastrophic signal (INSTANT EXIT)
6     PEAK_REVERSAL          Peak rate went negative (lifecycle: PEAK→DECAY)
7     SIGNAL_RECOVERY        Recovered from WEAKENING→SUSTAINED
8-15  (reserved)
```

---

## 2. Signal-Driven State Machine

### 2.1 States (Replace EARLY/MOMENTUM/TIGHTEN)

| State | Meaning | Trail Range (vSOL bp) | Entry Condition | Exit Condition |
|---|---|---|---|---|
| **STRONG_PUMP** | Active pump, heavy buy flow, price rising | 400–600 | Initial state on RIDE activation | composite_score < 600 for 2+ events |
| **SUSTAINED** | Healthy pump, moderate buy flow, price stable/rising | 250–400 | Score in [400, 700] sustained | Score < 400 for 3+ events OR > 700 |
| **WEAKENING** | Pump losing steam, sell pressure rising, buys slowing | 150–250 | Score < 400 for 3+ events | Score > 500 (→ recovery) OR score < 200 (→ EXIT) |
| **EXIT** | Terminal. Trail at minimum, actively seeking exit. | 101 (emergency) | Score < 200 OR emergency flag | Trail stop triggers sell |

### 2.2 State Transitions — Bidirectional

```
                  score ≥ 700
              ┌───────────────┐
              │               ▼
         STRONG_PUMP ──→ SUSTAINED ──→ WEAKENING ──→ EXIT
              ▲        score<600    score<400     score<200
              │          (×2)        (×3)
              │               ▲           │
              │               └───────────┘
              │              score > 500
              │              (recovery)
              │
              └── WEAKENING recovery if score > 700
                  (rare: requires massive buy surge)
```

**Key difference from v1:** Transitions are **bidirectional**. If a pump goes quiet for 2 seconds (WEAKENING) but then gets hit with a wave of buys, it can recover to SUSTAINED or even STRONG_PUMP. The current system's one-way EARLY→MOMENTUM→TIGHTEN can never widen the trail back, even when the pump clearly recovers.

### 2.3 Transition Hysteresis

To prevent oscillation, transitions require **N consecutive events** in the target score range:

| Transition | Events Required | Rationale |
|---|---|---|
| STRONG_PUMP → SUSTAINED | 2 events below 600 | Quick tightening on first weakness signal |
| SUSTAINED → STRONG_PUMP | 3 events above 700 | Must prove strength before widening |
| SUSTAINED → WEAKENING | 3 events below 400 | Give pump time to recover from dips |
| WEAKENING → SUSTAINED | 2 events above 500 | Quick recovery on buy resumption |
| WEAKENING → EXIT | 2 events below 200 | Don't linger in death spiral |
| Any → EXIT (emergency) | 1 event | Emergency overrides skip hysteresis |

**Implementation:** Store a 2-bit transition counter in the lower bits of `signal_state`:
```
signal_state layout (u8):
  bits [7:4] = current state (0–3)
  bits [3:2] = pending direction (0=none, 1=tighten, 2=widen)
  bits [1:0] = consecutive count toward transition (0–3)
```

This packs the entire state machine into a single byte.

### 2.4 Eliminating max_hold_ms

The current system has a 300s (5 minute) max hold as a safety backstop. **RideState v2 eliminates this entirely.** Exit is purely signal-driven:

- If the pump is still STRONG_PUMP after 300s, *there is no reason to exit*. The signal says hold.
- The trailing stop itself is the exit mechanism. If price reverses, the trail catches it.
- Emergency overrides (creator sell, whale dump, cascade) handle pathological cases.

**From the data:** 29/35 max_hold trades were still pumping at exit. Removing max_hold and letting them ride would have captured significantly more profit. The signal-based EXIT state replaces the time backstop with a functionally superior mechanism.

---

## 3. Composite Signal Score

### 3.1 Signal Vector (6 features → 1 composite score)

The composite score is a weighted sum of 6 real-time features, each normalized to [0, 1000]:

| Feature | Weight | Range | Computation | Budget |
|---|---|---|---|---|
| **F1: Net Flow Rate** | 0.30 | [0, 1000] | `(buy_count_1s - sell_count_1s) × 100`, clamp | ~3ns |
| **F2: Volume Ratio** | 0.25 | [0, 1000] | `buy_msol_3s × 1000 / (buy_msol_3s + sell_msol_3s)` | ~5ns |
| **F3: Buy Acceleration** | 0.15 | [0, 1000] | `buy_count_1s × 1000 / max(buy_rate_at_start, 1)`, clamp | ~3ns |
| **F4: Price Velocity** | 0.15 | [0, 1000] | `(current_mvsol - prev_peak_mvsol + delta) × factor`, clamp | ~5ns |
| **F5: Buy Gap Penalty** | 0.10 | [0, 1000] | `max(0, 1000 - (now_ms - last_buy_ms) × 2)` | ~3ns |
| **F6: Sell Size Penalty** | 0.05 | [0, 1000] | `max(0, 1000 - last_sell_msol × 2)`, only if sell in window | ~2ns |

**Composite score = Σ(weight × feature) = range [0, 1000]**

### 3.2 Feature Detail: F1 — Net Flow Rate (weight 0.30)

The strongest predictor from the data (r=0.63 for buysAfterEntry, r=0.93 for confirmingBuySol). Net flow rate captures both count and direction.

```
raw = (rolling_buy_count_1s as i16 - rolling_sell_count_1s as i16)
scaled = (raw × 100).clamp(0, 1000) as u16
```

Interpretation:
- 0 buys, 2 sells in 1s → raw = -2 → scaled = 0 (EXIT signal)
- 3 buys, 0 sells in 1s → raw = 3 → scaled = 300 (moderate)
- 8 buys, 1 sell in 1s → raw = 7 → scaled = 700 (strong)
- 10+ buys, 0 sells in 1s → raw = 10 → scaled = 1000 (maximum)

### 3.3 Feature Detail: F2 — Volume Ratio (weight 0.25)

Not just count but SOL magnitude. A single 1 SOL buy overwhelms five 0.02 SOL buys.

```
total = rolling_buy_msol_3s + rolling_sell_msol_3s
if total == 0: F2 = 500  // neutral if no activity (shouldn't happen)
else: F2 = (rolling_buy_msol_3s as u32 × 1000 / total) as u16
```

Interpretation:
- 100% buy volume → 1000 (max bullish)
- 80% buy, 20% sell → 800 (healthy pump)
- 50/50 → 500 (neutral — pump exhausting)
- 20% buy, 80% sell → 200 (dump)

### 3.4 Feature Detail: F3 — Buy Acceleration (weight 0.15)

Is the pump accelerating or decelerating relative to its start?

```
if buy_rate_at_start == 0: F3 = rolling_buy_count_1s > 0 ? 500 : 0
else: F3 = min(1000, rolling_buy_count_1s × 1000 / buy_rate_at_start)
```

- Current rate = start rate → 1000/start_rate×... actually let me normalize properly.

The buy_rate_at_start is a count per 5s. To compare with the 1s window:
```
start_rate_1s = buy_rate_at_start / 5  // normalize to per-second
current_rate_1s = rolling_buy_count_1s
F3 = min(1000, current_rate_1s × 1000 / max(start_rate_1s, 1))
```

- Maintaining start pace → F3 = 1000 (capped)
- Half the start pace → F3 = 500
- Zero buys → F3 = 0

### 3.5 Feature Detail: F4 — Price Velocity (weight 0.15)

Is price making new highs, stalling, or reversing?

```
if current_mvsol >= peak_mvsol:
    F4 = 800 + min(200, (current_mvsol - prev_peak_mvsol) × X)  // new high = strong
else:
    drawdown_bp = (peak_mvsol - current_mvsol) × 10000 / peak_mvsol
    F4 = max(0, 800 - drawdown_bp × 2)  // penalize drawdown from peak
```

- Making new highs → 800–1000
- At peak, no movement → 800
- 1% below peak → ~600
- 3% below peak → ~200
- 4%+ below peak → 0

### 3.6 Feature Detail: F5 — Buy Gap Penalty (weight 0.10)

Time since last buy. Pumps have continuous buy flow; gaps are danger signals.

```
gap_ms = now_ms - last_buy_ms
F5 = max(0, 1000 - gap_ms × 2)  // 500ms gap → 0 penalty; 500ms+ → starts decaying
```

Wait, this needs calibration:
```
F5 = max(0, 1000 - (gap_ms / 5) as u16)  // at 5s gap, F5 = 0
```

- 0ms gap (just got a buy) → 1000
- 1s gap → 800
- 3s gap → 400
- 5s gap → 0

### 3.7 Feature Detail: F6 — Sell Size Penalty (weight 0.05)

Large single sells are more dangerous than multiple small ones.

```
if last_sell_msol == 0 || no_sell_in_window:
    F6 = 1000  // no sell activity = good
else:
    F6 = max(0, 1000 - last_sell_msol as u16)  // 1 SOL sell → F6 = 0
```

- No sells → 1000
- 0.2 SOL sell → ~800
- 0.5 SOL sell → ~500
- 1.0 SOL sell → 0

### 3.8 Integer-Only Weighted Sum

Weights as integers summing to 256 (for >> 8 division):

```
W1 = 77   (0.30 × 256 ≈ 77)
W2 = 64   (0.25 × 256 ≈ 64)
W3 = 38   (0.15 × 256 ≈ 38)
W4 = 38   (0.15 × 256 ≈ 38)
W5 = 26   (0.10 × 256 ≈ 26)
W6 = 13   (0.05 × 256 ≈ 13)
Sum = 256
```

```
composite_score = (F1×77 + F2×64 + F3×38 + F4×38 + F5×26 + F6×13) >> 8
```

This is 6 multiplies + 5 adds + 1 shift. All u32 intermediate. ~15ns.

**Score interpretation:**
- 700–1000: STRONG_PUMP territory
- 400–700:  SUSTAINED territory
- 200–400:  WEAKENING territory
- 0–200:    EXIT territory

---

## 4. Pump Lifecycle Phase Detection

### 4.1 Four Phases (Independent of Signal State)

The pump lifecycle detector tracks *where we are in the pump's natural arc*, distinct from *how strong the current signal is*. A pump in PEAK phase with a strong signal score is different from one in IGNITION with the same score — the PEAK phase pump has less upside remaining.

| Phase | Characteristic | Detection | phase_mult (FP 8.8) |
|---|---|---|---|
| **IGNITION** | First 0–2s, initial buy cascade | `ride_elapsed < 2000ms AND buy_count < 5` | 256 (1.0×) — neutral |
| **ACCELERATION** | Sustained buying, price making new highs every ~200ms | `peak_rate_bp_per_s > 50 AND buy_count_1s > 3` | 288 (1.125×) — widen trail |
| **PEAK** | Highest activity, peak rate decelerating | `peak_rate declining AND sell_count_1s > 0` | 192 (0.75×) — tighten trail |
| **DECAY** | Buy rate falling, sells increasing, price stalling | `peak_rate ≤ 0 AND volume_ratio < 600` | 128 (0.5×) — aggressive tighten |

### 4.2 Peak Rate Computation

The peak rate measures how fast the high-water mark is advancing:

```
On each event where current_mvsol > peak_mvsol:
    delta_bp = (current_mvsol - prev_peak_mvsol) × 10000 / prev_peak_mvsol
    delta_ms = now_ms - last_peak_update_ms
    peak_rate_bp_per_s = delta_bp × 1000 / max(delta_ms, 1)
    prev_peak_mvsol = peak_mvsol  // (after peak update in hot path)

On each event where current_mvsol <= peak_mvsol:
    // Peak rate decays toward zero
    // Use the same exponential decay as rolling windows
    if enough time since last peak update:
        peak_rate_bp_per_s >>= 1  // halve every 500ms without new peak
```

### 4.3 Lifecycle Phase Transitions

```
IGNITION:
  → ACCELERATION when: peak_rate_bp_per_s > 50 AND buy_count > 3
  → DECAY when: buy_gap > 2000ms (never really ignited)

ACCELERATION:
  → PEAK when: peak_rate_bp_per_s declining for 3+ events AND sell_count_1s >= 1
  → IGNITION when: price drops below entry + 2% (false start)

PEAK:
  → DECAY when: peak_rate_bp_per_s == 0 for 500ms+ OR volume_ratio < 500
  → ACCELERATION when: peak_rate_bp_per_s resumes > 50 (second wave)

DECAY:
  → ACCELERATION when: peak_rate_bp_per_s > 80 (strong recovery)
  → (terminal state if sustained — trail tightens to exit)
```

**Key insight:** The lifecycle detector allows ACCELERATION→PEAK→ACCELERATION (second wave). This captures the common pump pattern where there's a pause/dip, then a second wave of buyers discovers the token. The v1 system would have already tightened to TIGHTEN phase by time, missing the second wave.

### 4.4 Phase Multiplier Effect

The phase multiplier scales the trail width:
- During ACCELERATION (1.125×): trail is ~12% wider, giving the pump room to breathe
- During PEAK (0.75×): trail is 25% tighter, locking in gains near the top
- During DECAY (0.5×): trail is 50% tighter, aggressively protecting remaining profit

This is **multiplicative with the signal score**, not additive. A STRONG_PUMP signal in DECAY phase still gets a tight trail (the signal might be a dead-cat bounce). A WEAKENING signal in ACCELERATION phase gets moderate tightening (the weakness might be temporary in a multi-wave pump).

---

## 5. Kelly-Derived Dynamic Trail

### 5.1 Optimal f in the Ride Context

The Kelly criterion gives optimal bet sizing: `f* = p - (1-p)/R` where p = win probability, R = win/loss ratio. In the RIDE context, we reinterpret Kelly not for position sizing (already determined at entry) but for **trail width optimization**.

The insight: trail width is a risk/reward tradeoff.
- Wider trail → higher probability of capturing the full move, but gives back more if reversal
- Tighter trail → captures less of the move, but keeps more on reversal

The "Kelly-optimal trail" is the width that maximizes expected log-profit.

### 5.2 Real-Time Edge Estimation

We estimate the current edge from the composite signal score and realized PnL:

```
// Estimate current win probability from signal score
// Calibrated from backtest data:
//   score 700+ → ~95% WR (from net_flow_rate ≥10: 96% WR)
//   score 400-700 → ~85% WR (from buy/sell ratio 5-10: 96% WR)
//   score 200-400 → ~70% WR (from ratio 2-5: 77% WR)

p_win_fp8 = lookup_table[composite_score >> 6]  // 16-entry LUT, FP 8.8

// Estimate payoff ratio from current unrealized gain and trend
// If currently at +10% with strong signal, expected additional gain is high
// If currently at +2% with weak signal, expected additional gain is low

current_gain_bp = (current_mvsol - entry_mvsol) × 10000 / entry_mvsol
expected_additional_bp = current_gain_bp × signal_strength_factor  // rough estimate

R_fp8 = expected_additional_bp / trail_cost_bp  // win/loss ratio

// Kelly optimal f: how much risk to take
kelly_f_fp8 = p_win_fp8 - ((256 - p_win_fp8) × 256 / max(R_fp8, 1))
kelly_mult_fp8 = 256 + kelly_f_fp8.clamp(-64, 64)  // range: 192–320 (0.75×–1.25×)
```

### 5.3 Simplified Kelly Multiplier (Practical Implementation)

The full Kelly computation above requires division. For the hot path, use a **precomputed lookup table** indexed by (signal_state, current_gain_bucket):

```
kelly_lut[4][4] (4 signal states × 4 gain buckets):

                  gain<2%   gain 2-5%  gain 5-15%  gain>15%
STRONG_PUMP:       256       288        320         320      // 1.0× to 1.25×
SUSTAINED:         224       256        288         288      // 0.875× to 1.125×
WEAKENING:         192       192        224         224      // 0.75× to 0.875×
EXIT:              128       128        128         128      // 0.5× (minimum)
```

**Why gain bucket matters:** With more unrealized profit, we can afford wider trails (Kelly says bet more when ahead). With less profit, tighten to preserve capital.

**Lookup is a single table index:** `kelly_mult_fp8 = KELLY_LUT[signal_state][gain_bucket]` — ~2ns.

---

## 6. Trail Computation Pipeline

### 6.1 The Complete Trail Formula

```
trail_bp = (base_trail × kelly_mult × phase_mult) >> 16

where:
  base_trail:   u16, range [200, 600] vSOL