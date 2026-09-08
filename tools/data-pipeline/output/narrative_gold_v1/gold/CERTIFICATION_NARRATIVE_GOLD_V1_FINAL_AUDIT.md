# FINAL READ-ONLY SEMANTIC CERTIFICATION
## narrative_gold_v1 — August 29, 2026 17:00 PT

**Auditor:** Hermes Agent (glm-5.2)
**Mode:** READ-ONLY. Frozen corpus NOT mutated during audit.
**Directive:** QUALITY > QUANTITY. If material defect found, preserve frozen v1 and create new version.

---

## EXECUTIVE SUMMARY

**VERDDICT: CERTIFIED_WITH_MATERIAL_DEFECTS — FROZEN v1 preserved as-is. Version as `narrative_gold_v1.1` recommended if rebuild desired.**

The corpus contains substantive Pump.fun memecoin reasoning from alpha callers but has **7 material defects** that must be addressed before Qwen export:

1. **FREEZE INTEGRITY VIOLATION** — Gold layers rebuilt 3.8h AFTER declared freeze time
2. **MINT LINKAGE BROKEN** — 0/1211 claims and 0/1212 states have mint data (field empty)
3. **STRATEGY CARDS DEVOID OF SEMANTIC CONTENT** — 26/26 cards: no EV, no win rate, no failure modes, all validations unresolved
4. **NO ENTITY CONFIDENCE TRACKING** — 0/1211 claims have entity_confidence field
5. **NO ORIGIN→AMPLIFICATION STRUCTURE** — 0/1506 content records have parent/origin linkage (37,982 near-duplicate pairs undetected)
6. **PRIORITY CREATOR COVERAGE GAPS** — Pr6sper, Cupsey, DVCS absent from corpus
7. **FREEZE MARKER SELF-HASH STALE** — FREEZE_MARKER.json modified post-freeze

---

## 1. GOLD ADMISSION FUNNEL

### Raw → Gold
| Source | Raw Events | Gold Content | Admission Rate |
|--------|-----------|-------------|---------------|
| telegram | 12,737 | 1,180 | 9.3% |
| dexscreener | 549 | 0 (claims only) | — |
| twitter | 356 | 77 | 21.6% |
| youtube | 204 | 64 | 31.4% |
| web | 195 | 172 | 88.2% |
| web_search | 158 | 0 | — |
| twitch | 39 | 13 | 33.3% |
| **TOTAL** | **14,238** | **1,506** | **10.6%** |

Raw empty files: 4 (0-byte placeholders)

### Gold Claims by Admission
| Status | Count | % |
|--------|-------|---|
| GOLD | 431 | 35.6% |
| REJECTED | 427 | 35.3% |
| UNRESOLVED | 353 | 29.1% |

**Rejection reason:** `no_actionable_signal` — 427/427 (100%)

### Claim Types
| Type | Count |
|------|-------|
| narrative | 800 |
| entry | 213 |
| risk | 75 |
| exit | 64 |
| observation | 45 |
| regime | 14 |

---

## 2. SCOPE CLASSIFICATION

### Content Layer (1,506 records)
| Scope | Count | % |
|-------|-------|---|
| pump_specific | 419 | 27.8% |
| solana_memecoin_regime | 455 | 30.2% |
| generic_crypto | 268 | 17.8% |
| unresolved | 364 | 24.2% |

### Claims Layer (1,211 records)
| Scope | Count | % |
|-------|-------|---|
| pump_specific | 273 | 22.5% |
| solana_memecoin_regime | 347 | 28.7% |
| generic_crypto | 222 | 18.3% |
| unresolved | 369 | 30.5% |

**⚠️ CRITICAL:** Only ~22-28% of corpus is pump-specific. 17.8% is generic crypto that should NOT enter Pump SFT. ~24% is unresolved (no domain signal detected).

---

## 3. CREATOR QUALITY AUDIT

### Top 10 Creators by Volume
| Creator | Content | Claims | Gold | Rejected | Platform | Top Scope |
|---------|---------|--------|------|----------|----------|-----------|
| alphacalls | 645 | 591 | 246 | 156 | telegram | pump_specific |
| jacalcooks | 402 | 267 | 88 | 85 | telegram | unresolved |
| dexscreener | 137 | 137 | 0 | 134 | web | solana_memecoin_regime |
| pikalosicalls | 81 | 57 | 25 | 24 | telegram | pump_specific |
| youtube_search | 51 | 17 | 6 | 9 | youtube | solana_memecoin_regime |
| pump.fun_board | 32 | 32 | 32 | 0 | web | pump_specific |
| solanaalpha | 27 | 27 | 13 | 3 | telegram | solana_memecoin_regime |
| marlonalpha | 15 | 9 | 3 | 4 | telegram | pump_specific |
| cented7 | 14 | 6 | 2 | 4 | twitter | pump_specific |
| megga | 10 | 5 | 2 | 0 | twitch/twitter/youtube | solana_memecoin_regime |

