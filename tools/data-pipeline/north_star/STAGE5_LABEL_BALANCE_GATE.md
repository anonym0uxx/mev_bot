# North Star — Stage 5 Label Balance Gate & BUY-Count Audit (FROZEN SPEC)

Status: **spec frozen for Astra review.** Implemented as an acceptance test + fail-closed
exporter guard before any training export is admitted. This is the single gate that would
have caught the 22-BUY failure.

## 1. The failure this guards against

Prior CPT/SFT datasets contained **22 labeled BUYs** out of hundreds of thousands of
examples → the model learned to *never* emit BUY. Root cause: **label starvation**, not
data starvation. The counterfactual already held the economically-justified buys; the
*export* collapsed them to 22.

This spec makes that collapse **impossible to ship silently**: the exporter refuses to
produce a training set whose label distribution fails the floors below, and a pre-training
audit re-verifies before any launch.

## 2. Label vocabulary & source (must be pinned before export)

| Label | Meaning | Source (counterfactual_trade_v3) |
|---|---|---|
| BUY | execute a justified entry | `economic_class ∈ {STRONG, GOOD}` AND a capacity bucket `szXXX_feasible=true` AND `net_return_bp > 0` |
| SELL | execute an exit | exit side (`exit_reason`, `exit_feasible`; TP/SL/trailing) — **separate source from BUY** |
| SKIP | do not enter | `economic_class ∈ {SKIP, BAD, TOXIC}` (bad/rug/no-edge) |
| WATCH | accumulate evidence, no action | `economic_class = MARGINAL` |

⚠️ SELL labels come from the **exit-side** counterfactual, not the entry `economic_class`.
Stage 5 must pin the exact exit-label predicate before export — the gate below treats SELL
as its own counted class and fails closed if it's missing or collapsed.

## 3. Class-balance gate (fail-closed, applied at export)

The exporter MUST reject the batch (emit `BLOCKED`, write nothing) if any of:

1. **No empty class** — each of `{BUY, SELL, SKIP, WATCH}` has ≥ **1,000** examples.
2. **BUY floor (absolute)** — `count(BUY)` ≥ **10,000**.
3. **BUY floor (relative)** — `count(BUY) / total` ≥ **5%**.
4. **SELL floor (relative)** — `count(SELL) / total` ≥ **5%**.
5. **SKIP cap** — `count(SKIP) / total` ≤ **80%** (guarantees ≥20% is action-oriented).

Defaults are floor-of-margin minima against the measured raw distribution (STRONG+GOOD ≈
15.6%, TOXIC+BAD+SKIP ≈ 84%) — the exporter is expected to **down-sample SKIP/TOXIC/BAD**
to hit the SKIP cap, never to touch BUY/SELL downward. Ratios are operator-tunable; the
*fail-closed* behavior is not.

## 4. BUY-count audit (pre-training, mandatory)

Before any training run consumes a label export:

1. Load the export, count labels by class.
2. Re-assert all five §3 conditions against the *actual serialized file* (not the
   in-memory generator — a poisoned write must be caught).
3. Report a one-line signature: `labels={buy:N,sell:N,skip:N,watch:N} balance_ok=bool`.
4. If `balance_ok=false` → **hard stop**, no training, escalate to operator.

This is the exact check that would have printed `buy=22 balance_ok=false` and halted the
prior run.

## 5. Regression test (proves the gate catches 22)

```
test_balance_gate_rejects_starved_export:
    build a synthetic label set {BUY:22, SELL:0, SKIP:2000, WATCH:0}
    run the §3 gate
    assert == BLOCKED
    assert error mentions "BUY count 22 < 10,000" and "SELL missing"
```

Companion tests:
- `test_balance_gate_accepts_healthy`: a 20/20/40/20 split passes.
- `test_buy_audit_reads_file_not_generator`: a serialized file that fails → audit fails even
  if the generator claimed balance.
- `test_skip_cap_enforced`: SKIP 95% → BLOCKED regardless of BUY count.
- `test_leakage_still_applies`: the balanced export still honors Stage 1 split
  protections (ex-ante, no future fields, group-disjoint) — balance does NOT excuse leakage.

## 6. Relationship to release states (spec §8)

A label export is `READY` only if it passes §3 + §4 AND structural + rights + temporal +
economic-support passes independently. A numerically-balanced-but-leaky export is
`QUARANTINED`, never `READY`. Balance is necessary, not sufficient.

## 7. Why recurrence is now structurally prevented

| Prior failure | Now |
|---|---|
| 22 BUYs slipped through, un-checked | §4 audit refuses to train on `buy < 10,000` |
| label collapse was silent | §3 exporter fails closed, writes nothing |
| no pre-training verification | §4 is a mandatory, file-level, pre-launch gate |
| BUY/SELL/WATCH all starved | §3 requires every class ≥1,000 + BUY/SELL ≥5% |