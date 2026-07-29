# BAND THESIS — optimising the bot for the $9k–$20k pump.fun band (2026-07-28)

> **ERRATUM — 2026-07-28 (re-pin #26, COST-MODEL UNIFICATION), CHASED FORWARD TO re-pin #27
> (2026-07-28, DEPTH + MOVE PROVENANCE).** Every golden-tape absolute below is superseded, twice.
> At #26 the engine carried two disagreeing round-trip cost models and used one to DECIDE and the
> other to BOOK; `crates/pump-quant-app/src/cost_model.rs` is now the single authority for both.
> At #27 the fixtures themselves were corrected: they had declared a payout depth the curve does
> not escrow (`real_sol = virtual_sol − 30 SOL`, a **30×** overstatement near launch), and the
> confirmed-set eviction key reordered under the corrected depth.
>
> | quantity | published | re-pin #26 | **re-pin #27 (live)** |
> |---|---|---|---|
> | golden net (lamports) | 8,124,568 | 16,778,896 | **31,111,528** |
> | golden digest | — | 6163272398497391826 | **13693021370354439552** |
> | promoted / admitted / rejected | 504 / 13 / 457 | 504 / 12 / 447 | **504 / 11 / 448** |
> | universe_filtered | 72 | 72 | **72** |
>
> The book roughly quadrupled across the two re-pins and **not one qualitative conclusion about
> edge changes**: it is now 11 trades in 5 distinct markets, still statistically indistinguishable
> from zero (integer t² < 4), and still dominated by end-of-tape force closure. Read every absolute
> below as history under this header; the surviving verdicts are re-argued explicitly, not assumed
> (A-13(5)). Live pins: `crates/pq-regression/src/baselines.rs`.

**Mandate (operator, verbatim intent):** the engine appears to enter on raw unhashed numbers.
Shouldn't it holistically examine the signals, build a score, rank the trade's potential upside, and
enter — then exit on algorithms too? Scrutinise end to end from a principal memecoin quant
architect's seat and build the thesis for optimising the whole bot for positive net SOL on
**$9k–$20k market cap** targets.

**Bottom line, up front.** The premise needs one correction and then it is right, and the corrected
version is more actionable than the original. The hash is in the **test fixture**, not the engine —
the engine never sees it. But the engine *does* have the defect the question is reaching for, in a
sharper form: it already scores and ranks in two places, and **neither of them is allowed to touch
the number that decides whether a trade is worth taking.** This document builds the fix, prices the
$9k–$20k band exactly, and ships both DISARMED because the corpus that would arm them does not exist
yet.

---

## §1. The correction, because it changes what to build

`main_scalp` — the hash-driven trajectory generator — lives in
`crates/pump-quant-app/tests/tape_golden/mod.rs`. It is **synthetic test data**. The engine has no
access to it, no knowledge of it, and would behave identically if it were deleted.

What `docs/EDGE_PROVENANCE_2026-07-27.md` established is narrower and worse than "the engine is
naive": the *fixture* contains no information linking observables to outcomes, so **it cannot
measure whether the engine's sophistication is worth anything**. The engine may be brilliant or
useless; the golden tape returns the same verdict either way.

So the corrective action is not "add scoring to a naive engine." It is (a) find where scoring is
already happening and why it does not reach the money, and (b) stop asking a fixture to answer a
question it structurally cannot.

## §2. What the engine actually does today — traced, not assumed

There are already **two** ranking mechanisms:

| mechanism | granularity | built from | what it decides |
|---|---|---|---|
| `Candidate::discovery_score` | **per candidate** | the discovery lanes' signals | *promotion ordering* — which candidates the gate looks at (`evaluate()`, sorted descending with a §71 union-preservation quota) |
| `Engine::conditional_edge_bps` | **per lane** (~6 numbers) | realized per-lane returns, shrunk toward the prior | *slot arbitration* — ranking of already-admitted pending entries by `expected_net` |

And then the actual admission decision:

```rust
let band = size_band(
    cfg.gate_expected_move_bps,          // <-- GLOBAL CONSTANT
    cfg.gate_base_fixed_lamports,
    cfg.gate_fail_rate_bps,
    cfg.gate_protocol_bps,
    cfg.gate_margin_bps,
    conf.numeric.liquidity_lamports,     // per-candidate: COST / capacity
    &impact,
    conf.sellable_depth_lamports,        // per-candidate: COST / capacity
);
```

**Every per-candidate input is a cost or capacity term. The benefit term is one constant for every
token in the universe.** `discovery_score` is explicitly barred from the economics — `engine.rs`
carries the comment *"expected net SOL, never raw discovery score"* — and that bar is correct, because
a salience score is not a return forecast. But the bar leaves the money decision with a constant, and
`conditional_edge_bps` cannot fill the gap: six lane-level numbers cannot distinguish two tokens
surfaced by the same lane, which is the distinction that matters.

**So the honest statement of the defect is: the system has rich per-candidate information and a
per-lane learner, and the admission economics can see neither.**

## §3. Why a composite score is the wrong fix, stated before building anything

The obvious response is to weight the eight signals we already compute into one number. **That is
strictly worse than the constant it replaces**, and the reasoning matters more than the conclusion:

1. **A hand-weighted score is still a constant** — the weights are. It relocates the arbitrariness
   without removing it.
2. **It is a constant that looks principled**, which makes it much harder to retire. A disappointing
   result invites a re-weighting rather than a retirement, and the search never terminates.
3. **It adds a large overfitting surface** to a system whose only arbiter is a fixture that cannot
   measure alpha (§1). Every "improvement" would be fitted to a hash.

`docs/STRATEGY_PERMUTATION_STUDY_2026-07-25.md` is the record of what tuning an uncalibrated shape
produces: a 135-configuration lattice whose best result was a fraction of one 0.1-SOL bite.

**The correct fix is an estimator whose *shape* is code and whose *values* must come from realized
outcomes — and which refuses to answer until they do.**

## §4. What was built

### 4.1 `curve_state.rs` — market cap for free

pump.fun's curve is constant-product on virtual reserves, so the platform's own market-cap
definition collapses:

```
k    = vsol · vtokens                    (invariant)
mcap = vsol · SUPPLY / vtokens           (platform definition)
     = vsol² · SUPPLY / k = vsol² / 32_190_000_000
```

**The token side cancels.** Market cap is a pure function of `vsol` — exactly like own-curve impact —
and `Features::liquidity_lamports` *is* `vsol`. The engine has carried the number needed to band by
market cap since the first commit, with no oracle and no extra decode, and never computed it.

Two anchors are asserted, because reproducing them from first principles is what says the constants
are the real program's: launch (`vsol` = 30 SOL) → **27.96 SOL** market cap; graduation (all
793,100,000 real tokens sold) → `vsol` **115.01 SOL**, i.e. **85.01 SOL raised** — the figure the
whole ecosystem quotes — at **410.88 SOL** of market cap.

