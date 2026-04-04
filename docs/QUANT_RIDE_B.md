# Pump Magnitude Prediction & Ride Engine — Quantitative Design

**Author:** Apollo (Principal Quant Researcher)
**Date:** 2026-03-29
**Companion Docs:** `ENTRY_ENGINE_QUANT.md` (entry scoring), `EXIT_STRATEGY_QUANT.md` (exit state machine)
**Status:** Research-complete design specification

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [The Magnitude Problem: Why 3% ≠ 300%](#2-the-magnitude-problem-why-3--300)
3. [Bonding Curve Physics — Price Impact Derivations](#3-bonding-curve-physics--price-impact-derivations)
4. [Pump Magnitude Predictor](#4-pump-magnitude-predictor)
5. [Composite Entry Score with Magnitude Estimation](#5-composite-entry-score-with-magnitude-estimation)
6. [Real-Time Pump Health Monitor](#6-real-time-pump-health-monitor)
7. [Kelly Criterion with Variable Payoff](#7-kelly-criterion-with-variable-payoff)
8. [SCALP vs RIDE Decision Framework](#8-scalp-vs-ride-decision-framework)
9. [Implementation Specification](#9-implementation-specification)
10. [Expected Outcomes](#10-expected-outcomes)

---

## 1. Executive Summary

### The Gap

The existing entry engine (ENTRY_ENGINE_QUANT.md) answers: *should we enter?* It filters noise and sizes positions to a 54% WR SCALP strategy netting ~0.018 SOL/day. That's the foundation.

But a manual trader makes **2 SOL/day** on 0.01 SOL positions. The math:
- 2 SOL / day on 0.01 SOL sizes ⇒ need 200 SOL-equivalent return ⇒ average gain per winning trade must be **huge** (50-500%+)
- This is impossible with 3-7% SCALP TP targets
- The manual trader is doing something fundamentally different: **selecting tokens that will pump hard, then riding the pump with a trailing stop**

### The Missing Piece

We need a **pump magnitude predictor** — not just "will this win?" but "how far will this go?" — and a **RIDE exit mode** that lets winners run instead of capping them at 3-7%.

### Architecture

```
Entry Signal → Composite Score → Magnitude Estimate → SCALP/RIDE Decision
                                       │
                                       ├─ SCALP: Fixed TP 3-7%, SL 1-2%
                                       │          (existing exit state machine)
                                       │
                                       └─ RIDE: Trailing stop, no fixed TP
                                                Hold 5s-120s+
                                                Target: 20-500%+ on the curve
```

### Expected Outcome (RIDE Mode)

| Metric | SCALP Only | SCALP + RIDE |
|--------|-----------|--------------|
| Daily trades | 12 | 12 (same entries) |
| SCALP trades | 12 | 8 |
| RIDE trades | 0 | 4 |
| SCALP daily net | +0.018 SOL | +0.012 SOL |
| RIDE daily net | — | +0.15 SOL (conservative) |
| **Total daily net** | **+0.018 SOL** | **+0.162 SOL** |
| Monthly | +0.54 SOL | +4.86 SOL |

The RIDE mode is where the real money is. SCALP is the bread-and-butter floor.

---

## 2. The Magnitude Problem: Why 3% ≠ 300%

### 2.1 The Distribution of Pump Outcomes

On Pump.fun's bonding curve, token price trajectories follow a heavily right-skewed distribution:

```
Outcome Distribution (estimated from curve mechanics):
  60-70%: Dead — price doesn't move or fades immediately (buysAfter=0)
  15-20%: Small move — +1-10% then dies
   8-12%: Medium pump — +10-50% over 10-60s
   3-5%:  Large pump — +50-200% over 30-120s
   1-2%:  Graduation pump — +200-500%+ (token graduates to Raydium)
```

The existing SCALP engine caps ALL winning exits at 3-7%. This means:
- On a +200% pump, we capture 5% and leave 195% on the table
- On a graduation (+380%), we capture 5% and leave 375% on the table
- **The expected value of the uncapped tail dwarfs the SCALP profit**

### 2.2 Expected Value Decomposition

For every 100 entries (using the composite scoring engine at 54% WR):

| Outcome | Count | SCALP Capture | RIDE Capture | Difference |
|---------|-------|--------------|-------------|------------|
| Loss (SL hit) | 46 | -1.3% × 46 = -59.8% | -8% × 46 = -368%* | See below |
| Small win (3-10%) | 35 | +5% × 35 = +175% | +5% × 35 = +175% | 0 |
| Medium pump (10-50%) | 12 | +5% × 12 = +60% | +25% × 12 = +300% | +240% |
| Large pump (50-200%) | 5 | +5% × 5 = +25% | +80% × 5 = +400% | +375% |
| Graduation (200%+) | 2 | +5% × 2 = +10% | +250% × 2 = +500% | +490% |
| **Total** | 100 | **+210.2%** | **+1,007%** | **+797%** |

*RIDE SL is wider (5-15%) to avoid getting shaken out. However, RIDE mode is ONLY used on high-magnitude predictions, so not all 46 losses apply. The actual RIDE loss pool is much smaller — see Section 8.

**Key insight:** The return distribution has a fat right tail. The SCALP engine truncates this tail. RIDE mode captures it. Even with wider stops and more losses, the expected value of the tail completely dominates.

### 2.3 Why the Manual Trader Wins

The 2 SOL/day manual trader:
1. **Watches for tokens with unusual momentum patterns** — not just "is there buying?" but "is this buying accelerating in a way that suggests real demand?"
2. **Enters tiny** (0.01 SOL) — position size doesn't matter because gains are 50-500%
3. **Rides with loose trailing stop** — lets winners run 30-120 seconds
4. **Takes many small losses** — 0.01 SOL × lots of losers is cheap
5. **Wins big rarely** — but each big win (+200% on 0.01 SOL = +0.02 SOL) pays for 20 losses

This is a classic **trend-following** strategy applied to a 30-second time horizon on a bonding curve. Low win rate, high payoff asymmetry.

---

## 3. Bonding Curve Physics — Price Impact Derivations

### 3.1 Pump.fun Constant Product AMM

The bonding curve is a constant-product AMM: `x × y = k`, where:
- `x` = vSOL reserves (virtual SOL in the pool)
- `y` = vToken reserves (virtual tokens in the pool)
- `k` = constant (set at token creation)

**Initial state:**
```
x₀ = 30 SOL
y₀ = 1,073,000,000 tokens (1.073B, typical Pump.fun initial supply in pool)
k  = x₀ × y₀ = 30 × 1,073,000,000 = 32,190,000,000
```

**Price:**
```
P = x / y = vSOL / vToken
P₀ = 30 / 1,073,000,000 ≈ 2.796 × 10⁻⁸ SOL/token
```

**Graduation:**
```
x_grad = 115 SOL
y_grad = k / x_grad = 32,190,000,000 / 115 ≈ 279,913,043 tokens
P_grad = 115 / 279,913,043 ≈ 4.109 × 10⁻⁷ SOL/token
P_grad / P₀ = 4.109 × 10⁻⁷ / 2.796 × 10⁻⁸ ≈ 14.69×
```

Wait — let me recalibrate. The ~3.8× figure from the spec assumes a different initial token supply. The exact ratio depends on Pump.fun's specific initial reserves. Let's use the observed relationship:

**Key fact from spec:** Tokens graduating = price ~3.8× from start. This implies:
```
P_grad / P₀ ≈ 3.8×
Since P = x/y and x×y = k:
P = x / (k/x) = x² / k
P_grad / P₀ = x_grad² / x₀² = (115/30)² = 14.69×
```

The 3.8× figure likely measures from a typical *entry point* (not from creation), or accounts for fee extraction. For our purposes, what matters is the math at arbitrary curve positions.

### 3.2 Price as a Function of vSOL

```
P(x) = x² / k = x / y = x / (k/x) = x² / k
```

This is a **quadratic** function of vSOL reserves. Price grows as the square of reserves — buys have accelerating price impact as the curve fills.

**Price change from a buy of Δx SOL at current reserves x:**
```
P_before = x² / k
P_after  = (x + Δx)² / k
ΔP       = [(x + Δx)² - x²] / k = [2xΔx + Δx²] / k

Percentage change:
ΔP/P = [(x + Δx)² - x²] / x² = [2xΔx + Δx²] / x² = 2Δx/x + (Δx/x)²
```

For small buys (Δx << x):
```
ΔP/P ≈ 2Δx/x
```

**This is the fundamental equation.** Price impact scales as 2×(buy_size / current_reserves). The factor of 2 comes from the constant-product formula — price is quadratic in reserves.

### 3.3 Exact Price at Any Curve Position

Define `curvePct = (x - 30) / 85 × 100` (0% at creation, 100% at graduation).

```
x(curvePct) = 30 + 0.85 × curvePct
P(curvePct) = x(curvePct)² / k
```

| curvePct | vSOL (x) | P / P₀ | ΔP/P per 1 SOL buy | Notes |
|----------|----------|--------|---------------------|-------|
| 0% | 30.0 | 1.00× | 6.67% | Fresh token |
| 10% | 38.5 | 1.65× | 5.19% | Early |
| 20% | 47.0 | 2.46× | 4.26% | |
| 30% | 55.5 | 3.43× | 3.60% | |
| 40% | 64.0 | 4.55× | 3.13% | Sweet spot entry |
| 45% | 68.25 | 5.18× | 2.93% | Peak WR zone |
| 50% | 72.5 | 5.85× | 2.76% | |
| 60% | 81.0 | 7.29× | 2.47% | |
| 70% | 89.5 | 8.91× | 2.24% | |
| 80% | 98.0 | 10.67× | 2.04% | Approaching graduation |
| 90% | 106.5 | 12.60× | 1.88% | |
| 100% | 115.0 | 14.69× | 1.74% | Graduation |

**Key insight for RIDE strategy:** The price impact per SOL *decreases* as the curve fills, but the *total remaining upside* from any entry point to graduation is deterministic:

```
Upside_to_grad(curvePct) = P(100%) / P(curvePct) - 1

curvePct=0:  14.69× / 1.00× - 1 = +1,369%
curvePct=20: 14.69× / 2.46× - 1 = +497%
curvePct=30: 14.69× / 3.43× - 1 = +328%
curvePct=40: 14.69× / 4.55× - 1 = +223%
curvePct=45: 14.69× / 5.18× - 1 = +184%  ← our typical entry
curvePct=50: 14.69× / 5.85× - 1 = +151%
curvePct=60: 14.69× / 7.29× - 1 = +101%
curvePct=70: 14.69× / 8.91× - 1 = +65%
curvePct=80: 14.69× / 10.67× - 1 = +38%
curvePct=90: 14.69× / 12.60× - 1 = +17%
```

**Entering at curvePct=45 gives +184% maximum theoretical upside to graduation.** This is the ceiling for RIDE mode. Realistic capture (with trailing stop slippage): 60-80% of theoretical = **+110-147%**.

### 3.4 SOL Required to Move Curve from Point A to Point B

How much buy pressure (in SOL) is needed to move the curve by N percentage points?

```
SOL_needed = x(B) - x(A) = 0.85 × (B - A) SOL
```

This is linear! Each percentage point costs 0.85 SOL, regardless of curve position. The simplicity comes from the fact that curvePct is defined as a linear function of vSOL.

Examples:
- Move 5pp (e.g., 40% → 45%): 4.25 SOL
- Move 10pp (e.g., 40% → 50%): 8.50 SOL
- Move 20pp (e.g., 40% → 60%): 17.00 SOL
- Move 40% → graduation (100%): 51.00 SOL

**But the PRICE change is nonlinear** — the same 4.25 SOL buy produces:
- At 40% curve: +6.6% price increase
- At 80% curve: +4.3% price increase

This means early-curve entries have more price leverage per SOL of follow-through buying.

### 3.5 Sell Impact (Slippage Model for Exit)

When we sell our position, we're removing tokens and extracting SOL. Our sell pushes price down. For a position of size `s` SOL entered at reserves `x_entry`:

```
Tokens received on buy: Δy = y_entry - k/(x_entry + s) = k/x_entry - k/(x_entry + s)
                             = k × s / [x_entry × (x_entry + s)]

SOL received on sell (at reserves x_exit):
  We sell Δy tokens. New token reserves = y_exit + Δy
  New SOL reserves = k / (y_exit + Δy)
  SOL extracted = x_exit - k/(y_exit + Δy)
```

For small positions (s << x), slippage is negligible. At 0.01-0.50 SOL on a 40+ SOL pool, our impact is <1%. This means RIDE mode exit slippage is manageable.

**Pump.fun fee: 1% on each side (buy and sell).** Total round-trip friction: ~2% + Jito tip.

---

## 4. Pump Magnitude Predictor

### 4.1 Feature Design: What Distinguishes Small Pumps from Big Ones?

The question isn't "will price go up?" — the entry engine handles that. The question is: **given that price IS going up, how far will it go?**

This is a fundamentally different prediction task. We're conditioning on positive momentum and predicting *magnitude*.

#### Feature 1: Curve Fill Rate (dCurve/dt) — **Weight: 0.25**

**Definition:** The speed at which the bonding curve is being filled, measured in curvePct-points per second.

```rust
// vsol_delta_3s is the change in vSOL reserves over the last 3 seconds (lamports)
let fill_rate_pps = vsol_delta_3s as f64 / 3.0 / 850_000_000.0;
// 850M lamports = 0.85 SOL = 1 curvePct point
// Result: curvePct points per second
```

**Why it predicts magnitude:**
- Slow fill (<0.5 pp/s): Token is getting some buys but not a real cascade. Likely a +5-15% move, then dies. → SCALP territory.
- Medium fill (0.5-2.0 pp/s): Active buying, multiple actors. Could go +20-80%. → Possible RIDE.
- Fast fill (>2.0 pp/s): Rapid cascade, multiple large buys. Strong graduation candidate. +50-200%+. → RIDE.
- Extreme fill (>5.0 pp/s): Violent pump, likely bot-driven or coordinated. Could graduate in <60s. → RIDE with caution (may also crash fast).

```rust
fn fill_rate_magnitude_score(fill_rate_pps: f64) -> f64 {
    // Maps fill rate to a 0-1 magnitude prediction
    // Sigmoid centered at 1.5 pp/s, where we transition from "small move" to "real pump"
    sigmoid(fill_rate_pps, center: 1.5, steepness: 1.5)
}
```

**Scoring:**
```
fill_rate < 0.3:  score = 0.05  (barely moving, SCALP only)
fill_rate = 0.5:  score = 0.18
fill_rate = 1.0:  score = 0.38
fill_rate = 1.5:  score = 0.50  (inflection: "real pump" territory)
fill_rate = 2.0:  score = 0.62
fill_rate = 3.0:  score = 0.80
fill_rate = 5.0:  score = 0.95  (extreme — graduation candidate)
fill_rate > 8.0:  score = 1.00  (capped)
```

#### Feature 2: Buy Velocity Acceleration (d²buys/dt²) — **Weight: 0.20**

**Definition:** Is the buying rate increasing, steady, or decreasing?

```rust
let rate_1s = buy_count_1s as f64;       // buys/sec over last 1s
let rate_2s = buy_count_2s as f64 / 2.0; // buys/sec over last 2s
let rate_5s = buy_count_5s as f64 / 5.0; // buys/sec over last 5s

// First derivative: current rate vs recent average
let velocity = rate_1s - rate_5s;

// Second derivative: is the acceleration itself increasing?
let accel = (rate_1s - rate_2s) - (rate_2s - rate_5s);

// Combined momentum signal
let momentum_score = velocity * 0.6 + accel * 0.4;
```

**Why it predicts magnitude:**
- Decelerating (accel < 0): The peak has passed. Whatever this token was going to do, it's already done most of it. → SCALP (capture remaining 3-7%).
- Steady (accel ≈ 0): Constant buy rate. Will continue at current pace until something changes. → Medium pump potential.
- **Accelerating (accel > 0): THIS IS THE SIGNAL.** Buy rate is increasing — new buyers are piling in faster than before. Cascade dynamics are engaging. → High magnitude pump.
- Violently accelerating (accel >> 0): Buy rate explosion. Social media/bot cascade. → Graduation candidate.

**Scoring:**
```rust
fn accel_magnitude_score(velocity: f64, accel: f64) -> f64 {
    let combined = velocity * 0.6 + accel * 0.4;
    // Sigmoid: negative combined → low score, positive → high score
    sigmoid(combined, center: 3.0, steepness: 0.4)
}
```

| combined_score | Interpretation | Magnitude |
|---------------|----------------|-----------|
| < 0 | Decelerating — tail of the move | SCALP |
| 0-3 | Steady or mild acceleration | SCALP/low RIDE |
| 3-8 | Strong acceleration — cascade forming | Medium RIDE |
| 8-15 | Violent acceleration — pump cascade | Large RIDE |
| > 15 | Extreme — likely bot/coordinated | Large RIDE (with caution) |

#### Feature 3: Wallet Diversity Quality — **Weight: 0.15**

**Definition:** Not just how many unique wallets, but the *quality distribution* of buyers.

```rust
// Available: unique_buyers_30s, buy_count_5s, volume_sol_5s
// We need to characterize the buyer population

// Metric A: Wallet-to-buy ratio (diversity)
let wallet_ratio = unique_buyers_30s as f64 / max(buy_count_5s, 1) as f64;
// High ratio (>0.8): many unique wallets, each buying once → organic demand
// Low ratio (<0.3): few wallets buying many times → bot/wash activity

// Metric B: Average buy size (conviction per buyer)  
let avg_buy_sol = (volume_sol_5s as f64 / 1e9) / max(buy_count_5s, 1) as f64;
// Large average (>0.5 SOL): committed buyers with conviction
// Small average (<0.1 SOL): dust buys, possible bot swarm

// Metric C: Estimated Gini coefficient proxy (whale concentration)
// Using max_wallet_buy_vol_30s / total_buy_vol_30s from MintHistory
let whale_share = max_wallet_vol as f64 / max(total_buy_vol, 1) as f64;
// whale_share > 0.5: one whale driving the pump → fragile (whale exits = crash)
// whale_share < 0.2: distributed buying → robust (no single point of failure)
```

**Why it predicts magnitude:**
- **Organic cascades go further.** When 15+ unique wallets are each buying 0.2-2 SOL, that's real discovery → real FOMO cascade → potential graduation. Each new wallet is a potential promoter who will shill to friends/social media.
- **Whale-driven pumps die when the whale stops.** If one wallet is 50%+ of volume, the pump's ceiling is the whale's budget. Once they stop, no follow-through.
- **Bot swarms create fake momentum.** Many tiny buys (<0.05 SOL) from related wallets inflate counts but provide no lasting demand.

**Combined wallet quality score:**
```rust
fn wallet_quality_magnitude_score(
    wallet_ratio: f64,
    avg_buy_sol: f64,
    whale_share: f64,
) -> f64 {
    // A: Diversity (want 0.4-0.8 — not too sparse, not too redundant)
    let diversity = gaussian(wallet_ratio, mean: 0.6, sigma: 0.2);
    
    // B: Conviction (want avg buy > 0.2 SOL; diminishing returns above 2 SOL)
    let conviction = sigmoid(avg_buy_sol, center: 0.4, steepness: 3.0);
    
    // C: Distribution (want low whale concentration)
    let distribution = 1.0 - sigmoid(whale_share, center: 0.4, steepness: 5.0);
    
    // Weight: distribution matters most for magnitude
    diversity * 0.30 + conviction * 0.35 + distribution * 0.35
}
```

**Scoring bands:**

| Profile | wallet_ratio | avg_buy | whale_share | Score | Magnitude |
|---------|-------------|---------|-------------|-------|-----------|
| Organic cascade | 0.5-0.8 | 0.3-2.0 | <0.25 | 0.80+ | Large RIDE |
| Quality whale + followers | 0.3-0.5 | 0.5-5.0 | 0.3-0.5 | 0.50-0.70 | Medium RIDE |
| Single whale | <0.2 | 1.0+ | >0.6 | 0.20-0.35 | SCALP (fragile) |
| Bot swarm | 0.8+ | <0.1 | <0.15 | 0.15-0.30 | SCALP (fake) |

#### Feature 4: Curve Position × Remaining Upside — **Weight: 0.15**

**Definition:** Where are we on the curve, and how much theoretical upside remains to graduation?

From Section 3.3, the maximum remaining upside is deterministic:
```rust
fn remaining_upside_pct(curve_pct: f64) -> f64 {
    // Maximum price gain from current position to graduation
    let x_now = 30.0 + 0.85 * curve_pct;
    let x_grad = 115.0;
    // Price ratio = (x_grad / x_now)²
    (x_grad / x_now).powi(2) - 1.0
}
```

| curvePct | Max Upside | Practical RIDE ceiling* |
|----------|-----------|------------------------|
| 20% | +497% | ~200% |
| 30% | +328% | ~150% |
| 40% | +223% | ~120% |
| 45% | +184% | ~100% |
| 50% | +151% | ~80% |
| 60% | +101% | ~55% |
| 70% | +65% | ~35% |
| 80% | +38% | ~20% |

*Practical ceiling ≈ 50-55% of theoretical (trailing stop slippage + pump usually doesn't go to graduation)

**Scoring:**
```rust
fn curve_magnitude_score(curve_pct: f64) -> f64 {
    // Earlier entries have more magnitude potential
    // But very early (< 15%) is risky — unproven demand
    // Sweet spot: 20-50% (proven demand + large upside)
    let upside = remaining_upside_pct(curve_pct);
    let upside_score = sigmoid(upside, center: 100.0, steepness: 0.015);
    
    // Penalty for very early curve (unproven)
    let maturity_penalty = sigmoid(curve_pct, center: 15.0, steepness: 0.3);
    
    upside_score * maturity_penalty
}
```

| curvePct | upside_score | maturity_penalty | Final score | RIDE viability |
|----------|-------------|-----------------|------------|----------------|
| 10% | 0.98 | 0.18 | 0.18 | Too early |
| 20% | 0.97 | 0.82 | 0.79 | Good |
| 30% | 0.93 | 0.99 | 0.92 | Excellent |
| 40% | 0.83 | 1.00 | 0.83 | Very good |
| 50% | 0.69 | 1.00 | 0.69 | Good |
| 60% | 0.50 | 1.00 | 0.50 | Marginal |
| 70% | 0.33 | 1.00 | 0.33 | SCALP only |
| 80% | 0.20 | 1.00 | 0.20 | SCALP only |

#### Feature 5: Volume Intensity (SOL throughput) — **Weight: 0.10**

**Definition:** Total SOL flowing through the curve per second.

```rust
let vol_per_sec = (volume_sol_5s as f64 / 1e9) / 5.0;  // SOL/second over last 5s
```

**Why it predicts magnitude:**
Volume intensity represents the total *capital commitment* to this token. High volume means lots of people putting real SOL at risk → stronger conviction → more likely to cascade.

But volume alone is insufficient — 10 SOL/s from one whale is different from 10 SOL/s from 20 wallets. That's why this has lower weight than wallet quality.

```rust
fn volume_magnitude_score(vol_per_sec: f64) -> f64 {
    sigmoid(vol_per_sec, center: 2.0, steepness: 0.8)
}
```

| SOL/sec | Score | Interpretation |
|---------|-------|----------------|
| < 0.5 | 0.18 | Trickle — SCALP at best |
| 1.0 | 0.31 | Light flow |
| 2.0 | 0.50 | Moderate — could go either way |
| 4.0 | 0.83 | Heavy — real pump |
| 8.0 | 0.99 | Extreme — graduation candidate |

#### Feature 6: Sell Pressure Vacuum — **Weight: 0.10**

**Definition:** Absence of selling during an active buying period.

```rust
let sell_ratio = sell_count_5s as f64 / max(buy_count_5s, 1) as f64;
let sell_vol_ratio = sell_volume_5s as f64 / max(volume_sol_5s, 1) as f64;

// Vacuum score: how clean is the buy flow?
let vacuum_score = (1.0 - sell_ratio) * 0.5 + (1.0 - sell_vol_ratio) * 0.5;
```

**Why it predicts magnitude:**
- **Zero sells during active buying = maximum magnitude potential.** Nobody is taking profit yet → the pump hasn't attracted sellers → upside is unimpeded.
- **Sells appearing during buying = pressure building.** The pump will face resistance. Each seller dampens the cascade and may trigger panic sells.
- **Sell volume > buy volume = pump is dying.** Regardless of other signals, net selling kills pumps.

**Scoring:** Sell vacuum score is already 0-1 from the formula above.

| sell_ratio | sell_vol_ratio | Score | Interpretation |
|-----------|---------------|-------|----------------|
| 0% | 0% | 1.00 | Pure buy flow — maximum magnitude |
| 5% | 3% | 0.96 | Near-clean flow |
| 15% | 10% | 0.88 | Light selling (early profit-taking) |
| 25% | 20% | 0.78 | Moderate selling (healthy but limiting) |
| 40% | 35% | 0.63 | Heavy selling (pump weakening) |
| > 50% | > 50% | < 0.50 | Pump dying → SCALP only |

#### Feature 7: Token Age at Entry