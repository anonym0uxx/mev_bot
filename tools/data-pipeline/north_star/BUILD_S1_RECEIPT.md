# Stage 1 bounded foundation receipt

**Result: reviewer-reported boundary defects repaired and regression-tested. Stage 1 remains PARTIAL / NOT CLOSED.**

Verification snapshot: `2026-09-09T16:03:50.895497+00:00`, native Windows Python `3.11.15`. No commits, collectors, source data, training, tuning, economic evaluation or live orders were performed by this repair.

## Scope and location correction

Modified:
- `tools/data-pipeline/src/north_star/splits.py`
- `tools/data-pipeline/src/north_star/contracts.py`
- `tools/data-pipeline/tests/north_star/test_splits.py`
- `tools/data-pipeline/tests/north_star/test_config_contract.py`
- `tools/data-pipeline/north_star/EVAL_PROTOCOL.md`
- `tools/data-pipeline/north_star/BUILD_S1_RECEIPT.md`

Moved all four misplaced repository-root artifacts into `tools/data-pipeline/north_star/`: `EVAL_PROTOCOL.md`, `OPERATING_CONTRACT.md`, `BUILD_S1_RECEIPT.md`, `EVAL_FREEZE.json`. All destinations were absent; no conflicting files were overwritten. `OPERATING_CONTRACT.md` and `EVAL_FREEZE.json` moved byte-for-byte unchanged. The empty repository-root `north_star/` directory was removed. Tests now require the correct locations and absence of the root copies.

Other workers own other modules/tests in this shared worktree; their changes are not claimed here.

## Repaired boundaries

- Unknown sources always quarantine, including when an entity is registered DEVELOPMENT or protected. A window-only source outside its registered window also quarantines.
- Pending policies cannot claim protected source/entity assignments, materialized protected IDs or selected windows. Existing development exposure assignments remain legal.
- Nested policy object/list/scalar types are validated. Timeless reference exceptions require a list of nonblank string IDs and exact membership, never substring matching.
- Canonical hashing rejects non-string object keys recursively before JSON coercion, and rejects Python-only containers. Numeric mapping keys cannot reuse a string-key policy pin.
- The public policy digest is a read-only property; artifact receipts retain the verified pin.
- Training label end must be strictly earlier than the earliest protected window start minus the maximum declared dependency duration. Touching boundaries, inter-window gaps and all post-holdout intervals fail closed. Unknown durations still block acceptance.
- Malformed assignment IDs/parent closures quarantine. Malformed stages/receipts fail with ValueError. Contract approval evidence must be a nonblank string, nested approval/venue/limit structures are checked, and no new numeric limits are supplied.

Private Python internals are not an adversarial sandbox. Callers still must supply complete transitive dependencies, authenticate evidence and hashes, and enforce both parent and chronological gates.

## Actual strict TDD evidence

Commands ran from `D:/repos/mev_bot-north-star/tools/data-pipeline`. All new behavioral regressions were added and exercised before the corresponding implementation changes. Synthetic values are software fixtures, not actual reservations or approved risk parameters.

| Red regression batch | Actual result before repair |
|---|---|
| Unknown-source/entity bypass | 4 failed, 34 deselected |
| Pending protected assignments | 4 failed, 38 deselected |
| JSON key/container identity | 5 failed, 42 deselected |
| Nested policy types, digest mutability, future/gap training | 33 failed, 57 passed |
| Runtime inputs and nested contract types | 37 failed, 132 passed |
| Correct document locations and fit-stage types | 4 failed, 170 passed |

During test refinement, the malformed-root test was corrected to bypass the helper's `None` default, and the chronological positive fixture was placed before the existing maximum embargo. Two old expectations that explicitly permitted unknown-source entity admission and post-holdout training were corrected to the required fail-closed behavior.

Final focused command:

```text
python -B -m pytest tests/north_star/test_splits.py tests/north_star/test_config_contract.py -q
174 passed in 0.12s
```

Shared-suite snapshot, including concurrently owned modules:

```text
python -B -m pytest tests/north_star -q
486 passed in 1.53s
```

`git diff --check` returned exit 0. It does not cover untracked files; edited Python files also passed tool syntax checks. The focused suite verifies the checked-in pending policy and location correction. No independent approval of Stage 1 completion is claimed.

## Frozen policy remains pending

Canonical policy pin, unchanged:

`de8479ca8a255f892b71332d55ab1182e76de8e57fdf048195b50b0a25e14137`

The canonical input excludes the self-describing `policy_sha256` field. Verification loaded the relocated JSON and constructed `EvaluationBoundary` successfully with that pin. Status remains `POLICY_FROZEN_RESERVATION_PENDING`; selection time is null, windows/entities/materialized protected IDs are empty, and all four dependency durations remain null. No windows, dates, thresholds or protected membership were invented. Fitting remains blocked.

## Exact file SHA256 at verification

```text
ef20358df2773108a6bd9eceba24892ef06ad8983d0af1809037aeeeed19a27c  tools/data-pipeline/src/north_star/splits.py
9b4623449609aec098cdeaa85371d1618fc25d53429f84a35c9e7f2219312f93  tools/data-pipeline/src/north_star/contracts.py
3f89889ce5f578f3a7739ee57d69bac5038047d041d2d7497d31198a54bafe48  tools/data-pipeline/tests/north_star/test_splits.py
6debde4447bb6369703d7c854b1ee4490503f02f4684cdf84e2b46cbf28467b3  tools/data-pipeline/tests/north_star/test_config_contract.py
96a85423d1e7dd6d3304f1d492f42f81c4b417afe690bdcdc0910821721b16c3  tools/data-pipeline/north_star/EVAL_PROTOCOL.md
f74b812632c2424c151af0f06aa96c5d2a847810a31a0c28172994854420fabc  tools/data-pipeline/north_star/EVAL_FREEZE.json
3c1c9ef73710a1c3531fa36ba8f62a43e2cf75f8e27d72a250ed562c1ec26020  tools/data-pipeline/north_star/OPERATING_CONTRACT.md
```

This receipt is excluded from its own hash list. The JSON byte hash and canonical policy hash intentionally differ.

## Policy v2 metadata correction — parent verification

The prior v1/hash table above is historical. Current policy is v2, canonical pin `2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa`. It adds actual historical Twitch VOD, current stream and LaserStream session identifiers as exposed DEVELOPMENT. The separate master-listed YouTube source remains exposed; no protected window or fitting authorization was added. Parent constructed EvaluationBoundary with the recomputed pin and reran the full suite: 491 passed. This completes verification of edits left by the credit-interrupted subagent; no unchecked success is inherited. Current JSON SHA256: `309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3`.

## Remaining gates

Formal outcome-blind pre-collection reservation and custody; exact future source/time bounds and development/challenge memberships; exhaustive exposure and model/data ancestry audit; dependency materialization and overlap counts; producer/loader integration; approved dependency and operating/economic limits; actual referenced artifact verification; independent review. No admitted source rows, model-fit results, economic performance or profitability are claimed.
