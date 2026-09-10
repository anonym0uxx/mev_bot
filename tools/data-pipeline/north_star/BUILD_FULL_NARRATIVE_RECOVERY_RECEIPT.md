# Complete legacy narrative source-recovery receipt

## Outcome

**Recovered all 1,471 claims joined to all 1,471 content records in the supplied frozen legacy corpus.** This is complete source recovery for `creator_claim_v1` and `creator_content_v1`, not a first-50 fixture. It covers 1,421 additional claims beyond first50; earlier first50 v1/v2 artifacts remain unchanged.

- Output version: `D:/mev_bot-artifacts/north_star/aggregation/narrative_legacy_v1`
- Unmodified adapter output: `source_recovery/`
- 1,471/1,471 claim IDs and 1,471/1,471 content IDs covered; exact one-to-one joins.
- All 1,471 exact UTF-8 byte evidence spans independently checked against original claim text and stored source text.
- All 2,942 record pointers independently checked against original file paths, line numbers, offsets, lengths, SHA-256 hashes, and exact original JSONL bytes.
- All 4,414 adapter output files read back and hashed, including its manifest. All 4,420 complete-version files, including the runner, reports, and seal, read back; no unexpected or unreferenced adapter files.
- Zero source-recovery rejections, duplicate claim/content IDs, ID conflicts, missing joins, dropped records, or padded records.
- 1,470 unique text objects: one exact-text duplicate group at original content lines 1369 and 1383. Storage deduplicates those bytes only; both original IDs, rows, metadata, and claims are retained. No semantic deduplication.

## Exact source and freeze verification

Source root: `D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1.1/gold`

| Original file | Actual complete-file rows | Actual bytes | SHA-256 |
|---|---:|---:|---|
| `creator_claim_v1/creator_claim_v1.jsonl` | 1471 | 2353008 | `45077c0562b0cd56165d4d37ab894b04accef406993dce514700b4393a1a0f0f` |
| `creator_content_v1/creator_content_v1.jsonl` | 1471 | 4556118 | `ed8ce91fcae3e4ca4f4ee846372535f4f72e6be675ff4a939c00eeb57efafeaf` |

Both complete-file hashes and independently counted rows match `FREEZE_MARKER.json` fields `file_hashes_sha256` and `layer_counts`. Marker status is `FROZEN_IMMUTABLE`, freeze UUID `ng11_43bf56a50ed7`, SHA-256 `bd099d34dd7f7021d39dae7ec6caa22143fda2d27428b932302c55938a7c3f76`. Its historical certification/admission assertions are preserved as legacy metadata, not adopted as new approval.

Before/after hashing verified 308 protected files: both originals, freeze marker, reviewed adapter, and every existing first50 v1/v2 file. All unchanged.

## Reviewed adapter and explicit bounds

Read the complete adapter before execution. Its actual SHA-256 exactly matches the supplied review pin:

`8ecb07d39d0cf1756cb9f6982a33c8723435aec3f30484fda4a086cf6f7617d7`

Actual signature:

```python
recover_sources(claims_path, contents_path, output_dir, *, claim_limit=50,
                max_content_rows=100_000, max_line_bytes=1_048_576,
                max_input_bytes=268_435_456, max_selected_bytes=33_554_432)
```

Actual invocation overrides:

```python
recover_sources(CLAIMS, CONTENTS, OUT,
                claim_limit=1471,
                max_content_rows=2000,
                max_line_bytes=1048576,
                max_input_bytes=16777216,
                max_selected_bytes=33554432)
```

Selected source-record bytes were 6,909,126, safely below the 33,554,432-byte selected-input ceiling. Maximum original line sizes were 4,979 claim bytes and 29,021 content bytes. Each complete input is below the explicit 16,777,216-byte scan ceiling. The selected-byte ceiling measures retained input bytes, **not total Python process memory**; process RSS was not measured.

The adapter always labels its claim scan `hash_scope=selected_prefix`, `scan_complete=false`. This manifest was not altered. Independent scans to EOF established exactly 1,471 total claim rows, and the adapter's scanned bytes/hash equal the complete original file. Content scan is explicitly complete. Thus complete coverage is proven separately, not inferred from a limit or misrepresented manifest field.

