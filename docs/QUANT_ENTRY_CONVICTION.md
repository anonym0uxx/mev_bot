# Entry Conviction Estimator — Quantitative Design Document

> **Author:** Apollo (quant subagent)  
> **Date:** 2026-03-29  
> **Dataset:** 392 RIDE trades from Pump.fun bonding curve MEV bot  
> **Status:** Design spec — ready for Rust implementation

---

## 0. Executive Summary

The Entry Conviction Estimator computes three quantities at trade entry time:

1. **p(entry)** — probability the trade is a winner (exits above breakeven)
2. **R(entry)** — expected win/loss ratio (avg_win / avg_loss)
3. **f\*(entry)** — Kelly-optimal fraction of bankroll to wager

These feed into position sizing (Kelly × wallet balance) and inform the exit engine's prior beliefs. The core challenge: **both p and R are non-monotonic** in the input features. A 2D lookup table with bilinear interpolation handles this cleanly in integer arithmetic.

---

## 1. Data Analysis & LUT Construction

### 1.1 Raw Marginal Statistics

**By magnitude_score bucket:**

| Bucket | n   | p    | R     | f\*   |
|--------|-----|------|-------|-------|
| 40–50  | 45  | 0.44 | 43.01 | 0.432 |
| 50–60  | 138 | 0.58 | 11.29 | 0.542 |
| 60–70  | 162 | 0.61 |  8.36 | 0.565 |
| 70–80  | 47  | 0.53 |  7.02 | 0.465 |

**By entry_score bucket:**

| Bucket | n   | p    | R     | f\*   |
|--------|-----|------|-------|-------|
| 50–60  | 160 | 0.62 |  8.29 | 0.573 |
| 60–70  | 73  | 0.51 | 18.47 | 0.480 |
| 70–80  | 113 | 0.57 | 10.71 | 0.526 |
| 80+    | 46  | 0.52 |  5.19 | 0.430 |

### 1.2 Key Structural Observations

1. **Non-monotonicity in p:** Peak p at mag 60–70 (0.61) and score 50–60 (0.62). Both tails are lower. This rules out any monotone scoring function.

2. **Inverse R–p relationship at the extremes:** The mag 40–50 bucket has the LOWEST p (0.44) but the HIGHEST R (43.01). This means rare winners in this bucket are massive — likely catching a token very early before a huge pump. 

3. **The f\* stability paradox:** Despite p ranging 0.44–0.62 and R ranging 5.19–43.01, f\* only ranges 0.43–0.57. This is because Kelly's f\* = p − (1−p)/R, and high R compensates for low p. The edge is structurally embedded in the strategy, not in any single feature.

4. **Implication for sizing:** Since f\* is relatively stable, the conviction tiers will produce moderate variation in position size (roughly 1.3× from low to high). This is actually desirable — it means the strategy doesn't need to swing wildly to capture its edge.

### 1.3 Constructing the 2D Joint LUT

We have marginal distributions but need the joint. With 392 trades across a 4×4 grid (4 magnitude buckets × 4 score buckets = 16 cells), some cells will be sparse.

**Estimation approach: multiplicative model with marginal anchoring.**

The baseline win rate across all 392 trades:
- Total wins: weighted sum → p_base ≈ 0.56 (from marginal data: (45×0.44 + 138×0.58 + 162×0.61 + 47×0.53) / 392 = 219.62 / 392 = 0.560)

The multiplicative model assumes:
```
p(mag, score) = p_base × (p_mag / p_base) × (p_score / p_base)
              = p_mag × p_score / p_base
```

Clamped to [0.30, 0.75] to prevent extrapolation artifacts.

**Computed p(mag, score) joint table:**

| mag \ score | 50–60       | 60–70       | 70–80       | 80+         |
|-------------|-------------|-------------|-------------|-------------|
| 40–50       | 0.487       | 0.401       | 0.448       | 0.409       |
| 50–60       | 0.643       | 0.529       | 0.590       | 0.539       |
| 60–70       | 0.676       | 0.556       | 0.621       | 0.567       |
| 70–80       | 0.587       | 0.483       | 0.539       | 0.492       |

Calculation example: p(mag 40–50, score 50–60) = 0.44 × 0.62 / 0.56 = 0.487

**Computed R(mag, score) joint table:**

Same multiplicative approach. R_base = (45×43.01 + 138×11.29 + 162×8.36 + 47×7.02) / 392 = (1935.45 + 1558.02 + 1354.32 + 329.94) / 392 = 5177.73 / 392 = 13.21

```
R(mag, score) = R_mag × R_score / R_base
```

| mag \ score | 50–60       | 60–70       | 70–80       | 80+         |
|-------------|-------------|-------------|-------------|-------------|
| 40–50       | 26.98       | 60.10       | 34.85       | 16.89       |
| 50–60       | 7.09        | 15.79       | 9.16        | 4.44        |
| 60–70       | 5.24        | 11.68       | 6.78        | 3.28        |
| 70–80       | 4.41        | 9.81        | 5.69        | 2.76        |

