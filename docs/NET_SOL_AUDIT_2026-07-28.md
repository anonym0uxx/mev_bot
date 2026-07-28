# NET-SOL END-TO-END AUDIT — every lamport that leaves the wallet (2026-07-28)

**Mandate (operator):** scrutinise the finalised engine and bot end to end to ensure we are getting
every bit of net-SOL maximisation out of every action the bot, Hermes and the codebase take. Produce
findings and hand them to a principal bare-metal Rust engineer to build with minimal latency in mind.

**Bottom line, up front, and it redirects the handoff.** The hot path is already disciplined — a
57-rule hot-path lint in the gate, two documented allocation-elimination passes, p50/p99 budgets in
`docs/LATENCY.md`, a zero-allocation steady-state `evaluate()`. **A bare-metal engineer pointed at
compute latency would be optimising the wrong thing.** The net-SOL leakage in this system is not in
nanoseconds. It is in **lamports the cost model never counted**, and the largest single item is
**203 bps that appears nowhere in the codebase at all**.

Findings are ordered by lamports, not by tidiness.

---

## F1 — CRITICAL: Associated Token Account rent is invisible to the entire system (203 bps)

**The finding.** To hold an SPL token on Solana you must own an Associated Token Account. Creating
one locks the rent-exempt minimum:

```
rent_exempt = (128 + 165 bytes) · 3,480 lamports/byte-year · 2 years = 2,039,280 lamports
```

**2,039,280 lamports is 203 bps on the operator's 0.1 SOL clip.** A grep of the entire 26-crate
workspace for `rent`, `ATA`, `AssociatedToken`, `2_039_280`, `close_account` returns **zero hits**.
Neither the gate nor the position lifecycle nor the backtest builder has any concept of it.

**Why it is the largest item in this document.** Against a modelled ~292–302 bps round trip in the
target band, an unreclaimed ATA is a **1.67–1.70× multiplier on true cost**:

| | modelled | with unreclaimed ATA rent |
|---|---|---|
| $9k band | 302 bps | **505 bps** |
| $20k band | 292 bps | **495 bps** |

At volume it compounds: 100 tokens traded locks **0.204 SOL**; 1,000 locks **2.04 SOL**. For scale,
the entire golden reference book (8,124,568 lamports) is **four ATAs' worth of rent**.

**Why it is also the cheapest fix in this document.** Rent is a *deposit*, not a fee. Closing an
emptied token account returns all 2,039,280 lamports. The close instruction costs one signature,
~5,000 lamports. **That is a 408:1 return on the cheapest instruction Solana offers**, and it is
currently not being collected because nothing in the system knows the deposit exists.

**Required:** (a) charge ATA rent in the cost model on any token we do not already hold; (b) emit a
`CloseAccount` on the exit leg once the balance reaches zero and credit the reclaim; (c) treat an
unreclaimed account as a *carried liability* in the bankroll reconciliation, not as a vanished cost.

## F2 — CRITICAL: THREE cost models exist, and they disagree in OPPOSITE DIRECTIONS by config

> **CORRECTED after implementation review.** The first draft of this finding claimed two models
> diverging by 219 bps, and quoted 538 bps as "the gate". The engineer building F1 pushed back on
> both and was right on both. Recorded here per A-13(5) rather than quietly rewritten, because the
> corrected finding is worse than the original.

**Correction (a): there are three, not two.** Round-trip cost is expressed independently in
`economic_gate::size_band` (admission), in `engine.rs`'s entry-cost construction plus
`position::realize` (P&L), and again in `scalp.rs` as a `pump_quant_simulator::CostModel` (the paper
fill path).

**Correction (b): 538 bps is the GOLDEN TAPE's gate, not the shipped one.** `dev_portable` ships
`gate_base_fixed_lamports: 50_000`, `gate_protocol_bps: 100`, `gate_impact_den: 1_000_000`. The
200,000 / 450 pair is a test-tape override. My original text quoted the fixture as if it were the
product.

**Correction (c): the lifecycle DOES charge an entry fee.** `entry_fee_bps = 100` is applied — in
`engine.rs`, at the entry-cost construction, not inside `realize`. Original F4 was wrong.

**The corrected picture, and why it is worse:**

| config | gate prices | lifecycle books | gate is |
|---|---|---|---|
| golden tape | **538 bps** | 419 bps | **+119 bps — STRICTER** |
| shipped `dev_portable` | **205 bps** | 452 bps | **−247 bps — LOOSER** |

**The two configurations disagree in opposite directions.** On the fixture the gate is conservative
and the reported book is flattered by 119 bps (15.47M lamports over 13 trades — **1.9× the entire
book**). On the shipped profile the gate is *permissive* by 247 bps, so in production it would admit
trades the P&L accounting then books as losses.

The engineer's framing of the root cause is better than mine and should be the one that survives:
**reading `realize` alone shows an unfee'd entry; reading the gate alone shows an unrented round
trip. Neither file is wrong on its own terms.** The defect is that no single place in the codebase
expresses what a trade costs, so no reader can be wrong about it and no reader can be right about it
either.

**Required:** one authoritative cost function called by all three sites. `cost_model.rs` is now that
function; the wiring is the next commit, and it must decide explicitly which configuration it is
reconciling to.

