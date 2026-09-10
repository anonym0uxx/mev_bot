# Stage 3 narrative context development receipt

## Outcome and boundary

Implemented `src/north_star/narrative_context.py` and synthetic-only parser tests
`tests/north_star/test_narrative_context.py`. This is a **context-assembly eligibility
contract**, never training admission, semantic review, verified trade attribution,
collector completion or Stage 3 closure. `training_admitted` is always false.
No paid calls, scrapers, collector edits, corpus relabeling, generated teaching prose,
commits or other module edits were performed.

The gate requires hash-verified raw bytes with bounded BYTE evidence span and
content version; verified source/speaker/entity confidence evidence; cleared
CONTEXT_ASSEMBLY rights evidence; exact Solana mint link bound to content
version; same-mint numerical snapshot at the same decision cutoff; origin graph;
and UTC availability for publication, retrieval, claim, media, proofs and listed
dependencies. Max dependency time plus explicit compute delay controls the join.
ASR requires completion and resolved media epoch proof. Same-timestamp joins
require explicit causal-order proof. Unknown source stays null with a reason.
Reposts/duplicates retain canonical testimony grouping and graph edges, never new
independent testimony. EX_ANTE, keyword themes and GOLD have no gate authority.

## Independent-review remediation (supersedes original trust limitation)

Recovered the complete `deleg_88620449` result from the local state database
read-only; the live transcript truncated the final response. Its full must-fix
list is **N1, N2, N3**, all HIGH. No additional must-fix was omitted.

- **N1:** `evaluate_context(..., trusted_proofs=...)` now requires a separately
  supplied authenticated upstream registry. Row-embedded registries are ignored.
  Missing/unresolvable/mismatched attestations never confer context eligibility,
  verified source identity or independent testimony. Structural-only results are
  explicit (`structural_valid`, `verification_status=STRUCTURAL_ONLY`); their
  availability/group/edges are declarations, not verified provenance.
- **N2:** Every resolved attestation must match the entire evidence envelope:
  raw_id, content_id, content_version, SHA-256, BYTE unit and exact start/end.
  Changing an in-bounds interval or replacing bytes and recomputing their hash
  cannot reuse old attestations. Local raw hash/span validation remains required.
- **N3:** Snapshot proof resolution binds snapshot_id, snapshot_version, Solana
  chain, mint, observation/availability clocks and decision cutoff. Observation
  must be a nonnegative integer no later than availability or cutoff. All proof
  declarations use closed schemas; unsupported/conflicting extra source/hash/
  timestamp fields are rejected, not ignored. Cutoffs reject floats/bools.

### Registry contract and remaining authenticity boundary

The registry is an explicit API argument loaded through a trusted caller path,
not a field read from the candidate and not a production verifier implemented
here. Each `evidence_ref` maps to an attestation with:

- `kind`: exact proof role (`source_confidence`, `speaker_confidence`,
  `entity_confidence`, `rights`, `mint_link`, `numeric_snapshot`, `origin`,
  `dependency:<literal dependency key>`, `media_epoch`, or `same_time_order`).
- `subject`: exact source_id, speaker_id, entity_id, content_id, content_version,
  chain, mint, snapshot_id and decision_cutoff_ms. Missing identities fail closed.
- `evidence`: exact complete evidence envelope, including hash and BYTE interval.
- `declaration`: exact role-specific proof object, including state, proof version,
  evidence_ref, clock and available_at_ms; no unknown fields. Snapshot additionally
  requires snapshot_version, chain and observed_at_ms.
- `availability`: exact complete availability object, including all listed
  dependencies, optional epoch/order declarations and compute delay.

Comparison is typed/deep: integer 100 never equals float 100.0 or bool. Epoch,
origin, rights, dependencies and same-time order require their own resolved role,
not a shared magic reference. Unknown timing fields are rejected on the verified
path. The snapshot observation uses its declared UTC_KNOWN clock; no secondary
unresolved clock field or inferred as-of timestamp is accepted.

