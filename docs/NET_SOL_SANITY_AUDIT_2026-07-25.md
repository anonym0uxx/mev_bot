# NET-SOL SANITY AUDIT — end-to-end (2026-07-25)

**Mandate:** end-to-end sanity check across code, constitution, and instructions — are we actually
optimizing for realized net SOL, and is the approach sane?

**Method:** four independent audit lenses (objective alignment, armed-law evidence, dead weight,
economic arithmetic) run as parallel adversarial agents, then every load-bearing claim re-verified
by hand against the source before any change was made. Findings below are only the ones that
survived that verification.

**Headline:** the architecture is sound and the objective plumbing is overwhelmingly correct — but
the audit found **one defect that inverted the objective at the single most consequential decision
point in the system**, plus two latent correctness bugs and two economic-model gaps. The flagship
defect is fixed, pinned, and shipped with the golden digest unchanged.

---

## FIXED THIS PASS

### 1. LAW B3 vetoed the fat right tail — the exact shape that makes memecoin trading pay

**Severity: critical. This is the most expensive defect found in any audit of this repo.**

LAW B3 is the **only armed discretionary law** in the system: at admission it vetoes or haircuts
position size from episodic recall. Its "this class bled" test was:

```rust
let bled = stats.median_net_lamports < 0;          // median ONLY
if stats.win_rate_bp <= 1_500 { return Veto; }     // ≤15% win rate
if stats.win_rate_bp <= 3_500 { return Haircut; }  // ≤35% win rate
```

The canonical profitable memecoin payoff is a **fat right tail**: most trades lose small, rare
trades win huge. That shape has a **negative median** and a **low win rate** — so it satisfied both
conditions and was **VETOED**, no matter how positive its aggregate.

Concrete, verified counter-example: 18 losses of −0.10 SOL against 2 wins of +3.00 SOL. Median
−0.10 SOL, win rate 1,000 bp, **total +4.20 SOL**. The single most profitable class in the book,
refused at admission. The haircut band is the same disease: 15 × −0.10 against 5 × +2.00 = **+8.50
SOL total**, sized to half.

Two aggravating factors made it worse than a one-off misjudgement:

* **The veto is absorbing.** `require_admitted` is true by default and `was_admitted` is set only in
  `record_exit`, which runs only for admitted trades. A vetoed class books no episode, so its
  statistics **freeze permanently** — there is no path by which it redeems itself. One unlucky early
  sample creates a permanent blacklist.
* **Conditioning is deliberately broad** (phase-partitioned only, by design, so the law does not
  degenerate to `Unknown`), which makes a `Known` verdict *more* likely, not less. No upstream guard
  mitigates the defect; the breadth amplifies it.

**The codebase already contained the correct test.** `brain_analysis::is_conditioned_negative`,
used for *retirement*, has always required `median < 0 && mean < 0`, and its doc comment states the
reasoning verbatim: *"A negative median with a positive mean is a subject that pays rarely and
hugely — a lottery, but not necessarily a losing one… Requiring both is the conjunction that says
'this is not paying, in either the typical or the aggregate sense'."* The authors articulated the
right rule and applied it to the *lenient* decision while omitting it from the *aggressive* one.
This was an oversight, not a considered trade-off — the doc comment above `size_verdict` discusses
only the fat **left** tail.

**Fix:** `bled` now requires `median < 0 && mean < 0`, matching retirement exactly. The verdict rule
was extracted into a pure function (`BrainPlane::verdict_from_stats`) so it is directly testable —
the defect survived precisely because the rule could only be exercised end-to-end.

**Result: the golden digest did NOT move and no pinned number changed**, while
`b3_armed_recall_haircut_strictly_out_earns_its_absence` still passes — the hazard tape's bleeding
class genuinely bleeds in both senses, so the law keeps its earnings and loses only its false
positives. Pinned by `tests/lottery_class_admission.rs` (4 tests), including a test asserting that
admission and retirement can never again disagree on what "bleeding" means.

### 2. The crate that owns every lamport compiled with overflow checks OFF

`Cargo.toml` enables `overflow-checks = true` in release for twelve money crates — but **not
`pump-quant-app`**, which owns `bankroll_realized`, `bankroll_committed`, `book_exit`, and all of
`position.rs::realize`. The one crate §22's "silent money wrap is prohibited" rule most obviously
targets was the one omitted. Fixed.

### 3. A loss could saturate into a maximal gain in a decision path

`engine.rs`: `i64::try_from(e.net_lamports).unwrap_or(i64::MAX)`. `try_from` fails at **both** ends,
so an out-of-range **loss** became `+9.22e18` — and this value feeds `lane_perf`/`disc_perf`, which
drive reflection weights (a decision path, not a readout). The sibling attribution 77 lines below
already did it correctly with `.clamp()`. Magnitude is unreachable in practice (needs ~9.2e9 SOL),
but the direction was wrong and the fix is one line. Fixed to clamp.

