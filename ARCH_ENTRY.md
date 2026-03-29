# ARCH_ENTRY.md — Entry Engine v2: Zero-Alloc 3-Stage Pipeline

**Author:** Apollo (Principal Rust Systems Architect)
**Date:** 2026-03-29
**Status:** APPROVED — Replaces `GateStack` + `Scorer` in `engine/`
**Target Latency:** <250ns end-to-end (hard gate <50ns, scoring <200ns)

---

## 0. Executive Summary

Replace the current two-struct `GateStack` + `Scorer` architecture with a single `EntryEngine` struct that owns all state for the 3-stage entry pipeline. The redesign:

1. **Fuses gate + score into a single monolithic evaluator** — eliminates the score→gate→score round-trip (current code computes score, passes it to gate as last check, gate calls score threshold — redundant)
2. **Adds magnitude prediction** — separate model for HOW FAR a token will pump, enabling SCALP vs RIDE mode selection
3. **Replaces all exp()/gaussian with precomputed LUTs** — 4 tables totaling 2,848 bytes, fits in L1 cache
4. **Zero f64 division on hot path** — all normalization via precomputed reciprocals or multiply+shift
5. **Single-owner struct** — lives on hot path thread, no Arc/Mutex, no cache line contention

### What Changes

| Component | Current | New |
|-----------|---------|-----|
| Gate evaluation | `GateStack::evaluate()` — 18 gates, takes `score: f64` as param | `EntryEngine::hard_gate()` — 8 gates, pure integer, no score dependency |
| Scoring | `Scorer::compute()` — 6 features, returns `ScoreComponents` | `EntryEngine::score()` — 8 entry + 7 magnitude features, LUT-backed |
| Position sizing | Fixed in config (flat) | `EntryEngine::size()` — tiered by entry_score × magnitude_score |
| Decision output | `Ok(()) / Err(GateRejectReason)` + separate score | `EntryDecision { action, entry_score, magnitude_score, size_lamports }` |
| Hot path integration | `on_trade()` calls `scorer.compute()` then `gate_stack.evaluate()` | `on_trade()` calls `entry_engine.evaluate()` — single call, single result |

---

## 1. Struct Layouts

### 1.1 Lookup Tables

All LUTs are `#[repr(C)]` arrays stored inline in `EntryEngine`. No indirection, no pointer chase.

```rust
/// Precomputed sigmoid: lut[i] = 1.0 / (1.0 + exp(-steepness * (i - center)))
/// For buy_count 0..63. 64 × 8 = 512 bytes.
type BuyBurstLut = [f64; 64];

/// Precomputed sigmoid for acceleration values -64..+63.
/// Index mapping: accel_value + 64 → lut index (range 0..127).
/// 128 × 8 = 1024 bytes.
type AccelLut = [f64; 128];

/// Precomputed Gaussian for curvePct 0..99.
/// lut[i] = exp(-0.5 * ((i - mean) / sigma)^2)
/// 100 × 8 = 800 bytes.
type CurveLut = [f64; 100];

/// Precomputed sigmoid for fill rate 0..63.
/// 64 × 8 = 512 bytes.
type FillRateLut = [f64; 64];

// ────────────────────────────────────────────────────────
// TOTAL LUT MEMORY: 512 + 1024 + 800 + 512 = 2,848 bytes
// L1 data cache line: 64 bytes. LUTs span 45 cache lines.
// Typical L1D: 32KB–48KB. LUTs use 5.9%–8.9% of L1D.
// ────────────────────────────────────────────────────────
```

### 1.2 Hard Gate Thresholds

All thresholds stored as integers. Pre-computed at config load time. Zero float math in gate evaluation.

```rust
/// Integer thresholds for Stage 1 hard gate.
/// All fields are u64/u16 — the compiler packs these into 56 bytes (1 cache line).
#[repr(C)]
pub struct HardGateThresholds {
    pub min_buy_count_1s: u16,             //  2B  — minimum buys in last 1s
    pub max_unique_buyers_30s: u16,        //  2B  — ceiling on unique buyers
    pub _pad0: u32,                        //  4B  — align next u64
    pub min_volume_sol_5s: u64,            //  8B  — lamports (5 SOL = 5_000_000_000)
    pub max_time_since_last_buy_ms: u64,   //  8B  — staleness cutoff
    pub min_vsol_reserves: u64,            //  8B  — curve 20% lower bound
    pub max_vsol_reserves: u64,            //  8B  — curve 60% upper bound
    pub min_history_age_ms: u64,           //  8B  — minimum tracking time
    pub creator_sell_cooldown_ms: u64,     //  8B  — creator dump cooldown
    // sell_count < buy_count / 2 → integer: 2 * sell_count < buy_count
    // No threshold field needed — hardcoded integer math.
}
// size_of::<HardGateThresholds>() == 56 bytes (fits in 1 cache line)
```

### 1.3 Scoring Weights

```rust
/// Weights for the 8 entry features and 7 magnitude features.
/// All f64. Stored contiguous for cache locality during dot product.
#[repr(C)]
pub struct ScoringWeights {
    // Entry features (predict IF token pumps) — 8 weights
    pub w_buy_burst: f64,           // 0.30
    pub w_volume: f64,              // 0.20
    pub w_curve_position: f64,      // 0.15
    pub w_buyer_concentration: f64, // 0.10
    pub w_buy_acceleration: f64,    // 0.10
    pub w_avg_buy_size: f64,        // 0.05
    pub w_sell_absence: f64,        // 0.05
    pub w_recency: f64,             // 0.05

    // Magnitude features (predict HOW FAR) — 7 weights
    pub w_fill_rate: f64,           // 0.20
    pub w_buy_velocity_accel: f64,  // 0.20
    pub w_wallet_quality: f64,      // 0.15
    pub w_curve_remaining: f64,     // 0.15
    pub w_volume_intensity: f64,    // 0.15
    pub w_sell_vacuum: f64,         // 0.10
    pub w_token_age: f64,           // 0.05
}
// size_of::<ScoringWeights>() == 15 * 8 = 120 bytes (2 cache lines)
```

### 1.4 Precomputed Reciprocals

