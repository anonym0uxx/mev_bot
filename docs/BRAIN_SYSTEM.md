# THE BRAIN — episodic recall memory (§45/§46/§56.9, Amendment A-8)

Local, deterministic, integer-only episodic memory that lets Hermes answer a principal
quant's questions in microseconds: *"What happened last time a coin looked like this?
Does this match the current or a past meta? Did a candle setup like this earn net SOL?
Who called this mint, and do they actually make money?"*

**No LLM. No embeddings. No third-party API. Nothing leaves the machine.**

## Why integer feature fingerprints, not text embeddings

Vector-database memory products (Honcho, Mnemosyne, and the general RAG pattern) sell
*recall by semantic resemblance over text*. That is the wrong instrument here, for four
reasons that are load-bearing in this codebase:

1. **Determinism (§22/§54).** Every value in an outcome path must be integer and
   replay-reproducible byte-for-byte. Float embeddings + approximate nearest-neighbour
   search are neither.
2. **Semantics.** "This coin looks like that coin" is not a *language* question — it is a
   microstructure question. The engine already computes the right vocabulary: order-flow
   imbalance, CVD, trend structure, burst phase, realized volatility, liquidity depth,
   buyer breadth, attention velocity, authenticity, creator class, meta state. Similarity
   over those is strictly more meaningful than similarity over prose about them.
3. **Latency.** A recall is on the decision path's shoulder. Integer popcount over packed
   signatures is microseconds; a network round-trip to a hosted memory service is 4–6
   orders of magnitude slower and can fail.
4. **Sovereignty (§6.6).** Our edge is the strategy. Shipping our setup history to a
   third party to be indexed is handing it over.

## Design

### Fingerprint (`pump-quant-brain::fingerprint`)
20 integer features → monotone named-const bucket ladders → a packed `u128` signature
(99 bits) plus the ordinal bucket vector.

The encoding is **thermometer (unary) for ordinal fields, one-hot for nominal fields**.
This is the key choice: it makes `signature_hamming(a,b)` *exactly equal* to the ordinal
distance |Δbucket|, so the fast prefilter is not an approximation of the precise rank —
it **is** the rank at uniform weights. (A naive binary packing would put bucket 3 and
bucket 4 three bits apart, which is meaningless.)

Feature weights are integer and named: venue phase highest (10), then trend/burst/OFI/
range/meta, with time-of-day lowest (1 — a cyclic axis on a linear encoding is a weak
prior).

### Recall (`pump-quant-brain::recall`)
Two-stage, over contiguous parallel arrays (signatures / filter keys / episodes):
1. **Prefilter** — `xor` + `count_ones()` across every signature; filter (phase, meta,
   lane, admitted-only) compiled into a single mask/expect pair; distances memoised into
   a 1-byte scratch; a stack histogram picks the cutoff with no sort.
2. **Rank** — weighted integer L1 over the top-M survivors, tie-broken `(distance,
   episode_id)` so ordering is total and deterministic.

Measured (release): **~10 µs @ 1k episodes, ~21 µs @ 4k, ~70 µs @ 16k** (full cap).

### The safety property that matters most: fail-closed at small n

```rust
pub enum RecallVerdict { Known(RecallStats), Unknown(RecallUnknown) }
```
`RecallUnknown` has **no field of type lamports, win-rate, or hold** — only counts and
distances. The sole accessor yielding numbers is `stats() -> Option<&RecallStats>`, which
is `None` for every Unknown shape. There is no `Default` and no `unwrap_or`.

Small-sample recall is precisely how a quant fools himself: with n=2 every pattern looks
prophetic. Here it is **structurally impossible to read an estimate** below `min_sample`
(§46 small-n guard), out of radius, or from an empty index. Fail-closed by type, not by
discipline.

Two further anti-fooling defaults: **phase pinning is unrepresentable to violate** —
there is no API path to an estimate that pools pre-migration curve markets with
post-migration pool markets (§100) — and **`require_admitted: true`**, because a rejected
setup's "zero" is structural, not observed, and pooling it drags every estimate toward
zero while manufacturing flattering low variance.

### Meta timeline & social recall
`meta_timeline` answers "what is the state of meta this week" and "which past metas does
this one resemble, and what did they pay" (§21.4 meta-lifecycle history). `social_recall`
answers "who called this mint in the last week" and "does that author actually earn"
(`AuthorTrackRecord`, same fail-closed min-sample discipline).

### Durability (`pump-quant-brain::persist`, and the same idiom in `pump-quant-memory`)
Pure-std, no database, no dependencies. Append-only journal (`payload_len | fnv1a_64
checksum | fixed-width LE payload`) plus snapshot written temp → fsync → atomic rename.
Restore = snapshot then journal-tail replay.

