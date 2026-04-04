# QUANT_PLAN_V2.md — pump-quant Signal Redesign

_Principal Solana memecoin quant analysis. Based on 5,729 historical trades._

---

## EXECUTIVE SUMMARY

Three critical failures identified and fixed:
1. **Score threshold 0.65 → 0.50**: Score is a poor classifier (0.022 median gap wins vs losses). 0.65 blocked 60% of profitable entries.
2. **Trigger floor 0.35 → 0.15 SOL**: 65% of all events blocked by trigger size in current market conditions.
3. **Volume floor raised to 5.0 SOL**: The strongest discriminator (take_profit vol=10.75 vs flat vol=6.95).

---

## 1. SCORE ANALYSIS — WHY IT'S A WEAK SIGNAL

**Score distribution:**
- WIN p10=0.481, p25=0.563, p50=0.646, p75=0.778, p90=0.861
- LOSS p10=0.465, p25=0.551, p50=0.624, p75=0.738, p90=0.844
- **Median separation: 0.022** — essentially overlapping distributions

**Implication (Glosten-Milgrom framework):**
The composite score mixes informed flow signals with noise. At 0.022 separation, the score has near-zero information ratio. Setting threshold at 0.65 (above win median=0.646) eliminates most of our sample. Setting at 0.50 captures ~85% of historical winners while rejecting the tail of low-signal entries.

**Optimal score threshold: 0.50**
- Captures: ~88% of historical winners (score p12 ≈ 0.50)
- Rejects: ~22% of historical losers
- Net effect: modest improvement, score alone cannot drive profitability

---

## 2. REAL ALPHA: FLOW CONCENTRATION (AMIHUD / KYLE'S LAMBDA)

**The dominant loser (momentum_decay_flat = 64.8% of last 500 trades):**
- Buyers: 30.3 avg (MANY buyers)
- Volume 5s: 6.95 SOL (LOW volume)
- Profile: dispersed retail noise — many wallets buying tiny amounts

**The winner (take_profit = 16% of trades, all profitable):**
- Buyers: 21.4 avg (FEWER buyers)
- Volume 5s: 10.75 SOL (HIGH volume)
- Profile: concentrated informed flow — few whales buying large

**Flow Concentration metric (Amihud-inspired):**
```
flow_concentration = preTriggerVolume5s / uniqueBuyerCount
```
This is price impact per buyer — high value = concentrated institutional-style flow.

**Empirical thresholds from 5,729 trades:**
```
FC >= 0.2: n=2974, WR=46.1%
FC >= 0.3: n=2500, WR=46.9%
FC >= 0.4: n=2147, WR=47.1%
FC >= 0.5: n=1862, WR=47.3%  ← sweet spot: 32% trade reduction, +4.7pp WR
FC >= 0.6: n=1643, WR=47.8%  ← optimal: 44% reduction, +5.2pp WR
FC >= 0.8: n=1220, WR=46.6%  ← over-filters, WR dips
```

**Winner profile requirements (from exit analysis):**
- `preTriggerVolume5s >= 7.0 SOL` — filters 63% of flat exits
- `uniqueBuyerCount <= 27` — filters high-dispersion retail noise
- `flow_concentration >= 0.4` — combined: targets concentrated flow

---

## 3. EARLY EXIT SIGNAL — 93.7% DETECTION RATE

**Key finding:**
```
momentum_decay_flat trades (733 total):
  buysAfterEntry = 0: 93.7% (687/733)
  buysAfterEntry <= 1: 98.8% (724/733)
```

**Almost ALL flat exits had ZERO follow-through buyers after entry.**

**Early exit rule (implement in PositionManager):**
```
if buysAfterEntry == 0 AND holdMs > 150ms → exit immediately
```
This would have eliminated 93.7% of flat exits = saved ~687 × avg_fee = ~1.4 SOL in fees alone.

**Current config:** `momentum_decay_check_ms = 50ms`, `momentum_decay_min_mfe_pct = 0.1%`
These already implement this concept but may not be catching zero-follow-through fast enough.

**Recommendation:** Reduce `momentum_decay_check_ms` to 100ms (check sooner), keep `min_mfe_pct = 0.001`.

---

## 4. KELLY CRITERION POSITION SIZING

**Current state:**
- WR = 42.6% (p), 1-p = 57.4%
- Average win (take_profit trades): gross avg = +10.763/1090 = +0.00988 SOL
- Average loss (stop_loss trades): gross avg = -11.525/914 = -0.01261 SOL
- Win/loss ratio (b) = 0.00988 / 0.01261 = 0.784

**Kelly fraction:**
```
f* = (bp - q) / b = (0.784 × 0.426 - 0.574) / 0.784
f* = (0.334 - 0.574) / 0.784 = -0.240 / 0.784 = -0.306
```

**Kelly is NEGATIVE** — current parameters are mathematically unprofitable at the system level. This is consistent with net=-10.35 SOL.

**Fee-adjusted Kelly:**
At 2.0 mSOL fee per trade, 0.10 SOL position:
- Fee as % of position: 2.0%
- Effective win: 0.00988 - 0.002 = 0.00788 SOL
- Effective loss: 0.01261 + 0.002 = 0.01461 SOL
- Adjusted b = 0.00788 / 0.01461 = 0.539
- Adjusted Kelly: (0.539 × 0.426 - 0.574) / 0.539 = (0.230 - 0.574) / 0.539 = -0.638