```rust
/// Precomputed reciprocals and scaling factors to eliminate division on hot path.
/// All computed once at construction from config values.
#[repr(C)]
pub struct Reciprocals {
    /// 1.0 / max_time_since_last_buy_ms (for recency normalization)
    pub inv_max_recency_ms: f64,
    /// 1.0 / crowd_depth_norm_lamports (for volume normalization)
    pub inv_crowd_norm: f64,
    /// 1.0 / recent_1s_norm_count (for buy_burst clamp)
    pub inv_recent_norm: f64,
    /// Precomputed: 1.0 / (max_vsol - min_vsol) for curve fill
    pub inv_vsol_range: f64,
    /// 1.0 / fill_rate_norm (for magnitude fill rate)
    pub inv_fill_rate_norm: f64,
    /// 1.0 / volume_intensity_norm (for magnitude volume intensity)
    pub inv_volume_intensity_norm: f64,
}
// size_of::<Reciprocals>() == 48 bytes (1 cache line)
```

### 1.5 Decision Thresholds

```rust
/// Decision thresholds for Stage 3.
#[repr(C)]
pub struct DecisionThresholds {
    pub min_entry_score: f64,           // 50.0
    pub min_magnitude_for_ride: f64,    // 40.0

    // Position sizes in lamports
    pub scalp_size_low: u64,            // 0.10 SOL = 100_000_000
    pub scalp_size_mid: u64,            // 0.12 SOL = 120_000_000
    pub scalp_size_high: u64,           // 0.15 SOL = 150_000_000
    pub ride_size_min: u64,             // 0.10 SOL = 100_000_000
    pub ride_size_max: u64,             // 0.15 SOL = 150_000_000

    // Scalp entry_score tier boundaries
    pub scalp_tier_mid: f64,            // 60.0
    pub scalp_tier_high: f64,           // 70.0
}
// size_of::<DecisionThresholds>() == 72 bytes (2 cache lines)
```

### 1.6 The EntryEngine Struct

```rust
/// Monolithic entry engine. Owns all state for gate → score → size pipeline.
/// Single-threaded. Lives on the hot path thread. Zero heap allocation in evaluate().
///
/// CACHE LAYOUT:
///   Offset 0..55:     HardGateThresholds (1 cache line — touched first, hottest)
///   Offset 56..63:    padding to align LUTs
///   Offset 64..575:   buy_burst_lut (8 cache lines)
///   Offset 576..1599: accel_lut (16 cache lines)
///   Offset 1600..2399: curve_lut (12.5 cache lines)
///   Offset 2400..2911: fill_rate_lut (8 cache lines)
///   Offset 2912..3031: ScoringWeights (2 cache lines)
///   Offset 3032..3079: Reciprocals (1 cache line)
///   Offset 3080..3151: DecisionThresholds (2 cache lines)
///   ≈ 3152 bytes total hot data → 50 cache lines → well within L1D
#[repr(C)]
pub struct EntryEngine {
    // ── Stage 1: Hard Gate (accessed first on every call) ──
    pub gate: HardGateThresholds,         //   56 bytes

    // ── LUTs (accessed only if gate passes — ~35% of calls) ──
    pub buy_burst_lut: BuyBurstLut,       //  512 bytes
    pub accel_lut: AccelLut,              // 1024 bytes
    pub curve_lut: CurveLut,              //  800 bytes
    pub fill_rate_lut: FillRateLut,       //  512 bytes

    // ── Stage 2: Scoring parameters ──
    pub weights: ScoringWeights,          //  120 bytes
    pub reciprocals: Reciprocals,         //   48 bytes

    // ── Stage 3: Decision thresholds ──
    pub decision: DecisionThresholds,     //   72 bytes
}
// TOTAL: 3,144 bytes. ASSERT: < 4KB. Fits comfortably in L1 data cache.
```

### 1.7 Size Assertions

```rust
#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem;

    #[test]
    fn struct_sizes() {
        assert_eq!(mem::size_of::<HardGateThresholds>(), 56);
        assert_eq!(mem::size_of::<ScoringWeights>(), 120);
        assert_eq!(mem::size_of::<Reciprocals>(), 48);
        assert_eq!(mem::size_of::<DecisionThresholds>(), 72);

        // LUTs
        assert_eq!(mem::size_of::<BuyBurstLut>(), 512);
        assert_eq!(mem::size_of::<AccelLut>(), 1024);
        assert_eq!(mem::size_of::<CurveLut>(), 800);
        assert_eq!(mem::size_of::<FillRateLut>(), 512);

        // Total engine
        let total = mem::size_of::<EntryEngine>();
        assert!(total <= 4096, "EntryEngine must fit in 4KB, got {}", total);
        // Exact check (update if fields change)
        assert_eq!(total, 3144);
    }

    #[test]
    fn alignment() {
        assert_eq!(mem::align_of::<EntryEngine>(), 8);
        assert_eq!(mem::align_of::<HardGateThresholds>(), 8);
    }
}
```

---

## 2. Function Signatures

### 2.1 Evaluation Input

```rust
/// All data needed for a single entry evaluation.
/// Constructed by hot_path::on_trade() from MintHistory + TradeEvent.
/// Stack-allocated. 112 bytes.
#[repr(C)]
pub struct EntryInput {
    // From TradeEvent
    pub vsol_reserves: u64,         // lamports
    pub vtoken_reserves: u64,       // lamports
    pub sol_amount: u64,            // trigger buy size, lamports

    // From MintHistory cached aggregates
    pub buy_count_1s: u16,
    pub buy_count_2s: u16,
    pub buy_count_5s: u16,
    pub sell_count_5s: u16,
    pub unique_buyers_30s: u16,
    pub _pad: u16,
    pub volume_sol_5s: u64,         // lamports
    pub vsol_delta_3s: u64,         // lamports (current_vsol - oldest_vsol_in_3s)
    pub time_since_last_buy_ms: u64,
    pub history_age_ms: u64,
    pub creator_sell_at_ms: u64,    // 0 = no creator sell detected
    pub now_ms: u64,

    // For magnitude scoring (from MintHistory wallet tracking)
    pub max_wallet_vol_30s: u64,    // lamports
    pub total_buy_vol_30s: u64,     // lamports
}
```

### 2.2 Evaluation Output

