# S0 requirements registry and source-admission receipt

## Delivered scope

Only these delegated paths under `tools/data-pipeline` were created/modified by this worker:
- `src/north_star/requirements.py`
- `src/north_star/admission.py`
- `tests/north_star/test_requirements_coverage.py`
- `tests/north_star/test_source_admission.py`
- `north_star/REQUIREMENTS_MATRIX.json`
- `north_star/BUILD_S0_RECEIPT.md`

No commits, shared `__init__.py` edits, collector edits, gold reads, corpus writes, fitting, training or live orders. Worktree: `D:/repos/mev_bot-north-star`. Original repository was read for the approved plan only.

Read supplied master V5, amendment V6 and approved plan. Matrix retains their SHA256 hashes, exact cited excerpts and line ranges. It includes 26 requirements (D01-D14/G01-G12), 9 stages (0-8), 8 corpus-object contracts and 6 source classes. Counts were computed from the written JSON. Requirement states: 25 MISSING_SOURCE, 1 UNSUPPORTED (D14). Every measured-support count is null, not fabricated zero. Historical PRESENT claims from the plan are separately qualified, not recertified. All source-backed acceptance remains NOT_RUN.

This receipt certifies registry/policy unit tests, not completion of the full S0 inventory or dataset admission. D14 and the Rust bridge remain engineering-only: Qwen trader-only, Astra Rust, mandatory action/runtime conformance retained. Narrative, Kelly/net-returned-SOL, genuine V6 class support and label-token audits are explicit; stricter proposed class floors remain unresolved rather than silently weakened.

## Strict TDD execution evidence

1. Before `requirements.py` existed, `python -m pytest tests/north_star/test_requirements_coverage.py -q` returned exit 1: **1 failed in 0.04s**, assertion `requirements validator is missing`. Implemented ID validation only; rerun: **1 passed in 0.01s**.
2. Wrote registry/admission tests before registry validation/matrix and `admission.py` existed. `python -m pytest tests/north_star/test_requirements_coverage.py tests/north_star/test_source_admission.py -q --tb=short` returned exit 1 for missing implementations. Implemented those contracts; rerun: **64 passed in 0.10s**.
3. Added false VERIFIED producer/output and false acceptance PASS tests before guards: **3 failed, 26 passed in 0.12s**, all `DID NOT RAISE ValueError`. Added evidence guards; combined targeted suite: **67 passed in 0.11s**.
4. Added malformed nested/top-level admission tests before input-shape guards: **6 failed, 39 passed in 0.09s**, AttributeError on list/string/None. Added fail-closed guards; final runs below.

## Final real execution

Terminal interpreter:
`C:\Users\Alon\AppData\Local\hermes\hermes-agent\venv\Scripts\python.exe`

Command:
`python -m pytest tests/north_star/test_requirements_coverage.py tests/north_star/test_source_admission.py -q --tb=short`

```text
........................................................................ [ 97%]
..                                                                       [100%]
74 passed in 0.11s
```

Integration snapshot (additional modules owned by other agents):
`python -m pytest tests/north_star -q --tb=short`

```text
........................................................................ [ 31%]
........................................................................ [ 63%]
........................................................................ [ 94%]
............                                                             [100%]
228 passed in 0.54s
```

`git -C D:/repos/mev_bot-north-star diff --check`: exit 0, no output. Untracked files are not covered by git diff; Python/JSON writes also reported syntax/lint checks OK.

An attempted execution-kernel subprocess used a different bare Python without pytest (`No module named pytest`). No result was substituted: reran successfully through the terminal's actual venv, as recorded above.

## Independent-review remediation (requirements/admission only)

Read `deleg_2ec9a44f/task-0.log` and reproduced the concrete bypasses before changing production code. This follow-up changed only `src/north_star/requirements.py`, `src/north_star/admission.py`, their two test files, and this receipt. It did not modify the requirements matrix, collectors, other modules, real data or commit anything.

