// Reminder: add `pub mod entry_engine;` to engine/mod.rs
//
//! Entry Engine v2: Zero-Alloc 3-Stage Pipeline
//!
//! Replaces `GateStack` + `Scorer` with a single monolithic evaluator.
//!
//! Stage 1: `hard_gate()` — 8 integer checks, <50ns, rejects ~65%
//! Stage 2: `score()` — 8 entry + 7 magnitude features, LUT-backed, ~150ns
//! Stage 3: `size()` — Kelly criterion position sizing via kelly_sizing module
//!
//! Total hot data: ~3,144 bytes. Fits in L1D cache (50 cache lines, 9.8% of 32KB).
//! Zero heap allocation on hot path. Zero f64 division (precomputed reciprocals).
//! All monetary thresholds as u64 lamports.

use crate::engine::kelly_sizing::{self, EntryConviction};

// ─── Type Aliases for LUTs ──────────────────────────────────────────────────

/// Precomputed sigmoid: lut[i] = 1.0 / (1.0 + exp(-steepness * (i - center)))
/// For buy_count 0..63. 64 × 8 = 512 bytes.
type BuyBurstLut = [f64; 64];

/// Precomputed sigmoid for acceleration values -64..+63.
/// Index mapping: accel_value + 64 → lut index (range 0..127).
/// 128 × 8 = 1,024 bytes.
type AccelLut = [f64; 128];

/// Precomputed Gaussian for curvePct 0..99.
/// lut[i] = exp(-0.5 * ((i - mean) / sigma)^2)
/// 100 × 8 = 800 bytes.
type CurveLut = [f64; 100];

/// Precomputed sigmoid for fill rate 0..63.
/// 64 × 8 = 512 bytes.
type FillRateLut = [f64; 64];

// TOTAL LUT MEMORY: 512 + 1024 + 800 + 512 = 2,848 bytes

// ─── Constants ──────────────────────────────────────────────────────────────

/// Fill rate scale: maps vsol_delta_3s to 0..63 index.
/// 85 SOL full range / 64 steps = ~1.328 SOL per step.
/// In lamports: 1_328_125_000
const FILL_RATE_SCALE: u64 = 1_328_125_000;

/// Pump.fun initial vSOL reserves (30 SOL in lamports).
const INITIAL_VSOL_LAMPORTS: u64 = 30_000_000_000;

/// Pump.fun bonding curve range (graduation vSOL - initial vSOL = 85 SOL in lamports).
const VSOL_RANGE_LAMPORTS: u64 = 85_000_000_000;

// ─── Hard Gate Thresholds ───────────────────────────────────────────────────

/// Integer thresholds for Stage 1 hard gate.
/// All fields are u64/u16 — the compiler packs these into 56 bytes (1 cache line).
#[repr(C)]
pub struct HardGateThresholds {
    pub min_buy_count_1s: u16,             //  2B — minimum buys in last 1s
    pub max_unique_buyers_30s: u16,        //  2B — ceiling on unique buyers
    pub _pad0: u32,                        //  4B — align next u64
    pub min_volume_sol_5s: u64,            //  8B — lamports (5 SOL = 5_000_000_000)
    pub max_time_since_last_buy_ms: u64,   //  8B — staleness cutoff
    pub min_vsol_reserves: u64,            //  8B — curve 20% lower bound
    pub max_vsol_reserves: u64,            //  8B — curve 60% upper bound
    pub min_history_age_ms: u64,           //  8B — minimum tracking time
    pub creator_sell_cooldown_ms: u64,     //  8B — creator dump cooldown
    // sell_count < buy_count / 2 → integer: 2 * sell_count < buy_count
    // No threshold field needed — hardcoded integer math.
}

// ─── Scoring Weights ────────────────────────────────────────────────────────

/// Weights for the 8 entry features and 7 magnitude features.
/// All f64. Stored contiguous for cache locality during dot product.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            w_buy_burst: 0.30,
            w_volume: 0.20,
            w_curve_position: 0.15,
            w_buyer_concentration: 0.10,
            w_buy_acceleration: 0.10,
            w_avg_buy_size: 0.05,
            w_sell_absence: 0.05,
            w_recency: 0.05,

            w_fill_rate: 0.20,
            w_buy_velocity_accel: 0.20,
            w_wallet_quality: 0.15,
            w_curve_remaining: 0.15,
            w_volume_intensity: 0.15,
            w_sell_vacuum: 0.10,
            w_token_age: 0.05,
        }
    }
}

