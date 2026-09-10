# NorthStar source-rights evidence reconciliation

**Result: exact source identity recovered; rights admission remains blocked.** No dataset was admitted or training source/index changed. Read-only evidence recovery cannot replace the binding permissive-license and source/license/token disclosure policy in master lines 343–357.

## Slinky: publisher revision proof, not a chosen license

- Publisher: `https://huggingface.co/datasets/Slinky21/Pumpfun_Memecoin_Corpus`
- Download metadata, current public commit listing, and exact publisher revision endpoint agree on `13bec2bf3089d56d50c35e602616d1b993bb37f5`.
- Original local `D:/repos/mev_bot/rust/data/slinky21_data/README.md` is **byte-identical** to the publisher's README at that revision: SHA-256 `0cd64ffd8c20b6aee2ecb70c956d47fa7a24e786f44c7c84f85b4c4fffbc7639`; Git blob SHA-1 `61622f3d21eadc5b4d9bd563bb00b8c5c3cd0a70`.
- **Both declarations occur in that same file:** line 18 says `license: mit`; lines 135–137 say CC BY 4.0. This is not a cache mismatch or different downloaded revisions. No separate LICENSE file appears among the 28 publisher files at the exact revision.
- Earlier revision `73e32a87e2b0744e1fabb08482d30a24f6f5d3a1` says MIT in YAML but “To be finalized” in the License section. Revision `808c31cd3b30734e7e933b2bc2abf7a00d39ed9e` says license-tbd; revision `ed93c84ddb47199f7370dbae4a95746191160274` already contains the present conflict. These exact historical files do **not** establish a later resolution, license election or sublicensing authority.
- `KNOWN_ISSUES.md` also matches publisher bytes exactly: SHA-256 `82534ee226e1447e5ab0c0eb4b1b5453f0241aa5426d561b8f4b00314d1da299`. It documents quality limitations, not a grant. Its chain/identity assertions were not independently validated in this rights audit.
- All **24 actual local Parquet payloads** were streamed and SHA-256 matched to publisher LFS object hashes at the pinned revision. All **4 non-LFS files** match publisher Git blob hashes. All 28 sizes match. Original small files and artifact-store large files are individually located and hashed in JSON; no redownload was necessary.

### Exact publisher statements retained

README lines 131–137:

> - Collected via websocket + on-chain RPC polling of Solana, plus
>   concentration/holder data from a third-party API.
> - No PII beyond public, pseudonymous on-chain wallet addresses and
>   user-submitted token names/symbols.
> - **License**: CC BY 4.0. You are free to use, share, and
>   dapt this dataset for any purpose, including commercially, as long as you give appropriate credit — cite this dataset
>   (see Citation below) and indicate if changes were made.

The spelling `dapt` is preserved rather than repaired. The specified citation is:

> Slink Dev (slink21taken). PumpFun Launch Corpus. 2026.
> Research papers will be released later on.

These are **observed declarations**, not a determination that CC BY 4.0 overrides MIT. The selected effective license, exact grant scope and unnamed third-party API rights remain `UNKNOWN`. No rights to the README's proposed paid live service are inferred.

### Operator report is preserved, not downgraded or inflated

The task reports permission granted without grant text. Historical `STAGE2_AGGREGATION_INVENTORY.md` lines 20–21 say:

