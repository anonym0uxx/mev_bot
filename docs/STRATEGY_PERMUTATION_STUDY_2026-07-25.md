# STRATEGY PERMUTATION STUDY — exhaustive entry/exit search + Tsetlin assessment (2026-07-25)

> **ERRATUM #2 — 2026-07-28 (re-pin #26, COST-MODEL UNIFICATION). The `k = 5` verdict
> STANDS; the sentence "it turns the book negative" below is retracted, and Erratum #1's
> reading of it was wrong.**
>
> Erratum #1 (below) recorded that `k = 5` had gone from destroying 85% of the book to turning
> it negative outright, and concluded "the `k = 5` verdict is now stronger than published".
> That was a misreading, and it is worth naming because it is an easy one to make twice.
>
> The engine now prices a round trip through one authority (`cost_model.rs`) instead of two
> disagreeing ones: the impact denominator is derived per candidate from each market's own
> reserve, a phantom ~200 bps of "bid/ask spread" that a constant-product AMM cannot charge is
> deleted, 2,039,280 lamports of ATA rent is modelled as the refundable deposit it is, and a
> 150 bps first-sell penalty that was own-impact under another name is deleted. The golden book
> roughly doubles.
>
> | quantity | published (2026-07-25) | erratum #1 (2026-07-27) | honest (2026-07-28) |
> |---|---|---|---|
> | golden net, shipped `k = 1` | 15,410,801 | 8,124,568 | **16,778,896** |
> | golden net at `k = 5` | 2,322,301 | −3,223,175 | **+5,309,323** |
> | **`k = 5` HARM (`k=1` − `k=5`)** | **13,088,500** | **11,347,743** | **11,469,573** |
> | best golden gain over all `k` | +257,400 (`k = 2`) | +207,252 (`k = 2`) | **+177,199 (`k = 2`)** |
>
> **Read the HARM column, not the level column.** `k = 5` does not "turn the book negative" and
> never did anything of the sort *to* `k`; it forfeits ~11.4 million lamports of upside on this
> tape, and it has forfeited approximately that same amount under all three cost models. The sign
> of the residual is a statement about how big the book is, not about how harmful `k` is. Under
> honest costs `k = 5` is **1.1% MORE harmful than Erratum #1 measured**, which also refutes the
> natural hypothesis that the old harm was an artifact of the phantom costs — deleting them made
> `k = 5` slightly worse, not better.
>
> **What DOES change is §3 below: the law no longer passes on its own tape either.** `tape_flow`
> declared **0.26 SOL pools** against a 0.1-SOL clip. That was invisible while the gate's impact
> model was a config constant; once it was derived from the declared reserve, the tape refused
> every candidate and both sides read `0`. At real depth (32 SOL) the tape trades and `k = 5`
> **loses on its own happy side**: 104,607,333 → −52,846,461, while the mirror also worsens
> (−54,978,642 → −94,186,083). The admit count falls 63 → 36: at realistic size the position slots
> are the binding resource and patience is paid for in round trips not taken. §3's claim that "if
> that tape were the arbiter, this law would ship armed" is **withdrawn**. P1, P2 *and* P3 now
> fail.
>
> Every relative verdict in §4's table is superseded by `tests/flow_persistence_laws.rs`, which
> holds the live numbers.
>
> **Erratum #1's closing paragraph is also retracted** where it says "a uniform depth mispricing
> moves both arms together, preserving sign and ordering". That is true only while the GATE cannot
> see depth. Once it can, a depth mispricing does not scale both arms — it REFUSES both arms, and
> a tape that admits nothing preserves nothing.

> **ERRATUM #1 — 2026-07-27. Every absolute lamport figure below is superseded; every verdict stands,
> and one of them got STRONGER.**
>
> This study ran against a golden tape whose pools were **0.12–0.47 SOL** against our 0.1-SOL
> minimum clip, with fills at the print — our own order was 21–83% of the pool and was charged
> nothing for it. The tape now carries real pump.fun depth (30 SOL virtual at launch) with own-curve
> impact charged on both legs. Golden net **15,410,801 → 8,124,568**. See `docs/BACKTEST.md §9`.
>
> Corrections to the specific numbers this study turns on:
>
> | quantity | as published (2026-07-25) | honest (2026-07-27) |
> |---|---|---|
> | golden net, shipped `k = 1` | 15,410,801 | **8,124,568** |
> | golden net at `k = 5` | 2,322,301 (positive) | **−3,223,175 (NEGATIVE)** |
> | best golden gain over all `k` | +257,400 (`k = 2`) | **+207,252 (`k = 2`)** |
>
> **The `k = 5` verdict is now stronger than published.** Under fictional depth, `k = 5` merely
> destroyed 85% of the book; under honest fills it turns the book negative outright. The
> pre-existing-corpus arbiter rule rejected it either way, but the harm it rejects is larger than
> we knew. `thesis_persist_obs` stays DISARMED at `k = 1`, and
> `tests/flow_persistence_laws.rs` pins the corrected magnitude.
>
> The relative verdicts survive because each is an A/B on one tape with depth held fixed: a uniform
> depth mispricing moves both arms together, preserving sign and ordering. What does not survive is
> reading any absolute net as an economic result — the golden book is smaller than one 0.1-SOL bite
> and is a regression fixture, not a forecast.

