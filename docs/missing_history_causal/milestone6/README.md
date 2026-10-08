# Milestone 6: wallet-history provenance (read-only search result)

## Producer (traced)
- Fields: flow_lookback_d, smart_entrants_300s, smart_net_flow_sol_300s, coentry_wallets_300s (+ the other 9 flow fields).
- Builder: training/code/src/v2/rl/build_c11_flow_enrichment.py -> /training/v2/reports/C11_FLOW_ENRICHMENT.jsonl (c11_flow_build.log: 15,894,228 events, 123,840 clocks, 513,327 wallets, 84.6 s). Assembled by inject_c11_flow.py.
- Input: ONE pass over the FULL corpus tape /training/v2/canonical/renormalized_v7/trades.sorted.jsonl (all venues, all mints, 7.35 GB, 4 capture days: 2026-08-23, 08-24, 09-09, 09-10), applying EVERY event in recv order to global state (wallet first/last/extracted/distinct-mints, early-buyer co-entry graph) and serving each clock BEFORE events at/after it.
- "7-day" = a configured cap (FlowParams.lookback_ms = 604,800,000; fresh rule 24 h), NOT a stored file. The history is IN-MEMORY state generated from the tape prefix; nothing persisted except the OUTPUT (C11_FLOW_ENRICHMENT.jsonl). flow_lookback_d = min(days since tape_t0, 7), tape_t0 = first tape event (2026-08-23 13:32:57 UTC).
- The tape is reconstructible: renormalize_raw.py over the raw captures (4 sessions under /mnt/data/mev_bot-artifacts/{raw,recovered_raw,north_star/capture}); logs renorm_aug23/aug24/sep10.log.
- Two sessions of this audit's captures (09-09) are 2 of the 4 tape days. A cold collector has no 08-23/08-24 prefix.

## Causal replay proof (production Rust FlowReducer, diagnostic example flow_tape_replay)
- Full tape prefix through the production reducer reproduces C11_FLOW_ENRICHMENT for mint 51nH at all 14 clocks: 0 mismatches over 10 flow fields (flow_fulltape).
- Same reducer started cold at the S1 capture start: 4 fields x 14 clocks mismatch (flow_lookback 0.0 vs 7.0; smart=0; coentry=0). A strict 7-day-before-start window gives the same mismatch, so what is missing is the 08-23/08-24 prefix, not "7 days" per se.
- Seeding (new DecisionCache::seed_flow_history, trades strictly before the first clock) + other-mint corpus-basis events in-session: coentry matches at 5 of 14 clocks (9 differ), flow_lookback matches 14/14, smart still differs (13/14 entrants, 14/14 flow). Cause NOT isolated: the live event path carries curve trades only; the tape also holds PumpSwap rows and curve rows outside the corpus table, which feed wallet extraction. Untested.

## Enriched block (holders/bundles/wash/volume) -- separate cause
- Producer: rl/build_c9_enrichment_full.py: ONE tape pass, per decision: all tape trades of THAT mint with recv <= t_dec (inclusive), pumpfun+pumpswap rows, NO price band, NO dust floor. Not wallet history; different population rule than the state block (band+floors, recv < t).
- C11_FLOW_ENRICHMENT vs prompts: 188 prompts checked, 0 mismatches (persisted values are what the prompts carry).
