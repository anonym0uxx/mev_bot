# Full corrected capture envelope aggregation — executed

## Outcome

**All three raw parts reached EOF: 107,912 physical records = 107,338 accepted envelopes + 574 preserved rejections.** This exactly matches the producer's unchanged `total_raw_records=107912`. This is the entire one-minute session `20260910_015052_000575`, not another first-100 sample.

Atomic published artifact:
`D:/mev_bot-artifacts/north_star/aggregation/corrected_capture_envelopes_v1/`

| Raw part | Physical lines | Accepted | Rejected | Decompressed bytes |
|---|---:|---:|---:|---:|
| 0000 | 50,000 | 49,727 | 273 | 464,649,979 |
| 0001 | 50,000 | 49,739 | 261 | 432,350,195 |
| 0002 | 7,912 | 7,872 | 40 | 72,609,255 |
| Total | 107,912 | 107,338 | 574 | 969,609,429 |

Accepted types: transaction **86,452**, account **20,122**, slot **573**, block_meta **191**. Every rejection is `unknown_slot_status`; a separate exact-source scan confirmed all 574 rejected raw payloads are slot records with literal `status="Unknown"`. No status was repaired or upgraded. For example, part0000 physical line 0 has exact-byte SHA256 `519b8f84c645b4b11155aec13150b207f1a4f616f4bce2e674bb50a1202497f7`.

## Artifact layout and conservation

- `part=0000/`, `part=0001/`, `part=0002/`, each with `accepted/`, `rejected/`, and `ledger/` gzip JSONL streams.
- 25 compressed chunks total, 81,909,841 compressed bytes. Maximum 10,000 rows per chunk; writes buffer one record, not a whole chunk.
- Accepted rows contain `{envelope, raw_pointer}`. `envelope` has exactly `partitions.ENVELOPE_SCHEMA` fields, including original nullable fields and `null_reasons`. Every envelope is Arrow schema validated and roundtripped for equality without changing the written dictionary.
- Rejections contain reason, development restrictions, and the exact raw pointer/hash. Original malformed bytes stay in the immutable pinned source rather than being reserialized or discarded.
- Ledger contains exactly 107,912 ordered `{raw_pointer, disposition}` rows. It is an accounting index, not additional training examples.
- Every pointer binds source part, literal source path, compressed-source SHA256, zero-based physical line, zero-based **decompressed** byte offset, byte length, and SHA256 including the original line terminator. These are not compressed seek offsets.
- 8 MiB raw-record parse bound; oversized physical lines are drained in bounded pieces, hashed in full, and represented as one rejection. Final lines without LF remain exact-byte records. Zstd decoder window is separately capped at 128 MiB. Python/Arrow overhead is not a fixed RSS guarantee; one observed running Python RSS sample was 90,611,712 bytes, not a measured peak.

`REPORT.json` lists every output's compressed size/hash, decoded-content hash, row count, all per-part counts and ordered pointer hashes, schema/code hashes, and explicit prospective-development restrictions. `COMPLETE.json` pins its hash and is created only after full verification; the staging directory is published by Windows atomic no-clobber rename. Pending directories are not completed artifacts. Existing outputs are never overwritten/healed.

### Content pins

- Original producer manifest SHA256: `0c80e5816948851de1d6f06c3279e732aa7d72c76707419ec14bc5050b0a758e`
- Published report SHA256: `84798d95ae3e5af1d78029a3e33363d5e74591caf3aabd549da14fd375c00351`
- Completion marker SHA256: `29f1076395813bfc160e94c313fa5114def254fef22f944932c7d65c51ef05a2`
- Ordered raw-pointer sequence SHA256: `d9f369ce5d62feb4305e39e1370fc26d0c50135a0c43dad6adcf2692253de193`
- Executed driver SHA256: `5123e91c9dfdeb76700adebaafc887520e347b799263d59d6f9b3d88204016d2`
- Unchanged adapter SHA256: `6f11413dc4be53bac1e776485c9b1800879a2d10bc0d6d52e43bba01c14f3ec6`
- Arrow schema SHA256: `282a11a8becc2fe3bab43f70e2ff7a4619632e8da2dcabada04a2cef1690a5d4`

The new driver has explicit filename/size/SHA256 pins for all three raw parts and the manifest itself. It verifies **all four manifest payload objects**, including the normalized-events companion without interpreting that companion, before invoking per-record semantics. Full raw replay rechecks those pins before verification. Sources are read only through stable descriptors with checked non-link/reparse ancestry; trusted immutable directories are required. No hostile-filesystem or power-loss guarantee is claimed.

## Actual execution and tests

From `D:/repos/mev_bot-north-star/tools/data-pipeline`:

```text
python -m pytest tests/north_star/test_aggregate_corrected_capture.py -q --tb=short
10 passed in 0.88s

python -m pytest tests/north_star/test_raw_adapter.py -q --tb=short
89 passed in 0.29s

python -m pytest tests/north_star/test_partitions.py -q --tb=short
71 passed in 2.71s

python scripts/aggregate_corrected_capture.py
part 0: 50000 read, 49727 accepted, 273 rejected
part 1: 50000 read, 49739 accepted, 261 rejected
part 2: 7912 read, 7872 accepted, 40 rejected
complete=true, rows_read=107912, accepted=107338, rejected=574
exit 0

python scripts/aggregate_corrected_capture.py --verify-only
complete=true, rows_read=107912, accepted=107338, rejected=574
exit 0
```

The build itself verifies staged compressed and content hashes, every reconstructed envelope/rejection against a second full raw pass, every exact pointer against the ordered ledger, all source EOF/counts and producer equality before publication. The separate read-only command then repeats this against the exact published target and completion/report hashes. This is executable verification by the same driver, **not independent reviewer approval**.

Tests cover three-part tiny-fixture conservation and exact Arrow field/null semantics; injected KeyboardInterrupt and write OSError with no completion and successful retry; bad final-part pin before any semantic read; immutable producer-count mismatch; oversized-line single rejection and final no-LF record; missing output verification without healing; destination race preserving unrelated data; duplicate keys/nonfinite/empty/invalid UTF-8 rejections; chunk rotation. Initial missing-driver test failed as expected. An early decoder-window unit error was exposed by the oversized fixture and corrected before the real run.

## Scope and remaining gates

This is an explicitly exposed **new prospective validation development source**, not a source from the original EVALv2 manifest. `training_eligible=false`, `source_admission=unadmitted`, `policy_acceptance_claimed=false`; no train examples, venue truth, label truth, ownership, fills, or finalized economic events are claimed. Literal commitment `CONFIRMED` is subscription provenance, not inferred per-transaction finality.

`raw_adapter.project_record(raw_record, *, source_path, source_sha256, revision=0, source_commitment=None)` is the actual parsing API used. Neither `project_bounded_file` nor `write_partition` is called; their existing 100-row fixture bounds and source files are unchanged. Importing the Arrow schema does **not** imply the old Parquet writer was approved at scale. Downstream canonical semantic decoding, genuine venue evidence, policy/split admission and independent review remain separate work.

Created only the new orchestration script, its tests, and this receipt in the worktree. Original sources untouched; no network, commits, source deletion, old-output replacement, or venue-worker edits.