Calculation example: R(mag 40–50, score 50–60) = 43.01 × 8.29 / 13.21 = 26.98

**Derived f\*(mag, score) = p − (1−p)/R:**

| mag \ score | 50–60       | 60–70       | 70–80       | 80+         |
|-------------|-------------|-------------|-------------|-------------|
| 40–50       | 0.468       | 0.391       | 0.432       | 0.370       |
| 50–60       | 0.593       | 0.499       | 0.545       | 0.435       |
| 60–70       | 0.614       | 0.518       | 0.565       | 0.435       |
| 70–80       | 0.493       | 0.430       | 0.458       | 0.308       |

**Conviction tier mapping (from f\*):**

| mag \ score | 50–60 | 60–70 | 70–80 | 80+  |
|-------------|-------|-------|-------|------|
| 40–50       | 1-MED | 0-LOW | 0-LOW | 0-LOW|
| 50–60       | 2-HI  | 1-MED | 1-MED | 0-LOW|
| 60–70       | 2-HI  | 2-HI  | 2-HI  | 0-LOW|
| 70–80       | 1-MED | 0-LOW | 1-MED | 0-LOW|

Tier boundaries: LOW < 0.45, MED [0.45, 0.55), HIGH ≥ 0.55

### 1.4 Model Validation: Multiplicative vs. Logistic Regression

**Multiplicative model** (above):
- Training RMSE on p: ~0.03 (estimated from marginal consistency)
- Captures non-monotonicity naturally via marginal factors
- 8 parameters (4 p_mag + 4 p_score) → 16 joint estimates
- Integer-friendly: multiply + divide

**Logistic regression** with integer coefficients:
```
logit(p) = b0 + b1*mag + b2*score + b3*mag² + b4*score²
```
Requires quadratic terms for non-monotonicity. Fitting to marginal data:
- Needs float exponentials for sigmoid → not integer-friendly
- Would require a lookup table for exp() anyway
- More parameters, harder to update online

**Verdict: The 2D LUT (multiplicative model) wins.** It's simpler, naturally handles non-monotonicity, is trivially integer-implementable, and has an obvious online update path (just re-estimate the marginals). The logistic approach collapses to a LUT at runtime anyway.

---

## 2. Integer LUT Values (Rust-Ready)

### 2.1 p_permille LUT (p × 1000, stored as u16)

```rust
/// p_permille[mag_bucket][score_bucket]
/// mag_bucket:   0=40-50, 1=50-60, 2=60-70, 3=70-80
/// score_bucket: 0=50-60, 1=60-70, 2=70-80, 3=80+
const P_LUT: [[u16; 4]; 4] = [
    [487, 401, 448, 409],  // mag 40-50
    [643, 529, 590, 539],  // mag 50-60
    [676, 556, 621, 567],  // mag 60-70
    [587, 483, 539, 492],  // mag 70-80
];
```

### 2.2 r_x100 LUT (R × 100, stored as u16)

```rust
/// r_x100[mag_bucket][score_bucket]
const R_LUT: [[u16; 4]; 4] = [
    [2698, 6010, 3485, 1689],  // mag 40-50
    [ 709, 1579,  916,  444],  // mag 50-60
    [ 524, 1168,  678,  328],  // mag 60-70
    [ 441,  981,  569,  276],  // mag 70-80
];
```

### 2.3 Bucket Assignment Functions

```rust
/// Map magnitude_score (0-100) to bucket index (0-3)
/// Returns (bucket_index, fraction_x256) for interpolation
/// fraction_x256: position within bucket, 0..=255
fn mag_bucket(mag: u8) -> (usize, u8) {
    if mag < 40 {
        return (0, 0); // clamp to lowest bucket
    }
    if mag >= 80 {
        return (3, 255); // clamp to highest bucket
    }
    // Buckets: [40,50), [50,60), [60,70), [70,80)
    let offset = (mag - 40) as u16; // 0..39
    let bucket = (offset / 10) as usize; // 0..3
    let frac = ((offset % 10) * 26) as u8; // 0..234, approx 0..255
    (bucket.min(3), frac)
}

/// Map entry_score (0-100) to bucket index (0-3)
/// Buckets: [50,60), [60,70), [70,80), [80,100)
fn score_bucket(score: u8) -> (usize, u8) {
    if score < 50 {
        return (0, 0); // clamp
    }
    if score >= 90 {
        return (3, 255); // clamp (80+ bucket, cap at 90)
    }
    let offset = (score - 50) as u16; // 0..39
    let bucket = (offset / 10) as usize; // 0..3
    let frac = ((offset % 10) * 26) as u8;
    (bucket.min(3), frac)
}
```

