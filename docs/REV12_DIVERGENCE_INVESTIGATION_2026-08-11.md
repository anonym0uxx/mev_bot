# Rev-12 Paper-Trade Divergence Investigation — Final Report
**Date:** 2026-08-11
**Status:** INVESTIGATION COMPLETE — no config changes made (per directive)
**Tape:** 82 trades, Rev-12 clean baseline (label: rev12-baseline-v1)

---

## Executive Summary

The walk-forward sim projected **40-45% WR, PF 2.7+, +1,416 SOL / 84,809 trades**.
Live paper trading shows **15.9% WR, PF 0.14, -0.226 SOL / 82 trades**.
The avg trade PnL is **-0.00276 SOL ≈ the 2.82% bonding-curve round-trip cost**.
This means the bot's entry-to-exit price movement averages to **ZERO** — it is paying the curve tax on every trade with no net edge.

**Two fundamental sim-to-live divergences explain the gap:**

1. **TICK SEMANTICS MISMATCH** — The sim treated each swap as one tick (stall_ticks=100 = 100 swaps = potentially 50+ minutes). Live uses wall-clock 250ms ticks (stall_ticks=100 = 25 seconds). Positions exit **20-100x faster** in live than the sim assumed, before tokens can find organic buyers.

2. **CURVE IMPACT NOT MODELED** — The sim report explicitly admits "Slippage (not modeled in HF data)" (line 242). Live charges ~2.82% round-trip curve impact on every trade. The sim's break-even was ~0%; live's break-even is ~2.82%. Many trades the sim counted as "wins" are losses in live.

---

## Root Cause #1: Tick Semantics Mismatch (PRIMARY)

### The Unit Error
| Component | Tick semantics | stall_ticks=100 means |
|-----------|---------------|----------------------|
| **Sim** (Python on HF data) | 1 swap = 1 tick | 100 swaps (potentially 50+ min at 2 swaps/min) |
| **Live** (Rust daemon) | 250ms wall-clock = 1 tick | 100 × 250ms = 25 seconds |

The sim gave each position **100 swaps** to find a new price high.
Live gives each position only **25 seconds** (maybe 2-5 swaps at low-activity tokens).

### Affected Levers (all `*_ticks` parameters)

| Lever | Value | Live (seconds) | Sim assumed | Impact |
|-------|-------|---------------|-------------|--------|
| **lc_stall_ticks** | 100 | 25 sec | 100 swaps | **PRIMARY KILLER** — exits 20-100x too fast |
| lc_max_hold_ticks | 2400 | 600 sec (10 min) | 2400 swaps | Time stop at 10 min — never reached (stall fires first) |
| confirm_ttl_ticks | 200 | 50 sec | 200 swaps | Onchain confirm must arrive in 50 sec — tight |
| reentry_cooldown_ticks | 2400 | 600 sec (10 min) | 2400 swaps | 10-min cooldown — reasonable for live |
| reflect_every_ticks | 50 | 12 sec | 50 swaps | Engine evaluates every 12 sec — OK |
| **universe_window_ticks** | 24 | 6 sec | 24 swaps | 6-sec activity window — extremely selective |
| moon_bag_acceleration_window | 10 | 2.5 sec | 10 swaps | 2.5-sec velocity window — too noisy |

### The Stall=100 vs 600 Reversal
The sim concluded stall=100 > stall=600 (tighter is better, in swap-count).
**In live, the opposite may be true:** stall=600 = 150 sec (2.5 min) gives tokens more real time to find buyers. The old config (stall=600) had WR 28.3% and win/loss 1.35x; the new config (stall=100) has WR 15.9% and win/loss 1.04x. The direction is consistent with the hypothesis, though mcap band and other levers also changed (confounding).

### Meta-Saturation Pressure (compounding factor)
When multiple positions share a narrative category, `apply_pressure()` halves the stall window: 100 → 50 ticks = **12.5 seconds**. With max_concurrent=10, this can fire easily, further reducing hold time.

---

## Root Cause #2: Curve Impact Not Modeled (SECONDARY)

### The Hidden Cost
Every trade pays ~**2.82% of position size** in bonding-curve round-trip impact:
- Buy 0.1 SOL → curve price goes up
- Sell 0.1 SOL → curve price goes down
- Net round-trip cost: ~0.00282 SOL (measured from 13 trades with consistent diff)

The cost **scales** with price move: small moves ≈2.8%, large moves (15-20%) ≈8-11%.

### Break-Even Shift
| | Sim | Live |
|---|---|---|
| Break-even MFE | ~0 bps | ~280 bps (2.82%) |
| Trades that sim counted as "wins" at 1-2% move | Winners | **Losers** (below 2.82% curve cost) |

### Evidence from Tape
- Average trade PnL: **-0.00276 SOL ≈ curve cost** — entry-to-exit price movement averages to ZERO
- 54% of went-green losers had MFE < 200 bps — below the 2.82% break-even
- Only 16% of went-green losers had MFE 200-500 bps — marginal candidates
- The 2.82% cost is the difference between the sim's 42% WR and live's 16% WR

---

## Root Cause #3: CVD Noise Sensitivity (TERTIARY)

### The Problem
`cvd_hold_frac_bps=8000` (80%) requires CVD to stay above 80% of peak.
- **Sim:** CVD accumulates over 100+ swaps → a 20% drop requires many sellers → meaningful signal
- **Live:** CVD accumulates over 2-3 swaps → one sell after one buy = >20% drop → **noise, not signal**