### 4.2 `expected_move.rs` — the per-candidate estimate, empty on purpose

> **EXTENDED 2026-07-28 (same day, operator challenge).** The first cut of this module
> stratified on curve progress *alone*. The operator pushed back — *"should the admission
> economics see this? I think it should. A real quant would consider these datapoints"* —
> and was right. **§10 replaces the single-stratum design with an additive
> marginal-effect model over every signal available at gate time.** The paragraphs below
> describe the base term, which survives unchanged as one component of it.


A **stratified empirical estimator**: strata over curve progress, each holding `(n, Σ realized bps)`,
shrunk toward the cold-start prior with the same hierarchical partial pooling
`conditional_edge_bps` already uses, and **refusing below a sample floor** exactly as the episodic
brain's recall does (§46).

Ships with an **empty table**, so every lookup returns `Unknown`, `gate::decide` receives `None`, and
the constant is used — byte-identical to the pre-model engine.

**Why stratify on curve progress and nothing else — a sample-size argument, not a taste.** Graduation
runs at ~0.198% (arXiv:2607.02823, 832,941 launches), so any outcome worth conditioning on is rare. A
replay corpus yielding a few thousand clean episodes supports ~10 cells at 30+ observations each, not
72. `curve_progress × flow × age` would produce confident numbers assembled from four observations —
worse than the constant, because confidently wrong beats honestly arbitrary only in appearance.

Curve progress earns the single slot on four structural grounds no other available feature satisfies
together: it is exactly observable from `vsol` with no oracle; it is monotone in the token's own life
rather than a market covariate; it *determines our own execution cost*, since own-impact is
`notional·10_000/vsol`; and it is bounded by a **structurally defined** terminal event — graduation —
rather than a fitted level.

### 4.3 The band law — wired, distinct, and off

