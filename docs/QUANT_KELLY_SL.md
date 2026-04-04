# Kelly Criterion & Optimal-f Dynamic Stop-Loss System

## RIDE Strategy Quantitative Analysis — 215 Trades

---

## 1. Kelly-Derived Dynamic Trail Distance

### 1.1 Vince Optimal f — Full Derivation

**Inputs from RIDE dataset:**
- Win probability: p = 0.823 (177 wins / 215 trades)
- Average win: W̄ = +0.00793 SOL (gross +1.40 / 177 wins, adjusted for partial wins)
- Average loss: L̄ = -0.00682 SOL (approximate from gross losses across 38 losing trades)
- Win/Loss ratio: R = W̄ / |L̄| = 0.00793 / 0.00682 = 1.163

**Vince's optimal f formula (simplified Kelly for unequal payoffs):**

```
f* = (p(R + 1) - 1) / R
f* = (0.823 × (1.163 + 1) - 1) / 1.163
f* = (0.823 × 2.163 - 1) / 1.163
f* = (1.780 - 1) / 1.163
f* = 0.780 / 1.163
f* = 0.6707
```

**f* = 0.671 — fraction of capital at risk per trade on the largest expected loss.**

This is the **theoretical maximum**. In practice we use fractional Kelly.

### 1.2 Half-Kelly and Current Sizing

Current system: half-Kelly with 0.10–0.15 SOL positions.

Half-Kelly risk fraction: f*/2 = 0.336

With position size 0.125 SOL (midpoint) and bankroll ~1.5 SOL:
- Position/bankroll = 0.125/1.5 = 0.083
- This is ~0.083/0.671 = **12.4% of full Kelly** — very conservative

This extreme conservatism is appropriate for:
1. Non-stationary edge (bonding curve dynamics change)
2. Fat-tailed losses possible (rug, liquidity drain)
3. Correlated positions (sequential RIDE trades in same market regime)

### 1.3 Mapping f* to Trail Distance

**Core insight:** f* represents the edge strength. When edge is strong (high f*), we can afford wider trails (more room to breathe). When edge degrades, we tighten.

**Trail distance scaling:**

Let `base_trail` be the static trail distance (current system). Define:

```
kelly_ratio = f*_current / f*_baseline
```

Where `f*_baseline = 0.671` (dataset average). Then:

```
trail_distance = base_trail × kelly_ratio^α
```

With α = 0.5 (square root dampening — prevents over-widening):

| f*_current | kelly_ratio | trail_multiplier (α=0.5) |
|-----------|-------------|--------------------------|
| 0.80      | 1.192       | 1.092 (9% wider)         |
| 0.671     | 1.000       | 1.000 (baseline)         |
| 0.50      | 0.745       | 0.863 (14% tighter)      |
| 0.30      | 0.447       | 0.669 (33% tighter)      |
| 0.10      | 0.149       | 0.386 (61% tighter)      |
| 0.00      | 0.000       | 0.000 (immediate exit)   |

**Break-even threshold:**

At f* = 0, there is no edge. The break-even condition:

```
f* = 0  ⟺  p(R+1) = 1  ⟺  p = 1/(R+1)
```

For R = 1.163: p_breakeven = 1/2.163 = **0.462**

When estimated p drops below 0.462, f* goes negative → **exit immediately**.

---

## 2. Real-Time p(t) and R(t) Estimation

### 2.1 Feature-Conditional Win Rates

From the dataset, we can decompose p into conditional estimates based on observable features during the hold period.

#### 2.1.1 By Exit Type (proxy for trade quality during hold)

| Exit Type | n | WR | Avg Hold | Avg Gross PnL/trade |
|-----------|---|-----|----------|---------------------|
| sell_cascade | 54 | 100% | 1653ms | +0.01104 |
| trailing_stop | 50 | 100% | 1211ms | +0.00682 |
| max_hold | 35 | 100% | 1528ms | +0.01097 |
| whale_exit | 38 | 45% | 781ms | +0.00205 |
| hard_floor | 38 | 55% | 606ms | +0.00003 |

**Key observation:** The 76 whale_exit + hard_floor trades (35% of dataset) account for only +0.079 SOL gross (5.6% of total gross PnL) but represent nearly ALL losses. These trades have:
- Shorter hold times (avg ~690ms vs ~1450ms for winners)
- Lower win rates (50% vs 100%)

#### 2.1.2 By Buys After Entry