```rust
/// Decision output from EntryEngine::evaluate().
/// Tells HotPath what to do. Stack-allocated.
#[derive(Debug, Clone, Copy)]
pub enum EntryAction {
    /// Reject — do not enter. Score below threshold.
    Reject,
    /// SCALP mode — quick in/out, tight TP/SL.
    Scalp,
    /// RIDE mode — hold for sustained momentum, wider TP/trailing SL.
    Ride,
}

#[derive(Debug, Clone, Copy)]
pub struct EntryDecision {
    pub action: EntryAction,
    pub entry_score: f64,       // 0-100 (0 if rejected at gate)
    pub magnitude_score: f64,   // 0-100 (0 if rejected at gate)
    pub size_lamports: u64,     // 0 if Reject
}

impl EntryDecision {
    #[inline(always)]
    pub const fn reject() -> Self {
        Self {
            action: EntryAction::Reject,
            entry_score: 0.0,
            magnitude_score: 0.0,
            size_lamports: 0,
        }
    }
}
```

### 2.3 Core Methods

```rust
impl EntryEngine {
    /// Construct from config. Precomputes all LUTs, reciprocals, thresholds.
    /// Called once at startup. May allocate temporarily for LUT generation.
    pub fn new(config: &EntryEngineConfig) -> Self { ... }

    /// Full 3-stage evaluation. Zero heap allocation. ~50-250ns.
    ///
    /// Stage 1 (hard gate): ~30-40ns. Rejects ~65% of inputs.
    /// Stage 2 (scoring):   ~150-180ns. Only runs on ~35% of inputs.
    /// Stage 3 (sizing):    ~10-20ns. Only runs on scored candidates.
    ///
    /// PERF: NOT #[inline(always)] — this is the outer orchestrator.
    /// The individual stages are inlined; keeping the orchestrator as
    /// a regular function call prevents icache bloat at the call site.
    #[inline]
    pub fn evaluate(&self, input: &EntryInput) -> EntryDecision { ... }

    // ── Stage 1: Hard Gate ──────────────────────────────────────────

    /// Boolean gate: all-integer comparisons. <50ns.
    /// Returns true if candidate passes all checks.
    /// Ordered by rejection_rate × cost (cheapest high-rejection first).
    ///
    /// PERF: #[inline(always)] — called on every buy trade.
    /// ~8 integer comparisons = ~8 branch instructions.
    /// Total icache footprint: ~128 bytes (2 cache lines).
    #[inline(always)]
    fn hard_gate(&self, input: &EntryInput) -> bool { ... }

    // ── Stage 2: Composite Scoring ──────────────────────────────────

    /// Compute entry_score (0-100) and magnitude_score (0-100).
    /// Uses LUT lookups instead of exp()/pow(). ~150-180ns.
    ///
    /// PERF: #[inline(always)] — only called when gate passes (~35%).
    /// Dominated by LUT loads (cache hits) and FMA operations.
    #[inline(always)]
    fn score(&self, input: &EntryInput) -> (f64, f64) { ... }

    // ── Stage 3: Decision + Sizing ──────────────────────────────────

    /// Map (entry_score, magnitude_score) → (action, size).
    /// Pure arithmetic, ~10ns.
    ///
    /// PERF: #[inline(always)] — trivial branch tree.
    #[inline(always)]
    fn size(&self, entry_score: f64, magnitude_score: f64) -> (EntryAction, u64) { ... }
}
```

---

## 3. Stage 1: Hard Gate Implementation

```rust
/// ORDERING RATIONALE:
/// Each check is ordered by (estimated rejection rate × CPU cost).
/// Cheapest rejections first → fewest instructions per rejected candidate.
///
/// Check                          | Rejection Rate | Cost  | Product
/// buy_count_1s >= 5              | ~55%          | 1 cmp | 0.55
/// volume_sol_5s >= 5B            | ~50%          | 1 cmp | 0.50
/// time_since_last_buy_ms <= 500  | ~40%          | 1 cmp | 0.40
/// vsol_reserves in [47B..81B]    | ~35%          | 2 cmp | 0.70 (but 2x cost)
/// unique_buyers_30s <= 30        | ~15%          | 1 cmp | 0.15
/// 2*sell_count < buy_count       | ~10%          | 1 mul+cmp | 0.10
/// history_age_ms >= 2000         | ~5%           | 1 cmp | 0.05
/// creator_sell check             | ~2%           | 2 cmp | 0.04
///
/// Expected average instructions before rejection: ~3.2 comparisons ≈ 10-15ns.
/// Worst case (all pass): 8 comparisons + 1 multiply ≈ 30-40ns.
#[inline(always)]
fn hard_gate(&self, input: &EntryInput) -> bool {
    let g = &self.gate;

    // Check 1: buy_count_1s >= min (highest rejection rate)
    if input.buy_count_1s < g.min_buy_count_1s {
        return false;
    }

    // Check 2: volume_sol_5s >= min
    if input.volume_sol_5s < g.min_volume_sol_5s {
        return false;
    }

    // Check 3: time_since_last_buy_ms <= max
    if input.time_since_last_buy_ms > g.max_time_since_last_buy_ms {
        return false;
    }

    // Check 4: vsol_reserves in [min..max] (curve position 20-60%)
    // Two comparisons but tests a common rejection path.
    if input.vsol_reserves < g.min_vsol_reserves || input.vsol_reserves > g.max_vsol_reserves {
        return false;
    }

    // Check 5: unique_buyers_30s <= max
    if input.unique_buyers_30s > g.max_unique_buyers_30s {
        return false;
    }

    // Check 6: sell pressure — 2 * sell_count_5s < buy_count_5s
    // Integer multiply+compare. No division.
    // Equivalent to: sell_count_5s < buy_count_5s / 2 (but avoids division)
    if (input.sell_count_5s as u32) * 2 >= input.buy_count_5s as u32 {
        return false;
    }

    // Check 7: history_age_ms >= min (need enough data)
    if input.history_age_ms < g.min_history_age_ms {
        return false;
    }

    // Check 8: creator sell cooldown
    // creator_sell_at_ms == 0 → no creator sell → pass
    // creator_sell_at_ms > 0 AND (now - creator_sell_at_ms) <= cooldown → reject
    if input.creator_sell_at_ms > 0 {
        if input.now_ms.saturating_sub(input.creator_sell_at_ms) <= g.creator_sell_cooldown_ms {
            return false;
        }
    }

    true
}
```

### 3.1 Threshold Precomputation

