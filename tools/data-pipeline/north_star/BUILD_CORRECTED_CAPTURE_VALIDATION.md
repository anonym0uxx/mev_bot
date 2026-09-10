# Corrected capture: one-minute prospective validation

Actual candidate invocation completed exit 0. New session `20260910_015052_000575`, artifact directory `D:/mev_bot-artifacts/north_star/development/corrected_capture_validation_v1/`.

- Candidate SHA256 `e87d98ac982d149b2f4afcfabc71b7fbbdb36e782f343566d243de15169cfc59` unchanged before/after.
- Producer reports 107912 raw records and 15881 normalized events. Parent verified manifest byte counts and SHA256 for all four payload files. These are producer record counts, not independent semantic census.
- First100 raw records were read: slot5, transaction67, account28.
- First10 transactions have ten literal 32-ones system-program identities and zero 33-ones identities. No historical record was normalized.
- Exact raw-byte endpoint reconciliation: eight results with `canonical_identity_status=format_valid_only`; two `missing_token_counterpart` rejections. This removes the old formatting quarantine in this sample, not execution/finality/ownership uncertainty. No admitted trades or training examples.
- Parent initially passed parsed dicts to a bytes-only API. `FIRST10_PARENT_RECONCILIATION.json` preserves those invocation errors; the corrected exact-source-byte run is `FIRST10_PARENT_RECONCILIATION_V2.json`. Do not count the former as source defects.
- Runtime manifest `repo_sha` is empty because WSL Git cannot resolve the Windows absolute worktree pointer. `LAUNCH_PROVENANCE.json` separately pins Windows HEAD, source hashes and candidate binary. No Git pointer repair or fabricated runtime SHA.
- One-minute validation only, bounded 180-second wait with graceful stop/kill fallback unused. No old process/binary/launcher replaced; no trading. Existing subscription scope unchanged. Billed credits not directly metered; no overage settings changed.

Parent additionally ran actual full crate tests after serialization fixtures landed: **13 passed, zero failed**, covering six new serialization cases plus seven encoder cases. Release binary predates test-only additions. The live run validates candidate acquisition/encoding, not whole-corpus semantics or restart supervision.

Completed process `proc_ff6c73746f2e`; process exit and launch metadata persisted beside immutable raw files. No source deletion. Independent semantic/reconciliation review and approved future dataset integration remain required.