Higher post-entry buy flow = stronger momentum = higher p(t).

```
buys_after_entry ≥ 11 (p75+): predominantly sell_cascade/max_hold exits → p ≈ 0.95+
buys_after_entry 3-11 (IQR): mixed → p ≈ 0.82 (baseline)
buys_after_entry < 3 (p25-): predominantly whale/hard_floor exits → p ≈ 0.55
```

**Empirical model:**

```
p_buys(n) = min(0.98, 0.50 + 0.04 × n)  for n = buys_after_entry
```

Clamped: p_buys ∈ [0.50, 0.98]

#### 2.1.3 By Confirming Buy SOL

Larger confirming buys indicate stronger conviction:

```
confirm_sol ≥ 5.33 (p75+): p ≈ 0.90
confirm_sol 1.96-5.33 (IQR): p ≈ 0.82
confirm_sol < 1.96 (p25-): p ≈ 0.65
```

**Empirical model:**

```
p_confirm(s) = min(0.95, 0.55 + 0.07 × s)  for s = confirming_buy_sol
```

Clamped: p_confirm ∈ [0.55, 0.95]

#### 2.1.4 By Sells During Hold

Sells are counter-momentum. More sells = degrading edge:

```
sells = 0 (13.5% of trades): p ≈ 0.92 (no counter-pressure)
sells = 1-2 (typical): p ≈ 0.82
sells ≥ 3: p ≈ 0.60 (heavy selling pressure)
```

**Empirical model:**

```
p_sells(n) = max(0.45, 0.92 - 0.10 × n)  for n = sells_during_hold
```

Clamped: p_sells ∈ [0.45, 0.92]

#### 2.1.5 By Time Since Entry

Empirical time decay of edge from hold time distribution:

```
t < 400ms (p25):   p_time ≈ 0.70 (too early, uncertain)
t = 400-1000ms:    p_time ≈ 0.85 (sweet spot)
t = 1000-1500ms:   p_time ≈ 0.88 (still strong)
t = 1500-2000ms:   p_time ≈ 0.82 (starting to decay)
t > 2000ms (p90+): p_time ≈ 0.75 (diminishing edge, should have exited)
```

**Piecewise model (ms):**

```
p_time(t) = 
  0.70 + 0.25 × (t/1000)      for t ∈ [0, 600)      → ramps 0.70 to 0.85
  0.85 + 0.03 × ((t-600)/400)  for t ∈ [600, 1000)   → ramps 0.85 to 0.88
  0.88 - 0.04 × ((t-1000)/1000) for t ∈ [1000, 2000) → decays 0.88 to 0.84
  0.84 - 0.01 × ((t-2000)/1000) for t ≥ 2000          → slow decay below 0.84
```

### 2.2 Combined p(t) Estimator

**Bayesian combination with prior:**

```
p_combined = w_prior × p_prior + w_buys × p_buys + w_confirm × p_confirm + w_sells × p_sells + w_time × p_time
```

**Weights (normalized to sum = 1.0):**

| Factor | Weight | Rationale |
|--------|--------|-----------|
| Prior (0.823) | 0.15 | Base rate anchor |
| Buys after entry | 0.30 | Strongest real-time signal (momentum continuation) |
| Confirming SOL | 0.15 | Entry quality (known at entry, decays in importance) |
| Sells during hold | 0.25 | Direct counter-signal (real-time) |
| Time since entry | 0.15 | Structural decay |

**Example calculations:**

**Strong trade (sell_cascade type):**
- buys_after=12 → p_buys = min(0.98, 0.50+0.48) = 0.98
- confirm=6.0 → p_confirm = min(0.95, 0.55+0.42) = 0.95
- sells=0 → p_sells = 0.92
- time=1200ms → p_time = 0.87
- **p_combined = 0.15×0.823 + 0.30×0.98 + 0.15×0.95 + 0.25×0.92 + 0.15×0.87 = 0.930**
- **f* = (0.930×2.163 - 1)/1.163 = (2.012-1)/1.163 = 0.870**
- Trail multiplier: (0.870/0.671)^0.5 = 1.139 → **14% wider trail**

**Weak trade (hard_floor type):**
- buys_after=1 → p_buys = 0.54
- confirm=1.5 → p_confirm = 0.655
- sells=3 → p_sells = 0.62
- time=500ms → p_time = 0.825
- **p_combined = 0.15×0.823 + 0.30×0.54 + 0.15×0.655 + 0.25×0.62 + 0.15×0.825 = 0.663**
- **f* = (0.663×2.163 - 1)/1.163 = (1.434-1)/1.163 = 0.373**
- Trail multiplier: (0.373/0.671)^0.5 = 0.745 → **25% tighter trail**