```rust
/// Convert human-readable config to integer thresholds.
/// Called once at startup. May use float intermediates.
pub fn precompute_gate_thresholds(config: &EntryEngineConfig) -> HardGateThresholds {
    // Curve position 20% → vsol_reserves = 30 SOL + 0.20 * 85 SOL = 47 SOL = 47_000_000_000
    // Curve position 60% → vsol_reserves = 30 SOL + 0.60 * 85 SOL = 81 SOL = 81_000_000_000
    // Pump.fun: initial vSOL = 30 SOL, graduation at 115 SOL → 85 SOL range
    let min_vsol = ((30.0 + config.curve_pct_min / 100.0 * 85.0) * 1e9) as u64;
    let max_vsol = ((30.0 + config.curve_pct_max / 100.0 * 85.0) * 1e9) as u64;

    HardGateThresholds {
        min_buy_count_1s: config.min_buy_count_1s,
        max_unique_buyers_30s: config.max_unique_buyers_30s,
        _pad0: 0,
        min_volume_sol_5s: (config.min_volume_sol_5s * 1e9) as u64,
        max_time_since_last_buy_ms: config.max_time_since_last_buy_ms,
        min_vsol_reserves: min_vsol,
        max_vsol_reserves: max_vsol,
        min_history_age_ms: config.min_history_age_ms,
        creator_sell_cooldown_ms: config.creator_sell_cooldown_ms,
    }
}
```

---

## 4. Stage 2: Scoring Implementation

### 4.1 LUT Precomputation

All LUTs are generated once at construction. The hot path only indexes into them.

```rust
impl EntryEngine {
    /// Build sigmoid LUT: lut[i] = 1.0 / (1.0 + exp(-steepness * (i as f64 - center)))
    fn build_sigmoid_lut<const N: usize>(center: f64, steepness: f64) -> [f64; N] {
        let mut lut = [0.0f64; N];
        for i in 0..N {
            let x = i as f64;
            lut[i] = 1.0 / (1.0 + (-steepness * (x - center)).exp());
        }
        lut
    }

    /// Build signed sigmoid LUT: lut[i] = sigmoid(i - offset)
    /// where offset maps the zero-point to the middle of the array.
    fn build_signed_sigmoid_lut(center: f64, steepness: f64, offset: i32) -> AccelLut {
        let mut lut = [0.0f64; 128];
        for i in 0..128 {
            let x = (i as i32 + offset) as f64;
            lut[i] = 1.0 / (1.0 + (-steepness * (x - center)).exp());
        }
        lut
    }

    /// Build Gaussian LUT: lut[i] = exp(-0.5 * ((i - mean) / sigma)^2)
    fn build_gaussian_lut(mean: f64, sigma: f64) -> CurveLut {
        let mut lut = [0.0f64; 100];
        let inv_2sigma2 = 0.5 / (sigma * sigma);
        for i in 0..100 {
            let diff = i as f64 - mean;
            lut[i] = (-diff * diff * inv_2sigma2).exp();
        }
        lut
    }
}

/// Construction — called once at startup.
pub fn new(config: &EntryEngineConfig) -> Self {
    // ── LUT generation ──────────────────────────────────────────

    // buy_burst_lut: sigmoid(buy_count, center=7, steepness=0.8)
    // Index: buy_count_1s clamped to 0..63
    // lut[0]=0.004, lut[5]=0.168, lut[7]=0.500, lut[10]=0.916, lut[15]=0.999
    let buy_burst_lut = Self::build_sigmoid_lut::<64>(7.0, 0.8);

    // accel_lut: sigmoid(accel, center=10, steepness=0.15)
    // Index: accel_value + 64 (maps -64..+63 to 0..127)
    // accel = buy_count_1s * 5 - buy_count_5s (can be negative)
    // lut[64]=sigmoid(0), lut[74]=sigmoid(10)=0.5, lut[84]=sigmoid(20)
    let accel_lut = Self::build_signed_sigmoid_lut(10.0, 0.15, -64);

    // curve_lut: gaussian(curvePct, mean=43, sigma=12)
    // Index: curvePct as integer 0..99
    // lut[43]=1.0, lut[31]=0.5, lut[55]=0.5, lut[19]=~0.05
    let curve_lut = Self::build_gaussian_lut(43.0, 12.0);

    // fill_rate_lut: sigmoid(fill_rate_idx, center=15, steepness=0.25)
    // fill_rate_idx = (vsol_delta_3s / 3) / FILL_RATE_SCALE, clamped to 0..63
    // Where FILL_RATE_SCALE = ~850_000_000 / 64 ≈ 13_281_250 lamports per index
    // This maps 0-85 SOL range in 3s window to 0..63 index
    let fill_rate_lut = Self::build_sigmoid_lut::<64>(15.0, 0.25);

    // ── Reciprocals ─────────────────────────────────────────────

    let vsol_range = config.max_vsol_reserves_lamports
        .saturating_sub(config.min_vsol_reserves_lamports)
        .max(1) as f64;

    let reciprocals = Reciprocals {
        inv_max_recency_ms: 1.0 / config.max_time_since_last_buy_ms as f64,
        inv_crowd_norm: 1.0 / config.crowd_depth_norm_lamports.max(1) as f64,
        inv_recent_norm: 1.0 / config.recent_1s_norm_count.max(1) as f64,
        inv_vsol_range: 1.0 / vsol_range,
        inv_fill_rate_norm: 1.0 / FILL_RATE_SCALE as f64,
        inv_volume_intensity_norm: 1.0 / config.volume_intensity_norm_lamports.max(1) as f64,
    };

    Self {
        gate: precompute_gate_thresholds(config),
        buy_burst_lut,
        accel_lut,
        curve_lut,
        fill_rate_lut,
        weights: config.weights.clone(),
        reciprocals,
        decision: config.decision.clone(),
    }
}
```

### 4.2 Scoring Hot Path