### Priority Creator Coverage
| Creator | Found? | Content | Claims | Gold Claims | Notes |
|---------|--------|---------|-------|-------------|-------|
| Pr6sper/Prosper | ❌ NOT FOUND | — | — | — | Major gap — elite trader absent |
| Cupsey | ❌ NOT FOUND | — | — | — | Major gap — elite trader absent |
| Cented7 | ✅ | 14 | 6 | 2 | Twitter, pump_specific dominant |
| Cented (Twitch) | ✅ | 5 | 0 | 0 | All unresolved, no claims extracted |
| DVCS | ❌ NOT FOUND | — | — | — | Major gap |
| Megga | ✅ | 10 | 5 | 2 | Multi-platform |
| Orangie/OrangeSBS | ✅ | 6 | 3 | 0 | Twitter, 2 rejected |

**Coverage gaps:** 3 of 7 named priority traders (Pr6sper, Cupsey, DVCS) are absent. Content-level labels (HIGH_SIGNAL, TRADE_THESIS) are NOT assigned per-item in the schema — only admission_status (GOLD/UNRESOLVED/REJECTED) exists.

---

## 4. ENTITY RESOLUTION

### Content Layer
- Content with pump mints in text: **345/1506** (22.9%)
- Content with any Solana address: **540/1506** (35.9%)
- Content with cashtags: **832/1506** (55.2%)

### Claims Layer
- Claims with non-empty entities field: **974/1211** (80.3%)
- Claims with primary_mint set: **631/1211** (52.1%)
- Claims with pump mints in text: **205/1211** (16.9%)
- Claims with mentioned_mints populated: **0/1211** (0%) ← **DEFECT**
- Entity confidence field: **0/1211** ← **DEFECT**

### States Layer
- States with mentioned_mints populated: **0/1212** (0%) ← **DEFECT**

### Validation Layer
- Validations with mint: **374/374** (100%)
- 3 GOLD validations linked to Slinky via mint ✅
- 371 unmatched (mint present but no Slinky outcome)

**Mint linkage is broken between content→claims→states.** Content has 345 pump mints in raw text, but the claim/state builders do not propagate `mentioned_mints`. The `primary_mint` field IS populated for 631 claims, but `mentioned_mints` is empty across all claims and states. No forced cashtag→mint mapping occurs ✅, but entity confidence tracking is absent ❌.

---

## 5. DEDUP / INDEPENDENCE

| Metric | Count |
|--------|-------|
| Exact duplicate groups | 26 |
| Exact duplicate records (beyond first) | 26 |
| Duplication rate | 1.7% |
| Near-duplicate pairs (>85% similarity) | 37,982 |
| Reposts/retweets/forwards | 297 |
| Cross-platform echoes | 0 |
| Content with origin/parent linkage | **0/1506** |

**DEFECT:** No origin→amplification structure. 37,982 near-duplicate pairs exist but no dedup linkage field tracks which content originated a thesis vs which amplified it. One thesis echoed by multiple accounts could be counted as independent confirmations. Telegram "forwarded from" markers exist in text (297 instances) but are not structured into parent/origin fields.

---

## 6. TEMPORAL / CAUSAL SAFETY

### Temporal Fields
- Content with publish_time_ms: **1,493/1506** (99.1%)
- Content with first_seen_ms: **1,506/1506** (100%)
- Content with retrieval_time_ms: **0/1506** (0%) ← missing field
- publish_time == first_seen_time: 252 (potential same-time events)
- Wayback/archive provenance: 77 records

### Temporal Classification
| Classification | Content | Claims |
|---------------|---------|--------|
| EX_ANTE (live-causal) | 407 | 252 |
| EX_POST (retrospective) | 26 | 67 |
| AMBIGUOUS | 1,073 | 892 |

**⚠️ 1,073 content records (71%) are temporally AMBIGUOUS** — cannot determine if they were live-causal signals or retrospective recaps. This is a major issue for training: Qwen must not learn from ex-post recaps as if they were ex-ante predictions.