**Dying trade (should exit):**
- buys_after=0 → p_buys = 0.50
- confirm=1.0 → p_confirm = 0.62
- sells=5 → p_sells = 0.45 (floor)
- time=300ms → p_time = 0.775
- **p_combined = 0.15×0.823 + 0.30×0.50 + 0.15×0.62 + 0.25×0.45 + 0.15×0.775 = 0.596**
- **f* = (0.596×2.163 - 1)/1.163 = (1.289-1)/1.163 = 0.249**
- Trail multiplier: (0.249/0.671)^0.5 = 0.609 → **39% tighter trail**

### 2.3 Break-Even Threshold and Aggressive Tightening

When p_combined < p_breakeven = 0.462:

```
f* < 0 → NO EDGE → exit at next opportunity
```

**Tightening schedule near break-even:**

```
p_combined ∈ [0.462, 0.55]: trail_multiplier = 0.30 (70% tighter — emergency mode)
p_combined ∈ [0.55, 0.65]:  trail_multiplier = 0.55 (45% tighter — defensive)
p_combined ∈ [0.65, 0.75]:  trail_multiplier = 0.75 (25% tighter — cautious)
p_combined ≥ 0.75:          trail_multiplier = normal Kelly-derived
```

### 2.4 Real-Time R(t) Estimation

R is harder to update in real-time, but we can adjust based on unrealized PnL:

```
If unrealized > 0 (in profit):
  R_effective = max(R_baseline, unrealized / |avg_loss|)
  → Wider trail (more room to capture bigger win)

If unrealized < 0 (in drawdown):
  R_effective = R_baseline × (1 - |unrealized| / |avg_loss|)
  → Tighter trail (shrinking expected payoff)
```

This creates a **convex trail profile**: the more profit we have, the more room we give; the more drawdown, the faster we cut.

---

## 3. Continuous Trail Formula (Integer-Only, Rust-Compatible)

### 3.1 Fixed-Point Arithmetic

All values in **8.8 fixed-point** (multiply by 256, divide by 256):

```
// 8.8 fixed-point: 1.0 = 256, 0.5 = 128, 2.0 = 512

// Constants (precomputed)
const KELLY_BASELINE_FP: u16 = 172;     // 0.671 × 256 = 171.8 → 172
const P_BREAKEVEN_FP: u16 = 118;        // 0.462 × 256 = 118.3 → 118
const R_PLUS_1_FP: u16 = 554;           // 2.163 × 256 = 553.7 → 554
const R_FP: u16 = 298;                  // 1.163 × 256 = 297.7 → 298
const P_PRIOR_FP: u16 = 211;            // 0.823 × 256 = 210.7 → 211
```

### 3.2 p_combined Computation (Integer)

```
// Weights as 8-bit fractions of 256:
// w_prior=38, w_buys=77, w_confirm=38, w_sells=64, w_time=38
// Sum = 255 ≈ 256 (close enough, or adjust w_sells=65 for sum=256)

fn p_combined_fp(p_buys: u16, p_confirm: u16, p_sells: u16, p_time: u16) -> u16 {
    // All inputs are 8.8 FP (0-256 range for probabilities 0.0-1.0)
    let sum: u32 = 
        38 * P_PRIOR_FP as u32 +    // w_prior × p_prior
        77 * p_buys as u32 +         // w_buys × p_buys
        38 * p_confirm as u32 +      // w_confirm × p_confirm
        65 * p_sells as u32 +        // w_sells × p_sells
        38 * p_time as u32;          // w_time × p_time
    
    (sum >> 8) as u16  // Divide by 256 to normalize
}
```

### 3.3 f* Computation (Integer)

```
// f* = (p × (R+1) - 1) / R
// In 8.8 FP: f*_fp = (p_fp × R_PLUS_1_FP / 256 - 256) × 256 / R_FP

fn optimal_f_fp(p_fp: u16) -> u16 {
    let numerator: i32 = (p_fp as i32 * R_PLUS_1_FP as i32) >> 8;  // p × (R+1) in 8.8
    let numerator = numerator - 256;  // subtract 1.0 in 8.8
    if numerator <= 0 { return 0; }   // no edge
    
    ((numerator as u32 * 256) / R_FP as u32) as u16  // divide by R, result in 8.8
}
```