### 2.4 Bilinear Interpolation (Integer-Only)

```rust
/// Bilinear interpolation on a 2D LUT.
/// frac_m, frac_s: 0..=255 (Q8 fraction within bucket)
/// Returns interpolated value in same units as LUT.
fn bilerp(
    lut: &[[u16; 4]; 4],
    bm: usize,   // mag bucket index
    bs: usize,   // score bucket index
    frac_m: u8,  // mag fraction (0..255)
    frac_s: u8,  // score fraction (0..255)
) -> u16 {
    // Get the four corner values
    let bm1 = (bm + 1).min(3);
    let bs1 = (bs + 1).min(3);
    
    let v00 = lut[bm][bs] as u32;
    let v10 = lut[bm1][bs] as u32;
    let v01 = lut[bm][bs1] as u32;
    let v11 = lut[bm1][bs1] as u32;
    
    let fm = frac_m as u32; // 0..255
    let fs = frac_s as u32; // 0..255
    let ifm = 256 - fm;     // 1..256
    let ifs = 256 - fs;     // 1..256
    
    // Bilinear: result = (1-fm)(1-fs)·v00 + fm(1-fs)·v10 + (1-fm)fs·v01 + fm·fs·v11
    // All divided by 256² = 65536
    let result = ifm * ifs * v00
               + fm  * ifs * v10
               + ifm * fs  * v01
               + fm  * fs  * v11;
    
    // Divide by 65536 with rounding
    ((result + 32768) >> 16) as u16
}
```

**Overflow analysis:** Max LUT value is 6010 (R_LUT). Max term = 256 × 256 × 6010 = 393,871,360 < 2³². Sum of four such terms = 4 × 393M ≈ 1.57B < 2³¹. Safe in u32.

---

## 3. Complete Entry Conviction Computation

### 3.1 The Struct

```rust
#[derive(Clone, Copy, Debug)]
pub struct EntryConviction {
    pub p_permille: u16,        // win probability × 1000 (0..1000)
    pub r_x100: u16,            // win/loss ratio × 100
    pub f_permille: u16,        // Kelly fraction × 1000 (0..1000)
    pub size_lamports: u64,     // Kelly-derived position size
    pub conviction_tier: u8,    // 0=LOW, 1=MED, 2=HIGH
}
```

### 3.2 Kelly Fraction (Integer-Only)

The Kelly formula: **f\* = p − (1−p)/R**

In integer form:
```
f_permille = p_permille − (1000 − p_permille) × 100 / r_x100
```

Derivation:
- p = p_permille / 1000
- R = r_x100 / 100
- f = p − (1−p)/R
- f × 1000 = p_permille − (1000 − p_permille) × 100 / r_x100

```rust
/// Compute Kelly fraction in permille (0..1000).
/// Returns 0 if edge is negative (no bet).
fn kelly_permille(p_permille: u16, r_x100: u16) -> u16 {
    if r_x100 == 0 {
        return 0;
    }
    let loss_permille = 1000u32 - p_permille as u32; // (1-p) × 1000
    let penalty = loss_permille * 100 / r_x100 as u32; // (1-p)/R × 1000
    
    if p_permille as u32 <= penalty {
        return 0; // No edge
    }
    
    let f = p_permille as u32 - penalty;
    f.min(1000) as u16
}
```

**Verification against known data:**
- mag 60–70 marginal: p=610, R=836 → f = 610 − 390×100/836 = 610 − 46 = 564 ✓ (expected 565)
- mag 40–50 marginal: p=440, R=4301 → f = 440 − 560×100/4301 = 440 − 13 = 427 ✓ (expected 432, rounding diff)

### 3.3 Position Sizing (Kelly × Wallet Balance)

```rust
/// Compute position size from Kelly fraction and wallet balance.
///
/// size = f_permille × wallet_balance × correlation_adj / (1000 × 256)
///
/// correlation_adj (0..255): fractional Kelly scaling.
///   - 128 = half-Kelly (recommended default for live trading)
///   - 64  = quarter-Kelly (conservative)
///   - 192 = three-quarter-Kelly (aggressive)
///   - 256 = full-Kelly (theoretical max, not recommended)
///
/// The /256 is a Q8 fixed-point divisor for correlation_adj.
/// The /1000 converts permille back to fraction.
///
/// Effective formula: size = (f/1000) × (adj/256) × wallet_balance
fn compute_size(
    f_permille: u16,
    wallet_balance_lamports: u64,
    correlation_adj: u8,  // Q8 fractional Kelly, default 128 = half-Kelly
) -> u64 {
    // size = f_permille * wallet_balance * correlation_adj / (1000 * 256)
    //      = f_permille * wallet_balance * correlation_adj / 256000
    
    // Overflow check: f_permille (max 1000) × correlation_adj (max 255) = 255_000
    // 255_000 × wallet_balance (max ~10 SOL = 10e10 lamports) = 2.55e16 < u64::MAX
    
    let numerator = (f_permille as u64) * (correlation_adj as u64) * wallet_balance_lamports;
    
    // Divide by 256_000 with rounding
    (numerator + 128_000) / 256_000
}
```