Publish time range: 2023-06-01 to 2026-12-20 (spans beyond Slinky window)

---

## 7. FULL SEMANTIC QA — ALL 26 strategy_cards + sampled claims

### Strategy Cards: FULL INSPECTION (26/26)

| Field | Populated | Null/Empty | % Null |
|-------|-----------|-----------|--------|
| setup_type | 26/26 | 0 | 0% |
| narrative_theme | 26/26 | 0 (15 "unspecified") | 57.7% unspecified |
| direction | 26/26 | 0 | 0% |
| entry_price_sol_avg | 12/26 | 14 | 53.8% null |
| exit_target_sol_avg | 10/26 | 16 | 61.5% null |
| ev_sol_300s | **0/26** | 26 | **100% null** |
| win_rate | **0/26** | 26 | **100% null** |
| failure_modes | **0/26** | 26 | **100% missing** (no schema field) |
| capacity_note | 26/26 | 0 | 0% |
| provenance (git_sha) | 26/26 | 0 | 0% |

### Strategy Card Admission
| Status | Count |
|--------|-------|
| REJECTED | 15 |
| UNRESOLVED | 6 |
| GOLD | 5 |

### Validation within strategy cards
- Cards with zero validated claims: **17/26** (65.4%)
- Cards where ALL validations are unresolved: **26/26** (100%)
- Cards with any SUPPORTED/CONTRADICTED: **0/26** (0%)

**DEFECT:** Strategy cards are structurally hollow:
- 0/26 have EV (expected value) — the core metric for strategy evaluation
- 0/26 have win_rate — cannot assess strategy effectiveness
- 0/26 have failure_modes — the required risk/failure analysis is missing from the schema
- 26/26 have ALL validations unresolved — no strategy has been validated against Slinky outcomes
- 57.7% have "unspecified" narrative_theme — no regime/context attached
- 15/26 are REJECTED (insufficient_claims)

The 5 GOLD-admitted strategy cards were admitted based on claim count, not validation:
- Card 3 (generic_setup, narrative, 4 claims) — 0 validated, all unresolved
- Card 5 (dip_buy, narrative, 31 claims) — 0 validated, all unresolved
- Card 11 (rug_avoid, narrative, 0 claims) — admitted with ZERO claims
- Card 12 (sniper_avoid, narrative, 0 claims) — admitted with ZERO claims
- Card 19 (kol_pump_play, narrative, 0 claims) — admitted with ZERO claims

**3 of 5 GOLD cards were admitted with ZERO validated claims.** This is a material defect — admission_status=GOLD was assigned despite no evidence of strategy effectiveness.

### Claims Semantic Audit (20 sampled across types)

Content quality observations from sampled claims:
- **HIGH-SUBSTANCE:** PikalosiCalls pump mint calls (e.g., `7yzxw5pa8kay...pump 2x from the 2nd call`) — mint-level entry calls with pump addresses ✅
- **HIGH-SUBSTANCE:** MduzCalls regime analysis ("crypto feels pretty calm right now") — market regime interpretation ✅
- **HIGH-SUBSTANCE:** MduzCalls BTC/SOL analysis ("btc is range-bound near $68k") — macro regime context ✅
- **LOW-SUBSTANCE:** "best in market copy trading and limit orders with auto exit strategies" — referral/product marketing ❌
- **LOW-SUBSTANCE:** "if i only had 0.1 sol, i'd trade memecoins like this (full guide)" — engagement bait ❌ (correctly REJECTED)
- **OFF-TOPIC:** Standard Chartered HKDAP stablecoin announcement — generic crypto, not Pump.fun ❌
- **QUESTIONABLE GOLD:** Jacal's Portal "i started with 90$ and i didn't even buy more than 5 coins" — PnL bragging without reasoning, admitted as GOLD ❌

**No invented rationale detected in derived summaries** — claim_text appears to be raw source text, not generated. ✅

---

## 8. VALIDATION

### Validation Verdicts
| Verdict | Count |
|---------|-------|
| (empty/unset) | 374 |

The `validation_status` field exists but the `verdict` field is empty for ALL records. Admission is used instead:

| Admission | Count | Outcome Source |
|-----------|-------|---------------|
| UNRESOLVED | 371 | unmatched |
| GOLD | 3 | slinky_gold_v3 |

