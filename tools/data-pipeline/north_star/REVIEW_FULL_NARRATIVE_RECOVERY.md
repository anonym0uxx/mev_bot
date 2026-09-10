# Independent FULL1471 narrative recovery review

## Verdict

**PASS: complete, byte-exact recovery of the supplied frozen content/claim pair. NOT semantic approval, creator-argument coverage, provider-source completeness, full G coverage, or training admission.**

Independent read-only Python verification consumed the actual original JSONL files, every recovered row, every pointer and text object, the freeze marker, manifest, seal and per-record report. It did not import or execute the recovery adapter or the full-recovery runner. This reviews the new full output, not the reusable module. No network, generated training labels, teacher prose, translation, source edits, commits, or admissions. The sole authored deliverable is this review.

Artifact root: `D:/mev_bot-artifacts/north_star/aggregation/narrative_legacy_v1`.
Original root: `D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1.1/gold`.

## Exhaustive mechanical checks: actual execution passed

| Check | Independently observed result |
|---|---:|
| Original claims scanned to EOF | 1,471 |
| Original contents scanned to EOF | 1,471 |
| Unique claim IDs / unique content IDs | 1,471 / 1,471 |
| Exact one-to-one original claim/content/output joins | 1,471 |
| Exact UTF-8 evidence spans against original claim text and original content | 1,471 |
| Original-record pointers checked | 2,942 |
| Distinct stored text objects | 1,470 |
| Adapter files checked, including manifest | 4,414 |
| Seal children checked against actual bytes and SHA-256 | 4,419 |
| Complete version files, including seal | 4,420 |
| Per-record reports joined and non-approval flags checked | 1,471 |
| Protected files matching preflight before-hashes and byte sizes | 308 |
| Missing joins, duplicated IDs, missing records, padded records | 0 |
| Source-recovery rejections | 0 |
| Current admitted rows | 0 |

For every pointer, checked original path equality, independently calculated line number and cumulative byte offset, byte length, SHA-256, exact original JSONL bytes including line endings, and corresponding content-addressed record file. For every text object, checked original decoded `raw_text` UTF-8 bytes, stored file equality, size and content-addressed hash. For every span, checked integer byte bounds, `span_unit=BYTE`, exact sliced equality to original `claim_text`, content ID/version and object hash/path bindings. All source and output line orders also match.

Compared ID multisets, not just aggregate counts; every original claim references exactly one original content, and both multisets match output exactly. Full original JSONL record retention includes `normalized_text`; convenience content metadata omits only `normalized_text` and `raw_text`, and every retained metadata field equals its original value. No source metadata was interpreted as new approval.

Verified both manifest and seal file maps against file bytes and independently enumerated actual files. No unexpected, missing or unreferenced adapter files. The seal itself matches its receipt pin. The 308 protected-file matches include originals, marker, adapter, and first50 artifacts; this is a current comparison to the recorded before-hashes, not an independent observation of the historical execution interval.

Exact-text storage deduplication only: content lines **1369 and 1383** share `objects/80967e71dd17afc94a631b145bde0075796c9ea73d3c6900994811cbfc674589.utf8`. Both IDs, claims, source record files and metadata survive. No semantic deduplication, loss or padding was performed by this recovery.

## Frozen source and artifact handles

SHA-256 values independently computed from actual files:

| Handle | Bytes / rows where applicable | SHA-256 |
|---|---|---|
| `creator_claim_v1/creator_claim_v1.jsonl` under original root | 2,353,008 / 1,471 | `45077c0562b0cd56165d4d37ab894b04accef406993dce514700b4393a1a0f0f` |
| `creator_content_v1/creator_content_v1.jsonl` under original root | 4,556,118 / 1,471 | `ed8ce91fcae3e4ca4f4ee846372535f4f72e6be675ff4a939c00eeb57efafeaf` |
| `FREEZE_MARKER.json` under original root | Original marker | `bd099d34dd7f7021d39dae7ec6caa22143fda2d27428b932302c55938a7c3f76` |
| `ARTIFACT_SEAL.json` under artifact root | Seal | `0d23fd6f1cd465b042439273572862b07cf863d86d81b065836364234ac4bb25` |
| `source_recovery/manifest.json` | Manifest | `4aba932c387869696b700bcfe0f75b63560b1030f6579e3c70b2fe2f7608193f` |
| `source_recovery/recovered_sources.jsonl` | 1,471 rows | `15e6a1f5c60faaec8dd631000f5685ec0336d2742e5f09ad7e2c17761a2742e8` |

Actual original row counts and entire-file hashes equal the freeze marker's `layer_counts` and `file_hashes_sha256`. The manifest's claim scan still says `selected_prefix` and `scan_complete=false`; independent EOF scanning, not that flag, establishes full supplied-file coverage. Historical freeze certification/admission is not adopted.

## Original platform/content composition, not generated semantic labels

This is an inventory of existing metadata, checked against stored text form. It does not create row labels, claim validity, creator identity, thesis quality, rights, or causal timing.

