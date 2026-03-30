# QUANT: Signal-Kelly Coherence — Algorithmic Threshold Calibration

## Problem Statement

Our signal engine uses **arbitrary magic thresholds** (Exit<200, Weakening<400, Sustained<700, Strong≥700) with no mathematical connection to the Kelly criterion that governs entry sizing and exit trail width. The composite score (0–1000) is a dimensionless number with no probabilistic interpretation.

**What we want:** Signal thresholds that are *derived from* Kelly parameters, so the entire system speaks one language: "what is the expected growth rate of holding vs. exiting right now?"

## The Core Insight

Kelly criterion answers: "given win probability p and reward ratio R, what fraction f* of bankroll maximizes long-run geometric growth?"

```
f* = (p(R+1) - 1) / R     (classical Kelly)
g(f) = p·ln(1+f·R) + (1-p)·ln(1-f)   (growth rate at fraction f)
```

The key: **g(f) = 0 defines the breakeven boundary.** When g drops to zero, holding is no better than not having entered. When g goes negative, we're *destroying* bankroll.

**Signal thresholds should map to growth rate boundaries:**
- `StrongPump`: g(f*) >> 0 (significantly positive, widen trail)
- `Sustained`: g(f*) > 0 (positive but moderate, normal trail)
- `Weakening`: g(f*) ≈ 0 (near breakeven, tighten trail)
- `Exit`: g(f*) < 0 (negative expected growth, close immediately)

## Three Algorithmic Approaches

### 1. SPRT (Sequential Probability Ratio Test) — Wald, 1945

**Idea:** Treat each incoming trade event as an observation. We're sequentially testing two hypotheses:
- H₁: "pump is alive" (p = p_entry from Kelly LUT, trade has positive EV)
- H₀: "pump is dead" (p = p_dead, trade has zero/negative EV)

The log-likelihood ratio accumulates:

```
Λₙ = Σᵢ log(L(xᵢ|H₁) / L(xᵢ|H₀))
```

For our setting:
- **Buy event** with sol_amount s: contributes `+log(p₁/p₀)` weighted by size
- **Sell event**: contributes `+log((1-p₁)/(1-p₀))` (negative — evidence for H₀)
- **No event (gap)**: contributes a time-decay penalty

**SPRT stopping rule:**
- `Λₙ > A = log((1-β)/α)`: Accept H₁ → StrongPump (pump alive, wide trail)
- `Λₙ < B = log(β/(1-α))`: Accept H₀ → Exit (pump dead, close immediately)
- `B < Λₙ < A`: Continue monitoring → Sustained/Weakening

**Where α,β come from:** They are the Type I (false exit) and Type II (false hold) error rates. **These connect directly to Kelly:**
- α (false exit) = probability of closing a winning position early → reduces realized R
- β (false hold) = probability of holding a losing position → increases realized loss
- Optimal α,β minimize Kelly growth rate loss: `Δg = -α·p·ln(1+f*·R) - β·(1-p)·ln(1-f*)`

**Connection to our system:**
```
composite_score → Λₙ (log-likelihood ratio)
Exit threshold  → B = log(β/(1-α))    ≈ f*-dependent
Strong threshold → A = log((1-β)/α)    ≈ f*-dependent
```

**Implementation (integer):**
Instead of the current weighted-sum score, accumulate a log-likelihood ratio in fixed-point:

```rust
// On each buy event:
llr += LLR_BUY_BP;  // = 1000 * log(p_alive / p_dead) per unit SOL

// On each sell event:
llr += LLR_SELL_BP;  // negative: 1000 * log((1-p_alive) / (1-p_dead))

// On each tick (time decay — no news is bad news in pump-space):
llr -= TIME_DECAY_BP;  // pump lifetime is short; silence = death

// Thresholds derived from entry conviction:
let exit_threshold = entry_conviction.exit_llr_bp();     // = 1000 * log(β/(1-α))
let strong_threshold = entry_conviction.strong_llr_bp();  // = 1000 * log((1-β)/α)
```

**Pros:** Mathematically optimal (Wald-Wolfowitz theorem: SPRT minimizes expected sample size for given error rates). Directly interpretable. Thresholds are *derived* from Kelly parameters, not hand-tuned.