**Mandate:** test every rational, factually-supported low-cap memecoin scalping permutation
end-to-end across entry and exit, fine-tune and A/B every identifiable variant to find the
absolute maximum net-SOL edge; research every external source (NIH/PMC, arXiv) for
cross-reference; and assess Tsetlin Machines (PMC12231370) as a strategy helper.

**Bottom line:** the search found **one genuinely large lever** — and proved we cannot yet aim it.
No parameter change ships. The lever is built, disarmed, guarded, and handed to the server with
the exact live measurement that would justify arming it. Everything else is a documented negative.

---

## 1. The structural finding that reframed the whole search

The prior study found every price-based exit knob decision-INERT. This study found *why*, and it
is not a defect in the knobs — it is that **something upstream always binds first**.

The binding rule is the §32 thesis force-exit: it fires the instant windowed order-flow imbalance
turns net-sell. An exit-reason census confirms it — positions leave via `ThesisInvalidation` and
`TakeProfitLadder` only, with **zero** hard-stop, trailing-stop, time-stop or force-close fills.
The flow flip always fires before price retraces far enough to reach any price stop, so the entire
price-stop geometry is downstream of a rule that pre-empts it. That rule was a **hardcoded
constant, never a config knob** — which is precisely why no previous sweep could reach it.

That makes the real question: *is exiting on the first flow flip correct?*

---

## 2. External research — the literature says our binding rule is the weakest possible read

Two independent lines converge, and both point the same way:

**arXiv 2606.16269** (Lillo–Mike–Farmer, `γ = α − 1`): trade signs are long-memory because
metaorder lengths are Pareto-distributed. Persistence lives in **event time**, not wall-clock, and
the paper is explicit that *"a single flow sign flip carries limited information independently…
predictive power derives from the persistent sequence of same-signed orders within a meta-order,
not isolated trades."*

**Kaminski & Lo**, *J. Financial Markets* 18:234–254: a stop rule's "stopping premium" is
**negative** unless the trigger predicts *persistent* adverse drift. Under a random walk it is
always negative. Their equity data shows daily- and weekly-frequency stops already lose money;
only monthly+ is positive. A **per-print** flow stop sits far further into the losing regime.

Supporting context, all cross-checked: OFI's genuine predictive horizon is ~1–3 **seconds**
(arXiv 2602.00776), i.e. we apply a seconds-scale signal to a minutes-scale decision; pump.fun
runs resolve in minutes (median time-to-graduation 4.4 min, arXiv 2602.14860; 60% of migrated
tokens below 20% of migration price within 20 min, arXiv 2602.13480); and 82.8% of >100% runs
show manufactured-growth signatures with a median dump of −55.8% (arXiv 2507.01963).

**Disconfirming evidence was sought and found, and it is strong:** no published memecoin strategy
has positive out-of-sample expectancy. The best published classifier reduces losses to ~27–31%
versus 60.7% random — a *loss reduction, not a profit* (arXiv 2602.13480). Unconditional memecoin
exposure ran −78.7% over 2025–26 (SSRN 6292920). Lottery-like assets carry a systematic negative
premium (Bali–Cakici–Whitelaw, *JFE* 99:427–446). The widely-cited "73% of pump.fun traders are
profitable" figure is realized-PnL-only, explicitly excludes bagholders, and is driven by
attrition. **Nothing here supports optimism about the base rate; it supports discipline about cost.**

---

## 3. The experiment: flow persistence, tested two-sided

`thesis_persist_obs` (`k`) was added: require a **run of `k` consecutive adverse observations**, in
event time, before the force-exit fires. `k = 1` is the historical first-flip behaviour. Any
non-adverse observation resets the run, so it is a run-length gate, not a delay timer.

