# Milestone 7: controlled-population flow replay, enriched rebuild, field matrix, median defect

All numbers below were produced by the scripts in this directory (hashes of inputs: see milestone6/ and INPUT_HASHES.txt).
Diagnostic = reads the frozen tape. Nothing here is a serving rule.

## 1. Controlled populations (mint 51nH, session 1, 14 clocks, 10 flow fields)
Seed = tape rows with recv < capture start (6,389,425 rows), applied once. In-session rows streamed in receive order; clock served before any event with recv >= clock.
- current path (curve rows with corpus basis only): smart_entrants 13/14 clocks mismatch, smart_net_flow 14/14. Other 8 fields match.
- + PumpSwap rows: 0 mismatches at all 14 clocks.
- + excluded curve rows only: identical to current path (no effect on these fields).
- + both: 0 mismatches.
First divergent wallet at the first clock (CyaE1VxvBr...): extracted 538,040,693 lamports under the current path vs 22,985,622,716 with all rows; distinct mints 70 vs 71. First event the current path lacks: a PumpSwap trade at 1788965372595 for 22,447,582,023 lamports. So the smart-wallet threshold (5 SOL extracted, 5 mints) is crossed by PumpSwap activity the curve-only path never sees. Population cause, not wallet identity, amount, ordering or reducer state, for this wallet at this clock. One mint, one clock examined at wallet level.
CAVEAT: this replay takes PumpSwap rows from the frozen tape. The production path has no PumpSwap row producer with corpus-basis semantics yet; that is the AMM slice.

## 2. Enriched block (c9 builder)
Rebuilt from the verified all-venue mint population, no band, no dust floor: 164/164 clocks match on all 8 compared fields for BOTH the <= and the < decision-time boundary. Pumpfun-only population fails (up to 150/164 per field). The <= vs < boundary is therefore NOT separable on this mint: equal-timestamp events either do not exist at these clocks or do not change the value. Whether such events were available to a live decision at that instant is NOT established.

## 3. History semantics
- flow_lookback_d = min(clock - first tape event, 7 d) in days to 1 dp, a cap on elapsed time since the first tape event.
- Wallet first-seen is a rolling value that resets after >7 d idle; extracted lamports and distinct-mint samples persist for the whole run; the co-entry graph is global and unpruned.
- Observed coverage of the tape: events in only 21 distinct UTC hours (0.88 days) across a 17.67-day span. Gaps: 2026-08-23 19:00 -> 08-24 04:00 (10 h), 2026-08-24 11:00 -> 09-09 13:00 (387 h), 2026-09-09 20:00 -> 09-10 01:00 (6 h). So a "7.0" does not mean seven days of continuous observation. Do not describe it as complete seven-day history.
- Live recovery design (not implemented): checkpoint (wallet states, co-entry graph, mint first slots) with source provenance and the last applied event identity; restore then advance from events strictly after that identity, deduplicating on event_id; refuse an incompatible schema.

## 4. Whole-run median band (future-dependent corpus rule), both sessions, all mints with >=5 trades
S1: 63,655 of 2,791,409 trades dropped (2.28%); 1,411 mints affected; median share among affected mints 4.76%; max 59.1%.
S2: 64,303 of 2,906,229 (2.21%); 1,697 mints; median 5.18%; max 90%.
Not emulated live.

## 5. Representative sample (fixed rule, relaxed once after seeing counts: >=100 -> >=20 decision rows, disclosed)
4 mints per session were chosen, restricted to the span covered by the first 20 raw parts. 8 mints, 40 clocks, 39 rendered (1 refused join_curve_absent), 5 clocks per mint. Field matrix: field_matrix.txt. Six enriched fields at one clock remain UNCLASSIFIED (not investigated).
Most clock mismatches in count/volume fields coincide with the whole-run median band plus population; the matrix attributes them jointly and does NOT separate the two effects per field. creator_past_launches differs because the harness registers only this mint's launch: harness limitation, not a finding about the corpus.

## 6. Price note
The prompt carries the state block price (trader basis) in `price_lamports_per_raw_token`; `supplied_price` (model_authority.rs) reads that first match for the F5c grounding check (capture only). `PRICE LIMIT` is in the same unit and feeds min_tokens_out and the simulated fill limit check (model_admit.rs, price_anchor.rs). Fills are computed from curve reserves (buy_avg_price_fp). So the model is shown a trader-basis price and bounds it, while the executed fill is on reserves: a limit set from the shown price is compared with a reserve-based achieved price. On the 14 clocks of mint 51nH the trader/reserve ratio ranged 0.93-1.35 (median 1.04, trader price below reserve at 36% of clocks). The corpus prompt shows the same relation, so this is the trained contract; the difference is explicit but not harmless: a BUY limit equal to the shown price can be missed or met by reserve movement up to those ratios. Not changed.

## 7. Attribution tests
Added: two same-mint/same-side corpus-known instructions in one transaction are attributed one-to-one in order; an event with no unused matching instruction is refused, not reassigned. LIMIT: both tests have instruction order equal to event order, so they do not exercise a transaction where the order differs.
