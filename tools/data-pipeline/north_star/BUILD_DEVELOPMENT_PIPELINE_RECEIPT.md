# Bounded integrated development pipeline receipt

## Outcome and authority

**Real bounded integration completed and independently read back. Unadmitted development only; no training, export, fitting, source admission, new exposure, or master-stage closure.**

New first-available artifact directory:
`D:/mev_bot-artifacts/north_star/development/integrated/first100raw_10tx_50existing_v1/`

| Scoped operation | Actual result |
|---|---|
| Raw envelope projection | First 100 raw rows: 98 accepted, 2 rejected |
| Parquet storage | 98 input envelopes -> 98 output rows; all columns independently equal |
| Transaction endpoint arithmetic | First 10 attempts, stopping at raw line 23: 9 reconciled, 1 rejected |
| Endpoint rows, not trades | 69 token endpoint rows; 305 native endpoint rows |
| Canonical transaction identity | All 9 reconciled transactions quarantined; 0 admissions |
| Existing recovered source input | 50 existing rows referenced; 50 exact byte spans verified; 0 fresh recovery transformations |
| Source/context and mint joins | Null; no verified clock-plus-mint proof; never paired by count |

`source_admission=UNADMITTED`, `training_eligible=false`, `fit_authorized=false`, `export_authorized=false`. Native amounts remain lamports; token movements remain integer raw units with decimals. Recorder Unix milliseconds are not promoted to verified source availability. Existing legacy timestamps, admission labels and span metadata confer no new authority.

## Exact inputs and exposure gate

Raw input:
`D:/mev_bot-artifacts/north_star/capture/20260909_144906_000490/pumpfun_laserstream_raw_v1_20260909_144906_000490_part0000.ndjson.zst`

- Bytes: `40872244`.
- SHA256: `85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f`.
- Literal exposed identity: `urn:laserstream:session:20260909_144906_000490`.
- Identity is **not inferred merely from the folder name**. The adjacent preserved `pumpfun_laserstream_manifest_v1_20260909_144906_000490.json`, SHA256 `15e1c691b49b03fde0f4fdac4c613fc9941ada2e972b23f360109ce4376461e3`, contains that exact `session_id` and exactly one matching filename/size/SHA entry. The driver verifies this binding before decoding. This particular input is not the recovered August historical raw. Historical or alternative raw paths cannot borrow this source identity.
- Manifest identity is local acquisition provenance, not provider/chain identity or rights certification.

Existing recovery input:
`D:/mev_bot-artifacts/north_star/development/narrative_sources/first50_exact_v1/manifest.json`

- Manifest SHA256: `65af14eb5a4707b7348fff0a81fe09728385e89a1ea056faef3dba8eb3287040`.
- Recovered JSONL SHA256: `f19c2e8b2f73facbeb3ff4bb0282656a7f22cc2d9e5149cc2135f341709f9c16`.
- Exact exposed **dataset** identity: `tools/data-pipeline/output/narrative_gold_v1.1/gold/FREEZE_MARKER.json`. This is not a claim that every upstream provider/entity is verified or admitted.
- All 151 manifest-listed files checked for exact hashes and sizes, before/after execution and separately during independent readback. All 50 spans checked against their exact retained original claim-record `claim_text` UTF-8 bytes and matching content IDs.
- `narrative_sources` is never imported or called. Independent artifact review cleared this immutable sample; adapter code review has separate must-fixes owned by another worker. This run does not rely on that code or its repair and never opens the original legacy corpus.

Pinned policy files remain unchanged:

| Policy | Exact file SHA256 | Canonical digest |
|---|---|---|
| EVAL_FREEZE.json v2 | `309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3` | `2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa` |
| EVAL_RESERVATION_V3.json | `a78b6efc2df16a69e52dc7fe33728fba2a2768572d62bc05cde978e375cbcb82` | `8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8` |