Crash safety is proven, not asserted: truncating the final record at **every byte offset**
loses exactly one record; a corrupt mid-file frame is skipped and the reader resynchronises
so later records survive; restore never fails on damage, it *reports* it. A record whose
schema version is newer than the running binary is refused, never reinterpreted.

`pump-quant-memory` (the research store: hypotheses, experiments, social calls, markouts,
source quality, meta categories) now persists through the same idiom — closing the
previous RAM-only gap. Sealed experiments restore **sealed** (§56.9); capacity overflow
refuses and reports rather than evicting sealed evidence (§57 durability-first).

## Engine integration (LAWs B1–B5)

| Law | What | Plane |
|---|---|---|
| **B1** | Episode recorded at every completed trade. The fingerprint is captured **at admit time**, before the position exists — an exit-time fingerprint would be look-ahead-contaminated and worthless. Pinned by a test that diverges the post-entry price path wildly and asserts the recorded fingerprint is byte-identical. | inert |
| **B2** | Reflection queries recall to produce *grounded* hypotheses; `Report` exposes recorded/known/unknown counts, the strongest setup classes (n, median net, win rate), meta state, and author track records. | inert |
| **B3** | **Reduce-only** recall haircut/veto: a setup class that historically bled may shrink or refuse a trade. There is **no size-up path** — `BrainSizeVerdict` has no `Boost` variant. Sizing up on historical winners is exactly where episodic recall overfits. | decision |
| **B4** | An `Unknown` verdict can never change a decision. Pinned by comparing the full journalled decision stream with the brain on vs off — byte identity required. | invariant |
| **B5** | Episodes persist and restore; recall verdicts byte-identical across a simulated restart; a fresh store restores to an empty fail-closed brain (a restart never manufactures evidence). | durability |

### B3 is DEFAULT OFF, and that is the honest result

On the representative golden tape the haircut is **exactly neutral**: recall reaches
`Known` 144 times, but every class it can speak about is profitable, so a reduce-only law
correctly does nothing. Under §46 a feature does not get armed on the assumption it will
earn, so `brain_haircut_enable` defaults **off**.

It *does* earn where it should. On a hazard tape carrying a bleeding setup class **inside
an otherwise profitable discovery lane**, armed vs neutral is **+293,235,710 vs
−98,696,856 lamports — ~0.39 SOL of loss avoided.**

That construction is the real finding: a single-bleeding-setup tape proves nothing,
because the existing per-lane expectancy estimator (EXPECTANCY_V1) already refuses that
lane after 8 fills. **Episodic recall only adds edge where the lane-pooled estimator is
structurally blind — discriminating a bad *setup* inside a good *lane*.** That is the
precise niche this system occupies, and the reason to keep it.

## Known gaps (stated, not estimated — §6.4)
`holder_growth_accel` is always 0 (no holder second-derivative estimator exists yet);
`CreatorClass::Proven` and `MetaSaturationState::Decaying` are unreachable (no
survived-migration ledger / no decay vocabulary in the app); the app's 4-class narrative
taxonomy maps injectively into the brain's 8 nominal slots. Each is left unpopulated
rather than approximated. `brain_recall_max_distance` defaults to 12, which pools
aggressively — an operator arming B3 should tune it down (the hazard tape needed 3).

---

# The social-cognition layer (Amendment A-9)

Four faculties that turn recall into judgement. All **report-plane** — proven decision-inert by
byte-identical journal-digest comparison.

## `social_support` — "does this coin have real social support?"
Distinct-**originator** breadth (echoes and reposts are not support), trust-weighted so three proven
callers outrank sixty anonymous handles, times cross-platform spread (single-platform concentration
is a coordination smell), times **velocity** — the derivative of support across sub-windows, because
a level without a derivative is a lagging indicator — minus an echo/coordination penalty. Fail-closed
`Unknown` below a minimum of distinct originators.

It also publishes `support_inputs_needed()`: which platforms lack coverage, whose track record is
unresolved, which sources need an operator exposure judgement. **The brain states its information
needs rather than fabricating them** — and that list is the capture plane's work queue, which is what
makes the memory get better rather than merely bigger.

## `trust` — "can I trust the accounts saying it?"
Trust is earned **exclusively from realized net SOL** on attributable calls. Follower count,
engagement, badges and self-claimed records are not ignored-by-policy — they are **structurally
unreachable**: the trust path reads only `CallMarkout` (`author_id`, `realized_net_lamports`,
`hold_duration`, `info_time`), while popularity fields live on `CallRecord`, which this module never
touches. Changing that requires changing the shape of the data, which is a reviewable act.

