# narrative_gold_v1 — Design Document

## Status
- **Schema:** COMPLETE (5 gold layers + RAW layer defined)
- **Seeds:** PARTIALLY RESOLVED (Orangie, Cented, Cupsey, Megga, Potion, Ansem resolved; Prosper + J7Tracker unresolved)
- **Acquisition framework:** COMPLETE (first_seen recorded, cron heartbeat active)
- **Gold layer builders:** NOT YET STARTED (pending real adapter data)

## Objective
Maximize executable NET SOL after fees, slippage, latency, failed fills/losses and capacity.
Authority hierarchy: on-chain truth > policy-independent economics > policy critique > creator/tool claims.

## Architecture

```
RAW LAYER (immutable, never deleted)
  raw_social_event_v1/     ← social-ingest adapters (telegram, x, tiktok, web)
                           ← first_seen recorded at ingestion
                           ← one-object-per-line JSONL

GOLD LAYERS (fail-closed admission, derived from RAW)
  1. creator_content_v1     ← cleaned, deduped, chronologically ordered
  2. creator_claim_v1       ← extracted claims (entry/exit/risk/narrative/regime)
  3. narrative_state_v1     ← causal features at time t (joins to Slinky/LaserStream)
  4. narrative_validation_v1 ← links claims to on-chain outcomes (SUPPORTED/CONTRADICTED/MIXED/UNRESOLVED)
  5. strategy_card_v1       ← synthesized strategies from validated evidence
```

## Causal/Bias Controls (BINDING)
1. **Availability:** first_seen + realistic ingestion latency. No future edits/replies/win-rate.
2. **Reliability at t:** uses ONLY prior resolved claims (track_record_as_of_t).
3. **Dedup:** reposts/copies collapsed so amplification != independent breadth.
4. **Required controls:** contemporaneous unmentioned mints + matched same-regime/onchain states.
5. **Propagation tracking:** event→alpha/tracker→wallet/onchain→CT to separate early edge from crowding.
6. **Missing source != zero mentions.**
7. **Ex-ante vs ex-post:** separate ex-ante calls from ex-post recap/PnL brag/education.

## Seed Sources (resolved wallets)
| Creator | Wallets | X Handle | Status |
|---------|---------|----------|--------|
| Orangie | 10 | @OrangeSBS | RESOLVED |
| Cented  | 8 | TBD | RESOLVED (wallets) |
| Cupsey  | 6 | TBD | RESOLVED (wallets) |
| Megga   | 2 | TBD | RESOLVED (wallets) |
| Potion  | 4 | Discord:potionalpha | RESOLVED |
| Ansem   | 4 | @blknoiz06 | RESOLVED |
| Prosper | 0 | TBD | UNRESOLVED |
| J7Tracker | 0 | TBD | UNRESOLVED |

## Next Steps
1. **Connect real adapters** — need API keys (TELEGRAM_API_ID/HASH, TWITTERAPI_IO_KEY)
2. **Build gold layer 1-5 builders** — once raw data is flowing
3. **LaserStream gold** — from CLEAN 300-min RAW capture (parallel track)
4. **Rust gold** — from Git history (after LaserStream)

## File Locations
- Schema: `schemas/narrative_gold_v1.py`
- Seeds: `schemas/narrative_seeds_v1.yaml`
- Acquisition: `src/acquire_narrative_v1.py`
- RAW output: `output/narrative_gold_v1/raw/raw_social_event_v1/`
- First_seen log: `output/narrative_gold_v1/raw/FIRST_SEEN.log`
- Acquisition state: `output/narrative_gold_v1/raw/acquisition_state.json`
- Cron heartbeat: job `8e9bce8d7dc8` (every 15 minutes)