### 3 Slinky-Matched Validations (GOLD)
All 3 matched validations have:
- Mint linkage ✅ (e.g., `BcHEaaTCvycPwwsJ9yQTXdHP9X2gCLkznDbZ8VySpump`)
- Slinky state ID ✅
- Outcome time ✅
- **BUT:** `ret_1s_bp`, `ret_5s_bp` are NaN — return metrics not populated
- `lead_lag_seconds` is enormous (5.8M–7.0M seconds = ~67–81 days) — the claim was made WEEKS/MONTHS after the Slinky outcome

**⚠️ TEMPORAL SAFETY ISSUE:** The 3 "GOLD" validations match claims to Slinky outcomes, but the claims were made 67–81 days AFTER the Slinky outcome_time. This means the narrative claims are EX-POST observations about mints that already had outcomes — not live-causal signals that predicted the outcome. These should be labeled retrospective, not used as causal validation evidence.

### Validation Coverage
- Total claims: 1,211
- Total validations: 374 (30.9% of claims)
- Slinky-matched: 3 (0.25% of claims)
- Unmatched: 371 (30.6% of claims)
- No CONTRADICTED or MIXED verdicts exist in the corpus

---

## 9. TRAINING-USABILITY REPORT

### Final Counts
| Layer | Count | Unique | Duplicates |
|-------|-------|--------|------------|
| creator_content | 1,506 | 1,480 | 26 exact |
| creator_claim | 1,211 | — | — |
| narrative_state | 1,212 | — | — |
| narrative_validation | 374 | — | — |
| strategy_card | 26 | — | — |

### Quality/Scope Distribution (Content)
- pump_specific: 419 (27.8%)
- solana_memecoin_regime: 455 (30.2%)
- generic_crypto: 268 (17.8%) — **EXCLUDE from Pump SFT**
- unresolved: 364 (24.2%) — **review before training**

### Platform Distribution (Content)
- telegram: 1,180 (78.4%)
- web: 172 (11.4%)
- twitter: 77 (5.1%)
- youtube: 64 (4.2%)
- twitch: 13 (0.9%)

### Creator Distribution
- Top 3 creators (alphacalls, jacalcooks, dexscreener) = 1,184 content (78.7%)
- Long tail of 20+ small creators
- Priority trader coverage: 4/7 named (57%)

### Duplication Rate
- Exact: 1.7% (26 records)
- Near (85%+ similarity): 37,982 pairs — **high noise risk**
- No structured dedup linkage

### Entity Resolution Confidence
- No entity_confidence field in schema (0/1211)
- 631/1211 claims have primary_mint (52.1%)
- 0/1211 claims have mentioned_mints populated (defect)

### Causal vs Retrospective
- EX_ANTE (live-causal): 407 content / 252 claims
- EX_POST (retrospective): 26 content / 67 claims
- AMBIGUOUS: 1,073 content / 892 claims

### Validation Coverage
- 374/1211 claims validated (30.9%)
- 3 Slinky-matched (0.25%)
- 0 CONTRADICTED, 0 MIXED
- 3 GOLD validations are temporally ex-post (67-81 day lag)

### Rejection Reasons
- `no_actionable_signal`: 427/427 (100% of rejected claims)

### Coverage Gaps
1. **Pr6sper/Prosper** — NOT in corpus (elite trader)
2. **Cupsey** — NOT in corpus (elite trader)
3. **DVCS** — NOT in corpus (elite trader)
4. **Meggga** (spelling) — only 10 content records
5. **Cented** (Twitch) — 5 content, 0 claims extracted
6. **Entity confidence** — no tracking
7. **Origin→amplification** — no linkage structure
8. **Strategy card semantics** — no EV, win_rate, failure_modes
9. **Temporal disambiguation** — 71% ambiguous
10. **Mint propagation** — broken from content→claims→states

---

## 10. FREEZE INTEGRITY

### Freeze Marker
| Field | Value |
|-------|-------|
| Status | FROZEN_IMMUTABLE |
| UUID | ng1_7a3f9c2b-8e1d-4b2a-a3f7-c0492e58b1a6 |
| Frozen at (declared) | 2026-08-29T07:15:00Z |
| Git SHA at freeze | cf97c33 |
| Hashes tracked | 13 (12 data files + self) |
| Data file hash matches | 12/12 ✅ |
| FREEZE_MARKER self-hash | STALE (modified post-freeze) |

