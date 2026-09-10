# Prospective HLS clock snapshot receipt

## Outcome

One public, unauthenticated yt-dlp API extraction and one media-playlist fetch succeeded. Twitch reported source stream **320254509148**, exactly matching the requested source, live at observation. No recording restart, collector control, segment download, credentials, subscription, or commits. Worktree actually reports branch `task/north-star-build`, not `master`; branch was not changed.

## Exact artifact handles

- `D:/mev_bot-artifacts/north_star/development/hls_clock/20260909T182859284620Z_fce795df/receipt.json`
- `D:/mev_bot-artifacts/north_star/development/hls_clock/20260909T182859284620Z_fce795df/playlist_00.redacted.m3u8` (master)
- `D:/mev_bot-artifacts/north_star/development/hls_clock/20260909T182859284620Z_fce795df/playlist_01.redacted.m3u8` (media)

Original playlist bytes were hashed in memory, not persisted with secrets. URI lines and non-clock tags/attributes are redacted. Original SHA-256 values:

- Master, 9,199 bytes: `39198ba911d51f70c5befdd8009f41e3c0748eb5447a1f06eb2e0272470c014f`
- Media, 5,835 bytes: `965def85d54856fd6e9bb01ddeb5dae1ce177d9916666322c26169512c7e5e41`

The original acquisition **reported** four application requests and 17,763 decoded response-body bytes including extractor JSON. Its configured caps were 2,000,000 decoded bytes, eight application requests, 10-second socket timeout and a 60-second soft deadline, with retries configured zero. **Review found that the underlying yt-dlp requests handler automatically followed redirects:** those application counters and destination checks were not independently enforced on redirect hops or intermediate bodies. Do not reinterpret the old receipt as a verified wire-level request/byte/destination guarantee. The collector made no explicit media-segment requests; the old transport does not prove that a hidden redirect could not have reached one. The original evidence remains immutable. The offline-only fix below applies to future runs, not retroactively to this acquisition.

Media retrieval wall clock: start `1788978539023` ms, completion `1788978539284` ms since Unix epoch. Master start/completion: `1788978538855` / `1788978539023` ms.

## Exact publisher clock evidence

| Media sequence | Discontinuity sequence | Explicit PROGRAM-DATE-TIME | Unix ms | EXTINF ms |
|---|---|---|---|---|
| 0 | 1 | 2026-09-09T18:28:56.326Z | 1788978536326 | 2000 |
| 1 | 1 | 2026-09-09T18:28:58.326Z | 1788978538326 | 2000 |
| 2 | 1 | 2026-09-09T18:29:00.326Z | 1788978540326 | 2000 |

The actual playlist explicitly starts MEDIA-SEQUENCE at 0. It has **no DISCONTINUITY-SEQUENCE tag**; its initial value is the HLS default 0, incremented by the leading DISCONTINUITY to 1 for all three entries. ENDLIST absent. Three explicit anchors, 6,000 ms aggregate listed duration. These values were reported by the acquisition parser. Its original Decimal-based exactness guard was later found context-sensitive and could silently round other inputs; the historical result is not a general exactness certificate. The bounded string/integer replacement and offline regression evidence are documented below; they do not retroactively certify this snapshot.

## Hard limitations

- These are prospective **playlist/publisher timestamps**, not decoded media PTS, segment identity validation, or proof the listed material is Megga's content. Ad/slate substitution remains unverified; no segments were fetched to distinguish it.
- The final listed PDT is **1,042 ms later than local retrieval completion**. Publisher/local clocks were not calibrated. Do not silently certify clock accuracy or causality from these timestamps.
- The original `D:/mev_bot-artifacts/narrative/twitch_live/megga_320254509148.mp4.part` retains an **unknown UTC offset**. This snapshot provides no mapping to its byte positions or PTS and does not retroactively align it.
- One snapshot only, not an ongoing collector. No continuity proof across later playlists. Original secret-bearing bytes deliberately absent; original SHA is an acquisition commitment, not independently recomputable from the redacted copy.
- The immutable acquisition receipt predates added parser annotations distinguishing explicit/default sequence tags and unverified content identity. Those caveats are documented here; no network re-fetch or rewriting of original evidence was performed.

## Verification and scope

Parser tests were written and observed failing before implementation. Final focused suite: **10 passed** (synthetic exact timestamps/sequences, discontinuity reset, redaction/hash, precision rejection, offline/mismatch, hard byte budget, safe persistence, denial/no retry, and production transport segment blocking).

All three real artifact files were read back and scanned for URLs/token/signature query strings; scan passed. Reparsing the persisted redacted media playlist produced **identical anchors and segments** to the acquisition receipt.