| Original platform | Original content_type | Rows | Actual stored form and limits |
|---|---|---:|---|
| telegram | message | 1,167 | Telegram message/page Markdown strings; all contain `https://t.me/`. Includes navigation, avatar links, multiple-post pages, brief calls, mint-only messages and forwarded material. Not 1,167 independently validated creator arguments. |
| web | chart | 137 | Every string is exactly `DexScreener listing: ` plus its stored `primary_mint`; no chart image or creator explanation recovered. |
| web | tracker_alert | 32 | Every string is exactly `Pump.fun board listing: ` plus its stored `primary_mint`; not creator reasoning. |
| youtube | video | 46 | 44 `account_handle=youtube_search` title/search cards, each starting `### ` and containing views metadata; two other stored prose/transcript-like texts (`gravy`, `HowToCrypto`). No video/audio or transcript-to-media alignment verified. |
| twitch | video | 11 | Three clip-title strings, four live-title strings, four serialized profile JSON objects. No spoken creator narrative recovered from these rows. |
| twitter | post | 76 | Stored post strings. Some contain reasoning, others brief observations or promotion; all remain semantically unreviewed. |
| web | post | 2 | `web_crawl`, `account_id=https://padre.gg`: a 63-character heading and a 215-character marketing paragraph, not two trading arguments. |
| **Total** | | **1,471** | Exact inventory, with no discarded records. |

Disjoint form accounting: **224 listing/title/profile records** = 137 DexScreener + 32 Pump.fun + 44 YouTube search cards + 3 Twitch clip titles + 4 Twitch live titles + 4 Twitch profile JSON objects. The other records are **1,167 Telegram + 76 Twitter + 2 YouTube non-search prose texts + 2 website fragments**. Together these exhaust all 1,471. The remaining records are not automatically genuine creator arguments; their argument-level count is **not established**.

The first exploratory Twitch form assertion (all clip titles) failed against actual data. Inspection of all 11 rows established the corrected exhaustive 3/4/4 split above. This was a review assumption corrected from evidence, not a recovery defect.

### Important semantic limitations exposed by actual bytes

- **All 1,471 legacy claim texts equal `original_content.raw_text[:500]` in Python Unicode characters.** 296 equal the entire stored text; 1,175 are shorter prefixes. Thus exact span recovery validates deterministic legacy prefixes, not 1,471 extracted arguments. The recovered object still retains the entire supplied stored text.
- For content/claim/output line 1, claim ID `5a568ffdf4b307b5`, content ID `ce4eb4dec2ed644a`, the exact evidence `[0,500)` bytes consists of Telegram navigation/avatar/channel-link Markdown, not the purported trading thesis. Its full object is `source_recovery/objects/e617a2605f0c4e26ba6258f67f18716cc440d66900d61ce22e3a098a4f5b9c28.utf8`.
- **59 Telegram strings are exactly 10,000 Unicode characters.** Visible examples end mid-link or mid-markup. Recovery preserves their existing boundaries; it cannot assert original provider-page completeness or restore upstream truncation.
- Original metadata is not identity proof. For example, content line 400 (`8b73314e1a95c615`) has `account_handle=alphacalls` while the stored page names/links `Mduz Calls` / `mduzcalls`. Preserve this discrepancy; do not silently rewrite identity or count it as verified creator attribution.
- Historical `claim_type=narrative`, `quality_label`, `usefulness_class`, `GOLD`, and other legacy labels do not turn listing strings, profile metadata or navigation prefixes into creator reasoning.

### Inspectable composition examples

Line handles below are 1-based in **both original JSONL files and `source_recovery/recovered_sources.jsonl`**, verified rather than assumed. Each recovered row resolves to both exact original record files and the full text object's SHA-256 path.

| Lines | Form / exact content ID examples |
|---|---|
| 7–38 | Pump.fun listings; line 7 `afc2f5fbfc3e55d2`, claim `d64395af728f5264` |
| 39–175 | DexScreener listings; line 39 `d64c13b0bb86412d`, claim `8239941edeec5417` |
| 176–219 | YouTube search cards; line 176 `69adc5833bde7510`, claim `af89b3f6444d80de` |
| 220–222 | Twitch clip titles; line 220 `5b1cc8e90048f094` |
| 223–226 | Twitch profile JSON; line 223 `6f40170eaa9a9ca0` |
| 227–230 | Twitch live titles; line 227 `a01306bd24dd7604` |
| 231–232 | Website marketing fragments: `0d66c7fa5d5bb140`, `a8e1b17a0f8db5bf` |
| 1–6, 233–1393 | Telegram strings; line 233 `dfb6ba1bc38482af` contains a mint-only message within page markup; line 236 `07a6014a3e4bbdb3` contains a short risk statement |
| 1394–1469 | Twitter posts; line 1394 `ef2c7b7c77de0462`, claim `85c96acd0d704350`, contains risk-management advice |
| 1470 | YouTube `gravy`, content `d5e2aa21055826c9`, claim `01e5ff128cdf1e68`: 14,025-character stored trading narration; object `objects/e84daa879721a7e7d2c30d05fb3c9a294629a9b5de6b37273a86322ec378489b.utf8` |
| 1471 | YouTube `HowToCrypto`, content `ace86e0e1e3d5c15`, claim `465fe825c8f622c5`: 428-character stored psychological/profit-taking passage; object `objects/bf8996e3b37a7202011d174eb9818623910bd9b278685fd47837016bc747d4a4.utf8` |

## Admission and scope boundaries

Every recovered row was checked as `split=development`, `admitted=false`, `availability_verified=false`, `available_at_ms=null`, with `source_state=rights_state=temporal_state=UNKNOWN`. Every row has `original_provider_blob.state=MISSING` and exactly `ORIGINAL_PROVIDER_BLOB_MISSING` as its reason code. Every report row denies semantic approval, rights approval and training admission. **All 1,471 remain unadmitted; master rights remain pending.**

No loss or padding is established relative to the supplied frozen content/claim pair, not relative to unseen original provider responses or the full desired corpus. The freeze marker additionally lists states 1,183, strategy 97, trajectories 490 and validations 615; those layers were not recovered or verified by this bounded review. Full G coverage, missing creator coverage, provider media/response recovery, argument-level evidence, temporal availability, rights, and admission remain separate unresolved work.