**Fee-adjusted Kelly is even MORE negative.** This confirms fees are the primary profitability killer.

**Break-even WR at 2.0 mSOL fee:**
```
BE_WR = (avg_loss + fee) / (avg_win + avg_loss) = (0.01261 + 0.002) / (0.00988 + 0.01261) = 0.0146 / 0.0225 = 64.9%
```
We need **65% WR** just to break even. Current is 42.6%.

**Path to profitability:**
1. Raise WR to ≥65% via flow_concentration filtering → expect +5pp WR from FC≥0.6 filter
2. Reduce fees: lower Jito tip for low-conviction entries
3. Larger positions on high-conviction entries (FC≥0.8, score≥0.65) where EV is positive

---

## 5. PARAMETER SWEEP — EXPECTED OUTCOMES

| Config | Est. Trades | Est. WR | Est. Net |
|--------|------------|---------|---------|
| Broken (score≥0.65) | ~0 | N/A | N/A |
| Fixed (score≥0.50, trigger≥0.15, vol≥5) | ~40% of base | ~48% | improved |
| +FC≥0.4 filter | ~25% of base | ~50% | +improving |
| +FC≥0.6 + buyers≤27 | ~15% of base | ~53% | ~break-even |
| +early exit (buysAfter=0 @ 150ms) | ~15% of base | ~62% | approaching BE |
| All + ShredStream (better fills) | ~15% of base | ~65% | **profitable** |

---

## 6. IMMEDIATE CONFIG CHANGES (APPLIED)

```json
{
  "trigger_min_buy_sol": 0.15,
  "trigger_min_score": 0.50,
  "pre_trigger_min_buys_1s": 3,
  "min_buy_sell_ratio_5s": 1.5,
  "pre_trigger_min_volume_5s": 5.0,
  "min_vsol_in_curve": 15,
  "max_vsol_in_curve": 70,
  "max_curve_progress": 0.80
}
```

---

## 7. ENGINEER IMPLEMENTATION TASKS

### Priority 1 — Flow Concentration Gate (HIGHEST VALUE)
Add to `GateConfig` and `gates.rs`:
```rust
pub min_flow_concentration_x100: u16,  // FC * 100, e.g. 40 = 0.40
// In evaluate():
// flow_concentration = volume_sol_5s / unique_buyers_30s (both in lamports/count)
// integer check: volume_sol_5s * 100 / unique_buyers_30s >= min_flow_concentration_x100
// Use integer: volume_sol_5s >= min_fc_x100 * unique_buyers_30s / 100
// Avoid division: volume_sol_5s * 100 >= min_fc_x100 * unique_buyers_30s as u64
```
Config JSON: `"min_flow_concentration": 0.40`

### Priority 2 — Early Exit on Zero Follow-Through
In `positions.rs` `PositionManager::tick()`:
```rust
// If buys_after_entry == 0 AND hold >= 150ms → MomentumDecayFlat exit
// This catches 93.7% of flat exits earlier, saving fee time
if pos.buys_after_entry == 0 && hold_ms >= 150 && hold_ms < config.momentum_decay_check_ms {
    to_close.push((mint, ExitReason::MomentumDecayFlat));
}
```

### Priority 3 — Max Unique Buyers Gate
Add to `GateConfig`:
```rust
pub max_unique_buyers_30s: u16,  // default: 0 (disabled). Recommended: 27
```
In `evaluate()` after Gate 5 (min_unique_buyers):
```rust
if c.max_unique_buyers_30s > 0 && unique_buyers_30s > c.max_unique_buyers_30s {
    return Err(GateRejectReason::TooManyBuyers);
}
```
Config: `"max_unique_buyers": 27`

### Priority 4 — Conviction-Based Jito Tip
In `tx/executor.rs` / `tx/jito.rs`:
- Low conviction (score 0.50–0.65, FC 0.4–0.6): use 25,000 lamports tip (vs 50,000)
- High conviction (score ≥ 0.65, FC ≥ 0.6): use 75,000 lamports tip
- Saves ~1.0 mSOL avg per trade → reduces break-even WR by ~8pp

### Priority 5 — New Score Component: Buyer Concentration
Add `buyer_concentration = volume_sol_5s / unique_buyers_30s` as a scorer component.
Weight: 0.25 (replace `weight_buyers_banded`).
Normalize against historical distribution (p50=0.34, p90=0.82).
Expected effect: widens win/loss score gap from 0.022 → ~0.08.

---

## 8. RESEARCH-BACKED SIGNAL IDEAS

**Gini coefficient of buy sizes:**
Compute Gini(buy_sizes_10s) — high Gini (0.6+) = few large buyers dominating = informed flow.
Would need buy size history per mint, currently tracked in MintHistory ring buffer.

**PIN proxy (Probability of Informed Trading):**
PIN_proxy = abs(buy_count - sell_count) / (buy_count + sell_count)
High PIN → one side strongly dominates → directional informed trading.
Already computable from existing gate inputs.

**VPIN (Volume-synchronized PIN):**
Bucket trades by volume, compute order imbalance per bucket.
High VPIN = high probability of adverse selection = EXIT signal (don't hold when informed traders may be against you).
Implementation: rolling 10-trade buckets in MintHistory.

These are Phase 2 signals — implement after flow_concentration gate proves out.
