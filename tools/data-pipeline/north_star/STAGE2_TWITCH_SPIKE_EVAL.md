# North Star — Twitch/Video Transcript Spike Evaluation

> **Historical record, not current completion evidence.** Stage numbering and several
> status/semantic claims below were superseded by the master-DAG audit. Consult
> `00_HOLISTIC_CONTEXT.md`, `BUILD_PLAN.md`, and task receipts before implementation.
> In particular, pseudo-tests, heuristic GOLD/EX_ANTE labels, and old PASS reports
> do not certify North Star admission. Original content is retained for traceability.

Spike goal: prove the Twitch VOD→transcript pipeline and measure whether Megga's
live-stream audio yields decision-reasoning signal worth ingesting for D07/D11.

## Result
| Metric | Value |
|---|---|
| Source | Megga VOD `v2866050335` "I hit the MILLY PNL | trading memecoins" |
| Audio span | 6h01m (21,669s) |
| Download | 557 MB (worstaudio), ~14 min, no auth |
| Transcription | faster-whisper large-v3, GPU — **13.4× real-time** (27 min) |
| Output | 6,153 segments / 33,211 words |
| **Reasoning yield** | **21.0%** of words (stable vs 21.3% on a 45-min slice) |

## Quality findings
1. **Transcription is clean.** Correct jargon ("sold an 8 and not a 7", "is he rug",
   "spawn a 12k"), proper nouns ("kimchi's wallet", "soul narrative dev"). large-v3 on
   GPU is accurate enough as-is.
2. **Reasoning is balanced across the decision lifecycle** (segment coverage):
   ENTRY 304, EXIT 293, MCAP/price 172, HOLDER/distribution 124, DEV/rug 121,
   NARRATIVE/meta 82, RISK/sizing 53. Entry+exit dominate — real action, not fluff.
3. **Trading activity is temporally clustered**, not uniform — dense buckets
   (2.0h–5.0h at 17–40 ticker-segments/30min) separated by lulls (1.5h=1, 5.5h=6).
   → decision-episode windows are extractable.
4. **Coin attribution is the weak link.** A naive ticker regex catches mostly false
   positives (BNB/XRP/QQQ/NASDAQ are chatter) because the streamer names coins by
   *narrative label* ("the goat", "otto", "soul", "kimchi"), not ticker. Real coin
   linking needs name→mint resolution against the token registry (slinky21's 798K
   tokens) + GMGN-style name lookup, NOT regex.

## How to leverage (design guidance)
- **D11 decision episodes**: slice dense temporal windows, align timestamps to Megga's
  on-chain trades (his wallet in slinky21), extract (coin, rationale, action,
  conviction) triples. The stream narrates exactly the reasoning behind each trade.
- **D07 narrative meaning**: "soul narrative dev", "narrative meta", "rotation" talk
  is raw market-thesis labeling — feed the narrative layer directly.
- **D13 numeracy**: mcap reads, position sizing ("80s", "8 vs 7"), "spawn a 12k" —
  real trader numeracy vocab.
- **Required pre-processing** (production, not this spike):
  1. Speaker diarization (Megga vs chat vs guests) — single-channel mix dilutes signal.
  2. Name→mint resolution against token registry (replaces naive ticker regex).
  3. Decision-episode segmentation on temporal density + silence + "now we buy X".

## Constraints
- Forward-only: 14-day VOD retention → live corpus going forward, no Jun–Jul backfill.
- 79% is banter/chat/music → the classifier/segmentation pass is the value-add, not raw
  Whisper output.

## Verdict
**Worth integrating as a recurring lane**, gated on the three pre-processing steps
above (they turn 21% raw signal into attributable decision episodes). Without name→mint
resolution the transcript can't be joined to on-chain truth, which is the whole point.