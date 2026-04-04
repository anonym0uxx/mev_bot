# UNIFIED BUILD SPECIFICATION — Pump.fun Principal Quant Bot

**Status:** Implementation-ready  
**Date:** 2026-03-29  
**Source Documents:**  
- `QUANT_RIDE_C.md` — Trailing stop math (complete, 1019 lines)  
- `QUANT_RIDE_A.md` — RIDE exit strategy (598 lines, Kelly section completed below)  
- `QUANT_RIDE_B.md` — Entry + magnitude prediction (527 lines, features 7+ and sections 5-10 completed below)  
- `ARCH_ENTRY.md` — Entry engine architecture (1182 lines, test section completed below)  
- `ARCH_RIDE.md` — RIDE mode architecture (1222 lines, config parsing completed below)  
- `ARCH_HOTPATH.md` — Hot path integration (627 lines, enum + event loop completed below)

**Convention:** All monetary values in lamports (u64) unless noted. 1 SOL = 1,000,000,000 lamports. All basis-point fields (bp) are ÷10,000 (e.g., 800 bp = 8.00%). All milli-SOL vSOL (mvsol) fields are ÷1,000,000 from lamports.

---

## Part 1: System Overview

### 1.1 Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         HOT PATH THREAD                             │
│                                                                     │
│  TradeEvent (from Helius/PumpPortal websocket)                      │
│       │                                                             │
│       ▼                                                             │
│  ┌──────────────────────┐                                           │
│  │   Pre-Filters (5ns)  │ ← excluded_mints, is_buy, graduation     │
│  │   + RiskManager      │   boundary, health monitor, daily cap     │
│  └──────────┬───────────┘                                           │
│             │ pass                                                   │
│             ▼                                                       │
│  ┌──────────────────────────────────────┐                           │
│  │  Has existing position for mint?     │                           │
│  └──────┬───────────────┬───────────────┘                           │
│    NO   │               │ YES                                       │
│         ▼               ▼                                           │
│  ┌──────────────┐  ┌────────────────────────────┐                   │
│  │ EntryEngine  │  │ PositionManager             │                   │
│  │ (3-stage)    │  │ .on_subsequent_trade()      │                   │
│  │              │  │                              │                   │
│  │ S1: hard_gate│  │ match exit_mode {            │                   │
│  │   (<50ns)    │  │   Scalp(sm) =>              │                   │
│  │   65% reject │  │     track confirming buys    │                   │
│  │              │  │     if ride_qualified()      │                   │
│  │ S2: score()  │  │       → TRANSITION to RIDE  │                   │
│  │   (~150ns)   │  │     sm.on_event()           │                   │
│  │   entry +    │  │   Ride(rs) =>               │                   │
│  │   magnitude  │  │     rs.on_buy_event() or    │                   │
│  │              │  │     rs.on_sell_event()       │                   │
│  │ S3: size()   │  │     rs.on_tick()            │                   │
│  │   (~10ns)    │  │ }                            │                   │
│  └──────┬───────┘  └────────────┬───────────────┘                   │
│         │                       │                                    │
│         ▼                       ▼                                    │
│  ┌──────────────┐  ┌────────────────────────────┐                   │
│  │ Open Position│  │ Exit Decision               │                   │
│  │ (always      │  │ Hold / Exit(reason)         │                   │
│  │  SCALP mode) │  │                              │                   │
│  └──────────────┘  └────────────┬───────────────┘                   │
│                                  │ Exit                              │
│                                  ▼                                    │
│                    ┌──────────────────────────────┐                  │
│                    │ close_position()              │                  │
│                    │ → submit sell tx (Jito)       │                  │
│                    │ → RiskManager.on_trade_result │                  │
│                    │ → log ClosedPosition          │                  │
│                    └──────────────────────────────┘                  │
└─────────────────────────────────────────────────────────────────────┘

Timer Thread (50ms ticks):
  on_tick() → iterate all OpenPositions → check max_hold, trail, buy_gap
