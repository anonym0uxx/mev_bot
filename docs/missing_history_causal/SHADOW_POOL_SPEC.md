# SHADOW_POOL_SPEC — `paper_fill_v2_shadow`

Status: implemented as library modules on branch `task/slice-shadow`, **not yet wired into the engine executor**.
Code: `rust/crates/pump-quant-app/src/{shadow_pool,settlement,exec_identity}.rs`.
Laws: `tests/{shadow_pool,shared_settlement,exec_identity}.rs`.

## 1. Versioning
- `PaperFillVersion::V1Observed` (`paper_fill_v1_observed`, the default and current executor behaviour)
  and `PaperFillVersion::V2Shadow` (`paper_fill_v2_shadow`) are SEPARATE models. V2 is not a patch that makes v1 optimistic.
  The durable shadow book carries `version: "paper_fill_v2_shadow"`; any other version string is refused on restore.
- v1 results stay reproducible: nothing in v1 code paths changed (golden_digest 2/2 and m3_exec_economics 8/8 still pass).

## 2. Evidence vs model
- **Observed on-chain state is immutable evidence.** `CurveBase`/`PoolBase` records are never written to.
- **External-liquidity benchmark** = the size-specific quote (`exec_quote::curve_sell`/`amm_sell`) on the observed state alone.
  It is always reported (`ShadowSellView.external`) next to the shadow result, never replaced by it.
- **Shadow pool** = observed reserves + OUR carried delta (`sol_in - sol_out`, `tok_out - tok_in`), kept per mint in
  `ShadowMarket`. Only our reconciled simulated fills change the delta.
- **Counterfactual limit (stated explicitly):** our trade could have changed later participants' behaviour
  (who bought, how much, whether a sniper sold into us). Replay cannot establish that counterfactual. The shadow model assumes
  later observed flow is unchanged and adds our delta on top; that assumption is a modelling choice, not evidence.

## 3. Update rules
| Event | Rule |
|---|---|
| Our fill (BUY/ADD/SELL) | `apply_own_fill(mint, basis, leg, order_id, cum_tokens, cum_lamports)`. Idempotent by `(leg, order_id)` + cumulative: equal -> `Duplicate`, smaller -> `Backwards` (no change), larger -> only the increment is applied. The first fill also records the reconciliation basis (curve virtual offsets or pool k) from the landing state. |
| Later observed trades / fresh snapshot | `on_curve_observation`/`on_pool_observation`: the new observed state REPLACES the base; the delta is carried once (not re-added per snapshot). |
| Fees | Venue fees are inside the size-specific quote; only the venue-net SOL that entered/left reserves (`net_in`, sell `gross`) is in the delta. Network fee: `NETWORK_FEE_PER_LANDED_LEG_ESTIMATE = 10_000` lamports/landed leg — a **labelled estimate** (measured p50), not observed truth. |
| Graduation | `on_graduation`: the curve shadow ENDS, the pool segment starts with an EMPTY delta on the observed pool. Curve SOL is not carried into the pool. Fill identities survive (late duplicate stays duplicate; a late new curve increment is `NotCarried`). |
| Restart | `to_json`/`from_json` round-trip exactly (incl. divergence, applied identities, basis). Malformed or wrong-version input -> `Err`, never an empty book. |

## 4. Reconciliation and divergence (named, permanent per segment)
A snapshot that cannot be reconciled with the shadow sets `Divergence` and DROPS the delta; a later consistent snapshot does
not revive it. Sale capacity then falls back to what the observed state alone supports.
- `shadow_divergence:curve_virtual_offset_changed` — `vsol - real_sol` moved (not a constant-product trade).
- `shadow_divergence:adverse_liquidity_change` — curve token offset moved / curve complete, or pool `k` decreased (withdrawal).
- `shadow_divergence:unreconcilable_snapshot` — observed reserves cannot fund what the shadow says (e.g. real SOL + delta < 0).
Capacity: `capacity = observed real SOL (or quote vault) + max(0, sol_in - sol_out)`; a shadow sell whose gross exceeds it is
refused `quote_unavailable:shadow_capacity_exceeded`. (Defence in depth: the venue quote's own real-SOL/vault guard already
binds first in every reachable case — see mutation 4.)

## 5. Engine hooks (for the stop table)
- `ShadowBook::divergence(mint) -> Option<Divergence>` (named; `None` = not diverged).
- `ShadowBook::net_liquidation_estimate(mint, Option<&ObservedVenue>, tokens) -> Option<i128>` — shadow venue-net minus the
  network estimate. `None` = unknown (diverged, no observation, venue mismatch, quote refused). Never zero, never cost.

## 6. Shared BUY/ADD/SELL settlement (`settlement.rs`)
One path `SettlementLedger::settle(FillReport)`; cumulative per `(mint, leg, order_id)`; refuses named
(`settle_refused:*`) and leaves the ledger unchanged; asserts `cash + committed == seed + realized` after every step. Sells
release basis pro rata; the last token releases the whole remainder. Network fee booked into `network_estimate` with label
`network_fee:estimate_p50_10000_per_landed_leg`.

## 7. Graduation execution identity (`exec_identity.rs`)
- UNSUBMITTED intent: may be re-routed to the pool, only after the pool route was verified.
- SUBMITTED/UNCERTAIN curve attempt: venue fixed; never re-labelled as a pool attempt; reservation + fill evidence stay attributed
  until definitive reconciliation (`Final` with cumulative fill, or `Failed` with zero fill).
- Replacement: explicit `AttemptId{intent, seq}` with `seq = previous + 1`, only when no attempt of the intent is open, route
  changed explicitly (`route_replacement_to_pool`, verified), quantity = remaining after reconciled fills (never resized up).
- No overlapping disposal: across all intents of a mint, open reservations <= held.
- Restart: open attempts become `Uncertain` (never reset to unsubmitted); durable form round-trips exactly.

## 8. 804a86fe regression (fixture in `tests/shadow_pool.rs`)
- Entry on the landing state (slot 445,637,772): `curve_buy(spend 115,840,829)` -> tokens 12,440,905,893,289 (= held.json
  inventory exactly), net_in 114,410,694.
- **External** (final observed state slot 445,637,874, real SOL 37,657,528): `curve_real_sol_insufficient`, max_tokens
  6,528,869,872,346 — the v1 result, unchanged.
- **Shadow at landing:** sell back <= net_in (no manufactured profit).
- **Shadow through the tape:** the next snapshot (slot 445,637,775) moves `vsol` by 2,366,586,160 while real SOL moves by
  44,667,771 -> `curve_virtual_offset_changed`; delta dropped; final shadow capacity 37,657,528; shadow sell refused;
  net liquidation estimate `None`. So: neither an endless refusal loop presented as correct nor a manufactured exit;
  the named result is "shadow cannot be reconciled with this tape".
- Tape-wide diagnostic (`proc/shadow_k_check.py`): 3,147 / 3,962 consecutive CurveObserved pairs are constant-product with
  constant `vsol - real_sol`; 815 are not. Root cause of the non-constant offset in the tape is OPEN (see report).

## 9. Not built (by instruction)
No agent-based market simulator. No engine wiring yet (the executor still runs v1).