Both externally pinned file and canonical digests are checked through the existing boundary/reservation classes. Exact source IDs must be explicitly DEVELOPMENT and exposed, and the raw/recovery path+hash pair must match the fixed allowlist. Unknown sources, alternate registered-but-out-of-scope sources, historical path substitutions and whitespace aliases refuse. No policy expansion. The prospective September 16–23 PT reservation remains protected, collection/fit unauthorized; exposed data routes QUARANTINED under its reservation API. Observed raw clocks conflicting with the reserved window block completion; absent narrative clocks stay unknown.

## Composition and completion protocol

API:

```python
from north_star.development_pipeline import run_development
result = run_development(NEW_OUTPUT_DIRECTORY)
assert run_development(NEW_OUTPUT_DIRECTORY, resume=True) == result
```

Composes existing reviewed helpers:
1. `raw_adapter.project_bounded_file(..., limit=100)`.
2. `partitions.write_partition(..., max_rows=100, batch_rows=32, max_line_bytes=8388608, compression="zstd")`.
3. `transaction_truth.sample_bounded_file(..., max_transactions=10)`.
4. References verified existing recovery manifest and files unchanged.

Transaction truth deliberately reads the original bounded raw prefix, **not the envelope-only Parquet**, which does not carry balance payloads. This required second prefix read is disclosed, not hidden behind a false serial semantic transform. Existing helper IO/Parquet validation is reused rather than reimplemented. Source commitment defaults to null: no finality/commitment evidence is manufactured. Consequently raw output path spelling/commitment and Parquet hashes differ from earlier standalone projections; transaction outputs are byte-identical to the preserved sample.

`identity.json` pins exact config and canonical config hash, nine local code-file hashes, input file hashes/sizes, acquisition manifest evidence, both policies, output target and Python/PyArrow/zstandard/base58 versions. `manifest.json` unifies per-transform conservation, independently scoped states, null joins, and all eight produced child/identity file hashes/sizes. `COMPLETE.json`, pinning the exact manifest bytes and identity digest, is published **last**, no-clobber, and read back.

**Resume limitation (corrected after independent P2 review):** only exact completed-run verification is supported. No partial-stage resume is claimed: raw/transaction helpers are new-only and cannot honestly support safe continuation. Any interrupted run, including manifest-without-completion, is retained and refuses resume; use a new target. The original implementation did NOT guarantee nonmutation for malformed completed manifests: omitting both partition files and their inventory entries, then recomputing COMPLETE's manifest hash/size, reached the shared writer through `_summarize`, recreated those files, and only then refused. The original successful exact resume was real but did not prove the broader verification-only claim. The fix below checks the exact eight-file inventory and strict metadata before summarization and uses only read-only partition validators. Changed config/code/input, missing child output, corrupted output, or orphan completion refuses; the driver never deletes sources or partial artifacts.

Trusted caller-controlled local roots only. Existing fs_integrity/child-helper limitations remain: no hostile concurrent mutation/ABA sandbox, no signed custody, no process-isolation memory ceiling, no power-loss durability guarantee. Hashes establish local byte equality, not independent original/provider custody. Per-file ceiling 128 MiB; raw bounded record ceiling 8 MiB; compressed blocks may prefetch. Full compressed file hashes are streamed, but the full source is not decoded and tail/session coverage is not certified. One sequential worker, no scale-up.

## Historical build TDD and validation (before P2 repair)

Observed runs, not inferred:
- Initial missing-driver assertion: 1 failed -> 1 passed for pinned exposure gate.
- Added integration contract: 2 passed / 16 setup errors because required API was absent; implementation then 8 failed / 10 passed due to an incorrectly assumed package `__init__.py`; removed the nonexistent namespace-package file from the explicit code list -> 18 passed.
- Added completion/orphan/real-helper transaction and reserved-clock regressions: 1 failed / 25 passed (reserved clock accepted) -> 26 passed after reserved-window check.
- Added acquisition-manifest source-binding regression: 1 failed / 26 deselected -> 27 passed after exact source-manifest binding.
- Original focused driver suite: **27 passed in 2.32s**.
- Driver + raw + partitions + transaction truth + splits + reservation: **678 passed in 4.58s**.
- Full concurrent-worktree North Star suite: **1713 passed in 11.67s**. This includes other workers' current files, not independent admission review of them.