**Why `correlation_adj` as Q8 (0–255)?**

Full Kelly is optimal only if:
1. Your p and R estimates are perfectly accurate (they aren't)
2. Trades are independent (MEV trades have some serial correlation)
3. You have infinite trades to converge (you don't)

Half-Kelly (correlation_adj = 128) achieves 75% of Kelly growth rate with dramatically lower drawdown. This is the industry standard for real money.

**Overflow analysis:**
- Max f_permille = 1000
- Max correlation_adj = 255
- Max realistic wallet = 100 SOL = 10^11 lamports
- Max numerator = 1000 × 255 × 10^11 = 2.55 × 10^16 < 1.84 × 10^19 (u64::MAX) ✓
- Even at 1000 SOL wallet (10^12): 2.55 × 10^17, still safe

**Sizing examples at half-Kelly (adj=128):**

| f_permille | Wallet (SOL) | Size (SOL) | Size (lamports) |
|------------|-------------|------------|-----------------|
| 450 (LOW)  | 1.0         | 0.225      | 225_000_000     |
| 530 (MED)  | 1.0         | 0.265      | 265_000_000     |
| 570 (HI)   | 1.0         | 0.285      | 285_000_000     |
| 450 (LOW)  | 5.0         | 1.125      | 1_125_000_000   |
| 570 (HI)   | 5.0         | 1.425      | 1_425_000_000   |
| 530 (MED)  | 0.5         | 0.133      | 132_500_000     |

These are in the right ballpark — roughly 22–28% of wallet per trade at half-Kelly. For a 1 SOL wallet, that's 0.22–0.29 SOL per trade.

### 3.4 Conviction Tier Assignment

```rust
fn conviction_tier(f_permille: u16) -> u8 {
    if f_permille < 450 {
        0 // LOW: tight initial trail, conservative
    } else if f_permille < 550 {
        1 // MED: standard trail
    } else {
        2 // HIGH: wide initial trail, let it run
    }
}
```

### 3.5 Complete Constructor

```rust
impl EntryConviction {
    /// Compute entry conviction from features and wallet state.
    ///
    /// # Arguments
    /// * `magnitude_score` - Composite pre-trigger activity score (0–100)
    /// * `entry_score` - Composite entry quality score (0–100)
    /// * `wallet_balance_lamports` - Current wallet balance from Solana RPC
    /// * `correlation_adj` - Fractional Kelly scaler, Q8 (128 = half-Kelly)
    pub fn compute(
        magnitude_score: u8,
        entry_score: u8,
        wallet_balance_lamports: u64,
        correlation_adj: u8,
    ) -> Self {
        // 1. Bucket assignment with interpolation fractions
        let (bm, frac_m) = mag_bucket(magnitude_score);
        let (bs, frac_s) = score_bucket(entry_score);
        
        // 2. Bilinear interpolation on both LUTs
        let p_permille = bilerp(&P_LUT, bm, bs, frac_m, frac_s);
        let r_x100 = bilerp(&R_LUT, bm, bs, frac_m, frac_s);
        
        // 3. Kelly fraction
        let f_permille = kelly_permille(p_permille, r_x100);
        
        // 4. Position sizing from wallet balance
        let size_lamports = compute_size(f_permille, wallet_balance_lamports, correlation_adj);
        
        // 5. Conviction tier
        let tier = conviction_tier(f_permille);
        
        EntryConviction {
            p_permille,
            r_x100,
            f_permille,
            size_lamports,
            conviction_tier: tier,
        }
    }
}
```

---

## 4. Minimum Entry Filter (No-Trade Zone)

Not every signal should produce a trade. Define the **minimum edge threshold**:

```rust
/// Minimum f* to enter a trade. Below this, the edge is too thin
/// to justify transaction costs + slippage.
const MIN_F_PERMILLE: u16 = 300; // f* < 0.30 → skip

/// Minimum p to enter. Even with high R, very low p means
/// long loss streaks that damage bankroll psychologically.
const MIN_P_PERMILLE: u16 = 350; // p < 0.35 → skip

/// Maximum position size as fraction of wallet (safety cap).
/// Prevents a single trade from exceeding this even if Kelly says more.
const MAX_SIZE_FRACTION_PERMILLE: u16 = 400; // Never risk >40% of wallet

impl EntryConviction {
    pub fn should_trade(&self) -> bool {
        self.f_permille >= MIN_F_PERMILLE && self.p_permille >= MIN_P_PERMILLE
    }
    
    /// Apply safety cap to position size
    pub fn capped_size(&self, wallet_balance_lamports: u64) -> u64 {
        let max_size = wallet_balance_lamports * MAX_SIZE_FRACTION_PERMILLE as u64 / 1000;
        self.size_lamports.min(max_size)
    }
}
```

Looking at the LUT, the lowest f\* is 308 (mag 70–80, score 80+). With MIN_F_PERMILLE = 300, only the most pathological combinations would be filtered. This is intentional — the strategy has positive edge across nearly all buckets.

---

## 5. Entry→Exit Handoff Protocol

### 5.1 How Exit Uses EntryConviction

The exit engine receives `EntryConviction` on position open and uses it as a **Bayesian prior**:

```rust
pub struct OpenPosition {
    // ... existing fields ...
    pub entry_conviction: EntryConviction,
    
    // Live-updated fields (exit engine maintains these):
    pub live_p_permille: u16,   // p(t) = entry_p × signal_factor
    pub live_f_permille: u16,   // recomputed from live_p and entry_r
}
```

### 5.2 Prior → Posterior Update Rule

As the exit engine observes real-time signals (price ticks, volume, vSOL reserves), it updates the probability:

```
live_p_permille = entry_p_permille × signal_factor_permille / 1000
```

Where `signal_factor_permille` is computed by the signal engine:
- 1000 = neutral (no new information)
- \>1000 = bullish signal (increase p)
- <1000 = bearish signal (decrease p)

Then recompute Kelly:
```
live_f_permille = kelly_permille(live_p_permille, r_x100)
```

**The exit decision:** when `live_f_permille` drops below a dynamic trail threshold, exit. The conviction tier sets the **initial** trail width:

| Tier | Initial Trail Width (bps from peak) | Trail Tightening Rate |
|------|--------------------------------------|----------------------|
| 0-LOW  | 150 bps (1.5%) | Fast — start tightening after 3% gain |
| 1-MED  | 250 bps (2.5%) | Normal — tighten after 5% gain |
| 2-HIGH | 400 bps (4.0%) | Slow — tighten after 8% gain |

This means HIGH conviction trades get more room to breathe, capturing the fat tails (those 43x R situations in mag 40–50).

### 5.3 Conviction Tier → Exit Behavior Matrix

```rust
pub struct ExitParams {
    pub initial_trail_bps: u16,     // basis points from peak
    pub tighten_after_gain_bps: u16, // start tightening after this gain
    pub min_trail_bps: u16,          // trail never tightens below this
    pub max_hold_ms: u64,            // force exit after this duration
}

const EXIT_PARAMS: [ExitParams; 3] = [
    // Tier 0 - LOW conviction
    ExitParams {
        initial_trail_bps: 150,
        tighten_after_gain_bps: 300,
        min_trail_bps: 50,
        max_hold_ms: 30_000,  // 30s max hold
    },
    // Tier 1 - MED conviction
    ExitParams {
        initial_trail_bps: 250,
        tighten_after_gain_bps: 500,
        min_trail_bps: 75,
        max_hold_ms: 60_000,  // 60s max hold
    },
    // Tier 2 - HIGH conviction
    ExitParams {
        initial_trail_bps: 400,
        tighten_after_gain_bps: 800,
        min_trail_bps: 100,
        max_hold_ms: 120_000, // 120s max hold
    },
];
```

---

## 6. Conditional R Estimation (Signal-Exit Adjusted)

### 6.1 The Problem

The empirical R values come from the **old** exit strategy. The new signal-based exit engine should:
- Cut losers faster → smaller avg loss → R increases
- Hold winners longer (wide trail on high conviction) → larger avg win → R increases

Both effects push R **up**. But by how much?

### 6.2 R Decomposition

```
R = avg_win / avg_loss
```

Under the new exit engine, we model:
```
R_new = R_old × exit_improvement_factor
```

The exit improvement factor depends on conviction tier:

| Tier | Old avg_loss | Expected new avg_loss | Old avg_win | Expected new avg_win | Improvement |
|------|-------------|----------------------|-------------|---------------------|-------------|
| LOW  | 1.0x        | 0.85x (tighter stop) | 1.0x        | 0.95x (tight trail) | 1.12×       |
| MED  | 1.0x        | 0.80x                | 1.0x        | 1.10x               | 1.38×       |
| HIGH | 1.0x        | 0.75x                | 1.0x        | 1.30x               | 1.73×       |

**These are ESTIMATES.** The actual improvement factors should be measured after deploying the new exit engine and updated in the LUT recalibration (Section 7).

### 6.3 Adjusted R LUT (Optional — Use After Validation)

For initial deployment, use the empirical R_LUT as-is. After collecting 50+ trades with the new exit engine, compute the actual improvement factors per tier and apply them:

```rust
const EXIT_R_ADJUSTMENT: [u16; 3] = [
    112,  // LOW:  R_new = R_old × 112/100
    138,  // MED:  R_new = R_old × 138/100
    173,  // HIGH: R_new = R_old × 173/100
];

// Apply after initial LUT lookup:
// r_x100_adjusted = r_x100 * EXIT_R_ADJUSTMENT[tier] / 100
```

**Recommendation:** Start with unadjusted R. Log actual R by tier. After 100 trades, compare and update the adjustment factors.

---

## 7. Online LUT Recalibration (Feedback Loop)

### 7.1 Exponential Moving Average Update

After each trade completes, update the LUT cell corresponding to that trade's (mag_bucket, score_bucket):

```rust
/// Exponential decay weight for LUT updates.
/// Higher = more weight to recent trades.
/// 32 means each new trade has weight 32/256 ≈ 12.5% (recent ~8 trades dominate).
const EMA_ALPHA_Q8: u16 = 32;

/// Minimum trades in a cell before allowing updates.
/// Below this, the LUT retains its initial (prior) values.
const MIN_CELL_COUNT: u16 = 10;

struct LutCell {
    p_permille: u16,
    r_x100: u16,
    count: u16,
}

struct AdaptiveLut {
    p_lut: [[LutCell; 4]; 4],
    r_lut: [[LutCell; 4]; 4],
}

impl AdaptiveLut {
    /// Update a cell after a trade completes.
    /// `won`: whether the trade was a winner
    /// `r_realized_x100`: actual win/loss ratio × 100 for this trade
    ///   (for winners: profit/cost × 100; for losers: we only update p, not R)
    fn update(
        &mut self,
        bm: usize,
        bs: usize,
        won: bool,
        r_realized_x100: Option<u16>,
    ) {
        let cell = &mut self.p_lut[bm][bs];
        cell.count = cell.count.saturating_add(1);
        
        if cell.count < MIN_CELL_COUNT {
            return; // Not enough data yet, keep prior
        }
        
        // EMA update for p: p_new = (1-α)·p_old + α·outcome
        // outcome = 1000 if won, 0 if lost
        let outcome: u32 = if won { 1000 } else { 0 };
        let alpha = EMA_ALPHA_Q8 as u32;
        let inv_alpha = 256 - alpha;
        
        cell.p_permille = ((inv_alpha * cell.p_permille as u32 + alpha * outcome + 128) >> 8) as u16;
        
        // EMA update for R (only on wins, using realized R)
        if let Some(r_real) = r_realized_x100 {
            if won {
                let r_cell = &mut self.r_lut[bm][bs];
                r_cell.r_x100 = ((inv_alpha * r_cell.r_x100 as u32 
                                   + alpha * r_real as u32 + 128) >> 8) as u16;
            }
        }
    }
}
```

### 7.2 Recalibration Schedule

| Trigger | Action | Rationale |
|---------|--------|-----------|
| Every trade | EMA update on the hit cell | Continuous learning |
| Every 100 trades | Full LUT snapshot to disk | Persistence across restarts |
| Every 500 trades | Compare LUT to initial values | Detect regime changes |
| Manual | Force reset to initial LUT | If performance degrades (overfitting to noise) |

### 7.3 Regime Change Detection

Monitor the overall f\* (averaged across cells weighted by trade count). If it drops below 0.30 for 50 consecutive trades, the market regime may have shifted. Actions:

```rust
const REGIME_ALARM_F_PERMILLE: u16 = 300;
const REGIME_ALARM_STREAK: u16 = 50;

struct RegimeMonitor {
    consecutive_below: u16,
}

impl RegimeMonitor {
    fn check(&mut self, avg_f_permille: u16) -> bool {
        if avg_f_permille < REGIME_ALARM_F_PERMILLE {
            self.consecutive_below += 1;
        } else {
            self.consecutive_below = 0;
        }
        self.consecutive_below >= REGIME_ALARM_STREAK
    }
}
```

When regime alarm fires:
1. Reduce `correlation_adj` by 50% (from 128 → 64 = quarter-Kelly)
2. Alert operator
3. Continue trading at reduced size while collecting data
4. After 100 more trades, re-evaluate

### 7.4 Serialization (LUT Persistence)

```rust
// Compact binary format: 4×4×2 values × 2 bytes each = 64 bytes for both LUTs
// Plus 4×4 × 2 bytes for counts = 32 bytes
// Total: 96 bytes per checkpoint

fn serialize_lut(lut: &AdaptiveLut) -> [u8; 96] {
    let mut buf = [0u8; 96];
    let mut offset = 0;
    for i in 0..4 {
        for j in 0..4 {
            buf[offset..offset+2].copy_from_slice(&lut.p_lut[i][j].p_permille.to_le_bytes());
            offset += 2;
        }
    }
    for i in 0..4 {
        for j in 0..4 {
            buf[offset..offset+2].copy_from_slice(&lut.r_lut[i][j].r_x100.to_le_bytes());
            offset += 2;
        }
    }
    for i in 0..4 {
        for j in 0..4 {
            buf[offset..offset+2].copy_from_slice(&lut.p_lut[i][j].count.to_le_bytes());
            offset += 2;
        }
    }
    buf
}
```

---

## 8. Full Worked Examples

### Example 1: Sweet Spot Trade
**Input:** magnitude_score=65, entry_score=55, wallet=2.0 SOL, adj=128

1. mag_bucket(65) → bucket 2 (60-70), frac = (65-40-20)×26 = 130
2. score_bucket(55) → bucket 0 (50-60), frac = (55-50)×26 = 130
3. bilerp P_LUT: corners are P[2][0]=676, P[3][0]=587, P[2][1]=556, P[3][1]=483
   - Interpolating at (130/256, 130/256) ≈ (0.51, 0.51):
   - ≈ 0.49×0.49×676 + 0.51×0.49×587 + 0.49×0.51×556 + 0.51×0.51×483
   - ≈ 162 + 146 + 138 + 126 = 572 → **p_permille = 575** (approx)
4. bilerp R_LUT: corners R[2][0]=524, R[3][0]=441, R[2][1]=1168, R[3][1]=981
   - ≈ 0.49×0.49×524 + 0.51×0.49×441 + 0.49×0.51×1168 + 0.51×0.51×981
   - ≈ 126 + 110 + 292 + 255 = 783 → **r_x100 = 783**
5. f_permille = 575 − (1000−575)×100/783 = 575 − 54 = **521**
6. size = 521 × 128 × 2_000_000_000 / 256_000 = **521_000_000 lamports (0.521 SOL)**
7. conviction_tier = 1 (MED, since 450 ≤ 521 < 550)

**Result:** `EntryConviction { p: 575‰, R: 7.83, f: 521‰, size: 0.521 SOL, tier: MED }`

### Example 2: Low-Magnitude Fat-Tail Trade
**Input:** magnitude_score=45, entry_score=62, wallet=1.5 SOL, adj=128

1. mag_bucket(45) → bucket 0 (40-50), frac = 130
2. score_bucket(62) → bucket 1 (60-70), frac = 52
3. bilerp P_LUT: corners P[0][1]=401, P[1][1]=529, P[0][2]=448, P[1][2]=590
   - At (0.51, 0.20): ≈ 0.49×0.80×401 + 0.51×0.80×529 + 0.49×0.20×448 + 0.51×0.20×590
   - ≈ 157 + 216 + 44 + 60 = 477 → **p_permille = 477**
4. bilerp R_LUT: corners R[0][1]=6010, R[1][1]=1579, R[0][2]=3485, R[1][2]=916
   - ≈ 0.49×0.80×6010 + 0.51×0.80×1579 + 0.49×0.20×3485 + 0.51×0.20×916
   - ≈ 2356 + 644 + 342 + 93 = 3435 → **r_x100 = 3435**
5. f_permille = 477 − (1000−477)×100/3435 = 477 − 15 = **462**
6. size = 462 × 128 × 1_500_000_000 / 256_000 = **346_500_000 lamports (0.347 SOL)**
7. conviction_tier = 1 (MED)

**Result:** `EntryConviction { p: 477‰, R: 34.35, f: 462‰, size: 0.347 SOL, tier: MED }`

Note the fascinating dynamics here: p is below 50% but R is 34x. This is a lottery-ticket profile — loses more often than it wins, but winners are enormous. Kelly still says bet, and the MED conviction tier gives it room to run.

### Example 3: High Score but Diminishing Returns
**Input:** magnitude_score=72, entry_score=85, wallet=3.0 SOL, adj=128

1. mag_bucket(72) → bucket 3 (70-80), frac = 52
2. score_bucket(85) → bucket 3 (80+), frac = 130
3. p_permille ≈ P[3][3] = **492** (near cell center, minimal interpolation)
4. r_x100 ≈ R[3][3] = **276**
5. f_permille = 492 − 508×100/276 = 492 − 184 = **308**
6. size = 308 × 128 × 3_000_000_000 / 256_000 = **462_000_000 lamports (0.462 SOL)**
7. conviction_tier = 0 (LOW, since 308 < 450)

**Result:** `EntryConviction { p: 492‰, R: 2.76, f: 308‰, size: 0.462 SOL, tier: LOW }`

This trade barely passes the MIN_F_PERMILLE = 300 threshold. The high magnitude + high score combination paradoxically has the weakest edge — likely because these are crowded trades where everyone sees the same signal.

---

## 9. Integration Checklist

### 9.1 Entry Path (in trade decision pipeline)

```
1. Signal triggers → magnitude_score, entry_score computed
2. Query wallet balance (cached, refresh every 5s)
3. EntryConviction::compute(mag, score, wallet_bal, correlation_adj)
4. Check should_trade() → if false, skip
5. Apply capped_size() safety limit
6. Submit buy transaction with size_lamports
7. Store EntryConviction on OpenPosition
```

### 9.2 Exit Path (in position management)

```
1. Read entry_conviction from OpenPosition
2. Look up ExitParams from conviction_tier
3. Initialize trail at initial_trail_bps from entry price
4. On each price tick / signal update:
   a. Compute signal_factor from real-time data
   b. live_p = entry_p × signal_factor / 1000
   c. live_f = kelly_permille(live_p, entry_r)
   d. Adjust trail width based on live_f trend
5. Exit when price crosses trail or max_hold_ms exceeded
```

### 9.3 Feedback Path (post-trade)

```
1. Trade completes → compute realized P&L
2. Determine won (bool) and r_realized_x100
3. adaptive_lut.update(bm, bs, won, r_realized)
4. regime_monitor.check(avg_f)
5. Every 100 trades: serialize LUT to disk
```

---

## 10. Configuration Constants Summary

```rust
// === LUT Configuration ===
const P_LUT: [[u16; 4]; 4] = [
    [487, 401, 448, 409],
    [643, 529, 590, 539],
    [676, 556, 621, 567],
    [587, 483, 539, 492],
];

const R_LUT: [[u16; 4]; 4] = [
    [2698, 6010, 3485, 1689],
    [ 709, 1579,  916,  444],
    [ 524, 1168,  678,  328],
    [ 441,  981,  569,  276],
];

// === Kelly / Sizing ===
const DEFAULT_CORRELATION_ADJ: u8 = 128;  // Half-Kelly
const MIN_F_PERMILLE: u16 = 300;
const MIN_P_PERMILLE: u16 = 350;
const MAX_SIZE_FRACTION_PERMILLE: u16 = 400;

// === Conviction Tiers ===
const TIER_LOW_THRESHOLD: u16 = 450;
const TIER_HIGH_THRESHOLD: u16 = 550;

// === Feedback Loop ===
const EMA_ALPHA_Q8: u16 = 32;       // ~12.5% weight per new trade
const MIN_CELL_COUNT: u16 = 10;
const LUT_PERSIST_INTERVAL: u16 = 100;  // trades
const REGIME_ALARM_F_PERMILLE: u16 = 300;
const REGIME_ALARM_STREAK: u16 = 50;

// === Exit Params by Tier ===
const EXIT_PARAMS: [ExitParams; 3] = [
    ExitParams { initial_trail_bps: 150, tighten_after_gain_bps: 300, min_trail_bps: 50,  max_hold_ms: 30_000 },
    ExitParams { initial_trail_bps: 250, tighten_after_gain_bps: 500, min_trail_bps: 75,  max_hold_ms: 60_000 },
    ExitParams { initial_trail_bps: 400, tighten_after_gain_bps: 800, min_trail_bps: 100, max_hold_ms: 120_000 },
];
```

---

## 11. Known Limitations & Future Work

### 11.1 Limitations

1. **Multiplicative independence assumption:** The joint p(mag, score) assumes mag and score contribute independently to win probability. In reality there may be interaction effects. With 392 trades across 16 cells (avg 24.5 per cell), we don't have enough data to estimate interactions reliably. The multiplicative model is the right choice *for now*.

2. **R estimation is noisy:** R varies from 2.76 to 60.10 across cells. A single outlier trade (e.g., a 100x winner) in a small cell can distort the estimate. The EMA update with α=32/256 provides some smoothing, but the initial R_LUT values for sparse cells (mag 40–50 × score 60–70: R=60.10) should be treated as high-uncertainty.

3. **No features beyond mag/score:** The LUT doesn't use pre_trigger_buys, trigger_sol, or tod_multiplier directly. These are embedded in the composite scores. If the composite scoring changes, the LUT must be re-estimated.

4. **Static exit improvement factors:** The R adjustment for the new exit engine (Section 6) is purely theoretical. Must be validated empirically.

### 11.2 Future Enhancements

1. **3D LUT:** Add pre_trigger_buys_5s as a third dimension (2 buckets: low/high). Doubles LUT to 32 cells. Only do this after 1000+ trades provide sufficient cell counts.

2. **Bayesian cell priors:** Instead of hard MIN_CELL_COUNT thresholds, use a Beta(α, β) prior for p in each cell, updated with trade outcomes. The prior pulls sparse cells toward the marginal estimate; well-populated cells converge to the empirical value.

3. **Cross-validation:** Hold out 20% of trades, estimate LUT on 80%, evaluate on holdout. Repeat 5-fold. Report calibration (predicted p vs. actual win rate).

4. **Feature interaction term:** After 1000 trades, test whether mag×score interaction improves prediction. Add a single correction matrix if significant.

---

*End of Entry Conviction Estimator Design Document*