The upstream loader must authenticate and preserve registry snapshots; passing a
registry synthesized from a row violates the API trust boundary. No signature
checker, legal verifier, numerical reconstruction or network loader is supplied
or claimed. If authentic attestations cannot be supplied separately, call without
the registry: eligibility stays false. `REGISTRY_VERIFIED` means matching trusted
attestations plus local structural checks, not independently established semantic
truth or training admission. Training admission is always false.

### Remediation RED/GREEN evidence

All commands ran offline in the working directory below, with
`PYTHONDONTWRITEBYTECODE=1 python -m pytest ... -q -p no:cacheprovider --tb=short`.
Test fixtures are synthetic protocol-only bytes/IDs, never real positive rows or
teaching examples. Existing positive parser tests now explicitly supply isolated
fixture registries; no registry is generated in production code.

- Original baseline: **39 passed**.
- No-registry promotion regression: **1 failed**, then **1 passed**.
- N1 lookup/role/subject/version/clock/timing/origin/order regressions plus positive
  control: **16 failed / 1 passed**, then **17 passed**.
- N2 hash/interval/raw-object rebinding: **5 failed**, then cumulative **22 passed**.
- N3 unsupported/contradictory snapshot/source metadata: **15 failed / 5 passed**
  (the five snapshot replay cases already rejected by N1), then all N3 passed.
- Further exact cutoff/schema/source-null regressions: **5 failed / 10 passed**
  (ten per-role missing-attestation controls already passed), then green.
- Final combined narrative/availability/media-clock suite: **182 passed in 0.46s**.
- Re-executed the already-exposed first50 audit: exact equality with the original
  embedded payload, **50 audited, 0 eligible, 0 training admitted**. Canonical
  audit hash remains `6cda4dc1d26da383f1208f3c6ab4bebd7a46b9442682aba702533bb4eb90ad09`.
  Independent post-audit source rehash also matches the original source hash.

The original audit payload below is preserved, not relabeled or replaced. No
source changes, network calls, commits or edits to other modules were made.

## Original implementation RED/GREEN record

Working directory: `D:/repos/mev_bot-north-star/tools/data-pipeline`.
Command: `python -m pytest tests/north_star/test_narrative_context.py -q --tb=short`.
Each failing assertion run preceded the corresponding implementation:

- Missing gate: 1 failed (module path assertion), then 1 passed.
- Immutable raw hash/span: 1 failed / 1 passed, then 2 passed.
- Identity/rights/mint/snapshot: 1 failed / 2 passed, then 3 passed.
- Timing/ASR/dependencies/origin: RED run exited 1, then 28 passed.
- Null cutoff/source and read-only audit: 6 failed / 28 passed, then 34 passed.
- Mixed proof clocks/self-canonical repost: 5 failed / 34 passed, then **39 passed**.
- Owned-path `git diff --check` exited 0 (untracked files are not covered).
- Combined regression: `python -m pytest tests/north_star/test_narrative_context.py tests/north_star/test_availability.py tests/north_star/test_media_clock.py -q --tb=short` returned **125 passed in 0.26s**.
- Independent post-audit whole-source SHA-256 recheck returned `45077c0562b0cd56165d4d37ab894b04accef406993dce514700b4393a1a0f0f`, identical to the audit input hash.
- Owned-path git status lists exactly the three new untracked files; no commit was made.

These are synthetic software tests, not teaching examples or creator testimony.

## Real-source audit and reproduction

`python src/north_star/narrative_context.py D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1.1/gold/creator_claim_v1/creator_claim_v1.jsonl --limit 50`

Actual CLI exited 0. First 50 physical rows only were decoded; remaining file bytes
were hashed without parsing more rows. No missing metadata was invented or
inferred from prose, handles, first_seen, labels or scores. No raw span/version
proof is present in these rows; claim_text was not exported. All 50 remain
ineligible for context assembly. Every missing-proof reason below occurs in 50
rows. Account handles remain observed metadata, not verified identity. Missing
means not proved by this bounded audit, NOT that no companion source exists.

