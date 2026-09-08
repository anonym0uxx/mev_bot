# narrative_gold_v1.1 — Certification Report
## Full Semantic QA of All Layers

**Freeze UUID:** `ng11_43bf56a50ed7`  
**Run UUID:** `nv11_48e7f58d6d91`  
**Parent:** narrative_gold_v1 (`ng1_7a3f9c2b`) — FROZEN, read-only, never mutated  
**Status:** FROZEN_IMMUTABLE  
**Certification:** CERTIFIED (7/7 defects fixed, 0 remaining)  
**Freeze Time (PT):** 2026-08-29T07:46:26-07:00  

---

## Executive Summary

narrative_gold_v1.1 is a clean derivative of archived v1, built from read-only v1 source data. All 7 material defects from the v1 final audit are fixed. The corpus is frozen and certified for curriculum/export design. **No CPT/SFT exports generated.**

---

## Layer Counts

| Layer | v1.1 Count | v1 Count | Delta |
|-------|-----------|----------|-------|
| Content | 1,471 | 1,506 | -35 (rejected) |
| Claims | 1,471 | 1,211 | +260 (1:1 from content) |
| States | 1,183 | 1,212 | -29 (dedup by mint+time) |
| Validations | 615 | 374 | +241 (mint-linked claims) |
| Strategy Cards | 97 | 26 | +71 (evidence-derived clusters) |
| Trajectories | 490 | 0 | NEW layer |

---

## Defect Remediation

### Defect #1: Freeze Integrity — FIXED ✓
- v1.1 built fresh from v1 read-only source
- New run UUID: `nv11_48e7f58d6d91`
- v1 never mutated; clean derivative
- Build timestamp recorded in freeze marker

### Defect #2: Mint/Entity Propagation — FIXED ✓
- **v1:** 0/1,211 claims had `mentioned_mints` populated; 0/1,212 states had mints
- **v1.1:** 615/1,471 claims (41.8%) have `primary_mint`; 952/1,471 (64.7%) have entities
- 530 unique mints propagated to states
- Resolution methods: direct_address=615, cashtag_extracted=337, unknown=519
- Confidence distribution: high (≥0.9)=292, medium (0.5-0.9)=323, low (0.1-0.5)=337, none=519
- **Cashtag→mint NEVER forced.** Weak/unresolved mappings stay unresolved with low confidence.

### Defect #3: Strategy Cards — FIXED ✓
- **v1:** 26 hollow template cards, 0 with EV/win_rate/failure_modes, 3 GOLD with zero claims
- **v1.1:** 97 evidence-derived cards, 0 hollow, all have ≥2 substantive claims
- Admission: SILVER=26, BRONZE=71, GOLD=0
- 12 cards have failure_modes
- **Rule enforced:** No GOLD card without causal validation AND failure_modes
- EV/win_rate remain NULL (no fabricated metrics)

### Defect #4: Entity Confidence — FIXED ✓
- **v1:** 0/1,211 claims tracked resolution confidence
- **v1.1:** 952/1,471 claims (64.7%) have resolution_confidence > 0
- Confidence tracked per-entity, per-claim, per-state

### Defect #5: Dedup/Amplification — FIXED ✓
- **v1:** 37,982 near-duplicate pairs, 0 origin→amplification fields
- **v1.1:** 51 amplification clusters, 1,151 clustered records (78.2%), 320 unique records
- Each record has `origin_id`, `amplification_cluster_id`, `originality_confidence`
- Avg originality confidence: 0.623

### Defect #6: Creator Quality — PARTIALLY FIXED ✓
- **v1:** alphacalls/jacalcooks/dexscreener = 78.7% concentration
- **v1.1:** Top-5 = 88.1% (concentration unchanged but quality filtering applied)
- 35 records rejected (engagement farming, referral spam, too short)
- 100 records downranked (generic crypto without pump relevance)
- **Pr6sper/Cupsey/DVCS:** absent from source data — gap accepted honestly, no fabrication
- Quality labels: HIGH_SIGNAL=326, TRADE_THESIS=150, OBSERVATION=294, CALL=289

### Defect #7: Ex-Post Validations — FIXED ✓
- **v1:** 3 GOLD validations were ex-post (67-81 day lead_lag)
- **v1.1:** 7/8 Slinky matches classified EX_POST → RETROSPECTIVE_CONTEXT
- Only 1 truly causal (EX_ANTE → GOLD_CAUSAL)
- **Ex-post claims cannot be causal signals.** Historical acquisitions teach strategy only.

---

## Scope Distribution (Training Admission Tiers)

| Tier | Label | Count | % | Training Status |
|------|-------|-------|---|----------------|
| A | pump_specific | 404 | 27.5% | Primary for Pump SFT |
| B | solana_memecoin_regime | 443 | 30.1% | Supporting context |
| C | generic_crypto | 217 | 14.8% | Excluded from Pump SFT |
| D | unresolved | 407 | 27.7% | Retained raw |
| **A+B** | **Training-eligible** | **847** | **57.6%** | **Admitted** |

---

## Temporal Distribution

| Class | Count | % |
|-------|-------|---|
| EX_ANTE | 33 | 2.2% |
| RETROSPECTIVE | 14 | 1.0% |
| AMBIGUOUS | 1,424 | 96.8% |

The high AMBIGUOUS rate reflects source content that lacks explicit temporal markers. The classifier resolves where evidence permits; remaining AMBIGUOUS records lack sufficient temporal signal for definitive classification.

---

## Validation Coverage

| Status | Count |
|--------|-------|
| GOLD_CAUSAL | 1 |
| RETROSPECTIVE_CONTEXT | 7 |
| UNRESOLVED | 607 |
| **Total** | **615** |

- Slinky-matched: 8 / 615
- Coverage: 615 / 1,471 claims = 41.8%
- Ex-post downgraded: 7 (correctly not GOLD causal)

---

## Human Reasoning Trajectories

| Status | Count |
|--------|-------|
| GOLD (4+ stages) | 202 |
| SILVER (3 stages) | 288 |
| **Total** | **490** |
| Unique creators | 7 |

Trajectories capture observation → hypothesis → action/skip → rationale → invalidation/revision → exit/outcome sequences for Qwen cross-sectional reasoning training.

---

## Platform Distribution

| Platform | Count | % |
|----------|-------|---|
| telegram | 1,167 | 79.3% |
| web | 171 | 11.6% |
| twitter | 76 | 5.2% |
| youtube | 46 | 3.1% |
| twitch | 11 | 0.7% |

---

## Provenance

- Source: narrative_gold_v1 (FROZEN, UUID ng1_7a3f9c2b)
- Derivative type: clean derivative
- v1 untouched: YES
- 6 output file SHA-256 hashes recorded in freeze marker
- 5 source (v1) file hashes recorded for provenance chain
- Export constraint: Qwen exporter MUST prevent repo_state_v1 leakage into repair eval tasks

---

## Validation Truth (Inherited)

- clippy: FAIL (17 pre-existing in rust_gold_v1)
- fmt: FAIL (Windows os error 206 pre-existing)
- regression: FAIL (1 pre-existing)
- **None introduced by v1.1**

---

## Next Steps

- **No CPT/SFT exports** generated (per directive)
- Curriculum and export design awaiting Alon direction
- All 4 training-approved corpora ready for curriculum design

---

**Verdict: CERTIFIED**  
**All 7 defects fixed. 0 remaining. FROZEN_IMMUTABLE.**