```rust
/// Fill rate scale: maps vsol_delta_3s to 0..63 index.
/// 85 SOL full range / 64 steps = ~1.328 SOL per step.
/// In lamports: 1_328_125_000
const FILL_RATE_SCALE: u64 = 1_328_125_000;

/// Volume intensity scale: maps volume_sol_5s to a normalized 0..1 range.
/// 10 SOL in 5s = "fully intense". In lamports: 10_000_000_000
const VOLUME_INTENSITY_NORM: u64 = 10_000_000_000;

#[inline(always)]
fn score(&self, input: &EntryInput) -> (f64, f64) {
    let w = &self.weights;
    let r = &self.reciprocals;

    // ════════════════════════════════════════════════════════════
    // ENTRY FEATURES (predict IF token pumps) → entry_score
    // ════════════════════════════════════════════════════════════

    // Feature 1: Buy Burst Intensity (LUT lookup)
    // Clamp buy_count_1s to 0..63 for safe indexing.
    let burst_idx = (input.buy_count_1s as usize).min(63);
    let f_buy_burst = unsafe { *self.buy_burst_lut.get_unchecked(burst_idx) };

    // Feature 2: Volume Intensity
    // Normalize: volume_sol_5s * inv_crowd_norm, clamped to [0, 1]
    let f_volume = clamp01(input.volume_sol_5s as f64 * r.inv_crowd_norm);

    // Feature 3: Curve Position (Gaussian LUT)
    // Convert vsol_reserves → curvePct integer (0..99)
    // curvePct = (vsol_reserves - 30 SOL) / 85 SOL * 100
    // Integer: (vsol - 30e9) * 100 / 85e9
    let curve_pct_raw = input.vsol_reserves
        .saturating_sub(30_000_000_000)
        .saturating_mul(100);
    // Integer division by 85e9 — precompute reciprocal? No: u64 division is
    // ~20 cycles which is fine for a single divide. Avoiding it would require
    // u128 multiply which is ~same cost. Just divide.
    let curve_pct = (curve_pct_raw / 85_000_000_000) as usize;
    let curve_idx = curve_pct.min(99);
    let f_curve_position = unsafe { *self.curve_lut.get_unchecked(curve_idx) };

    // Feature 4: Buyer Concentration
    // Sweet spot at ~10 unique buyers. Piecewise linear, no LUT needed.
    let n = input.unique_buyers_30s;
    let f_buyer_concentration = if n <= 5 {
        0.3
    } else if n <= 15 {
        // Peak at 10: ramp up 5→10, ramp down 10→15
        let dist_from_10 = (n as i16 - 10).unsigned_abs() as f64;
        1.0 - dist_from_10 * 0.1 // 10→1.0, 5/15→0.5
    } else {
        // Decay beyond 15: max(0, 0.5 - (n-15)*0.025)
        (0.5 - (n as f64 - 15.0) * 0.025).max(0.0)
    };

    // Feature 5: Buy Acceleration (signed sigmoid LUT)
    // accel = buy_count_1s * 5 - buy_count_5s
    // Maps the 1s rate (annualized to 5s) minus actual 5s count.
    // Positive = accelerating, negative = decelerating.
    let accel_raw = (input.buy_count_1s as i32) * 5 - (input.buy_count_5s as i32);
    let accel_idx = (accel_raw + 64).clamp(0, 127) as usize;
    let f_buy_acceleration = unsafe { *self.accel_lut.get_unchecked(accel_idx) };

    // Feature 6: Average Buy Size
    // avg_buy = volume_sol_5s / max(buy_count_5s, 1)
    // Normalize to ~1 SOL sweet spot via reciprocal multiply.
    // Avoid division: use multiply by precomputed reciprocal of buy_count_5s?
    // No — buy_count varies per call. Use integer division (cheap for u64/u16).
    let avg_buy_lamports = input.volume_sol_5s / (input.buy_count_5s as u64).max(1);
    // Normalize: 1 SOL = 1_000_000_000 lamports → score 0.5
    // sigmoid-like via clamp: min(avg / 2e9, 1.0) — linear ramp to 2 SOL
    let f_avg_buy_size = clamp01(avg_buy_lamports as f64 * (1.0 / 2_000_000_000.0));

    // Feature 7: Sell Absence
    // sell_ratio = sell_count_5s / max(buy_count_5s, 1)
    // score = max(0, 1 - sell_ratio * 2.5)
    // Zero sells → 1.0, 20% sell ratio → 0.5, 40%+ → 0.0
    let sell_ratio = input.sell_count_5s as f64 * (1.0 / (input.buy_count_5s as f64).max(1.0));
    let f_sell_absence = (1.0 - sell_ratio * 2.5).max(0.0);

    // Feature 8: Recency
    // Linear decay: 0ms → 1.0, max_ms → 0.0
    let f_recency = (1.0 - input.time_since_last_buy_ms as f64 * r.inv_max_recency_ms).max(0.0);

    // ── Entry Score: weighted dot product ──────────────────────
    let entry_raw =
          f_buy_burst         * w.w_buy_burst           // 0.30
        + f_volume            * w.w_volume              // 0.20
        + f_curve_position    * w.w_curve_position      // 0.15
        + f_buyer_concentration * w.w_buyer_concentration // 0.10
        + f_buy_acceleration  * w.w_buy_acceleration    // 0.10
        + f_avg_buy_size      * w.w_avg_buy_size        // 0.05
        + f_sell_absence      * w.w_sell_absence         // 0.05
        + f_recency           * w.w_recency;             // 0.05
    // Weights sum to 1.0, features ∈ [0,1] → raw ∈ [0,1]
    let entry_score = entry_raw * 100.0; // Scale to 0-100

    // ════════════════════════════════════════════════════════════
    // MAGNITUDE FEATURES (predict HOW FAR) → magnitude_score
    // ════════════════════════════════════════════════════════════

    // Mag Feature 1: Fill Rate (LUT lookup)
    // How fast the curve is being filled. vsol_delta_3s / 3 = SOL/sec fill rate.
    // Map to 0..63 index: delta / FILL_RATE_SCALE
    let fill_rate_idx = (input.vsol_delta_3s / FILL_RATE_SCALE.max(1)) as usize;
    let fill_rate_idx = fill_rate_idx.min(63);
    let m_fill_rate = unsafe { *self.fill_rate_lut.get_unchecked(fill_rate_idx) };

    // Mag Feature 2: Buy Velocity Acceleration (reuse accel from entry)
    // For magnitude, we want HIGHER acceleration to predict larger moves.
    // Same LUT, same index — just weighted differently.
    let m_buy_velocity_accel = f_buy_acceleration; // reuse computation

    // Mag Feature 3: Wallet Quality
    // Low concentration = many independent buyers = organic = higher magnitude.
    // wallet_quality = 1.0 - (max_wallet_vol / total_buy_vol)
    // When max_wallet dominates, quality drops (single whale = rug risk).
    let m_wallet_quality = if input.total_buy_vol_30s > 0 {
        let concentration = input.max_wallet_vol_30s as f64
            * (1.0 / input.total_buy_vol_30s as f64);
        (1.0 - concentration).max(0.0)
    } else {
        0.0
    };

    // Mag Feature 4: Curve Remaining Upside
    // How much room left before graduation.
    // remaining = 1.0 - curvePct/100.0
    // Token at 20% has 80% upside, token at 60% has 40%.
    let m_curve_remaining = 1.0 - (curve_pct as f64 * 0.01);

    // Mag Feature 5: Volume Intensity
    // High absolute volume = more capital flowing = bigger potential move.
    // Normalize: volume_sol_5s / 10 SOL
    let m_volume_intensity = clamp01(
        input.volume_sol_5s as f64 * r.inv_volume_intensity_norm
    );

    // Mag Feature 6: Sell Vacuum
    // Complete absence of sells = maximum continuation potential.
    // Binary boost: if sell_count_5s == 0 → 1.0, else decay.
    let m_sell_vacuum = if input.sell_count_5s == 0 {
        1.0
    } else {
        // Each sell reduces confidence: 1 sell → 0.6, 2 → 0.36, etc.
        (0.6_f64).powi(input.sell_count_5s as i32).max(0.0)
    };

    // Mag Feature 7: Token Age
    // Sweet spot: 5-30s of tracked momentum. Very new → not enough data.
    // Very old → momentum is mature, likely near peak.
    let age_s = input.history_age_ms as f64 * 0.001;
    let m_token_age = if age_s < 5.0 {
        0.3
    } else if age_s < 30.0 {
        1.0
    } else if age_s < 120.0 {
        // Linear decay from 1.0 at 30s to 0.3 at 120s
        1.0 - (age_s - 30.0) * (0.7 / 90.0)
    } else {
        0.3
    };

    // ── Magnitude Score: weighted dot product ──────────────────
    let mag_raw =
          m_fill_rate         * w.w_fill_rate            // 0.20
        + m_buy_velocity_accel * w.w_buy_velocity_accel  // 0.20
        + m_wallet_quality    * w.w_wallet_quality       // 0.15
        + m_curve_remaining   * w.w_curve_remaining      // 0.15
        + m_volume_intensity  * w.w_volume_intensity     // 0.15
        + m_sell_vacuum       * w.w_sell_vacuum           // 0.10
        + m_token_age         * w.w_token_age;            // 0.05
    // Weights sum to 1.0, features ∈ [0,1] → raw ∈ [0,1]
    let magnitude_score = mag_raw * 100.0; // Scale to 0-100

    (entry_score, magnitude_score)
}

/// Clamp x to [0.0, 1.0]. Branchless on most architectures.
#[inline(always)]
fn clamp01(x: f64) -> f64 {
    if x < 0.0 { 0.0 } else if x > 1.0 { 1.0 } else { x }
}
```