`mcap_band_enable` refuses candidates outside `[lo, hi]` **before** the economic band, under its own
reject code (`OutsideMcapBand`, 9) so that band tuning can never contaminate the cost-floor reject
statistics.

## §5. The $9k–$20k band, priced exactly

At the operator conversion recorded here — **SOL ≈ $76** (CoinGecko/MetaMask, 2026-07) — and with the
verified curve constants:

| target | market cap | `vsol` reserve | curve progress | own impact, round trip on 0.1 SOL |
|---|---|---|---|---|
| launch | 27.96 SOL ($2,125) | 30.00 SOL | 0% | 67 bps |
| $5,000 | 65.8 SOL | 46.02 SOL | 19% | 43 bps |
| **$9,000** | **118.4 SOL** | **61.74 SOL** | **37%** | **32 bps** |
| **$20,000** | **263.2 SOL** | **92.04 SOL** | **72%** | **22 bps** |
| graduation | 410.9 SOL ($31,227) | 115.01 SOL | 100% | 17 bps |

**Three findings, one of them unwelcome.**

**(a) "Low market cap" is misleading — this band is the MIDDLE of the curve.** $9k–$20k is 37%–72% of
the way to graduation. Every token in it has already survived the phase where the overwhelming
majority die. That is a real conditional-survivorship advantage and it is the strongest argument for
the band. It is also entirely pre-graduation, so no migration event can fire mid-hold.

**(b) The band is worth 46 bps of round-trip impact, and that is all it is worth.** Own impact falls
from 33 bps a leg at launch depth to 10 bps at $20k. Real, computed, not fitted — but it is ~15% of a
~300 bps round trip.

**(c) THE UNWELCOME ONE: no pre-graduation band can reduce the fee.** pump.fun's fee is tiered on
SOL-denominated market cap and the first tier break is at **420 SOL**. Graduation happens at **410.88
SOL**. The tier break sits **9 SOL of market cap above the end of the curve**, so *every point on
every bonding curve pays the top 1.25%-per-trade rate*. This retires the "trade a cheaper fee tier"
lever from `EDGE_PROVENANCE §7` for all pre-graduation trading: fee relief begins only after
migration. Pinned in `curve_state::tests::no_pre_graduation_band_can_reduce_the_fee`.

**Resulting cost floor in the band: ~292–302 bps round trip on a 0.1 SOL clip.** A trade must clear
~3.0% gross to break even.

**And a confirmation that closes a line of enquiry.** The cost-minimising clip in this band is
**0.079 SOL at $9k and 0.096 SOL at $20k** — the 0.1 SOL operator floor is essentially exactly
optimal for the band the operator chose. Sizing stays closed in both directions; the floor and the
band are well matched, which is a genuine (if quiet) piece of good news.

### 5.1 Why the band is denominated in SOL, not USD

The operator stated the band in dollars; it is implemented in SOL, with the conversion recorded here
rather than read from a feed. Three reasons in order of weight: the **objective is net SOL** and every
venue cost is SOL-denominated, so a USD band makes behaviour a function of a price we do not trade;
an oracle is a **new fail-closed dependency**, so a price-feed outage would stop trading for a reason
unrelated to the trade (§18.2); and a USD band makes the journal digest a function of an external
time-varying quantity, which **would never replay** (§22). If SOL moves materially the operator
re-pins the SOL band. The bot never guesses.

## §6. Exit — the honest state, and why nothing changed here

The §32 flow-flip force-exit is the binding exit. `edge_provenance.rs` showed that on the golden tape
it is a **clock**, not a signal — `peak_round` is constant, so flow flips at the same point for a rug
and a 2.2× runner. That means every exit study in this repo measured "hold N more ticks," not
"distinguish shakeout from top."

For this band specifically there is one structural exit reference that is *not* a fitted parameter:
**graduation at 410.88 SOL of market cap.** It is where liquidity migrates and the fee schedule
finally changes. `curve_state::lamports_to_graduation` now exposes distance-to-graduation so a future
exit thesis can be written against a venue-defined level rather than a tuned one. **No exit default
was changed in this commit**, because changing an exit rule on evidence from a corpus that cannot
price exits is precisely the error A-13(4) names.

## §7. Verdict, per the pre-registered rule

Both laws ship **DISARMED**, and the reasoning differs between them:

* **The band law** clears P1 (seed-only) and P2 (the impact arithmetic is real and computed). It
  **cannot be evaluated on P3** — whether the band contains *better tokens* — because band selection
  is a claim about which population to trade, and no corpus we own can distinguish a good token from
  a bad one. The golden tape spans $2.1k–$10.7k of market cap and overlaps the operator band only in
  a sliver at its floor; arming it there admits almost nothing and books zero, which is a degenerate
  outcome rather than a verdict. `mcap_band_laws.rs` pins that so a future arming has to trip an
  explicit guard rather than slide in on the cost argument alone.
* **The expected-move model** cannot be armed at all until a table exists, and a table requires
  realized outcomes stratified by curve progress from live or replay data.

**This commit contains no alpha, and says so.** It contains the correctly-shaped, correctly-guarded
place that alpha goes, the exact arithmetic of the band the operator chose, and the discipline that
stops us pretending we have some.

## §8. What would arm each of these — the measurement, stated precisely

1. **Expected-move model.** From a replay corpus with the full launch universe (not survivors), for
   each decision point record `curve_progress_bps` and the realized forward return over our actual
   holding distribution. Fill `MoveTable`. Arm only if a stratum's shrunk estimate clears the ~300 bps
   band cost floor with the A-11 leg set satisfied on a pre-existing corpus.
2. **Band law.** The same corpus, stratified by curve progress, reporting **realized per-trade net in
   each band**. The question is empirical and narrow: does the $9k–$20k stratum's realized net per
   trade exceed the adjacent strata's by more than one 0.1-SOL bite? The evaluator crate's
   `EntryZone` / `ZoneOutcomeRow` machinery — built and never wired — is the natural reporting
   surface for exactly this.
3. **Both depend on the same missing thing**, which is the recurring conclusion of every study in this
   repository: a real corpus. The builder (`tools/backtest/pump_replay_build.py`) exists, refuses
   without a universe manifest, and has never been run at scale.

## §9. Repo changes

`src/curve_state.rs` (new, 7 tests) — exact integer curve math: market cap, curve progress,
distance-to-graduation, band membership, integer sqrt with ceiling semantics at band edges.
`src/expected_move.rs` (new, 7 tests) — the stratified estimator, empty and refusing.
`tests/mcap_band_laws.rs` (new, 5 tests) — the two-sided band study and its guard.
`gate::decide` gains an `expected_move_bps_override: Option<u32>` parameter and the band check;
`GateReject::OutsideMcapBand` is a distinct selection code (9). Six `Config` fields added, all
defaulting to the disarmed state.

**Re-pin #25 — SEED-ONLY.** Adding `Config` fields re-seeds the §19 journal digest with zero decision
change: net 8,124,568, promoted 504, admitted 13, rejected 457, universe_filtered 72 — every one
unchanged. Digest → `13_150_420_781_254_346_145`.

---

## Sources

* pump-fun-sdk bonding-curve math — initial virtual reserves (30 SOL / 1,073,000,000,000,000), real
  token reserves (793,100,000,000,000), total supply, and the market-cap definition.
* CryptoSlate, *Pump.fun Review 2026* — live fee schedule: 1.25% total on the bonding curve (0.95%
  protocol + 0.30% creator), tiered on SOL-denominated market cap with the first break at **420 SOL**
  and 0.30% only above 98,240 SOL.
* Froglabs, *Pump.fun Fees Explained (2026)* — corroborates 1.25% per bonding-curve trade.
* Kamat, A. U., arXiv:2607.02823 — graduation 0.198% over 832,941 launches; Cox concordance 0.858.
* CoinGecko / MetaMask, 2026-07 — SOL ≈ $75.71–$76.33, the conversion recorded for the band.


---

## §10. Letting the admission economics see every signal — the marginal-effect model

**The operator's challenge, and the concession.** §2 established that the engine holds
rich per-candidate information and a per-lane learner, and that the admission economics
can see neither. §4.2 then closed only part of that gap: a base term stratified on curve
progress. The operator's response was that a real quant *would* consider the other
datapoints, and that the admission economics should see them.

That is correct, and the first cut under-built it. What follows is the corrected design.
One thing does not move, though, and it is the reason this is a redesign rather than a
capitulation.

### 10.1 What does not change: signals reach money only through calibration

`Candidate::discovery_score` is, by its own documentation, *"raw discovery score in
caller-defined fixed-point units"* — an **ordinal salience rank**. The gate's arithmetic
is in **basis points of expected return**. Feeding one into the other is not aggressive,
it is *dimensionally meaningless*: it produces a number with no interpretation that
nonetheless moves real size.