A two-sided tape (`tape_flow`) was built to the literature's structure, **byte-identical on both
sides up to and including the first adverse observation** — at the moment of decision the two are
indistinguishable, which is what makes the mirror fair. They diverge only afterward: on
`ShakeoutThenRun` the burst was noise and the market runs on; on `TrueTop` the burst *was* the top
and the market collapses. Competing lifecycle exits (CVD rollover, stall, precursor) were
neutralized so the tape measures the flow lever and nothing else; the hard stop and trailing stop
were deliberately left at shipped defaults so the mirror side genuinely pays for patience.

**On its own tape the law passes both bars:** `k = 5` gains **+152,694,678** on the happy side and
loses **−44,095,911** on the mirror — asymmetry **3.46×**, clearing the pre-registered 3× bar, with
a material gain. If that tape were the arbiter, this law would ship armed.

> **RETRACTED (Erratum #2, 2026-07-28).** This paragraph was measured on a tape declaring 0.26 SOL
> pools. At real depth `k = 5` LOSES on its own happy side (104,607,333 → −52,846,461) and worsens
> the mirror too. There is no tape on which flow persistence pays. See
> `tests/flow_persistence_laws.rs::the_mechanism_is_real_on_its_own_two_sided_tape`.

---

## 4. Why it does not ship: the pre-existing tapes overturn it

`k` swept across every pre-existing tape (all reused verbatim, none authored for this hypothesis):

| k | GOLDEN (representative) | B3-hazard | B7-happy | B7-unhappy | CONC-happy | FLOW-shake | FLOW-top |
|---|---|---|---|---|---|---|---|
| **1 (ship)** | **15,410,801** | 293,235,710 | 479,556,343 | 601,202,914 | **+16,567,514** | 13,170,840 | 13,170,840 |
| 2 | 15,668,201 | 293,235,710 | 482,140,552 | 604,305,163 | +16,567,514 | 13,170,840 | 13,170,840 |
| 3 | 13,023,101 | 293,235,710 | 482,140,552 | 604,305,163 | +16,567,514 | 13,170,840 | 13,170,840 |
| 4 | 2,322,301 | 293,235,710 | 482,140,552 | 604,305,163 | **−22,894,988** | 144,720,561 | −30,925,071 |
| **5** | **2,322,301** | 293,235,710 | 482,140,552 | 604,305,163 | **−22,894,988** | **165,865,518** | −30,925,071 |
| 8 | 12,663,401 | 293,235,710 | 482,140,552 | 604,305,163 | −3,571,503 | 36,535,209 | −65,684,622 |
| 12 | 5,733,401 | 293,235,710 | 482,140,552 | 604,305,163 | −8,392,202 | −9,200,211 | −6,447,807 |

> **SUPERSEDED (Erratum #2).** Every cell in the table above is measured under the retired split
> cost model and on hazard tapes whose sub-SOL depth made them vacuous under a derived impact
> model. `tests/flow_persistence_laws.rs` holds the live numbers. The paragraph below is retained
> as the historical argument; its conclusion is unchanged and its arithmetic is not current.

The `k` that wins on the purpose-built tape **destroys 85% of net on the representative tape**
(15,410,801 → 2,322,301) and **flips the concentration-hazard book from positive to negative**.
That is the textbook signature of a result fitted to its own fixture. On the two large tapes
(B3, B7) it is essentially inert — at most +0.6% on a 479–601M book. The best gain any `k`
produces on golden is **+257,400 lamports (k = 2), under 1.7% of the book.**

**A scale caveat that governs how every number above reads, stated plainly:** the golden tape's
*entire* book is 15,410,801 lamports — **smaller than one 0.1-SOL materiality bite (100,000,000)**.
Absolute bite-bars are therefore meaningless on that tape; materiality is judged *relatively* on
golden and *absolutely* only on the large hazard tapes. This cuts against the law, not for it.

**Joint lattice.** Because relaxing `k` is exactly what would *unbind* the price geometry, a
**135-configuration joint lattice** (`k` × trail × hard-stop × TP-margin) was swept on the golden
tape. Best of all 135: **+438,538 lamports — 1/228th of one bite.** Trail and hard stop remained
perfectly inert at every `k`. There is no joint configuration that earns.

**Verdict: DISARMED (`k = 1`), decisions byte-identical.** P1 and P2 both fail.

### Why the capability is kept rather than deleted
The mechanism is genuine and the largest lever either study has found — it moves net by ±85%. The
reason it cannot be aimed is a **missing measurement, not a disproven theory**: nobody, including
the published literature, knows the live base rate of *shakeout* vs *true top* at the first flow
flip on pump.fun. (The research sweep flagged this as the single most valuable missing number, and
noted it is computable from our own data.) So it ships disarmed and byte-identical — exactly as
LAW B7 and the concentration law did — and `arming_beyond_the_shakeout_threshold_is_harmful` pins
the harm so that arming it on the happy-path numbers trips a loud, explicit guard.

**The one live measurement that would justify arming it:** of positions whose flow first turns
net-sell, what fraction subsequently make a new high before a sustained reversal, and by how much?
If shakeouts are common and their continuation is large, `k > 1` earns; the tape above shows the
exact shape of the trade-off.

---

## 5. Tsetlin Machines — assessed, and declined (for now)

The linked paper (Elmisadr, Belaid & Yazidi, *Frontiers in AI*, 2025; PMC12231370) proposes an
asymmetric Tsetlin Automaton transition rule. **It does not support adoption**, on its own terms:
it is evaluated on Iris, Mushroom and MNIST — no time-series, no finance, and it states outright
that *"direct timing measurements were not captured."* Its internal validity is also broken: it
reports a classical Tsetlin Machine at **74% on MNIST** where the original literature reports
**98.2%**, so its comparative claims are measured in a regime where every model is non-functional.
Worse for us, its proposed method **adds Gaussian sampling and a normal CDF** — i.e. floating point
and RNG — the exact opposite of our §22 integer-only, RNG-free decision path.

**The underlying vanilla TM is a genuinely good architectural fit and deserves the credit:**
inference is pure integer/bitwise with **no floating point and no RNG** (training needs RNG, but
that is offline; we would ship a frozen bitmask), it is ~5 KB and L1-resident, runs in the low
hundreds of nanoseconds with constant tail latency, and its clauses read out as human-auditable
propositional rules — which suits a constitution-governed system unusually well.

**But the honest case against adopting it now is strong, and it is the same argument this study
keeps arriving at:**

1. **It would see exactly the information we already feed the brain.** Our ~104-bit thermometer /
   one-hot signature *is* the booleanization a TM requires. A TM would add a different inductive
   bias over identical features — zero new information.
2. **In a high-Bayes-error domain, swapping inductive biases over identical features is
   second-order.** The one substantive TM finance paper (arXiv 2607.06719, FX regimes) ablates
   exactly this: removing macro *features* cost ~23 points; swapping the *architecture* bought ~2.
   The lift lives in features, not classifiers.
3. **There is no published TM result demonstrating trading PnL** — the best finance paper has no
   backtest, no costs, no slippage, and an overall accuracy *below its own majority-class rate*.
4. **TM's headline advantages are ones we already bank** (integer-only, tiny, no FP, edge-ready).
   We would pay full complexity cost for benefits already held.
5. **A named failure mode bites us specifically:** a standard TM clause is disqualified entirely if
   even one literal fails, and our thermometer bits jitter at threshold boundaries by construction.
   Weighted-L1 recall degrades gracefully there; a TM clause silently stops voting.

**Recommended (cheap, no hot-path risk):** use a TM *offline as a rule miner* — train a small one
(100–400 clauses) on our existing signatures and labels, read out the highest-weighted clauses, and
treat them as human-auditable **candidate laws** for the constitution. That captures the
interpretability benefit with zero live exposure. Before any Rust is written, the falsifiable
go/no-go is a purged walk-forward comparison against the existing recall on **balanced accuracy AND
net PnL after fees** — and per the same standard every law here is held to, it ships only if it
beats both. Prior: it fails the PnL leg.

---

## 6. What changed in the repo

- **`thesis_persist_obs`** — new §32 flow-persistence lever. **Default `k = 1`: every decision
  number byte-identical to re-pin #21** (net 15,410,801 / promoted 504 / admitted 13 / rejected 457
  / universe_filtered 72). Bounded per-mint run state (§99), integer-only, no RNG.
- **`tests/tape_flow/`** — the two-sided flow-noise tape (fair mirror by construction).
- **`tests/flow_persistence_laws.rs`** — 4 tests: the disarmed default, the mechanism's reality on
  its own tape, its failure on every pre-existing tape, and the arming-harm guard.
- **Golden re-pin #22 — SEED-ONLY.** Digest `3_604_954_302_921_337_343` →
  `8_413_891_310_981_713_968`, moving *only* because §19 folds `fnv1a_64(format!("{cfg:?}"))` into
  the seed and `Config` gained a field. **No decision number moved.**

Green: golden_digest 8/8, flow_persistence_laws 4/4, law_permutation_sweep 6/6,
entry_exit_frontier 3/3, brain_laws 11/11, holder_concentration 19/19, engine_e2e 15/15,
pq-regression all suites, ci_gate PASSED.