Legacy rows remain unchanged. This is DEVELOPMENT / EXPOSED, never heldout.
The JSON embedded below is the new development-only audit artifact, keeping all
repository writes within the three owned files. Row IDs/hashes and literal metadata
provide source handles, with no generated narrative content. Original CLI log:
`C:/Users/Alon/AppData/Local/hermes/cache/terminal-output/out-1788978560-6524-3390.log`.

## Limitations / remaining integration

- Trusted upstream registries must verify proof references and bind source,
  speaker, entity, rights and immutable versions. This gate checks the contract
  and raw bytes, not signatures, legal grants, semantic truth or dependency completeness.
- Media epoch and uncertainty-conservative latest times must be resolved upstream;
  unknown/relative clocks fail closed for UTC context. No media mapper implementation
  or coupling/edits to concurrently owned availability/media-clock modules.
- Numerical reconstruction, on-chain action attribution, rights revocation, split
  protection, human review, final exports and training admission remain separate.
- Downstream assembly must group by testimony_group, never sum repost row counts.
  Legacy amplification_cluster_id/originality_confidence cannot prove independence.
- First50 is bounded, not representative/full-corpus certification. No positive
  real-data eligibility or completed Stage3 capability is claimed.

## Audit payload

Canonical audit SHA-256 (UTF-8 JSON, sort_keys=True, separators=(',', ':'),
ensure_ascii=True; excludes receipt wrapper): `6cda4dc1d26da383f1208f3c6ab4bebd7a46b9442682aba702533bb4eb90ad09`

