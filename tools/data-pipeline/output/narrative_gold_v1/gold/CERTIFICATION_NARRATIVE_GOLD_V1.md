# CERTIFICATION REPORT — narrative_gold_v1

**Status:** CERTIFIED_WITH_LIMITATIONS — FROZEN_IMMUTABLE
**Date:** 2026-08-29 (Pacific Time)
**Certified by:** Hermes Agent (glm-5.2)
**Git SHA at freeze:** cf97c33

---

## 1. Scope

Narrative scope remains tightly **Pump.fun/PumpSwap/Solana memecoin-specific**. Sources are alpha-caller Telegram channels (PikalosiCalls, Marlonalpha, alphacalls, jacalcooks, solanaalpha) covering: narrative/meta formation, launch selection, dev/wallet behavior, bundles, social-account/site reuse, OG-vs-beta, precursor wallets, crowding/copytrade adverse selection, entry/exit reasoning, and live thesis revision.

**Creator claims remain HUMAN HYPOTHESES, never truth.** They are validated against Slinky/LaserStream outcomes/economics whenever causal timestamp + entity resolution permit.

---

## 2. Data Quality Summary

| Layer | Records | GOLD | REJECTED | UNRESOLVED |
|-------|---------|------|----------|------------|
| raw_social_event_v1 | 14,238 | — | — | — |
| creator_content_v1 | 1,506 | 1,136 | 12,732 | 370 |
| creator_claim_v1 | 1,211 | 431 | 427 | 353 |
| narrative_state_v1 | 1,221 | — | — | 1,212 |
| narrative_validation_v1 | 374 | 3 | — | 371 |

### Claim Type Distribution
- entry: 213, exit: 64, risk: 75, narrative: 800, observation: 45, regime: 14

### Modality Distribution
- prediction: 258, warning: 95, opinion: 569, call: 236, fact: 53

### Temporal Labeling
- EX\_ANTE (genuine predictions): 252
- EX\_POST (post-hoc): 67
- AMBIGUOUS: 892

---

## 3. Slinky v3 On-Chain Validation

**Overlap: 6 of 147 Pump.fun claim mints (4.1%)**

This low overlap is **structural, not a bug**:
1. Slinky v3 captures 622,870 Pump.fun mints from June 5 – July 14, 2026
2. Alpha callers discuss only 147 unique Pump.fun mints across all time
3. Only 7 of those 147 fall in Slinky's June-July capture window
4. Of those 7, **3 match Slinky (43% hit rate within-window)**
5. The remaining 3 overlapping mints are from August (outside Slinky's nominal window but still captured)

**Validation verdict:** 3 MIXED, 371 UNRESOLVED. No test success was fabricated.

---

## 4. Bugs Fixed During Certification

| Bug | Impact | Fix |
|-----|--------|-----|
| Mint case lowercasing | All Solana addresses lowercased → 0 Slinky matches | Use raw\_text (original case) for mint extraction, normalized\_text only for keyword matching |
| Publish time stub | All content tagged with retrieval date (Aug 26-29) → temporal misalignment | Parse TG page date headers (Month DD, YYYY) + message time stamps |
| alphacalls parser | Forwarded messages with different channel names missed | Match any t.me/<channel>/<id> pattern, not just current channel |
| Marlonalpha parser | "viewsText, [HH:MM]" format not recognized | Flexible regex accepting text between "views" and time bracket |
| Validation parquet scan | Only first 20 of 134 files loaded | Scan all 134 parquet files for full Slinky mint coverage |

---

## 5. Limitations (HONEST)

1. **Low Slinky overlap (6 mints):** Alpha callers discuss a tiny fraction of Pump.fun mints. This limits on-chain validation coverage. 371 of 374 validations remain UNRESOLVED.
2. **AMBIGUOUS temporal labeling (892/1211):** Cannot determine whether many claims were made before or after the event they reference.
3. **Priority trader coverage gap:** Pr6sper, Cupsey, Cented, DVCS, Meggga personal TG channels are private/empty. Their reasoning appears indirectly through alpha channels (PikalosiCalls, alphacalls) but not as direct first-person sources.
4. **No X/Twitter content:** Firecrawl could not reliably scrape X profiles for trader content.
5. **August 2026 retrieval-time artifacts:** Some events still carry retrieval timestamps rather than true publish dates (5,995 events from earlier scrape before date fix).

---

## 6. Frozen Corpora Integrity

| Corpus | Status | Verified |
|--------|--------|----------|
| rust\_gold\_v1 | FROZEN\_IMMUTABLE (FREEZE\_MARKER.json) | ✅ No tracked changes |
| laserstream\_gold\_v3 | FROZEN (cf97c33) | ✅ No tracked changes |
| slinky\_gold\_v3 | FROZEN | ✅ No tracked changes |
| narrative\_gold\_v1 | FROZEN\_IMMUTABLE (FREEZE\_MARKER.json) | ✅ New freeze marker |

---

## 7. Export Constraint (recorded for future Qwen exporter)

**repo\_state\_leakage prevention:** For each held-out repair/task, the Qwen exporter MUST construct evaluation against the narrative state available immediately BEFORE that task/commit, or exclude overlapping current-snapshot narrative from its training context.

---

## 8. Certification Decision

**CERTIFIED\_WITH\_LIMITATIONS.** The corpus is frozen immutable. Validation truth is preserved exactly:
- Slinky overlap: 6/147 (4.1%) — not inflated
- 371 UNRESOLVED validations — not converted to PASS
- No fabricated test success

**STOP: Do not generate CPT/SFT exports.** Report all four frozen corpora together for curriculum/export design.

---

## 9. Target Architecture (FUTURE DESIGN INTENT — recorded separately)

See `TARGET_ARCHITECTURE_PUMP_FUN_QWEN.md` for future design intent. This document records **historical repo truth only** and does not relabel historical code.