---

## OPEN — verified real, deliberately NOT fixed in this pass

Both are genuine and both deserve their own study rather than a rushed patch. Specified precisely
here so they can be picked up without re-deriving.

### 4. Scale-in books added size at the ORIGINAL entry price (phantom PnL)

`position.rs::scale_in` increments `size_lamports` and `cost_lamports` but **never touches
`entry_price_fp`**. Every subsequent `realize()` computes `gross = size × mult_bps / 10_000` with
`mult_bps` measured off the *original* entry — so lamports bought at the current mark are booked as
if bought at entry. The trigger (authentic flow, not-Downtrend) correlates with a rising tape, so
the bias is **systematically positive**. Worked example: target 0.25 SOL, probe 0.10, scale-add
0.15, scale-in at +20% → **phantom gain ≈ 0.03 SOL per trade**.

**Fix (needs its own A/B + re-pin):** re-average the basis on scale-in —
`entry_price_fp = (old_size × old_px + add × mark_px) / (old_size + add)` — or carry the added
tranche as a second lot with its own entry price. Note this makes reported paper net *lower* and
more honest; expect the golden net to move, which is the point.

### 5. The gate's cost model and the exit's actual charges disagree

The gate prices `gate_protocol_bps` as the whole round trip, but booking charges `entry_fee_bps`
**plus** `exit_fee_bps` (two legs), and `first_sell_penalty_bps = 150` is charged at exit while
appearing **nowhere** in the gate's cost model (it is not even config-reachable — hardcoded in
`LifecycleParams::standard()`). Conversely the gate's impact term is never actually deducted in
fill modes A/B. Net: **the default config under-prices the round trip by ~150 bps.**

This is partly known and partly not. The golden tape *deliberately* overrides the defaults
(`gate_protocol_bps = 450`, fixed 200k, impact_den 250k) precisely because "the default
`dev_portable` economics yield a ~150–190 bps round-trip — far too cheap". So the shipped default is
a **laptop-profile** number, not a live one, and Phase-B tuning is expected to set it from measured
cost. **But the first-sell penalty being absent from the gate model is a genuine modeling gap at any
setting**, and a default that structurally under-prices is a live hazard if Hermes ever runs it
un-tuned. Both are now called out for the server pass.

---

## VERIFIED SOUND (no action)

* **§22 compliance is clean.** Zero `f32`/`f64`/`rand`/`SystemTime`/`Instant` in any decision path
  across all 26 crates. The single `f64` is an explicitly lint-allowed, NaN-rejecting ingest-boundary
  adapter, off the hot path.
* **Realized-only PnL.** `bankroll_realized` is fed only from `Exit.net_lamports`; marks never enter
  it. Partial tranches and the terminal close cannot double-count (`remaining_bps` decrements,
  `tranche_mask` makes each rung one-shot, `close()` removes before realizing). Entry cost is
  recovered exactly once and the tranche fractions sum to exactly 10,000.
* **Bankroll origin separation holds.** `PaperSeed` / `LiveReconciled` are distinct; a paper seed
  provably cannot back a live trade (`require_live_verified` fail-closes).
* **Authenticity enters the sizing chain exactly once** — the constitutional concern is not
  violated. Concentration and authenticity were confirmed to measure genuinely disjoint quantities.
* **Entry arbitration, the economic gate, retirement review, sequential retirement, and author
  trust all rank on realized net lamports.** Correctly aligned.
* **Every armed flag has test coverage**, and the armed set is either constitutionally mandated
  (`derived_targets` per §24, `creator_dump_veto` per §26) or earned via permutation sweep (B3).
* Rounding bias inside `realize` is +1 to +5 lamports per trade in the bot's favour (~5e-9 SOL) —
  immaterial, no fix warranted.

## Noted, lower priority

* **Social source quality is a hit rate that discards magnitude** (`social_earn.rs`:
  `let favorable = net_lamports > 0`). A caller with ten +0.05 SOL calls and one −5.0 SOL call
  (net −4.5) outranks one with a single +5.0 SOL call. It only *orders* candidates into the net-SOL
  gate, so the gate still refuses bad economics — but under position caps, ordering is money.
* **Promotion gates on a sign-test LLR** rather than net-SOL expectancy (`shadow.rs` reduces
  `challenger − incumbent` to its sign), which is close to what criterion 74 forbids. Held down by a
  magnitude side-guard and currently latent.
* **The shadow tournament pairs full-size challengers against a probe-size incumbent** — a
  systematic ~2.5× size advantage to every challenger, biasing §48 promotion.
* **Creator credibility is computed twice from identical inputs** (`cred_mult` and `deployer_mult`
  read the same launch tuple and apply the same constants). Latent only because
  `deployer_screen_enable` is default-false; it would double-fade if ever armed.

---