### No Warm-Up Period
Confirmed from `position.rs:865`: `cvd_dead = cvd_peak > 0 && cvd < cvd_peak * 0.80`
There is **no minimum swap count** before cvd_dead can fire. The 2nd swap after entry (if it's a sell) can trigger ThesisInvalidation.

### The Contradiction
- Sim: cvd_hold=80% is the **BEST single lever** (PF 1.05 → 1.17, the largest improvement)
- Live: cvd_hold=80% is the **secondary killer** (noise-triggered exits on sparse early swaps)
- The sim's #1 improvement is live's #2 problem — because CVD needs swap-count richness to be meaningful

### Never-Green Losers
28 losers (42% of all trades) had MFE=0 (price never went above entry). For these:
- `stalled = FALSE` (requires mult > 10000 = in profit)
- The ONLY exit trigger is `cvd_dead`
- One sell after the entry buy drops CVD >20% → immediate exit
- These trades never had a chance to find buyers

---

## Entry-Side Analysis

The entry funnel is extremely selective: **1 admitted / 1346 candidates (0.07%)**.
- `universe_window_ticks=24` = 6 seconds in live
- `universe_min_entities=15` = 15 distinct buyers in 6 seconds = 2.5+ buyers/second
- Only viral moments qualify

**Entry quality is NOT the problem.** The tokens that pass are in momentary viral bursts.
The problem is that after entry, the burst pauses (naturally), and within 25 seconds the stall timer fires. The token may resume climbing 30-60 seconds later — we already exited.

---

## Foregone Upside Quantification

| Metric | Value |
|--------|-------|
| Expected wins (sim 42% of 82 trades) | 34 |
| Actual wins | 13 |
| Gap | 21 trades that should have won |
| Avg win PnL | +0.00424 SOL |
| Avg went-green loss PnL | -0.00408 SOL |
| Per-trade swing | 0.00832 SOL |
| Estimated foregone PnL | 0.175 SOL |
| Actual net PnL | -0.226 SOL |
| Projected net if gap trades became wins | -0.051 SOL |

Even closing the win-rate gap partially would significantly reduce the losses. The 31 went-green losers (MFE > 0) are the prime candidates — they moved in our favor 1-3% but the stall timer killed them before they could extend the move.

---

## MFE Distribution of Went-Green Losers

| MFE Bucket | Count | % | Status |
|------------|-------|---|--------|
| 0-50 bps | 11 | 35% | Below curve cost — guaranteed loss |
| 50-100 bps | 6 | 19% | Below curve cost — guaranteed loss |
| 100-200 bps | 9 | 29% | Marginal — barely covers cost |
| 200-500 bps | 5 | 16% | Would be winners with more time |
| 500+ bps | 0 | 0% | — |

Only 16% of went-green losers had enough MFE to clear the curve cost — and the stall timer killed them before they could exit green.

---

## Summary of Divergence Causes

| # | Cause | Severity | Affected Levers |
|---|-------|----------|---------------|
| 1 | Tick semantics mismatch (swap-count vs wall-clock) | **CRITICAL** | stall_ticks, max_hold_ticks, universe_window_ticks, moon_bag_window |
| 2 | Curve impact not modeled in sim | **HIGH** | All PnL-based thresholds; break-even shifted from 0% to 2.82% |
| 3 | CVD noise on sparse early swaps | **MEDIUM** | cvd_hold_frac_bps (80% too tight for 2-3 swap sample) |
| 4 | Meta-saturation pressure halves stall | **LOW** | stall_ticks (100→50 = 12.5 sec under pressure) |
| 5 | Entry window too short (6 sec) | **INFO** | universe_window_ticks (selective but may filter good tokens) |

---

## What This Means for the Sim's Validity

The walk-forward sim is **NOT invalid** — its relative rankings are likely correct:
- mcap 20-50 IS better than 118-154 (this is a regime property, not tick-dependent)
- Higher entity bars DO help (wash trading screening is swap-count-independent)
- The trail widening formula IS correct (it's price-based, not tick-based)

But the sim's **absolute projections are unreliable** for live because:
- stall_ticks and cvd_hold_frac are tick/count-dependent and their optimal values are WRONG for wall-clock semantics
- The break-even threshold is 2.82% in live, not 0% as the sim assumed
- The sim's PF and WR projections are inflated by the missing curve cost and inflated hold times

---

## Recommendations (for Alon's decision — NOT applied)

1. **The sim needs to be re-run with wall-clock tick semantics** — convert tick parameters to seconds and re-optimize. The current sim optimizes for the wrong unit.
2. **Curve impact must be modeled** — add the 2.82% round-trip cost to the sim's PnL calculation. Without it, the sim's WR/PF projections are meaningless for live.
3. **stall_ticks needs to be calibrated in SECONDS, not swap counts** — 25 seconds is too short for memecoins. Consider 120-300 seconds (480-1200 ticks at 250ms).
4. **cvd_hold_frac needs a warm-up period** — require a minimum swap count (e.g., 10-20 swaps) before cvd_dead can fire, to prevent noise-triggered exits.
5. **Alternatively: add a minimum-swap gate before thesis invalidation** — don't exit until the position has seen at least N swaps, regardless of CVD/stall signals.
6. **universe_window_ticks should be increased** — 6 seconds is too short to assess token quality. Consider 60-120 seconds (240-480 ticks).

---

*Investigation by Hermes Agent — 2026-08-11*
*Method: Systematic analysis of all ~80 config levers against 82-trade live tape*
*No config changes made per Alon's directive: "Report on it only"*
