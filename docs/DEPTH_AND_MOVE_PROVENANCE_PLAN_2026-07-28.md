# PRINCIPAL FIX PLAN — provenance types for the two remaining silos (2026-07-28)

**Mandate (operator):** research, analyse and plan the principal-level fix for F1 (the expected-move
silo) and F3 (the depth duality) across the codebase end to end, then hand the build plan to the
principal Rust engineer.

**Scope note.** F1 already has a *minimal* fix in `a41a76e` — the priced move is threaded into
arbitration. That fix is correct and decision-inert, and it is **not** the principal fix. It removes
today's divergence without removing the ability to reintroduce it: any future edit can reach for
`conditional_edge_bps` again and nothing will complain. This plan replaces both patches with the
structural pattern the codebase already proved works.

---

## §1. F3 IS NOT WHAT I SAID IT WAS — the corrected finding

The silo audit recorded: *"`liquidity_lamports` and `sellable_depth_lamports` are independently
sourced but describe the same pool… now asserted so a bad decode fails loud."*

**That framing is wrong, the assertion it produced is far too weak, and the corrected version is a
materially worse defect.**

### 1.1 They are different quantities with an exact relationship

pump.fun's bonding curve carries **four** reserve numbers, and the decoder already reads all of them
(`pump-quant-protocol/src/decode.rs`, `PumpCurve`):

| field | meaning |
|---|---|
| `virtual_sol` | sets the **price curve**. Seeded at 30 SOL. |
| `real_sol` | the SOL **actually in the pool** — the only SOL a seller can receive. Seeded at 0. |
| `virtual_token` / `real_token` | the token-side mirror. |

Every buy adds the same lamports to `virtual_sol` and `real_sol`, so:

```
real_sol = virtual_sol − LAUNCH_VSOL_LAMPORTS      (exactly, modulo protocol fees)
```

**Verification, and it is decisive:** at graduation `virtual_sol = 115,005,359,056`, so the identity
predicts `real_sol = 85,005,359,056` — **85.005 SOL**, which is precisely the raise the entire
ecosystem quotes as pump.fun's graduation threshold. An identity that reproduces the venue's own
published constant from first principles is the identity.

This is the **third** member of a family this codebase keeps rediscovering. Market cap
(`vsol²/32_190_000_000`), own-curve impact (`notional·10_000/vsol`) and now extractable depth
(`vsol − 30 SOL`) are all pure functions of the SOL-side reserve. Each was, at some point,
independently sourced, guessed, or configured — and each collapsed to an identity once someone did
the algebra.

### 1.2 What the code actually does today — worse than "independently sourced"

* **`real_sol` is decoded and then discarded.** Grep for consumers outside the protocol crate's own
  tests: there are none. The one number that says how much SOL the pool can pay out is parsed off
  chain and dropped.
* **`sellable_depth_lamports` has at least three producers with different semantics.**
  `engine.rs:1721` takes it from an `OnchainConfirm` event (externally supplied);
  `engine.rs:3333` and `:3359` **assign it `numeric.liquidity_lamports`** — i.e. on those paths it is
  literally set to `virtual_sol`; and `ablation_replay.rs` / `live_status.rs` hardcode `200_000_000`.
* So the same field means "externally confirmed depth" on one path, "the virtual reserve" on another,
  and "0.2 SOL" on a third. **Nothing reconciles them and nothing can, because the field has no
  declared provenance.**

### 1.3 The magnitude — my assertion passes while the fixtures are 30× wrong

`one_authority_laws.rs` asserts `sellable ≤ vsol`. Every fixture passes it. Against the *correct*
bound it is not close:

| declared `vsol` | declared sellable | actually extractable | overstatement |
|---|---|---|---|
| 30.0 SOL | 29.0 SOL | **0.0 SOL** | **infinite** |
| 31.0 SOL | 30.0 SOL | 1.0 SOL | **30×** |
| 32.0 SOL | 30.0 SOL | 2.0 SOL | **15×** |
| 34.0 SOL | 30.0 SOL | 4.0 SOL | **7.5×** |

A market at `vsol = 30 SOL` is a curve **nobody has bought into yet**. It can pay out nothing. The
tape declares 29 SOL of sellable depth there, and `size_band` will happily size against it.

### 1.4 Why it is currently latent, and exactly where it stops being latent

In the operator's $9k–$20k band a 0.1 SOL clip is 0.16%–0.32% of genuinely extractable SOL, so the
`x_max` cap never binds and the defect costs nothing today. **It binds hard near launch** — which is
precisely where a creation-sniper or fresh-launch lane operates, and precisely the regime the
`inflated_depth_claim_buys_no_size` law exists to police. That law is currently policing a bound that
is 30× too loose.

## §2. F1, RESTATED AS AN ARCHITECTURE PROBLEM

