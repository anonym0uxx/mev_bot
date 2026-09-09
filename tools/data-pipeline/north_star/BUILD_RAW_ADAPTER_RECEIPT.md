# Stage 2 raw-projection adapter — development receipt

Status: **implemented and bounded-source exercised; NOT source admission, full consumer canonicalization, or Stage 2 closure.** Independent review remains required. No training/fitting/export build, collector action, commit, or original-repository modification was performed.

## Owned changes

- `src/north_star/raw_adapter.py`
- `tests/north_star/test_raw_adapter.py`
- `north_star/BUILD_RAW_ADAPTER_RECEIPT.md`

Other concurrent workers' files were not edited. This module is a standalone development API, not wired into any corpus admission/export path.

## Producer schema and projection contract

Read original `D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/src/raw_recorder.rs` and the discriminator call sites in `capture.rs`.

- `RawRecord` fields: `record_type`, `slot`, `recv_unix_ms`, `record_index`, `payload`. The recorder generates host receive time while writing; it is not on-chain block time. Index is session-global, not necessarily part-local.
- Exact discriminator is `block_meta` (not `blockmeta`). Transaction, account, slot and block metadata remain distinct.
- Producer transaction `raw_hash` hashes signature bytes only (raw_recorder.rs:331–336). Account `raw_hash` hashes account data only (:349–365). Neither is an exact record-content hash. Preserve these only as `source_payload_raw_hash`; compute `raw_record_sha256` over the original decompressed NDJSON bytes **including line terminator**. `source_sha256` hashes the complete compressed object.
- Full primary signature and complete source signatures list are preserved; account transaction signatures are separate from account `write_version`. Revision zero means initial adapter projection, NOT account write version or a chain-finality revision. Explicit caller revisions are labeled as such.
- Resolve account index space by exact ordered concatenation: message static keys, metadata loaded writable addresses, metadata loaded readonly addresses. Preserve each group and duplicates; never sort or deduplicate keys.
- No heuristic mint/trader, economic action, available-at claim, or finality inference. Source commitment is an optional literal provenance assertion. Raw slot status remains separate, even when it is `Finalized`. Block time is copied only from block metadata and remains null for other types.
- Unknown record/status, malformed envelope/projected fields, missing transaction message/meta, duplicate JSON keys, invalid UTF-8/JSON, nonfinite numbers, slot mismatch, bad account base64/length, and invalid provenance context fail closed with stable reasons. Unsupported non-envelope payload internals are neither decoded nor certified. Literal key/signature strings are preserved, not cryptographically validated or repaired.
- Every projection is permanently marked `projection_scope=envelope_only_development`, `training_eligible=false`, `source_admission=unadmitted`. Parser success cannot grant rights or protected-evaluation eligibility.

API: `project_record(raw_bytes, source_path=..., source_sha256=..., revision=0, source_commitment=None)`; `project_bounded_file(source_path, output_dir=NEW_DIRECTORY, limit=100, source_commitment=None)`. File wrapper enforces 1–100 rows and an 8 MiB per-record cap, refuses outputs overlapping the source directory or existing outputs, hashes source before/after, and writes the success report last. It does not establish whole-source coverage or zstd-tail integrity; the prefix reader can prefetch compressed blocks.

## Actual TDD / test evidence

Executed before implementation changes, in order:

1. First slot-envelope test: **1 failed**, adapter absent.
2. Minimal slot implementation: **1 passed**.
3. Added record types, malformed cases, identity and bounded-I/O requirements: **39 failed, 1 passed**.
4. Implemented projection and bounded reader: **40 passed**.
5. Added observed uppercase manifest commitment preservation and exponent-overflow JSON regression: **2 failed, 40 passed**.
6. Implemented literal uppercase commitment support and finite-float parse guard: **42 passed**.

Latest retained targeted execution: `python -m pytest tests/north_star/test_raw_adapter.py -q` → **42 passed in 0.08s**.