```

### 1.2 Module Map

| File | Action | Contents |
|------|--------|----------|
| `engine/entry_engine.rs` | **CREATE** | `EntryEngine`, `HardGateThresholds`, LUTs, `EntryInput`, `EntryDecision`, `EntryAction` |
| `engine/scoring.rs` | **CREATE** | `ScoringWeights`, `Reciprocals`, `DecisionThresholds`, LUT builders, `clamp01()` |
| `engine/ride_state.rs` | **CREATE** | `RideState` (64B), `RidePhase`, `RideExitReason`, `RideConfig`, `ride_flags`, `CascadeDetector`, qualification logic |
| `engine/risk_manager.rs` | **CREATE** | `RiskManager`, daily loss cap, circuit breaker, concurrent position limits |
| `engine/positions.rs` | **MODIFY** | Add `ExitMode` enum, `OpenPosition.ride_state`, confirming-buy tracking, `on_subsequent_trade()` with RIDE routing |
| `engine/hot_path.rs` | **MODIFY** | Replace `GateStack`+`Scorer` with `EntryEngine`, add `RiskManager` gate, add RIDE safety timer |
| `engine/config.rs` | **MODIFY** | Add `RideConfig`, `RideJsonConfig`, `EntryEngineConfig`, JSON deserialization |
| `config/canary.json` | **MODIFY** | Add `entry_engine` and `ride` config sections |

### 1.3 Data Flow

```
TradeEvent
  → MintHistoryMap.push(record)          // update cached aggregates
  → if has_position → on_subsequent_trade()
      → if is_buy:
          accumulate confirming_buy_sol, unique_wallets, buys_since_entry
          if ride_state.is_some() → RideState::on_buy_event()
          else → ExitStateMachine::on_buy_event()
              if conviction ≥ 2 && ride_qualified() → RideState::activate()
      → if is_sell && ride_state.is_some():
          RideState::on_sell_event() → may return Exit(CreatorSell|WhaleDump|SellCascade)
      → ride_state.on_tick() OR exit_sm.on_price_tick()
  → else (new entry):
      RiskManager::allows_entry() → EntryEngine::evaluate()
          Stage 1: hard_gate() → reject 65%
          Stage 2: score() → entry_score + magnitude_score
          Stage 3: size() → (action: Scalp|Ride|Reject, size_lamports)
      → PositionManager::open_position_v2(SCALP mode, Kelly-sized)
```

---

## Part 2: Entry Engine Spec

### 2.1 EntryEngine Struct Layout

Total size: **3,144 bytes** — fits comfortably in L1 data cache (~50 cache lines, 9.8% of typical 32KB L1D).

```rust
#[repr(C)]
pub struct EntryEngine {
    // Stage 1: Hard Gate (1 cache line, accessed on EVERY call)
    pub gate: HardGateThresholds,         //   56 bytes

    // LUTs (accessed only on gate pass ~35% of calls)
    pub buy_burst_lut: [f64; 64],         //  512 bytes  — sigmoid(buy_count, center=7, steepness=0.8)
    pub accel_lut: [f64; 128],            // 1024 bytes  — sigmoid(accel+64, center=10, steepness=0.15)
    pub curve_lut: [f64; 100],            //  800 bytes  — gaussian(curvePct, mean=43, sigma=12)
    pub fill_rate_lut: [f64; 64],         //  512 bytes  — sigmoid(fill_idx, center=15, steepness=0.25)

    // Stage 2: Scoring parameters
    pub weights: ScoringWeights,          //  120 bytes  (15 × f64)
    pub reciprocals: Reciprocals,         //   48 bytes  (6 × f64)

