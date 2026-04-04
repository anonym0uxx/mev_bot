# Entry Engine Architecture — Rust Systems Design

**Author:** Apollo (Principal Rust Systems Architect)
**Date:** 2026-03-29
**Source Spec:** `ENTRY_ENGINE_QUANT.md`
**Status:** Implementation-ready architecture

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [File Organization](#2-file-organization)
3. [Struct Layouts & Memory Analysis](#3-struct-layouts--memory-analysis)
4. [Stage 1: Hard Gate](#4-stage-1-hard-gate)
5. [Stage 2: Composite Scoring with LUTs](#5-stage-2-composite-scoring-with-luts)
6. [Stage 3: Kelly Position Sizing](#6-stage-3-kelly-position-sizing)
7. [EntryEngine: Unified Pipeline](#7-entryengine-unified-pipeline)
8. [Config Integration](#8-config-integration)
9. [Hot Path Integration](#9-hot-path-integration)
10. [LUT Precomputation](#10-lut-precomputation)
11. [Cache Line Analysis](#11-cache-line-analysis)
12. [Latency Budget](#12-latency-budget)
13. [Risk Management Integration](#13-risk-management-integration)
14. [Test Specifications](#14-test-specifications)
15. [Benchmark Specifications](#15-benchmark-specifications)
16. [Migration Plan](#16-migration-plan)
17. [Config Schema (canary.json)](#17-config-schema-canaryjson)

---

## 1. System Overview

### Current Flow (being replaced)

```
TradeEvent
    → Scorer::compute()         ← RUNS ON EVERY BUY (wasteful for rejects)
    → GateStack::evaluate()     ← 18+ branch checks, score passed as param
    → PositionManager::open()   ← static size tiers from config
```

**Problems:**
- Scorer runs BEFORE gate stack in `hot_path.rs` line ~195 — wasted work on 95%+ of events that get rejected by gates
- GateStack and Scorer are separate structs with separate configs
- No lookup tables — sigmoid/gaussian would need exp() calls
- Position sizing is flat tiers from config, not conviction-driven
- Gate ordering not systematically optimized by rejection probability × cost

### New Flow

```
TradeEvent
    → EntryEngine::evaluate()
        ├─ Stage 1: hard_gate()        <50ns   — boolean ops, ~65% reject rate
        ├─ Stage 2: composite_score()  ~200ns  — 8 features, LUT lookups
        └─ Stage 3: position_size()    ~10ns   — Kelly-derived tiers
    → PositionManager::open_position()  ← UNCHANGED
```

**Key improvement:** Score is ONLY computed for hard-gate survivors (~35% of buy events). Current code computes score for ALL buys, wasting ~200ns on the ~65% that get gate-rejected.

---

## 2. File Organization

### New Files

```
engine/
├── entry_engine.rs     ← NEW: Unified 3-stage pipeline (EntryEngine struct)
├── scoring.rs          ← NEW: ScoringEngine with LUTs, feature transforms
├── sizing.rs           ← NEW: Kelly position sizing (score → lamports)
├── config.rs           ← MODIFY: Add EntryEngineConfig, ScoringConfig, SizingConfig
├── hot_path.rs         ← MODIFY: Replace GateStack+Scorer with EntryEngine
├── mod.rs              ← MODIFY: Add new module declarations
├── gates.rs            ← KEEP: Retained for backward compat & tests (deprecated)
├── scorer.rs           ← KEEP: Retained for backward compat & tests (deprecated)
└── ... (other files unchanged)
```

### Module Declarations (mod.rs additions)

```rust
// Add to engine/mod.rs:
pub mod entry_engine;
pub mod scoring;
pub mod sizing;
```

---

## 3. Struct Layouts & Memory Analysis

### 3.1 EntryEngine (top-level struct)

```rust
/// The unified entry engine. Owns all precomputed state.
/// Lives on the hot-path thread — not Send/Sync (no Arc needed).
///
/// Memory layout (ordered hot → cold):
///   gate:           64 bytes   (1 cache line)  — accessed every evaluate()
///   weights:        64 bytes   (1 cache line)  — accessed on gate-pass only
///   buy_burst_lut: 512 bytes   (8 cache lines) — accessed on gate-pass only
///   accel_lut:    1024 bytes  (16 cache lines)  — accessed on gate-pass only
///   curve_lut:     800 bytes  (12.5 cache lines) — accessed on gate-pass only
///   sizing:         64 bytes   (1 cache line)  — accessed on score-pass only
///   risk_config:    48 bytes   (fits in 1 CL)  — accessed on score-pass only
///   risk_state:     64 bytes   (1 cache line)  — mutated on position open/close
///   stats:          64 bytes   (1 cache line)  — mutated every evaluate()
///
/// Total: ~2,704 bytes (≤43 cache lines)
/// Hot path (gate reject): touches 1-2 cache lines.
/// Warm path (gate pass, score reject): touches ~40 cache lines (LUTs).
/// Cold path (full entry): touches all ~43 cache lines.
#[repr(C)]
pub struct EntryEngine {
    // ── HOT: Stage 1 thresholds (accessed every call) ──────
    gate: HardGateConfig,               // 64 bytes, 1 CL

    // ── WARM: Stage 2 scoring (accessed ~35% of calls) ─────
    weights: ScoringWeights,            // 64 bytes, 1 CL
    buy_burst_lut: [f64; 64],           // 512 bytes, 8 CLs
    accel_lut: [f64; 128],              // 1,024 bytes, 16 CLs
    curve_lut: [f64; 100],              // 800 bytes, 12.5 CLs

    // ── COLD: Stage 3 sizing (accessed ~12% of calls) ──────
    sizing: SizingConfig,               // 64 bytes, 1 CL

    // ── COLD: Risk management ──────────────────────────────
    risk_config: RiskConfig,            // 48 bytes
    risk_state: RiskState,              // 64 bytes, 1 CL

    // ── Stats (mutated every call, but written not read) ────
    pub stats: EntryStats,              // 64 bytes, 1 CL

    // ── Scoring threshold (hot — used for branch) ──────────
    min_score: f64,                     // 8 bytes (lives after stats)
}
```

**Static size assertion:**
```rust
const _: () = assert!(std::mem::size_of::<EntryEngine>() <= 3072);
```

### 3.2 HardGateConfig (64 bytes, 1 cache line)

```rust
/// All hard gate thresholds. Packed into exactly 1 cache line.
/// Every field is a precomputed integer — zero float ops in gate evaluation.
///
/// Field ordering: by branch evaluation order (cheapest + highest-reject first).
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct HardGateConfig {
    pub curve_min_vsol: u64,             // 8B — precomputed: 47_000_000_000
    pub curve_max_vsol: u64,             // 8B — precomputed: 81_000_000_000
    pub min_volume_sol_5s: u64,          // 8B — 5_000_000_000 (5 SOL in lamports)
    pub min_buy_count_1s: u16,           // 2B — 5
    pub max_sell_ratio_buy_divisor: u16, // 2B — 2 (sell < buy / divisor)
    pub max_unique_buyers_30s: u16,      // 2B — 30
    pub _pad0: u16,                      // 2B — alignment padding
    pub min_history_age_ms: u64,         // 8B — 2000
    pub creator_sell_cooldown_ms: u64,   // 8B — 5000
    pub max_time_since_last_buy_ms: u64, // 8B — 500
    pub _pad1: [u8; 8],                  // 8B — fill to 64
}
// Size: 8+8+8+2+2+2+2+8+8+8+8 = 64 bytes.
const _: () = assert!(std::mem::size_of::<HardGateConfig>() == 64);
```

### 3.3 ScoringWeights (64 bytes, 1 cache line)

```rust
/// Precomputed scoring weights. Packed to exactly 1 cache line.
/// All f64 — used in the weighted sum.
/// Invariant: all 8 weights sum to 1.0 (verified at construction).
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct ScoringWeights {
    pub w_buy_burst: f64,            // 0.30
    pub w_volume: f64,               // 0.20
    pub w_curve_position: f64,       // 0.15
    pub w_buyer_concentration: f64,  // 0.10
    pub w_buy_acceleration: f64,     // 0.10
    pub w_avg_buy_size: f64,         // 0.05
    pub w_sell_absence: f64,         // 0.05
    pub w_momentum_recency: f64,     // 0.05
}
// Size: 8 × 8 = 64 bytes. Perfect cache line.
const _: () = assert!(std::mem::size_of::<ScoringWeights>() == 64);
```

### 3.4 SizingConfig (64 bytes, 1 cache line)

```rust
/// Kelly-derived position sizing tiers.
/// Pure integer ops at evaluation time (compare + min).
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct SizingConfig {
    // Tier thresholds — f64 for direct comparison with score
    pub tier_high_min_score: f64,    // 80.0
    pub tier_med_min_score: f64,     // 65.0
    pub tier_low_min_score: f64,     // 50.0

    // Tier sizes (lamports)
    pub tier_high_size: u64,         // 500_000_000 (0.50 SOL)
    pub tier_med_size: u64,          // 350_000_000 (0.35 SOL)
    pub tier_low_size: u64,          // 250_000_000 (0.25 SOL)

    // Bankroll fraction divisors (bankroll / N = max for tier)
    pub tier_high_bankroll_div: u64, // 10 (max 10%)
    pub tier_med_bankroll_div: u64,  // 14 (max ~7%)
}
// Size: 3×8 + 3×8 + 2×8 = 64 bytes.
const _: () = assert!(std::mem::size_of::<SizingConfig>() == 64);
```

### 3.5 RiskConfig + RiskState

```rust
/// Immutable risk management config. Loaded once at startup.
#[derive(Clone, Copy, Debug)]
pub struct RiskConfig {
    pub daily_loss_cap_lamports: u64,    // 8B
    pub max_consecutive_stops: u32,      // 4B
    pub stop_pause_duration_ms: u64,     // 8B
    pub max_daily_trades: u32,           // 4B
    pub loss_cooldown_ms: u64,           // 8B
    pub _pad: [u8; 16],                  // pad to 48B
}

/// Mutable risk state. Updated on position close.
/// Single-thread access only (hot-path thread).
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct RiskState {
    pub daily_loss_lamports: i64,        // 8B
    pub daily_reset_day: u32,            // 4B
    pub consecutive_stops: u32,          // 4B
    pub stop_pause_until_ms: u64,        // 8B
    pub daily_trade_count: u32,          // 4B
    pub _pad0: u32,                      // 4B
    pub last_loss_ms: u64,              // 8B
    pub bankroll_lamports: u64,          // 8B
    pub _pad1: [u8; 16],                // 16B → fill to 64
}
const _: () = assert!(std::mem::size_of::<RiskState>() == 64);
```

### 3.6 EntryStats (64 bytes, 1 cache line)

```rust
/// Entry engine counters. Updated on every evaluate() call.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(64))]
pub struct EntryStats {
    pub gate_evaluations: u64,   // total calls to evaluate()
    pub gate_passes: u64,        // passed hard gate → scoring
    pub score_passes: u64,       // passed scoring threshold → sizing
    pub positions_sized: u64,    // non-zero position size returned
    pub risk_rejects: u64,       // blocked by risk management
    pub score_sum: u64,          // integer sum of scores × 100 (for average)
    pub _pad: [u8; 16],          // fill to 64
}
const _: () = assert!(std::mem::size_of::<EntryStats>() == 64);
```

### 3.7 EntryDecision (return value)

```rust
/// Result of EntryEngine::evaluate(). Stack-allocated, Copy, no heap.
#[derive(Debug, Clone, Copy)]
pub enum EntryDecision {
    /// Hard gate rejected — no score computed. Zero cost beyond gate checks.
    GateReject,
    /// Score below threshold.
    ScoreReject { score: f64 },
    /// Risk management blocked entry.
    RiskReject { score: f64 },
    /// Entry approved. Score and position size in lamports.
    Enter { score: f64, size_lamports: u64 },
}
```

### 3.8 EntryInput (data bag for evaluate)

```rust
/// All inputs needed by the entry engine. Constructed by hot_path.rs
/// from TradeEvent + MintHistory cached aggregates.
/// Stack-allocated. No heap references.
#[derive(Clone, Copy, Debug)]
pub struct EntryInput {
    pub buy_count_1s: u16,
    pub buy_count_5s: u16,
    pub volume_sol_5s: u64,            // lamports
    pub sell_count_5s: u16,
    pub unique_buyers_30s: u16,
    pub history_age_ms: u64,
    pub creator_sell_at_ms: u64,
    pub time_since_last_buy_ms: u64,
    pub vsol_reserves: u64,            // from TradeEvent
    pub now_ms: u64,
}
// Size: 2+2+8+2+2+8+8+8+8+8 = 56 bytes. Fits in 1 cache line.
```

---

## 4. Stage 1: Hard Gate

### 4.1 Implementation

```rust
// engine/entry_engine.rs

impl EntryEngine {
    /// Stage 1: Hard gate. Boolean integer ops only.
    /// Target: <50ns. Expected reject rate: ~65% of buy events.
    ///
    /// PRECONDITIONS (checked by hot_path.rs before calling evaluate()):
    ///   - event.is_buy == true
    ///   - !excluded_mints.contains(mint)
    ///   - graduation boundary check passed
    ///   - health_monitor.is_trading_allowed()
    ///   - !position_manager.has_position(mint)
    ///
    /// Branch ordering: highest (reject_rate × cheapness) first.
    #[inline(always)]
    fn hard_gate(&self, input: &EntryInput) -> bool {
        let g = &self.gate;

        // ── Check 1: Curve band ─────────────────────────────────────
        // Single u64 range check. ~40% reject rate. 2 cycles.
        // Thresholds precomputed at config load from percentages:
        //   curve_min_vsol = 30e9 + 0.20 × 85e9 = 47e9
        //   curve_max_vsol = 30e9 + 0.60 × 85e9 = 81e9
        if input.vsol_reserves < g.curve_min_vsol
            || input.vsol_reserves > g.curve_max_vsol
        {
            return false;
        }

        // ── Check 2: Buy burst minimum ──────────────────────────────
        // u16 compare. ~30% reject of remaining. 1 cycle.
        if input.buy_count_1s < g.min_buy_count_1s {
            return false;
        }

        // ── Check 3: Volume floor ───────────────────────────────────
        // u64 compare. ~25% reject of remaining. 2 cycles.
        if input.volume_sol_5s < g.min_volume_sol_5s {
            return false;
        }

        // ── Check 4: Sell pressure ──────────────────────────────────
        // Integer multiply + compare. sell * 2 >= buy → reject.
        // Avoids division. 3 cycles.
        if (input.sell_count_5s as u32) * (g.max_sell_ratio_buy_divisor as u32)
            >= input.buy_count_5s as u32
        {
            return false;
        }

        // ── Check 5: Unique buyers cap ──────────────────────────────
        // u16 compare. 1 cycle.
        if input.unique_buyers_30s > g.max_unique_buyers_30s {
            return false;
        }

        // ── Check 6: History age minimum ────────────────────────────
        // u64 compare. 2 cycles.
        if input.history_age_ms < g.min_history_age_ms {
            return false;
        }

        // ── Check 7: Creator sell cooldown ──────────────────────────
        // u64 subtract + compare. 4 cycles. Low reject rate.
        if input.creator_sell_at_ms > 0
            && input.now_ms.saturating_sub(input.creator_sell_at_ms)
                < g.creator_sell_cooldown_ms
        {
            return false;
        }

        // ── Check 8: Momentum recency ───────────────────────────────
        // u64 compare. 2 cycles.
        if input.time_since_last_buy_ms > g.max_time_since_last_buy_ms {
            return false;
        }

        true
    }
}
```

### 4.2 Branch Ordering Rationale

| Order | Check | Est. Reject % (of remaining) | Cost (cycles) | Notes |
|-------|-------|------------------------------|---------------|-------|
| 1 | Curve band | ~40% | 2 | Range check on u64. Highest absolute reject rate. |
| 2 | Buy burst min | ~30% | 1 | Single u16 compare. Cheapest check. |
| 3 | Volume floor | ~25% | 2 | u64 compare. High reject on low-activity tokens. |
| 4 | Sell pressure | ~15% | 3 | Integer multiply. Structural: sells kill momentum. |
| 5 | Unique buyers cap | ~10% | 1 | u16 compare. Filters diffuse retail. |
| 6 | History age | ~5% | 2 | u64 compare. Rare reject (most tokens >2s old). |
| 7 | Creator sell | ~3% | 4 | Conditional subtract. Rare event. |
| 8 | Recency | ~5% | 2 | u64 compare. Filters stale momentum. |

**Expected total cost:** ~15 cycles on average (most events rejected by check 1-3).
At 3GHz: 15 cycles ≈ 5ns. Well under 50ns target.

---

## 5. Stage 2: Composite Scoring with LUTs

### 5.1 Fast Sigmoid (no exp())

```rust
// engine/scoring.rs

/// Algebraic sigmoid approximation. No transcendental functions.
///   sigmoid(x, center, k) ≈ 0.5 + 0.5 × z / (1 + |z|)
///   where z = k × (x - center)
///
/// Max error vs. true sigmoid: ~4.7% at z = ±1.24 (inflection region).
/// For our scoring use case, this error is negligible — the weights
/// and thresholds absorb it during calibration.
///
/// Cost: 1 subtract, 2 multiply, 1 fabs, 1 add, 1 divide ≈ 5-7ns.
#[inline(always)]
fn fast_sigmoid(value: f64, center: f64, k: f64) -> f64 {
    let z = k * (value - center);
    // Clamp output to [0, 1] — the algebraic sigmoid asymptotes but
    // can slightly exceed bounds due to float precision.
    let raw = 0.5 + 0.5 * z / (1.0 + z.abs());
    // No clamp needed: algebraic sigmoid is bounded in (0, 1) by construction.
    // But for safety on extreme inputs:
    if raw < 0.0 { 0.0 } else if raw > 1.0 { 1.0 } else { raw }
}
```

### 5.2 Scoring Function

```rust
// engine/scoring.rs (called by entry_engine.rs)

impl EntryEngine {
    /// Stage 2: Composite score. Returns 0.0–100.0.
    /// Target: ~200ns. All exp()/pow() calls replaced by LUT lookups
    /// or algebraic approximation.
    ///
    /// 8 features, each scored [0.0, 1.0], weighted, summed, scaled to 0–100.
    #[inline(always)]
    fn composite_score(&self, input: &EntryInput) -> f64 {
        let w = &self.weights;

        // ── Feature A: Buy burst intensity (weight 0.30) ────────────
        // LUT lookup: buy_burst_lut[min(buy_count_1s, 63)]
        // Precomputed sigmoid_ramp(x, center=7, k=0.8)
        let burst_idx = (input.buy_count_1s as usize).min(63);
        let f_burst = unsafe { *self.buy_burst_lut.get_unchecked(burst_idx) };

        // ── Feature B: Volume intensity (weight 0.20) ───────────────
        // Convert lamports → SOL via precomputed reciprocal (multiply, not divide).
        // Then fast_sigmoid(volume_sol, center=10.0, k=0.3).
        // Cost: 1 multiply + fast_sigmoid (~7ns) = ~9ns.
        let volume_sol = input.volume_sol_5s as f64 * (1.0 / 1_000_000_000.0);
        let f_volume = fast_sigmoid(volume_sol, 10.0, 0.3);

        // ── Feature C: Curve position (weight 0.15) ─────────────────
        // LUT lookup: curve_lut[curve_pct_idx]
        // curve_pct_idx = (vsol - 30_000_000_000) / 850_000_000
        //   → integer division, yields 0–99 for the 0%–99% range
        //   → 850_000_000 = 85_000_000_000 / 100 (1% of full curve in lamports)
        // Precomputed: gaussian(pct, mean=43, sigma=12)
        let curve_raw = input.vsol_reserves.saturating_sub(30_000_000_000);
        let curve_pct_idx = (curve_raw / 850_000_000) as usize;
        let f_curve = if curve_pct_idx < 100 {
            unsafe { *self.curve_lut.get_unchecked(curve_pct_idx) }
        } else {
            0.0
        };

        // ── Feature D: Buyer concentration (weight 0.10) ────────────
        // Piecewise integer logic. No LUT needed — 3 branches.
        // Peak at ~10 unique buyers, decay above 15.
        let n = input.unique_buyers_30s;
        let f_concentration = match n {
            0..=5 => 0.3,
            6..=10 => 0.3 + (n - 5) as f64 * 0.14,
            11..=15 => 1.0 - (n - 10) as f64 * 0.06,
            _ => {
                let v = 0.7 - (n.saturating_sub(15)) as f64 * 0.04;
                if v < 0.2 { 0.2 } else { v }
            }
        };

        // ── Feature E: Buy acceleration (weight 0.10) ───────────────
        // LUT lookup: accel_lut[clamp(accel + 64, 0, 127)]
        // accel = buy_count_1s × 5 - buy_count_5s (integer subtraction)
        // Precomputed: sigmoid(accel, center=10, k=0.15)
        let accel_raw = (input.buy_count_1s as i32) * 5 - (input.buy_count_5s as i32);
        let accel_idx = (accel_raw + 64).clamp(0, 127) as usize;
        let f_accel = unsafe { *self.accel_lut.get_unchecked(accel_idx) };

        // ── Feature F: Average buy size (weight 0.05) ───────────────
        // avg_lamports = volume_5s / max(buy_count_5s, 1) [integer division]
        // Then fast_sigmoid(avg_sol, center=1.0, k=1.0).
        // Integer division is ~15 cycles on x86. Acceptable for 0.05 weight.
        let avg_lamports = input.volume_sol_5s
            / (input.buy_count_5s as u64).max(1);
        let avg_sol = avg_lamports as f64 * (1.0 / 1_000_000_000.0);
        let f_avg_size = fast_sigmoid(avg_sol, 1.0, 1.0);

        // ── Feature G: Sell absence (weight 0.05) ───────────────────
        // sell_ratio = sell_count / max(buy_count, 1)
        // score = clamp(1.0 - sell_ratio × 2.5, 0.0, 1.0)
        // Cost: 1 int→f64 convert, 1 divide, 1 multiply, 1 subtract = ~7ns
        let buy_f = (input.buy_count_5s as f64).max(1.0);
        let sell_ratio = input.sell_count_5s as f64 / buy_f;
        let f_sell_absence = (1.0 - sell_ratio * 2.5).clamp(0.0, 1.0);

        // ── Feature H: Momentum recency (weight 0.05) ───────────────
        // score = clamp(1.0 - time_since_last_buy_ms / 500.0, 0.0, 1.0)
        // Precomputed reciprocal: 1/500 = 0.002
        // Cost: 1 multiply, 1 subtract = ~3ns
        let f_recency = (1.0 - input.time_since_last_buy_ms as f64 * 0.002)
            .clamp(0.0, 1.0);

        // ── Weighted sum → [0.0, 100.0] ─────────────────────────────
        // 8 multiplies + 7 adds = ~15 cycles
        let raw = f_burst         * w.w_buy_burst
                + f_volume        * w.w_volume
                + f_curve         * w.w_curve_position
                + f_concentration * w.w_buyer_concentration
                + f_accel         * w.w_buy_acceleration
                + f_avg_size      * w.w_avg_buy_size
                + f_sell_absence  * w.w_sell_absence
                + f_recency       * w.w_momentum_recency;

        raw * 100.0
    }
}
```

### 5.3 Feature Summary Table

| Feature | Method | Cost (ns) | LUT? | Hot-path notes |
|---------|--------|-----------|------|----------------|
| buy_burst | LUT[64] | ~2 | ✅ | Array index, no bounds check (unsafe) |
| volume | fast_sigmoid | ~9 | ❌ | 1 multiply (lamports→SOL) + algebraic sigmoid |
| curve_position | LUT[100] | ~3 | ✅ | Integer division + array index |
| buyer_concentration | piecewise | ~5 | ❌ | 3 branches, integer compare + multiply |
| buy_acceleration | LUT[128] | ~3 | ✅ | Integer subtract + clamp + array index |
| avg_buy_size | fast_sigmoid | ~20 | ❌ | 1 integer division + algebraic sigmoid |
| sell_absence | arithmetic | ~7 | ❌ | 1 f64 division + multiply |
| momentum_recency | arithmetic | ~3 | ❌ | 1 multiply + subtract |
| weighted_sum | 8 FMA ops | ~8 | — | 8 multiplies + 7 adds |
| **Total** | | **~60ns** | | Well under 200ns budget |

---

## 6. Stage 3: Kelly Position Sizing

### 6.1 Implementation

```rust
// engine/sizing.rs

impl EntryEngine {
    /// Stage 3: Kelly-derived position sizing.
    /// Maps composite score → position size in lamports.
    /// Target: <10ns. Pure integer ops. Branch-prediction friendly.
    ///
    /// The common case (score < 50 → rejected by scoring threshold
    /// before reaching here) is never evaluated. Most calls that
    /// reach here are low-conviction (50-65), which is the first branch.
    ///
    /// Returns 0 if score < min threshold (should not happen if called
    /// after scoring threshold check, but defensive).
    #[inline(always)]
    fn position_size(&self, score: f64) -> u64 {
        let s = &self.sizing;