Broader concurrent-worktree check is not all green: latest `python -m pytest tests/north_star -q --tb=short` → **2 failed, 631 passed in 4.54s**. Both failures are outside this ownership in `test_jobs.py`: `test_exact_completed_resume_verifies_logs_without_launch`, `test_launch_error_has_sanitized_exit_and_no_retry`. Earlier broad execution caught another worker's actions module during its RED phase (8 failed, 580 passed). No unrelated fixes made.

One log-capture subprocess used bare `python` and resolved to the system interpreter without pytest. That invocation failed honestly and its logs are retained. Repeated using `sys.executable` from the working Hermes venv; verified logs below are the valid results. No provider/network calls were required.

## Bounded real-source result

Exact preserved source:

`D:/mev_bot-artifacts/north_star/capture/20260909_144906_000490/pumpfun_laserstream_raw_v1_20260909_144906_000490_part0000.ndjson.zst`

- Compressed size: **40,872,244 bytes**.
- Before/after SHA256: `85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f` (equal; size and mtime unchanged).
- Read only first **100** decoded lines, indices 0–99: **54 transaction, 42 account, 4 slot**; no block-meta rows in this prefix. Block-meta behavior has synthetic producer-shaped tests only.
- **98 projected, 2 rejected**. Raw indices 2 and 3 contain literal slot status `Unknown`, which the producer maps from unrecognized SDK enum values. Rejection reason `unknown_slot_status`; not silently promoted to processed/confirmed/finalized.
- **13** of 54 transactions contain loaded addresses; all accepted records were independently reread against their exact line hashes, source indices and receive timestamps. All transaction full signatures and concatenated key arrays matched the raw source.
- Source commitment literal `CONFIRMED` came from preserved manifest `pumpfun_laserstream_manifest_v1_20260909_144906_000490.json`, SHA256 `15e1c691b49b03fde0f4fdac4c613fc9941ada2e972b23f360109ce4376461e3`. This is subscription provenance, NOT independently verified transaction finality or admission.
- `limit_reached=true`, `eof_observed=false`, `full_source_decoded=false`. No whole-session inference.

## Artifact handles

New development-only directory:

`D:/mev_bot-artifacts/north_star/development/raw_adapter/part0000_first100_v1/`

- `events.jsonl`: 98 projected envelopes; SHA256 `87d8f3210a9e392add7da328feff6b5ad6b20d7c85cd4a02d40f6b54e030586e`.
- `report.json`: exact source, code/output hashes, counts, commitment, exclusions, failure line indices/reasons/hashes; SHA256 `429d7741aba37e07f2741cd3539f895b67e460b7e4903daf114633517c74ba16`.
- `targeted_tests_verified.txt`, `north_star_regression_verified.txt`, `verification_tests_verified.json`: actual rerun outputs with working interpreter.
- Earlier `targeted_tests.txt`, `north_star_regression.txt`, `verification_tests.json` record the failed interpreter-discovery attempt; they are NOT successful test evidence.

Adapter code hash at projection: `4ea2bcfca2e2056a1f2b2ec6f531b958eff9d39e82156845cd3fe3bdbb31460e`.

Source rights, inclusion approval, protected ancestry, semantic decoding, append-only correction orchestration, whole-session coverage and independent Stage 2 certification remain open. All upstream data stays unadmitted.

## Independent-review fixes — v2 (supersedes latest-test claims above)

Confirmed the **complete formal must-fix list contains one item**: oversized JSON integers escape `RawProjectionError`, aborting a bounded batch rather than recording a stable rejection. The live review `deleg_4a462dca/task-0.log` truncates its final response inline; the full response was recovered read-only from Hermes `state.db`, message **149824**, session `20260909_104935_a97680`. No additional formal must-fix was hidden in that response. The same review log independently observes an accepted unpaired surrogate; the follow-up task explicitly also requires fixing this Unicode case.

Implemented only in the owned adapter/tests:

- Parse every integer token with a bounded conversion hook, rejecting values outside the recorder-compatible integer domain `[-2**63, 2**64 - 1]` as `RawProjectionError("json_integer_out_of_range")`, including nested, unprojected values. Check token length before conversion so 5000-digit tokens never depend on Python's configurable integer-conversion limit. Existing unsigned projected fields and signed block-time constraints remain narrower where appropriate.
- Validate decoded string values and object keys throughout the JSON tree, including opaque nested fields; reject any remaining surrogate code points as `invalid_unicode_scalar`. Validate caller source-path text too. Valid paired JSON escapes decode to their actual scalar; non-BMP characters and combining sequences remain unchanged. No normalization, replacement or cryptographic key validation is introduced.
- Preserve `duplicate_json_key` and `nonfinite_json_number` exactly; no broad `ValueError` catch masks their reasons. Existing field-specific validation reasons remain in force for values within the generic integer domain.
- Both malformed-row classes now produce exact rejection line hashes and continue a bounded synthetic zstd batch, leaving subsequent valid rows unadmitted. UTF-8 serialization of valid projected Unicode is explicitly tested. The existing file wrapper uses ASCII JSON escapes, so the old surrogate case was accepted there rather than immediately crashing; downstream `ensure_ascii=False` UTF-8 output was the latent hazard.

### Strict RED → GREEN evidence

Command for each targeted run, in `tools/data-pipeline`:

`PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star/test_raw_adapter.py -q -p no:cacheprovider --tb=short`

1. Untouched baseline: **42 passed in 0.09s**.
2. Added integer and batch regressions before changing implementation: **9 failed, 57 passed in 0.31s**. Failures included actual Python digit-limit `ValueError`, wrong field-only rejection reason, silently accepted nested out-of-range values, and aborted batch.
3. Integer hook implemented: **66 passed in 0.25s**.
4. Added Unicode regressions before Unicode implementation: **18 failed, 71 passed in 0.41s**. Surrogates were accepted across keys/values/path and the batch incorrectly accepted both rows.
5. Unicode validation implemented: **89 passed in 0.28s**.
6. Final retained targeted run: **89 passed in 0.24s**. Retained broad run (`tests/north_star` with the same flags): **693 passed in 4.78s**. Prior transient jobs failures above are historical, not this run's result. Concurrent-worker code remains uncommitted and these checks do not certify Stage 2.

### Fresh bounded source evidence, old artifacts preserved

Rejection semantics changed, so reran **only the same first 100 decoded source rows**, using the exact source-path literal and `CONFIRMED` commitment from the preserved v1 report. Created only the new development directory:

`D:/mev_bot-artifacts/north_star/development/raw_adapter/part0000_first100_v2/`

- **98 accepted, 2 rejected**; accepted types: **54 transaction, 42 account, 2 slot**. Rejections remain source lines **2 and 3**, `unknown_slot_status`, with identical line hashes.
- Source before/after SHA256 remains `85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f`; size **40,872,244 bytes**, size/mtime checks passed.
- New adapter SHA256: `6f11413dc4be53bac1e776485c9b1800879a2d10bc0d6d52e43bba01c14f3ec6`.
- New `report.json` SHA256: `f03f3165b553678091b569dda6daa26bbbdc4060ad4d6a1ad87f3aed6b693a21`.
- `events.jsonl` SHA256: `87d8f3210a9e392add7da328feff6b5ad6b20d7c85cd4a02d40f6b54e030586e`; verified **byte-identical to v1**. The real sample contains neither newly rejected malformed case.
- Read the written v2 report back and compared it with the actual returned report; checked event count, event hash, exclusions, failure list and source hashes. Hashed all **8 existing v1 files** before/after and verified all unchanged, including old report SHA256 `429d7741aba37e07f2741cd3539f895b67e460b7e4903daf114633517c74ba16`.
- Retained actual final outputs in v2: `targeted_tests_verified.txt`, `north_star_regression_verified.txt`, `verification_tests_verified.json`.

Limitations unchanged: envelope-only development projection, `training_eligible=false`, `source_admission=unadmitted`; no semantic economics, rights/ancestry admission, training/export, whole-source decode, tail integrity, new collector action or commit. No real block-meta row is in this prefix. Commitment provenance was reused from v1, not newly certified. Only the three owned repository files were edited; unrelated workers' files were not changed.
