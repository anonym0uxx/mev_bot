# SILO AUDIT — is any other decision fed by unreconciled duplicate sources? (2026-07-28)

**Mandate (operator):** make sure we do not have any more "three siloed data points for one decision"
anywhere else in the code. One clear end-to-end sanity check before running on the server.

**Bottom line.** Eight candidate silos were traced from producer to every decision-consuming reader.
**One is a live defect** (latent — it only bites when a disarmed law is armed). **One is dead code
that would recreate the original defect the moment anyone wired it.** **One is a genuine but bounded
duality worth knowing about.** The remaining five are clean, and two of them are clean in a way that
is instructive: the codebase already contains the correct pattern for this defect class and simply
never applied it to cost.

---

## F1 — LIVE DEFECT: admission and arbitration price the same trade with different expected moves

**Severity: HIGH (latent). This is the original defect, in the benefit term instead of the cost term.**

`Engine::gate_evaluate` makes two consecutive decisions about one candidate, using two different
answers to "what do we expect this trade to make":

```rust
// engine.rs:2664 — ADMISSION
let move_override = if self.cfg.expected_move_model_enable {
    self.expected_move.estimate(vsol, obs, …).known_bps()   // PER-CANDIDATE
} else { None };
match decide(&cand, confirmation, &self.cfg, move_override) { …

// engine.rs:3138 — ARBITRATION, same function, ~470 lines later
let edge_bps = self.conditional_edge_bps(cand.lane) - i128::from(rt_bps);  // PER-LANE
let expected_net = i128::from(size).saturating_mul(edge_bps) / 10_000;
```

Admission prices candidate X on the per-candidate stratified estimate. §23 slot arbitration then
ranks candidate X by the per-**lane** estimate — roughly six numbers for the whole universe —
**discarding `move_override` entirely**. The two can differ by the model's full range: base plus up
to `MAX_TOTAL_LIFT_BPS` (1,000 bps), which on a 0.1 SOL clip is 10,000,000 lamports of `expected_net`
mis-ranking, more than half the golden book, applied to the decision that allocates the scarce
position slots.

**Not live today** — `expected_move_model_enable` ships `false`, so `move_override` is always `None`
and arbitration's per-lane estimate is the only one in play. That is exactly why it must be fixed
*before* the model is ever armed: the day someone turns it on, admission and ranking silently start
disagreeing and nothing fails.

**Minimal fix:** thread the resolved expected move from `gate_evaluate`'s admission step into the
arbitration term, so `edge_bps` is computed from the same number that priced the band, falling back
to `conditional_edge_bps` only when the estimator refused.

## F2 — DEAD CODE THAT RECREATES THE ORIGINAL DEFECT: `scalp::scalp()`

**Severity: LOW today, MEDIUM latent.**

`scalp.rs` builds a complete fourth round-trip cost model:

```rust
entry_fee_bps: cfg.entry_fee_bps,          // 100 — NOT venue_fee_bps_per_leg (125)
exit_fee_bps:  cfg.exit_fee_bps,           // 100
first_sell_penalty_bps: cfg.exit_fee_bps,  // the penalty re-pin #26 DELETED as double-counted impact
fee_escalation_bps: cfg.entry_fee_bps,
impact_k_bps: cfg.sim_impact_k_bps,        // 50 — a THIRD impact model, unrelated to the curve
```

Every one of those is a value the cost-model unification retired. `Config` now documents
`entry_fee_bps` / `exit_fee_bps` / `entry_tip_lamports` / `exit_tip_lamports` as **decision-inert,
retained only so an existing operator config still parses** — and that claim is TRUE, verified: the
only live caller into `scalp.rs` is `capacity_report`, which `engine.rs` documents and this audit
confirms is report-only.

**`pub fn scalp()` itself has no callers anywhere in the workspace**, tests included. It is public API
that looks authoritative, carries the retired arithmetic, and would silently reintroduce the exact
four-way divergence re-pin #26 removed the moment anyone wired it.

