# Stage 2 bounded event identity / finality revision journal

Status: development-only helper verified; **Stage 2 is NOT closed**. No training admission, labels, fitting, source deletion, recorder change, network access or commit.

## Owned deliverables

- `src/north_star/event_revisions.py`: pure append/reconcile API; no filesystem/network/clock calls.
- `tests/north_star/test_event_revisions.py`: independent synthetic fixtures plus real raw-adapter API integration using synthetic bytes.
- This receipt. Other workers' files were neither edited nor committed.

## Contracts and limits

Reviewed raw adapter/schema, adapter receipt and tests, master V5 Stage 2 (line 395), canonical registry identity/revision and separate observed-commitment/later-finality/correction-availability contracts. The balance-only transaction helper still makes no finality claims. Recorder `slot_status_to_str` preserves Processed/Confirmed/Finalized but collapses other enum values to Unknown. This helper does not repair Unknown into Dead/Reorg.

`append_revisions(journal, updates, max_events=100, max_revisions=400)` accepts an immutable tuple of serialized entries plus a materialized list/tuple of updates. It returns a new tuple retaining the exact old prefix. Every delivery/evidence assertion, including a repeated delivery, gets an append-only revision. Caller projection revision remains separately preserved under `input.projection.revision`; account write_version is never treated as journal revision. `summarize(journal)` is a deterministic set-reconciled view, not an overwrite of history. Current derived results cannot be used as historical features; pass the appropriate immutable journal prefix for an as-known view.

Hard maximums: 100 distinct event/slot identities, 400 journal entries. Positive exact-integer caller budgets checked before iteration; revision count checked before decode; event identity preflight checked before full projection validation/serialization. Smaller run budgets are supported. A 1,048,576-character entry ceiling, depth preflight of 32 and 10,000-item container ceiling bound normal materialized inputs. Arbitrary hostile Python objects are outside this pure JSON-input API. No generator consumption. Caller input construction/I/O is outside these limits. Malformed batch raises without returning partial revisions. Re-reading journal validates identities, assertion hashes, operation and consecutive per-identity revisions; this detects inconsistent entries, not cryptographic tampering by a party able to replace and rehash the whole journal.

Update shape: `kind` (event or slot_evidence), `chain`, `network`, `projection` (raw_projection_v1), `fork_id`, `fork_evidence_ref`, `payload_sha256`. Slot evidence additionally names `evidence_authority` and `evidence_ref`. All literal provenance is retained; tokens are not normalized/repaired. Digests must be lower-case full SHA-256. Source projection admission must remain unadmitted and training_eligible false. Identity validation is not Solana cryptographic validation or full consumer schema admission.

Identity scope is `(chain, network, record_type, slot, fork_id)` plus transaction signature, or account pubkey/write_version, or blockhash for block-meta. Slot observations share their slot/fork identity across statuses. Missing fork stays null, never gets inferred from time or maximum slot. Known forks with the same signature and slot are separate identities. Unknown-fork identity is a provisional unresolved bucket, not proof two deliveries are the same fork.

`raw_delivery_id` hashes the source locator `(source_path, source_sha256, record_index)`. Original adapter event_id (which includes exact record hash) and raw_record_sha256 remain intact in each input. `assertion_id` hashes the complete input assertion. Duplicate raw-delivery count is repeated identical assertions at a locator; conflicting raw-delivery count is additional distinct assertions at a locator. This deliberately conservatively treats changed projection/provenance assertions at one locator as conflicts too, rather than silently replacing one. Both logical identities are tainted when a locator is asserted with incompatible identity. Logical-payload repeated/conflict counts use distinct input assertions with supplied full-payload digests, exclude exact repeated assertions, and never select the last payload. Multiple digests retain all candidates and resolve to null. Neither producer raw_hash nor exact NDJSON-record hash is promoted into full payload-equivalence evidence: producer hash scope can be narrower and record hash includes delivery metadata/formatting. Zero detected payload conflicts on missing digests is NOT verified payload agreement.

Finality requires an explicit `slot_evidence` assertion with authority `slot_notification`, exact chain/network/slot, non-null matching fork_id, and caller-supplied fork-membership evidence references. The helper validates references structurally, not by contacting a chain. Observed subscription commitment remains separate even when its token says FINALIZED. Bare projected slot events do not implicitly become evidence. Unknown status, another slot, absent fork membership, another fork/network, elapsed time and maximum observed slot cannot finalize an event. Conflicting source-locator evidence, competing finalized forks or Finalized vs Dead/Reorg yield null finality, never last-write-wins. Matching Dead/Reorg is an explicit `supersede_slot` journal operation and derived `superseded_by` reference, preserving original events and other forks. Unknown-fork supersession evidence is retained but cannot be attributed to a specific event fork. No replacement fork is fabricated. Correction availability remains null with clock UNKNOWN; recorder receive milliseconds do not establish verified UTC correction availability. Diagnostic null-reason strings are local development reasons, not a completed canonical null-reason schema.

## Strict TDD execution

Tests were written and run before each implementation increment. Observed RED -> GREEN sequence:

1. Missing module: 1 failure -> 1 pass (immutable history / identity separation).
2. Bounds: 7 failures / 1 pass -> 8 passes.
3. Account/network/fork/block identities: 5 failures / 8 passes -> 13 passes.
4. Duplicate/conflict reconciliation: 3 failures / 13 passes -> 16 passes.
5. Finality and supersession: 5 failures / 24 passes -> 29 passes.
6. Validation/tamper/boundary cases: 19 failures / 30 passes -> 49 passes.
7. Preflight ordering, competing finalized forks, size ceiling: 3 failures / 50 passes -> 53 passes.

