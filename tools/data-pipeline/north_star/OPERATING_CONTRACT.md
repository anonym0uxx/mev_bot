# North Star operating contract — foundation, NOT GO

## Authority and objective

Later operator decisions in the approved audit/build plan govern older master/v6 model-role prose: **Qwen = trader-only; Astra = Rust developer**. Execution scope is **Solana, pumpfun/Pump.fun and pumpswap/PumpSwap**, with explicit migration continuity and independently verified economics. Other chains/venues are context-only, not execution authorization. Required narrative D07/D08/D09 cannot be replaced by a numerical-only release.

The objective is Kelly-informed sizing and net returned SOL, with reconciled all-in portfolio equity, failed costs, open/unsellable inventory, capital-time, tail risk and equal-capital cash/frozen-baseline comparisons. Kelly is not a numeric bankroll, an approved leverage/risk cap, a fixed take-profit rule or evidence that estimated edge exists. Sizing distributions, uncertainty shrinkage, capacity/correlation and the supported execution envelope must be development-calibrated after reservation; no tuning in this tranche.

## Unknown production values remain unknown

Unresolved: deployment capital; maximum order size; concurrent positions; maximum hold/runner policy; latency/freshness; drawdown; correlated exposure; fractional Kelly cap; meaningful incremental uplift; uncertainty confidence/sample sufficiency/stopping rules; cost/latency stress and fee model; frozen baseline hashes; risk-policy hash; approved numeric limits with evidence and effective timestamp. `EVAL_FREEZE.json` separately leaves all embargo durations null and future reservation pending. No values from test fixtures are production proposals.

`contracts.assert_stage_allowed` enforces required numeric/scope/version/hash-presence preconditions for strategy/model/economic tuning and promotion. Positive integers use named native units; basis-point fractions cannot exceed their denominator; confidence cannot equal certainty; order size cannot exceed capital. Missing/placeholder/bool/nonfinite/fractional integer values fail closed. Required hashes include risk, fee, baseline and evaluation policy. Numeric approval needs an explicit true flag, evidence ID and timestamp.

This is **structural validation, not authentication or permission issuance**. A string that looks like a hash does not prove a baseline exists. Production callers must verify artifact contents, independently authenticate the approval and apply the evaluation before-fit and interval gates plus stage-specific admission/rights controls. A forged or fixture approval must never be treated as user consent. Independent whole-system promotion gates still apply. This helper alone cannot start training, tune parameters, place orders or promote a model.

## Permitted foundation work versus gated work

| Work | Numeric contract status |
|---|---|
| Read-only inventory | Unresolved live limits do not block |
| Test-only parser and small ledger fixtures | Unresolved live limits do not block |
| Real teacher/feature/cluster/retrieval fit, calibration, curation | Evaluation reservation first; current policy blocks |
| Strategy/model/economic tuning or promotion | Approved complete numerical contract plus every independent gate; currently blocked |
| Training run or live orders | Not authorized by this module; always refused here |

Safe-stage validation is not a generic exemption for collection rights, private access, resource limits, source transforms or source admission. Exact permissive license/evidence, source/version lineage and operator inclusion approval remain independent. No human decision history is invented; deterministic fixtures are test-only, not human-origin labels or corpus evidence. No duplicate padding or Qwen/agent-generated teaching prose.

## Runtime safety scope

The trader cannot edit/deploy Rust, change its risk limits, access signing authority or convert social text into instructions. Deterministic execution checks balances, pending reservations, inventory, idempotency, expiry and reconciliation. Protective actions cannot require model availability and are not guaranteed fills. No collector, signer, live process, current training run, frozen legacy dataset or original dirty checkout was modified by this boundary task.