The minimal fix makes admission and arbitration agree *today*. It does not make disagreement
*unrepresentable*. Both call sites still have independent access to `conditional_edge_bps`, the
values are bare `i128`/`u32`, and nothing records which estimator produced the number that priced a
given trade. A future edit reintroduces the silo silently, and the journal cannot tell you afterwards
which estimate was used.

## §3. THE PATTERN — copy `BankrollOrigin`, do not invent

The codebase already solved this defect class once, correctly. `BankrollOrigin` is either
`PaperSeed(cfg.bankroll_initial_lamports)` or a live reconciled balance; the sizing base is computed
**from the origin**, never from the config field directly. The operator's live-bankroll rule is
therefore enforced by the type system, not by a comment. That is why bankroll came back clean from
the silo audit while cost, impact, fees and depth all came back dirty.

**Principle: when one quantity can come from more than one place, the value and its provenance travel
together in one type, and consumers receive the type — never a bare integer.**

## §4. THE BUILD

### 4.1 `CurveDepth` — one type, both venues, fail-closed

```rust
pub enum DepthBasis {
    /// Bonding curve: both reserves decoded. `real_sol` is authoritative for payout.
    CurveDecoded { virtual_sol: u64, real_sol: u64 },
    /// Bonding curve: only `virtual_sol` known; `real_sol` DERIVED by the identity.
    CurveDerived { virtual_sol: u64 },
    /// Post-graduation PumpSwap AMM: no virtual offset, reserves are the reserves.
    MigratedPool { sol_reserve: u64 },
    /// Undecoded. Prices nothing, sizes nothing (§18.2).
    Unknown,
}
```

* `price_reserve()` → what the *price/impact* model must use: `virtual_sol` on the curve,
  `sol_reserve` post-migration.
* `payout_reserve()` → what a seller can actually receive: `real_sol` (decoded, or derived as
  `virtual_sol − LAUNCH_VSOL_LAMPORTS`), or `sol_reserve` post-migration.
* `Unknown` returns `None` from both. **Never zero** — a zero is a number and would size.

**The venue distinction is load-bearing.** The `−30 SOL` offset is a bonding-curve fact. Applying it
to a migrated pool would understate payout depth by 30 SOL; not applying it on the curve overstates by
up to infinity. `curve_state::GRADUATION_VSOL_LAMPORTS` already draws that line.

**Cross-check when both are known.** `CurveDecoded` must verify the decoded `real_sol` against the
derived value and **refuse on material disagreement** rather than silently preferring one. Suggested
tolerance: the greater of 1% or one 0.1 SOL clip, to absorb protocol-fee drift without absorbing a
decoder bug. A refusal here is a fail-closed `Unknown`, which is a rejected trade, not a crash.

### 4.2 `PricedMove` — the expected move carries its own provenance

```rust
pub enum MoveSource {
    Model { band: usize, n: u32, signals_applied: u32 },
    LanePrior { lane: WlLane },
    ColdStart,
}
pub struct PricedMove { bps: u32, source: MoveSource }
```

Computed **once** in `gate_evaluate`, passed to `decide()` for admission and to the arbitration term.
`conditional_edge_bps` becomes a *constructor* of `PricedMove::LanePrior`, not a second thing a caller
can reach for. Journal the `MoveSource` on every admit so a replay can answer "which estimator priced
this trade" — a question that is currently unanswerable.

### 4.3 Wiring, in dependency order

1. `curve_state`: add `real_sol_for(vsol)` and `LAUNCH_VSOL_LAMPORTS`-based derivation with the
   graduation branch. Pin the 85.005 SOL anchor.
2. New `CurveDepth` in `pump-quant-app` (it needs `curve_state`; keep it out of `pump-quant-strategy`
   to avoid the cycle that shaped the cost-model wiring).
3. Ingest: surface `PumpCurve.real_sol` — it is already decoded and thrown away.
4. `gate::decide`: take `CurveDepth`. Impact from `price_reserve()`, `x_max` cap from
   `payout_reserve()`. This is where the 30× overstatement dies.
5. Replace the three `sellable_depth_lamports` producers. The two sites assigning
   `numeric.liquidity_lamports` become `CurveDerived`; the `OnchainConfirm` path becomes
   `CurveDecoded` and gains the cross-check; the hardcoded `200_000_000` in `ablation_replay.rs` and
   `live_status.rs` must be re-expressed or explicitly marked report-only.
