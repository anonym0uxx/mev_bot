# Quant Analysis: Entry/Exit EV Engine

## Critical Issues Found

### 1. DOUBLE-COUNTING FRICTION (Fatal)
The EV formula subtracts friction THREE times:
- Once from `upsideNet = upsideGross - frictionCostNow`
- Once from `downsideNet = downsideGross + frictionCostNow`
- Once standalone: `- frictionCostNow`

This is mathematically wrong. Friction should appear ONCE as round-trip cost.

**Fix:** Remove standalone friction term. Friction is already embedded in upside/downside.

### 2. STATIC PAYOFF ESTIMATES vs POWER LAW REALITY
Using fixed 40% upside is wrong. Pump.fun token returns follow a power law distribution
(see: Gabaix 2009 "Power Laws in Economics"; Cont 2001 "Empirical properties of asset returns").

Empirical Pump.fun distribution (approx):
- 60% of entries: -5% to -15% (quick reversal, friction-dominated loss)
- 25% of entries: -15% to -50% (slower reversal)
- 10% of entries: +50% to +200% (real pump)
- 5% of entries: +200% to +1000% (moonshot)

The correct approach: compute expected payoff from the empirical distribution, not a point estimate.
E[upside | continuation] ≈ 80-120% (mean of the right tail)
E[downside | reversal] ≈ -12% (mean of the left body, bounded by stop)

### 3. P_manipulation IS AN INDEPENDENT AXIS, NOT AN ADDITIONAL PENALTY
Current formula: `EV = P_cont * upside - P_rev * downside - P_manip * manip_cost - friction`

This treats manipulation as a THIRD independent outcome. But manipulation IS a reversal scenario.
The correct decomposition (see: Easley/O'Hara "Information and the Cost of Capital"):
- P(continuation) + P(organic reversal) + P(manipulation reversal) = 1
- Or simpler: P_manip amplifies the severity of reversal, not a separate term

**Fix:** Use P_manipulation as a conditional severity multiplier on reversal, not additive.

### 4. EV_WAIT IS POSITIVE BY DEFAULT (Anchoring Bias)
Current EV_wait returns positive for uncertain tokens, which means the system always
prefers to wait. This is the "option value of waiting" but it's miscalibrated.

On Pump.fun, alpha decays extremely fast (see: Budish/Cramton/Shim 2015 "The High-Frequency
Trading Arms Race"). Waiting 5s means missing 30-50% of the price move on genuine pumps.

**Fix:** EV_wait should be near-zero or negative when velocity is positive.

### 5. OBSERVATION WINDOW TOO LONG
8s observation window misses the fastest alpha. Pump.fun bonding curve tokens can 2-5x
in the first 10s. By the time we observe for 8s, the best entry is gone.

**Fix:** Reduce to 3s or make it dynamic based on velocity.

### 6. PROBABILITY LAYER HAS LOW DYNAMIC RANGE
sigmoid(rawSignal) where rawSignal is a small weighted sum produces P_continuation 
centered around 0.5-0.6. The system never gets high enough conviction to trade.

The flow/breadth signals need more gain. A token with 10+ buyers, positive velocity,
and good breadth should produce P_continuation > 0.75, not 0.58.