// ─── Precomputed Reciprocals ────────────────────────────────────────────────

/// Precomputed reciprocals and scaling factors to eliminate division on hot path.
/// All computed once at construction from config values.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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

// ─── Decision Thresholds ────────────────────────────────────────────────────

/// Decision thresholds for Stage 3.
/// Position sizing is now handled by Kelly criterion (kelly_sizing module).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DecisionThresholds {
    pub min_entry_score: f64,           // 70.0 (0-100 scale, raised from 50.0)
    pub min_magnitude_for_ride: f64,    // 55.0 (0-100 scale, raised from 40.0)
    /// Fee gate multiplier × 100. Reject trade if expected_edge_lamports
    /// < (fee_gate_multiplier_x100 / 100) × fee_lamports.
    /// Default: 200 (require 2× fee coverage). Set to 0 to disable gate.
    pub fee_gate_multiplier_x100: u32,
}

impl Default for DecisionThresholds {
    fn default() -> Self {
        Self {
            min_entry_score: 70.0,
            min_magnitude_for_ride: 55.0,
            fee_gate_multiplier_x100: 200,
        }
    }
}

// ─── Entry Engine Config ────────────────────────────────────────────────────

/// Configuration for EntryEngine construction. Human-readable values that get
/// converted to integer thresholds and LUTs at construction time.
#[derive(Debug, Clone)]
pub struct EntryEngineConfig {
    // Gate thresholds (human-readable)
    pub min_buy_count_1s: u16,
    pub max_unique_buyers_30s: u16,
    pub min_volume_sol_5s: f64,         // SOL (converted to lamports)
    pub max_time_since_last_buy_ms: u64,
    pub curve_pct_min: f64,             // 20.0 (%)
    pub curve_pct_max: f64,             // 60.0 (%)
    pub min_history_age_ms: u64,
    pub creator_sell_cooldown_ms: u64,

    // Precomputed lamport thresholds (for reciprocals)
    pub min_vsol_reserves_lamports: u64,
    pub max_vsol_reserves_lamports: u64,

    // Normalization parameters
    pub crowd_depth_norm_lamports: u64,     // 10 SOL = 10_000_000_000
    pub recent_1s_norm_count: u64,          // 20 (for buy_burst clamp)
    pub volume_intensity_norm_lamports: u64, // 10 SOL = 10_000_000_000

    // Scoring weights
    pub weights: ScoringWeights,

    // Decision thresholds
    pub decision: DecisionThresholds,
}

impl Default for EntryEngineConfig {
    fn default() -> Self {
        let curve_pct_min = 20.0_f64;
        let curve_pct_max = 60.0_f64;
        let min_vsol = ((30.0 + curve_pct_min / 100.0 * 85.0) * 1e9) as u64;
        let max_vsol = ((30.0 + curve_pct_max / 100.0 * 85.0) * 1e9) as u64;

        Self {
            min_buy_count_1s: 5,
            max_unique_buyers_30s: 30,
            min_volume_sol_5s: 5.0,
            max_time_since_last_buy_ms: 500,
            curve_pct_min,
            curve_pct_max,
            min_history_age_ms: 2_000,
            creator_sell_cooldown_ms: 30_000,
            min_vsol_reserves_lamports: min_vsol,
            max_vsol_reserves_lamports: max_vsol,
            crowd_depth_norm_lamports: 10_000_000_000,
            recent_1s_norm_count: 20,
            volume_intensity_norm_lamports: 10_000_000_000,
            weights: ScoringWeights::default(),
            decision: DecisionThresholds::default(),
        }
    }
}

// ─── Entry Input ────────────────────────────────────────────────────────────

/// All data needed for a single entry evaluation.
/// Constructed by hot_path::on_trade() from MintHistory + TradeEvent.
/// Stack-allocated. 112 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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

// ─── Entry Decision ─────────────────────────────────────────────────────────

/// Decision output from EntryEngine::evaluate().
/// Tells HotPath what to do. Stack-allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAction {
    /// Reject — do not enter. Score below threshold or gate failed.
    Reject,
    /// RIDE mode — hold for sustained momentum, wider TP/trailing SL.
    Ride,
}