Permutation tests independently assert expected payload conflicts, matching evidence before/after event arrival, separate forks, competing finalized forks, Dead/Reorg and contradictory evidence. Incremental and batch journal output must match exactly; pure input history remains unchanged. Synthetic expected identities/states are not generated by the implementation.

Final commands (PYTHONDONTWRITEBYTECODE=1, python -B, pytest -p no:cacheprovider):

- `tests/north_star/test_event_revisions.py`: **53 passed**.
- Plus `test_raw_adapter.py`: **142 passed**.
- Plus `test_transaction_truth.py`: **202 passed in 0.47s**.

## Bounded already-exposed real exercise

Only the existing first100 adapter artifact was read, not compressed raw, not another source partition. Its 349,639-byte / 98-envelope content hash matched the prior receipt before use and remained byte-identical afterward. Prior adapter: 100 rows exposed, 98 accepted, 2 rejected Unknown slot statuses. Those rejects remain rejects; this helper did not invent their payloads.

New artifact directory, created only when absent:
`D:/mev_bot-artifacts/north_star/development/event_revisions/part0000_first100_v1/`

- `journal.jsonl`: 98 entries, preserving 54 transaction and 42 account observations plus 2 explicit slot-evidence assertions.
- `summary.json`: 96 logical event states; **0 finalized, 96 null later_finality**.
- `report.json`: source and implementation hashes, budgets, counts and limits.

Run budgets: max_events=100, max_revisions=100. Source commitment CONFIRMED on all 98 envelopes is subscription provenance only. Actual slot notifications are Processed at 445637627 and Finalized at 445637596; the latter cannot finalize transaction/account observations in a different slot. Fork membership is absent throughout. Chain/network solana/mainnet-beta is explicitly a caller development assertion, not verified by these envelopes. All 96 event deliveries lack a supplied full-payload digest; resolved payloads remain null. Duplicate raw, conflicting raw, repeated logical payload and conflicting logical payload counts are all zero, with 96 payload-unverified deliveries. No finality or economic label is admitted. Raw-file hash is inherited from prior adapter verification; raw was not opened/rehashed in this exercise.

## Full SHA-256 provenance

- `D:\repos\mev_bot-north-star\tools\data-pipeline\north_star\master\NORTH_STAR_WINDOWS_MASTER_V5.md`: `ebee45d5b98a5daeed72edebf16e7ef2b8878d5cc96b191c451584a315cea57c`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\schemas\north_star\CANONICAL_REGISTRY.json`: `c204f940371f2f1bfe318b6b215614727da13d564a64017bdb8e8ea9ce062685`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\src\north_star\raw_adapter.py`: `6f11413dc4be53bac1e776485c9b1800879a2d10bc0d6d52e43bba01c14f3ec6`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\src\north_star\schema.py`: `17ede8827c9f30ee75b5d3b50244137c80b3634e496521617982c21db8914419`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\src\north_star\transaction_truth.py`: `f3410aca28d4fb62878da1323f9c3c9de659bbb171243235c8ad2e750813a110`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\north_star\BUILD_RAW_ADAPTER_RECEIPT.md`: `9c810629f5eb6c1b6fcb2c8f65621c5079156e4a346084b06c0eeb286aee1b39`
- `D:\repos\mev_bot\tools\stream-capture-rs\grpc-server-only\src\raw_recorder.rs`: `ced2af61486619270de24fbd59b13c82dbabee88de95fa53df100e97c89ad06d`
- `D:\mev_bot-artifacts\north_star\development\event_revisions\part0000_first100_v1\report.json`: `f00acbea98acea74462396e57df28164482fc150c09ca7d6e8ac059e58cb565a`
- `D:\mev_bot-artifacts\north_star\development\event_revisions\part0000_first100_v1\journal.jsonl`: `d94c2d9703b2ff405a003b2bdb0ec176d8b9a3ccfb2f3280821361d6762ecd21`
- `D:\mev_bot-artifacts\north_star\development\event_revisions\part0000_first100_v1\summary.json`: `f9dc95a5a9c69a871abcd46d4fe0037572246a3fb841b6cd6facd1b23372c234`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\src\north_star\event_revisions.py`: `2dc63cfc8752af30e4c02d76e5169ef34a8880d840835f47f6ba49f4cc1184a9`
- `D:\repos\mev_bot-north-star\tools\data-pipeline\tests\north_star\test_event_revisions.py`: `611779b86a5682ad5a1af12ed2bf34c196551210a825a91c5eaaf0a4ded5d06f`
- `D:\mev_bot-artifacts\north_star\development\raw_adapter\part0000_first100_v2\events.jsonl`: `87d8f3210a9e392add7da328feff6b5ad6b20d7c85cd4a02d40f6b54e030586e`
- Preserved raw SHA-256 (prior adapter assertion only): `85a02eb32405d8e0ddd96f463ceaea1f52951161298e9f1f027e854e4935543f`.

## Remaining gaps

No real event-finality positive case is established by this slice. Full payload digests, verified chain/fork membership, reliable dead/reorg notifications, correction availability, source admission and complete consumer schemas remain external prerequisites. No whole-source canonicalization, Parquet export or ledger/instruction integration was attempted. This is a bounded reusable journal primitive and negative real evidence, not Stage 2 closure.
