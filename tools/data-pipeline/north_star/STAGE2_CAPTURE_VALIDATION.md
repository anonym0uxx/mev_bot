# North Star — Stage 2 Capture Closure & Validation

Records the final closure of Stage 2 capture gaps: the LaserStream integrity
validation (D01), the Reddit lane fix, and the honest status of the remaining
social/streaming lanes. Frozen for Astra's Stage 1–2 review.

## 1. D01 — LaserStream raw capture: VALIDATED (99.87% complete)

Validation script: `src/validate_laserstream.py` (hashes compressed bytes vs. the
manifest — no decompression needed).

| Check | Result |
|---|---|
| Raw parts present | **368 / 369** |
| Events file (60 GB ndjson) | present + size + SHA256 match |
| Size mismatches (present files) | 0 |
| Hash mismatches (size-ok files) | 0 |
| Missing | `part0368.ndjson.zst` (18,142,012 bytes = **0.128%** of raw) |

- Manifest census: 18,425,391 raw records / 4,243,995 events across 369 parts
  (14.18 GB), window 441,326,362–441,375,694 (299 min), dup=0 decode_fail=0
  gaps=[].
- The single missing file is the **capture tail** (last partial part). Loss is
  0.128% — negligible for training; documented rather than re-captured.
- Orphan `...20260823_133256_000398_part0004.ndjson.zst` = abandoned earlier run,
  NOT part of the Aug-24 capture. Left as-is.

**Verdict:** capture complete and integrity-verified to 99.87%.

## 2. D07 — Reddit lane: FIXED (was IP rate-limited)

Root cause: Reddit's anonymous `.rss` feed rate-limits to ~1 req/min/IP; the cron
hit 3 subreddits/run and tripped intermittent **HTTP 429** (confirmed across all
User-Agents → IP-level, not UA-based).

Fix (`stage2_social_capture.py`, deployed to Hermes scripts dir):
- Pull **one** subreddit per run (rotating by hour), not three.
- **Self-cooldown 30 min** after any 429 (state file), so the shared IP is never
  hard-blocked.

## 3. Remaining social lanes — honest status

| Lane | Status |
|---|---|
| Telegram (8 public channels) | ✅ wired + working (33/34 scrape, 1 gap: PikalosiLounge). Inherently low-signal (call-channel spam ~77% TOOL_WALLET_SIGNAL). |
| Wayback X archive (14 handles) | ✅ wired + running on cron. |
| Twitch (7 streamers) | ✅ metadata only (clip titles, identity). Video reasoning → no transcript (clips are 6–20 s). |
| YouTube reasoning transcripts | ⛔ **blocked** — see below. |

## 4. YouTube reasoning-transcript lane: BLOCKED (documented, not faked)

- Cented `twitch_youtube` ID resolves to a **Fortnite** channel — one relevant video
  (`4euyrV50ovw` "How anyone can make $50,000 trading memecoins"), and that video's
  subtitles are **disabled** (`TranscriptionsDisabled` from the API).
- Megga `twitch_youtube` ID returns **404** (stale/invalid).
- Consequence: the spoken-reasoning content these streamers publish is not
  capturable via the transcript API. Recovering it needs (a) re-resolving live
  channels and (b) Whisper transcription — a Stage 3+ build, not a capture fix.

Seed registry (`narrative_seeds_v1.yaml`) updated to mark both channel IDs stale so
this is not re-attempted blindly.