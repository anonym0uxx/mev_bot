# Strategy Review — Pump.fun Bonding Curve Momentum Backrunning

*2026-03-29 | Principal MEV Strategy Assessment*

---

## Q1: Is This Fundamentally Viable?

**No — not at current parameters. Yes — conditionally, with structural changes.**

The aggregate numbers are damning: 54.3% WR against a ~65% break-even, negative Kelly, 158% fee drag. At 0.10 SOL position size, this is a guaranteed bleed.

But the data contains a buried signal: the buysAfterEntry≥1 cohort at 80.2% WR is a legitimately profitable edge. The problem is that 92.9% of your trades are the buysAfterEntry=0 cohort at 39.1% WR — these are pure noise trades that drown the signal.

You're not running one strategy. You're running two: a profitable momentum-confirmed strategy (7.1% of trades) and a losing coin-flip strategy (92.9% of trades), blended into a net loser.

**Minimum viable parameters:** At 0.50 SOL position size (0.4% fee drag), with entry gates that eliminate most buysAfterEntry=0 trades, you need ~55% WR on a 5% TP / 1.5% SL profile. The ≥1 buysAfter cohort already clears this by 25 points. The strategy is viable — your filter is broken.

---

## Q2: Single Highest-Impact Change

**Answer: (c) Tighter entry gates. Not close.**

This isn't an optimization problem — it's a filtering problem. 92.9% of your trades are entries where zero subsequent buys occurred, running a 39.1% WR. These trades are destroying your edge.

ShredStream (b) is irrelevant if you're entering bad trades 80ms faster. Position size (a) reduces fee drag but amplifies losses on a negative-EV subset. Dynamic Jito tips (d) are second-order optimization.

**The move:** Gate on pre-entry momentum signals that predict buysAfterEntry≥1. You already have the outcome data — reverse-engineer the entry conditions. Candidates: velocity of buys in the 1-3 seconds before entry, bonding curve % filled, unique wallet count, SOL volume in the preceding window. You need features that discriminate between "this token has momentum that continues" vs. "this was a single buy that dies."

Reducing pass rate from 0.9% to ~0.1-0.2% while capturing most of the buysAfterEntry≥1 cohort would flip the entire P&L. Do this before touching anything else.

---

## Q3: Optimal Position Size

**0.50 SOL is the correct target. Here's the math.**

| Position Size | Fee Drag | Net TP (at 5% gross) | Net SL (at 1.5% gross) | Break-even WR |
|---|---|---|---|---|
| 0.10 SOL | 2.0% | +3.0% | -3.5% | 53.8% |
| 0.25 SOL | 0.8% | +4.2% | -2.3% | 35.4% |
| 0.50 SOL | 0.4% | +4.6% | -1.9% | 29.2% |
| 1.00 SOL | 0.2% | +4.8% | -1.7% | 26.2% |

At 0.50 SOL, fee drag becomes negligible (0.4%) and break-even WR drops to ~29% — well below even your worst cohort. The jump from 0.10→0.50 captures 80% of the fee-drag benefit.

**Why not 1.0 SOL:** Marginal improvement is small (29.2%→26.2% break-even), but you double slippage risk on pump.fun bonding curves, increase Jito tip competition, and amplify losses during the filter-tuning phase. The risk/reward of the extra 0.50 SOL doesn't justify it until the filter is proven.

**Sequence:** Fix filters first at 0.10 SOL to validate the edge, then scale to 0.50 SOL once buysAfterEntry≥1 capture rate is confirmed.

---

## Q4: Gate Pass Rate — Filter More Aggressively

**Filter MORE aggressively. Target 0.2-0.3% pass rate.**

Current state: 0.9% pass rate → 897 trades → 92.9% are buysAfterEntry=0 (losers). Your filter passes almost everything that vaguely looks like momentum, and the vast majority is noise.

The buysAfterEntry≥1 cohort is 7.1% of 897 = ~64 trades. If your improved filter could capture 50-60 of those 64 while rejecting 80%+ of the buysAfterEntry=0 trades, you'd get:

- ~50-60 winning trades (80% WR) + ~160 noise trades (39% WR) = ~220 trades
- Pass rate: ~0.2%
- Blended WR: ~50 profitable momentum trades + ~63 noise wins ≈ 113/220 ≈ 51% — still not great

The real target: get buysAfterEntry=0 entries below 70% of total trades. That means a pass rate around 0.15-0.25%, producing ~100-150 trades with a blended WR above 60%.

**Don't go to 0.1%.** You need volume for statistical significance and to refine the model. At 0.1% pass rate you'd get ~100 trades total — insufficient for rapid iteration. Target 0.2-0.3%, iterate on filter features for 2-3 days, then tighten further once you have signal clarity.

---

## Summary: Priority Stack

1. **Build buysAfterEntry predictor features** — this is the entire edge
2. **Tighten gate to 0.2-0.3% pass rate** — kill the noise trades
3. **Scale to 0.50 SOL position size** — only after filter is validated
4. **Then** consider ShredStream, dynamic tips, and other optimizations