What a desk actually does is two steps, never one: signals → a **calibrated forecast of
expected return**, each coefficient fitted on realized outcomes; then forecast → the
economic decision. Collapsing those two steps into a hand-weighted composite is how the
arbitrariness gets smuggled into the weights, where it is harder to see and much harder
to retire.

So: **yes, every signal reaches the admission decision. Through a coefficient it earned.**

### 10.2 Why marginal effects and not a joint table — the arithmetic

The obvious way to condition on many signals is to stratify on their cross-product. It is
unaffordable, and by a margin that is not close:

| scheme | cells | episodes needed at a 30-observation floor |
|---|---|---|
| joint (curve × 4 signals × 5 bands) | 5,625 | **168,750** |
| **marginal (additive lifts)** | **29** | **870** |

**194× fewer.** A replay corpus of ~50,000 launches yields on the order of a few thousand
tradeable episodes — comfortably enough for the marginal model, nowhere near enough for
the joint one. A joint table fed that corpus would not fail loudly; it would answer
confidently from four observations a cell, which is strictly worse than the constant it
replaced. Pinned in
`expected_move::tests::the_marginal_decomposition_costs_194x_less_data_than_a_joint_table`.

The estimate therefore decomposes:

```text
  expected_move_bps = base(curve_progress)      <- must be calibrated, or the whole thing refuses
                    + Σ lift(signal_band)       <- each earns its place separately
```

`lift` is a signal band's realized mean **minus the global realized mean** — the marginal
*excess* return associated with being in that band, which is the only quantity it is
legitimate to add to a base.

### 10.3 The asymmetry that makes wiring everything safe

* The **base** must clear its own sample floor or the entire estimate refuses and the
  gate falls back to the configured constant. *No base, no opinion* —
  `a_calibrated_signal_cannot_rescue_an_uncalibrated_base` pins that a rich signal cannot
  paper over an empty stratum.
* Each **lift** contributes only if *its own* band clears the floor. An uncalibrated
  signal contributes **exactly zero** — not a guess, not a neutral default.

That second property is load-bearing and is why the engine now presents *every* signal it
holds at gate time without any risk being added:
**adding signals cannot add fabricated edge, only earned edge.**
`uncalibrated_signals_contribute_exactly_zero` asserts that presenting signals the table
cannot price changes the output not at all.

A related distinction the type system enforces: an **unobserved** signal is not a
zero-valued one. `SignalObs` carries `Option<band>`, and a missing measurement contributes
nothing rather than looking like a weak reading (§6.4/§18.2).

### 10.4 The known flaw, named rather than hidden

Marginal effects **double-count correlated signals**. Buy pressure and unique buyers are
surely correlated; if each marginally carries +200 bps, adding both claims +400 where the
true joint lift might be +250. Three guards, in increasing bluntness:

1. each single lift is clamped to ±600 bps;
2. their sum is clamped to ±1,000 bps — deliberately far below the sum of the individual
   caps, precisely because correlated marginals over-add;
3. the total may never exceed the base itself, so **signals modulate an estimate and can
   never manufacture one**.

The principled fix is **sequential residual calibration** — fit signal *k* against what
signals *1…k−1* left unexplained — which removes the double-count exactly but needs both
an ordering and more data than a first corpus will supply. It is the documented upgrade
path, not a substitute for the caps.

### 10.5 What is wired, and what it costs

Presented to the estimator at gate time, straight off the candidate's confirmed feature
snapshot: **buy pressure**, **unique buyers**, **market age**, and a **holder
concentration** slot the caller fills (kept caller-supplied because
`ConcentrationReading` distinguishes *measured-low* from *unknown*, and that distinction
must not be flattened into a numeric bucket).

Every table is empty, so today every one of them contributes zero and the gate is
byte-identical — **the golden digest does not move for this change**, since no `Config`
field was added and no decision path is reachable while the table is empty.

### 10.6 The honest status

The structure now answers the operator's question with *yes*: the admission economics can
see every signal the engine computes. **What it still cannot do is tell you what any of
them is worth**, because that requires realized outcomes from a real corpus — the same
blocker every study in this repository has reached.

The difference is that the blocker is now purely a data problem. When a replay corpus
lands, filling `MoveTable` is a mechanical operation over episodes already being sealed,
and the A-11 leg set decides whether it arms. There is no further design work between
here and a calibrated per-candidate expected move.
