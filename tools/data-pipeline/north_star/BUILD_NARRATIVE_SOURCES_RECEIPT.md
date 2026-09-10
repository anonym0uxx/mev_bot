# Narrative source-span recovery receipt

## Current hardened transform: first50_exact_v2

Created and independently read back (using the reviewer's verification script,
with only the target/version assertions changed):
`D:/mev_bot-artifacts/north_star/development/narrative_sources/first50_exact_v2/`

- Manifest SHA-256: `4c809e8aefb13ee32ddbf95200f43c123b6df7235728444aab764b4404baa53a`.
- Recovered JSONL SHA-256: `f19c2e8b2f73facbeb3ff4bb0282656a7f22cc2d9e5149cc2135f341709f9c16`.
- Manifest-pinned transform: `narrative_sources_strict_v2`, source SHA-256
  `8ecb07d39d0cf1756cb9f6982a33c8723435aec3f30484fda4a086cf6f7617d7`.
- 151 listed files plus manifest verified; 50 exact UTF-8 prefix spans and 100
  original offset/length/hash pointers checked against original input bytes.
  Zero admitted rows; all availability/rights/source/temporal uncertainty remains.
- All 152 v1 files were hashed before/after the v2 build and remained unchanged.
  Both complete original inputs were hashed before/after and unchanged.
- V1 remains historical and immutable. Integration must continue consuming its
  already-reviewed v1 artifact until the hardened code receives final review.

### Four independent-review mustfixes addressed

1. Reject input/output symlinks and Windows reparse points, including ancestors,
   before any link resolution. Reuse `fs_integrity` without changing that helper.
2. Read each stream through `stable_reader`; snapshot both input identities before
   either scan and recheck both after each scan, before staging, and immediately
   before publication. Atomic replacement and observed in-place mutations fail
   closed. Source references still identify the scanned bytes, not mutable future
   pathname contents.
3. Complete all JSON validation and output serialization before staging. Stage in
   a new sibling temporary directory, check write lengths, and read every staged
   byte back before publishing. Windows `os.rename` refuses any existing target,
   including an empty directory arriving at the rename boundary. Failed writes
   remove staging and leave the final destination absent and retryable; concurrent
   existing targets are preserved. Other platforms fail closed rather than using
   POSIX's potentially overwriting directory rename.
4. Reject duplicate decoded keys at any depth, NaN/Infinity/-Infinity, finite-token
   exponent overflow (e.g. `1e9999`), invalid UTF-8, lone surrogate strings/keys,
   non-object records, and nesting beyond 64 containers. Every scanned row is
   checked, including ignored metadata and unselected content. Rejections use
   plain `ValueError("invalid JSONL record")`, before output creation. The lexical
   depth pass precedes recursive JSON decoding; existing byte/row/selected-byte
   bounds remain in effect. Valid surrogate pairs and finite floats are accepted.

IDs remain **literal nonempty Unicode data**, including whitespace/pathlike IDs;
no trimming, path interpretation, case folding, or Unicode normalization occurs.
Object/record filenames derive from SHA-256 only.

Supported boundary: caller-controlled trusted directory trees, not a hostile
filesystem sandbox. `fs_integrity` detects observed identity/size/timestamp changes,
not undetectable ABA, same-size timestamp-coalesced writes, hardlink aliases,
untrusted ancestor races, or mutations after the final check. Atomic visibility
is not a claim of power-loss durability. A process kill can leave hidden staging.

### Observed RED → GREEN and reproduction

- Strict recursive JSON: 48 failed, 35 passed → 83 passed.
- Symlink/reparse paths: 7 failed → 90 passed overall.
- Cross-scan source mutations: 8 failed → 8 passed (replacement plus deterministic
  timestamp-changing in-place writes). Windows can coalesce rapid same-size writes;
  tests explicitly advance mtime rather than claiming to detect an unobserved ABA.
- Publication/current-transform pin: 8 failed → passing. Added real-OS concurrent
  empty-directory/file tests, successful-count-but-corrupt readbacks, and the valid
  depth/escaped-string boundary as additional regression checks.
- Final targeted run: `python -m pytest tests/north_star/test_narrative_sources.py
  tests/north_star/test_narrative_context.py -q` → **207 passed**.
- `python -m py_compile` on both owned Python files → exit 0.
- Full `tests/north_star` run → **1789 passed, 3 failed**, all failures in the
  concurrently developed, unowned `test_cash_ledger.py` (missing `embedded_action_id`
  and `rent_id` constructor fields). No out-of-scope fix was attempted.

Reproduced all 26 independent adversarial probes in:
`C:/Users/Alon/AppData/Local/Temp/narrative_sources_hardening_v2/`.
`adversarial_results.json` reports strict-JSON failures without output, source and
output links rejected, source replacement rejected, and injected manifest failure
with no final directory/manifest, followed by a successful valid retry. Probe script
changes catch the now-expected source-replacement exception and inject failure at
the staged manifest; original review scripts/results were not modified.
`build_artifact.py`, `verify_artifact.py`, `build_verification.json`, and
`artifact_verification.json` contain reproducible real-artifact evidence.

Final independent code review is requested through the parent agent; this receipt
records reproduced evidence, not a claim that the hardened code is already reviewed.
No shared modules, original inputs, v1 artifacts, commits, network, capture, or
training were changed or invoked.

## Historical v1 result

Recovered **50/50 first legacy claim rows**, with **50/50 exact content-ID joins,
nonempty original legacy raw texts, and exact UTF-8 byte-prefix spans**.
There are 50 unique claim IDs and no selected duplicate/conflicting content IDs.
This is a development recovery artifact, **not admitted narrative context**.