Commands from `D:/repos/mev_bot-north-star/tools/data-pipeline`:
`python -B -m pytest tests/north_star/test_development_pipeline.py -q -p no:cacheprovider --tb=short`
and `python -B -m pytest tests/north_star -q -p no:cacheprovider --tb=short`.

Interruption injection covered after raw, partition, transactions and manifest, and failure publishing the final marker. Other tests cover strict fixed 100/10/50 bounds, bool/extra config rejection, no-clobber, exact completed resume, missing files, input mutation during run, corruption, unknown paths/sources, policy-byte edits, runtime code-identity mismatch and two config mismatch cases. Synthetic tests inject only a synthetic private allowlist/binding; no production bypass option is exposed.

Original real run elapsed: `0.6805252000049222` seconds, followed by successful exact completed resume under the original code. After the P2 fix this preserved artifact must refuse code mismatch, not be repinned. Preflight available RAM `250815918080` bytes; D: free `351525232640` bytes. These are bounded-run observations, not scale estimates.

Separate readback (outside driver): all 8 listed produced files plus manifest/completion verified; 151 recovery files plus recovery manifest verified; raw input hash/size unchanged; all 98 Parquet rows/all columns equal JSONL; 9+1 transaction conservation, 69/305 nested endpoints, 9 identity quarantines, 50 immutable recovered rows and 50 exact spans verified. No genuine training output is claimed.

## Original artifact output pins (preserved, not repinned)

- Original artifact's driver code SHA256: `ba45647d5cfc6bae2774642dd0d8d9d62d5e7fefdc82311acaeddd7f3d234f48`.
- Identity canonical SHA256: `652bf452a8d5937f729c0316284b32c1f64c0a000d268def1997a192eef19514`.
- `manifest.json`: 41263 bytes, `dce8f16f716597ebc42f9f63f9be4bc44e5f2044ad645568dde5570a7baba441`.
- `COMPLETE.json`: 214 bytes, `40272acb3aacf7b03596118c8f7cfa7d20741c3a4090f4b98203daac35508216`.
- `events.parquet`: 136643 bytes, `b332354382720a1e4bd5a77c9f507d1e5643abd9a8d75223f7854ad332c61bf5`.
- `raw/events.jsonl`: 353853 bytes, `c0763b3f215eee38011ba9e97125149a3429de77754cb107959765f355b91161`.
- `transactions/reconciled.ndjson`: 135483 bytes, `2e4ff8237cce0adf3f52e09c39d10fb2abf23a7c99ddfe99d5bedbda3faaff0b`.
- `transactions/rejected.ndjson`: 534 bytes, `ee65a263495ea95f26967dbfca2f9f486b1a4bc0af7b58520d55a084dfa8e8de`.
- Remaining exact child-file and code hashes are in the unified manifest.

Only owned repository files created/modified: `src/north_star/development_pipeline.py`, `tests/north_star/test_development_pipeline.py`, and this receipt. No commits, collectors, network, fitting, trading, source deletion or original-worktree writes. No sibling modules or policies edited.

## Independent P2 repair: actual RED–GREEN and preservation evidence

Independent original artifact review passed 98 rows, 9+1 transactions and 50 spans with all hashes unchanged, no admission. Its separate malformed-resume finding was reproducible; it was not a successful source/policy bypass.

Fix:
- Require precisely `events.parquet`, `events.parquet.receipt.json`, `identity.json`, `raw/events.jsonl`, `raw/report.json`, `transactions/receipt.json`, `transactions/reconciled.ndjson`, `transactions/rejected.ndjson` as `manifest.files` keys before `_summarize`.
- Each inventory record has exactly `bytes` and `sha256`; bytes must be an actual integer (not bool/float), nonnegative and at most 128 MiB; digest must be exactly 64 lowercase hexadecimal characters. Validate the entire metadata inventory before checking output bytes.
- `_verify_completed_partition` composes existing read-only `_report`, `_events`, `_verify` and `_unchanged` from the code-pinned partitions module. Expected provenance/content/config/schema/writer/runtime hashes are rebuilt independently of the receipt. Required output/receipt paths cannot be missing and no parent creation flag is used. Bounds remain 100 rows, 8 MiB/line, 1 MiB report/receipt, 128 MiB Parquet, and checked batch sizes. The shared writer is used only for new runs; the shared partitions module was not edited.

