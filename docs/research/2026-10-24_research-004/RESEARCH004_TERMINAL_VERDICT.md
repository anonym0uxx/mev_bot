# research-004 — TERMINAL OBSERVATION VERDICT (SHORTENED OBSERVATION)

Run: 2026-10-24 16:28:34 PT start; enrollment closed 17:48:41 PT (manifest amendment v1.2, Alon, before any outcome
inspection; v1.1 had been 18:28:34); follow-up ended 18:28:41 PT; daemon stopped (unit inactive). Zero capital, zero order
authority, zero RPC. Frozen analysis `research004_analysis.py` (commit-pinned before outcomes) → `RESEARCH004_RESULT.json`.
Binary v5 sha256 31fa14bdacb7200e… (commit 65d685c7). Credits spent: ~137,967 streaming (cap 1.5M; no RPC).

This is an OBSERVATION: policy-VALUED episode simulations on journaled swap states, not executed fills and not sequential
portfolio P&L.

## 1. Measurement — working
131,360 / 131,360 journaled AmmSwap records for enrolled mints carry a state-matched virtual_quote; 13/13 enrolled episodes
priced to a landed standing exit (0 unresolved, 0 unavailable); exit landing delay median 556 ms; max attempts 1.

## 2. Opportunity / sample — inadequate for any selection claim
- 784 distinct mints dispatched across venues (8,755 dispatches); **13 distinct eligible AMM episodes enrolled**.
  5 further migrations hit the pool-lane cap (dropped_cap, not enrolled — a coverage loss, outcome-blind).
- **0 first-ask BUY intentions → the frozen-Qwen BUY-intent first-ask subset is EMPTY (n=0).** 12 of 13 enrolled mints
  received a BUY intention on a LATER ask (descriptive count only; no outcome test was registered for them). 1,208 repeat
  asks on the 13 mints. First-ask analysis evaluates Qwen's first encounter, NOT the repeated-decision entry policy.
- 13 episodes is a coverage count, not statistical power. All episodes come from one ~80-minute enrollment window.

## 3. Economic evidence (policy-valued, exact PROD_HOLD_V16 + 35 % fill-anchored hard stop + 30 % precursor, engine fees)
Applies to ALL 13 first-ask AMM episodes (i.e. unselective entry at first sight), conditional on priced coverage:
- 0.05 SOL clip: sum −0.331 SOL on 0.65 deployed; mean −0.0255 SOL/episode (−51 % of clip); median −0.0225; 1/13 positive
  (+0.0167, barrier exit); 12/13 exited by amm_hard_stop; by-mint cluster bootstrap 90 % interval of the mean
  [−0.0319, −0.0180] (conditional on priced coverage).
- 0.15 SOL clip: sum −0.994 SOL on 1.95 deployed; mean −0.0764; 1/13 positive; interval [−0.0957, −0.0540].
- Protocol-125 bps sell-fee sensitivity changes the sum by < 0.003 SOL. Both clips reported; neither selected.
- Frozen-Qwen BUY-intent episodes: n=0 → no estimate; outcome-blind matched-participation abstention not computable.
- Caveat: stops use the engine's vq-inclusive spot; 12/13 stop-outs on first-sight entries is a large effect that was not
  independently audited beyond the pricer selftest.

## 4. Verdict scope
- Unselective first-sight entry into freshly migrated PumpSwap pools under this policy: NEGATIVE in this sample (n=13).
- The frozen Qwen entry baseline: INCONCLUSIVE — it made no first-ask BUY, so this run produced no outcomes for its
  decisions. This run does not show that Qwen's selection is profitable or unprofitable and does not show the model failed.
- No defensible new paper hypothesis is supported by 13 episodes. Not an edge; not a proof that no edge exists.
- Actual executed paper trading (separate evidence): cohort-003 realized −1.484686018 SOL (account of record),
  research-001 realized −0.281095791 SOL; combined −1.7658 SOL on 37 fills; both closed flat. Those are paper fills under
  modelled costs, not live trades (research/paper cohorts ran with live-order authority off; the live wallet inventory
  check is recorded separately in the handoff).