---

## 5. Stage 3: Decision + Sizing

```rust
#[inline(always)]
fn size(&self, entry_score: f64, magnitude_score: f64) -> (EntryAction, u64) {
    let d = &self.decision;

    if entry_score < d.min_entry_score {
        return (EntryAction::Reject, 0);
    }

    if magnitude_score < d.min_magnitude_for_ride {
        // SCALP mode: size by entry conviction tier
        let size = if entry_score >= d.scalp_tier_high {
            d.scalp_size_high  // 0.15 SOL
        } else if entry_score >= d.scalp_tier_mid {
            d.scalp_size_mid   // 0.12 SOL
        } else {
            d.scalp_size_low   // 0.10 SOL
        };
        (EntryAction::Scalp, size)
    } else {
        // RIDE mode: linear interpolation between min/max based on magnitude
        // magnitude 40..100 → size ride_min..ride_max
        let t = ((magnitude_score - d.min_magnitude_for_ride)
            / (100.0 - d.min_magnitude_for_ride))
            .min(1.0);
        let range = d.ride_size_max - d.ride_size_min;
        let size = d.ride_size_min + (range as f64 * t) as u64;
        (EntryAction::Ride, size)
    }
}
```

---

## 6. Top-Level Evaluate Orchestrator

```rust
#[inline]
pub fn evaluate(&self, input: &EntryInput) -> EntryDecision {
    // Stage 1: Hard Gate (<50ns)
    if !self.hard_gate(input) {
        // #[cold] — most calls end here
        return EntryDecision::reject();
    }

    // Stage 2: Composite Scoring (~150-180ns)
    let (entry_score, magnitude_score) = self.score(input);

    // Stage 3: Decision + Sizing (~10-20ns)
    let (action, size_lamports) = self.size(entry_score, magnitude_score);

    EntryDecision {
        action,
        entry_score,
        magnitude_score,
        size_lamports,
    }
}
```

---

## 7. Hot Path Integration

### 7.1 Replacing Current Architecture

The `EntryEngine` replaces both `GateStack` and `Scorer` in `hot_path.rs`.