### 3.4 Kelly Scale Factor (Integer Square Root)

We need `(f*_current / f*_baseline)^0.5` in fixed-point.

**LUT approach (16 entries, linear interpolation):**

Precompute `sqrt(ratio)` for ratio ∈ [0.0, 2.0] in steps of 0.125:

```
// kelly_scale = isqrt(f_current_fp * 256 / KELLY_BASELINE_FP)
// Using 16-entry LUT for sqrt in 8.8 FP:

const SQRT_LUT: [u16; 17] = [
    0,     // sqrt(0.000) = 0.000 → 0
    91,    // sqrt(0.125) = 0.354 → 91
    128,   // sqrt(0.250) = 0.500 → 128
    156,   // sqrt(0.375) = 0.612 → 157
    181,   // sqrt(0.500) = 0.707 → 181
    202,   // sqrt(0.625) = 0.791 → 202
    222,   // sqrt(0.750) = 0.866 → 222
    239,   // sqrt(0.875) = 0.935 → 239
    256,   // sqrt(1.000) = 1.000 → 256
    271,   // sqrt(1.125) = 1.061 → 272
    286,   // sqrt(1.250) = 1.118 → 286
    299,   // sqrt(1.375) = 1.173 → 300
    312,   // sqrt(1.500) = 1.225 → 314
    325,   // sqrt(1.625) = 1.275 → 326
    337,   // sqrt(1.750) = 1.323 → 339
    349,   // sqrt(1.875) = 1.369 → 350
    362,   // sqrt(2.000) = 1.414 → 362
];

fn kelly_scale_fp(f_current_fp: u16) -> u16 {
    // ratio = f_current / f_baseline, in 8.8 FP
    let ratio_fp: u32 = (f_current_fp as u32 * 256) / KELLY_BASELINE_FP as u32;
    
    // Clamp to LUT range [0, 2.0] = [0, 512]
    let ratio_fp = ratio_fp.min(512) as u16;
    
    // LUT index: ratio_fp / 32 (each step = 0.125 = 32 in 8.8)
    let idx = (ratio_fp >> 5) as usize;  // divide by 32
    let frac = ratio_fp & 0x1F;          // remainder for interpolation
    
    if idx >= 16 { return SQRT_LUT[16]; }
    
    // Linear interpolation between LUT[idx] and LUT[idx+1]
    let lo = SQRT_LUT[idx] as u32;
    let hi = SQRT_LUT[idx + 1] as u32;
    (lo + (hi - lo) * frac as u32 / 32) as u16
}
```

### 3.5 Final Trail Formula

```
fn dynamic_trail_bp(base_trail_bp: u16, kelly_scale: u16) -> u16 {
    // trail_bp = base_trail_bp × kelly_scale / 256
    let result = (base_trail_bp as u32 * kelly_scale as u32) >> 8;
    
    // Floor: never tighter than 20% of base (avoid zero-width trail)
    let floor = (base_trail_bp as u32 * 51) >> 8;  // 51/256 ≈ 0.20
    
    result.max(floor) as u16
}
```

### 3.6 Bonding Curve Consideration (vSOL Space)

The bonding curve is **Price = vSOL² / k** (quadratic). Trail distances are already expressed in **milli-vSOL basis points** (vSOL space), which linearizes the quadratic curve.

In vSOL space, a trail of `d` milli-vSOL corresponds to a price change of approximately:

```
ΔPrice/Price ≈ 2 × d/vSOL_current  (first-order Taylor expansion of quadratic)
```

The factor of 2 comes from the quadratic: d(vSOL²)/d(vSOL) = 2×vSOL.

**This means:** a fixed trail in vSOL space is already **tighter in percentage terms at higher vSOL levels** (deeper in the bonding curve). The Kelly scaling operates on top of this natural tightening.

No additional bonding curve correction is needed in the trail formula — the vSOL-space representation handles it.

### 3.7 Compute Budget

Total operations for one trail update:

| Step | Operations | Estimate |
|------|-----------|----------|
| p_buys, p_confirm, p_sells, p_time | 4 clamp+scale | ~5ns |
| p_combined (weighted sum) | 5 multiply + 1 shift | ~8ns |
| optimal_f | 1 multiply + 1 shift + 1 subtract + 1 divide | ~6ns |
| kelly_scale (LUT + lerp) | 1 multiply + 1 divide + 1 shift + 1 lerp | ~8ns |
| dynamic_trail | 1 multiply + 1 shift + 1 max | ~3ns |
| **Total** | | **~30ns** ✓ |