- ADMITTED_WITH_SUPPORT now requires VERIFIED producer/output with production evidence, explicit independent acceptance PASS, a complete positive measured-support vector (including independent sessions/useful label tokens), and coherent raw/canonical/admitted counts. All six admission-chain receipt kinds must have production scope.
- Acceptance/measurement references must be nonempty lists of literal paths resolving unambiguously to local hash-checked receipts of the correct kind. Support receipts must declare production scope and independence; acceptance receipts additionally declare result PASS. Fixture-scoped counts are rejected even outside admitted states. Stage acceptance states are checked and PASS cannot be unsupported, missing or receipt-free.
- Admission validates scoped approval maps, license rights maps, registry types and parent-record maps. Malformed source identity is blocked before set membership. Intended tasks must be a nonempty list of nonblank strings. Every denial includes status BLOCKED and reasons instead of the reproduced AttributeError/TypeError; ADMISSIBLE denotes only the existing inclusion gate, not operational authorization.

Actual TDD runs (terminal venv, bytecode/plugins/pytest cache disabled):

1. Deep malformed-contract regressions: **25 failed, 45 deselected in 0.13s** (nested AttributeError/TypeError and missing explicit status). After guards: **70 passed in 0.04s**.
2. Intended-task regressions: **7 failed, 1 passed, 70 deselected in 0.06s** (invalid tasks returned ADMISSIBLE). After list/item validation: **78 passed in 0.05s**.
3. Coherent admitted-state regressions: **9 failed, 3 passed, 29 deselected in 0.28s**. Expanded evidence/stage regressions before implementation: **25 failed, 33 passed in 0.57s**, all missing expected ValueError. After implementation the combined scoped suite returned **136 passed in 0.50s**.
4. Fixture-only producer/output regressions: **2 failed, 58 deselected in 0.17s**. After production guards, final scoped command:

```text
PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python -B -m pytest -p no:cacheprovider tests/north_star/test_requirements_coverage.py tests/north_star/test_source_admission.py -q --tb=short
138 passed in 0.58s
```

`git diff --check` exited 0 (tracked files only). Concurrent integration snapshot using the same flags on `tests/north_star`: **4 failed, 482 passed in 1.66s**. All four failures were outside this worker's scope in `test_splits.py`: missing pipeline-local EVAL_FREEZE.json/EVAL_PROTOCOL.md and list/dict stage TypeError. They were not edited here; splits work was still owned by another agent.

Read-only matrix verification: **26 requirements: 25 MISSING_SOURCE, 1 UNSUPPORTED; every support value null; 26 requirement and 9 stage acceptance states NOT_RUN**. No coverage gaps were closed. Test receipts live only in pytest temporary directories and simulate declarations; they are not production support. Scope/independence/result metadata and byte hashes do not authenticate the author or recompute corpus counts: an authenticated evidence producer and independent source-level QA remain required.

## Admission and remaining limitations

Independent gates cover collection access, local transformation, permissive rights, technical eligibility, source/license/token disclosure, training inclusion, training-run authorization, live-order authorization and candidate-loader release. Collection does not imply training. Literal permissive allowlist only; unknown/restrictive/training-only grants, missing proof, revocation/conflicts, unmet attribution, stale reports, missing scoped approvals, generated/uncertain origin and missing/restricted/cyclic parents reject inclusion. Fixtures are explicitly test-only, never teaching/training records.

Evidence IDs/URIs remain declarative pointers requiring an upstream authenticated evidence store. The pure admission validator does not contact licensors or establish legal authenticity. Parent source versions, independent corpus acceptance and downstream source-specific validation remain necessary. Registry receipt hashes prove bytes, not independent truth. Governing document hashes/excerpts are checked against originals when locally available; portable review retains pinned excerpts. This software does not implement actual source inventory, source acquisition, release scheduling, training launch or order execution.