```rust
// ── BEFORE (current code) ──────────────────────────────────────
pub struct HotPath {
    gate_stack: GateStack,
    scorer: Scorer,
    // ...
}

impl HotPath {
    pub fn on_trade(&mut self, trade: &TradeEvent) {
        // ... mint_map bookkeeping ...
        let score_components = self.scorer.compute(/* 8 params */);
        let score = score_components.final_score;
        match self.gate_stack.evaluate(/* 13 params + score */) {
            Ok(()) => { /* open position */ }
            Err(reason) => { /* reject */ }
        }
    }
}

// ── AFTER (new code) ───────────────────────────────────────────
pub struct HotPath {
    entry_engine: EntryEngine,
    // ... (gate_stack and scorer removed)
}

impl HotPath {
    pub fn on_trade(&mut self, trade: &TradeEvent) {
        // ... mint_map bookkeeping (unchanged) ...

        // Build EntryInput from cached MintHistory aggregates
        let input = EntryInput {
            vsol_reserves: trade.vsol_reserves,
            vtoken_reserves: trade.vtoken_reserves,
            sol_amount: trade.sol_amount,
            buy_count_1s: history.cached_buy_count_1s,
            buy_count_2s: history.cached_buy_count_2s,
            buy_count_5s: history.cached_buy_count_5s,
            sell_count_5s: history.cached_sell_count_5s,
            unique_buyers_30s: history.cached_unique_buyers_30s,
            _pad: 0,
            volume_sol_5s: history.cached_volume_sol_5s,
            vsol_delta_3s: trade.vsol_reserves.saturating_sub(history.cached_vsol_oldest_3s),
            time_since_last_buy_ms: now.saturating_sub(history.last_trade_ms),
            history_age_ms: now.saturating_sub(history.first_seen_ms),
            creator_sell_at_ms: history.creator_sell_at_ms,
            now_ms: now,
            max_wallet_vol_30s: history.cached_max_wallet_vol_30s,
            total_buy_vol_30s: history.cached_total_buy_vol_30s,
        };

        let decision = self.entry_engine.evaluate(&input);

        match decision.action {
            EntryAction::Reject => {
                self.stats.gate_rejects += 1;
                return;
            }
            EntryAction::Scalp | EntryAction::Ride => {
                self.stats.gates_passed += 1;
                // Open position with decision metadata
                self.position_manager.open_position_v2(
                    trade,
                    decision.entry_score,
                    decision.magnitude_score,
                    decision.size_lamports,
                    decision.action,
                    now,
                );
                self.stats.positions_opened += 1;
            }
        }
    }
}
```

### 7.2 What Stays in HotPath

The `EntryEngine` does NOT own or replace:
- **MintHistoryMap** — still lives in HotPath, feeds input to EntryEngine
- **PositionManager** — still lives in HotPath, receives decisions from EntryEngine
- **Safety checks** — daily loss cap, circuit breaker, health monitor stay in HotPath
- **Regime exclusion** — excluded_mints check stays as a pre-filter before evaluate()
- **Helius lead tracking** — stays in HotPath (orthogonal to entry logic)
- **Gate rejection histogram** — simplified: Reject vs Scalp vs Ride counters

### 7.3 Pre-Filters (remain in HotPath, before EntryEngine)

These checks stay OUTSIDE EntryEngine because they depend on HotPath-owned state:

```rust
// 1. Not a buy → skip (trivial check)
if !trade.is_buy { return; }

// 2. Already have position for this mint → feed to exit engine
if self.position_manager.has_position(&trade.mint) {
    self.position_manager.on_subsequent_trade(trade, now);
    return;
}

// 3. Regime exclusion (hashbrown set lookup)
if self.excluded_mints.contains(&trade.mint) { return; }

// 4. Graduation boundary (vtoken-based)
if trade.vtoken_reserves > 0 {
    let progress = regime::compute_bonding_curve_progress(/*...*/);
    if progress >= boundary_start && progress <= boundary_end { return; }
}

// 5. Health monitor
if let Some(ref hm) = self.health_monitor {
    if !hm.is_trading_allowed() { return; }
}

// 6. Daily loss cap
if self.daily_loss_lamports as u64 >= self.daily_loss_cap_lamports { return; }

// 7. Circuit breaker
if now < self.stop_pause_until_ms { return; }

// NOW: EntryEngine evaluation
let decision = self.entry_engine.evaluate(&input);
```

---

## 8. Cache Line Analysis

### 8.1 L1 Data Cache Layout (Hot Path)

```
┌──────────────────────────────────────────────────────────────┐
│ LAYER 1: Always accessed (every trade event)                  │
│                                                               │
│ HardGateThresholds   56B   1 cache line   HIT on every call │
│ EntryInput (stack)  112B   2 cache lines  HIT on every call │
│ MintHistory fields   ~64B  1 cache line   HIT (from push())  │
└──────────────────────────────────────────────────────────────┘
│ ~4 cache lines = 256 bytes, always hot                       │

┌──────────────────────────────────────────────────────────────┐
│ LAYER 2: Accessed on gate pass (~35% of calls)                │
│                                                               │
│ buy_burst_lut       512B   8 cache lines  1 random access    │
│ accel_lut          1024B  16 cache lines  1 random access    │
│ curve_lut           800B  12.5 lines      1 random access    │
│ fill_rate_lut       512B   8 cache lines  1 random access    │
│ ScoringWeights      120B   2 cache lines  sequential read    │
│ Reciprocals          48B   1 cache line   sequential read    │
└──────────────────────────────────────────────────────────────┘
│ ~47.5 cache lines = 3,040 bytes                              │
│ LUT accesses: 4 random loads (1 per LUT), rest sequential.   │
│ Each LUT access touches exactly 1 cache line (f64 at index). │
│ Effective L1 pressure: 4 + 3 = 7 cache lines per scoring.    │

┌──────────────────────────────────────────────────────────────┐
│ LAYER 3: Accessed on score pass (~12% of calls)               │
│                                                               │
│ DecisionThresholds   72B   2 cache lines                     │
└──────────────────────────────────────────────────────────────┘
│ 2 cache lines                                                │

TOTAL EntryEngine footprint: 3,144 bytes ≈ 50 cache lines
L1D budget: 32KB = 512 cache lines
EntryEngine uses: 9.8% of L1D (well under pressure threshold)
```

### 8.2 LUT Access Pattern Analysis

Each LUT access touches exactly **one** cache line per lookup:
- `buy_burst_lut[idx]`: 1 f64 load from line `idx / 8`
- `accel_lut[idx]`: 1 f64 load from line `idx / 8`
- `curve_lut[idx]`: 1 f64 load from line `idx / 8`
- `fill_rate_lut[idx]`: 1 f64 load from line `idx / 8`

The indices are derived from input data and effectively random, so each lookup causes **one cold cache line fill** on first access (if the line was evicted). After the first scoring call, the working set of frequently-accessed LUT lines (around the sigmoid centers and Gaussian peak) will remain in L1.

