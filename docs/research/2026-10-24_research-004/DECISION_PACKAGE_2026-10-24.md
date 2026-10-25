# Decision package — 2026-10-24 ~16:40 PT (research-001 closed; measurement repaired; next evidence step)

## 1. Terminal book economics and actual spend
| book | seed | realized | cash | open | status |
|---|---|---|---|---|---|
| cohort-003 (account of record) | 2.0 | **−1.484686018** | 0.515313982 | 0 | stopped 10-23 19:04 PT, capital-vetoed |
| research-001 (research book) | 2.0 | **−0.281095791** | 1.718904209 | 0 | ADMINISTRATIVELY CLOSED 10-24 15:47:25 PT |
Trading profit (after venue fees + 10k lamports/leg; failed-attempt costs unmodeled): **−1.765781809 SOL** across both books,
37 fills / 37 closes. Standalone operating cost (usage units, incremental): streaming **1,727,075 Helius credits**
(cohort-003 503,764; research-001 1,223,311 — of which ~490k in the last 6 h after the v4 repair widened tracking) + ~230
`getTransaction`/`getAccountInfo` RPC calls; reserve 500k untouched; 27.5M allocation → ~25.3M unconsumed. No documented
credit→$ rate on file → no monetary conversion. GPU serving (paper-engine-8004, owned GPU1/2) is allocated hardware, not
incremental. No daily-income extrapolation. research-001 close: 92,808/200,000 dispatches; cutoff, interim inspection
(14:54 PT) and reason recorded in `ADMINISTRATIVE_CLOSE_2026-10-24.json`; watchdog removed; desk/alert crons paused;
terminal desk report written; ledgers/journals/tapes preserved; research-003 NOT launched.
Classification: registered primary **INCONCLUSIVE** (1/11 primary, 10/56 calibration BUY episodes with valid outcomes;
availability depended on repeat asks → complete cases may be selected); R strategy **DECLINED** for lack of supporting
evidence. Carried-vq sensitivity arithmetic recorded: all 55 −0.248/2.75 SOL (−9.02 %), kept 41 −0.204/2.05 (−9.95 %),
rejected 14 −0.044/0.70 (−6.29 %) — retained set worse; not selection evidence. Not proof that every R filter, entry
policy or LLM must lose.

## 2. Focused measurement repair and proof
Cause: the AmmSwap journal dropped the swap event's own `virtual_quote_reserves`; vq was only pinned at Qwen asks, so an
episode's horizon state was priceable only if the same mint was asked again (vq varies continuously: 87/90 mints showed
multiple values; carrying is unsafe). Repair (commit `3e9203d0`, 1 junction test, build OK, NOT deployed):
every AmmSwap journal record now carries `virtual_quote` (null when the layout is unverified — never 0), `fee_bps`,
`fee_parts`, `pool_hex`, `pool_is_canonical`, `quote_is_wsol`, `trader_hex`. Reader `episode_pricer.py` (commit `c788161`)
enrolls an episode once and follows the tape alone: no later ask/BUY/SKIP/veto/eligibility affects availability; pricing
uses the horizon (or landing) state's OWN pre-trade reserves + OWN vq; proceeds capped by the real vault; named
unavailability (`no_state_at_entry`, `entry_vq_unverified`, `state_missing`, `vq_unverified`, `not_spendable_from_vault`,
`noncanonical_or_non_wsol_only`, `unresolved_*`). Proof (selftest passes): one enrollment → no asks → horizon reached →
priced on a state whose vq CHANGED (17.0→19.5 SOL; a carried vq gives a different number) ✓; missing vq on the horizon
state → `vq_unverified` ✓; no state → `state_missing` ✓; non-canonical/non-WSOL → unavailable ✓; legacy tape → unavailable
at entry ✓; causal policy exit priced on the landing state ✓. Episode coverage no longer depends on repeat requests.
Economic target aligned with the policy evaluated: `price_policy_exit` = PROD_HOLD_V16-like barrier (1,800 s) with
independent 50 % hard-stop INTENT, landing on the first state ≥ intent+1 s within 120 s, else UNRESOLVED (inventory kept,
no imputed value). 30 s/120 s markouts remain diagnostics only. No general monitoring framework added.
Observation-state preservation: the pool lane keeps subscriptions by activity; not changed (see coverage below).