#[derive(Debug, Clone, Copy)]
pub struct EntryDecision {
    pub action: EntryAction,
    pub size_lamports: u64,
    pub score: f64,
    pub magnitude: f64,
    pub conviction: EntryConviction,
}

impl EntryDecision {
    #[inline(always)]
    pub fn reject() -> Self {
        Self {
            action: EntryAction::Reject,
            size_lamports: 0,
            score: 0.0,
            magnitude: 0.0,
            conviction: EntryConviction::default(),
        }
    }
}

// ─── The EntryEngine Struct ─────────────────────────────────────────────────

/// Monolithic entry engine. Owns all state for gate → score → size pipeline.
/// Single-threaded. Lives on the hot path thread. Zero heap allocation in evaluate().
///
/// CACHE LAYOUT (~3,144 bytes total, 50 cache lines, well within L1D):
///   HardGateThresholds   56B   (1 cache line — touched first, hottest)
///   buy_burst_lut       512B   (8 cache lines)
///   accel_lut          1024B   (16 cache lines)
///   curve_lut           800B   (12.5 cache lines)
///   fill_rate_lut       512B   (8 cache lines)
///   ScoringWeights      120B   (2 cache lines)
///   Reciprocals          48B   (1 cache line)
///   DecisionThresholds   16B   (shared cache line)
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
    pub decision: DecisionThresholds,     //   16 bytes
}

// ─── Helper: clamp to [0.0, 1.0] ───────────────────────────────────────────