    // Stage 3: Decision thresholds
    pub decision: DecisionThresholds,     //   72 bytes
}
```

### 2.2 Hard Gate Function — All 8 Checks

All integer. Ordered by (rejection_rate × cost). Target: <50ns worst-case.

```rust
#[repr(C)]
pub struct HardGateThresholds {
    pub min_buy_count_1s: u16,             // default: 5
    pub max_unique_buyers_30s: u16,        // default: 30
    pub _pad0: u32,
    pub min_volume_sol_5s: u64,            // default: 5_000_000_000 (5 SOL)
    pub max_time_since_last_buy_ms: u64,   // default: 500
    pub min_vsol_reserves: u64,            // default: 47_000_000_000 (curve 20%)
    pub max_vsol_reserves: u64,            // default: 81_000_000_000 (curve 60%)
    pub min_history_age_ms: u64,           // default: 2000
    pub creator_sell_cooldown_ms: u64,     // default: 30_000
}
// 56 bytes = 1 cache line
```

**Check order and logic:**

| # | Check | Default | Rejection Rate | Operation |
|---|-------|---------|----------------|-----------|
| 1 | `buy_count_1s >= 5` | 5 | ~55% | 1 u16 cmp |
| 2 | `volume_sol_5s >= 5 SOL` | 5B lam | ~50% | 1 u64 cmp |
| 3 | `time_since_last_buy_ms <= 500` | 500 | ~40% | 1 u64 cmp |
| 4 | `vsol_reserves ∈ [47B..81B]` | 20–60% curve | ~35% | 2 u64 cmp |
| 5 | `unique_buyers_30s <= 30` | 30 | ~15% | 1 u16 cmp |
| 6 | `2 × sell_count_5s < buy_count_5s` | — | ~10% | 1 u32 mul + 1 cmp |
| 7 | `history_age_ms >= 2000` | 2000 | ~5% | 1 u64 cmp |
| 8 | Creator sell cooldown (if creator_sell_at_ms > 0 AND now - creator_sell_at_ms ≤ cooldown → reject) | 30s | ~2% | 2 cmp |

**Implementation:** See `ARCH_ENTRY.md` §3 for the complete `hard_gate()` function. Each check returns `false` immediately on failure (early-exit).

### 2.3 Composite Scoring — 8 Entry + 7 Magnitude Features

All features produce values in [0, 1]. Weights sum to 1.0 per model. Final scores scaled to 0-100.

#### Entry Features (predict IF token pumps)

| # | Feature | Weight | Source | LUT/Method |
|---|---------|--------|--------|------------|
| 1 | Buy Burst Intensity | 0.30 | `buy_count_1s` | `buy_burst_lut[min(count,63)]` — sigmoid(center=7, steep=0.8) |
| 2 | Volume Intensity | 0.20 | `volume_sol_5s` | `clamp01(vol × inv_crowd_norm)` — linear normalize to 10 SOL |
| 3 | Curve Position | 0.15 | `vsol_reserves` | `curve_lut[curvePct]` — Gaussian(mean=43%, σ=12) |
| 4 | Buyer Concentration | 0.10 | `unique_buyers_30s` | Piecewise linear: peak at 10, ramp 5→10→15, decay >15 |
| 5 | Buy Acceleration | 0.10 | `buy_count_1s*5 - buy_count_5s` | `accel_lut[accel+64]` — signed sigmoid(center=10, steep=0.15) |
| 6 | Avg Buy Size | 0.05 | `volume_sol_5s / buy_count_5s` | `clamp01(avg / 2 SOL)` — linear ramp |
| 7 | Sell Absence | 0.05 | `sell_count_5s / buy_count_5s` | `max(0, 1 - ratio × 2.5)` |
| 8 | Recency | 0.05 | `time_since_last_buy_ms` | `max(0, 1 - ms × inv_max_recency)` — linear decay |

#### Magnitude Features (predict HOW FAR it pumps)

| # | Feature | Weight | Source | Method |
|---|---------|--------|--------|--------|
| 1 | Fill Rate | 0.20 | `vsol_delta_3s` | `fill_rate_lut[delta/SCALE]` — sigmoid(center=15, steep=0.25) |
| 2 | Buy Velocity Accel | 0.20 | (reuse entry F5) | Same accel LUT, different weight |
| 3 | Wallet Quality | 0.15 | `max_wallet_vol_30s / total_buy_vol_30s` | `max(0, 1 - concentration)` |
| 4 | Curve Remaining | 0.15 | `curvePct` | `1.0 - curvePct/100` |
| 5 | Volume Intensity | 0.15 | `volume_sol_5s` | `clamp01(vol × inv_volume_intensity_norm)` — norm to 10 SOL |
| 6 | Sell Vacuum | 0.10 | `sell_count_5s` | If 0 → 1.0; else `0.6^sell_count` |
| 7 | Token Age | 0.05 | `history_age_ms` | Sweet spot 5-30s → 1.0; <5s → 0.3; 30-120s → linear decay to 0.3; >120s → 0.3 |

**(Gap fill: QUANT_RIDE_B Features 7-10):** Features 7 (Token Age) is the last feature needed. The original doc planned additional features (Time-of-day, Social signals, etc.) but these are external data sources not available on the hot path. Token Age at weight 0.05 covers the temporal dimension. Time-of-day is handled separately via `tod_multiplier` applied to position sizing (already in the existing codebase).

#### Composite Score Formulas

```
entry_score = (Σ entry_feature_i × weight_i) × 100    // 0-100
magnitude_score = (Σ mag_feature_i × weight_i) × 100  // 0-100
```

### 2.4 Position Sizing (Kelly-Derived Tiers)

**(Gap fill: QUANT_RIDE_A Kelly criterion, completed from QUANT_RIDE_C P&L model)**

#### Kelly Criterion for RIDE Mode

Using QUANT_RIDE_C §4 parameters:
```
Win rate (p) = 0.85
Average win (W) = 41.51% of position = 0.04151 SOL on 0.10 position
Average loss (L) = 60% of position = 0.06 SOL on 0.10 position

