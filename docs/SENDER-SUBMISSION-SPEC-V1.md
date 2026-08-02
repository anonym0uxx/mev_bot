# Helius Sender — submission-route spec (V1)

**Status:** policy leaf implemented and tested; transport layer not yet built.
**Date:** 2026-08-01
**Crate:** `pump-quant-execution` → `ex_sender_route`
**Supersedes, in part:** the multi-sender blast described in `7cb1452`
(`sniper RPC fallback blasts Nozomi+Helius+public mainnet in parallel`).
**Constitution refs:** §22 (integer/bps only), §24(b) (no `unsafe`; see §9).

---

## 1. What Sender is

A single submission endpoint that fans one transaction out to Helius **staked
connections** and the **Jito block engine** simultaneously — plus additional
builders (Harmonic, Rakurai) on the Max tier. It therefore subsumes what this bot
currently reaches through three separate senders, behind one call, one tip, and
one failure surface.

| Property | Value |
|---|---|
| Global endpoint | `https://sender.helius-rpc.com/fast` |
| Regional endpoints | `http://{slc,ewr,lon,fra,ams,sg,tyo}-sender.helius-rpc.com/fast` |
| Methods | `sendTransaction` (base64, `skipPreflight: true`, `maxRetries: 0`), `sendBundle` (≤ 4 txs, atomic) |
| Mandatory per tx | a SOL transfer to a tip account **and** a compute-unit-price instruction |
| Rate limit | 50 TPS default, raisable on request |
| API credits | **none consumed** |
| Options | `?swqos_only=true`, `?mev-protect=true` |

**No credits consumed** is the headline for our budget. Every send is free of the
$499/mo credit allowance, so the credit governor only has to budget *data*
(LaserStream, RPC reads) and never *execution*. That materially simplifies the
renewal-gate work and removes execution volume as an input to it.

---

## 2. The tip-minimum discrepancy — UNRESOLVED

Helius publishes two different Max-tier minimums on its own surfaces:

| Source | Max / Jito tier | SWQoS-only |
|---|---|---|
| Dashboard (2026-08-01) | 0.0002 SOL | — |
| Zero-slot blog post | 0.0002 SOL | 0.000005 SOL |
| Sender documentation page | **0.001 SOL** | 0.000005 SOL |

A 5× spread on the tier that dominates cost at our trade size. The SWQoS figure
is consistent everywhere and equals one signature's base fee.

**Default is the conservative value (0.001 SOL).** Over-reserving budget declines
a marginal trade; under-tipping pays the tip *and* fails to land, which is
strictly worse. Both constants are exported
(`MAX_TIER_MIN_TIP_LAMPORTS_DOCS`, `MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD`) and the
active value is a config field, so resolving this empirically is a config change,
not a code change.

**Open action:** submit at each level and record landing rate and slot delta.
Until then no claim about Max-tier economics is settled.

---

## 3. The economics — why the tip is the whole design

Tips are a **fixed lamport charge per send**, so cost as a fraction of trade value
is regressive. The bot's own ATA-rent finding (0.00203928 SOL reading as 203 bps)
implies a position size near **0.1 SOL**. And `ex_sell_ladder_escalate` means an
exit is not one send — call it one buy plus two ladder rungs, three sends.

Total tip cost per position, three sends, as bps of position value:

| Route | Per send | Per position | @ 0.1 SOL | @ 0.5 SOL | @ 1 SOL |
|---|---|---|---|---|---|
| SWQoS-only | 0.000005 | 0.000015 | **15 bps** | 3 bps | 1.5 bps |
| Max @ 0.0002 | 0.0002 | 0.0006 | **60 bps** | 12 bps | 6 bps |
| Max @ 0.001 | 0.001 | 0.003 | **300 bps** | 60 bps | 30 bps |

At 0.1 SOL, Max on the documented minimum costs **300 bps** — larger than the ATA
rent finding and 1.5× the entire pump.fun round-trip fee. It would consume the
strategy. SWQoS-only at 15 bps is noise.

Two conclusions:

1. **SWQoS-only is the default route.** Max is an escalation, not a baseline.
2. **The cost structure argues for larger positions independent of Sender.**
   Fixed per-trade charges — ATA rent, tips, base fees — dominate at 0.1 SOL.
   This bears directly on the target-band thesis and is a strategy question, not
   an execution one.

---

## 4. The tip-budget rule

```text
edge_lamports = entry_edge_bps * trade_size / 10_000
tip_budget    = edge_lamports * tip_budget_bps / 10_000     (default 1_000 bps = 10%)
total_tip     = compute_tip(tier_floor, congestion, urgency) * expected_sends
```

Choose the highest tier whose `total_tip <= tip_budget`. If **no** tier fits —
including the SWQoS floor — the decision returns `economic: false`.

`economic: false` means **the trade is uneconomic to submit and must not be
sent.** It is not a suggestion to fall back to the cheap tier and proceed. This is
the module's ability to answer *no*, and it is the property under test in
`negative_control_uneconomic_trade_is_declined`.