## F3 — SUBTLE AND DANGEROUS: two large errors are cancelling, and fixing either alone breaks it

`docs/EDGE_PROVENANCE_2026-07-27.md §5` identified that the golden tape's `gate_protocol_bps = 450`
decomposes in its own comment as ~200 bps swap fee + ~55 bps LP/protocol/creator + **~200 bps
"bid/ask spread on a thin low-cap"** — and that a constant-product AMM **has no spread**. That 200 bps
is phantom. `tape_golden/mod.rs:125` states it as an itemised component in so many words.

**F1 says the system is missing 203 bps of ATA rent. F3 says it contains 200 bps of phantom spread.
They are, to within 3 bps, equal and opposite.**

So the golden tape's gate total is accidentally close to a defensible real cost — for entirely the
wrong reasons. The consequence is sharp: **the recommendation in `EDGE_PROVENANCE §7` to "resolve the
~200 bps spread term" is dangerous in isolation.** Remove the phantom without adding the rent and the
gate becomes 200 bps *more* permissive than reality, admitting a class of trades that lose money.

**Required:** F1 and F3 ship in the same commit, or neither ships.
`cost_model::the_phantom_spread_and_the_missing_rent_are_nearly_equal` pins the coincidence so it
cannot be forgotten.

## F4 — WITHDRAWN: the lifecycle does charge an entry fee

Original claim: `realize()` applies `fee_bps` to the exit gross only, so the entry leg is unfee'd.
**False.** `entry_fee_bps` (default 100) is charged in `engine.rs` when `entry_cost` is constructed,
and `realize` nets it out pro-rata via `cost_lamports`. The finding is withdrawn; what survives of it
is folded into F2 as correction (c).

Kept in place rather than deleted, per A-13(5) and A-13(6): a wrong finding that was caught is part
of the record.

## F5 — Fixed cost is per-tranche in the lifecycle and per-round-trip in the gate

`realize()` subtracts `p.tip_lamports` on **every** partial exit — correct behaviour. But the gate
prices `gate_base_fixed_lamports` **once** for the whole round trip. A three-tranche exit therefore
pays three exit tips the gate never priced. The TP ladder makes multi-tranche exits the normal case,
so this is not an edge case; it is the modal path.

## F6 — Failed transactions: the tip and the priority fee do not fail alike

`effective_fixed_lamports` inflates the whole fixed cost by `1/(1−fail_rate)`. But on Solana a failed
transaction **pays the base fee and the priority fee, and does not pay the Jito tip** — tips are only
collected when the bundle lands. Inflating both identically overstates expected tip cost and
understates expected priority cost. The two components need separate failure treatment.

## F7 — The graduation fee cliff is the largest unexploited structural event on this venue

pump.fun's fee is tiered on SOL-denominated market cap: **1.25% per trade below 420 SOL, declining to
0.30% above 98,240 SOL.** Graduation occurs at 410.88 SOL of market cap
(`curve_state::GRADUATION_VSOL_LAMPORTS`), i.e. **9 SOL of market cap below the first tier break** —
so, as `curve_state::tests::no_pre_graduation_band_can_reduce_the_fee` pins, every point on every
bonding curve pays the top rate, and the fee only relents *after* migration.

That makes migration the single largest discrete cost event available to this strategy: a **95 bps
per-leg / 190 bps round-trip** improvement, dwarfing every parameter this repo has ever swept.
Nothing in the system knows migration is approaching. `curve_state::lamports_to_graduation` now
exists and **nothing calls it**.

This is a thesis to write, not a change to make blind — but it is the highest-ceiling item on the
list, and it is structural rather than fitted.

## F8 — Execution latency is unmeasured; compute latency is solved

The distinction matters for the handoff. **Compute** latency is governed: 57 hot-path lint rules
enforced in `ci_gate`, two documented allocation-elimination passes, per-tick p50/p99 budgets,
zero-allocation steady-state `evaluate()`. There is little left there and less worth having.

**Landing** latency is entirely unmodelled. We would fill at the price available at slot `t+Δ`, not
at `t`, and Δ is currently **zero** everywhere in the codebase. On a hot launch this is plausibly the
largest unrepresented cost in the system, and it is the one genuinely latency-shaped item on this
list: it is about getting a transaction *landed*, not about executing instructions faster.

## F9 — Capital idleness is uncosted

`max_concurrent_positions = 3` at `f_base_bp = 667` gives ~20% of bankroll deployed at full
occupancy. The remaining ~80% earns nothing and no accounting anywhere charges for that. This is not
necessarily wrong — a deep fractional-Kelly floor is deliberate — but the opportunity cost is
currently invisible, so the concurrency cap has never been priced against it.

---

## What this means for the handoff

The build order is not the finding order. **F1, F2 and F3 are one commit** — a single
authoritative cost function, ATA rent included, called by both the gate and the lifecycle — because
shipping any of them alone leaves the system in a worse-calibrated state than it is now. F4 is
withdrawn. F5 and F6 are refinements of that same function and follow immediately. F7 is a thesis. F8 is a Phase-B
measurement. F9 is an accounting change.

And the note the bare-metal engineer most needs: **do not optimise the hot path.** It is already
disciplined and it is not where the SOL is going. The SOL is going into a 2,039,280-lamport deposit
per token that nothing in this codebase has ever heard of.