Well under the 50ns budget. No branches except the break-even check.

---

## 4. MFE/MAE Analysis

### 4.1 MAE Distribution

From the dataset:

```
MAE p10 = 0.00%
MAE p25 = 0.00%
MAE p50 = 0.00%    ← MEDIAN adverse excursion is ZERO
MAE p75 ≈ 0.30%    (interpolated)
MAE p90 = 1.05%
MAE p95 ≈ 1.60%    (extrapolated: log-normal tail)
MAE p99 ≈ 3.00%    (extrapolated)
```

**Critical insight:** Over 50% of RIDE trades NEVER experience any drawdown. The median MAE is 0%.

### 4.2 Deriving Optimal Stop-Loss from MAE

**Principle:** The optimal SL should be set just beyond the MAE percentile where cumulative win rate exceeds break-even.

**MAE-based SL placement:**

If we set SL at MAE_px, we would stop out (100-x)% of trades that eventually recover. The optimal x minimizes:

```
Cost = (1-x/100) × avg_recovery_profit - (x/100) × avoided_losses
```

For RIDE trades:
- MAE p90 = 1.05%: Setting SL here stops out 10% of trades prematurely
- MAE p95 ≈ 1.60%: Setting SL here stops out 5% of trades prematurely

**With 215 trades at MAE p95 SL (1.60%):**
- ~11 trades (5%) would be stopped out that might have recovered
- Of those 11, empirically ~50% would have been losers anyway
- Net: ~5-6 premature stops, ~5-6 avoided false recovery attempts
- At avg loss of 0.0068 SOL: potential savings ≈ 5 × 0.0068 = 0.034 SOL

**Recommended hard SL: 1.80% adverse excursion** (slightly beyond p95)

This catches true adverse moves while allowing normal noise. In vSOL basis points:

```
For position at vSOL = 30.0:
  1.80% price drop ≈ 0.90% vSOL drop (quadratic halving)
  0.90% × 30,000 milli-vSOL = 270 milli-vSOL basis points

Hard SL = 270 mvSOL-bp (at vSOL=30.0, scales with entry level)
```

### 4.3 MFE Analysis — Profit Capture Efficiency

```
MFE p10 = 2.14%
MFE p25 = 3.30%
MFE p50 = 5.60%
MFE p75 = 9.75%
MFE p90 = 16.06%
```

**Edge ratio (MFE/MAE at matched percentiles):**

```
p50: MFE/MAE = 5.60% / 0.00% = ∞ (no drawdown!)
p75: MFE/MAE ≈ 9.75% / 0.30% = 32.5×
p90: MFE/MAE = 16.06% / 1.05% = 15.3×
```

These ratios are **exceptional**. The strategy has massive positive skew — favorable excursions dwarf adverse ones.

**Implication for trail distance:** Because MFE >> MAE, a wider trail costs very little (rarely triggered on noise) while capturing significant additional upside.

### 4.4 Optimal Trail as MFE Fraction

The trailing stop should capture a fraction of MFE. Empirical guideline:

```
Trail_distance ≈ MAE_p95 + (MFE_p50 - MAE_p95) × 0.15
             ≈ 1.60% + (5.60% - 1.60%) × 0.15
             ≈ 1.60% + 0.60%
             ≈ 2.20%
```

But this is the **static** trail. The Kelly-dynamic system adjusts:
- Strong signals: trail → 2.20% × 1.14 = 2.51% (more room)
- Weak signals: trail → 2.20% × 0.61 = 1.34% (tighter)
- Near break-even: trail → 2.20% × 0.30 = 0.66% (emergency)

---

## 5. Static vs Kelly-Dynamic Comparison

### 5.1 max_hold Trades — Money Left on Table

**Current state:** 35 max_hold exits, ALL profitable, avg gross = +0.01097 SOL/trade

These trades were forcibly exited at the max hold timer. They were still trending in our favor.

**Estimating additional capture with dynamic trailing:**

Since max_hold trades have characteristics of strong trades:
- High buys_after_entry (sustained momentum)
- Low sells_during_hold
- High confirming_buy_sol

A Kelly-dynamic system would assign p_combined ≈ 0.90-0.95 to these, giving:
- f* ≈ 0.80-0.95
- Trail multiplier ≈ 1.10-1.19
- **Wider trail → stays in trade longer**