Congestion and urgency scale the floor through the existing
`ex_tip_compute::compute_tip`, so a hot market can price Max out of budget and
force a step down to SWQoS rather than an overspend. Asserted in
`congestion_and_urgency_can_price_max_out_of_budget`.

---

## 5. Architecture — additive by construction

`Route`, `RouteCtx` and `route_ev_lamports` are **unchanged**. No variant was
added to `Route`; no field was added to `RouteCtx`. Every existing caller and
test compiles untouched. Verified: the `lib.rs` diff is +5 lines, −0.

Composition happens in a new type:

```rust
pub enum SubmitPlan {
    Legacy(Route),
    Sender { tier: SenderTier, mev_protect: bool },
}

pub fn choose_submit_plan(&RouteCtx, &SenderCtx) -> PlanOutcome
```

`PlanOutcome` reports the winning plan, the full `SenderDecision`, and **both**
expected values, so the comparison is auditable rather than a bare enum.

Sender is considered only when it is healthy **and** within budget, and it needs
**strictly greater** EV to displace the legacy winner. A tie preserves today's
behaviour — a new submission path proves itself or does not ship. Asserted in
`sender_wins_only_on_strictly_greater_ev`.

---

## 6. Two model defects — both FIXED

Both fixes are additive to `ex_route_policy.rs`: **+62 lines, −0**, two new
public functions, no change to `Route`, `RouteCtx`, `route_ev_lamports`,
`choose_route` or `select_forced_exit_route`. Every existing caller and test
keeps its current behaviour byte-for-byte.

### 6.1 `route_ev_lamports` charged a route's tip exactly once — FIXED

With the sell ladder an exit submits several tipped transactions, so the
single-charge model understated fee cost by roughly the ladder depth — and
understated it most for precisely the routes that cost most, biasing selection
toward expensive routes exactly when the ladder is deepest.

**Fix:** `route_ev_lamports_with_sends(mode, ctx, expected_sends)`. Only the fee
term scales; gross edge, the private-route slippage credit, latency decay and
failure cost are properties of the position, not of how many attempts it takes to
close it. `expected_sends == 0` is treated as `1`.

`route_ev_lamports` is unchanged and delegating callers get identical results at
`expected_sends == 1` — asserted in `fix_1_legacy_ev_now_charges_every_send`.

`choose_submit_plan` now prices **both** sides with the same send count. Before,
the legacy side was charged one tip and the Sender side all of them, which made
Sender look worse by exactly the ladder depth — a bias in favour of the
incumbent, which is the direction that hides a real improvement. Asserted in
`fix_1_plan_comparison_charges_both_sides_for_the_ladder`.

### 6.2 `Route::Rpc` is modelled as free, so unmeasured health made Sender unreachable — FIXED

`route_ev_lamports` charges `Route::Rpc` a fee of **zero**, so RPC's entire
disadvantage lives in its latency and failure inputs. With those at their default
of `0` — precisely what an unwired health feed produces — RPC scores gross edge
with nothing deducted and is unbeatable by any tipped route, Sender included.

The return value cannot distinguish *"RPC is genuinely best"* from *"we measured
nothing"*.

**This cannot be repaired by arithmetic.** Inventing a penalty for RPC would
fabricate the measurement the feed failed to supply — the same class of error as
crediting `mev-protect` for a benefit nobody measured.

**Fix:** `route_health_is_measured(&RouteCtx)` returns `false` when every latency
and failure input is zero. `choose_submit_plan` fails closed on it: Sender is not
selected, its EV is reported as `i128::MIN` rather than a number that looks
considered, and `PlanOutcome::health_measured` tells the caller *why*. A caller
seeing `health_measured == false` should report the health feed as broken, not
read the plan as a choice.

Wiring route health remains a **prerequisite** for Sender to do anything at all.
The difference is that the system now says so instead of silently preferring RPC
forever. Asserted in `fix_2_unmeasured_health_is_detected`,
`fix_2_negative_control_unmeasured_health_fails_closed` and
`fix_2_measured_health_re_enables_sender`.

---

## 7. Operational notes

**Use the HTTPS global endpoint.** The regional endpoints are plaintext `http://`
and are meant for colocated callers. Submitting a signed transaction in the clear
from outside the datacentre exposes it to any on-path observer before it lands —
a free front-run against a memecoin entry. The TLS handshake amortises across a
reused connection.

**All ten tip accounts are committed** in `SENDER_TIP_ACCOUNTS`, each
base58-decoded to exactly 32 bytes and the set checked for duplicates before
being written. `tip_accounts_are_valid_and_distinct` re-proves that at test time
so a later edit cannot quietly damage one.

Note the sixth entry is **43 characters, not 44**, and that is correct: a pubkey
whose leading byte is small encodes to a shorter base58 string. Length alone is
not a validity test — which is exactly why the validator decodes rather than
measures.

`is_valid_tip_account` decodes and checks two things:

1. every character is in the base58 alphabet and the value fits in 32 bytes;
2. the count of leading zero bytes equals the count of leading `'1'` characters.

