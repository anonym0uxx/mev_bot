# P6 management slice — report (branch `task/p6-management-slice`, tip c351324d)

Direction: Qwen manages positions; Rust keeps truthful state, executes, and enforces hard safeguards.
Paper only. No live submission, no model-policy change, no training, no D: write. This is
EXECUTION INTEGRATION, not evidence that management is profitable.

## Commits (all pushed; 1168 tests, 0 failed, golden digests unchanged)
1. ecbde065 position-store token-quantity sells (`sell_tokens`, named `SellRefusal`), inventory unknown
   until a fill, model-managed exit reason (app code 10, brain ordinal 8, append-only)
2. dd8fe683 `model_managed` positions: legacy ladder / trail / into-strength / thesis / stall / moon bag /
   time-stop / VPIN stand down; rug precursor + hard stop remain as the agreed hard safeguards
3. a60f1711 management lane end to end (snapshot -> trained renderer -> async verdict -> REDUCE/EXIT
   order -> paper fill -> position)
4. c39d4708 durable SAFETY_OFF
5. c351324d sent-state-age fix

## Action semantics (as implemented)
- REDUCE = floor(inventory/2) raw tokens at ORDER time; EXIT = all inventory. Inventory is the fill-set
  token count, not a notional-derived number. Pending quantity is not inventory; only a fill reduces it.
- Partial fills: remainder stays pending and the position stays open and monitored.
- Dust / zero / over-sell / unknown inventory are named refusals that change nothing.
- ADD: returns `mgmt:add_unsupported`; no amount is substituted; management is reported INCOMPLETE.

## ADD is unresolved (needs an Alon decision)
The sources disagree, so no faithful amount exists yet:
- seam + authority + reward_engine: ADD = min(max(cash,0), 0.5 x capital_sol)
- c11 validation rows: qty after ADD = 1.5 x qty before in 714/714 transitions (half-INVENTORY)
- On 9,694 ADD-labelled rows the two rules agree only about as often as not: ratio half-inventory/label
  10th-90th pct 0.42-2.6; half-inventory spend exceeds cash on 47%.
Pin which is the contract (the label authority is the reward engine) before ADD is wired.

## Timing / safety
- Cadence 30 s, first question after the 60 s hold. The hold gates only the model's questions; rug
  precursor / hard stop and SAFETY_OFF never wait on it (tested).
- Beyond the 16-step training cap: continues with the REAL step number, real hold time and real
  causal MFE/MAE; no fresh entry, no history reset. Counted as `mgmt:beyond_corpus_step_cap` (step >= 16).
  UNVALIDATED DEPLOYMENT EXTENSION: the model never saw step > 14 (max in the c11 corpus; the 16-step cap is a builder bound, not what the data reached) at a 30 s gap.

## SAFETY_OFF (durable)
- Persisted atomically (tmp + fsync + rename). Unreadable/unknown-schema file => BLOCKED.
- Restart restores the block and never re-arms; re-arm needs a named operator, is refused while a recon
  fault or uncertain-ack order is unresolved, and is refused if it cannot be made durable.
- Trip: invalidates queued certain entry orders; preserves uncertain-ack orders; held-position protection,
  reconciliation and REDUCE/EXIT keep running (separate request table).
- Hung endpoint: 3 consecutive abandoned/failed asks trips it. Controlled shutdown holds positions,
  keeps orders pending, writes the record.
- Two mutants (no restore on restart; uncertain orders invalidated) each fail their tests.

## Observability
- sent_state_age was `clock - t_dec` (identically 0). Now the age of the newest observation inside the
  prompt. Regression proven to fail on the old formula. Earlier freshness numbers from that counter
  are void.
- No retries (explicit current policy). Any later retry must keep request identity + original sampled action.
- Closed-ledger conflicts stay blocked and recorded (unchanged; no deliberate reconciliation yet).

## Remaining gaps (not done)
- ADD (decision above).
- Position RESTORE from the durable file on restart (the file records positions/orders; the daemon does
  not yet rebuild them). Until then a restart with held positions is blocked and listed, not resumed.
- Daemon wiring: `model_controlled_shutdown` / `model_safety_attach` are not yet called from pq_daemon.
- Management snapshot enrichment/flow fields come from the same decision cache; coverage by venue/age not
  yet measured on captured data. AMM REDUCE/EXIT fee on the sell side is unverified (counted
  `mgmt:fill_amm_sell_fee_unverified`); AMM management fills are not assessable.
- History trace continuation not touched this session.