An initial full North Star suite passed **1,080 tests**. After two added HLS tests, full suite returned **1,081 passed, 1 failed**: another worker's reservation test `test_checked_in_reservation_is_fixed_prospective_and_not_capture_evidence` encountered a changing reservation SHA. No reservation files were touched. Final HLS-only rerun: **10 passed**.

Owned repository files only:

- `tools/data-pipeline/src/north_star/hls_clock_capture.py`
- `tools/data-pipeline/tests/north_star/test_hls_clock_capture.py`
- `tools/data-pipeline/north_star/BUILD_HLS_CLOCK_RECEIPT.md`

No changes to `media_clock.py`, existing media collectors, original repository, or other workers' files. No commits.

## Offline review remediation (deleg_b3416ec9)

The full reviewer response was read from the session database (assistant message 153541, session `20260909_113009_9d6f52`), not inferred from the truncated delegation log.

### Transport

- The scoped adapter bypasses yt-dlp's automatically redirecting request director. It uses an isolated requests session with `allow_redirects=False`, `stream=True`, certificate verification enabled, HTTP adapter retries zero, no environment proxies/netrc or saved cookies.
- **Disabling redirects alone is insufficient in requests:** `Session.send` still prepares `Response.next` via `resolve_redirects`, which consumes the redirect body. A response hook rejects and closes every non-2xx response before this happens. No redirect destination is followed, including same-host/relative playlist redirects. No denial response body is read.
- Destination checks run on the original and prepared URLs and the returned response URL before body reads; a changed final destination is rejected. URL credentials, non-443 ports and fragments are rejected. Public Twitch GraphQL `/gql` and the existing Twitch/CDN HTTPS `.m3u8` allowlist remain the only destinations.
- Request-count, soft-time and remaining decoded-body-budget checks prevent dispatch once exhausted. Successful bodies pass through the aggregate bounded reader; reaching the exact byte cap conservatively fails without an extra EOF probe. Request slots include failed attempts; no hidden redirect/retry hop is permitted.
- Limits describe admitted HTTP requests and decoded body bytes delivered to the reader, **not total TCP/TLS/header/wire bytes or a process-memory/decompression sandbox**. HTTP libraries may buffer compressed/network data internally. The 60-second deadline remains soft, checked between blocking operations; a 10-second socket timeout is not a total DNS/operation-duration guarantee.

### Parser

- Negative, signed/non-decimal, oversized, duplicate and late sequence declarations fail closed. Sequence values and generated identifiers stay in the unsigned 64-bit domain.
- Duplicate PDT for one segment, duplicate EXTINF, URI without EXTINF, dangling PDT/EXTINF/discontinuity, misplaced discontinuities and tags after ENDLIST fail closed. PDT before or after EXTINF is supported only for that pending segment.
- Within one discontinuity, a subsequent explicit PDT must exactly equal the prior EXTINF-derived timestamp. Backward/overlapping **and forward-gap** contradictions are rejected; no implicit clock reset or invented drift tolerance. A proper discontinuity clears inference, and a later explicit PDT can establish a new publisher epoch.
- This is a strict **complete-segment clock subset**, not a general HLS conformance parser. Unsupported EXT tags fail closed, including master syntax, delta SKIP, LL-HLS PART/PART-INF/PRELOAD-HINT/SERVER-CONTROL/RENDITION-REPORT, GAP, and unimplemented extensions. KEY/MAP/BYTERANGE/DATERANGE/vendor tags are also outside the supported parsing subset. This conservatism may reject otherwise playable playlists; it must not silently promote an invented timeline.
- Supported header tags are MEDIA-SEQUENCE, DISCONTINUITY-SEQUENCE, VERSION, TARGETDURATION, PLAYLIST-TYPE and INDEPENDENT-SEGMENTS, before segment tags. EXTINF must have ordinary nonnegative decimal syntax and a comma; exact-millisecond checks still apply. Missing PDT remains unknown, not backfilled.
- Parsing uses original bytes, **not the redacted derivative**. Redaction is a disclosure filter, not a validity certificate: it erases unsupported semantics and must never be used to upgrade rejected originals into validated captures.
- Invalid/unsupported originals produce a safe gap receipt, not successful clock evidence. `unverified_may_include_ad_or_slate` and null original-recording UTC offset remain explicit. Stream-ID agreement does not identify the speaker or prove creator/ad content.

### Verification