## 3. Existing-data coverage for the proposed economic target
`TAPE_SUPPORT_CHECK_2026-10-24.json` (research-001 tape, 81 first-entry episodes, 506,369 swaps for those mints):
swaps with a state-matched vq field **0** → the existing tapes CANNOT price the policy target with verified vq (nor the
120 s diagnostic beyond ask-pinned states). Structural coverage if vq had been present: entry state 81/81; 120 s state
70/81; **barrier landing state (1,800–1,920 s) 37/81**; any state after 30 min 46/81. So ~45 % of episodes would end
UNRESOLVED at the barrier because the pool goes quiet — a real feature of this regime (dead inventory), not a measurement
defect; it must be reported as unresolved exposure, never as zero loss. cohort-003's tape has the same legacy format.
No retrospective repair of the inspected panel is possible or proposed.

## 4. One recommended next evidence step — OBSERVATION, not a trading experiment
Research question (unchanged): can information available before entry distinguish continued demand from distribution
well enough to support a profitable exit at our latency and size? The data to answer it does not exist yet (§3).
Proposal: **research-004 "policy-target observation"** — one daemon (v5 = repaired journal), frozen Qwen entry asked
exactly as research-001 (baseline label), ZERO simulated or live capital (no book), enrolling EVERY accepted AMM-regime
first ask as an episode (BUY-intent and SKIP alike; re-entries flagged), outcomes priced offline by `episode_pricer.py`:
 • main target: policy exit net (barrier 1,800 s / hard-stop intent 50 % / landing ≤120 s) at the STANDARD 0.05 SOL
   hypothetical clip and at 0.15 SOL (size sensitivity), plus unresolved share; diagnostics 30 s/120 s kept separate;
 • pre-decision observables recorded as already measured, with honest names: gross buy/sell quote volume since first
   trade, distinct signer count (NOT people), cumulative sell/vault (turnover, NOT absorption capacity), ret_5s/30s,
   vault + state vq (executable liquidity), top1 float share (inferred, NOT ownership), pool age vs token age;
 • comparisons frozen now: no-trade cash (0), frozen Qwen BUY-intent episodes, all-enrolled (outcome-blind abstention
   benchmark at matched participation = random subsets of the same size as the BUY-intent set), transparent numerical
   benchmark = the existing C rule (`pricing_eligible ∧ net_flow_sol_300s>0 ∧ ret_30s_bp>0`) at the same clip;
 • mechanism under test: post-migration demand persistence vs. distribution into the pool; competing explanations:
   latency (the move is consumed before landing), size-insensitivity of the hypothetical clip, regime/time clustering;
 • rejection (declared now): if the all-enrolled policy-net mean has a by-mint bootstrap 90 % upper bound < 0 AND the
   frozen-Qwen BUY-intent subset's upper bound < 0 AND fewer than 10 % of priced episodes exceed +2× fees, the regime
   under this policy is declared unprofitable for selection work and retired; no threshold sweep; no relabeling.
 • boundaries frozen before any fit: this run = development; any rule derived from it must be confirmed on a later
   untouched window; overlapping outcome windows never split across sides; repeated mints stay on one side.
Cost ceiling: ≤ **1.5M streaming credits** within the existing allocation (reserve protected), 0 RPC, 0 capital, serving
on owned GPU1/2 (GPU0 protected). Endpoint: 24 h or 1.5M credits, whichever first; expected turnaround 24 h + 32 min
barrier + ~1 h analysis. Coverage floor 40 distinct first-entry episodes with priced policy outcomes (expected ≈45 of ≈90
enrolled at research-001's rate; floor ≠ power). Stop: credits cap, 24 h, safety latch, feed loss > 30 min, operator stop.
Decision value: (a) a labelled episode set whose outcome IS the policy's executable net (needed for any Qwen target of
conditional executable value/downside); (b) a go/no-go on whether selection within this regime can be positive at all.
If rejected → retire the AMM-regime/PROD_HOLD combination as a profit candidate; no further panels on it.
Blockers: none technical (v5 built, pricer proven). Requires Alon's approval to deploy v5 and spend ≤1.5M credits.
Not requested: new feeds, bulk RPC, training, reseeding, research books, live orders.