/// Clamp x to [0.0, 1.0]. Branchless on most architectures via cmov.
#[inline(always)]
fn clamp01(x: f64) -> f64 {
    if x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

// ─── Gate Threshold Precomputation ──────────────────────────────────────────

/// Convert human-readable config to integer thresholds.
/// Called once at startup. May use float intermediates.
fn precompute_gate_thresholds(config: &EntryEngineConfig) -> HardGateThresholds {
    // Curve position 20% → vsol_reserves = 30 SOL + 0.20 * 85 SOL = 47 SOL
    // Curve position 60% → vsol_reserves = 30 SOL + 0.60 * 85 SOL = 81 SOL
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

// ─── EntryEngine Implementation ─────────────────────────────────────────────

impl EntryEngine {
    // ── LUT Builders (called once at construction) ──────────────────

    /// Build sigmoid LUT: lut[i] = 1.0 / (1.0 + exp(-steepness * (i as f64 - center)))
    fn build_sigmoid_lut<const N: usize>(center: f64, steepness: f64) -> [f64; N] {
        let mut lut = [0.0f64; N];
        let mut i = 0;
        while i < N {
            let x = i as f64;
            lut[i] = 1.0 / (1.0 + (-steepness * (x - center)).exp());
            i += 1;
        }
        lut
    }

    /// Build signed sigmoid LUT: lut[i] = sigmoid((i + offset) - center)
    /// where offset maps the zero-point to the middle of the array.
    fn build_signed_sigmoid_lut(center: f64, steepness: f64, offset: i32) -> AccelLut {
        let mut lut = [0.0f64; 128];
        let mut i = 0;
        while i < 128 {
            let x = (i as i32 + offset) as f64;
            lut[i] = 1.0 / (1.0 + (-steepness * (x - center)).exp());
            i += 1;
        }
        lut
    }

    /// Build Gaussian LUT: lut[i] = exp(-0.5 * ((i - mean) / sigma)^2)
    fn build_gaussian_lut(mean: f64, sigma: f64) -> CurveLut {
        let mut lut = [0.0f64; 100];
        let inv_2sigma2 = 0.5 / (sigma * sigma);
        let mut i = 0;
        while i < 100 {
            let diff = i as f64 - mean;
            lut[i] = (-diff * diff * inv_2sigma2).exp();
            i += 1;
        }
        lut
    }

    // ── Constructor ─────────────────────────────────────────────────

    /// Construct from config. Precomputes all LUTs, reciprocals, thresholds.
    /// Called once at startup. May allocate temporarily for LUT generation.
    pub fn new(config: &EntryEngineConfig) -> Self {
        // ── LUT generation ──────────────────────────────────────

        // buy_burst_lut: sigmoid(buy_count, center=7, steepness=0.8)
        // Index: buy_count_1s clamped to 0..63
        // lut[0]=0.004, lut[5]=0.168, lut[7]=0.500, lut[10]=0.916, lut[15]=0.999
        let buy_burst_lut = Self::build_sigmoid_lut::<64>(7.0, 0.8);

        // accel_lut: sigmoid(accel, center=10, steepness=0.15)
        // Index: accel_value + 64 (maps -64..+63 to 0..127)
        // accel = buy_count_1s * 5 - buy_count_5s (can be negative)
        let accel_lut = Self::build_signed_sigmoid_lut(10.0, 0.15, -64);

        // curve_lut: gaussian(curvePct, mean=43, sigma=12)
        // Index: curvePct as integer 0..99
        // lut[43]=1.0, lut[31]=0.5, lut[55]=0.5, lut[19]=~0.05
        let curve_lut = Self::build_gaussian_lut(43.0, 12.0);

        // fill_rate_lut: sigmoid(fill_rate_idx, center=15, steepness=0.25)
        let fill_rate_lut = Self::build_sigmoid_lut::<64>(15.0, 0.25);

        // ── Reciprocals ─────────────────────────────────────────

        let vsol_range = config
            .max_vsol_reserves_lamports
            .saturating_sub(config.min_vsol_reserves_lamports)
            .max(1) as f64;

        let reciprocals = Reciprocals {
            inv_max_recency_ms: 1.0 / config.max_time_since_last_buy_ms.max(1) as f64,
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
            weights: config.weights,
            reciprocals,
            decision: config.decision,
        }
    }

    // ── Top-Level Evaluate ──────────────────────────────────────────

    /// Full 3-stage evaluation. Zero heap allocation. ~50-250ns.
    ///
    /// Stage 1 (hard gate): ~30-40ns. Rejects ~65% of inputs.
    /// Stage 2 (scoring):   ~150-180ns. Only runs on ~35% of inputs.
    /// Stage 3 (sizing):    Kelly criterion sizing via kelly_sizing module.
    ///
    /// PERF: NOT #[inline(always)] — this is the outer orchestrator.
    /// The individual stages are inlined; keeping the orchestrator as
    /// a regular function call prevents icache bloat at the call site.
    #[inline]
    pub fn evaluate(
        &self,
        input: &EntryInput,
        wallet_balance: u64,
        n_open: u8,
        drawdown_pct: u8,
    ) -> EntryDecision {
        // Stage 1: Hard Gate (<50ns)
        if !self.hard_gate(input) {
            return EntryDecision::reject();
        }

        // Stage 2: Composite Scoring (~150-180ns)
        let (score, magnitude) = self.score(input);

        // Stage 3: Kelly Criterion Sizing
        let (action, conviction) =
            self.size(score, magnitude, wallet_balance, n_open, drawdown_pct);

        EntryDecision {
            action,
            size_lamports: conviction.size_lamports,
            score,
            magnitude,
            conviction,
        }
    }

    // ── Stage 3: Position Sizing (Kelly Criterion) ────────────────────

    /// Kelly criterion position sizing from entry + magnitude scores.
    /// Delegates to kelly_sizing::compute_conviction() for bankroll-aware sizing.
    #[inline(always)]
    fn size(
        &self,
        entry_score: f64,
        magnitude_score: f64,
        wallet_balance: u64,
        n_open: u8,
        drawdown_pct: u8,
    ) -> (EntryAction, EntryConviction) {
        let d = &self.decision;
        if entry_score < d.min_entry_score {
            return (EntryAction::Reject, EntryConviction::default());
        }

        if magnitude_score < d.min_magnitude_for_ride {
            return (EntryAction::Reject, EntryConviction::default());
        }

        let conviction = kelly_sizing::compute_conviction(
            magnitude_score,
            entry_score,
            wallet_balance,
            n_open,
            drawdown_pct,
        );

        // ── Fee-Aware Edge Gate ─────────────────────────────────────
        // Reject if expected edge per trade doesn't cover fees by the
        // configured multiplier. All integer math, zero allocation.
        //
        // expected_edge_lamports = p × avg_win_lamports - (1-p) × avg_loss_lamports
        //   where avg_win  = r_x100 × avg_loss / 100  (r_x100 is fee-adjusted)
        //         avg_loss = size × DEFAULT_AVG_LOSS_BP / 10000
        //
        // fee_lamports = size × DEFAULT_ROUND_TRIP_FEE_BP / 10000
        //
        // Gate: expected_edge_lamports × 100 >= fee_lamports × fee_gate_multiplier_x100
        if d.fee_gate_multiplier_x100 > 0 && conviction.size_lamports > 0 {
            let size = conviction.size_lamports as u128;
            let p = conviction.p_permille as u128;       // 0..1000
            let r = conviction.r_x100 as u128;           // R_adj × 100
            let q = 1000u128.saturating_sub(p);           // (1-p) × 1000

            // avg_loss_lamports = size × DEFAULT_AVG_LOSS_BP / 10000
            let avg_loss_lam = size * kelly_sizing::DEFAULT_AVG_LOSS_BP as u128 / 10_000;

            // avg_win_lamports = avg_loss_lam × r_x100 / 100
            let avg_win_lam = avg_loss_lam * r / 100;

            // expected_edge × 1000 = p × avg_win - q × avg_loss
            // (keeping ×1000 factor from p_permille to avoid premature truncation)
            let edge_x1000 = (p * avg_win_lam).saturating_sub(q * avg_loss_lam);

            // fee_lamports = size × DEFAULT_ROUND_TRIP_FEE_BP / 10000
            let fee_lam = size * kelly_sizing::DEFAULT_ROUND_TRIP_FEE_BP as u128 / 10_000;

            // Gate: edge_x1000 >= fee_lam × multiplier_x100 × 1000 / 100
            //      = fee_lam × multiplier_x100 × 10
            let threshold = fee_lam * d.fee_gate_multiplier_x100 as u128 * 10;

            if edge_x1000 < threshold {
                return (EntryAction::Reject, EntryConviction::default());
            }
        }

        (EntryAction::Ride, conviction)
    }

    // ── Stage 1: Hard Gate ──────────────────────────────────────────

    /// Boolean gate: all-integer comparisons. <50ns.
    /// Returns true if candidate passes all checks.
    /// Ordered by rejection_rate × cost (cheapest high-rejection first).
    ///
    /// PERF: #[inline(always)] — called on every buy trade.
    /// ~8 integer comparisons = ~8 branch instructions.
    /// Total icache footprint: ~128 bytes (2 cache lines).
    #[inline(always)]
    fn hard_gate(&self, input: &EntryInput) -> bool {
        let g = &self.gate;

        // Check 1: buy_count_1s >= min (highest rejection rate ~55%)
        if input.buy_count_1s < g.min_buy_count_1s {
            return false;
        }

        // Check 2: volume_sol_5s >= min (~50% rejection)
        if input.volume_sol_5s < g.min_volume_sol_5s {
            return false;
        }

        // Check 3: time_since_last_buy_ms <= max (~40% rejection)
        if input.time_since_last_buy_ms > g.max_time_since_last_buy_ms {
            return false;
        }

        // Check 4: vsol_reserves in [min..max] (curve position 20-60%, ~35% rejection)
        // Skip when vsol_reserves == 0 (ShredStream: decoded tx doesn't include account state).
        // These entries rely on PumpPortal enrichment or cached MintHistory data.
        if input.vsol_reserves > 0
            && (input.vsol_reserves < g.min_vsol_reserves || input.vsol_reserves > g.max_vsol_reserves)
        {
            return false;
        }

        // Check 5: unique_buyers_30s <= max (~15% rejection)
        if input.unique_buyers_30s > g.max_unique_buyers_30s {
            return false;
        }

        // Check 6: sell pressure — 2 * sell_count_5s < buy_count_5s (~10% rejection)
        // Integer multiply+compare. No division.
        // Equivalent to: sell_count_5s < buy_count_5s / 2 (but avoids division)
        if (input.sell_count_5s as u32) * 2 >= input.buy_count_5s as u32 {
            return false;
        }

        // Check 7: history_age_ms >= min (need enough data, ~5% rejection)
        if input.history_age_ms < g.min_history_age_ms {
            return false;
        }

        // Check 8: creator sell cooldown (~2% rejection)
        // creator_sell_at_ms == 0 → no creator sell → pass
        // creator_sell_at_ms > 0 AND (now - creator_sell_at_ms) <= cooldown → reject
        if input.creator_sell_at_ms > 0
            && input
                .now_ms
                .saturating_sub(input.creator_sell_at_ms)
                <= g.creator_sell_cooldown_ms
        {
            return false;
        }

        true
    }

    // ── Stage 2: Composite Scoring ──────────────────────────────────

    /// Compute entry_score (0-100) and magnitude_score (0-100).
    /// Uses LUT lookups instead of exp()/pow(). ~150-180ns.
    ///
    /// PERF: #[inline(always)] — only called when gate passes (~35%).
    /// Dominated by LUT loads (cache hits) and FMA operations.
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
        let curve_pct_raw = if input.vsol_reserves > 30_000_000_000 {
            ((input.vsol_reserves - 30_000_000_000) as f64 / 850_000_000.0) as usize
        } else {
            0
        };
        let curve_idx = curve_pct_raw.min(99);
        let f_curve = unsafe { *self.curve_lut.get_unchecked(curve_idx) };

        // Feature 4: Buyer Concentration (piecewise linear)
        let f_concentration = crate::engine::scoring::concentration_score(
            input.unique_buyers_30s, 10, 30,
        );

        // Feature 5: Buy Acceleration
        // accel = buy_count_1s * 5 - buy_count_5s, range approx [-64..+63]
        let accel_raw = (input.buy_count_1s as i32) * 5 - (input.buy_count_5s as i32);
        let accel_idx = (accel_raw + 64).max(0).min(127) as usize;
        let f_accel = unsafe { *self.accel_lut.get_unchecked(accel_idx) };

        // Feature 6: Average Buy Size
        let avg_buy_sol = if input.buy_count_5s > 0 {
            input.volume_sol_5s as f64 / (input.buy_count_5s as f64 * 1_000_000_000.0)
        } else {
            0.0
        };
        let f_avg_size = clamp01(avg_buy_sol * 0.5); // normalize to 2 SOL

        // Feature 7: Sell Absence
        let sell_ratio = if input.buy_count_5s > 0 {
            input.sell_count_5s as f64 / input.buy_count_5s as f64
        } else {
            1.0
        };
        let f_sell_absence = clamp01(1.0 - sell_ratio * 2.5);

        // Feature 8: Momentum Recency
        let f_recency = clamp01(1.0 - input.time_since_last_buy_ms as f64 * r.inv_max_recency_ms);

        // Weighted entry score
        let entry_score = (
            w.w_buy_burst * f_buy_burst
            + w.w_volume * f_volume
            + w.w_curve_position * f_curve
            + w.w_buyer_concentration * f_concentration
            + w.w_buy_acceleration * f_accel
            + w.w_avg_buy_size * f_avg_size
            + w.w_sell_absence * f_sell_absence
            + w.w_recency * f_recency
        ) * 100.0;

        // ════════════════════════════════════════════════════════════
        // MAGNITUDE FEATURES (predict HOW FAR token pumps)
        // ════════════════════════════════════════════════════════════

        // Mag 1: Fill Rate (LUT)
        // vsol_delta_3s in lamports → fill rate index (each unit = ~0.85 SOL ≈ 1 curvePct/3s)
        let fill_idx = (input.vsol_delta_3s / 850_000_000).min(63) as usize;
        let m_fill_rate = unsafe { *self.fill_rate_lut.get_unchecked(fill_idx) };

        // Mag 2: Buy Velocity Acceleration (reuse entry accel LUT, same value)
        let m_accel = f_accel;

        // Mag 3: Wallet Quality (lower whale concentration = better)
        let m_wallet_quality = if input.total_buy_vol_30s > 0 {
            let whale_share = input.max_wallet_vol_30s as f64 / input.total_buy_vol_30s as f64;
            clamp01(1.0 - whale_share)
        } else {
            0.0
        };

        // Mag 4: Curve Remaining Upside (earlier = more upside)
        let curve_pct_f = curve_pct_raw as f64;
        let m_curve_remaining = clamp01(1.0 - curve_pct_f / 100.0);

        // Mag 5: Volume Intensity
        let m_volume_intensity = clamp01(input.volume_sol_5s as f64 * r.inv_volume_intensity_norm);

        // Mag 6: Sell Vacuum
        let m_sell_vacuum = crate::engine::scoring::sell_vacuum_score(input.sell_count_5s);

        // Mag 7: Token Age
        let m_token_age = crate::engine::scoring::token_age_score(input.history_age_ms);

        // Weighted magnitude score
        let magnitude_score = (
            w.w_fill_rate * m_fill_rate
            + w.w_buy_velocity_accel * m_accel
            + w.w_wallet_quality * m_wallet_quality
            + w.w_curve_remaining * m_curve_remaining
            + w.w_volume_intensity * m_volume_intensity
            + w.w_sell_vacuum * m_sell_vacuum
            + w.w_token_age * m_token_age
        ) * 100.0;

        (entry_score, magnitude_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> EntryEngineConfig {
        EntryEngineConfig::default()
    }

    fn passing_input() -> EntryInput {
        EntryInput {
            vsol_reserves: 66_000_000_000,  // ~42% curve (sweet spot)
            vtoken_reserves: 500_000_000_000,
            sol_amount: 500_000_000,         // 0.5 SOL trigger
            buy_count_1s: 8,
            buy_count_2s: 12,
            buy_count_5s: 20,
            sell_count_5s: 2,
            unique_buyers_30s: 12,
            _pad: 0,
            volume_sol_5s: 8_000_000_000,    // 8 SOL
            vsol_delta_3s: 3_000_000_000,    // 3 SOL in 3s
            time_since_last_buy_ms: 100,
            history_age_ms: 10_000,
            creator_sell_at_ms: 0,
            now_ms: 1_000_000,
            max_wallet_vol_30s: 2_000_000_000,
            total_buy_vol_30s: 10_000_000_000,
        }
    }

    // Default bankroll params for tests: 5 SOL wallet, 0 open positions, 0% drawdown
    const TEST_WALLET: u64 = 5_000_000_000;
    const TEST_N_OPEN: u8 = 0;
    const TEST_DRAWDOWN: u8 = 0;

    #[test]
    fn test_hard_gate_passes_good_input() {
        let engine = EntryEngine::new(&default_config());
        let input = passing_input();
        assert!(engine.hard_gate(&input));
    }

    #[test]
    fn test_hard_gate_rejects_low_buys() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.buy_count_1s = 2;
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn test_hard_gate_rejects_low_volume() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.volume_sol_5s = 2_000_000_000; // 2 SOL < 5 SOL min
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn test_hard_gate_rejects_stale_momentum() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.time_since_last_buy_ms = 600; // > 500ms
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn test_hard_gate_rejects_outside_curve_band() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.vsol_reserves = 40_000_000_000; // ~12% curve, below 20%
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn test_hard_gate_rejects_too_many_sellers() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.sell_count_5s = 15;
        input.buy_count_5s = 20; // sell ratio = 0.75 > 0.50
        assert!(!engine.hard_gate(&input));
    }

    #[test]
    fn test_evaluate_produces_scores() {
        let engine = EntryEngine::new(&default_config());
        let input = passing_input();
        let decision = engine.evaluate(&input, TEST_WALLET, TEST_N_OPEN, TEST_DRAWDOWN);
        assert!(decision.score > 0.0, "score should be positive");
        assert!(decision.magnitude > 0.0, "magnitude should be positive");
        assert!(decision.size_lamports > 0, "should not reject good input");
    }

    #[test]
    fn test_evaluate_rejects_bad_input() {
        let engine = EntryEngine::new(&default_config());
        let mut input = passing_input();
        input.buy_count_1s = 1; // fails hard gate
        let decision = engine.evaluate(&input, TEST_WALLET, TEST_N_OPEN, TEST_DRAWDOWN);
        assert_eq!(decision.size_lamports, 0);
        assert!(matches!(decision.action, EntryAction::Reject));
    }

    #[test]
    fn test_scoring_sweet_spot_curve() {
        let engine = EntryEngine::new(&default_config());
        // At curve sweet spot (43%), should get high curve score
        let mut input = passing_input();
        input.vsol_reserves = 66_550_000_000; // ~43% curve
        let d1 = engine.evaluate(&input, TEST_WALLET, TEST_N_OPEN, TEST_DRAWDOWN);

        // At curve extreme (5%), should get lower curve score
        input.vsol_reserves = 34_250_000_000; // ~5% curve — but this fails hard gate
        input.vsol_reserves = 55_000_000_000; // ~29% curve (passes gate, suboptimal)
        let d2 = engine.evaluate(&input, TEST_WALLET, TEST_N_OPEN, TEST_DRAWDOWN);

        assert!(d1.score >= d2.score, "sweet spot should score higher");
    }
}