Observed incremental tests (each RED was executed before its production fix):

| Regression | RED | GREEN |
|---|---|---|
| Deleted both partition files + omitted entries + recomputed COMPLETE, observer and blocking guards | 2 failed / 27 deselected in 0.92s: actual tree grew by 2 files; blocking guard caught pending-file `os.open` | 2 passed / 27 deselected in 0.73s after exact inventory check |
| Strict file metadata before summarization | 11 failed / 29 deselected in 2.30s: bool/float zero reached summarization; remaining malformed records lacked metadata refusal | 11 passed / 29 deselected in 1.92s |
| Exact resume with all producer APIs forbidden + write guard; direct `_summarize` with missing partition pair | 2 failed / 39 deselected in 0.78s: writer called on valid resume; missing pair attempted creation | 2 passed / 39 deselected in 0.66s after read-only composition |

Post-fix focused command:
`python -B -m pytest tests/north_star/test_development_pipeline.py -q -p no:cacheprovider --tb=short`
Result: **41 passed in 4.33s**.

Post-fix focused + dependencies command:
`python -B -m pytest tests/north_star/test_development_pipeline.py tests/north_star/test_raw_adapter.py tests/north_star/test_partitions.py tests/north_star/test_transaction_truth.py tests/north_star/test_splits.py tests/north_star/test_reservation.py tests/north_star/test_fs_integrity.py tests/north_star/test_atomic_io.py tests/north_star/test_dependencies.py -q -p no:cacheprovider --tb=short`
Result: **766 passed in 7.00s**. Historical full-worktree results above are not represented as a new post-fix run.

Regression snapshots compare the complete synthetic parent tree (files and directories, file bytes, identities, modes and mtimes). The observer permits attempted writes to expose actual mutation; the blocker refuses writable `builtins.open`/`io.open`/`os.open` modes/flags and filesystem mutation APIs before IO. Passing malformed-resume refusal and successful exact resume both have zero write attempts and unchanged snapshots. Direct summarization also refuses missing partition files without any writer capability. This guard is Python-level evidence, not a native-code or hostile-filesystem sandbox; filesystem atime changes caused by reads are not claimed absent.

Real preserved artifact verification under the same write-blocking guard returned exactly:
`resume identity mismatch: exact config/code/input required`.
Only `development_pipeline.py` differs among its nine pinned code files. Zero write attempts. All **170 checked entries** (entire real integration and existing recovery trees plus the exact raw object and acquisition manifest) retained file bytes/hashes/sizes, identities, modes and mtimes, with unchanged tree membership. All ten real integration file hashes also match the pre-edit snapshot. No real output regenerated, identity/manifest/COMPLETE rewritten, or receipt repinned. A preliminary overly broad capture-directory snapshot hit the existing 128 MiB read ceiling before resume; verification was then scoped to the exact authorized raw object and acquisition manifest rather than unrelated capture files.

Stable repaired code SHA256: `c3146fcb9134aeba43079875f1f7de1d9c04f2143872eb2effb76ba730aac659`.
Stable repaired test SHA256: `3686f7817fce86645e023823e9ca7422e1b827fada010b5584f61220f743fa1b`.

Remaining limits: private partition verifier coupling is explicitly code-pinned and dependency-tested; trusted local tree/ABA/concurrency and memory-isolation limitations remain. Inventory exactness concerns the eight manifest-listed outputs; unrelated unlisted directory entries are not deleted. No new real transform or admission was authorized/performed. **Independent re-review requested** for the repair, including malformed inventory/metadata refusals, writer-free exact resume, and preserved original artifact code-mismatch refusal.
