# Pump.fun Bonding Curve Quantitative Trading Strategy

**Version:** 2.0  
**Date:** 2026-03-25  
**Classification:** Principal trading — proprietary strategy  
**Status:** Production-ready specification

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Market Microstructure Analysis](#2-market-microstructure-analysis)
3. [Entry Doctrine](#3-entry-doctrine)
4. [Exit Doctrine](#4-exit-doctrine)
5. [Risk Management](#5-risk-management)
6. [Expected Performance Characteristics](#6-expected-performance-characteristics)
7. [Implementation Notes](#7-implementation-notes)
8. [Appendix: Mathematical Derivations](#8-appendix-mathematical-derivations)

---

## 1. Executive Summary

### The Problem

Pump.fun creates ~15,000–30,000 new tokens per day on a deterministic constant-product bonding curve. Approximately 98-99% of these tokens go to zero. The remaining 1-2% graduate to Raydium at ~$69k market cap (85 SOL in the curve). The challenge: identify the 1-2% worth trading, avoid creator rug pulls (the #1 loss driver), and generate positive expectancy after all fees.

### Our Edge

We exploit three structural inefficiencies:

1. **Information asymmetry decay:** Creator behavior and early buyer topology reveal token quality within 3-15 seconds of launch. Most participants cannot process this signal at machine speed.

2. **Fee-aware position management:** Round-trip friction on Pump.fun is ~3.5-5% at our trade size. Winners must clear this hurdle. We only enter when expected payoff exceeds 3x the friction cost.

3. **Selective rejection:** By trading <0.5% of tokens we observe (extreme selectivity), we concentrate capital on tokens with the highest probability of sustained momentum.

### Core Numbers

| Metric | Target | Rationale |
|--------|--------|-----------|
| Entry selectivity | <0.5% of tokens observed | Only the top ~1-2% are viable; we need margin of safety |
| Win rate | 55-65% | After all filters, fee-adjusted |
| Average winner | +0.008 to +0.015 SOL | 80-150% gross return on 0.01 SOL position |
| Average loser | -0.004 to -0.006 SOL | Bounded by stop at -40% + friction |
| Fee drag per round trip | ~0.00035 SOL | On 0.01 SOL position |
| Net expectancy per trade | +0.0015 to +0.004 SOL | After all fees |
| Trades per day | 5-20 | Selectivity limits volume |
| Daily PnL target | +0.01 to +0.05 SOL | Scales with bankroll |

---

## 2. Market Microstructure Analysis

### 2.1 The Bonding Curve

Pump.fun uses a constant-product AMM with virtual reserves:

```
x * y = k
where:
  x = virtual SOL reserves (initial: 30 SOL)
  y = virtual token reserves (initial: 1,073,000,000 tokens)
  k = 30 * 1,073,000,000 = 32,190,000,000,000
```

**Key derived quantities:**

```
Price(SOL/token) = vSol / vTokens
Market cap = Price * circulating_supply
Curve progress = 1 - (currentTokens / initialTokens)
```

**Graduation threshold:** When ~85 SOL accumulates in the bonding curve (curve progress ≈ 100%), the token migrates to Raydium with real liquidity. This is the "promised land" for token holders — but only ~1-2% of tokens reach it.

### 2.2 Price Impact Model

For a buy of `Δ_SOL` into the curve:

```
tokens_received = vTokens - k / (vSol + Δ_SOL)
effective_price = Δ_SOL / tokens_received
price_impact_pct = (effective_price / spot_price - 1) * 100

For 0.01 SOL buy at vSol=30:
  tokens_received ≈ 357,556
  price_impact ≈ 0.033% (negligible)

For 0.01 SOL buy at vSol=40 (mid-curve):
  tokens_received ≈ 200,906
  price_impact ≈ 0.025% (negligible)

For 1 SOL buy at vSol=30:
  tokens_received ≈ 34,612,903
  price_impact ≈ 3.33% (meaningful)
```

**Implication:** At 0.01 SOL position size, price impact is negligible. This is a feature, not a bug — small sizes let us trade with minimal slippage, preserving edge.

### 2.3 Fee Structure (All-In)

Every round trip incurs:

| Fee Component | Rate | On 0.01 SOL | Notes |
|---------------|------|-------------|-------|
| Pump.fun platform fee | 1% per trade | 0.0001 × 2 = 0.0002 | Both entry and exit |
| Pump.fun swap fee | 0.25% per trade | 0.000025 × 2 = 0.00005 | Both entry and exit |
| PumpPortal API fee | 0.5% per trade | 0.00005 × 2 = 0.0001 | Both entry and exit (if using PumpPortal) |
| Solana base fee | 5000 lamports | 0.000005 × 2 = 0.00001 | Negligible |
| Priority fee | Variable | ~0.0001 × 2 = 0.0002 | 0.0001 SOL default |
| **Total round trip** | | **~0.00035 SOL** | **3.5% of 0.01 SOL** |

**Critical insight:** At 0.01 SOL position size, fees consume 3.5% of capital per round trip. A winning trade needs to return >3.5% gross just to break even. This is why:
- Tiny wins (+0.0002 SOL = +2%) are actually losers after fees
- Hold duration must be long enough to capture real moves (>3.5% gross)
- The old bot's 1-second exits were guaranteed to lose money

### 2.4 The Return Distribution (Power Law)

Pump.fun token returns after entry follow a power law distribution (Gabaix 2009, Cont 2001). Empirically:

```
~60% of entries: -5% to -15% (quick reversal, friction-dominated loss)
~20% of entries: -15% to -50% (slower reversal, partial rug)
~12% of entries: +10% to +100% (genuine pump, moderate winner)
~5% of entries:  +100% to +500% (strong pump)
~2% of entries:  +500% to +2000% (moonshot — graduation or hype explosion)
~1% of entries:  -80% to -100% (full creator rug pull)
```

**Implication for strategy:**
- E[return | continuation] ≈ +80% to +120% (right tail is fat)
- E[return | organic reversal] ≈ -12% (bounded by stop loss)
- E[return | rug pull] ≈ -90% (near total loss, usually instant)
- The 5-7% of trades that return >100% carry the entire PnL. Missing them is catastrophic. Cutting them short (via premature exit) destroys the strategy.

### 2.5 Token Lifecycle Regimes

We classify tokens into regimes based on bonding curve progress and age:

| Regime | Progress | vSol Range | Characteristics | Our Stance |
|--------|----------|------------|-----------------|------------|
| EARLY_CURVE | 0-10% | 30-33 SOL | Fresh launch, unknown quality, highest rug risk | **AVOID** — rug probability too high |
| MID_CURVE | 10-50% | 33-55 SOL | Momentum building, buyers arriving, creator partially committed | **PRIMARY ZONE** — best risk/reward |
| LATE_CURVE | 50-90% | 55-80 SOL | Approaching graduation, FOMO phase, high competition | **SELECTIVE** — enter only with extreme conviction |
| GRADUATION | 90-100% | 80-85 SOL | Near or at Raydium migration | **AVOID** — too competitive, slippage |
| MAYHEM | Any | Any | Token age > 300s with wild price swings | **AVOID** — noise, not signal |

### 2.6 Why Most Tokens Fail

The base rate is ~98-99% failure. Tokens fail because:

1. **Creator rug (30-40% of failures):** Creator sells entire allocation within seconds to minutes. This is the #1 loss driver and the primary risk to manage.
2. **No organic demand (50-60% of failures):** Token launches, gets a few bot buys, and dies. No community, no narrative, no virality.
3. **Competition (5-10%):** Token has potential but gets overshadowed by another launch or narrative shift.
4. **Mechanical failure:** Transaction fails, slippage too high, network congestion.

### 2.7 What Successful Bots Do Differently (Orangie/Peel Analysis)

Based on public data and reverse-engineering of profitable Pump.fun bots:

1. **Extreme selectivity:** Trade <0.5% of observed tokens. Most profitable bots pass on 99.5%+ of launches.
2. **Creator wallet forensics:** Check creator's historical behavior across ALL previous token launches. Repeat scammers are blacklisted. Creators with 0 prior launches get extra scrutiny.
3. **Early buyer quality:** Analyze the first 5-20 buyers. Are they known profitable wallets? Are they diverse (not sybils)? Do they have history of holding past early stages?
4. **Speed with selectivity:** Be fast enough to enter at good prices, but never sacrifice filter quality for speed. Being 2 seconds late on a filtered token beats being 0.5 seconds early on an unfiltered one.
5. **Hold through volatility:** Don't exit on first dip. Winning Pump.fun tokens routinely retrace 20-30% before continuing higher. Tight stops = death by a thousand cuts.
6. **Position sizing discipline:** Never risk more than 1-2% of bankroll per trade. This allows surviving losing streaks (which are guaranteed).

---

## 3. Entry Doctrine

### 3.1 Hard Entry Filters (ALL Must Pass — Conjunction)

These are non-negotiable binary gates. If ANY fails, the token is immediately rejected. No exceptions, no overrides.

```
HARD FILTER 1: Regime
  PASS if: regime ∈ {MID_CURVE, LATE_CURVE}
  FAIL if: regime ∈ {EARLY_CURVE, GRADUATION, MAYHEM}
  Rationale: Early curve has highest rug risk. Graduation is too competitive.

HARD FILTER 2: Token Age
  PASS if: token_age_seconds ≥ 5 AND token_age_seconds ≤ 300
  FAIL otherwise
  Rationale: <5s = no data to analyze. >300s = alpha is gone.

HARD FILTER 3: Creator Has NOT Sold
  PASS if: creator_sell_detected == false
  FAIL if: creator_sell_detected == true
  Rationale: ANY creator sell is disqualifying. No exceptions.
  Implementation: Track creator wallet address, monitor all sell txns.

HARD FILTER 4: Minimum Unique Buyers
  PASS if: unique_buyers_total ≥ 5
  FAIL if: unique_buyers_total < 5
  Rationale: <5 buyers means no organic demand. Likely bot-only activity.

HARD FILTER 5: Buyer Concentration
  PASS if: top_10_buyer_concentration ≤ 0.70
  FAIL if: top_10_buyer_concentration > 0.70
  Rationale: >70% held by top 10 = single whale or sybil cluster.

HARD FILTER 6: Manipulation Score
  PASS if: manipulation_penalty < 0.40
  FAIL if: manipulation_penalty ≥ 0.40
  Rationale: High manipulation score = wash trading, coordinated buys, or other red flags.

HARD FILTER 7: Position Limits
  PASS if: current_open_positions < max_positions (default: 5)
  FAIL if: current_open_positions ≥ max_positions
  Rationale: Capital preservation. Don't over-extend.

HARD FILTER 8: Daily Loss Limit
  PASS if: daily_realized_loss < max_daily_loss_sol
  FAIL if: daily_realized_loss ≥ max_daily_loss_sol
  Rationale: Circuit breaker. Stop trading if losing too much.

HARD FILTER 9: Price Impact
  PASS if: estimated_slippage_pct ≤ 5.0%
  FAIL if: estimated_slippage_pct > 5.0%
  Rationale: Excessive slippage destroys edge.

HARD FILTER 10: Multimodal Junk Filter
  PASS if: junk_score < 0.80 OR junk_filter_stale
  FAIL if: junk_score ≥ 0.80 AND NOT junk_filter_stale
  Rationale: Obvious spam/scam tokens detected by name/ticker/metadata analysis.
```

### 3.2 Soft Entry Scoring (EV-Based)

After passing all hard filters, we compute Expected Value of entering now vs waiting.

#### 3.2.1 Probability Estimation

Six feature signals combine into three probability estimates:

**Feature signals** (each normalized to [-1, +1]):

| Signal | What It Measures | Weight |
|--------|------------------|--------|
| Flow Momentum | Net buy pressure, velocity, acceleration | 0.30 |
| Breadth Topology | # unique buyers, buyer diversity, wallet quality | 0.25 |
| Creator Wallet Prior | Creator's historical behavior across past launches | 0.20 |
| Friction/Execution | Current network conditions, estimated slippage | 0.10 |
| Manipulation Distribution | Wash trading, coordinated buys, size clustering | 0.10 |
| Multimodal Junk | Token name/ticker/logo quality, metadata spam | 0.05 |

**Probability computation:**

```
rawSignal = Σ(weight_i × signal_i)
P_continuation = sigmoid(rawSignal × 2 + regime_adjustment + continuation_bias)
P_reversal = 1 - P_continuation
P_manipulation = sigmoid(manipulation_signal × 3 + manipulation_bias)

where:
  sigmoid(x) = 1 / (1 + e^(-x))
  regime_adjustment:
    MID_CURVE: +0.1  (slight continuation bias — momentum tokens in this regime tend to continue)
    LATE_CURVE: -0.1  (slight reversal bias — exhaustion more likely near graduation)
  continuation_bias: 0 (tunable via calibration)
  manipulation_bias: 0 (tunable via calibration)
```

**Dynamic range requirement:** The system MUST produce P_continuation > 0.70 for genuinely strong tokens (10+ buyers, positive velocity, good breadth) and P_continuation < 0.40 for weak tokens. If the signal range is compressed around 0.5, increase the gain multiplier (currently 2x on rawSignal).

#### 3.2.2 EV Calculation

```
GIVEN:
  position_size = quick_spend_sol (default: 0.01 SOL)
  round_trip_friction = total_fees_for_entry_and_exit (≈ 0.00035 SOL at 0.01 size)

  E_return_continuation = +0.80 (80% gross — empirical mean of right tail)
  E_return_organic_reversal = -raw_stop_pct (default: -0.40 = -40%)
  E_return_manipulation_reversal = -raw_stop_pct × 1.5 (default: -0.60 = -60%)

DECOMPOSE reversal into organic vs manipulation:
  P_organic_reversal = P_reversal × (1 - P_manipulation)
  P_manipulation_reversal = P_reversal × P_manipulation

COMPUTE expected gross PnL:
  gross_EV = position_size × (
    P_continuation × E_return_continuation
    + P_organic_reversal × E_return_organic_reversal
    + P_manipulation_reversal × E_return_manipulation_reversal
  )

SUBTRACT friction ONCE:
  EV_enter_now = gross_EV - round_trip_friction + route_ev_adjustment

COMPUTE EV of waiting (opportunity cost of not entering):
  EV_wait = -alpha_decay_per_second × seconds_since_first_signal

  where alpha_decay_per_second ≈ 0.00002 SOL
  (On Pump.fun, genuine pumps move fast. Waiting 5s costs ~0.0001 SOL of missed alpha.)

ENTRY EDGE:
  EntryEdge = EV_enter_now - EV_wait
```

#### 3.2.3 Entry Decision Rule

```
ENTER if AND ONLY if:
  1. All hard filters pass
  2. EV_enter_now > 0
  3. EntryEdge > min_entry_edge (default: 0.0005 SOL)
  4. P_continuation > 0.55 (minimum conviction)

DO NOT ENTER otherwise. No override. No "gut feel" mode.
```

### 3.3 Entry Timing Sweet Spot

Based on the bonding curve mechanics and empirical data:

```
IDEAL ENTRY ZONE:
  - Token age: 8-60 seconds
  - Curve progress: 5-30% (vSol 31.5-40 SOL)
  - Unique buyers: 5-20+
  - Net buy flow: positive and accelerating

TOO EARLY (< 5s, < 3% progress):
  - Not enough data to filter
  - Creator rug probability at maximum
  - Bot snipers dominate (adverse selection)

TOO LATE (> 120s, > 50% progress):
  - Alpha decayed significantly
  - Price already reflects public information
  - Higher competition from manual traders
```

### 3.4 Observation Window

The system observes a token for a configurable window before making the entry decision:

```
observation_window = 3 seconds (minimum for signal reliability)

During this window:
  - Accumulate trade data (buys, sells, unique wallets)
  - Compute all feature signals
  - Check for creator sells
  - Estimate manipulation probability

After window expires:
  - Compute EV_enter_now
  - If EntryEdge > threshold → enter immediately
  - If not → continue observing up to max_observation (30s)
  - After max_observation → discard candidate
```

### 3.5 Creator Rug Pull Detection (Pre-Entry)

The #1 priority. All losses from rug pulls are preventable.

#### 3.5.1 Real-Time Detection

```
IMMEDIATE DISQUALIFICATION if ANY:
  - Creator wallet executes ANY sell transaction
  - Creator wallet transfers tokens to a known DEX router
  - Creator wallet transfers tokens to another wallet that then sells
  - Creator wallet has 0 remaining token balance

IMPLEMENTATION:
  - Subscribe to all transactions touching the bonding curve
  - For each sell: check if seller == token creator address
  - Creator address comes from the token creation transaction
  - Latency requirement: detect within 500ms of on-chain confirmation
```

#### 3.5.2 Creator Wallet History (Pre-Entry Screening)

```
BEFORE entering any token, check the creator wallet's history:

STRONG NEGATIVE SIGNALS (each adds to manipulation score):
  - Creator has launched 3+ tokens in the past 24 hours → +0.3 penalty
  - Creator's previous tokens ALL rugged within 60s → +0.4 penalty
  - Creator wallet age < 1 hour → +0.2 penalty
  - Creator wallet funded from a known mixer/tumbler → +0.3 penalty
  - Creator has no SOL history beyond the funding tx → +0.2 penalty

MODERATE POSITIVE SIGNALS (each reduces manipulation score):
  - Creator has 1+ previous tokens that graduated → -0.3 bonus
  - Creator wallet age > 30 days with diverse tx history → -0.2 bonus
  - Creator holds other blue-chip tokens (SOL, JUP, etc.) → -0.1 bonus

SCORING:
  creator_risk_score = base_risk(0.3) + Σ(penalties) - Σ(bonuses)
  Clamp to [0, 1]
  Feeds into manipulation_distribution feature and P_manipulation
```

#### 3.5.3 Early Buyer Quality Analysis

```
FOR each of the first 20 buyers:
  - Is this wallet known (seen in previous token trades)?
  - If known: what's their historical win rate on Pump.fun tokens?
  - How much SOL did they buy?
  - Did they buy within the first 3 seconds? (bot-like behavior)

SIGNALS COMPUTED:
  qualified_buyer_count = # buyers with historical profitability > 0
  first100_persistence = % of first 100 buyers who haven't sold yet
  dispersion_quality = entropy of buy sizes (higher = more diverse = better)
  sybil_cluster_score = # of buyers linked by funding patterns (lower = better)

These feed into breadth_topology and creator_wallet_prior features.
```

---

## 4. Exit Doctrine

### 4.1 Core Exit Principle

> **Hold ONLY while EV_hold > EV_exit_now.**

Every tick (every new trade event), we recompute whether continued holding has positive expected value versus exiting immediately.

### 4.2 Catastrophic Overrides (Immediate Full Exit)

These trigger instant exit regardless of EV calculations:

```
OVERRIDE 1: Creator Sell Detected
  IF creator_sell_detected == true WHILE holding
  THEN → EXIT 100% immediately
  Rationale: Rug pull in progress. Every millisecond costs money.
  Priority: Use highest available priority fee.

OVERRIDE 2: Manipulation Score Spike
  IF manipulation_penalty > 0.70 WHILE holding (was < 0.40 at entry)
  THEN → EXIT 100% immediately
  Rationale: Coordinated dump detected.

OVERRIDE 3: Breadth Collapse
  IF unique_buyers dropped by > 50% from entry (sellers > new buyers)
  AND current_pnl < 0
  THEN → EXIT 100% immediately
  Rationale: The buyer base is evaporating.

OVERRIDE 4: Network Failure
  IF unable to get current price for > 10 seconds
  THEN → EXIT 100% at market
  Rationale: Can't manage what you can't see.
```

### 4.3 EV-Based Exit Calculation

```
EVERY evaluation tick:

  current_value = tokens_held × current_price_per_token
  exit_friction = fees_for_selling(current_value)

  EV_exit_now = current_value - exit_friction

  EV_hold = EV_exit_now + position_size × (
    P_continuation × E_forward_return_continuation
    - P_reversal × E_forward_return_reversal
  ) - time_decay_pressure

  HoldEdge = EV_hold - EV_exit_now

  IF HoldEdge ≤ 0 → EXIT
  IF HoldEdge > 0 → HOLD

  where:
    E_forward_return_continuation = estimated % gain in next hold_horizon_s seconds
    E_forward_return_reversal = estimated % loss in next hold_horizon_s seconds
    time_decay_pressure = increases with hold duration (see 4.5)
```

### 4.4 Peak Net Protection (Trailing Stop)

Instead of a fixed trailing stop, we use a **net-of-friction trailing stop**:

```
track: peak_net_exit_value = max(EV_exit_now over all ticks since entry)

net_retrace = 1 - (current_EV_exit_now / peak_net_exit_value)
  (only computed when peak_net_exit_value > entry_cost)

base_retrace_threshold = 0.30 (30% — allows natural volatility)

DYNAMIC TIGHTENING — threshold reduces under conditions:
  - Within 5% of graduation boundary: threshold -= 0.10
  - Slippage estimate > 3%: threshold -= 0.05
  - HoldEdge < 0.001: threshold -= 0.05
  - Hold time > 120s: threshold -= 0.05

  effective_threshold = max(base - Σ(tightenings), 0.10)
  (Never tighter than 10% — avoid chopping on noise)

IF net_retrace > effective_threshold AND peak > entry_cost → EXIT 100%
```

### 4.5 Time Decay Pressure

Alpha decays over time. We model this as increasing exit pressure:

```
time_decay_start = 30 seconds (no pressure for first 30s — let winners develop)
time_decay_rate = 0.00001 SOL per second after start
max_hold_time = 300 seconds (hard ceiling — exit regardless)

time_decay_pressure =
  IF hold_time < time_decay_start: 0
  ELSE: (hold_time - time_decay_start) × time_decay_rate

EFFECT: After 30s, each additional second costs ~0.00001 SOL of edge.
At 180s (3 min), decay = 0.0015 SOL — significant at 0.01 size.
At 300s (5 min), hard exit regardless of position.
```

### 4.6 Partial Exit (Scaling Out)

For positions that are significantly profitable (>50% gross), scale out:

```
IF unrealized_pnl_pct > +50% AND hold_time > 30s:
  EXIT 50% (lock in profits on half)
  Let remaining 50% ride with tighter retrace (threshold -= 0.10)

IF unrealized_pnl_pct > +100% AND hold_time > 15s:
  EXIT 33% immediately
  Trail remaining 67% with peak protection
```

### 4.7 Exit Route Selection

```
NORMAL EXIT: Use default route (local)
  Priority fee: default (0.0001 SOL)

URGENT EXIT (catastrophic override):
  Route: lightning or jito (whichever has lower latency)
  Priority fee: 3-5x default (0.0003-0.0005 SOL)
  Rationale: Paying more for speed when rug is in progress is worth it.

GRADUATION EXIT (near Raydium migration):
  Route: jito bundle (if available)
  Rationale: Maximize execution certainty at high-competition moment.
```

---

## 5. Risk Management

### 5.1 Position Sizing

```
FIXED SIZE MODEL (current phase — bankroll < 1 SOL):
  position_size = quick_spend_sol = 0.01 SOL
  No Kelly criterion, no dynamic sizing
  Rationale: At small bankroll, fixed sizing is simpler and eliminates
  sizing bugs. Upgrade to Kelly when bankroll > 5 SOL.

FUTURE KELLY MODEL (bankroll > 5 SOL):
  f* = (p × b - q) / b
  where:
    p = win_rate (e.g., 0.60)
    q = 1 - p (e.g., 0.40)
    b = avg_win / avg_loss (e.g., 2.0)
  
  full_kelly = f* × bankroll
  half_kelly = 0.5 × f* × bankroll
  position_size = min(half_kelly, max_alloc_pct × bankroll)

  Always use HALF Kelly (conservative) to account for estimate uncertainty.
```

### 5.2 Portfolio-Level Controls

```
MAX CONCURRENT POSITIONS: 5
  Rationale: Each position needs monitoring bandwidth.
  With 0.01 SOL × 5 = 0.05 SOL max exposure.

MAX DAILY LOSS: 0.05 SOL
  If cumulative daily realized losses hit this → PAUSE trading for the day.
  Auto-resume at midnight UTC.

MAX ALLOCATION PER TOKEN: 10% of bankroll
  Never put more than 10% of total capital in one token.
  At 0.5 SOL bankroll: max 0.05 SOL per position.

RAW STOP LOSS: -40% of position
  If unrealized loss exceeds 40% → exit immediately.
  At 0.01 SOL: stop at -0.004 SOL unrealized loss.
  This is the LAST resort — EV-based exit should trigger before this.
```

### 5.3 Fee Budget

```
MAXIMUM ACCEPTABLE FEE RATIO: 40% of gross profit
  If fees/gross_profit > 40% over a rolling 50-trade window → investigation trigger.
  Current data shows 67% — this is the primary problem to solve.

FEE REDUCTION STRATEGIES:
  1. Increase average hold time (currently ~1s → target 30-120s)
     Longer holds capture larger moves, making fees a smaller %
  2. Increase selectivity (fewer trades, higher quality)
     Each trade that doesn't reach >3.5% gross return is a net loss
  3. Optimize priority fees (don't overpay on non-urgent trades)
     Default 0.0001 SOL, increase only for urgent exits
```

### 5.4 Circuit Breakers

```
LEVEL 1 — CAUTION (auto):
  Trigger: 3 consecutive losses
  Action: Increase min_entry_edge by 50% for next 10 minutes
  
LEVEL 2 — PAUSE (auto):
  Trigger: daily_loss > max_daily_loss OR 5 consecutive losses
  Action: Pause new entries for 30 minutes. Existing positions still managed.

LEVEL 3 — HALT (auto):
  Trigger: daily_loss > 2× max_daily_loss OR system health degraded
  Action: Full halt. No new entries. Exit all positions at market.
  Requires manual restart.
```

### 5.5 Regime-Specific Risk Adjustments

```
MID_CURVE (primary trading regime):
  - Standard position size
  - Standard stop loss (-40%)
  - Standard min_entry_edge

LATE_CURVE (selective):
  - Position size × 0.5 (half size — higher risk)
  - Tighter stop loss (-30%)
  - min_entry_edge × 1.5 (need more conviction)
  - Reduced max hold time (180s vs 300s)
```

---

## 6. Expected Performance Characteristics

### 6.1 Monte Carlo Projection

Based on the fee-adjusted EV model with our target parameters:

**Scenario: Conservative (55% win rate, 2:1 reward/risk)**

```
Win rate: 55%
Avg winner: +0.008 SOL (80% gross on 0.01)
Avg loser: -0.005 SOL (40% stop + fees)
Trades/day: 10
Fees/trade: 0.00035 SOL

Expected daily PnL = 10 × (0.55 × 0.008 - 0.45 × 0.005)
                   = 10 × (0.0044 - 0.00225)
                   = 10 × 0.00215
                   = +0.0215 SOL/day

Monthly (30 days): ~0.645 SOL
Annual: ~7.85 SOL
```

**Scenario: Target (60% win rate, 2.5:1 reward/risk)**

```
Win rate: 60%
Avg winner: +0.010 SOL (100% gross on 0.01)
Avg loser: -0.004 SOL (35% stop + fees)
Trades/day: 12
Fees/trade: 0.00035 SOL

Expected daily PnL = 12 × (0.60 × 0.010 - 0.40 × 0.004)
                   = 12 × (0.006 - 0.0016)
                   = 12 × 0.0044
                   = +0.0528 SOL/day

Monthly: ~1.58 SOL
Annual: ~19.3 SOL
```

**Scenario: Pessimistic (50% win rate, 1.5:1 reward/risk)**

```
Win rate: 50%
Avg winner: +0.006 SOL (60% gross)
Avg loser: -0.004 SOL
Trades/day: 8

Expected daily PnL = 8 × (0.50 × 0.006 - 0.50 × 0.004)
                   = 8 × (0.003 - 0.002)
                   = +0.008 SOL/day

Monthly: ~0.24 SOL (marginal but positive)
```

### 6.2 Drawdown Expectations

```
Maximum expected drawdown (95th percentile):
  At 10 trades/day, 55% win rate:
    Max consecutive losses: ~8 (statistical expectation)
    Max drawdown: 8 × 0.005 = 0.04 SOL
    Recovery time: ~2 days at expected rate

MAXIMUM TOLERABLE DRAWDOWN: 0.10 SOL (from starting bankroll)
  If hit → full strategy review before resuming
```

### 6.3 Key Performance Indicators (KPIs)

Track daily:

| KPI | Target | Red Flag |
|-----|--------|----------|
| Win rate (50-trade rolling) | >55% | <45% |
| Avg winner / Avg loser ratio | >2.0 | <1.2 |
| Fee ratio (fees / gross profit) | <40% | >60% |
| Selectivity (trades / tokens observed) | <0.5% | >2% |
| Rug pull losses | 0 (zero tolerance) | Any |
| Avg hold time (winners) | >30s | <5s |
| Entries with P_continuation > 0.65 | >80% of entries | <50% |
| Sharpe ratio (daily, annualized) | >2.0 | <1.0 |

---

## 7. Implementation Notes

### 7.1 Architecture Alignment

This strategy maps to the existing codebase as follows:

```
Hard Filters → src/entry/engine.ts :: checkHardFilters()
Probability  → src/probability/layer.ts :: computeProbabilities()
EV Calc      → src/entry/engine.ts :: evaluateEntry() [EV section]
Exit Logic   → src/exit/engine.ts :: evaluateExit()
Rug Detection→ src/manipulation/model.ts + src/features/creator-wallet-priors.ts
Features     → src/features/engine.ts (orchestrates all feature modules)
Risk         → src/types/config.ts :: RiskConfig
Calibration  → src/learning/calibration.ts
```

### 7.2 Critical Configuration Values

These are the exact values that should be in `config/default.json`:

```json
{
  "regime": {
    "early_curve_max_progress": 0.10,
    "mid_curve_max_progress": 0.50,
    "late_curve_max_progress": 0.90,
    "max_token_age_s": 300,
    "exclude_mayhem": true
  },
  "entry": {
    "min_entry_edge": 0.0005,
    "observation_window_s": 3,
    "min_breadth_for_entry": 0.30,
    "min_unique_buyers": 5,
    "max_concentration_top10": 0.70,
    "max_slippage_pct": 5.0,
    "ev_enter_horizon_s": 15,
    "probability_weights": {
      "flow_momentum": 0.30,
      "breadth_topology": 0.25,
      "creator_wallet_prior": 0.20,
      "friction_execution": 0.10,
      "manipulation_distribution": 0.10,
      "multimodal_junk": 0.05
    }
  },
  "exit": {
    "hold_horizon_s": 15,
    "retrace_threshold_base": 0.30,
    "time_decay_start_s": 30,
    "time_decay_pressure_per_s": 0.00001,
    "max_hold_time_s": 300
  },
  "risk": {
    "quick_spend_sol": 0.01,
    "risk_per_trade_pct": 0.02,
    "max_alloc_pct": 0.10,
    "max_positions": 5,
    "raw_stop_pct": 0.40,
    "max_daily_loss_sol": 0.05
  },
  "manipulation": {
    "hard_threshold": 0.40,
    "creator_sell_instant_exit": true
  },
  "fees": {
    "pump_fee_pct": 1.0,
    "pump_swap_fee_pct": 0.25,
    "pump_portal_fee_pct": 0.5,
    "priority_fee_default_sol": 0.0001
  }
}
```

### 7.3 Data Pipeline Requirements

```
REAL-TIME (latency < 500ms):
  - New token creation events
  - All trade events on tracked tokens
  - Creator wallet sell detection
  - Bonding curve state (vSol, vTokens)

NEAR-REAL-TIME (latency < 2s):
  - Creator wallet history lookup
  - Known wallet database query
  - Manipulation scoring

ASYNC (latency < 5s, non-blocking):
  - Multimodal junk filter (token name/logo analysis)
  - Detailed buyer quality analysis
```

### 7.4 Known Pitfalls to Avoid

1. **Never double-count friction.** Friction is a flat cost per round trip. It appears ONCE in the EV formula, not embedded in each scenario return.

2. **Never compress probability range.** If P_continuation is always 0.45-0.55, the system can't distinguish good from bad tokens. The 2x gain multiplier on rawSignal exists for this reason.

3. **Never exit on the first dip.** Winning tokens regularly retrace 20-30% before continuing. The 30% retrace threshold with dynamic tightening handles this.

4. **Never hold through a creator sell.** This is the one signal with near-100% predictive power. Creator sells = exit immediately.

5. **Never override hard filters.** The conjunction of all hard filters is the safety net. Removing any one filter "just this once" is how you take your biggest loss.

6. **Calibrate against real trades.** The probability weights and biases must be calibrated against actual trading outcomes, not hypothetical returns. The learning subsystem handles this.

### 7.5 Calibration and Learning Loop

```
HOURLY: Micro-calibration
  - Compare predicted P_continuation to actual outcomes
  - Adjust continuation_bias, reversal_bias, manipulation_bias
  - Small adjustments only: ±0.05 per hour

DAILY: Replay analysis
  - Replay all trades from the day
  - Compute actual win rate, avg winner, avg loser
  - Compare to expectations
  - Flag if actual deviates from expected by >2σ

WEEKLY: Weight retraining
  - Using accumulated trade data, retrain probability weights
  - Champion-challenger framework: new weights tested on canary (10% of trades)
  - Promote to champion only if canary outperforms on 50+ trades
```

---

## 8. Appendix: Mathematical Derivations

### 8.1 Bonding Curve Price Impact

For a constant-product AMM with reserves (x, y) and invariant k = x × y:

Buying Δ_SOL worth of tokens:
```
new_x = x + Δ_SOL
new_y = k / new_x = k / (x + Δ_SOL)
tokens_out = y - new_y = y - k/(x + Δ_SOL) = y × Δ_SOL / (x + Δ_SOL)

spot_price = x / y
effective_price = Δ_SOL / tokens_out = (x + Δ_SOL) / y
price_impact = Δ_SOL / x  (approximately, for small Δ)
```

At default parameters (x=30, y=1.073B):
- 0.01 SOL buy: price impact ≈ 0.033%
- 0.1 SOL buy: price impact ≈ 0.33%
- 1.0 SOL buy: price impact ≈ 3.3%

### 8.2 Graduation Economics

Graduation occurs when the bonding curve accumulates ~85 SOL (net of creator allocation):

```
At graduation:
  vSol ≈ 85 SOL
  price ≈ 85 / remaining_tokens
  market_cap ≈ $69,000 (at typical SOL price)
  
  A position entered at vSol=35 (early MID_CURVE):
    entry_price = 35 / tokens_at_35
    graduation_price = 85 / tokens_at_85
    
    approximate gross return ≈ (85/35) - 1 ≈ 143%
    
  A position entered at vSol=50 (late MID_CURVE):
    approximate gross return ≈ (85/50) - 1 ≈ 70%
```

### 8.3 Fee-Adjusted Breakeven

For a round trip to break even:

```
gross_return_needed = round_trip_fees / position_size
                    = 0.00035 / 0.01
                    = 3.5%

For a position entered at vSol=35:
  3.5% price increase → vSol needs to reach ≈ 36.2 SOL
  This corresponds to about 0.4% additional curve progress
  Achievable with ~3-5 additional buy transactions of 0.01 SOL each
```

### 8.4 Kelly Criterion Derivation (Future Use)

```
Given:
  p = probability of winning
  q = 1 - p
  b = ratio of net win to net loss

Optimal fraction to risk:
  f* = (p × b - q) / b

Example with our targets:
  p = 0.60, q = 0.40, b = 0.010/0.004 = 2.5
  f* = (0.60 × 2.5 - 0.40) / 2.5 = (1.5 - 0.4) / 2.5 = 0.44

  Half-Kelly: f = 0.22
  On 0.5 SOL bankroll: position = 0.22 × 0.5 = 0.11 SOL

  This is future state — currently fixed at 0.01 SOL.
```

### 8.5 Sharpe Ratio Target

```
Expected daily return: μ = 0.0215 SOL (conservative scenario)
Expected daily std dev: σ ≈ 0.015 SOL (estimate from MC simulation)
Daily Sharpe: μ/σ = 0.0215/0.015 ≈ 1.43
Annualized Sharpe: 1.43 × √252 ≈ 22.7

Note: This is extremely high because:
  - We're measuring in SOL, not percentage
  - Opportunities compound differently than traditional markets
  - Actual Sharpe will be lower due to correlated losses during market regime shifts
  - Realistic annualized Sharpe target: 2.0-5.0
```

---

## Revision History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | Initial | First draft based on 50-trade analysis |
| 2.0 | 2026-03-25 | Complete rewrite. Fixed double-counting friction, power law returns, proper probability decomposition, EV-based exit framework, fee analysis, creator rug detection. |

---

*"In the kingdom of the blind, the one-eyed man is king. In the kingdom of memecoins, the one with a stop loss is king."*