Kelly fraction f* = p/L - (1-p)/W
                  = 0.85/0.06 - 0.15/0.04151
                  = 14.167 - 3.614
                  = 10.55

Full Kelly says bet 10.55× bankroll — obviously over-leveraged.
Half-Kelly: f*/2 = 5.27 → still aggressive.
Quarter-Kelly: f*/4 = 2.64× bankroll per trade.

For a 5 SOL bankroll:
  Quarter-Kelly optimal: 5 × 2.64 = 13.2 SOL per trade (absurd — capped by strategy)
```

**Practical interpretation:** Kelly says RIDE has extreme edge. The constraint isn't Kelly (which says "bet everything") — it's:
1. **Slippage:** position > 1% of vSOL degrades execution
2. **Liquidity:** bonding curve has finite depth
3. **Correlation:** multiple RIDE positions can fail simultaneously

**Decision: Fixed tiers with Kelly-informed rationale:**

| Condition | Entry Score | Magnitude Score | Position Size | Rationale |
|-----------|-------------|-----------------|---------------|-----------|
| SCALP (low) | 50-60 | <40 | 0.10 SOL | Base size, acceptable Kelly fraction |
| SCALP (mid) | 60-70 | <40 | 0.12 SOL | Higher conviction, still SCALP |
| SCALP (high) | 70+ | <40 | 0.15 SOL | Strong entry signal |
| RIDE (any) | ≥50 | ≥40 | 0.10-0.15 SOL | Linear interpolation: mag 40→100 maps to 0.10→0.15 |

```rust
#[repr(C)]
pub struct DecisionThresholds {
    pub min_entry_score: f64,           // 50.0 — reject below
    pub min_magnitude_for_ride: f64,    // 40.0 — RIDE threshold

    pub scalp_size_low: u64,            // 100_000_000 (0.10 SOL)
    pub scalp_size_mid: u64,            // 120_000_000 (0.12 SOL)
    pub scalp_size_high: u64,           // 150_000_000 (0.15 SOL)
    pub ride_size_min: u64,             // 100_000_000 (0.10 SOL)
    pub ride_size_max: u64,             // 150_000_000 (0.15 SOL)

    pub scalp_tier_mid: f64,            // 60.0
    pub scalp_tier_high: f64,           // 70.0
}
```

**Sizing function:**
```rust
fn size(entry_score: f64, magnitude_score: f64) -> (EntryAction, u64) {
    if entry_score < min_entry_score { return (Reject, 0); }

    if magnitude_score < min_magnitude_for_ride {
        // SCALP: tiered by entry_score
        let size = if entry_score >= scalp_tier_high { scalp_size_high }     // 0.15
                   else if entry_score >= scalp_tier_mid { scalp_size_mid }  // 0.12
                   else { scalp_size_low };                                   // 0.10
        (Scalp, size)
    } else {
        // RIDE: linear interpolation by magnitude
        let t = ((magnitude_score - min_magnitude_for_ride) / (100.0 - min_magnitude_for_ride)).min(1.0);
        let size = ride_size_min + ((ride_size_max - ride_size_min) as f64 * t) as u64;
        (Ride, size)
    }
}
```

**Note:** All positions start as SCALP regardless of the `EntryAction`. The `Ride` action sets the `magnitude_estimate` on `OpenPosition` so the RIDE qualification check knows this token was predicted to be RIDE-worthy. The actual RIDE transition only happens after confirming buys arrive (see Part 4).

### 2.5 EntryInput Struct

```rust
#[repr(C)]
pub struct EntryInput {
    pub vsol_reserves: u64,
    pub vtoken_reserves: u64,
    pub sol_amount: u64,
    pub buy_count_1s: u16,
    pub buy_count_2s: u16,
    pub buy_count_5s: u16,
    pub sell_count_5s: u16,
    pub unique_buyers_30s: u16,
    pub _pad: u16,
    pub volume_sol_5s: u64,
    pub vsol_delta_3s: u64,
    pub time_since_last_buy_ms: u64,
    pub history_age_ms: u64,
    pub creator_sell_at_ms: u64,
    pub now_ms: u64,
    pub max_wallet_vol_30s: u64,
    pub total_buy_vol_30s: u64,
}
// 112 bytes, stack-allocated
```

### 2.6 EntryDecision Output

```rust
#[derive(Debug, Clone, Copy)]
pub enum EntryAction { Reject, Scalp, Ride }