```json
{
  "scope": "DEVELOPMENT_EXPOSED_METADATA_ONLY",
  "source_path": "D:\\repos\\mev_bot\\tools\\data-pipeline\\output\\narrative_gold_v1.1\\gold\\creator_claim_v1\\creator_claim_v1.jsonl",
  "source_sha256": "45077c0562b0cd56165d4d37ab894b04accef406993dce514700b4393a1a0f0f",
  "prefix_sha256": "67674bb1e4ce8ade9ad82a6e99098695017e9eaf7c283d4d538f5de99bb4cca1",
  "rows_audited": 50,
  "requested_limit": 50,
  "context_eligible_count": 0,
  "training_admitted_count": 0,
  "reason_counts": {
    "AVAILABILITY_MISSING": 50,
    "DECISION_CUTOFF_UNRESOLVED": 50,
    "ENTITY_CONFIDENCE_MISSING": 50,
    "MINT_LINK_UNVERIFIED": 50,
    "NUMERIC_SNAPSHOT_MISSING": 50,
    "ORIGIN_RELATIONS_MISSING": 50,
    "RAW_EVIDENCE_MISSING": 50,
    "RIGHTS_UNRESOLVED": 50,
    "SOURCE_CONFIDENCE_MISSING": 50,
    "SOURCE_UNKNOWN": 50,
    "SPEAKER_CONFIDENCE_MISSING": 50
  },
  "rows": [
    {
      "line": 1,
      "claim_id": "5a568ffdf4b307b5",
      "row_sha256": "372d64465b33d5c2d7a4b5c8f747c752364a548fb8edde4c620fa0f4fcdfae2e",
      "legacy_metadata": {
        "content_id": "ce4eb4dec2ed644a",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "crypticannouncements",
        "platform": "telegram",
        "entity_type": "unknown",
        "primary_mint": null,
        "resolution_method": "unknown",
        "resolution_confidence": 0.0,
        "admission_status": "SILVER",
        "temporal_class": "EX_ANTE",
        "publish_time_ms": 1787784872762,
        "first_seen_ms": 1787784872759,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 2,
      "claim_id": "6cef509503020c87",
      "row_sha256": "c94024604babcbf70ed0bdd5645834816f64bafe0a5dcf8f4718dd5633dc0c3c",
      "legacy_metadata": {
        "content_id": "bb38652b9463fa55",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "chasescharts",
        "platform": "telegram",
        "entity_type": "unknown",
        "primary_mint": null,
        "resolution_method": "unknown",
        "resolution_confidence": 0.0,
        "admission_status": "UNRESOLVED",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784875195,
        "first_seen_ms": 1787784875193,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 3,
      "claim_id": "7780acb577873a51",
      "row_sha256": "b0e5522a80556ccc0f227529821ad3d3daedd4477254c08d786031466e2c6ede",
      "legacy_metadata": {
        "content_id": "ee0f7e845ad13236",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "PikalosiCalls",
        "platform": "telegram",
        "entity_type": "mint",
        "primary_mint": "J5y4mRPHimVz13tT5Ty6efvq6Nn7oBdTpNQLFusPpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784878098,
        "first_seen_ms": 1787784878097,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 4,
      "claim_id": "59dcfab38be51b3f",
      "row_sha256": "7d4056f2e3b76c9ea4d899075733fd2a765b805a399e08d8e7234fa42d44ab1c",
      "legacy_metadata": {
        "content_id": "d1b72014b26e27f3",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "Marlonalpha",
        "platform": "telegram",
        "entity_type": "mint",
        "primary_mint": "HSeBxUHkZouxUR6WGoB8DS475vLoVrcf48YM8BFFpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784911325,
        "first_seen_ms": 1787784911323,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 5,
      "claim_id": "950e1e0a3cb5d225",
      "row_sha256": "64f67ff6eae6b9f3db7826257e997c226abfe123c1bf511662dfcd463b0ba63f",
      "legacy_metadata": {
        "content_id": "a3a2538fabe38bd3",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "solanaalpha",
        "platform": "telegram",
        "entity_type": "mint",
        "primary_mint": "3FoUAsGDbvTD6YZ4wVKJgTB76onJUKz7GPEBNiR5b8wc",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "GOLD",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784918858,
        "first_seen_ms": 1787784918857,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 6,
      "claim_id": "917bbfe20e48a789",
      "row_sha256": "d440f6733adb3278036974ae3f9058dd911e2b2b50b4c34a2f0760825d3f0e04",
      "legacy_metadata": {
        "content_id": "f830f0e6868188c4",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "alphacalls",
        "platform": "telegram",
        "entity_type": "mint",
        "primary_mint": "5u2cFYg9GRiP7fheZyEi9WcHUFFb2txFXSLC1uVApump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "GOLD",
        "temporal_class": "EX_ANTE",
        "publish_time_ms": 1787784921379,
        "first_seen_ms": 1787784921378,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 7,
      "claim_id": "d64395af728f5264",
      "row_sha256": "f1f3f656c089af5a105ca3189bfac1ce575d465e794c6ed77fd3e6ba5016e854",
      "legacy_metadata": {
        "content_id": "afc2f5fbfc3e55d2",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "EsjmiuE4e3CQmo3xMrUQk1wtHLspUqQhr3FvUea1pump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 8,
      "claim_id": "573220b274707a26",
      "row_sha256": "3bf0ea8b79cffed718e3f4b9ffb05882e1a7243312eac454c34c7980a2b674ce",
      "legacy_metadata": {
        "content_id": "e8fd8c70ed4469ee",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "Fm6L6QQzQ4p1Wa7jte3K91HSf8NBkJ7ddSy2fKEMpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 9,
      "claim_id": "c835117457bcef1e",
      "row_sha256": "ef5ae24f574f499d17b4354499ceb0309e0f2330daf515e048d2f111e3196812",
      "legacy_metadata": {
        "content_id": "709ac597b47fbc9b",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "J8PSdNP3QewKq2Z1JJJFDMaqF7KcaiJhR7gbr5KZpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 10,
      "claim_id": "d0ca76f76ae908ea",
      "row_sha256": "6e999eed924865cdc98bc8ca528371cbda4ef259fd0e2658d42e548956f3e87b",
      "legacy_metadata": {
        "content_id": "fe500e97a4760555",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "3qYtMU95VZSQkbVRUZv3pfTFFRtscKJhkW8KjN2Spump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 11,
      "claim_id": "58a0bd8c2ef9e481",
      "row_sha256": "74b2a2b11b0bd898ada1fefc382c7529a41ca0c201064f81b6f027f6aa6a1c88",
      "legacy_metadata": {
        "content_id": "a59aca1492ac6295",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "65nXmwxNvF1akva3Q9AaTT8AEqVrkzQXZEi9Rjzwpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 12,
      "claim_id": "8286540fa8e3c1a5",
      "row_sha256": "90c9ef22bf2cdf55f6eb79674f173a8c4dc46c1955ae737453494f2e85d9e9a8",
      "legacy_metadata": {
        "content_id": "d3d39a4564667fbb",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "FWuSmM21ZdwtVw6swS7GMQrAUrPKHP6BaesJyda3pump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 13,
      "claim_id": "ddf0b4549179cd76",
      "row_sha256": "c889786cd679e1c333dab4d1f8276dad7c1f9fbb2bdd3424d630545432bf2658",
      "legacy_metadata": {
        "content_id": "f3704a75472c8d9d",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "9Jgx76eE5f7ncc7TATqhzU1hPVXvFnX8HJ1d88mhpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 14,
      "claim_id": "9c19c34cb267756f",
      "row_sha256": "a4920dc11cb99f6e4f0bac15255ae611c7a60034f4eef3e12ffda33ece80c8b5",
      "legacy_metadata": {
        "content_id": "10010218cee2d3ba",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "9cRCn9rGT8V2imeM2BaKs13yhMEais3ruM3rPvTGpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 15,
      "claim_id": "9d683cf94896a95d",
      "row_sha256": "94cf75272c190aa87f9045584068eab9afe8740b9e73e789416c6a371aabbc9d",
      "legacy_metadata": {
        "content_id": "2b64e60c073edf7e",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "EygStH4gHv1h4E8w4raYv6NGsrDiij3nbfCc26iTpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 16,
      "claim_id": "fd4942e200cce4b9",
      "row_sha256": "ba16e21cdfe3ac074c397743fbdd2ff6ab3aa7e477fb169ec2d4eb8e3207d458",
      "legacy_metadata": {
        "content_id": "0adfdac69fa60de2",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "AnDE3asgH28QbinZ532Hqq1NoLjeLZPQ9w7MKCg9pump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 17,
      "claim_id": "6cad4547d7f98d54",
      "row_sha256": "9f468bcc54a0170567a0ea6420e49e2eb013a8a900ba4f8694ff91fd0f2fa703",
      "legacy_metadata": {
        "content_id": "1697b7c310cc9d0b",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "FZqdw6oSDCbHtKYxmhnfbi97SnyVy8jaYpdCoMrrjKa2",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 18,
      "claim_id": "f109bee0b9cf88a5",
      "row_sha256": "4558daa8af92ebac0d1c8d261269345d7d3681f6e8383b504acb6c07b1b2b5cc",
      "legacy_metadata": {
        "content_id": "7823981166b53519",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "ApZuxdpzMrbEYTGEzeY9afh5pj9d6qPRJCTgQYiipbKg",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 19,
      "claim_id": "4db6b0daf229df3d",
      "row_sha256": "aafe2f02957b3d5b42d2e81a35dcc8ef040e1914755054706f92ed121f3ffaa3",
      "legacy_metadata": {
        "content_id": "2fb0a1b9ecd72899",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "BRLAEDmZVZsvUKGzfYK7cQyhhJ3VN3sKMhbXUhsapump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 20,
      "claim_id": "94be8c3571a94c82",
      "row_sha256": "4398dc8d8809a0e0559721534a7dff627f6ef36e20f7abbebcdcde590720dee0",
      "legacy_metadata": {
        "content_id": "d1b479088433b803",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "j1ywxmt1nMRDQxZucL3CR5b3Z8DWEjTMrYdJUbPpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 21,
      "claim_id": "b2b8c2cc090fc0ac",
      "row_sha256": "0053b2344d9102274f51ca0c9547245faa22f250e3cce599bf69b39add1faaa2",
      "legacy_metadata": {
        "content_id": "80886d7d45cf151d",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "BsHvbwks2uN84vy627h1WhSsZDjSzuQNnaQsaR7apump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 22,
      "claim_id": "6c220250dd1e1d3e",
      "row_sha256": "e096fbac3e2651c352f2ccbe393e01357f366d7cff55dd45a185ffdbe0389834",
      "legacy_metadata": {
        "content_id": "443bd363b646a8d1",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "4mRWq4bBQXNoL2jWx5XydqSrnT96J55fQVahGtPFpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 23,
      "claim_id": "7342e2bb9ec55d08",
      "row_sha256": "032209c3051cafec60df59ec8d8ccf62db8f75e49af687507497a56833d2ff27",
      "legacy_metadata": {
        "content_id": "3a30baf94d23e5c7",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "2pAL1orRGyYTmfxw7SmdXCNyb3b9pvmuyFYoon8ppump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 24,
      "claim_id": "7ba327a34ea2d400",
      "row_sha256": "2463c6e4a0a5fb34fae9d3e82f02603c08c5d4d816e9097c4439a1c79584c178",
      "legacy_metadata": {
        "content_id": "0b826589fb555914",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "6E7rGoMUtVkruuWwDX6H6tHGwTuKp8mMobJULVTmpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 25,
      "claim_id": "509a45eb2092d6a7",
      "row_sha256": "9494a2c9408cf0bad9913382347653cb6f0a27168be01f13b8da56d58f4b29ba",
      "legacy_metadata": {
        "content_id": "49b2fb4882bee5d6",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "FbHGuqhPKMHKNJsymHH9XHh2X3ZQenJZpZkVaT1zpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 26,
      "claim_id": "19a8502802c4aafe",
      "row_sha256": "a71d3e86aa7136ec01c02a777d9957fafa702a26674263d28aef70c2b2eeb063",
      "legacy_metadata": {
        "content_id": "273ee7ed5464506e",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "ETFBbQ2hijwn1KGx5ZWerSVaNANQvhgC7aZwZ4WWpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 27,
      "claim_id": "bdfdddedf669101a",
      "row_sha256": "ad99c3b8bc794819d57f865839f33bdb5b01f850f845e2717691547aaf71ed8a",
      "legacy_metadata": {
        "content_id": "5083367f1fe80dea",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "H7xewRd49WN55Bz8zzu8BdpvCj2mPTY9cPrkuXScpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 28,
      "claim_id": "57cc5779ce8bf564",
      "row_sha256": "257cab7716d4bfa9a53b376b2f92533c6574da2f652cd17b36f4ad7999ba7fd8",
      "legacy_metadata": {
        "content_id": "da3a7ba1d5495068",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "297uf7pp737mnoqbWTp8APqkWASqbsE9hyjWrvivpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 29,
      "claim_id": "cbebb1923eb08a65",
      "row_sha256": "11b428693fa4f03dc88bb9619dbcad2791d76b46a559bc668feb8657b4062f58",
      "legacy_metadata": {
        "content_id": "c1b5df48d8c57923",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "7dc5UGmB1x6ykgXUf3w4JhfbZWX87qgAXZ5HArVqpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 30,
      "claim_id": "a35fc17a6ff82df5",
      "row_sha256": "b54c053eac77daa8b8ed9b0b7ed747296a50d7d9ef4a44743ed3468934420401",
      "legacy_metadata": {
        "content_id": "234c3584d086ed01",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "0x020bfC650A365f8BB26819deAAbF3E21291018b4",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 31,
      "claim_id": "2c7989722e855a48",
      "row_sha256": "dc262eacb621f6633db90ea607b7452cec35191f966816b599d63f907adccb36",
      "legacy_metadata": {
        "content_id": "bbf7a8f595246cc4",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "Ge87EtsjwRQbHaqQmKRno69RFTwh9bfSsm99XNxTpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 32,
      "claim_id": "c3ab72a98dbdea44",
      "row_sha256": "b83ebea0dda14d65f8dc29ccd7ba5fbf71148b35747021f5ff87e61537c4f0e6",
      "legacy_metadata": {
        "content_id": "df8ee95fe8c7aec4",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "2TvUVvuQeVcUwVFp6WVy61wwpnSMG6kNzYpdvEkMpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 33,
      "claim_id": "aa2fafa09f9b26e3",
      "row_sha256": "b85d0ea315b06cbbb85b381f025f2d3b437b0e2f2018e333d0f4d54936323684",
      "legacy_metadata": {
        "content_id": "877d72b3a98a96e4",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "3vSD9xyKCfRBpP3uDEUJaPyWGNWZDFkv4C4qHbjLpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 34,
      "claim_id": "2334098c0c3c9786",
      "row_sha256": "3865f032310a3aa7f0ea427c6caa69521dc7a2f183e998eb8869affe52761d8e",
      "legacy_metadata": {
        "content_id": "5ea5a44ce27fddbf",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "Ce2gx9KGXJ6C9Mp5b5x1sn9Mg87JwEbrQby4Zqo3pump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 35,
      "claim_id": "f8be45f932bb1027",
      "row_sha256": "83ee6c3fc6c1ec299a215d0a3cb5759266a256d283149c66ec71b64d4f672645",
      "legacy_metadata": {
        "content_id": "426d4ec4e634adde",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "gzhDEPxXShcyX8oKCidBnBXECPTrKfE5zbwJ6ePpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 36,
      "claim_id": "e863ed8877793bc1",
      "row_sha256": "73111219a0593a93618510bfbfe95d916e6330290526cac160089ac41eaa3bba",
      "legacy_metadata": {
        "content_id": "e088a4a309bce01c",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "BcHEaaTCvycPwwsJ9yQTXdHP9X2gCLkznDbZ8VySpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 37,
      "claim_id": "18eed925ffddc08a",
      "row_sha256": "0a4c87874ecd26337e6db4633340f19aa9f869687badbe7f660f896b69ee232e",
      "legacy_metadata": {
        "content_id": "bc03ed0a4fc1063f",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "GUmbtfjSZkybSFgPibBcvwExEBdXwewJHR5PkTjzpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 38,
      "claim_id": "bc6fe8d865e3f06c",
      "row_sha256": "ffb5d4a328746577e425f4f69d0e9fee4f8601bdc9b95ff9f0df91e5b984cee1",
      "legacy_metadata": {
        "content_id": "77d2d9be9d2421af",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "pump.fun_board",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "Hf1TiBWuMdrohY4DcrRSbLYXbpw8sEKBKUb3f9fjpump",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.95,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784923355,
        "first_seen_ms": 1787784923355,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 39,
      "claim_id": "8239941edeec5417",
      "row_sha256": "21666b25d8d683206083d77ca6283ed5bc3fd001fb469ff428d3d308a1056741",
      "legacy_metadata": {
        "content_id": "d64c13b0bb86412d",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "54vp27ulaw4wnlo5n7r4fcc6zlamoqc28xbarjss4euj",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 40,
      "claim_id": "9b316b5a76639304",
      "row_sha256": "2a21e8820841bae542b332e6fb9e3a91215bb71f1185368fb6783404ff7b3366",
      "legacy_metadata": {
        "content_id": "279d5b4fda80aa16",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "4qftp5njxektbjmxyipdyp2hderwf2gyuenv1ypzrxzp",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 41,
      "claim_id": "acc10135055a0075",
      "row_sha256": "b39803a5a6e9b1d605f55359f87c64c1981cca44be1f30277c37f276cf2d3b52",
      "legacy_metadata": {
        "content_id": "b2ba7dfb9443b416",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "cfxijypyd5ykiizhl5jckuai6xkpgnagstcdrephdnwv",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 42,
      "claim_id": "cbb41df3603c6955",
      "row_sha256": "ff0b369faf3afb6c9a8687a92c15e022818ea0bdd616b0bc8f3cac4ac110cd23",
      "legacy_metadata": {
        "content_id": "c48392baad583809",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "g8kgi7aupex8evr8vmkrth9skev5bietwc33ujaiimgh",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 43,
      "claim_id": "80e0359053cb7a16",
      "row_sha256": "632ff31a0cddeca813ff48378df3752e203efbe7c35028608f52a32f08a0f7f1",
      "legacy_metadata": {
        "content_id": "b1a438937b7bfd40",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "5jm4gnwpt62kphmheet6rjzzfjvsfnxnqdpeugwp2u9q",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 44,
      "claim_id": "16ff1a16fbc7f8d9",
      "row_sha256": "c77537257aab74aac74d433503f89bab2bcc458af1c52ae89e3f2b4dbce2f302",
      "legacy_metadata": {
        "content_id": "a316f91cbb14ded7",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "ganrccgz1rsv7bctqnmwus7sbbp2ppe9sfybmieghv7g",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 45,
      "claim_id": "a2b2a61bdc4bca39",
      "row_sha256": "d9ac4de45b4f34a16e8398730771409bb108fc9b9dd543d4519f5fd96adf80aa",
      "legacy_metadata": {
        "content_id": "94042eef0a00b3ac",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "xxmgtoigju5vgvu4d1pmyjzl12gmzsrdv2gockw8pt9",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 46,
      "claim_id": "d8bf7d8495ac0134",
      "row_sha256": "1736935b466bcdd233a434e20ccda2be64048857c64e12dd2543c376505964fb",
      "legacy_metadata": {
        "content_id": "aa7c304f1a76fc25",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "6a3a8zpkmssqqbfmjqzc6p9tfraue9ulafqu9txp2hlo",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 47,
      "claim_id": "4b497aa451dfb214",
      "row_sha256": "93ba1112b40edb67acfc58108be62a3e00d767ebe66989f38feaf33ac0920603",
      "legacy_metadata": {
        "content_id": "501d4ecfe2f3af20",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "e2jdplaxzhkcug9djjqj2ewmuz3acjpkjnzmpkcea6mq",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 48,
      "claim_id": "88803339f1b9e7b3",
      "row_sha256": "d62f00320e8c5e0640b78382ddb36a8aa5d7bbae06ac0ee7e8de63f50cf7da3e",
      "legacy_metadata": {
        "content_id": "55e64022feea4bb2",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "httde6bsdc3y58owhqayqmersnmwnrzfvdewjymxgax3",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 49,
      "claim_id": "7da9f9fb35c5f59d",
      "row_sha256": "c602f1ef4078c73af558cca4b1aec2c9a847bf96836f3b6d8f748d54b750d98a",
      "legacy_metadata": {
        "content_id": "5b78374c754c93bc",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "dky4gebq4fm4zrff5wqefqryrwqqbe81hpakebc5edx9",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    },
    {
      "line": 50,
      "claim_id": "cdb9bd18ac8794f3",
      "row_sha256": "d1e5b6da34d121d1ddc193ca54a329b272dd78d539f9a74c861c45a000affa75",
      "legacy_metadata": {
        "content_id": "9c29d8b658778a3e",
        "schema_version": "1.1.0",
        "pipeline_version": "1.1.0",
        "run_uuid": "nv11_48e7f58d6d91",
        "account_handle": "dexscreener",
        "platform": "web",
        "entity_type": "mint",
        "primary_mint": "6vkqkcnqljqnlkaboykka6l5gpvsjy2bbwrmevsppjoj",
        "resolution_method": "direct_address",
        "resolution_confidence": 0.7,
        "admission_status": "BRONZE",
        "temporal_class": "AMBIGUOUS",
        "publish_time_ms": 1787784926661,
        "first_seen_ms": 1787784926661,
        "amplification_cluster_id": 0,
        "originality_confidence": 1.0
      },
      "context_eligible": false,
      "reason_codes": [
        "DECISION_CUTOFF_UNRESOLVED",
        "RAW_EVIDENCE_MISSING",
        "SOURCE_UNKNOWN",
        "SOURCE_CONFIDENCE_MISSING",
        "SPEAKER_CONFIDENCE_MISSING",
        "ENTITY_CONFIDENCE_MISSING",
        "RIGHTS_UNRESOLVED",
        "MINT_LINK_UNVERIFIED",
        "NUMERIC_SNAPSHOT_MISSING",
        "AVAILABILITY_MISSING",
        "ORIGIN_RELATIONS_MISSING"
      ]
    }
  ]
}
```