The adapter published a new directory using its Windows atomic no-clobber path. The external runner also refuses an existing output and uses exclusive creation for reports. The version is sealed with hashes and is to be retained unchanged; this is not a claim of filesystem-enforced WORM storage.

## Classification and unresolved admission

Every row is classified in the audit as `STORED_ORIGINAL_LANGUAGE_CONTENT_RECOVERED`:

- Stored `raw_text` strings preserved exactly as UTF-8 after JSON decoding, without translation, normalization, inferred language labels, or generated rationale. These bytes are not original provider HTTP responses.
- Complete original JSONL claim and content rows retained, including metadata omitted from convenience fields such as `normalized_text`.
- All legacy source metadata and timestamps preserved and explicitly unverified. Existing legacy quality/admission labels are historical assertions, not newly validated semantic labels.
- All recovered rows remain `split=development`, `admitted=false`, `availability_verified=false`, `available_at_ms=null`, and `source_state=rights_state=temporal_state=UNKNOWN`.
- All 1,471 rows carry `ORIGINAL_PROVIDER_BLOB_MISSING`. No provider response was recovered or verified; no network was used.
- **Zero newly approved semantic labels, rights approvals, temporal/availability approvals, or training admissions.**

Historical unverified claim admission-status distribution is GOLD 470, SILVER 69, BRONZE 482, UNRESOLVED 450; these values are not current admission. All original claim rejection-reason values were null. The all-row audit independently records current recovery success and current non-admission for every record.

This completes the supplied content/claim source pair, not all narrative-state, validation, strategy, trajectory layers, all creator coverage, or missing original provider material. Those remain outside this recovery's completion claim.

## Artifacts and verification evidence

Within the external version:

- `run_full_recovery.py`: reproducible offline adapter execution and exhaustive verification; refuses reuse of the existing output version.
- `preflight.json`: full marker contents, actual entire-file counts/bytes, explicit bounds, and all before-hashes.
- `source_recovery/manifest.json`: untouched adapter manifest, including per-file hashes and counts.
- `source_recovery/recovered_sources.jsonl`: 1,471 recovered rows.
- `source_recovery/records/`: exact original JSONL rows, content-addressed.
- `source_recovery/objects/`: original-language stored text bytes, content-addressed.
- `record_recovery_and_rejection_report.jsonl`: all 1,471 rows, exact IDs/line joins, recovery class, adapter reasons, unverified legacy rejection/admission values, and explicit non-approval fields.
- `dedupe_and_conflicts.json`: complete duplicate/conflict and coverage results, including the exact-text duplicate group.
- `verification_summary.json`: actual complete-corpus counters and verification outcomes.
- `ARTIFACT_SEAL.json`: hashes/byte counts for all other 4,419 version files; the seal itself was read back exactly and independently hashed. Complete-version size: 20,315,021 bytes.

Key hashes:

| Artifact | SHA-256 |
|---|---|
| `ARTIFACT_SEAL.json` | `0d23fd6f1cd465b042439273572862b07cf863d86d81b065836364234ac4bb25` |
| `source_recovery/manifest.json` | `4aba932c387869696b700bcfe0f75b63560b1030f6579e3c70b2fe2f7608193f` |
| `source_recovery/recovered_sources.jsonl` | `15e6a1f5c60faaec8dd631000f5685ec0336d2742e5f09ad7e2c17761a2742e8` |

Executed from `D:/repos/mev_bot-north-star/tools/data-pipeline`:

```text
python -B D:/mev_bot-artifacts/north_star/aggregation/narrative_legacy_v1/run_full_recovery.py
exit_code: 0
status: COMPLETE_SOURCE_RECOVERY_NOT_SEMANTIC_OR_RIGHTS_ADMISSION
claims_total: 1471
contents_total: 1471
exact_prefix_spans_verified: 1471
source_record_pointers_verified: 2942
source_recovery_rejections: 0
sealed_files_including_seal: 4420
```

No adapter or other repository code modified. No network calls or commits. The supplied historical 111-test review count was not claimed as a newly executed suite; this task exercised the reviewed adapter on the complete actual corpus and verified every resulting file, record pointer, and span. Existing unrelated worktree changes were left untouched.
