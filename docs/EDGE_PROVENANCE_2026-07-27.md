# EDGE PROVENANCE — is there anything that can actually move net SOL? (2026-07-27)

> **ERRATUM — 2026-07-28 (re-pin #26, COST-MODEL UNIFICATION).** Every golden-tape
> absolute below is superseded. The engine carried two disagreeing round-trip cost models
> and used one to DECIDE and the other to BOOK; `crates/pump-quant-app/src/cost_model.rs`
> is now the single authority for both. Golden net **8,124,568 → 16,778,896**, digest →
> `6163272398497391826`, admitted **13 → 12**, rejected **457 → 447** (promoted 504 and
> universe_filtered 72 unchanged). The book roughly doubled and **not one qualitative
> conclusion about edge changes**: it is still 12 trades in 4 distinct markets, still
> statistically indistinguishable from zero (|t| ≈ 0.19), and still 60% boundary artifact
> from end-of-tape force closure. Live pins: `crates/pq-regression/src/baselines.rs`.

**Mandate (operator, verbatim intent):** knowing the current position at +8,124,568 lamports, is
there anything that will *actually* — unbiased, factually, mathematically, not a faked positive
lever — move net SOL positive given the current algorithm? And separately: are the tests 1:1 with
data that could be pulled from real pump.fun memecoins?

**Bottom line, up front, in two sentences.** The +8,124,568 is not a positive result; it is a
statistically insignificant number (t = 0.18) produced by thirteen trades in **five** markets, one
of which supplies the entire book. And the operator's suspicion about the fixture is correct and
understates the problem: the golden tape contains, by construction, **zero** information linking
anything the engine observes to any outcome — so no admission rule, exit rule, or signal can beat
any other on it, and every "this knob is inert" verdict in this repository was decided by the
fixture before the strategy ran.

---

## §1. The pre-registered question

Written before any number below was measured: *is the golden book a product of SELECTION and TIMING
(skill the engine supplies), or of an authored outcome distribution multiplied by a trade count
(arithmetic the fixture supplies)?* If the latter, parameter work on this corpus cannot move net
SOL, and the search for edge must move to real data or stop.

Method: instrument the sealed per-trade ledger (`tests/edge_provenance.rs`), read the tape's own
generator, derive the cost floor from first principles, and cross-check every economic assumption
against external sources.

## §2. What the thirteen trades actually are

| mint | net (lamports) | mfe bps | mae bps | exit |
|---|---|---|---|---|
| 16827573748223463086 | −1,191,208 | 265 | 0 | StructureBreak |
| 5818873279696574491 | **+15,425,570** | 1,924 | 0 | StructureBreak |
| 3934396005217763095 | +5,321,951 | 927 | 0 | StructureBreak |
| 6489040902985583152 | **+29,401,643** | 3,311 | 0 | StructureBreak |
| 17889110506501282699 | −4,281,209 | 0 | −58 | StructureBreak |
| 3934396005217763095 | +4,463,321 | 840 | 0 | StructureBreak |
| 6489040902985583152 | −10,711,823 | 0 | −706 | StructureBreak |
| 17889110506501282699 | −10,606,519 | 0 | −700 | StructureBreak |
| 3934396005217763095 | −333,164 | 354 | 0 | StructureBreak |
| 5818873279696574491 | +7,886,765 | 1,188 | 0 | StructureBreak |
| 3934396005217763095 | −491,073 | 338 | 0 | StructureBreak |
| 3934396005217763095 | −11,682,870 | 0 | −823 | **TimeStop (forced)** |
| 5818873279696574491 | −15,076,816 | 0 | −1,158 | **TimeStop (forced)** |

Four facts fall out immediately, and each one bounds what this tape may be used to claim.

**(a) Five markets, not 512.** The thirteen admits are re-entries into five distinct mints. The
effective sample size of every A/B ever run here is five hash draws.

**(b) The book is one trade.** Remove the single best trade (+29,401,643) and the book is
**−21,277,075**. Remove the best two and it is **−36,702,645**.

**(c) It is statistically indistinguishable from zero.** Mean +624,967, standard deviation
12,311,627, **t = 0.183** against a ~2.18 threshold at df = 12. The 95% confidence interval on the
*total* is **[−88.6M, +104.9M]**. Clustering by market — the honest unit of independence — gives
t = 0.289 on df = 4. There is no sense in which +8,124,568 is a measurement of edge.

**(d) 77% of the book is a boundary artifact.** Eleven positions closed on the strategy's own
triggers for **+34,884,254**. Two positions were still open when the tape ran out and were
force-closed by `finalize()` for **−26,759,686**. Whether that loss is real depends entirely on
what those two markets did after the fixture stopped, which the fixture cannot say.

*(Fact (d) is not a parochial problem. The widely-cited "73% of pump.fun traders are profitable"
figure is computed on realized PnL and explicitly excludes wallets that never sold — CoinGecko says
so in its own methodology note, and concedes it "understates losses." Our fixture exhibits the same
bias in miniature, and quantifies it: 77%.)*

## §3. Why no amount of tuning on this corpus can help — the fixture carries no information

`tape_golden::main_scalp` computes a mint's entire trajectory from a multiplicative hash of its tag:

```rust
let h = m.wrapping_mul(0x9E37_79B9_7F4A_7C15) ... ;
let bucket = h % 1_000;          // outcome class: loser / mid / runner
```

No feature the engine reads enters this function — not liquidity, holder concentration, narrative,
creator history, whale flow, or alpha calls. The outcome is a hash of the identifier.

Order flow is worse than uninformative; it is a **clock**:

```rust
let peak_round = 2u64;                       // CONSTANT for all 512 mints
let rising = round <= 1 || round <= peak_round;
let signed_base = if rising { +... } else { -... };
```

`tests/edge_provenance.rs::order_flow_on_this_tape_is_a_clock_not_a_signal` proves it directly: the
flow-sign series is byte-identical for a rug and for a 2.2× runner, and flips after exactly nine
prints for every mint. And rounds 0–1 are *deliberately* identical across all mints (the generator's
own comment: "discovery cannot front-run which coin will pump").

The consequences are exact, not rhetorical:

* Admission cannot beat a random draw from the authored mix, because nothing observable at entry
  correlates with outcome.
* The §32 flow-flip exit — documented throughout this repo as "the binding exit" — is a **timer**.
  All eleven natural closes are `StructureBreak`, which is `ThesisInvalidation` under the brain's
  mapping. They all fire at the same point in each mint's life.
* Therefore the `thesis_persist_obs` (`k`) study was not measuring "shakeout vs true top." It was
  measuring "hold N more ticks on a fixed clock." The verdict (stay disarmed) is still correct, but
  the *reason* recorded in `docs/STRATEGY_PERMUTATION_STUDY_2026-07-25.md` — that the pre-existing
  corpus overturned it — is weaker than written: the corpus could not have supported it either way.
* Every "knob is decision-inert" result is a property of the fixture. Only cost and count could ever
  have moved net here, and cost and count are exactly what the A/Bs found moving.

**This is not a defect to repair by making the tape more realistic.** Depth realism (re-pin #24) was
worth doing because cost is objectively modellable. Alpha is not: any outcome distribution we author
is one we could then "discover," which is circularity, not evidence. The golden tape's correct job
is determinism, boundedness, and code-path coverage. It should never again be asked whether the
strategy earns.

## §4. The gate compares a measured cost against an assumed benefit

`gate::decide` sizes and admits from:

```rust
let band = size_band(
    cfg.gate_expected_move_bps,   // <-- a global CONSTANT
    cfg.gate_base_fixed_lamports,
    cfg.gate_fail_rate_bps,
    cfg.gate_protocol_bps,
    cfg.gate_margin_bps,
    conf.numeric.liquidity_lamports,     // per-candidate: cost/capacity
    &impact,
    conf.sellable_depth_lamports,        // per-candidate: cost/capacity
);
```

Every per-candidate input is a **cost or capacity** term. The **benefit** term,
`gate_expected_move_bps`, is one number for every token in the universe, forever (300 in
`dev_portable`, forced to 1,800 on the golden tape — an assumed 18% favourable move on every
candidate).

There *is* a learner — `conditional_edge_bps` shrinks a lane's realized mean toward the prior with a
pseudo-count — but it is **per-lane** (about six numbers), it only moves after
`expectancy_min_lane_trades` fills, and per its own doc-comment it conditions **§23 slot
arbitration**, i.e. ranking. It does not enter the admit/refuse economics.

So: everything this system computes — holder concentration, narrative velocity, creator state, whale
flow, alpha calls, the brain, the Tsetlin assessment — feeds discovery, ranking and sizing. **None
of it prices whether a trade is worth taking.** That is the single largest unpriced assumption in
the codebase, and it is the structural reason no parameter search has found anything: the searches
were over the cost side of an inequality whose benefit side is a constant.

## §5. The cost floor, derived rather than assumed

Round-trip cost on a clip of `S` lamports against a pool with `vsol` SOL-side reserves:

```
cost_bps(S) = fee_rt + 200_000·10_000/S + 2·S·10_000/vsol
              ^fee     ^priority+tip      ^our own curve impact (both legs)
```

| venue | fee rt | fixed | own impact | **total on 0.1 SOL** |
|---|---|---|---|---|
| bonding curve (1.25%/trade, 30 SOL) | 250 | 20 | 67 | **337 bps** |
| PumpSwap post-graduation (0.30%/trade, 85 SOL) | 60 | 20 | 24 | **104 bps** |
| **what our config currently assumes** | 450 | 20 | 67 | **537 bps** |

**Two results follow, one negative and one positive.**

*Negative, and it closes a line of enquiry.* The cost-minimising clip is 0.0548 SOL on a 30 SOL
curve. At the operator's 0.1 SOL floor the cost is 337 bps versus 323 bps at the optimum — a
**14 bps** difference, below the floor anyway. Sizing down is a dead end, arithmetically. Sizing up
is worse than a dead end: 0.5 SOL costs 587 bps and 1.0 SOL costs 919 bps, because own-impact grows
linearly in clip size. **This system has a hard capacity ceiling of roughly 0.1–0.2 SOL per trade on
a launch-depth curve, and no strategy improvement changes that.**

*Positive, and it is the largest arithmetically certain lever found.* Our `gate_protocol_bps = 450`
decomposes in its own comment as ~200 bps swap fee + ~55 bps LP/protocol/creator + **~200 bps
"bid/ask spread on a thin low-cap."** A constant-product AMM **has no bid/ask spread.** There is the
fee, and there is the curve — and we now charge the curve exactly (`curve_fill`, re-pin #24). That
200 bps term is either double-counting impact we already charge, or it is a *latency /
adverse-selection* cost mislabelled as spread and never measured. Either way it is an assumed number
sitting beside measured ones, which is precisely the A-13 defect.

On the golden book, 200 bps of 13 × 0.1 SOL notional is **26,000,000 lamports — 3.2× the entire
net.** This single unexamined constant is worth more than everything the strategy did.

## §6. What the outside world says, and it is not all bad news

The externally-verified picture is harsh on the base rate and encouraging on predictability.

**The base rate is brutal and getting worse.** A survival analysis of **832,941** pump.fun launches
observed 2026-05-08 → 2026-06-10 puts the pooled graduation rate at **0.198%** (Wilson 95% CI
[0.189%, 0.208%]) — a **3.18× decline** from the 0.63% reported a year earlier. Independent industry
reporting puts the 2026 collapse in the same place.

**But graduation is strongly predictable from pre-trade observables.** The same study reports a Cox
model **concordance of 0.858**, with effects that are enormous rather than marginal: launches
advertising Telegram graduate at **1.485% vs 0.166%** (an **8.94× lift**, hazard ratio 5.40, log-rank
p < 1e-300); launches advertising all three social channels graduate at **1.919% vs 0.110%** — a
**17.4× lift**. Initial market cap above the 30 SOL platform default (i.e. creator self-buy) carries
a hazard ratio of **4.51**, with the top quartile graduating at 0.634%.

That is the finding that matters most for this project. **Cross-sectional information demonstrably
exists in observables we can obtain before trading, and it is large.** It is not proof of a
profitable scalp — graduation is a 0.2% tail event, not a 3.4% move within a hold — but it is
external, replicated, well-powered evidence that the constant in §4 can be replaced with a real
conditional estimate.

**And the corpus you would train it on has its own trap.** The MemeTrans dataset (41,470 tokens,
200M+ transactions, Dec 2024 – Mar 2025, 122 engineered features, public) covers **only tokens that
migrated to DEX.** It excludes failed and abandoned launches by construction — the exact survivorship
bias `tools/backtest/pump_replay_build.py` refuses without a universe manifest. Used naively it
would train a model on the surviving 0.2% and report a fictional edge. It is usable only as the
positive class, with the launch universe reconstructed independently from `create` instructions.

## §7. The answer, ranked, with what would falsify each

Ordered by expected magnitude. "Measurable now" means without new data.

| # | lever | size | measurable now? | status |
|---|---|---|---|---|
| 1 | **Replace the constant `gate_expected_move_bps` with a per-candidate conditional estimate** | decisive — it is the entire benefit side of the gate | no | unfalsified; external evidence (concordance 0.858) says the information exists |
| 2 | **Trade post-graduation instead of on the curve** | 337 → 104 bps, a **3.2× cut in the hurdle** | partly — the arithmetic is certain, the opportunity set is not | unexplored |
| 3 | **Resolve the ~200 bps "spread" term** | 26M lamports on the golden book, 3.2× the net | no — needs live fill-vs-decision measurement | assumed, never measured |
| 4 | **Flow-flip base rate (shakeout vs true top)** | ±85% of net on synthetic; unknown live | no | already documented as the missing measurement |
| 5 | Position sizing | ≤14 bps, and the floor forbids it | yes | **dead end, proven** |
| 6 | Exit geometry (stop / trail / CVD / TP spacing) | zero | yes | **dead end, proven — and the proof was the fixture's, not the strategy's** |
| 7 | More trades | linear in count *if* per-trade EV > 0 | no | EV sign is unknown |

**The honest summary: nothing that can be measured today will move net SOL positive, and that is not
a pessimistic reading — it is what the arithmetic says.** Levers 5 and 6 are closed. Levers 1, 3 and
4 are all the same kind of thing: quantities the system currently *assumes* and could *measure*, and
each is individually worth more than the entire current book. Lever 2 is the only one whose
magnitude is certain today, and it is a selection decision rather than an algorithm change.

## §8. Is the test corpus 1:1 with real pump.fun data? No — and here is the gap list

The operator's instinct was right. What a genuinely live-like corpus requires, and where we stand:

1. **Every `TradeEvent` at trade granularity** with slot, `sol_amount`, `token_amount`, `is_buy`,
   user, and virtual reserves before/after — so impact is *observed*, not modelled. *Builder exists
   (`tools/backtest/pump_replay_build.py`); never run at scale.*
2. **The complete launch universe**, enumerated from `create` instructions over the same window —
   not a survivor list. *Enforced: the builder REFUSES without `--universe-manifest`, and stamps a
   WINDOW MISMATCH detector into the header.*
3. **Real reserves at every print.** *Missing — this is what re-pin #24 approximated.*
4. **Contemporaneous priority-fee and tip conditions.** *Missing; `fee_calibration_v1` is a Phase-B
   deliverable.*
5. **Our own latency.** We would have filled at the price available at slot t+Δ, not t. *Not modelled
   anywhere. On a hot launch this is plausibly the largest single cost term and it is currently
   zero.*
6. **The counterfactual of our own order.** Our fill moves the curve and changes every subsequent
   print. Re-pin #24 charges the first-order impact; the path dependence is unmodelled. At 33 bps a
   leg over a session this is second-order but not nil.

Items 5 and 6 are the two that make backtests lie, and both are currently unrepresented.

## §9. What changed in the repo

`tests/edge_provenance.rs` (new). A diagnostic that prints the per-trade ledger, plus four pins that
stop these facts drifting silently: five distinct markets; the book indistinguishable from zero
(asserted in integer arithmetic as t² < 4); 77% of the book attributable to end-of-tape force
closure; and the two structural properties of the generator (outcome is a hash of the tag; flow is a
clock with a byte-identical sign series across every mint).

No default moved. No parameter changed. This study reaches a **negative** on everything measurable
today and names four unfalsified candidates, all of which require live or replay data. Per A-11 it is
published as an honest negative, and per A-13(6) the discovery that our own headline number was
insignificant is recorded with the same prominence an edge would have been.

## §10. What must not be concluded from this

That the strategy is broken, or that it is fine. **Neither is established.** The corpus cannot
distinguish a good strategy from a bad one, because it contains no information to distinguish them
with. The correct reading of +8,124,568 is: *the code runs deterministically, stays bounded, exits
positions, and books lamports in the right direction on an authored distribution.* That is what a
regression fixture is for and it is worth having. It is not a forecast, not an edge, and not
evidence.

Whether this system makes money is, still, an open question — and it is now precisely clear what
would answer it.

---

## Sources

* Kamat, A. U. — *Pump.fun Graduation Regime Windows: Survival Analysis of 832,941 Token Launches
  and the Social-Presence Effect*, arXiv:2607.02823 (graduation 0.198%; Telegram 8.94× lift; all-three
  17.4× lift; Cox HR 5.40; self-buy HR 4.51; concordance 0.858).
* *MemeTrans: A Dataset for Detecting High-Risk Memecoin Launches on Solana*, arXiv:2602.13480
  (41,470 tokens, 200M+ transactions, 122 features; **migrated tokens only**).
* *The Memecoin Phenomenon: An In-Depth Study of Solana's Blockchain Trends*, arXiv:2512.11850
  (graduation rate peaks below 2%).
* CoinGecko Research — *Pump.fun Traders Are Making a Comeback* (73.28% profitable April 2026;
  **realized PnL only, excludes never-sold bags, "understates losses"**; 5.4% of wallets above $1,000).
* Froglabs — *Pump.fun Fees Explained (2026)* (1.25% total per bonding-curve trade).
* pump.fun fee-schedule working note, HackMD (post-graduation swap total 0.30% = 20 bps LP + 5 bps
  protocol + 5 bps creator; market-cap-tiered bonding-curve proposal).