**Conservative estimate of additional MFE capture:**

If max_hold trades capture MFE at p50 = 5.60% currently, and the dynamic system allows them to reach MFE p60-p65:
- Additional MFE capture: ~1.5-2.0 percentage points
- On 0.125 SOL position: +0.00188 to +0.00250 SOL per trade
- Over 35 trades: **+0.066 to +0.088 SOL additional gross profit**

**But we must also extend max_hold limit or remove it for high-Kelly trades:**

If max_hold removed for trades with f* > 0.70:
- Estimated 20 of 35 max_hold trades would have reached sell_cascade exit
- sell_cascade avg gross = +0.01104, max_hold avg gross = +0.01097
- Minimal per-trade improvement, but some trades would capture much larger moves
- **Estimated improvement: +0.10 to +0.15 SOL over 35 trades**

### 5.2 Whale/Hard_Floor Trades — Faster Cuts

**Current state:**
- ride_whale_exit: 38 trades, 45% WR, gross +0.078 SOL
- ride_hard_floor: 38 trades, 55% WR, gross +0.001 SOL
- Combined: 76 trades, 50% WR, gross +0.079 SOL

**Losses in this group:**

- 38 losing trades (approx), avg loss ≈ -0.0068 SOL
- Total losses ≈ -0.258 SOL
- Total wins ≈ +0.337 SOL
- Net = +0.079 SOL

**With Kelly-dynamic tightening:**

These trades would have low p_combined (0.55-0.65) from:
- Few buys_after_entry
- More sells_during_hold
- Lower confirming_buy_sol

Kelly-dynamic trail multiplier: 0.55-0.75 → **25-45% tighter trail**

**Impact of tighter trails on losing trades:**

A 35% tighter trail on 38 losing trades:
- Average loss reduced from -0.0068 to approximately -0.0044 SOL (35% reduction)
- Loss reduction: 38 × 0.0024 = **+0.091 SOL saved**

**Impact on winning trades in this group:**

Some winning trades would be stopped out prematurely:
- Estimated 5-8 additional stops (trades that needed room to recover)
- Cost: ~7 × 0.004 = **-0.028 SOL lost**

**Net improvement from faster cuts: +0.091 - 0.028 = +0.063 SOL**

### 5.3 Combined P&L Impact

| Category | Static (Actual) | Kelly-Dynamic (Estimated) | Delta |
|----------|----------------|--------------------------|-------|
| max_hold trades (35) | +0.384 | +0.49 to +0.53 | +0.10 to +0.15 |
| whale+floor trades (76) | +0.079 | +0.142 | +0.063 |
| Other trades (104) | +0.937 | +0.937 (unchanged) | 0 |
| **Gross Total** | **+1.400** | **+1.57 to +1.61** | **+0.16 to +0.21** |
| Fees (unchanged) | -0.530 | -0.530 | 0 |
| **Net Total** | **+0.870** | **+1.04 to +1.08** | **+0.16 to +0.21** |

### 5.4 Profit Factor Comparison

**Current (static):**

```
Gross wins = ~1.66 SOL (estimated from net structure)
Gross losses = ~0.26 SOL
Profit Factor = 1.66 / 0.26 = 6.38
```

**Kelly-dynamic (estimated):**

```
Gross wins ≈ 1.76 SOL (max_hold captures more, some whale/floor trades exit faster with small profit)
Gross losses ≈ 0.17 SOL (faster cuts reduce loss magnitude by ~35%)
Profit Factor = 1.76 / 0.17 = 10.35
```

**Profit factor improvement: 6.38 → 10.35 (+62%)**

### 5.5 Impact on Win Rate

The Kelly-dynamic system affects win rate in two opposing ways:

1. **Negative:** Some marginal winners get stopped out by tighter trails → WR decreases slightly
2. **Positive:** Some current losers are cut before full loss materializes → fewer "full losses"

Estimated effect:
- ~5-8 marginal winners converted to small losses (from tighter trails on weak trades)
- ~3-5 losers converted to small winners (tighter trail → exit before drawdown exceeds trail → re-enter if conditions improve)
- Net: **WR approximately unchanged at 81-83%**, but average loss shrinks significantly

### 5.6 Sensitivity Analysis

