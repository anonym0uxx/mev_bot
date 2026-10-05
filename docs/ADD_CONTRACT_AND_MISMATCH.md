# ADD: contract choice and the seam / reward / corpus mismatch

Status: runtime contract chosen by the operator for PAPER. This is NOT a claim that the corpus's cash
transitions are correct, and no training artifact, label, or reward code was changed.

## Runtime contract (implemented, `engine/model_manage.rs`)
- TARGET = `floor(reconciled_inventory_tokens * 5000 / 10000)` raw tokens, read when the instruction is
  accepted and bound to that decision's position version. Account capital is never used.
- NOTIONAL is NOT copied from the corpus. At the landing state it is the MINIMAL notional whose delivered
  tokens reach the target under the venue's CURRENT executable economics:
  curve = constant product (`curve_fill::buy_tokens_out`); AMM = `buy_exact_quote_in` with the landing
  event's fee parts and virtual quote. Entry price is not an input of the planner (no such parameter).
- SPEND BOUNDS (all refusals named; nothing is resized, no capital rule substituted):
  `mgmt:refuse:add_insufficient_funds` (cash above the survival floor, net of committed capital, pending
  entries and other pending ADD reservations; includes venue fee and fixed leg cost),
  `mgmt:refuse:add_own_impact_limit` (existing 90 bp own-impact veto),
  `mgmt:refuse:add_spend_bound` (remaining order bound),
  `mgmt:refuse:add_no_executable_state`, `mgmt:refuse:add_amm_economics_missing`,
  `mgmt:refuse:add_unpriceable`, `mgmt:refuse:add_blocked_safety_off`.
- ROUNDING: target floors; notional is the smallest whole lamport reaching the target (so delivered tokens
  >= target); fee rounds down on notional as elsewhere in the lane; the cash reservation rounds fee UP.
- PENDING INTERACTIONS: one order per mint (an ADD blocks REDUCE/EXIT and vice versa); a pending ADD
  reserves `remaining notional + fee + fixed leg cost` so no second order can spend the same cash and the
  prompt's cash figure excludes it; a trip cancels an unfilled/partial ADD remainder (risk-increasing) and
  never a REDUCE/EXIT; an UNCERTAIN order is neither simulated-filled, expired nor cancelled.
- PARTIAL FILLS: change only the filled quantity and spend; the remainder stays pending; reports are matched
  by order id, kind, remaining quantity and remaining spend bound; duplicates/late reports are refused.
- ACCOUNTING: a buy realizes nothing. All-in cost joins committed capital and the position's cost basis;
  size/entry basis re-blend at the harmonic mean (notionals are not unit counts).

## What the sources say (preserved, unchanged)
- `pump-quant-inference/src/seam.rs` + `reward_engine.py:642`: ADD = `min(max(cash,0), 0.5 * capital_sol)`.
- `build_management_c12.py::invert_archived` / `build_replay_v2.py:226`: ADD = +50% of inventory, spend =
  `0.5*qty*mark*(1+OW)` with OW = 1.8% (archived one-way cost), cash-gated.
- c11 rows (8,702 consecutive ADD pre/post pairs): quantity change is +/-0.5 of pre-quantity; the
  corpus spend divided by (added quantity * MARK) is 0.982-1.018 (the OW band) while spend divided by
  (added quantity * ENTRY price) is spread over 0.04-3.2 (not entry-price-based). So the earlier
  "entry-price-based spend" suspicion is REFUTED for the transitions: they use the decision-time mark with
  a flat 1.8% cost, i.e. a historical flat-cost convention, not current executable economics. The runtime
  therefore does NOT replicate that spend either.
- Consequence: the reward engine (capital rule) and the SFT label transitions (inventory rule) disagree;
  RL was graded on one definition and the runtime now executes the other. Any reward-hacking or
  calibration conclusion about ADD must carry this caveat. Not fixed here (no training/label changes).