**Gates:** golden_digest 8/8 (digest UNCHANGED), lottery_class_admission 4/4, brain_laws 11/11,
flow_persistence_laws 4/4, entry_exit_frontier 3/3, engine_e2e 15/15, holder_concentration 19/19,
audit_wave2_laws 13/13, batch_e_laws 5/5, alpha_laws 5/5, sizing_floor_laws 4/4, reflect 3/3,
ci_gate PASSED.

---

# ADDENDUM (same day) — scale-in cost basis: FIXED, with the honest A/B

Open item **#4** above is now closed. `ScalpLifecycle::scale_in` blends the cost basis.

**The A/B is an arithmetic IDENTITY, not a tape comparison** — this is a correctness proof, not an
edge search, so the decisive scenario is the one whose truth is known independently of any model:
**scale in at mark M, then close at exactly M.** The added tranche was bought at M and sold at M, so
it is flat by construction and can contribute nothing. Any booked gain on it is phantom, and its
magnitude is closed-form. No tape can luck its way past that test.

Measured phantom, un-blended basis, 0.10 SOL probe + 0.15 SOL add:

| scale-in mark | truth (probe only) | old rule booked | phantom |
|---|---|---|---|
| 1.05× | +5,000,000 | +12,500,000 | **+7,500,000** |
| 1.20× | +20,000,000 | +50,000,000 | **+30,000,000** |
| 1.50× | +50,000,000 | +125,000,000 | **+75,000,000** |
| 2.00× | +100,000,000 | +250,000,000 | **+150,000,000** |

Exactly `add_lamports × (mark/entry − 1)`, pinned as a closed form in the test.

### The specification was wrong, and the identity test caught it

The natural fix — an arithmetic weighted average `(s1·p1 + s2·p2)/(s1+s2)` — is **incorrect for this
codebase**, and it was the fix originally specified. `size_lamports` is a **notional** (lamports
deployed), not a unit count, and `realize` computes `gross = size × mult_bps / 10_000`. A notional
`s` at price `p` buys `s/p` units, so the basis that makes `total_notional × P / B` equal the true
`units × P` is the **harmonic** mean:

```text
    B = (s1 + s2) · p1 · p2 / (s1·p2 + s2·p1)
```

The arithmetic mean leaves a residual phantom of ~0.9% of the added tranche at a 1.2× add. It only
failed by ~2.1M lamports rather than 30M, which is precisely the kind of error that survives review
and never survives an identity test. This is the argument for writing the assertion before trusting
the specification.

### Properties

* **Signed-correct, not a one-way haircut.** A scale-in BELOW entry blends the basis down and
  correctly RAISES reported net for that position. A fix that only ever reduced net would be a fudge.
* **Genuine gains survive in full.** Probe at 1.0×, add at 1.2×, close at 2.0× still books the true
  +0.20 SOL — the fix removes the phantom, not the earnings.
* **Fail-closed.** A zero mark is missing evidence, so the add is REFUSED rather than booked against
  an unknown basis (§6.4), and the refusal does not consume the one-shot scale slot.
* **Conservative rounding.** `div_ceil` on the basis plus integer-bps `mult_bps` quantization leaves
  at most **1 bp of notional** unrecovered, always against us.

### Golden impact: NONE — and that is the honest finding

**The golden digest and every pinned number are unchanged**, because the scale-in path never fires
on the golden tape: it requires EVIDENCED authenticity ≥ `scale_confirm_auth_min_bp` (8_000), which
that tape never produces. So the historical golden net was **not** inflated by this defect.

That is the correct and least convenient framing: the phantom was **live-only**. It would have
appeared the moment real flow produced authenticity evidence — i.e. on the server, in Phase-B, with
real money, and nowhere in any laptop measurement that had been used to justify anything. It is now
impossible.

### Still open (unchanged from above)

* The **shadow tournament** mirrors challengers at full size while the incumbent opens at probe size
  and now carries a blended basis — challengers will look systematically better by roughly the
  phantom just removed. §48 promotion verdicts on scaled mints are not apples-to-apples until
  `Tournament` mirrors the probe/scale split. Report-plane, but it gates promotion.
* The **§52 buy-and-hold baseline** still reads `att.entry_price` (probe price) against the full
  `entry_spend`. Report-only, and the error runs against us.
* The **gate cost model** under-prices the round trip by ~150 bps (open item #5).

**Gates:** golden_digest 8/8 (UNCHANGED), scale_in_basis 5/5, lottery_class_admission 4/4,
brain_laws 11/11, entry_exit_frontier 3/3, engine_e2e 15/15, sizing_floor_laws 4/4,
audit_wave2_laws 13/13, holder_concentration 19/19, flow_persistence_laws 4/4, batch_e_laws 5/5,
alpha_laws 5/5, reflect 3/3, concentration_stream 6/6, ci_gate PASSED.