| Scenario | f* | Trail Mult | Gross Δ | Risk |
|----------|-----|-----------|---------|------|
| Conservative (α=0.3) | 0.671 | 0.85-1.05 range | +0.08-0.12 | Low |
| **Recommended (α=0.5)** | **0.671** | **0.61-1.19 range** | **+0.16-0.21** | **Medium** |
| Aggressive (α=0.7) | 0.671 | 0.45-1.30 range | +0.20-0.28 | High (overfitting) |

The α=0.5 (square root dampening) provides the best balance. Higher α values risk over-fitting to the 215-trade sample.

---

## 6. Implementation Summary

### 6.1 System Architecture

```
Every tick (or on order book event):
  1. Update feature counts: buys_after_entry, sells_during_hold, time_elapsed
  2. Compute p_buys, p_confirm, p_sells, p_time (4 clamped linear models)
  3. Compute p_combined (weighted sum, 8.8 FP)
  4. If p_combined < P_BREAKEVEN_FP → EXIT IMMEDIATELY
  5. Compute f* from p_combined and R (8.8 FP)
  6. Look up kelly_scale from sqrt LUT with lerp
  7. trail_bp = base_trail_bp × kelly_scale >> 8
  8. Apply floor (20% of base)
  9. Update trailing stop level
```

### 6.2 Parameter Table (All Integer Constants)

```
// 8.8 Fixed Point Constants
P_PRIOR_FP          = 211    // 0.823
KELLY_BASELINE_FP   = 172    // 0.671
P_BREAKEVEN_FP      = 118    // 0.462
R_PLUS_1_FP         = 554    // 2.163
R_FP                = 298    // 1.163

// Weight vector (sum = 256)
W_PRIOR              = 38
W_BUYS               = 77
W_CONFIRM            = 38
W_SELLS              = 65
W_TIME               = 38

// p_buys: p = 0.50 + 0.04 × n, clamped [128, 251]
P_BUYS_BASE_FP       = 128    // 0.50
P_BUYS_SLOPE_FP      = 10     // 0.04 × 256 = 10.24
P_BUYS_CEIL_FP       = 251    // 0.98

// p_confirm: p = 0.55 + 0.07 × s, clamped [141, 243]
P_CONFIRM_BASE_FP    = 141    // 0.55
P_CONFIRM_SLOPE_FP   = 18     // 0.07 × 256 = 17.92
P_CONFIRM_CEIL_FP    = 243    // 0.95

// p_sells: p = 0.92 - 0.10 × n, clamped [115, 235]
P_SELLS_BASE_FP      = 235    // 0.92
P_SELLS_SLOPE_FP     = 26     // 0.10 × 256 = 25.6
P_SELLS_FLOOR_FP     = 115    // 0.45

// Trail
TRAIL_FLOOR_NUM       = 51    // 51/256 ≈ 0.20 minimum trail ratio
HARD_SL_PERCENT_FP    = 461   // 1.80% × 256 = 460.8 (hard stop-loss, MAE p95+)

// Recommended base_trail_bp: calibrate from current trailing stop distance
// (This maps to ~2.20% price trail in vSOL space at reference vSOL level)
```

### 6.3 LUT Generation

The 17-entry sqrt LUT covers ratio ∈ [0.0, 2.0]. For f* values beyond 2× baseline (extremely strong edge), clamp to 1.414× trail multiplier. This caps the maximum trail width, preventing runaway holds.

### 6.4 Magnitude Score Integration

The current system uses magnitude for position sizing (0.10-0.15 SOL). The Kelly-dynamic trail is **independent** of position sizing but should respect magnitude as an additional prior:

```
Magnitude segmented f* adjustment:

mag 70-80: WR=63% → p_mag_adj = -0.10 (reduce p_combined by 0.10)
mag 60-70: WR=86% → p_mag_adj = +0.02 (slight boost)
mag 50-60: WR=84% → p_mag_adj = 0.00 (baseline)
mag 40-50: WR=85% → p_mag_adj = +0.01 (near baseline)
```

**Key finding:** High magnitude (70-80) trades have LOWER win rate (63% vs 85% average for others). The Kelly system naturally handles this through confirming_buy_sol (which correlates with magnitude), but an explicit magnitude penalty for mag > 70 is warranted.

---

## 7. Key Findings & Recommendations

### 7.1 The Edge is Real but Lopsided

- **139 trades (65%)** exit via sell_cascade/trailing_stop/max_hold with **100% WR**
- **76 trades (35%)** exit via whale/hard_floor with **50% WR** and contribute only 5.6% of gross PnL
- The strategy's edge is almost entirely from momentum continuation trades
- The "bad" exits are essentially noise trades that the current system handles passively