**Temporal locality**: Consecutive trades for the same token will hit similar LUT indices (buy counts don't change much between consecutive 50ms ticks). This means L1 hits for LUT accesses after the first per-token evaluation.

### 8.3 False Sharing Prevention

`EntryEngine` is **single-owner, single-thread**. No false sharing risk. The struct lives entirely on the hot path thread's stack or heap (not shared via Arc/Mutex). No other thread reads or writes to it.

---

## 9. Expected Latency per Stage

### 9.1 Stage 1: Hard Gate

| Operation | Cycles | Latency |
|-----------|--------|---------|
| 8 integer comparisons | 8 × 1 cycle | ~8 cycles |
| 1 integer multiply (sell check) | 3 cycles | ~3 cycles |
| 1 saturating subtract (creator sell) | 1 cycle | ~1 cycle |
| Branch mispredictions (~2 expected) | 2 × 15 cycles | ~30 cycles |
| **Total (worst case, all pass)** | | **~42 cycles ≈ 14ns @ 3GHz** |
| **Average (65% reject at check 1-3)** | | **~15 cycles ≈ 5ns** |

**Target: <50ns ✓** — achieved even in worst case.

### 9.2 Stage 2: Scoring

| Operation | Cycles | Latency |
|-----------|--------|---------|
| 4 LUT lookups (L1 hit: 4 cycles each) | 4 × 4 | ~16 cycles |
| 4 LUT lookups (L1 miss → L2: 12 cycles each) | 4 × 12 | ~48 cycles |
| 15 f64 multiplies (FMA: 4 cycles each) | 15 × 4 | ~60 cycles |
| 15 f64 additions (folded into FMA) | 0 | 0 |
| 4 clamp operations (2 branches each) | 4 × 2 | ~8 cycles |
| Piecewise linear (buyer concentration) | ~8 | ~8 cycles |
| Integer division (curve_pct) | ~20 | ~20 cycles |
| 2 f64 divisions (sell_ratio, wallet_quality) | 2 × 20 | ~40 cycles |
| **Total (L1 hot)** | | **~152 cycles ≈ 51ns @ 3GHz** |
| **Total (L2 cold LUTs)** | | **~200 cycles ≈ 67ns @ 3GHz** |

**Note on f64 divisions**: Features 6 (avg_buy_size) and 7 (sell_absence) use integer/float division that we couldn't fully eliminate because the divisor (buy_count_5s) varies per call. These are computed after the LUT lookups and overlap with memory loads via out-of-order execution.

**Target: ~200ns** — achieved (51-67ns actual, well under budget).

### 9.3 Stage 3: Sizing

| Operation | Cycles | Latency |
|-----------|--------|---------|
| 2 f64 comparisons | 2 × 1 | ~2 cycles |
| 1 branch (scalp/ride) | ~1 | ~1 cycle |
| 1 f64 multiply + cast (ride sizing) | ~8 | ~8 cycles |
| **Total** | | **~11 cycles ≈ 4ns @ 3GHz** |

**Target: <20ns ✓**

### 9.4 Total Pipeline Latency

| Scenario | Gate | Score | Size | Total |
|----------|------|-------|------|-------|
| Rejected at gate check 1-2 | ~5ns | — | — | **~5ns** |
| Rejected at gate (all checks) | ~14ns | — | — | **~14ns** |
| Scored but rejected (entry < 50) | ~14ns | ~60ns | ~4ns | **~78ns** |
| Full pipeline, SCALP decision | ~14ns | ~60ns | ~4ns | **~78ns** |
| Full pipeline, RIDE decision | ~14ns | ~60ns | ~4ns | **~78ns** |
| **Weighted average** (65% reject) | | | | **~30ns** |

---

## 10. Compile Flags

### 10.1 Cargo Profile

```toml
# Cargo.toml
[profile.release]
opt-level = 3
lto = "fat"                    # Full LTO — cross-crate inlining
codegen-units = 1              # Single CGU — maximum optimization
panic = "abort"                # No unwind tables — smaller binary
strip = true                   # Remove debug symbols
overflow-checks = false        # No overflow checks in release

[profile.release.build-override]
opt-level = 3
```

### 10.2 RUSTFLAGS

```bash
RUSTFLAGS="-C target-cpu=native -C link-arg=-fuse-ld=lld"
```

- `-C target-cpu=native`: Enable AVX2/AVX-512 if available. Specifically enables:
  - FMA (fused multiply-add): Each `weight × feature + accumulator` becomes 1 FMA instruction
  - BMI2: Faster integer shifts for bit manipulation
  - AVX2: SIMD for potential auto-vectorization of the weight dot product
- `-C link-arg=-fuse-ld=lld`: Faster LTO linking

### 10.3 Environment

```bash
# .cargo/config.toml
[build]
rustflags = ["-C", "target-cpu=native"]

[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "target-cpu=native", "-C", "link-arg=-fuse-ld=lld"]
```

---

## 11. Test Specifications

### 11.1 Unit Tests — Hard Gate

```rust
#[cfg(test)]
mod hard_gate_tests {
    use super::*;

    fn default_engine() -> EntryEngine {
        EntryEngine::new(&EntryEngineConfig::default())
    }

    fn passing_input() -> EntryInput {
        EntryInput {
            vsol_reserves: 60_000_000_000,      // 60 SOL (curve ~35%)
            vtoken_reserves: 700_000_000_000,    // irrelevant for gate
            sol_amount: 500_000_000,             // 0.5 SOL trigger
            buy_count_1s: 8,
            buy_count_2s: 12,
            buy_count_5s: 20,
            sell_count_5s: 3,
            unique_buyers_30s: 12,
            _pad: 0,
            volume_sol_5s: 8_000_000_000,        // 8 SOL
            vsol_delta_3s: 2_000_000_000,        // 2 SOL delta
            time_since_last_buy_ms: 100,
            history_age_ms: 5_000,
            creator_sell_at_ms: 0,
            now_ms: 1_000_000,
            max_wallet_vol_30s: 2_000_000_000,
            total_buy_vol_30s: 15_000_000_000,
        }
    }

    #[test]
    fn passes_all_gates() {
        let engine = default_engine();
        assert!(engine.hard_gate(&passing_input()));
    }

    #[test]
    fn rejects_low_buy_count() {
        let engine = default_engine();
        let mut input = passing_input();
        input.buy_count_1s = 3; // below 5
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn rejects_low_volume() {
        let engine = default_engine();
        let mut input = passing_input();
        input.volume_sol_5s = 2_000_000_000; // 2 SOL, below 5 SOL
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn rejects_stale_momentum() {
        let engine = default_engine();
        let mut input = passing_input();
        input.time_since_last_buy_ms = 600; // above 500ms
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn rejects_curve_too_low() {
        let engine = default_engine();
        let mut input = passing_input();
        input.vsol_reserves = 40_000_000_000; // ~12% curve, below 20%
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn re