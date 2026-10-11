# Milestone 2: corpus-tape vs TradeEvent reconciliation (bounded; no relabel/retrain)

Scope: raw parts 0000-0019 of each session, successful transactions only. Regenerate: `python3 ledger.py SESS 20`
(needs /tmp/mh_recon2/ref/SESS/pf.jsonl = corpus `renormalize_raw.py` output; recipe in `../milestone1/run_ref.sh`),
then `family.py`, `crosstab.py`. Full ledgers (~44 MB) are regenerable; first 200 rows + sha256 kept.

## What the corpus tape's `sol_lamports` IS (measured)
For all 26,498 matched session-1 rows `tape_sol == the trader's SOL balance delta` (native lamports + WSOL token delta)
for the whole transaction. It is NOT the swap quantity. TradeEvent `sol_amount` is the curve swap quantity (pre curve-fee
split is in the event's fee fields). They differ because the balance delta also contains:
tx fee (payer), ATA rent (k*2,039,280), other transfers in the same tx (tips, other instructions), and the curve fee.
Matched-row residual classes (session 1): exact swap-qty-net-of-curve-fee 5,087 (19%); -txfee 691; -txfee-k*ATA 231;
other transfer / multi-instruction 20,489 (77%), whose non-swap component is p50 1.3M lamports, p90 9.3M, p99 37.8M, max 3.67B
(p50 1.0% of swap size, p90 13%, p99 4.5x). So the tape's net-flow-type quantities carry non-swap SOL for most rows.

## Classification (session 1 | session 2)
| class | rows | classification |
|---|---|---|
| matched | 26,498 | **intentional definition (different quantity)** for sol; token qty identical by construction of the match |
| event_only nonzero, tx has NO corpus-known buy/sell discriminator | 3,015 | **corpus coverage gap (likely bug, not yet judged)**: every such tx logs `BuyExactQuoteInV2` (3,222 txs session 1 incl. zero-reserve; also seen: V2BuyExactInPumpFun 659, PumpBuyV2 45, others), which the corpus DISC table lacks; 2,991 of the 3,015 show the user's token delta equal to the event quantity. Whether the corpus SHOULD have counted them is a definition decision, not made here. Session 2: 4,815 |
| event_only nonzero, tx HAS a corpus-known buy/sell ix | 1,142 | **unresolved**: the corpus resolver produced no tape row for an event whose instruction it knows; cause (sign-test / conservation rejection) suspected, NOT shown |
| event_only zero-reserve (vs==0) | 1,833 | **unresolved + decoder-scope**: quote is USDC (see below) |
| tape_only | 809 | **unresolved**: instruction recognised, tape row exists, no event with equal token qty; 795 of 809 have 1 ix/1 event/1 tape row (quantity differs, or event absent) |
| trader_mismatch | 98 | **unresolved**: all 98 tape rows came from the `net_position` router fallback; the event user's token delta does not equal the event quantity in all 98 |

Session 2 (same classes): matched 30,195+329 zero; event_only nonzero NOT-in-DISC 4,815; covered 1,430; zero-reserve 2,584; tape_only 1,300; trader_mismatch 129.

## Zero-reserve events
All 1,996 session-1 zero-reserve events (35 of 880 mints, no mint partly zero) occur in transactions that move USDC
(EPjFWd...) and whose instruction logs are V2 variants. For these: `sol_amount == 0`, `virtual_sol == 0`, `real_sol == 0`,
but virtual_token/real_token are non-zero and the user's token delta equals the event token quantity in 1,986 of 1,996.
In 1,629 of 1,996 the user's USDC balance moves with the correct sign for the direction. The event's trailing bytes do not
contain the USDC amount at a consistent offset (matched in 6 of 1,996 only), so the quote amount was NOT recovered from the event.
Reading: these are USDC-quoted curves; the SOL fields are legitimately zero, so a SOL price cannot be derived from them.
Verified facts (mint, trader, side, token quantity, slot, tx identity, fee/CU) remain usable; price/SOL-denominated quantity are
NOT established. This is a layout-supported hypothesis, not a proven program-version statement (no program IDL/version checked).
The Python comparison shares the vs==0 exclusion, so it cannot independently validate it.

## Train-serve consequence (documented, not acted on)
1. **Net-flow-type features** (`net_flow_sol_300s`, `smart_net_flow_sol_300s`) trained on the tape's wallet-balance delta;
   the event path serves swap quantity. Same name, different quantity: 78% of matched rows carry a non-zero non-swap component (median small, 1% of swap size; tail large). Serving TradeEvent `sol_amount` under these
   names is NOT parity. The event path currently emits `quote_lamports = sol_amount` into the flow reducer.
2. **Entrants/counts** differ where the corpus DISC table lacks an instruction family (`BuyExactQuoteInV2`: 3,222 txs session 1,
   5,094 session 2 -- 10.3% / 15.2% of event-bearing txs): the corpus never saw those buys, the live event path does.
3. **bot_uniform_share** keys on exact `sol_lamports` repeats: balance deltas almost never repeat exactly; swap quantities do.
No relabelling, retraining, or redefinition under unchanged names is performed here.