### 7.2 Dynamic System Value Proposition

The Kelly-dynamic system provides value in two ways:

1. **Let winners run:** +0.10 to +0.15 SOL from extending max_hold trades with strong signals
2. **Cut losers faster:** +0.063 SOL from tightening trails on weak-signal trades
3. **Combined net improvement: +0.16 to +0.21 SOL (+18-24% net PnL improvement)**

### 7.3 Critical Design Choices

1. **α = 0.5 (square root dampening):** Prevents the trail from varying too wildly. The 215-trade sample is sufficient for direction but not for aggressive parameter optimization.

2. **20% trail floor:** Even with zero edge, maintain minimum trail to avoid whipsawing on tick-level noise. The bonding curve has discrete price levels.

3. **Hard SL at 1.80%:** Beyond MAE p95, a drawdown almost certainly indicates adverse selection (rug, whale dump). Cut regardless of Kelly estimate.

4. **Break-even exit at p < 0.462:** When estimated win probability drops below the no-edge threshold, immediate exit preserves capital for better opportunities.

### 7.4 Validation Requirements Before Production

1. **Walk-forward test:** Split 215 trades into train (first 150) / test (last 65). Verify Kelly-dynamic outperforms static on test set.
2. **Monte Carlo:** Bootstrap 10,000 sequences of 215 trades from the empirical distribution. Verify Kelly-dynamic improves median AND p10 outcomes.
3. **Regime sensitivity:** The p(t) models are fitted to current market conditions. Re-calibrate weekly or when win rate deviates >5% from 82.3%.
4. **Latency verification:** Benchmark the integer computation pipeline. Must stay under 50ns to avoid impacting execution latency.

---

## Appendix A: Derivation of p_breakeven

For a binary outcome (win W with probability p, lose L with probability 1-p):

```
Expected value = p × W - (1-p) × L = 0  (break-even)
p × W = (1-p) × L
p × W = L - pL
p(W + L) = L
p = L / (W + L)
```

Substituting R = W/L:

```
p = L / (W + L) = 1 / (W/L + 1) = 1 / (R + 1)
p_breakeven = 1 / (1.163 + 1) = 1 / 2.163 = 0.4623
```

## Appendix B: Why Square Root Dampening (α = 0.5)

The trail multiplier uses `ratio^α` where ratio = f*_current / f*_baseline.

- α = 1.0 (linear): Trail scales linearly with Kelly edge. Problem: f* can swing 3× between strong and weak trades, causing 3× trail variation → too volatile for 215-trade calibration.
- α = 0.5 (sqrt): Trail scales with square root of edge ratio. A 3× edge swing → 1.73× trail swing. More stable, still responsive.
- α = 0.3 (cube root): Very dampened. Almost static trail. Loses most of the dynamic benefit.

The square root also has a natural interpretation: it corresponds to **volatility scaling** in continuous-time finance (σ ∝ √variance). The Kelly edge is analogous to a risk-adjusted return, and scaling trail by its square root matches the standard deviation of the underlying process.

## Appendix C: Integer p_time Implementation

The piecewise p_time function requires branching but is simple:

```
// t in milliseconds, output in 8.8 FP
fn p_time_fp(t_ms: u32) -> u16 {
    if t_ms < 600 {
        // 0.70 + 0.25 × (t/1000) → 179 + (64 × t) / 1000
        let result = 179u32 + (64 * t_ms) / 1000;
        result.min(256) as u16
    } else if t_ms < 1000 {
        // 0.85 + 0.03 × ((t-600)/400) → 218 + (8 × (t-600)) / 400
        let result = 218u32 + (8 * (t_ms - 600)) / 400;
        result.min(256) as u16
    } else if t_ms < 2000 {
        // 0.88 - 0.04 × ((t-1000)/1000) → 225 - (10 × (t-1000)) / 1000
        let result = 225u32.saturating_sub((10 * (t_ms - 1000)) / 1000);
        result as u16
    } else {
        // 0.84 - 0.01 × ((t-2000)/1000) → 215 - (3 × (t-2000)) / 1000
        let result = 215u32.saturating_sub((3 * (t_ms - 2000)) / 1000);
        result.max(128) as u16  // floor at 0.50
    }
}
```

Total: 1-2 comparisons + 1 multiply + 1 divide. ~5ns.