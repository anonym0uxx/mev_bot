# research-004 — policy-target OBSERVATION run (manifest v1, frozen before deployment)

Authority: Alon 2026-10-24 (conditional authorization: corrections 1–6 implemented and frozen first). Zero capital,
zero order authority, zero RPC, ≤1.5M streaming credits, ≤24 h, no funded book, no training, no live orders.

## Entry path (unchanged baseline, observation-only authority)
- Binary v5 = commit `65d685c7` (journal repair `3e9203d0` + `PQ_OBSERVATION_ONLY`), sha256 recorded at launch.
- Frozen Qwen entry: `probe_sl_v1_compact` on `probe-mg3-base` at 127.0.0.1:8004 (served identity verified by the
  daemon), prompts byte-identical to research-001 (entry prompts carry no account state; `cash_sol` exists only in
  management prompts, which PROD_HOLD never sends). step250 never requested.
- Every valid BUY verdict → disposition `notrade:observation_only` BEFORE any capital/portfolio veto; no order can
  exist. SKIPs journaled as before. Reference account inputs: paper seed 2.0 SOL declared for the engine's bookkeeping
  only; it never changes (no orders) and is NOT a book. Neither historical ledger is touched.
- Management inference: none (PROD_HOLD_V16; protections independent). Serving load = entry asks only.

## Episodes and measurement (independent of asks)
- Episode = the FIRST accepted ask per mint (AMM regime: venue pumpswap/mixed) after start; repeats and re-entry asks
  journaled separately. Enrollment is outcome-blind: the daemon's admission + pool-lane capacity (cap 16, FIFO by
  activity) decide which migrations are followed; `dropped_cap` migrations are NOT enrolled and are counted.
- Observation state: pool lane idle TTL raised 600 → 2,100 s so an enrolled pool stays subscribed through the
  1,800 s barrier + landing window even with no trading; TTL 3,600 s from migration unchanged.
- Journal per AmmSwap: state-matched `virtual_quote` (null = unverified), `fee_parts`, pool/quote identity, trader.
- Outcomes priced OFFLINE by `episode_pricer_v2.py` (exact frozen engine: landing 400 ms/5 s TTL, all-in entry price,
  hard stop 35 % fill-anchored, precursor 30 % single verified state, barrier 1,800 s from fill, exit TTL 60 s with
  engine retry semantics, 95 bps exit convention + 10k/leg + ATA rent/close) at hypothetical clips 0.05 AND 0.15 SOL;
  both reported, neither selected. Protocol-125-bps sell-fee variant reported as a labelled sensitivity.
- Named unavailability/unresolved: entry_not_landed, entry_economics_not_on_landing_event, capture_end_censored,
  pool_lane_expired_idle, pool_lane_dropped_cap, no_landing_swap_observed_while_subscribed,
  vq_unverified_on_all_candidate_states, not_spendable_from_vault. Absence from the tape ≠ dead market. Unresolved
  inventory kept at cost; valuation sensitivity on the last verified state shown separately, never as outcome.
- Diagnostics (separate): 30 s / 120 s decision-time markouts (`episode_pricer.py`).

## Cutoffs (frozen; metered by `research004_watch.sh` every 5 min)
- ENROLLMENT ends at the first of: 1,200,000 streaming credits | 14 h | 60,000 dispatches (daemon cap) → ENTRY_PAUSE
  (no new asks; follow-up continues). Projection at the observed 82k credits/h: ≈14.6 h → the 14 h clock likely binds.
- FOLLOW-UP ends at the first of: enrollment_end + 40 min | 1,500,000 credits (daemon alloc guard + watchdog) | 24 h →
  DAEMON_STOP. Late enrollees therefore always have their 30-min horizon inside the follow-up window or are reported
  as `capture_end_censored` — never silently missing.
- No automatic extension for any reason (coverage floor 40 priced episodes is coverage, not power).

## Analysis plan (frozen; development data; confirmation requires a later untouched window)
Counts: eligible migrations seen, dropped_cap, enrolled mints, BUY-intent mints, SKIP mints, repeats, re-entries;
priced / unresolved (by reason) / unavailable (by reason) per clip.
Economics per clip, conditional on priced coverage, with by-mint cluster bootstrap 90 % intervals (labelled as such):
mean/median/sum net, positive share, top-mint share of gross positive, exit-reason mix (barrier / hard stop /
precursor), exit landing delays, unresolved cost basis and sensitivity marks.
Comparisons (diagnostic): no-trade cash (0); frozen-Qwen BUY-intent episodes; outcome-blind matched-participation
abstention = 2,000 random subsets of all enrolled episodes with the BUY-intent count (distribution of mean net);
NO C rule (R* never selected; the declined R candidate is not revived).
Verdict scope: a negative result REJECTS DEPLOYING THE TESTED BASELINE (frozen Qwen + PROD_HOLD_V16 at these clips) and
does NOT establish that selection in this regime can never be profitable; failing to meet a rejection condition is not
success. "share of episodes above 2× fees" is a diagnostic only. These are policy-valued episode simulations, not
fills, not sequential portfolio P&L.
Deliverable: counts, economics with uncertainty, costs (credits as usage units), and ONE recommendation — no supported
next strategy, or one defined learning hypothesis with a bounded follow-up.

## Amendment v1.1 (recorded 2026-10-24 16:56:10 PT, before any outcome inspection)
Alon shortened the window: enrollment ends 18:28:34 PT (start+2 h); follow-up ≤40 min → 19:08:34 PT; credit caps unchanged; no extension to reach 40 episodes; policy, clips and outcome definitions fixed. Label: SHORTENED OBSERVATION.

## Amendment v1.2 (2026-10-24 17:48:41 PT, before outcome inspection)
Alon closed enrollment now (17:48:41 PT); follow-up 40 min to 18:28:41 PT; all else unchanged.