- Baseline: **10 passed**. Regression RED: **62 failed, 18 passed**, reproducing all 39 malformed/unsupported parser cases, redirect body/hop defects, exhausted-byte dispatch, changed destination and malformed-playlist success.
- Initial GREEN: **80 passed**. Final focused HLS suite: **88 passed**. Additional offline boundary/GraphQL/body-cap checks and the adjacent media-clock suite: **154 passed**. Owned Python files passed AST parsing; owned-file whitespace and `git diff --check` passed.
- All HLS tests block socket connections and DNS via an autouse fixture. Transport tests mock below requests at `HTTPAdapter.send`, so actual requests redirect preparation and response hooks execute. Redirect matrix covers 301/302/303/307/308 against off-list, segment, allowed-playlist and relative destinations, asserting one dispatch, zero body reads and closure.
- Original receipt and both redacted playlists were hashed before/after this remediation and are unchanged. Read-only reparse of the historical redacted media preserves its exact three anchors/segments and 6,000 ms listed duration. This is a compatibility check, not validation of omitted raw tags or proof of media alignment.
- Original receipt SHA-256: `33781dc7a8379ba9bc56c6d67778d941d4c8559b5a5c1028985cb3f40e3ec62b`.
- No live network calls, new source observation, collector changes, commits or other repository edits were made for this remediation. Independent re-review is required before any new live capture.

## Offline timestamp exactness remediation

### Reproduced defects and supported contract

- Direct probes reproduced `exact_ms('1.0000000000000000000000000000000001') == 1000` and the rounded large duration `123456789012345678901234567890.123`. The old PDT guard likewise accepted a nonzero fractional longtail beyond Decimal context precision. Decimal construction was exact; subsequent multiplication/modulo was not. Changing global precision is not a fix.
- `fromisoformat` normalized malformed `+00:99` and `-00:60` offsets rather than rejecting their minute fields. `+24:00` was already rejected. The previous helper also admitted pre-epoch UTC values and `-00:00` without distinguishing that unknown-local-offset notation.
- Both helpers now reject non-string inputs and clock literals longer than **64 characters before regex/numeric conversion**. This is a conservative local subset limit, not a standards-wide bound. Ordinary unsigned ASCII decimal duration syntax is required; signs, exponent notation, underscores, Unicode digits and whitespace are rejected. Large in-bound durations remain exactly represented as Python integers, not capped or rounded to a smaller numeric type.
- Every fractional digit beyond the third must be zero. Trailing zeros remain valid within the size bound; any nonzero tail fails closed. Conversion uses only string inspection and integer arithmetic, independent of Decimal precision and traps. There is no float or subsecond `fromisoformat` conversion.
- PDT supports uppercase `T`/`Z`, explicit `+/-HH:MM` offsets with hours **00..23** and minutes **00..59**, valid Gregorian calendar fields in years **0001..9999**, and time fields **00..23:00..59:00..59**. Leap seconds, lowercase variants, offset seconds, and `-00:00` are deliberately outside this supported subset. These exclusions do not assert that every excluded form is invalid RFC3339.
- The prospective-capture subset now explicitly requires the **offset-adjusted Unix millisecond value to be nonnegative**. This is a local contract restriction, not an assertion that pre-1970 dates are invalid RFC3339. A 1969 local date that adjusts to the Unix epoch is accepted. Offset arithmetic and calendar-day deltas are integral and platform timestamp conversion is not used.

### TDD and verification

- Baseline: **88 passed**, unchanged transport/parser regressions included.
- Duration RED: **21 failed, 1 passed, 88 deselected**; duration GREEN with baseline: **110 passed**.
- UTC/integration RED: **15 failed, 143 passed**; final focused GREEN: **158 passed**. Combined HLS and adjacent media-clock suites: **224 passed**.
- Regressions exercise Decimal contexts at precisions 2, 28 and 80 with `Inexact`/`Rounded` traps enabled; exact large integers; 64/65-character boundaries; longtail fractions; invalid calendar/time fields; ASCII/type restrictions; epoch boundaries; and parser/redaction propagation. An exhaustive loop tests **20,000** signed two-digit hour/minute offset combinations against independent range and integer expectations. This expands the actual probes rather than claiming recovery of the truncated independent-review JSON.
- Owned Python AST parsing passed. `git diff --check` passed (Git emitted an unrelated sibling CRLF warning); owned files also undergo a direct whitespace check because they remain untracked. Tests retain socket/DNS blocking and mocked transport. No live network calls or new acquisition occurred.
- Original receipt and both redacted playlists were hashed before/after and are byte-identical. No historical playlist was reparsed with this replacement for certification, no artifact was rewritten, and the old recording UTC offset remains unknown. Earlier reparse statements above describe the prior remediation only.
- Only the three owned repository files changed for this timestamp fix. No collectors, original media, sibling implementation files, or commits were touched. Independent re-review remains required before live use.

## Reproduction (offline only until review)

`PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python -m pytest tools/data-pipeline/tests/north_star/test_hls_clock_capture.py tools/data-pipeline/tests/north_star/test_media_clock.py -q -p no:cacheprovider`

Do not invoke the live capture CLI until the fix is independently reviewed. Any subsequently authorized capture writes a new evidence directory. Never loop on denied/offline responses; those produce a safe gap receipt. Never use a fresh snapshot to backdate or align an existing recording.
