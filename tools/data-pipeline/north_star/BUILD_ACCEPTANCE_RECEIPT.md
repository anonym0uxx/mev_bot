# All-stage build acceptance audit — Astra handoff

## Executed result: BLOCKED, not dataset completion

Final audit directory:
`D:/mev_bot-artifacts/north_star/development/build_acceptance/receipt_audit_v1_20260909_b`

- 26 mandatory requirements enumerated from `REQUIREMENTS_MATRIX.json`; 26 blocked.
- 9 master stages (0–8) evaluated; 9 blocked.
- 18 explicit tracker receipt files hash-verified as **supporting metadata only**.
- No supplied complete typed source/corpus acceptance chain. Existing component receipts, prose, tracker completion assertions and regression counts do not close corpus gates.
- `corpus_population: null`: this audit did not measure actual corpus population. This is **not** a claim that actual source/corpus population is zero.
- `training_run_authorized: false`, including on a hypothetical receipt-chain PASS. No model training, source-data scan, network request or admission was performed.

Output files: `evidence_registry.json`, `audit.json`, `artifact_manifest.json`.
The final files were reopened and both manifest hashes independently checked:

| Artifact | SHA-256 |
|---|---|
| evidence_registry.json | `18c81b6efda7d9a9399476da4f8d8f86b777ac3c410eb31cd6d2674e5c4fe77a` |
| audit.json | `a519ab3ce96e7537071dc8530ea82d57c6f3ad027c5ac1568fb33d2625eb61aa` |

The earlier `_a` directory is preserved; `_b` is the final rerun. Both contain real metadata audits, not fabricated corpus receipts.

## Invocation from repository root

```bash
PYTHONPATH=tools/data-pipeline/src python -m north_star.build_acceptance \
  --matrix tools/data-pipeline/north_star/REQUIREMENTS_MATRIX.json \
  --tracker tools/data-pipeline/north_star/BUILD_STAGE_TRACKER.json \
  --evidence-root tools/data-pipeline/north_star \
  --artifact-dir D:/mev_bot-artifacts/north_star/development/build_acceptance/NEW_UNIQUE_RUN
```

Exit code `0` means registered receipt-chain PASS; `2` means BLOCKED/error.
Stdout is a single JSON result. Destinations must not already exist. The runner never overwrites the master, matrix, tracker, input registry or existing receipts.
Optional `--registry PATH` supplies a typed registry instead of importing the tracker's explicit `existing_receipts`. There is no directory discovery or recursive source traversal. References contained inside receipts are never followed.

## Registry contract

Root fields:

- `schema_version: north_star_build_evidence_v1`
- nonempty `registry_version`, `build_id`
- `corpus_id` for all data-acceptance evidence
- `artifacts`: explicit local file entries

Each typed artifact requires `artifact_id`, `artifact_version`, `build_id`, `requirement_id`, `stage_ids`, `scope`, `evidence_type`, `evidence_category`, `semantics`, `producer_id`, `reviewer_id`, `result`, `path`, and literal lowercase SHA-256 `sha256`.

Typed acceptance files are JSON with `schema_version: north_star_evidence_receipt_v1`. All identity/scope/category/semantics/result fields above must match the hashed receipt; corpus evidence additionally binds `corpus_id` in receipt, entry and registry. `path` is relative to the supplied local root; parent escapes, absolute/drive paths, alternate-stream syntax and resolved links outside the root are rejected. No token normalization repairs malformed identifiers or hashes.

`requirement_id` must be a matrix requirement, with matching scope and only its declared stages. Whole-stage receipts use `requirement_id: null`, `scope: whole_build`, and `evidence_type: stage_acceptance`.

Categories and semantics remain distinct:

- `software_component_review`: engineering or synthetic checks can establish bounded component review, never data acceptance.
- `data_acceptance`: only `source` or `corpus` semantics; source-only evidence is allowed for producer/raw-manifest/source-admission, not downstream corpus gates.
- `supporting_metadata`: `metadata_only`, `existing_receipt`, `UNASSESSED`; hash-verifies arbitrary existing receipt bytes without interpreting prose as acceptance.

For each mandatory requirement at each matrix-listed stage, data acceptance requires these evidence types: `producer`, `output`, `raw_manifest`, `canonical_manifest`, `admitted_examples`, `source_admission`, `independent_acceptance`, `measured_support`. These are metadata receipts, not instructions to load underlying raw files. All must record PASS and bind the same build/corpus.

Independent acceptance and whole-stage acceptance require distinct nonempty producer/reviewer identities. D14 remains mandatory under the matrix's engineering-only override: `runtime_conformance` plus independent acceptance; no Qwen Rust SFT admission. All D01–D14/G01–G12 membership and stage assignment are read from the matrix, with the existing requirements module's exact-ID integrity guard preventing omissions/duplicates. No numerical-only policy downgrade is allowed.

Stage acceptance requires its own complete scoped evidence and independent whole-stage receipt, plus every earlier master stage passing. A requirement's later-stage missing evidence does not retroactively block an otherwise complete earlier stage. This conservative *acceptance* ordering does not prohibit safe parallel software construction or capture.

## Bounds and failure behavior

Defaults: 2 MiB/file, 32 MiB evidence-verification budget, 2,048 artifact entries. Tracker-import hashing has its own 32 MiB budget; governing JSON/master inputs are independently bounded at 2 MiB each. Failed reads/hash/JSON checks conservatively consume their reserved allowance, preventing repeated invalid inputs from escaping the budget. Duplicate JSON keys and nonfinite JSON values are rejected.

Missing, pending, malformed, hash-mismatched, wrong-build/corpus, duplicate-ID and out-of-scope evidence produce BLOCKED with reasons. Missing measurements remain unknown, never inferred zeros. The JSON report separates requirement software review from data acceptance and provides per-stage dependency blockers and input hashes.

**Trust boundary:** this certifies deterministic consistency/completeness of supplied registered declarations and local bytes. It does not authenticate reviewer identities, establish license/semantic truth, independently count corpus membership, reproduce the underlying acceptance checks, or prove that a receipt falsely labeled corpus is genuine. Actual independent source/corpus acceptance remains required upstream; do not interpret receipt-chain PASS as automatic production admission or training authorization.

## Verification and owned files

Strict RED/GREEN cycles were executed before implementation: missing runner, typed verification/gates, CLI/import behavior, and earlier-stage independence each had observed failing tests before their implementation/fix.

From repository root:

```bash
python -m pytest tools/data-pipeline/tests/north_star/test_build_acceptance.py -q
# 25 passed in 1.82s
python -m pytest tools/data-pipeline/tests/north_star -q
# 2224 passed in 16.40s
```

These are engineering test results, not corpus admission evidence. Tests include synthetic-vs-corpus separation, missing requirement, missing matrix ID, duplicate requirement/artifact IDs, hash mismatch, scope/stage/corpus mismatch, pending receipts, file/budget bounds, narrative omission, dependency gating, duplicate/nonfinite JSON, immutable tracker import, CLI BLOCKED exit and a deliberately declared complete fixture chain. The last is a schema/logic fixture only, never actual build evidence.

Repository edits are limited to:
- `src/north_star/build_acceptance.py`
- `tests/north_star/test_build_acceptance.py`
- `north_star/BUILD_ACCEPTANCE_RECEIPT.md`

No shared files or conftest were changed; no commits were made. Use canonical `north_star` imports; the subprocess CLI test explicitly supplies `PYTHONPATH`, so repository-root tests do not depend on the caller's working directory.