6. `PricedMove` through `gate_evaluate` → `decide` → arbitration.
7. Fixtures: every tape declaring a sellable depth above `vsol − 30 SOL` is declaring a market that
   cannot exist. **Expect a decision-plane re-pin (#27)** — sizes will change where the cap now binds.

### 4.4 What must NOT happen

* **Do not clamp.** A decode that reports impossible depth is a broken decode; clamping hides it.
  Refuse (`Unknown`), journal, reject the trade.
* **Do not delete `real_sol` because "the identity gives it".** Decoded truth beats derived truth when
  both exist; the derivation is the *fallback*, and the disagreement between them is a decoder health
  signal worth having.
* **Do not let `PricedMove` be constructible from a bare integer** outside the two named constructors,
  or it becomes a bare integer with extra steps.

## §5. EXPECTED OUTCOME

* One authority for depth, with the venue distinction explicit and the `x_max` cap finally bounded by
  SOL that exists.
* One authority for the expected move, with the estimator recorded in the journal.
* A re-pin (#27) on the decision plane, sized by how often the corrected cap binds — near-launch
  cohorts materially, the $9k–$20k target band probably not at all.
* Two more members of the "pure function of the SOL reserve" family collapsed into `curve_state`,
  where the first two already live.

**Honest expectation on net SOL: this does not increase it.** It removes an oversizing capability the
strategy was never entitled to. Where it changes a number, the new number is smaller and correct.

---

# OUTCOME — re-pin #27, measured (2026-07-28)

Built, verified, green: **494 test binaries / 2,635 tests, 0 failures; `ci_gate` PASSED.**

## The pins

| | re-pin #26 | re-pin #27 |
|---|---|---|
| digest | `6163272398497391826` | **`2822236667991883855`** |
| net | 16,778,896 | **31,111,528** |
| promoted / admitted / rejected / filtered | 504 / 12 / 447 / 72 | **504 / 11 / 448 / 72** |
| fixed ladder | 16,970,346 | **31,302,978** |
| derived − fixed | −191,450 | **−191,450 (unchanged)** |

## The +14,332,632 is a fixture artifact, and three independent facts say so

The book nearly doubled again. It is **not** either provenance fix — both were measured
decision-inert on this tape. It is the confirmed-set eviction key, which now orders by *payout*
reserve (the corrected form of the retired "lowest asserted sellable depth"). The tape presents ~268
confirmations against a 256-entry bound, so ~12 markets are evicted and the book is built from ~11
trades in a handful of markets: **which markets survive a capacity bound dominates the net.**

Three things corroborate that reading, none of which was engineered to:

1. **Both ladders moved by exactly the same amount.** Derived 16,778,896 → 31,111,528 and fixed
   16,970,346 → 31,302,978 — both `+14,332,632` to the lamport. A pricing change would move them
   differently; a market-selection change moves them together.
2. **`GOLDEN_DERIVED_MINUS_FIXED` is unchanged at −191,450.** The §24 margin survived a re-pin that
   doubled both its terms.
3. **The `k = 5` harm is invariant to the lamport at 11,469,573**, across a cost-model unification
   *and* this eviction reordering, while its *fraction* of the book drifted 140% → 68% → 37%. Read
   the magnitude; the fraction is measuring the denominator.

## Three "law verdicts" from re-pin #26 were all the same fixture defect

This is the finding that matters most, and it cuts against the convenient reading in every case.

* **LAW B7's asymmetry leg.** 1.27× (#24) → **5.78× (#26)** → **1.60× (#27)**. #26 was the outlier.
  Under honest payout depth the false-positive arm costs 69.3M rather than 15.2M — it was being let
  off cheaply by a cap that could not bind. The A-11 study request #26 raised is closed **by
  withdrawal, not by verdict**.
* **B7 as a permutation co-winner.** #26 produced two winners, `{B3}` and `{B3,B7}`, where every
  prior sweep produced one. #27 returns to the single winner `{B3}` — the shipped default.
* **The `k = 5` sign flip**, already traced at #26 to a doubled baseline rather than a moved harm.

**What did NOT revert, and it is a genuine change:** LAW B7's leg (a) now *clears* materiality for
the first time — 110,922,388 against a 100,000,000 bite, up from 26,697,249 at #24. B7 earns
materially on its own two-sided tape and still fails promotion, on the asymmetry leg and on the
arbiter. That is Amendment A-11's arbiter rule working exactly as written: a tape built *for* a
hypothesis may demonstrate a mechanism and may never decide promotion.

## The corrected F3 assertion

The retired guard asserted `sellable ≤ vsol` and passed while fixtures were 30× wrong. It is
replaced by `payout_depth_is_bounded_by_sol_that_actually_exists`, which pins that a freshly launched
curve pays out **zero**, that the identity reproduces the venue's own **85.005 SOL** graduation raise,
that payout is *strictly* below the price reserve everywhere on the curve, and — the part the first
version missed entirely — that the derivation **refuses** at and beyond graduation, because the
−30 SOL offset is a bonding-curve fact that must not touch a migrated AMM pool.

## Honest bottom line

Net SOL did not improve. An oversizing capability the strategy was never entitled to was removed, a
second silo was closed by type rather than by convention, and three law verdicts that looked like
evidence turned out to be readouts of one fixture defect. The book remains 11 trades in a handful of
markets, statistically indistinguishable from zero.