#[derive(Debug, Clone, Copy)]
pub struct EntryDecision {
    pub action: EntryAction,
    pub entry_score: f64,       // 0-100
    pub magnitude_score: f64,   // 0-100
    pub size_lamports: u64,     // 0 if Reject
}
```

---

## Part 3: RIDE Mode Spec

### 3.1 RideState Struct — 64 Bytes Exactly

**Critical design choice (resolving conflict between ARCH_RIDE and ARCH_HOTPATH):** ARCH_RIDE uses integer milli-SOL vSOL (mvsol, u32) for all prices — zero floating-point. ARCH_HOTPATH uses f64 for vSOL prices. **We use the ARCH_RIDE integer approach** because:
1. Zero f64 on hot path = no FP pipeline stalls
2. Precision at 0.001 SOL (0.003% at vSOL=30) exceeds needs
3. Fits in 64 bytes

```rust
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RideState {
    // Byte 0-3: Phase + counters
    pub phase: u8,               // 0=Early, 1=Momentum, 2=Tighten (RidePhase as u8)
    pub unique_wallets: u8,      // Confirming wallet count during ride
    pub sells_during_ride: u16,  // Sell event counter

    // Byte 4-19: Price levels (milli-SOL vSOL, u32)
    pub entry_mvsol: u32,        // Entry vSOL in mvsol (1 mvsol = 0.001 SOL = 1M lamports)
    pub peak_mvsol: u32,         // Peak vSOL (high water mark, ratchet up only)
    pub floor_mvsol: u32,        // Hard floor = entry × 1.01 (never sell below this)
    pub trail_stop_mvsol: u32,   // Current trailing stop (ratchet up only)

    // Byte 20-35: Timestamps
    pub ride_start_ms: u64,      // Epoch ms when RIDE activated
    pub last_buy_ms: u64,        // Last confirming buy timestamp (for gap detection)

    // Byte 36-43: Rate + trail tracking
    pub buy_rate_at_start: u16,  // buy_count_5s when RIDE activated
    pub trail_distance_bp: u16,  // Current trail in basis points (800 = 8.00%)
    pub flags: u16,              // Bitflags (ride_flags module)
    pub _reserved: u16,

    // Byte 44-51: Volume tracking (milli-SOL, u32)
    pub total_buy_msol: u32,     // Total buy SOL during ride (mvsol units)
    pub total_sell_msol: u32,    // Total sell SOL during ride (mvsol units)

    // Byte 52-59: Cascade detection
    pub recent_sell_count_3s: u8,
    pub recent_sell_window_start: u8,
    pub _pad: [u8; 2],
    pub last_sell_msol: u32,     // Size of most recent sell (for whale detection)

    // Byte 60-63: Entry context
    pub entry_gain_bp: u16,      // Unrealized gain at RIDE activation (bp)
    pub _pad2: [u8; 2],
}

const _: () = assert!(std::mem::size_of::<RideState>() == 64);
```

**Unit conversion:**
```rust
fn lamports_to_mvsol(lamports: u64) -> u32 { ((lamports + 500_000) / 1_000_000) as u32 }
fn mvsol_to_lamports(mvsol: u32) -> u64 { mvsol as u64 * 1_000_000 }
```

### 3.2 Phase Transitions — Exact Thresholds

Phase transitions are one-way (Early → Momentum → Tighten) and triggered by EITHER time OR gain, whichever comes first.

**CRITICAL NOTE on trail math:** ARCH_RIDE stores trail as **price-space basis points** (800 = 8.0% price trail). But from QUANT_RIDE_C §3, the trail stop comparison happens in **vSOL space**. The relationship is:

```
Price trail of P% requires vSOL trail of (1 - √(1-P)) × 100%