Integer partial pooling shrinks thin samples toward a population prior whose **positive side is
capped and negative side uncapped** — an estimator may be pessimistic for free, never optimistic for
free. A time-decay half-life returns stale reputation to the prior (caller edge decays like any
other). §28 public-burned exposure is **operator-set** (the module states plainly it cannot observe
crowding and won't pretend to) and demotes only *positive* scores — being crowded is not a defence
against losing money.

## `follow_reco` — "should I be following someone I'm not?"
Authors whose calls **preceded** our realized winners, weighted by lead time on a trapezoid: zero
credit below ~5s (a call after we were already in is a witness, not a signal), full credit in the
minutes band, decaying to zero at the lookback horizon so a firehose account cannot harvest
attribution from everything that ever worked. One credit per author per episode. Plus
`unfollow_candidates()` for followed sources whose attribution decayed negative.

**Scope boundary, stated in the module header:** recommendation only. No posting, engagement, or
promotional surface exists or may be added — promoting tokens we hold or trade is prohibited
(criterion 110, Tier-0 severity).

## `archetype` — "think like a pro"
Four **style lenses** — `EarlyRotation`, `FlowScalper`, `Sniper`, `ConvictionSize` — each a documented
`FeatureWeights` profile plus a `RecallFilter` shape and a rule table over the fingerprint. These are
style archetypes derived from observable trading behaviour, **not models of, or claims about, any
individual**; reconstructing a named trader's process would be fitting noise to a survivor. A lens is
only ever validated against **our own realized net SOL**, per venue phase (§100 — no phase-pooled
per-lens statistic exists). `best_paying_lens` returns `None` rather than crowning a least-bad loser:
least-bad is not paying, and re-weighting reflection toward a loser is worse than toward nothing.

# Social → on-chain hardening

**Provenance or nothing.** Every social quantity reaching a decision surface arrives through a struct
with no partial constructor, carrying platform, author, earned trust tier, operator exposure, and
first/last observation. There is no path for an anonymous social scalar.

**Staleness is dropped, not decayed in place.** A social row past its TTL is *removed* (§34.3/§29.6);
it can never be carried forward at its last value.

**The end-to-end authority proof** (`tests/social_hardening.rs`): a sweep of 3 social strengths × 4
failing on-chain positions — 12 cells — asserts `admitted == 0` in every one while `promoted > 0`
(refusing to admit is not refusing to look). The strongest form: ten callers who have **earned**
realized markouts, whom the operator has followed and marked niche, blast a market with no swaps and
no confirmation — admits stay at exactly zero, pinned to gate code 1. Positive controls prove the
chain still admits when on-chain evidence is present, and that the on-chain lane is self-authorising
without any social input at all.

**Social makes Hermes faster and better-targeted. Raw on-chain numbers authorize.**

# Gap closes and one production defect

Wired: holder-growth acceleration (time-normalized second difference — `+10%` over 60s and over 30s
are not the same rate), a creator survived-migration ledger where survival is **derived, never
asserted** (`CreatorClass::Proven` requires ≥2 survivors, zero rugs, and an *untruncated* history —
once the ring evicts a launch a dropped rug is possible, so the optimistic label is withheld), the
meta `Decaying` phase, and an additive `NarrativeFamily` axis.

Two declared gaps were assessed and **correctly declined**: `brain_path` in the §19 seed (the journal
path selects which corpus is recalled, so it genuinely is run identity) and info-time re-basing of
social stamps (mixing capture wall-clock with information time injects a latency-signed *bias*, not
noise).

**Production defect found and fixed forward:** `TAXONOMY_V0` matched category keywords as naive
substrings, so live tokens were mis-assigned — "Fair Launch"→AI (via "**ai**" in "f**ai**r"),
"Catalyst"→Animal ("**cat**alyst"), "Bottom Signal"→AI ("**bot**tom"), "Magazine"→Political
("**maga**zine"). Because `category_id` is a recall **filter key**, mis-assignment pools tokens with
the wrong meta's episodes and quietly corrupts every conditioned recall that touches them.
`TAXONOMY_V1` applies word-boundary matching to short English-carrier needles. **V0 is frozen and
pinned as historical record** — assignments are timestamped and never retroactive (criterion 81), so
the fix is forward-only under a bumped version.

**Recall radius tightened 12 → 8.** At radius 12 a maximally net-*buying* setup matched a maximally
net-*selling* one with all eighteen other fields identical. On a 13-episode memory the new default
refuses every admit-time query — which is the only honest answer that corpus can give.