Artifact directory:
`D:/mev_bot-artifacts/north_star/development/narrative_sources/first50_exact_v1/`

- `manifest.json` SHA-256: `65af14eb5a4707b7348fff0a81fe09728385e89a1ea056faef3dba8eb3287040`
- `recovered_sources.jsonl` SHA-256: `f19c2e8b2f73facbeb3ff4bb0282656a7f22cc2d9e5149cc2135f341709f9c16`
- 151 manifest-listed files independently read back and checked for byte length
  and SHA-256: recovered JSONL, 50 text objects, 50 original claim JSONL records,
  and 50 original content JSONL records. Manifest read back separately.
- All 50 text-object hashes, original content text bytes, exact claim-prefix
  bytes, content IDs, version pointers, and source-record offset/length/hash
  pointers were checked against the read-only original files.

## Inputs and bounds

Original root, read-only:
`D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1.1/gold/`

| Input | Read scope | Rows | Bytes | SHA-256 of read scope |
|---|---|---:|---:|---|
| `creator_claim_v1/creator_claim_v1.jsonl` | first-50 prefix only | 50 | 68,997 | `67674bb1e4ce8ade9ad82a6e99098695017e9eaf7c283d4d538f5de99bb4cca1` |
| `creator_content_v1/creator_content_v1.jsonl` | complete bounded stream | 1,471 | 4,556,118 | `ed8ce91fcae3e4ca4f4ee846372535f4f72e6be675ff4a939c00eeb57efafeaf` |

Full input hashes were also streamed before/after execution and were unchanged:
claim file `45077c0562b0cd56165d4d37ab894b04accef406993dce514700b4393a1a0f0f`;
content file hash as above. No original file was rewritten.

Execution limits: 50 claim rows; 2,000 content rows; 1,048,576 bytes per JSONL
record; 8,388,608 bytes per input stream; 8,388,608 retained selected-record
bytes. Actual retained selected-record bytes: 228,490. Content scan continues
through EOF even after every ID is found, detecting later duplicate/conflict
records. Overflow raises before output publication. Only selected content IDs
are retained; unrelated content is not materialized as a corpus.

## Semantics and remaining uncertainty

- Text objects are the exact UTF-8 encoding of legacy `raw_text` after JSON
  decoding, preserving Unicode, whitespace, CR/LF, and text content. They are
  **not claimed to be original provider-response bytes**. Original JSONL record
  bytes are separately copied and SHA-256 pinned, preserving escaping/newlines.
- `content_version` is explicitly a legacy-record SHA-256 version; legacy
  schema/pipeline versions and source hashes remain in nested metadata without
  being relabeled as provider verification.
- No normalization, fuzzy/substring search, generated prose, generated outcomes,
  timestamp repair, or synthetic evidence. Only a nonempty exact claim prefix
  receives a `BYTE` span. Non-prefix text can be recovered without a span.
- Duplicate claim IDs are retained and flagged, including conflicting claims;
  unique-ID counts prevent silently treating duplicate rows as unique claims.
  Duplicate content rows are flagged; any differing decoded content record
  (including timestamp/metadata differences) blocks arbitrary version selection.
  Original conflicting record pointers remain available. Conflict is not
  mislabeled as source text being unavailable.
- `ORIGINAL_PROVIDER_BLOB_MISSING` appears on all 50 recovered rows, scoped to
  **the supplied legacy rows only**, not a claim that no upstream blob exists
  elsewhere. Explicit blob references, when supplied, are only
  `UNVERIFIED_REFERENCE`; the adapter performs no provider/network lookup.
- Legacy timestamps and `EX_ANTE`/admission labels are preserved as legacy
  metadata, never promoted to proof. Every row has `availability_verified=false`,
  `available_at_ms=null`, `source_state=UNKNOWN`, `rights_state=UNKNOWN`,
  `temporal_state=UNKNOWN`, `admitted=false`, and `split=development`.
- Provider lineage, independently verified availability, rights, and source
  identity remain unresolved. Recovery does not grant training/admission rights.

## TDD and verification

Observed RED→GREEN cycles:

1. Missing adapter: 1 failure → 1 pass for exact-byte/pointer recovery.
2. Exact-only/missing-text cases: 8 failures + 1 pass → 9 passes.
3. Duplicate/conflict/provider-reference cases: 6 failures + 9 passes → 15 passes.
4. Bounds and identifier validation: 10 failures + 20 passes → 30 passes.

Final execution:

```text
python -m pytest tests/north_star/test_narrative_sources.py -q
30 passed

python -m pytest tests/north_star/test_narrative_sources.py tests/north_star/test_narrative_context.py -q
111 passed

python -m py_compile src/north_star/narrative_sources.py tests/north_star/test_narrative_sources.py
exit 0
```

Tests cover Unicode byte offsets, CR/LF, exact IDs, no Unicode/case/whitespace
repair, absent/empty/nonstring text, empty claims, non-prefix text, late
conflicts, metadata conflicts, duplicate claims, unverified blob pointers,
positive integer bounds (excluding booleans), byte/row overflow, bounded claim
prefixes, EOF hashes, empty streams, identifier type rejection, no input
mutation, and refusing to overwrite an existing artifact directory.

Reusable Python API:

```python
from north_star.narrative_sources import recover_sources

recover_sources(
    claims_path,
    contents_path,
    new_development_output_directory,  # must not exist
    claim_limit=50,
    max_content_rows=2000,
    max_line_bytes=1048576,
    max_input_bytes=8388608,
    max_selected_bytes=8388608,
)
```

Only owned repository files created:
`src/north_star/narrative_sources.py`,
`tests/north_star/test_narrative_sources.py`, and this receipt.
No shared modules, narrative-context changes, collectors, network calls, or commits.
