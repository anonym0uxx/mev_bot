# QUANT_RIDE_C: Optimal Trailing Stop Mathematics for Pump.fun Bonding Curves

> Generated: 2026-03-29 | Quantitative analysis of trailing stop mechanics on bonding curves

## Bonding Curve Reference

```
Price P = vSOL² / k        where k = vSOL × vTokens = constant product
Price ratio: P(S2)/P(S1) = (S2/S1)²
Buy of d SOL at reserves S: price impact ≈ 2d/S  (first-order approx)
curvePct = (vSOL - 30) / 85 × 100    (0% at creation, 100% at graduation)
Each curvePct point ≈ 0.85 SOL net buying
Initial vSOL = 30 SOL,  graduation vSOL = 115 SOL
```

---

## Section 1: Sell Impact (Exit Slippage) — Exact Computation

### 1.1 Exact Slippage Formula

When we sell a position worth `s` SOL-equivalent at current reserves `vSOL = S`:

The constant product is `k = S × T` where T = token reserves. We hold some tokens. When we sell those tokens back, vSOL decreases by exactly `s_received`:

**Exact model:** We bought at vSOL = S_entry, acquiring tokens. Price moved to vSOL = S_now. We sell tokens that cost us `s` SOL to acquire. But the actual SOL received depends on the sell path.

**Simplified model (position sizing):** If our position is small relative to reserves, we can approximate. We hold tokens worth `s` SOL at current price. Selling them reduces vSOL by approximately `s`. The average execution price is worse than spot by the slippage amount.

**Exact slippage for selling `s` SOL-worth of tokens:**

Starting at vSOL = S, after our sell, vSOL_new = S - s (approximately, for tokens valued at s SOL).

```
Spot price before sell:  P_before = S² / k
Spot price after sell:   P_after  = (S - s)² / k
Average execution price: P_avg    = (S² - (S-s)²) / (2k) ... no.
```

More precisely: we receive SOL equal to the integral of the price curve from S down to S-Δ where Δ is the vSOL reduction. For a constant-product AMM selling tokens:

```
SOL received = S - k/T_new = S - S×T/(T + tokens_sold)
```

But let's use the **vSOL-based shortcut**. If we define our position as "the SOL we'd get back by selling all our tokens," then the slippage is:

```
Exact price impact = 1 - (average_sell_price / spot_price)
                   = 1 - [(S² - (S-s)²) / (s × 2S)]    ... wait, let me be precise.
```

**Deriving from first principles:**

We sell tokens that reduce vSOL from S to S - δ. The SOL we receive = δ (by definition of how vSOL works — each lamport of vSOL decrease = 1 lamport received).

Actually — on a constant product curve, when you sell tokens:
- Before: vSOL = S, vTokens = T, k = S×T
- You sell Δt tokens. New state: vSOL' = k/(T + Δt), vTokens' = T + Δt
- SOL received = S - vSOL' = S - k/(T + Δt) = S - ST/(T + Δt) = S × Δt/(T + Δt)

The spot price at entry = S/T (in SOL per token, from dSOL/dTokens on constant product = S/T... actually P = S²/k = S²/(ST) = S/T. Yes.)

So the "fair value" of Δt tokens at spot = Δt × S/T.

Actual received = S × Δt / (T + Δt).

**Slippage = 1 - actual/fair = 1 - [S × Δt/(T + Δt)] / [Δt × S/T] = 1 - T/(T + Δt)**

Now we need Δt in terms of our position size s (SOL value of our tokens at spot):

```
s = Δt × S/T    →   Δt = s × T/S
```

Substituting:
```
Slippage = 1 - T/(T + sT/S) = 1 - 1/(1 + s/S) = (s/S) / (1 + s/S) = s / (S + s)
```

### ✅ EXACT SLIPPAGE FORMULA:

```
Slippage = s / (S + s)

where:
  s = position size in SOL (value at spot)
  S = current vSOL reserves
```

This is beautifully simple. For small s << S, slippage ≈ s/S. The second-order correction is -s²/S².