8% price trail → 4.081% vSOL trail → 408 vSOL basis points
6% price trail → 3.045% vSOL trail → 305 vSOL basis points
4% price trail → 2.020% vSOL trail → 202 vSOL basis points
2% price trail → 1.005% vSOL trail → 101 vSOL basis points
```

**RECONCILIATION DECISION:** The `trail_distance_bp` field in RideState stores **vSOL-space basis points** (not price-space). The RideConfig JSON accepts price-space percentages and converts during deserialization:

```rust
fn price_pct_to_vsol_bp(price_pct: f64) -> u16 {
    // vSOL_trail = 1 - sqrt(1 - price_trail)
    let vsol_trail = 1.0 - (1.0 - price_pct / 100.0).sqrt();
    (vsol_trail * 10_000.0).round() as u16
}

// 8.0% → 408 bp, 6.0% → 305 bp, 4.0% → 202 bp, 2.0% → 101 bp
```

**Complete transition table:**

| Transition | Time Trigger | Gain Trigger (vSOL ratio × 10000) | Trail (price) | Trail (vSOL bp) | Constant Name |
|---|---|---|---|---|---|
| Entry → EARLY | immediate | — | 8% | 408 | `TRAIL_EARLY_BP = 408` |
| EARLY → MOMENTUM | 15,000 ms | 10724 (= √1.15 × 10000, i.e. 15% price gain) | 6% | 305 | `TRAIL_MOMENTUM_BP = 305` |
| MOMENTUM → TIGHTEN | 60,000 ms | 12247 (= √1.50 × 10000, i.e. 50% price gain) | 4% | 202 | `TRAIL_TIGHTEN_BP = 202` |
| Any → EMERGENCY | — | signal-triggered (see §3.5) | 2% | 101 | `TRAIL_EMERGENCY_BP = 101` |

**Phase transition implementation (integer-only):**

```rust
fn update_phase(state: &mut RideState, current_mvsol: u32, elapsed_ms: u64, config: &RideConfig) {
    // Gain check: current × 10000 >= entry × threshold_ratio
    let gain_check = |threshold_ratio: u16| -> bool {
        (current_mvsol as u64) * 10_000 >= (state.entry_mvsol as u64) * (threshold_ratio as u64)
    };

    if state.phase == RidePhase::Early as u8 {
        if elapsed_ms >= config.early_to_momentum_ms || gain_check(config.gain_momentum_vsol_fp) {
            state.phase = RidePhase::Momentum as u8;
            transition_trail(state, config.momentum_trail_bp);
        }
    }
    if state.phase == RidePhase::Momentum as u8 {
        if elapsed_ms >= config.momentum_to_tighten_ms || gain_check(config.gain_tighten_vsol_fp) {
            state.phase = RidePhase::Tighten as u8;
            transition_trail(state, config.tighten_trail_bp);
        }
    }
}

fn transition_trail(state: &mut RideState, new_trail_bp: u16) {
    state.trail_distance_bp = new_trail_bp;
    // Recompute trail stop from current peak — trail can only ratchet up
    let new_stop = compute_trail_stop(state.peak_mvsol, new_trail_bp);
    let new_stop = new_stop.max(state.floor_mvsol);
    if new_stop > state.trail_stop_mvsol {
        state.trail_stop_mvsol = new_stop;
    }
}
```

### 3.3 Trailing Stop in vSOL Space — The Critical Math

From QUANT_RIDE_C §3.4:

```rust
/// Compute trail stop from peak vSOL and trail width in vSOL basis points.
/// trail_stop = peak × (10000 - trail_bp) / 10000
/// All integer. u64 intermediate prevents overflow.
#[inline(always)]
fn compute_trail_stop(peak_mvsol: u32, trail_bp: u16) -> u32 {
    let keep_bp = 10_000u32 - trail_bp as u32;
    ((peak_mvsol as u64 * keep_bp as u64) / 10_000) as u32
}

/// Check trail stop: pure integer compare
#[inline(always)]
fn should_exit_trail(current_m