Condition 2 is what upgraded this from a plausibility guess to a real check. A
**truncated** address — the exact damage a screenshot transcription causes —
decodes to a smaller number that still fits in 32 bytes, producing leading zero
bytes its string has no leading `'1'`s to justify. It is now rejected, and
`truncated_address_is_now_rejected` proves it on the 36-character truncation that
the previous length check accepted.

What it still cannot catch, stated so nobody over-trusts it: a **transposition**
that leaves a well-formed 32-byte address. `address_validator_rejects_the_usual_damage`
asserts that blind spot deliberately. Only the dashboard copy button protects
against a typo that stays valid.

**Tip accounts are rotated by a caller-supplied seed** (slot number or blockhash
bytes — never a random value, so replay reproduces the choice). A tip transfer
takes a write lock on its destination; every bot tipping the same account
serialises against every other.

**`mev-protect=true` earns no EV credit.** It should reduce realised slippage by
routing around sandwich-associated validators, but that benefit has not been
measured on this bot's own flow. Crediting an unmeasured benefit is how an EV
model learns to prefer the option it was never tested on. Measure, then model.
Asserted in `mev_protect_earns_no_ev_credit`.

---

## 8. What is NOT built — implementation plan for V2

This spec covers the **policy leaf only**. `pump-quant-execution` is a pure
decision crate: no network, no clock, no RNG, no floats. The transport belongs
elsewhere and is not written.

Remaining work, in dependency order. Each is small; the policy, the arithmetic,
the validation and the tests are already done.

1. **Route health feed — the gating item.** Populate `rpc_latency_ms`,
   `rpc_fail_bps`, `sender_latency_ms`, `sender_fail_bps` from observed landing
   outcomes. Per §6.2 the system will now decline to select Sender until this
   exists, and will say so via `health_measured`. Nothing else in this list
   matters first.
2. **Transport.** One JSON-RPC POST. `sendTransaction`, base64,
   `skipPreflight: true`, `maxRetries: 0`. The endpoint host comes from `Creds` /
   config, never a literal. Roughly 60 lines including error handling.
3. **Instruction assembly.** Every Sender transaction carries a tip transfer
   *and* a compute-unit-price instruction. Both, not either. The tip destination
   comes from `tip_account_from(&SENDER_TIP_ACCOUNTS, seed)` with the seed being
   a slot number or blockhash bytes — never a random value, so replay reproduces
   the choice.
4. **Tip-tier plumbing.** Feed `expected_sends` from the sell-ladder state so an
   exit is priced at its real depth rather than as a single send. Both sides of
   the comparison already accept it; this is wiring, not logic.
5. **Empirical tip resolution.** §2. Submit at both minimums, record landing rate
   and slot delta, then pin the config value with the evidence.
6. **Land-time measurement.** Helius claims ≤ 1.5 slots typical. That is a
   marketing claim with no published statistics behind it. Measure it on our own
   flow before any number enters the paper-trading fill model.
7. **Deprecation review.** If Sender's measured landing profile dominates,
   `Route::Nozomi` becomes redundant and should be removed with its config,
   credentials and tests. Do not remove it on the strength of this spec alone.

---

## 9. Dossier and safety status

`pump-quant-execution` is `#![forbid(unsafe_code)]`. `ex_sender_route` adds no
`unsafe` and no dependency; it is std-only and pure. **No `docs/dossiers/` entry
is required or created** — §24(b) applies to `unsafe` blocks, and there are none.
`verify_dossiers()` is unaffected by this change.

---

## 10. Verification performed

- `cargo test` — **27/27** pass in `tests/ex_sender_route.rs`.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo fmt -- --check` — clean.
- `lib.rs` diff against HEAD — **+5 / −0**.
- `ex_route_policy.rs` diff against HEAD — **+62 / −0**, two new public
  functions, nothing removed or altered.
- All ten tip accounts base58-decoded to exactly 32 bytes, duplicate-checked,
  before being committed.

Compiled and run against the real `ex_route_policy.rs` and `ex_tip_compute.rs`
sources, so cross-module types and arithmetic are verified rather than assumed.

**Eight of the 27 are negative controls**, which is the number that matters:

| Control | What it proves can fail |
|---|---|
| `negative_control_uneconomic_trade_is_declined` | the budget rule can say no |
| `negative_control_zero_edge_is_never_economic` | zero edge funds no tip |
| `negative_control_uneconomic_sender_never_wins_the_plan` | a declined tier cannot win |
| `negative_control_unhealthy_sender_never_wins` | health gates selection |
| `negative_control_tip_account_rejects_bad_input` | the selector fails closed |
| `fix_2_negative_control_unmeasured_health_fails_closed` | an uninformative comparison produces no score |
| `truncated_address_is_now_rejected` | the validator catches the real damage |
| `address_validator_rejects_the_usual_damage` | and states the blind spot it does not catch |

A budget rule that cannot decline is not a rule; a validator that cannot reject
is not a validator.