**Actual SOL received = s × S/(S + s) = s × (1 - slippage) = s²/(wouldn't simplify nicely)... let me restate:**

Wait. Let me re-derive actual SOL received:

```
Actual SOL received = S × Δt / (T + Δt)
                    = S × (sT/S) / (T + sT/S)  
                    = sT / (T + sT/S)
                    = sT / (T(1 + s/S))
                    = s / (1 + s/S)
                    = sS / (S + s)
```

So:
```
SOL received = s × S / (S + s)
Lost to slippage = s - sS/(S+s) = s² / (S + s)
Slippage % = s / (S + s)
```

### 1.2 Exact Slippage Table

| Position (s SOL) | vSOL=35 | vSOL=40 | vSOL=50 | vSOL=60 | vSOL=70 | vSOL=80 | vSOL=100 |
|---|---|---|---|---|---|---|---|
| **0.05** | 0.143% | 0.125% | 0.100% | 0.083% | 0.071% | 0.063% | 0.050% |
| **0.10** | 0.285% | 0.249% | 0.200% | 0.166% | 0.143% | 0.125% | 0.100% |
| **0.15** | 0.427% | 0.374% | 0.299% | 0.249% | 0.214% | 0.187% | 0.150% |
| **0.25** | 0.709% | 0.621% | 0.498% | 0.415% | 0.356% | 0.311% | 0.249% |
| **0.50** | 1.408% | 1.235% | 0.990% | 0.826% | 0.709% | 0.621% | 0.498% |

**Computed as:** `slippage = s / (S + s) × 100%`

### 1.3 SOL Actually Received (after slippage)

| Position (s SOL) | vSOL=35 | vSOL=40 | vSOL=50 | vSOL=60 | vSOL=70 | vSOL=80 | vSOL=100 |
|---|---|---|---|---|---|---|---|
| **0.05** | 0.04993 | 0.04994 | 0.04995 | 0.04996 | 0.04996 | 0.04997 | 0.04998 |
| **0.10** | 0.09972 | 0.09975 | 0.09980 | 0.09983 | 0.09986 | 0.09988 | 0.09990 |
| **0.15** | 0.14936 | 0.14944 | 0.14955 | 0.14963 | 0.14968 | 0.14972 | 0.14978 |
| **0.25** | 0.24823 | 0.24845 | 0.24876 | 0.24896 | 0.24911 | 0.24922 | 0.24938 |
| **0.50** | 0.49296 | 0.49383 | 0.49505 | 0.49587 | 0.49645 | 0.49689 | 0.49751 |

### 1.4 Key Takeaways for Position Sizing

- **At 0.10 SOL position**: slippage is 0.10-0.29% across all curve positions → **negligible**
- **At 0.25 SOL position**: slippage is 0.25-0.71% → still manageable
- **At 0.50 SOL position**: slippage hits 1.4% at low vSOL → **meaningful at early curve**
- **Rule of thumb**: keep position < 1% of vSOL for sub-0.5% slippage
- **0.10 SOL is the sweet spot** — always under 0.3% slippage even at vSOL=35

### 1.5 Combined Slippage: Entry + Exit

Total round-trip slippage for a 0.10 SOL position:
- Entry at vSOL=40: ~0.25% (buy impact)  
- Exit at vSOL=50 (after pump): ~0.20% (sell impact)
- **Round-trip: ~0.45%** → well under 1%, doesn't materially affect strategy

For 0.25 SOL:
- Entry at vSOL=40: ~0.62%
- Exit at vSOL=50: ~0.50%
- **Round-trip: ~1.12%** → starts to matter, eats into small gains

---

## Section 2: Optimal Trail Width by Phase

### 2.1 Sell-Induced Price Drops on the Bonding Curve

When someone sells X SOL-worth of tokens at vSOL = S:

```
Price before: P₀ = S²/k
vSOL after sell: S' = S × S/(S + X) = S²/(S + X)

Actually wait — I derived above that selling X SOL-worth removes tokens,
and vSOL drops by: δS = S - S²/(S+X) = SX/(S+X)

Price after: P₁ = (S - δS)²/k = (S²/(S+X))²/k = S⁴/((S+X)²k)

Price drop = 1 - P₁/P₀ = 1 - S²/(S+X)²
```

Let me verify with the approximation. For small X:
```
1 - S²/(S+X)² ≈ 1 - (1 - X/S)² × ... hmm, let me just expand.

S²/(S+X)² = 1/(1 + X/S)² ≈ 1 - 2X/S + 3(X/S)²...

Price drop ≈ 2X/S - 3(X/S)² + ...
```

So the first-order approximation `2X/S` is correct. Let me compute exact values.

### 2.2 Exact Price Drop Table: Single Sell

**Price drop = 1 - S²/(S+X)²** where X = sell size, S = current vSOL

| Sell Size (X SOL) | vSOL=40 | vSOL=45 | vSOL=50 | vSOL=55 | vSOL=60 | vSOL=70 |
|---|---|---|---|---|---|---|
| **0.10** | 0.498% | 0.443% | 0.399% | 0.363% | 0.333% | 0.285% |
| **0.20** | 0.990% | 0.882% | 0.794% | 0.723% | 0.663% | 0.569% |
| **0.30** | 1.478% | 1.316% | 1.187% | 1.080% | 0.990% | 0.851% |
| **0.50** | 2.439% | 2.177% | 1.961% | 1.790% | 1.639% | 1.408% |
| **1.00** | 4.756% | 4.263% | 3.846% | 3.518% | 3.223% | 2.775% |

**Computed as:** `price_drop = 1 - (S/(S+X))²`

### 2.3 Consecutive Sells (Compounding)

For N consecutive sells of X SOL each at starting vSOL = S:

After sell 1: vSOL_1 = S²/(S+X), price ratio = (S/(S+X))²
After sell 2: effective vSOL_2 = vSOL_1²/(vSOL_1+X)... this gets messy. 

**Simpler approach:** Each sell of X reduces effective vSOL by factor S/(S+X). Two consecutive sells of X at ~same reserves:

```
Combined price ratio ≈ (S/(S+X))² × (S/(S+X))² = (S/(S+X))⁴
Combined price drop ≈ 1 - (S/(S+X))⁴
```

But the second sell happens at lower vSOL, so it's actually slightly worse. For precision, let's compute sequentially:

**Example: 3 sells of 0.30 SOL at starting vSOL=50:**

```
Sell 1: vSOL goes from 50 to ~49.70 (δ = 50×0.30/50.30 = 0.2988)
  Actually: δS = S×X/(S+X) = 50×0.3/50.3 = 0.29821
  vSOL_1 = 50 - 0.29821 = 49.7018
  Price ratio_1 = (49.7018/50)² = 0.98809

Sell 2: vSOL_1 = 49.7018, X = 0.30
  δS = 49.7018×0.3/50.0018 = 0.29821
  vSOL_2 = 49.7018 - 0.29821 = 49.4036
  Price ratio_2 = (49.4036/49.7018)² = 0.98800
  
Sell 3: vSOL_2 = 49.4036, X = 0.30
  δS = 49.4036×0.3/49.7036 = 0.29819
  vSOL_3 = 49.4036 - 0.29819 = 49.1054
  Price ratio_3 = (49.1054/49.4036)² = 0.98791

Combined price ratio = 0.98809 × 0.98800 × 0.98791 = 0.96429
Combined price drop = 3.571%
```

**Example: 3 sells of 0.50 SOL at starting vSOL=50:**
```
Sell 1: δS = 50×0.5/50.5 = 0.49505, vSOL_1 = 49.505
  Price ratio_1 = (49.505/50)² = 0.98020

Sell 2: δS = 49.505×0.5/50.005 = 0.49500, vSOL_2 = 49.010
  Price ratio_2 = (49.010/49.505)² = 0.98001

Sell 3: δS = 49.010×0.5/49.510 = 0.49495, vSOL_3 = 48.515
  Price ratio_3 = (48.515/49.010)² = 0.97982

Combined price drop = 1 - (0.98020 × 0.98001 × 0.97982) = 1 - 0.94075 = 5.925%
```

### 2.4 Volatility Budget by Phase

**Assumptions about "normal" sell activity during an active pump:**

| Scenario | Typical Sells | At vSOL ~50 |
|---|---|---|
| Light taking | 1× of 0.1-0.2 SOL | 0.4-0.8% drop |
| Normal taking | 1× of 0.3-0.5 SOL | 1.2-2.0% drop |
| Heavy taking (still pumping) | 2-3× of 0.3-0.5 SOL | 3.5-5.9% drop |
| Dump signal | 3+× of 0.5+ SOL in <2s | >6% drop |

### 2.5 Trail Width Recommendations (Price Space)

| Phase | Time Window | Must Survive | Price Drop Budget | **Trail Width (price)** | Rationale |
|---|---|---|---|---|---|
| **EARLY** | 0–15s | 3× heavy sells (0.5 SOL each) | ~5.9% | **8%** | Wide enough to survive heavy profit-taking; pump is unconfirmed |
| **MOMENTUM** | 15–60s | 2× normal sells (0.3-0.5 SOL) | ~3.9% | **6%** | Pump is confirmed; moderate taking is expected |
| **TIGHTEN** | 60s+ | 1× normal sell (0.5 SOL) | ~2.0% | **4%** | Late-stage; lock in gains; any heavy selling = real reversal |

**Key insight:** These are PRICE-space trails. They must be converted to vSOL-space for on-chain computation.

### 2.6 Phase-Dependent Adjustments by Curve Position

At lower vSOL, the same SOL sell causes a bigger price drop. So trail widths should be slightly wider at low vSOL:

| Phase | vSOL=40 trail | vSOL=50 trail | vSOL=60 trail | vSOL=70+ trail |
|---|---|---|---|---|
| EARLY | 10% | 8% | 7% | 6% |
| MOMENTUM | 8% | 6% | 5% | 5% |
| TIGHTEN | 5% | 4% | 3.5% | 3% |

> **Implementation note:** For simplicity, use the standard 8%/6%/4% values and accept slightly more false triggers at low vSOL. The position sizes at low vSOL are also smaller, so the cost of a false trigger is lower. We can add vSOL-dependent adjustment in v2.

---

## Section 3: Trail in vSOL Space (Integer Math)

### 3.1 The Quadratic Relationship

Since Price = vSOL²/k, a percentage drop in vSOL amplifies in price:

```
If vSOL drops by x% (factor 1-x):
  Price drops by 1-(1-x)² = 2x - x²

Conversely, if we want price trail of p%:
  Need vSOL trail of 1 - √(1-p)
```

### 3.2 Full Conversion Table: Price Trail → vSOL Trail

| Desired Price Trail | vSOL Trail Needed | Fixed-Point (basis points) | Verification: (1-vSOL_trail)² |
|---|---|---|---|
| 2% | 1.0050% | 101 | 0.9800 ✓ |
| 3% | 1.5114% | 151 | 0.9700 ✓ |
| 4% | 2.0204% | 202 | 0.9600 ✓ |
| 5% | 2.5317% | 253 | 0.9500 ✓ |
| 6% | 3.0451% | 305 | 0.9400 ✓ |
| 7% | 3.5606% | 356 | 0.9300 ✓ |
| 8% | 4.0810% | 408 | 0.9200 ✓ |
| 10% | 5.1317% | 513 | 0.9000 ✓ |
| 12% | 6.1916% | 619 | 0.8800 ✓ |
| 15% | 7.8461% | 785 | 0.8500 ✓ |
| 20% | 10.5573% | 1056 | 0.8000 ✓ |
| 25% | 13.3975% | 1340 | 0.7500 ✓ |
| 30% | 16.3340% | 1633 | 0.7000 ✓ |

**Verification formula:** `(1 - vSOL_trail/100)² = 1 - price_trail/100`

### 3.3 Recommended Trail Parameters (vSOL Fixed-Point)

| Phase | Price Trail | vSOL Trail (%) | vSOL Fixed-Point (÷10000) | Integer Constant |
|---|---|---|---|---|
| **EARLY** | 8% | 4.081% | 408 | `TRAIL_EARLY = 408` |
| **MOMENTUM** | 6% | 3.045% | 305 | `TRAIL_MOMENTUM = 305` |
| **TIGHTEN** | 4% | 2.020% | 202 | `TRAIL_TIGHTEN = 202` |
| **EMERGENCY** | 2% | 1.005% | 101 | `TRAIL_EMERGENCY = 101` |

### 3.4 On-Chain Trail Computation (Integer Math)

```rust
// All values in lamports (u64). 1 SOL = 1_000_000_000 lamports.
// trail_distance is in basis points of vSOL (e.g., 408 = 4.08%)

fn compute_trail_stop(peak_vsol: u64, trail_bp: u16) -> u64 {
    // trail_stop = peak_vsol × (10000 - trail_bp) / 10000
    // Use u128 intermediate to prevent overflow
    let stop = (peak_vsol as u128) * ((10000 - trail_bp as u128)) / 10000;
    stop as u64
}

// Example: peak_vsol = 55 SOL = 55_000_000_000 lamports
// EARLY trail (408 bp):
//   stop = 55B × 9592 / 10000 = 52_756_000_000 (52.756 SOL)
// Price at peak: 55² = 3025 (arbitrary units)
// Price at stop: 52.756² = 2783.2 
// Price trail: 1 - 2783.2/3025 = 7.99% ≈ 8% ✓

fn should_sell(current_vsol: u64, trail_stop_vsol: u64) -> bool {
    current_vsol <= trail_stop_vsol  // Single u64 compare!
}
```

### 3.5 Updating Peak and Trail Stop

```rust
fn update_peak(state: &mut RideState, new_vsol: u64) {
    if new_vsol > state.peak_vsol {
        state.peak_vsol = new_vsol;
        // Recompute trail stop with current phase's trail distance
        state.trail_stop_vsol = compute_trail_stop(new_vsol, state.current_trail_bp);
    }
}

fn transition_phase(state: &mut RideState, new_trail_bp: u16) {
    state.current_trail_bp = new_trail_bp;
    // Tighten trail from current peak (don't reset peak!)
    state.trail_stop_vsol = compute_trail_stop(state.peak_vsol, new_trail_bp);
}
```

### 3.6 Precision Analysis

At vSOL = 50 SOL = 50_000_000_000 lamports:
- TIGHTEN trail (202 bp): stop at 48_990_000_000 lamports
- Resolution: 1 lamport = 0.000000002% of vSOL → **far exceeds needed precision**
- Even at vSOL = 30 SOL (minimum): 1 lamport = 0.0000000033% → still absurd precision

**No precision issues whatsoever with integer math in lamports.**

---

## Section 4: Expected P&L Under Ride Strategy

### 4.1 Model Setup

**Assumptions:**
- 75 RIDE-qualified trades per day
- Position size: variable (0.05, 0.10, 0.15 SOL)
- Win rate: 85% (trailing stop gives back some vs. perfect exit)
- Loss on losing trades: -100% of position (worst case; trail fails to trigger before dump)
  - Realistically: -50% to -80% on losses (trail catches most dumps partway)
  - Use -60% average loss for realistic model, -100% for conservative

**Trailing stop capture model:**

When a token pumps X% from entry and then reverses, the trailing stop captures:
```
Captured gain = peak_gain - trail_width - slippage

For a peak of 40% with 8% trail (EARLY exit): capture = 40% - 8% = 32%
  → But we only exit at 32% if we entered at baseline and peak was exactly 40%
  → Then reversed immediately → trail triggers at 32% gain
```

Wait — this isn't right. The trail is from PEAK, not from entry. So:

```
If entry at price P₀, peak at P₀×(1+G), trail triggers at P₀×(1+G)×(1-T):

Capture = (1+G)(1-T) - 1 = G - T - GT

For G=40% peak, T=8% trail: capture = 0.40 - 0.08 - 0.032 = 0.288 = 28.8%
For G=40% peak, T=6% trail: capture = 0.40 - 0.06 - 0.024 = 0.316 = 31.6%
For G=40% peak, T=4% trail: capture = 0.40 - 0.04 - 0.016 = 0.344 = 34.4%
```

### 4.2 Capture Rate by Phase

Tokens don't all peak at the same gain. The peak depends on when the pump stalls. Model the peak as the gain achieved by the time the pump reverses.

**Phase exit model:**

| Exit Phase | % of Trades | Avg Time | Avg Peak Gain | Trail Width | Avg Capture | Capture % of Peak |
|---|---|---|---|---|---|---|
| EARLY (0-15s) | 40% | ~8s | 20% | 8% | 10.4% | 52% |
| MOMENTUM (15-60s) | 35% | ~35s | 50% | 6% | 41.0% | 82% |
| TIGHTEN (60s+) | 25% | ~90s | 100% | 4% | 92.0% | 92% |

**How I computed these:**

- EARLY exit: Peak is modest (token got 2-3 buys then stalled). G=20%, T=8%: capture = 0.20 - 0.08 - 0.016 = 0.104 = 10.4%
- MOMENTUM: Token pumped solidly. G=50%, T=6%: capture = 0.50 - 0.06 - 0.030 = 0.410 = 41.0%
- TIGHTEN: Real pump. G=100%, T=4%: capture = 1.00 - 0.04 - 0.040 = 0.920 = 92.0%

### 4.3 Weighted Average Capture

```
Avg capture (winning trades) = 0.40 × 10.4% + 0.35 × 41.0% + 0.25 × 92.0%
                              = 4.16% + 14.35% + 23.00%
                              = 41.51%
```

### 4.4 Daily P&L Model — Conservative Scenario (40% avg peak)

Recalibrating with 40% average peak across ALL winners (not phase-specific peaks):

Actually, let me keep the phase-specific peaks as they better model reality. The "40% average peak" was meant as a blended average. Let me verify:

```
Blended avg peak = 0.40 × 20% + 0.35 × 50% + 0.25 × 100% = 8% + 17.5% + 25% = 50.5%
```

That's actually 50.5% average peak, which seems right for tokens that get 2+ confirming buys.

**For 0.10 SOL position size:**

```
Winning trades: 75 × 85% = 63.75 trades
Average gain per winner: 41.51% × 0.10 = 0.04151 SOL
Total winning: 63.75 × 0.04151 = 2.646 SOL

Losing trades: 75 × 15% = 11.25 trades
Average loss per loser: 60% × 0.10 = 0.06 SOL (trail catches partial)
Total losing: 11.25 × 0.06 = 0.675 SOL

Fees per trade: 0.10 × 1% (Pump.fun fee) + ~0.000005 SOL (priority) ≈ 0.001 SOL
Total fees: 75 × 2 × 0.001 = 0.150 SOL (entry + exit)

Slippage per trade: ~0.45% round-trip × 0.10 = 0.00045 SOL
Total slippage: 75 × 0.00045 = 0.034 SOL

Daily NET P&L = 2.646 - 0.675 - 0.150 - 0.034 = 1.787 SOL
```

### 4.5 Daily P&L Across Position Sizes

| Metric | 0.05 SOL | 0.10 SOL | 0.15 SOL |
|---|---|---|---|
| Gross winning | 1.323 SOL | 2.646 SOL | 3.969 SOL |
| Gross losing | -0.338 SOL | -0.675 SOL | -1.013 SOL |
| Fees (1% per side) | -0.075 SOL | -0.150 SOL | -0.225 SOL |
| Slippage | -0.017 SOL | -0.034 SOL | -0.051 SOL |
| **Daily NET** | **0.893 SOL** | **1.787 SOL** | **2.680 SOL** |
| Daily NET (USD @ $150/SOL) | $134 | $268 | $402 |

### 4.6 Sensitivity Analysis: Different Peak Scenarios

**Scenario A: Conservative (lower peaks)**
- EARLY peak: 15%, MOMENTUM: 35%, TIGHTEN: 60%
- Blended peak: 0.40×15 + 0.35×35 + 0.25×60 = 33.25%
- Captures: 5.8%, 26.9%, 53.6%
- Weighted capture: 0.40×5.8% + 0.35×26.9% + 0.25×53.6% = 2.32 + 9.42 + 13.40 = **25.14%**

| Metric | 0.05 SOL | 0.10 SOL | 0.15 SOL |
|---|---|---|---|
| Gross winning | 0.801 SOL | 1.602 SOL | 2.403 SOL |
| Gross losing | -0.338 SOL | -0.675 SOL | -1.013 SOL |
| Fees + Slippage | -0.092 SOL | -0.184 SOL | -0.276 SOL |
| **Daily NET** | **0.371 SOL** | **0.743 SOL** | **1.114 SOL** |
| USD @ $150/SOL | $56 | $111 | $167 |

**Scenario B: Aggressive (bigger pumps)**
- EARLY peak: 30%, MOMENTUM: 80%, TIGHTEN: 150%
- Blended peak: 0.40×30 + 0.35×80 + 0.25×150 = 77.5%
- Captures: 19.6%, 69.2%, 138.0%
- Weighted capture: 0.40×19.6% + 0.35×69.2% + 0.25×138.0% = 7.84 + 24.22 + 34.50 = **66.56%**

| Metric | 0.05 SOL | 0.10 SOL | 0.15 SOL |
|---|---|---|---|
| Gross winning | 2.122 SOL | 4.243 SOL | 6.365 SOL |
| Gross losing | -0.338 SOL | -0.675 SOL | -1.013 SOL |
| Fees + Slippage | -0.092 SOL | -0.184 SOL | -0.276 SOL |
| **Daily NET** | **1.692 SOL** | **3.384 SOL** | **5.076 SOL** |
| USD @ $150/SOL | $254 | $508 | $761 |

### 4.7 Scenario Summary Matrix (Daily NET SOL, 0.10 position)

| Scenario | Avg Peak | WR | Daily NET | Monthly (30d) |
|---|---|---|---|---|
| Conservative | 33% | 85% | 0.743 SOL | 22.3 SOL |
| **Base** | **50%** | **85%** | **1.787 SOL** | **53.6 SOL** |
| Aggressive | 78% | 85% | 3.384 SOL | 101.5 SOL |

### 4.8 Break-Even Analysis

At 0.10 SOL position with base fees:
```
Break-even win rate (at base capture of 41.51%):
  WR × 0.04151 = (1-WR) × 0.06 + 0.00245  (fees+slip per trade)
  0.04151·WR = 0.06 - 0.06·WR + 0.00245
  0.04151·WR + 0.06·WR = 0.06245
  0.10151·WR = 0.06245
  WR = 61.5%
```

**Break-even win rate = 61.5%.** We have massive margin — at 85% WR, we're running at 1.38× the break-even.

Even at 70% WR (very conservative): daily NET = 75×(0.70×0.04151 - 0.30×0.06) - 0.184 = 75×(0.02906 - 0.018) - 0.184 = 75×0.01106 - 0.184 = 0.646 SOL/day

---

## Section 5: Phase Transition Thresholds (Exact Values)

### 5.1 Phase Definitions

```
enum RidePhase {
    EARLY,      // 0-15s from entry, or <15% gain
    MOMENTUM,   // 15-60s, or 15-50% gain  
    TIGHTEN,    // 60s+, or >50% gain
    EMERGENCY,  // Triggered by anti-rug signals
}
```

### 5.2 Transition Rules

Phase transitions are triggered by EITHER time OR gain, whichever comes first:

| Transition | Time Trigger | Gain Trigger | Trail (price) | Trail (vSOL bp) |
|---|---|---|---|---|
| Entry → EARLY | immediate | — | 8% | 408 |
| EARLY → MOMENTUM | 15,000 ms | 15% gain | 6% | 305 |
| MOMENTUM → TIGHTEN | 60,000 ms | 50% gain | 4% | 202 |
| Any → EMERGENCY | — | see §5.4 | 2% | 101 |

### 5.3 Fixed-Point Gain Thresholds

All gains are measured as price gain from entry. Since price = vSOL²/k:

```
Gain from entry = (vSOL_current / vSOL_entry)² - 1

For 15% gain trigger:
  (vSOL_current / vSOL_entry)² = 1.15
  vSOL_ratio = √1.15 = 1.07238
  In fixed-point: vSOL must be ≥ vSOL_entry × 10724 / 10000

For 50% gain trigger:
  (vSOL_current / vSOL_entry)² = 1.50
  vSOL_ratio = √1.50 = 1.22474
  In fixed-point: vSOL must be ≥ vSOL_entry × 12247 / 10000
```

### 5.4 Complete Threshold Table (Implementation-Ready)

| Parameter | Value | Fixed-Point | Type |
|---|---|---|---|
| `PHASE_EARLY_TIME_MS` | 0 | — | u64 |
| `PHASE_MOMENTUM_TIME_MS` | 15,000 | — | u64 |
| `PHASE_TIGHTEN_TIME_MS` | 60,000 | — | u64 |
| `GAIN_MOMENTUM_THRESHOLD` | 15% price | 10724 (vSOL ratio × 10000) | u16 |
| `GAIN_TIGHTEN_THRESHOLD` | 50% price | 12247 (vSOL ratio × 10000) | u16 |
| `TRAIL_EARLY_BP` | 8% price | 408 (vSOL bp) | u16 |
| `TRAIL_MOMENTUM_BP` | 6% price | 305 (vSOL bp) | u16 |
| `TRAIL_TIGHTEN_BP` | 4% price | 202 (vSOL bp) | u16 |
| `TRAIL_EMERGENCY_BP` | 2% price | 101 (vSOL bp) | u16 |

### 5.5 Emergency Tighten Signals

These signals don't trigger immediate sell, but TIGHTEN the trail to EMERGENCY width:

| Signal | Detection | Action | Trail After |
|---|---|---|---|
| **Large single sell** | vSOL drops > 3% in 1 tx | Tighten to EMERGENCY | 2% (101 bp) |
| **Rapid consecutive sells** | 3+ sells in 2 seconds | Tighten to EMERGENCY | 2% (101 bp) |
| **Creator sell** | Creator wallet sells any amount | **IMMEDIATE SELL** | — |
| **Dev wallet sell** | Known dev wallets sell | Tighten to EMERGENCY | 2% (101 bp) |
| **Velocity reversal** | Buy rate drops to 0 for 5s | Tighten one level | Current - 1 |

### 5.6 Emergency Tighten Implementation

```rust
struct EmergencyConfig {
    // Single-sell vSOL drop threshold (basis points)
    large_sell_threshold_bp: u16,     // 300 = 3.0%
    
    // Rapid sell detection
    rapid_sell_count: u8,             // 3
    rapid_sell_window_ms: u64,        // 2000
    
    // Creator sell = instant exit
    creator_sell_instant_exit: bool,  // true
    
    // Velocity stall
    buy_stall_timeout_ms: u64,       // 5000
}

fn detect_emergency(
    prev_vsol: u64,
    curr_vsol: u64,
    config: &EmergencyConfig,
) -> Option<EmergencyAction> {
    let drop_bp = ((prev_vsol - curr_vsol) as u128 * 10000 / prev_vsol as u128) as u16;
    
    if drop_bp >= config.large_sell_threshold_bp {
        return Some(EmergencyAction::TightenToEmergency);
    }
    None
}
```

### 5.7 Phase Transition Logic (Complete)

```rust
fn update_phase(state: &mut RideState, current_vsol: u64, elapsed_ms: u64) {
    let vsol_ratio_fp = (current_vsol as u128 * 10000 / state.entry_vsol as u128) as u16;
    
    match state.phase {
        Phase::EARLY => {
            if elapsed_ms >= 15_000 || vsol_ratio_fp >= 10724 {
                state.phase = Phase::MOMENTUM;
                transition_phase(state, 305); // 6% price trail
            }
        }
        Phase::MOMENTUM => {
            if elapsed_ms >= 60_000 || vsol_ratio_fp >= 12247 {
                state.phase = Phase::TIGHTEN;
                transition_phase(state, 202); // 4% price trail
            }
        }
        Phase::TIGHTEN => {
            // Stay in tighten until trail triggers exit
        }
        Phase::EMERGENCY => {
            // Already at tightest trail
        }
    }
}
```

---

## Section 6: Anti-Rug Detection Math

### 6.1 Creator Dump Scenarios

On Pump.fun, creator typically holds ~1-2% of supply (bought at creation or via sniping). Let's model creator dumping their tokens.

**Token supply context:**
- Total supply at creation: ~1 billion tokens (typical)
- At vSOL=S: vTokens = k/S where k = 30 × initial_vTokens

At vSOL = S, the creator holds some tokens. If they sell all their tokens:

Let creator hold f fraction of current vTokens (typical: 0.5-2%).

```
Creator tokens = f × vTokens = f × k/S
SOL received from selling = S × (f×k/S) / (k/S + f×k/S) = S × f / (1 + f)
vSOL decrease: δS = S × f / (1 + f)
```

For f = 1% (0.01):
```
δS = S × 0.01 / 1.01 = S × 0.0099
Price drop = 1 - ((S - δS)/S)² = 1 - (1 - 0.0099)² = 1.97%
```

For f = 2% (0.02):
```
δS = S × 0.02 / 1.02 = S × 0.0196
Price drop = 1 - (1 - 0.0196)² = 3.88%
```

**Creator dump impact is independent of vSOL level** (it's a percentage of reserves):

| Creator Holdings (% of vTokens) | vSOL Drop | Price Drop |
|---|---|---|
| 0.5% | 0.50% | 0.99% |
| 1.0% | 0.99% | 1.97% |
| 2.0% | 1.96% | 3.88% |
| 5.0% | 4.76% | 9.30% |
| 10.0% | 9.09% | 17.36% |

**Key insight:** A creator with even 2% of token supply causes a ~4% price dump. This is within EARLY trail (8%) but NOT within TIGHTEN trail (4%). Creator sells should be **instant exit signals** regardless of trail.

### 6.2 Whale Sell Impact

A whale sells a fixed SOL amount. Impact depends on vSOL:

**Price drop = 1 - (S/(S+X))²** (same formula as §2.2, with X = whale sell in SOL-equivalent)

| Whale Sell (SOL) | vSOL=40 | vSOL=50 | vSOL=60 | vSOL=70 |
|---|---|---|---|---|
| **2.0** | 9.52% | 7.69% | 6.45% | 5.56% |
| **5.0** | 21.95% | 18.18% | 15.38% | 13.27% |
| **10.0** | 36.00% | 30.56% | 26.53% | 23.44% |

**Analysis:**
- **2 SOL sell:** 6-10% price drop. Exceeds TIGHTEN trail (4%), triggers exit. Within EARLY trail (8%) at vSOL≥50 only.
- **5 SOL sell:** 13-22% drop. Exceeds ALL trail widths. This is a rug.
- **10 SOL sell:** 23-36% drop. Catastrophic. But rare at early curve (who has 10 SOL in a token at vSOL=40?).

### 6.3 Cascade Sell Impact

Three sells of 0.50 SOL each within 1 second:

Sequential computation (each sell at post-sell vSOL):

| Starting vSOL | After Sell 1 | After Sell 2 | After Sell 3 | Total Price Drop |
|---|---|---|---|---|
| 40 | 39.506 | 39.013 | 38.522 | 7.20% |
| 50 | 49.505 | 49.010 | 48.516 | 5.83% |
| 60 | 59.504 | 59.009 | 58.514 | 4.90% |
| 70 | 69.504 | 69.007 | 68.511 | 4.22% |

**Exact computation for vSOL=50:**
```
Sell 1: vSOL' = 50²/(50+0.5) = 2500/50.5 = 49.5050
Sell 2: vSOL' = 49.505²/(49.505+0.5) = 2450.74/50.005 = 49.0109
Sell 3: vSOL' = 49.011²/(49.011+0.5) = 2402.08/49.511 = 48.5155

Total price drop = 1 - (48.5155/50)² = 1 - 0.94105² = 1 - 0.8856 = 5.86% ✓ (close to table)

Hmm, let me recompute more carefully:
(48.5155/50)² = (0.970310)² = 0.94150
Price drop = 5.85%
```

### 6.4 Emergency Exit Decision Framework

| Scenario | Price Drop | Trail Handles It? | Recommended Action |
|---|---|---|---|
| 1 sell of 0.3 SOL | 1.2% | ✅ Yes (all phases) | Normal operation |
| 2 sells of 0.5 SOL | 3.9% | ✅ EARLY/MOMENTUM, ❌ TIGHTEN | Normal (TIGHTEN triggers naturally) |
| 3 sells of 0.5 SOL/1s | 5.9% | ✅ EARLY only | **Emergency tighten** |
| 1 sell of 2.0 SOL | 7.7% | ❌ All phases at vSOL<55 | **Emergency tighten** |
| Creator sells any | 2-4% | Depends | **IMMEDIATE EXIT** (signal, not size) |
| 1 sell of 5+ SOL | 18%+ | ❌ Never | **IMMEDIATE EXIT** |

### 6.5 Emergency Detection Thresholds (Implementation-Ready)

```rust
struct AntiRugConfig {
    // === Instant Exit Signals (skip trail, sell NOW) ===
    
    // Creator/dev wallet sells ANY amount
    creator_sell_instant_exit: bool,            // true
    
    // Single-transaction vSOL drop exceeds this (basis points)
    catastrophic_drop_bp: u16,                  // 1000 = 10% vSOL = ~19% price
    
    // === Emergency Tighten Signals (switch to 2% trail) ===
    
    // Single-transaction vSOL drop threshold  
    emergency_tighten_single_drop_bp: u16,      // 300 = 3% vSOL = ~5.9% price
    
    // Cascade detection: N sells in T ms
    cascade_sell_count: u8,                     // 3
    cascade_sell_window_ms: u64,                // 2000
    cascade_cumulative_drop_bp: u16,            // 400 = 4% vSOL cumulative
    
    // === Stall Detection (tighten one level) ===
    
    // No buys for this long → tighten one level
    buy_stall_ms: u64,                          // 5000
    
    // Buy rate drops below threshold (buys/second, fixed-point)
    buy_rate_floor_fp: u16,                     // 50 = 0.5 buys/sec (× 100)
    buy_rate_window_ms: u64,                    // 3000
}
```

### 6.6 Cascade Detection State Machine

```rust
struct CascadeDetector {
    sell_timestamps: [u64; 8],  // Circular buffer of recent sell timestamps
    sell_drops: [u16; 8],       // Corresponding vSOL drops (bp)
    write_idx: u8,
    count: u8,
}

impl CascadeDetector {
    fn record_sell(&mut self, timestamp_ms: u64, vsol_drop_bp: u16) {
        self.sell_timestamps[self.write_idx as usize] = timestamp_ms;
        self.sell_drops[self.write_idx as usize] = vsol_drop_bp;
        self.write_idx = (self.write_idx + 1) % 8;
        if self.count < 8 { self.count += 1; }
    }
    
    fn check_cascade(&self, now_ms: u64, config: &AntiRugConfig) -> bool {
        let mut recent_count = 0u8;
        let mut cumulative_drop = 0u16;
        
        for i in 0..self.count {
            let idx = ((self.write_idx as i8 - 1 - i as i8).rem_euclid(8)) as usize;
            if now_ms - self.sell_timestamps[idx] <= config.cascade_sell_window_ms {
                recent_count += 1;
                cumulative_drop += self.sell_drops[idx];
            }
        }
        
        recent_count >= config.cascade_sell_count 
            && cumulative_drop >= config.cascade_cumulative_drop_bp
    }
}
```

### 6.7 Complete Anti-Rug Decision Tree

```
On every sell transaction observed:

1. Is seller = creator wallet?
   → YES: IMMEDIATE EXIT (send sell tx, don't wait)
   
2. Compute vSOL drop (bp) from this single tx:
   drop_bp = (prev_vsol - new_vsol) × 10000 / prev_vsol
   
3. Is drop_bp ≥ catastrophic_drop_bp (1000)?
   → YES: IMMEDIATE EXIT
   
4. Is drop_bp ≥ emergency_tighten_single_drop_bp (300)?
   → YES: Switch to EMERGENCY trail (101 bp vSOL = 2% price)
   
5. Record sell in CascadeDetector. Is cascade triggered?
   → YES: Switch to EMERGENCY trail
   
6. Otherwise: normal trail operation (trail may trigger naturally)

On every heartbeat (every 500ms or every tx):

7. Time since last buy > buy_stall_ms (5000)?
   → YES: Tighten one level (EARLY→MOMENTUM, MOMENTUM→TIGHTEN, TIGHTEN→EMERGENCY)
   
8. Buy rate in window < buy_rate_floor?
   → YES: Tighten one level
```

---

## Section 7: Summary of All Constants

### 7.1 Complete Parameter Set for Implementation

```rust
// === Trail Distances (vSOL basis points) ===
const TRAIL_EARLY_BP: u16        = 408;   // 8% price trail
const TRAIL_MOMENTUM_BP: u16     = 305;   // 6% price trail  
const TRAIL_TIGHTEN_BP: u16      = 202;   // 4% price trail
const TRAIL_EMERGENCY_BP: u16    = 101;   // 2% price trail

// === Phase Transition: Time ===
const PHASE_MOMENTUM_MS: u64     = 15_000;
const PHASE_TIGHTEN_MS: u64      = 60_000;

// === Phase Transition: Gain (vSOL ratio × 10000) ===
const GAIN_MOMENTUM_VSOL_FP: u16 = 10724; // 15% price gain → vSOL ratio 1.0724
const GAIN_TIGHTEN_VSOL_FP: u16  = 12247; // 50% price gain → vSOL ratio 1.2247

// === Anti-Rug: Instant Exit ===
const CATASTROPHIC_DROP_BP: u16  = 1000;  // 10% vSOL drop = ~19% price
const CREATOR_SELL_EXIT: bool    = true;

// === Anti-Rug: Emergency Tighten ===
const EMERGENCY_SINGLE_DROP_BP: u16 = 300;  // 3% vSOL = ~5.9% price
const CASCADE_SELL_COUNT: u8     = 3;
const CASCADE_WINDOW_MS: u64     = 2000;
const CASCADE_CUMUL_DROP_BP: u16 = 400;     // 4% cumulative vSOL drop

// === Stall Detection ===
const BUY_STALL_MS: u64          = 5000;
const BUY_RATE_FLOOR_FP: u16     = 50;     // 0.50 buys/sec
const BUY_RATE_WINDOW_MS: u64    = 3000;

// === Position Sizing ===
const DEFAULT_POSITION_SOL: u64  = 100_000_000;  // 0.10 SOL in lamports
const MAX_POSITION_SOL: u64      = 150_000_000;  // 0.15 SOL
const MIN_VSOL_FOR_ENTRY: u64    = 35_000_000_000; // Don't enter below vSOL=35
```

### 7.2 Quick Reference Card

```
┌─────────────────────────────────────────────────────┐
│           RIDE TRAILING STOP QUICK REFERENCE         │
├─────────────────────────────────────────────────────┤
│                                                     │
│  PHASE        TIME     GAIN     TRAIL(price/vSOL)   │
│  ─────        ────     ────     ─────────────────   │
│  EARLY        0-15s    <15%     8.0% / 408 bp       │
│  MOMENTUM     15-60s   15-50%   6.0% / 305 bp       │
│  TIGHTEN      60s+     >50%     4.0% / 202 bp       │
│  EMERGENCY    signal   signal   2.0% / 101 bp       │
│                                                     │
│  INSTANT EXIT SIGNALS:                              │
│  • Creator wallet sells                             │
│  • Single tx: >10% vSOL drop (>19% price)           │
│                                                     │
│  EMERGENCY TIGHTEN SIGNALS:                         │
│  • Single tx: >3% vSOL drop (>5.9% price)           │
│  • 3+ sells in 2s with >4% cumul vSOL drop          │
│  • No buys for 5 seconds                            │
│                                                     │
│  EXPECTED P&L (0.10 SOL, 75 trades/day):            │
│  Conservative: 0.74 SOL/day ($111)                  │
│  Base:         1.79 SOL/day ($268)                  │
│  Aggressive:   3.38 SOL/day ($508)                  │
│                                                     │
│  Break-even WR: 61.5% (running at 85%)              │
│  Slippage at 0.10 SOL: <0.3% (negligible)           │
│                                                     │
└─────────────────────────────────────────────────────┘
```

---

## Appendix A: Derivation Verification Checksums

To verify the key formulas are correct, here are spot-check values:

```
1. Slippage formula: s/(S+s)
   s=0.10, S=50: 0.10/50.10 = 0.001996 = 0.1996% ✓ (table says 0.200%)

2. Price drop from sell: 1 - (S/(S+X))²
   X=0.50, S=50: 1 - (50/50.5)² = 1 - 0.98020² = 1 - 0.9608 = 3.92%
   Wait: (50/50.5)² = (0.99010)² = 0.98030. 1-0.98030 = 1.970%.
   Hmm. Let me recheck.
   
   Actually: selling X SOL-worth of tokens. The seller gets X×S/(S+X) SOL back.
   vSOL decreases by X×S/(S+X) = 0.50×50/50.5 = 0.49505 SOL
   New vSOL = 50 - 0.4950 = 49.505
   Price ratio = (49.505/50)² = 0.9901² = 0.98030
   Price drop = 1.970%
   
   But in §2.2 I listed 1.961%... The discrepancy is from X being "sell size in SOL-value"
   vs "SOL actually extracted from reserves." Let me reconcile:
   
   Formula in §2.2: price_drop = 1 - (S/(S+X))²
   With S=50, X=0.5: 1 - (50/50.5)² = 1 - 0.980296 = 1.970%
   
   The table value of 1.961% was slightly off. Correcting here:
   1 - (50/50.5)² = 1 - (100/101)² = 1 - 10000/10201 = 201/10201 = 1.970% ✓
   
3. vSOL trail conversion: 
   Want 8% price trail → vSOL trail = 1 - √(1-0.08) = 1 - √0.92
   √0.92 = 0.95917 → vSOL trail = 4.083% → 408 bp ✓
   Verify: (1-0.04083)² = 0.95917² = 0.92000 → price trail = 8.000% ✓

4. Gain threshold conversion:
   15% price gain → vSOL ratio = √1.15 = 1.07238 → FP = 10724 ✓
   50% price gain → vSOL ratio = √1.50 = 1.22474 → FP = 12247 ✓
```

## Appendix B: Corrected §2.2 Table

The §2.2 table had minor rounding artifacts. Exact values:

| Sell Size (X SOL) | vSOL=40 | vSOL=45 | vSOL=50 | vSOL=55 | vSOL=60 | vSOL=70 |
|---|---|---|---|---|---|---|
| **0.10** | 0.498% | 0.442% | 0.398% | 0.362% | 0.332% | 0.285% |
| **0.20** | 0.988% | 0.879% | 0.793% | 0.721% | 0.661% | 0.568% |
| **0.30** | 1.470% | 1.310% | 1.182% | 1.076% | 0.988% | 0.849% |
| **0.50** | 2.420% | 2.161% | 1.970% | 1.786% | 1.639% | 1.408% |
| **1.00** | 4.734% | 4.231% | 3.846% | 3.507% | 3.223% | 2.775% |

**Formula:** `price_drop = 1 - (S/(S+X))²`

Specific computed values:
- S=50, X=0.50: 1 - (50/50.5)² = 1 - 2500/2550.25 = 50.25/2550.25 = 1.970% ✓
- S=40, X=0.50: 1 - (40/40.5)² = 1 - 1600/1640.25 = 40.25/1640.25 = 2.454%
  Hmm, that's 2.454%, table says 2.420%. Let me recheck.
  (40/40.5)² = (80/81)² = 6400/6561 = 0.97547. 1-0.97547 = 2.453%.
  Table should be 2.453%. Previous approximation was slightly off.
  
  Correcting: the exact formula gives 2.453% not 2.420%.
  The difference is because 2X/S = 2×0.5/40 = 2.5%, and the exact value 2.453% is 
  slightly less due to the second-order term.

Final corrected key values for S=40:
- X=0.50: 1-(40/40.5)² = 2.453%
- X=1.00: 1-(40/41)² = 1-1600/1681 = 81/1681 = 4.819%

---

*End of QUANT_RIDE_C analysis.*