### Timeline Reconstruction
| Time (UTC) | Event | Relative to Freeze |
|-----------|-------|-------------------|
| 00:36 | twitter_wayback written | 6h 39m BEFORE |
| 01:32–02:32 | twitter_historical (3 files) | 4h 43m–4h BEFORE |
| 05:37 | strategy_card_v1.jsonl written | 1h 38m BEFORE |
| **07:15** | **DECLARED FREEZE TIME** | **—** |
| 10:29 | twitter_historical (343 events, 334KB) written | **3h 14m AFTER** ❌ |
| 11:05 | content, claims, states rebuilt | **3h 50m AFTER** ❌ |
| 11:06 | validations rebuilt | **3h 51m AFTER** ❌ |
| ~11:10 | FREEZE_MARKER.json updated (Twitter note + new hashes) | **~4h AFTER** ❌ |

### Verdict

**FREEZE INTEGRITY VIOLATION.** The gold layers were rebuilt 3.8–3.9 hours AFTER the declared freeze time of 07:15 UTC. The 343-event Twitter file arrived 3.2h after freeze. The freeze marker was then updated with new hashes to reflect the post-rebuild state.

The current on-disk corpus is a **post-freeze rebuild**, not the originally frozen v1. The declared freeze time (07:15) does not match the actual rebuild time (11:05–11:06). All 12 data files match their current recorded hashes, but those hashes reflect the post-rebuild state, not the 07:15 freeze state.

**Per user directive:** Frozen v1 should be preserved as-is. The current corpus should be explicitly versioned. The on-disk state is effectively `narrative_gold_v1.1` (post-Twitter-rebuild), not the originally frozen v1.

---

## MATERIAL DEFECTS SUMMARY (7)

| # | Defect | Severity | Impact on Qwen Export |
|---|--------|----------|----------------------|
| 1 | Freeze integrity violation — gold rebuilt post-freeze | HIGH | Must version as v1.1 |
| 2 | Mint linkage broken — 0 claims/states have mentioned_mints | HIGH | Breaks entity resolution |
| 3 | Strategy cards hollow — 0 EV, 0 win_rate, 0 failure_modes, 0 validated | HIGH | 26 cards unusable for training |
| 4 | No entity confidence tracking | MEDIUM | Cannot weight mint resolution |
| 5 | No origin→amplification structure (37,982 near-dupes untracked) | MEDIUM | Inflation risk in training |
| 6 | Priority creator gaps (Pr6sper, Cupsey, DVCS absent) | MEDIUM | Reduces elite signal |
| 7 | 3 GOLD validations are temporally ex-post (67-81 day lag) | MEDIUM | False causal validation |

---

## RECOMMENDATION

**Preserve frozen narrative_gold_v1 as-is.** Do not mutate.

For Qwen curriculum/export:
- If a corrected version is desired, create `narrative_gold_v1.1` with:
  - Fixed mint propagation (content→claims→states)
  - Strategy card schema extended (EV, win_rate, failure_modes)
  - Entity confidence tracking
  - Origin→amplification dedup linkage
  - Temporal disambiguation of 1,073 ambiguous records
  - Correct freeze marker with actual rebuild time
- Use only `pump_specific` (27.8%) + `solana_memecoin_regime` (30.2%) for Pump SFT
- Exclude `generic_crypto` (17.8%) from Pump training
- Review `unresolved` (24.2%) before admission

**No CPT/SFT exports created.** Awaiting curriculum design decision.

---

## ALL FOUR FROZEN CORPORA — FINAL STATUS

| Corpus | Records | Status | UUID | Git SHA |
|--------|---------|--------|------|---------|
| laserstream_gold_v3 | 4 parquet layers | FROZEN | — | cf97c33 |
| slinky_gold_v3 | 134 parquet, 622,870 mints | FROZEN | — | cf97c33 |
| rust_gold_v1 | 737/541/163/534/21 | FROZEN_IMMUTABLE | 55e19441-809f-432f-b57c-fcb32769420a | — |
| narrative_gold_v1 | 1,506/1,211/1,212/374/26 | FROZEN_IMMUTABLE (post-rebuild) | ng1_7a3f9c2b-8e1d-4b2a-a3f7-c0492e58b1a6 | cf97c33 |

### Validation Truth (rust_gold_v1 — preserved exactly)
- ✅ cargo check — PASS
- ✅ cargo test --compile — PASS
- ❌ cargo clippy — FAIL (17 pre-existing warnings)
- ❌ cargo fmt — FAIL (Windows os error 206)
- ❌ pq_regression_tests — FAIL (1 pre-existing)

---

*Certification performed August 29, 2026 ~17:00 PT by Hermes Agent (glm-5.2).*
*Read-only audit. No files mutated. Report written to gold directory.*