> License: **CLEARED** — operator contacted slinky; public hosted dataset, use authorized.
> (README's MIT-vs-CC-BY inconsistency is superseded by direct publisher authorization.)

This is evidence of the operator's report, **not the publisher's grant text**. It does not identify permissive terms, exact version, attribution, permitted redistribution or authority over the third-party API data. Its historical CLEARED assertion is not inherited as current admission. A supplied authentic grant could change this conclusion; nothing here disputes that a conversation occurred.

## KOL, Twitch and narrative: exact existing inputs, no speculative discovery

The bounded search covered **96 metadata/handoff documents**, **23 existing raw acquisition JSONL files / 13,853 rows (metadata keys only)**, and **all 1,471 stored narrative content rows**. The JSON enumerates files, hashes, scan scope, matching document lines and every content ID/row-byte hash. No private email, messages, credentials or private groups were searched.

- **No explicit permissive grants were found in that scope.** None of the scanned raw/content records had explicit license/permission/grant/rights/attribution metadata keys. Absence here does not assert a grant cannot exist elsewhere.
- The 1,471 content records belong to **34 platform/account groups**; each has its own `UNKNOWN` rights entry. Handles are preserved as metadata assertions, not authenticated licensor identities. Missing original URLs/versions remain `UNKNOWN`, with exact local content IDs and bytes available for binding.
- The five existing KOL research entries retain exact YouTube URLs and their recorded retrieved-text hashes; the handoff itself labels rights `UNVERIFIED`. Those text hashes are **reported handoff values**, not newly rehashed provider payloads. The separately excluded starwifpump attribution is not reintroduced.
- Twitch capture context names `https://www.twitch.tv/megga`, stream ID `320254509148`; it has no permissive grant. The source media hash is explicitly attributed to the existing ASR receipt, not claimed rehashed here. Automated ASR, full-ASR aggregation and candidate recovery do not create rights in source audiovisual material, music, guest speech or embedded content.
- KOL **on-chain decisions derived from Slinky** inherit Slinky's unresolved source rights; they are not the same source as KOL speech/interviews. A Slinky dataset grant cannot clear Twitch or narrative content merely because the same wallet/name appears.
- Raw acquisition manifest currently reports zero events/files, while the bounded direct file scan finds the records above. It is pinned as observed metadata, not treated as an exhaustive inventory or a license. Candidate recovery of 1,471 records is not training admission or a token count.

## Minimum evidence the user must provide

1. **The already-received Slinky grant text**, with publisher provenance/identity and exact corpus/revision scope, resolving which permissive license applies and its attribution/change-notice terms. Alternatively provide an authoritative publisher revision/notice doing so. “Permission granted,” public hosting or choosing the more convenient README label is insufficient.
2. **Third-party concentration/holder-data authority or exclusions.** Evidence must establish the publisher can license those included fields, or identify what is excluded. A rights-clean subset is possible only after a separately verified field/dependency boundary—not by renaming a source “facts.”
3. **Permissive grants for the enumerated existing KOL/Twitch/narrative materials**, covering the exact work/session/version, licensor authority, intended collection/transcription/transformation/training/redistribution scope, attribution and third-party exclusions. A blanket grant must expressly cover every included work. Private-training-only or platform display permission does not meet this policy.
4. **Explicit inclusion approval after the exact source/license/token disclosure.** This is separate from the license grant, technical task eligibility, training-run approval and live-order approval.

## Technical work that is not a user permission decision

The exact Slinky revision lookup and all downloaded-file hash reconciliation are complete. Remaining technical work is to bind supplied grants to these hashes; authenticate missing narrative source versions/owners; propagate rights into derivatives and retrieval; compute actual proposed post-mask per-source token counts; and independently validate source, availability, identity, splits, semantics, economics and whole-corpus admission. Technical progress should continue only within its independently authorized stage; it cannot turn UNKNOWN rights into PASS.

No source/license/token report is complete yet because the effective licenses and exact training token counts are unknown. All 41 rights-table entries remain blocked, with zero newly admitted sources or records.

## Deliverables and verification

- `SOURCE_RIGHTS_EVIDENCE_RECONCILIATION.json`: 41 source-rights entries; complete 28-file Slinky hash table; exact public HTTP response bodies/hashes/URLs/retrieval times; historical license evidence; 96-document scan manifest and rights hits; 23-file metadata scan; 1,471 exact content bindings; minimum user evidence and separate technical follow-up.
- JSON SHA-256: `fc4fdba37e7bd4e25f5c3eb784214ac8965e55abcd53ef0eeb4f4068079c0239`.
- Assertions executed before publication: all publisher/local file hashes and sizes match; response-body hashes match; 1,471 unique content IDs equal summed account-group counts; all rights entries remain UNKNOWN/blocked. JSON writer verified stored bytes and syntax.
- Only these two reports were written in the task worktree. No original source, grant, master, registry, candidate export, training loader or unrelated agent file was modified; no contact, paid call, training or commit occurred.
