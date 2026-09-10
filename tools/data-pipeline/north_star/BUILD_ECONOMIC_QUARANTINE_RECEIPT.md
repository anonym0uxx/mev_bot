# Bounded Slinky economic quarantine loader receipt

## Outcome and boundary

Implemented and exercised `north_star.economic_quarantine` against the **existing saved development sample**, not the original parquet corpus. It refuses affected economic fields and named dependent utility/target requests for calibration, teacher, target and training use **before opening the sample payload**. Successful access returns `REFERENCE_ONLY_ALLOWED` with `admitted`, `training_approved`, `economic_certified` and `fit_authorized` all false.

**API call-site limitation:** no existing loader, main admission gate, training pipeline or gateway was modified or wired to this API. Existing callers that do not invoke it remain outside its enforcement. This is bounded library enforcement, not gateway-independent deployment permission, global ancestry enforcement, an OS sandbox or training approval. Python monkeypatching, direct filesystem access and hostile concurrent filesystem replacement are outside this in-process trust boundary. Exact-path resolution rejects an existing redirected sample path; it is not a race-free OS capability. Registry remains byte-identical and still honestly describes its original declarative publication status.

## Reviewed inputs and exact pins

Read `SLINKY_ECONOMICS_QUARANTINE.json` and `BUILD_SLINKY_ECONOMICS_AUDIT.md`; no edits to either.

- Registry: `north_star/SLINKY_ECONOMICS_QUARANTINE.json`, 30,461 bytes, SHA256 `0019b3455d9c6aebcafa85ee5fe0139aa9933583089a4bbd4d4d001dc62a4c9b` (review provenance supplied by delegation: `deleg_9abb77f8`; this implementation has not received a new independent review).
- Only payload path: `D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/development_samples.json`.
- Payload: 1,531,054 bytes, SHA256 `d1068309e3c16053d32c7ba55f8fcb7e6d44851c4ca8c03004331716f6054ae7`.
- Only split literal: `DEVELOPMENT_EXPOSED`. Protected, unknown and differently spelled splits fail closed.
- Exact request lineage is derived from the pinned registry; producer hash is a current-source receipt, **not proof that every original runtime input was pinned**. Registry's truncated composite and missing exact compactor/downstream build provenance limitations remain intact.

Registry bytes/hash are checked on **every call, before request processing and any sample access**, then its sample evidence binding is checked. No caller-supplied registry path, replacement digest, approval flag or allow-quarantine override exists. Reads are capped at exact expected bytes plus one overflow byte. Allowed reference access hashes payload bytes before JSON decoding, validates all three 128-row layers' producer provenance and identity joins, and returns only explicitly requested fields and a 1..128 prefix. The JSON is decoded in full: lowering `max_rows` limits returned rows, not parser work. No parquet or whole-corpus scans occur.

## Field semantics

- The request vocabulary is finite and literal `layer.column`; no glob expansion, inferred alias, automatic normalization or arbitrary nested path. Unknown fields fail closed even if present in the saved sample. Counterfactual fields are explicitly enumerated; observation inventory is deliberately a smaller allowlist.
- The two registry defect pattern lists classify nonbenchmark size exit/gross/net fields and all size/exact net fields. All five `sz*_feasible` fields are separately covered by the registry's third, prose-described censoring defect.
- `dependent_targets.utility`, `robust_utility`, `relative_rank`, `recommended_action`, `recommended_size`, `target_gate`, `feasibility`, `capacity`, `median_net_return_pct`, `profitability`, `size` and `rank` are **denial selectors**, not claimed saved columns or generated targets. Unknown descendants also fail closed. Audit mode never computes dependent targets.
- Affected fields can be viewed only in audit-reference mode with `ECONOMIC_QUARANTINED_UNADMITTED` status. Raw observation inventory is a separate API that refuses counterfactual and dependent fields before payload access.
- Barrier `net_pnl_sol`, `net_return_pct`, `net_return_bp`, classes, and matched-quantity `sz050` exit/gross fields are **not attributed to these defects**. Reference status is `NOT_ATTRIBUTED_TO_THESE_DEFECTS_UNADMITTED`; target use fails with `UNADMITTED_NOT_DEFECT_ATTRIBUTION`, not a false claim that every barrier label shares the surface bug. No clean-label certification is given.
- Saved observation inventory retains source identities, observed values, unknowns and censoring without relabeling. `OBSERVATION_REFERENCE_UNADMITTED` does not authenticate original transaction bytes or certify source truth.
- Original sample bytes remain on disk unchanged; returned row mappings/status are immutable and values are not recomputed. No broad full-payload byte escape is returned by the projection API.