**Minimal fix:** delete it, or gate it behind an explicit `#[deprecated]` naming `cost_model` as the
authority. Deleting is better — dead code that encodes a retired doctrine is worse than no code.

## F3 — GENUINE BUT BOUNDED: two independent depth numbers for one pool

**Severity: MEDIUM.**

`Features::liquidity_lamports` (the curve's SOL-side reserve, `vsol`) and
`Confirmation::sellable_depth_lamports` are independently sourced and feed **different** constraints:

* `liquidity_lamports` → `cost_model::impact_den_for` → the impact model, i.e. what our order costs.
* `sellable_depth_lamports` → `size_band`'s `sellable_max_lamports` → the `x_max` capacity cap.

On a pump.fun bonding curve these describe **the same physical reserve** — you sell back into the pool
you bought from. Two independent numbers for one quantity, with nothing enforcing agreement, is the
defect class. If a decoder ever reports a sellable depth materially above `vsol`, the capacity cap
permits a size the curve cannot absorb while impact is priced off the smaller number.

**Why it is bounded rather than urgent:** both are conservative in the same direction in every fixture
today, and the gate refuses on `sellable_depth == 0`. But nothing *checks* the relationship.

**Minimal fix:** on a bonding-curve venue derive sellable depth from `vsol` rather than accepting an
independent value; or, at minimum, assert `sellable_depth <= vsol` and refuse (never clamp) when it
is violated, so a bad decode fails loud instead of oversizing.

## Clean — with proof, because negative results are the point of an audit

**Bankroll (CLEAN, and instructive).** Exactly one authority, and it is *typed*: `BankrollOrigin` is
either `PaperSeed(cfg.bankroll_initial_lamports)` or a live reconciled balance, and `engine.rs`
computes the sizing base "from the ORIGIN, never `cfg.bankroll_initial_lamports` directly". The
operator's standing rule — live bankroll must come from the reconciled wallet, never the config seed —
is enforced by the type system rather than by convention.

**This is the pattern the cost model should have had from the start.** The codebase already knew how
to solve this defect class; it simply never applied it to cost. Any future fix to F1/F3 should copy
`BankrollOrigin`'s shape: make the provenance a type, not a comment.

**Price (CLEAN).** One producer, `lane.rs::latest_price_fp`. Entry price is derived from it through
`curve_fill`, not sourced separately.

**Impact (CLEAN since re-pin #26).** `cost_model::impact_den_for` and `curve_fill::own_impact_bps` are
now provably the same function — the gate's linear model equals the constant-product curve exactly at
`den = vsol/10_000`. `sim_impact_k_bps` is the simulator's own model but reaches only the dead
`scalp()` path and the report-only capacity curve (see F2).

**Fees (CLEAN since re-pin #26).** `venue_fee_bps_per_leg` is the single authority; the four legacy
config fields are decision-inert and documented as such, verified by tracing every reader.

**Size clamps (CLEAN).** They compose through one path rather than clamping independently:
`size_band` produces the band, `floor_size_band` lifts the lower edge to `min_trade_size_lamports`,
`apply_size_mult` can only *reduce* and re-clamps into the same band, and
`max_concurrent_positions` gates admission separately and is journalled. Each stage is
order-dependent by design and documented as such.

---

## Verdict for the server run

**F1 must be fixed before `expected_move_model_enable` is ever set true.** It is harmless while the
model is disarmed and becomes a silent mis-ranking the moment it is armed. Nothing else on this list
blocks a Phase-B stand-up: F2 is dead code, F3 is a consistency assertion worth adding but is not
currently violated by any live path.

The broader answer to the operator's question is: **no, there is not another three-way cost silo.**
There is one two-way *benefit* silo (F1), one dead fourth cost model (F2), and one unasserted
duality (F3). The system is materially more synthesised than it was this morning, and the one place
it is not is a place nothing currently reads.