**Cons:** Assumes independent observations (buys/sells aren't truly independent). Requires estimating p_alive and p_dead distributions from historical data.

### 2. Bayesian Posterior Update — Online p(t) Tracking

**Idea:** Instead of testing hypotheses, maintain a running Bayesian posterior estimate of the *current* win probability p(t) and reward ratio R(t). When p(t) drops below the Kelly breakeven line, exit.

The entry engine provides a prior: `p₀ = entry_p_permille/1000, R₀ = entry_r_x100/100`.

Each observation updates the posterior:

```
p(t) ∝ p(t-1) · L(observation | p)
```

For pump trading, we model "pump alive" as a Bernoulli process where each time window (say, 500ms bucket) is either "net buy" (success) or "net sell/flat" (failure):

```
Beta(α, β) prior at entry → Beta(α + buys, β + non_buys) posterior
p̂(t) = α_post / (α_post + β_post)
```

**Kelly breakeven condition:**
```
f*(t) = (p̂(t)·(R̂(t)+1) - 1) / R̂(t)
Exit when: f*(t) ≤ 0, i.e., p̂(t) ≤ 1/(R̂(t)+1)
```

**Signal state mapping:**
```
f*(t) > 0.8·f*_entry  →  StrongPump  (conviction holding strong)
f*(t) > 0.4·f*_entry  →  Sustained   (conviction degrading but positive)
f*(t) > 0             →  Weakening   (barely positive, tighten trail)
f*(t) ≤ 0             →  Exit        (negative EV, close now)
```

**The beauty:** Thresholds are expressed as *fractions of entry Kelly*, not arbitrary score values. A HIGH conviction entry (f* = 300 permille) gets more room to decay than a LOW conviction entry (f* = 100 permille). The system naturally adapts.

**R̂(t) update:** Use realized MFE trajectory as evidence for R:
```
R̂(t) = EMA(R_prior, max(0, current_unrealized_pnl / current_risk))
```

**Implementation (integer, ~10ns):**
```rust
struct BayesianSignal {
    alpha_x16: u16,   // Beta distribution α × 16 (4-bit fractional)
    beta_x16: u16,    // Beta distribution β × 16
    r_est_x100: u16,  // Current R estimate × 100
}

impl BayesianSignal {
    fn update_buy(&mut self, sol_msol: u16) {
        // Weight by buy size (larger buys = stronger evidence)
        let weight = (sol_msol / 100).min(16) as u16; // max 16 increments
        self.alpha_x16 = self.alpha_x16.saturating_add(weight);
    }

    fn update_sell(&mut self, sol_msol: u16) {
        let weight = (sol_msol / 100).min(16) as u16;
        self.beta_x16 = self.beta_x16.saturating_add(weight);
    }

    fn current_f_permille(&self) -> i16 {
        // p̂ = α / (α + β)
        let p_x1000 = (self.alpha_x16 as u32 * 1000) / 
                       (self.alpha_x16 as u32 + self.beta_x16 as u32);
        // f* = (p(R+1) - 1) / R
        let r_plus_1 = self.r_est_x100 as i32 + 100;
        let numerator = p_x1000 as i32 * r_plus_1 / 1000 - 100;
        let f = (numerator * 1000) / self.r_est_x100.max(1) as i32;
        (f / 2) as i16  // half-Kelly
    }
}
```

**Pros:** Natural Bayesian framework. Entry conviction is the prior, observations update it. Thresholds are Kelly-derived (f* fractions). Adapts to different entry qualities automatically.

**Cons:** Beta distribution assumes exchangeable observations (ordering doesn't matter). In reality, *acceleration* matters (3 buys in 1s > 3 buys spread over 5s). Need to handle time decay explicitly (Beta doesn't have a natural forgetting mechanism).

### 3. CUSUM (Cumulative Sum) — Page, 1954

**Idea:** Detect the *change point* where the pump regime shifts from "alive" to "dead." CUSUM is simpler than SPRT and naturally handles the one-sided detection we need (we only care about detecting pump death, not pump birth — entry engine handles that).

```
S(t) = max(0, S(t-1) + (x(t) - k))
```

Where:
- `x(t)` = net flow signal at time t (buys - sells, weighted)
- `k` = allowance parameter (expected flow under "pump alive")
- Exit when `S(t) > h` (threshold)

**Kelly-calibrated CUSUM:**
- `k` = expected net flow that maintains Kelly-positive EV
  - Derived from: at entry, we estimated p and R. The minimum buy rate to sustain p is calculable from historical data.
  - `k = expected_buy_rate_1s × avg_buy_size` (the flow that keeps us at f* > 0)
- `h` = threshold = amount of cumulative deviation before we're confident the regime changed
  - `h ∝ 1/f*_entry` (HIGH conviction entries tolerate more deviation before exit)

**Implementation:**
```rust
// On each 500ms bucket:
let net_flow = buy_vol_msol as i32 - sell_vol_msol as i32;
let deviation = k_expected_flow as i32 - net_flow; // positive when flow disappoints
cusum = (cusum + deviation).max(0); // one-sided CUSUM

if cusum > h_threshold {
    state = SignalState::Exit;
}
```

**Average Run Length (ARL):** CUSUM theory gives exact formulas for how quickly the detector fires:
- ARL₀ = expected ticks before false alarm (when pump is alive)
- ARL₁ = expected ticks before detection (when pump dies)
- Both are functions of k and h, which we set from Kelly parameters

**Pros:** Simplest implementation. Well-understood statistical properties. ARL is tunable. Change-point detection is exactly what we need.

**Cons:** Less information-rich than SPRT (doesn't maintain full likelihood ratio). No natural multi-state output (need separate thresholds for Strong/Sustained/Weakening).

## Recommendation: Hybrid Bayesian-SPRT

Use Approach 2 (Bayesian) as the **state tracker** with Approach 1 (SPRT) as the **decision boundary**:

### Architecture

```
                    Entry Engine
                        │
                        ▼
              EntryConviction {p₀, R₀, f*₀}
                        │
        ┌───────────────┤
        ▼               ▼
   Bayesian Prior    SPRT Boundaries
   Beta(α₀, β₀)     A = g(strong), B = g(exit)
        │               │
        ▼               │
   On each event:       │
   Update Beta dist     │
   Compute p̂(t), f̂*(t) │
        │               │
        ▼               ▼
   Map f̂*(t) to signal state via SPRT boundaries:
   
   f̂*(t) / f*₀ > threshold_strong  → StrongPump
   f̂*(t) / f*₀ > threshold_sustain → Sustained
   f̂*(t) / f*₀ > 0                 → Weakening
   f̂*(t) ≤ 0                       → Exit
```

### Concrete Threshold Derivation

Given entry conviction `{p, R, f*}`:

```
Breakeven:  p_break = 1 / (R + 1)
Kelly f*:   f* = (p(R+1) - 1) / R

Strong boundary:  f̂*(t) > 0.7 × f*_entry   (70% of entry conviction maintained)
Sustain boundary: f̂*(t) > 0.35 × f*_entry   (35% — enough for positive growth)
Weaken boundary:  f̂*(t) > 0                  (any positive Kelly fraction)
Exit boundary:    f̂*(t) ≤ 0                  (breakeven or worse → close)
```

For our LUT cell (p=542, R=43.00, f*=248 permille):
```
Strong:  f̂*(t) > 174 permille  →  p̂(t) > 0.510
Sustain: f̂*(t) > 87 permille   →  p̂(t) > 0.488
Weaken:  f̂*(t) > 0             →  p̂(t) > 0.463
Exit:    f̂*(t) ≤ 0             →  p̂(t) ≤ 0.463
```

### Prior Initialization from Entry Conviction

```
// Entry gave us p₀ and an observation count (implicit in magnitude + score)
// Map to Beta prior strength based on conviction tier:
//   LOW:  Beta(α=3, β=3)   — weak prior, easily swayed
//   MED:  Beta(α=5, β=4)   — moderate prior, balanced
//   HIGH: Beta(α=8, β=5)   — strong prior, more evidence needed to flip

// Adjust α/β so α/(α+β) = p₀:
let total = PRIOR_STRENGTH[conviction_tier];  // 6, 9, or 13
let alpha_0 = (p_permille * total / 1000) as u16;
let beta_0 = total - alpha_0;
```

### Time Decay (Forgetting Factor)

Beta distributions don't forget. For pump trading, recent evidence matters far more. Apply exponential forgetting:

```rust
// Every 500ms, shrink both α and β toward prior (information evaporation):
const DECAY_RATE: u16 = 240; // 240/256 ≈ 0.9375 per 500ms (half-life ≈ 5s)

fn decay(&mut self) {
    self.alpha_x16 = (self.alpha_x16 as u32 * DECAY_RATE as u32 / 256) as u16;
    self.beta_x16 = (self.beta_x16 as u32 * DECAY_RATE as u32 / 256) as u16;
    // Clamp to minimum prior to prevent division by zero
    self.alpha_x16 = self.alpha_x16.max(MIN_ALPHA);
    self.beta_x16 = self.beta_x16.max(MIN_BETA);
}
```

Half-life ≈ 5 seconds means evidence from 15 seconds ago has <12.5% weight. Perfect for sub-minute pump trades.

### Trail Width Integration

Current system: `trail_bp = base_bp × kelly_mult × phase_mult >> 16`

Proposed: `trail_bp = base_bp × (f̂*(t) / f*_entry) × scale >> 16`

The trail width is now *literally proportional to the current Kelly fraction*. High conviction → wide trail. Conviction decaying → trail tightens automatically. f* hits zero → trail width = 0 → immediate exit.

### What This Replaces

| Current (arbitrary) | Proposed (Kelly-derived) |
|---|---|
| composite_score: weighted sum of 12 features | f̂*(t): Bayesian posterior Kelly fraction |
| Thresholds: 200/400/700 (magic numbers) | Thresholds: 0/0.35f*/0.7f* (derived from entry conviction) |
| score 0-1000 (dimensionless) | f̂* in permille (meaningful unit: expected growth rate) |
| Same thresholds for all entries | Thresholds scale with entry quality |
| kelly_trail_mult: separate subsystem | Trail directly proportional to f̂*(t) |

### Implementation Cost

The Bayesian update is 3 integer multiplications + 1 division per event. The SPRT boundary check is 2 comparisons. Total: **<15ns per tick** on our hardware. Fits the <80ns pipeline budget with room to spare.

**Struct change (RideState v3):**
```rust
// Replace in RideState (same 128-byte budget):
// Remove: composite_score (u16), kelly_trail_mult (u16), phase_trail_mult (u16)
// Add:    alpha_x16 (u16), beta_x16 (u16), r_est_x100 (u16)
// Net: 0 bytes added — these replace existing fields exactly
```

## Key References

1. **Wald (1945)** — Sequential Analysis (SPRT foundation)
   - Proves SPRT minimizes expected sample size for given error rates
   - Our exit detection is literally a sequential test: "is the pump still alive?"

2. **Page (1954)** — CUSUM for change-point detection
   - Simpler one-sided alternative; good for "detect when flow drops below k"
   - ARL theory gives predictable detection delays

3. **Zambelli (arXiv:1609.00869)** — Bayesian stop-loss via drawdown distributions
   - Shows stop-loss levels should be derived from the distribution of maximum drawdowns, not set arbitrarily
   - Directly analogous to our problem: exit threshold should reflect the statistical properties of pump lifecycles

4. **Kelly (1956)** — Original Kelly criterion
   - g(f) = p·ln(1+fR) + (1-p)·ln(1-f) is the growth rate function
   - Our signal states map to g(f) > 0 (hold) vs g(f) ≤ 0 (exit)

5. **Thorp (1969)** — Practical Kelly for multiple simultaneous bets
   - Our correlation adjustment f_each = f*/(1+(n-1)ρ) should also apply to exit decisions: with more open positions, be more conservative on exit thresholds

6. **Dwarakanath et al. (arXiv:2209.14738)** — GP-based optimal stopping
   - Shows structural properties of financial time series enable closed-form optimal stopping policies
   - Too heavy for our latency budget but validates the principle

## Implementation Plan

### Phase 1: Bayesian Prior Wire-Up (1 session)
- Add `BayesianSignal` struct to RideState (replaces composite_score fields)
- Initialize from EntryConviction's p_permille and conviction_tier
- Update on buy/sell events
- Map f̂*(t) to signal states using Kelly-derived thresholds
- **Keep current signal engine as fallback** (feature flag)

### Phase 2: Calibrate from Paper Data (1-2 days)
- Log both old and new signal scores in JSONL
- Compare exit quality: does Bayesian produce better timing?
- Tune forgetting factor (DECAY_RATE) and prior strengths

### Phase 3: Remove Old Signal Engine
- Once Bayesian outperforms, remove the 12-feature weighted sum
- Simplify RideState (fewer fields, same 128 bytes)

## Open Questions

1. **Should R̂(t) update online?** The reward ratio could be tracked via realized MFE, but this adds complexity. Initial approach: keep R fixed at entry estimate.

2. **Multi-scale decay?** Current: single 500ms decay. Could use 2 timescales (1s for immediate flow, 5s for trend). Adds 4 bytes to state.

3. **Asymmetric evidence weighting?** A sell is stronger evidence than a missing buy (active liquidation vs. lull). Weight sells at 2× buys in the Beta update?

4. **Correlation between signal and Kelly sizing?** When we hold 5 positions and all are in Weakening state, that's a stronger signal than 1 in Weakening. Portfolio-level signal aggregation?