## Public API example

Run from `tools/data-pipeline` with `PYTHONPATH=src`:

```python
from north_star.economic_quarantine import (
    load_saved_sample, inventory_saved_observations,
)

lineage = {
    "source": "slinky21",
    "run_uuid": "ef113bd1-f5e",
    "pipeline_version": "3.1.0",
    "source_hash": "42132c2533b9effd",
    "code_config_hash": "af87ffefd9988a42",
    "execution_config_hash": "89207735e3c0f866",
    "producer_sha256": "db2530be539a1cfb03f01aa096e74a88d4eb920c458b0e13e029e25379722034",
}
request = dict(
    sample_path="D:/mev_bot-artifacts/north_star/development/slinky_economics_audit_v1/development_samples.json",
    fields=("counterfactual_trade_v3.sz005_net_return_bp",),
    lineage=lineage, split="DEVELOPMENT_EXPOSED",
)
# Raises ValueError: ECONOMIC_QUARANTINE before opening payload:
try:
    load_saved_sample(**request, use="target")
except ValueError as refusal:
    print(refusal)

reference = load_saved_sample(**request, use="audit_reference", max_rows=128)
observations = inventory_saved_observations(
    **{**request, "fields": ("pump_state_v3.state_id", "pump_state_v3.trade_side")},
    max_rows=3,
)
assert reference.disposition == observations.disposition == "REFERENCE_ONLY_ALLOWED"
assert not reference.training_approved and not observations.admitted
```

## Executed evidence

Strict TDD executions:

1. First refusal test: **1 failed**, expected assertion `quarantine loader missing` before implementation.
2. Minimal refusal implementation: **1 passed**.
3. Expanded field/request/pin/lineage/access tests: **40 failed, 38 passed** against minimal implementation.
4. Bounded implementation: **78 passed**.
5. Added redirected-path regression: **1 failed, 78 deselected**, observed payload-open guard trigger before path-resolution check.
6. Implemented redirect rejection; final owned test run: **79 passed in 0.12s**.
7. North Star suite snapshot: **2054 passed in 14.07s**. Shared worktree has concurrent owners; this is the actual observed snapshot, not an inferred baseline-plus-test total.

Commands:

```bash
python -m pytest tests/north_star/test_economic_quarantine.py -q
python -m pytest tests/north_star -q
```

Separate real saved-sample execution instrumented `io.open` for the exact sample path and printed:

```json
{"target_refusal": "ECONOMIC_QUARANTINE: counterfactual_trade_v3.sz005_net_return_bp", "payload_opens_before_refusal": 0}
{"disposition": "REFERENCE_ONLY_ALLOWED", "rows": 128, "training_approved": false, "before_sha256": "d1068309e3c16053d32c7ba55f8fcb7e6d44851c4ca8c03004331716f6054ae7", "after_sha256": "d1068309e3c16053d32c7ba55f8fcb7e6d44851c4ca8c03004331716f6054ae7", "original_bytes_unchanged": true, "registry_sha256": "0019b3455d9c6aebcafa85ee5fe0139aa9933583089a4bbd4d4d001dc62a4c9b"}
```

Tests cover tampered registry before even an invalid-field request, same-size producer-byte tampering before JSON decode, unknown/duplicate/wildcard/path fields, every declared lineage identity changed separately, missing/extra lineage, protected split, redirected path, invalid row bounds, raw-inventory laundering, forbidden override keywords, immutable projections, exact value preservation, and defensive decoded producer/join validation. Synthetic tampering is in-memory interception only; reviewed registry/sample files are never mutated by those tests.

## Not done / not claimed

No original live-repo mutations, label rewrites, legacy loader edits, source admission, corrected training positives, prevalence estimates, network calls, model calls, training, commits or global deployment changes. Audit figures (115 reproduced rows, 575 scenarios, 460 quantity mismatches and 575 tip omissions) remain inherited **bounded diagnostics**, not population estimates, independent trade counts or target-support evidence. Full downstream record ancestry, corrected immutable lineage, independent implementation review and real call-site integration remain outstanding.

Only owned deliverables: `src/north_star/economic_quarantine.py`, `tests/north_star/test_economic_quarantine.py`, and this receipt.
