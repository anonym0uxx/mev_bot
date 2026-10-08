# Acceptance ledger (PR #13, branch task/durable-missing-history)

Single running ledger. Status words: DONE (evidence cited), PARTIAL, OPEN. Linux evidence only; nothing here is Windows durability acceptance.

## Superseded results (hard-stop break-even bug)
Before the fix, the model-managed hard stop passed `trail_bps = 0` to `protection_level_fp`, so the trail leg equalled ENTRY and
the "-35% backstop" was a break-even stop. Contract restored (not changed): `LifecycleParams::standard().hard_sl_bps` and
`Config::lc_hard_sl_bps` = 3,500; `protection_level_fp` documents `entry * (10_000 - hard_sl_bps) / 10_000`.
Superseded (do not cite): any model-managed protection/lifecycle result produced before commit 71b6ff3f that depended on a close
at/below entry, notably the `amm_path_e2e` runs showing 18 closes for 1 opened position and `protect:amm_exit:HardStop` counts
in the pre-fix run, and the older `model_managed_positions_stand_down...` slow-bleed assertion (it passed but did not isolate
the leg). Replaced by `position::tests::{the_hard_stop_level_*, hard_stop_isolated_*, the_trail_leg_is_actually_disabled_*}`.
Legacy (unmanaged) path is unedited; `legacy_unmanaged_positions_keep_their_trail_and_hard_stop_behaviour` pins it, including its
pre-existing label quirk (a legacy trail exit at/below entry is labelled HardStop).

## Items
| # | Item | Status | Evidence / gap |
|---|------|--------|----------------|
| 1 | Hard stop pinned, isolated from other triggers | DONE | position tests; mutation (restore `0`) fails 2 isolated tests |
| 2 | AMM lifecycle independent ledger | DONE | `amm_lifecycle_independent_ledger_*`; mutation (double fill record) fails it |
| 3 | Protection-gap surfacing (named, last VERIFIED mark, alert path) | DONE | `held_amm_protection_gap.rs` (4 tests) |
| 4 | Seeded daemon baseline | BLOCKED (decision) | schema-2 seed rebuilt from the same tape/cutoff (6,389,425 events, sha in seed2/build.out); daemon restores it and, with an explicit replay clock (PQ_FLOW_RESUME_MS=first wire recv), reports an UNAVAILABLE interval of 1,397,631,566 ms (16.2 d): seed coverage ends 2026-08-24T10:35:15Z, capture starts 2026-09-09T14:49:07Z. Prompts correctly refuse by scope; 0 stub-model requests. Clearing it needs a separately approved contract, not a waiver. |
| 5 | Real-process recovery | OPEN | |
| 6 | universe_promotable trace / Qwen eligibility | OPEN | |
| 7 | Identity-registration negative tests, mid-life discovery | OPEN | |
| 8 | Executed AMM volume to shared consumers | OPEN | |
| 9 | BUY/SELL pre-execution quote matrix, WSOL route | OPEN | |
| 10 | Hosted full-workspace CI, hashed Windows package | OPEN | |

## Stated limits
- A held-AMM mark is the pool's PRE-trade state of the observed swap and excludes that swap's own price impact. A price-moving
  swap followed by silence is not detectable until a later swap's pre-trade state, or the 60 s pricing budget raises the gap.
- Spot trigger is not an executable sell quote. AMM routing fills stay non-assessable.
- No new liquidation rule: the gap alerts; it does not close anything.

## Update: option-2 continuous fixture (milestone10, SHA ce2c1168 + this ledger commit)

**Fixture.** s1 wire_0000..0004: 159,532 lines, 2026-09-09 14:49:07.168 to 14:52:22.338 UTC. Fresh flow history initialized at line 1 (seed_source `cold_start:segment_09_09_14_49_07`); no August state. Manifest with per-file SHA-256: `milestone10/fixture_manifest.json`.

**Continuity evidence (and its limit).** One capture session (20260909_144906_000490), one lane. Over the whole 679,746-line s1 recording: max recv gap 542 ms, max slot step 2. No reconnect/gap record in the session log. NOT established: recorder-side drop counters do not exist, so "no loss" is inferred from recv-gap/slot-step bounds, not proven.

**Readiness classes.** Daemon writes `data/flow_readiness.json`: `cold_start_declared_history_limited` vs missing/failed/incompatible bootstrap are distinct. Zeros under the declared class mean none observed in this history, not none globally. Prompt grammar unchanged. Does not enable history-poor production trading; the daemon's refusals are unchanged.

**Replay clock.** `PQ_FLOW_RESUME_MS` is honoured only with `PQ_OFFLINE_PAPER_REPLAY=1`, no `--live`, model lane armed; else exit 96 with a named reason. Process-level: no flag -> NotInOfflineReplayMode, `12abc` -> InvalidValue, far future -> InTheFuture, all exit 96. Affects only the flow-history resume declaration; freshness, staleness, inference deadlines and monotonic timeouts untouched. The live-mode process case could not reach the check (exits earlier on missing creds), so live+flag is covered by the unit test only.

**Recovery matrix** (`milestone10/recovery_matrix.log`):
- u1/u2 (two uninterrupted runs): flushed checkpoints are byte-identical (payload sha 95352e2b...), 42 and 44 stub requests.
- A->B (crash after publication at cursor 1788965446370; restart with overlap from line 70001): resume complete (unavailable_ms 0); final checkpoint identical to uninterrupted (95352e2b...).
- C->C2 (SIGKILL with no flush; periodic writer had published through cursor 1788965410114; restart replays whole segment): complete; final checkpoint identical; 44 requests.
- D (checkpoint at line 20000, restart at line 150001, hole 152,844 ms): Restored unavailable 152,845 ms, complete=false, gap recorded; 0 model requests (scoped refusal). The August-to-September 16.2-day hole case (milestone9) is retained as the negative test.

**Not equivalent (stated, not hidden).** Request streams are NOT byte-identical even between two uninterrupted runs (42 vs 44; first t_dec differs by ~1 s): prompts are cut on wall-clock ticks against a replay that runs faster than real time, so decision clocks and tick-aligned state (staleness, t_dec, age) vary run to run. Flow history state converges exactly; prompt-level equivalence is not yet demonstrated. Held exposure, emergency order->paper fill->settlement, and callout emission through the real daemon are not yet run in this matrix.

## Barrier-mode restart divergence (fe3c45f2 → next)

Status: WIP. Preserved: 187-barrier uninterrupted agreement (bU1/bU2/bV1/bV2/bW1/bX1/bY1 identical digests and prompt hashes);
restart after barrier 100 matches on held positions, cash, committed capital, dirty set and receive times (86 aligned barriers).

First divergent barrier (restart barrier 6 == baseline barrier 107, clock 1788965463929): market 7e888138.
- Identical on both sides immediately before: last_ask=1788965446146, dirty_since=1788965437622, reask_ok=true, held_or_pending=false, dirty=true.
- The STREAM scheduler did not dispatch it on either side (queue position ~306/594 against a budget of 8 per tick; `sched_deferred_budget` 118,298 in the restart run).
- The baseline dispatched it through the LEGACY-PROMOTION path (`model_admit_candidate`, called from the watchlist promotion in `Engine::evaluate`):
  watchlist rank_pos=7 of 45, rank 677,753 equal to the 8th-slot rank (kth_rank 677,753), min_rank=1, score 826,529, discovered_at=199, now=217.
- In the restart the same market is NOT_PRESENT in the watchlist (size 30 against 44-45), `now`=13 against 217.
- Earliest differing input: the legacy watchlist and numeric lane are driven by the engine TICK COUNTER (`self.now`), which advances once per evaluate and is neither persisted nor reproduced by the restart's ingest-only overlap replay. The restart therefore rebuilds that state at tick ~0.
- Path split over the whole baseline (watch prefix empty): entry dispatches 28 via candidate_path, 7 via the stream scheduler. So 28 of the 35 baseline entry dispatches depend on legacy watchlist rank, i.e. the unproven `universe_promotable`/legacy-authority question is live here, not only a counter name.
- Not a missing-history effect: continuity is Restored/complete in both runs; refusals are not the gate.

Not yet fixed. Options (decision for Alon, not taken unilaterally because it changes which markets reach Qwen):
 (a) make the stream scheduler fair (rotate or oldest-dirty-first instead of mint order) so legacy rank is not the de-facto entry path, then compare again;
 (b) persist and restore the tick counter plus watchlist state, and replay with ticks;
 (c) keep legacy promotion and declare restart parity out of scope for tick-domain state.

Instrumentation committed (env PQ_REPLAY_WATCH_MINT, barrier mode only, never in the digest): per-market scheduler trace in data/barrier_watch.log.

## Update: scheduler + excursion fix + aligned recovery comparison (tested head fc37a768 + test-only commit)

Binary identity: `/training/mh_build/proc/BINARY_IDENTITY_fc37a768.json` (source fc37a768, dev profile, sha256 62379d54...dba5ed; a forced rebuild reproduced the same hash).

* 9c37b3ab (preserved evidence): one stream entry scheduler (oldest-dirty-first, stable mint tie-break, budget 8/tick, work bound 64 examined/tick); legacy promotion counted only. Baselines bS1/bS2 agree on 187/187 barriers, 79 dispatches. Entry result at 9c37b3ab: 62 post-crash requests in baseline and restart, entry prompts identical.
* fc37a768: MFE/MAE tracker counts only prints with recv >= fill_ms. Tests (independent expected values, 3 mutations caught): pre-fill ignored, post-fill high/low exact, equal-time counted (documented conservative boundary: timestamp equality cannot distinguish before/after), duplicate/older prints do not regress extrema, restored extrema survive overlap replay and a genuinely new low applies once, and the current MARK is the newest accepted print (fails if the out-of-order guard is removed). 1,080 app+junction tests pass.
* Aligned comparison on fc37a768: bT1/bT2 agree 187/187 (79 dispatches), bT1 == bS1. Restart after barrier 100 (rs8_2): 86 common barriers, 62 dispatches, 0 barriers with a differing dispatch or prompt hash (entry AND management). State digest still differs in exactly two categories at all 86 barriers: closed-order audit history ("order id" lines and "fill order" lines for orders closed before the crash: ids 1,2,4,6,7). Positions, extrema, pending orders, cash, committed capital and order_seq are IDENTICAL.
* Open (NOT claimed closed): closed-order identity is not restored. Evidence-ingestion rejects `unknown_order` for a late report about an order closed before the crash; duplicate-fill prevention across restart for those ids is therefore not yet proven. Next item.
* Scheduler progress: 70 ineligible older markets + 3 eligible behind them: tick 1 spends the 64 bound on the old prefix, tick 2 examines the remaining 6 once and dispatches the 3 eligible oldest-first; no re-inspection (mutation that re-queues refused markets fails the test).
* Attribution: curve invocation-parent test established (b7080d8a). PumpSwap `ingest_amm_rows` has NO repeated-same-mint test and takes no invocation positions (identity = instruction index only). Not claimed equivalent.

## Update: terminal identity, uncertain orders, foreign verdicts, scheduler coverage (tested head: see last commit)

Evidence kept: the 86-barrier aligned restart agreement (prompts, dispatches, positions, extrema, pending orders,
cash, committed capital) on `fc37a768` is completed acceptance evidence and is not re-opened.

- **Held ledger schema 2** (was 1; old files refuse by name `schema`). It now carries settled-order identity
  (id, mint, attempt, clip, filled clip, state, terminal evidence), unresolved reconciliation faults verbatim, and a
  compaction floor. Restored AS RECORDS: balances/positions come only from the ledger totals and `held`, never from
  replaying an order's financial effect. Validation refuses (by name) an order id beyond the issued sequence and a
  fault naming no known order; unknown fault sources, non-settled states and duplicate ids make the file untrusted.
- **Late reports after a restart** (tests, each mutation-checked): identical -> `Duplicate`, ledger unchanged;
  conflicting -> `Fault`, first evidence kept verbatim, the mint's new exposure blocked, no second credit/debit;
  never-issued id -> `Rejected(unknown_order)`, never applied. The fault and its block survive a second restart.
- **Retention:** settled records are capped (4,096). Compaction drops only the oldest settled, un-faulted, non-position
  records and raises `order_floor`; a later report for such an id is `Rejected(compacted_order)` (named, unresolved,
  never applied). A faulted order is never compacted, so expiry cannot turn a conflict into a harmless "unknown".
- **Uncertain entry order:** restored unbooked and not resubmitted; the simulator never auto-fills it; reconciled
  Filled opens exactly one position and 3 repeated reports change nothing; reconciled NotFilled clears it once, a
  later Filled report faults and is not applied.
- **Foreign verdicts:** request ids restart at 1 per process, so id+mint alone could let an abandoned process's answer
  satisfy a NEW request. Found by test (it created an order), fixed: every job/verdict carries a per-process session
  id; a verdict of another session is discarded by name (`discard:foreign_session`) at the single verdict entry point
  and in barrier staging. Mutation-checked. The session id is never persisted or digested.
- **Equal-ms rule:** a print at exactly `fill_ms` is counted post-fill by CONVENTION. No ingest sequence is persisted,
  so same-ms order cannot be resolved; the convention can raise MFE as well as lengthen MAE (test pins both). Not
  "conservative". Protection and excursion features are separate paths; nothing in protection changed.
- **Queue coverage (bF1, end of the uninterrupted run):** 859 registered = 317 pumpfun + 441 pumpswap + 101 unknown.
  20 ready and dispatched. Never ready: 34 few_prior_trades, 7 join_curve_absent, 256 join_launch_unknown (pumpfun);
  1 curve_absent, 440 launch_unknown (pumpswap); 101 join_no_mint. Waiting in the queue at the end: 8, all inside
  the 15 s re-ask window (age 1.0-9.1 s); eligible-and-unserved = 0. The earlier "20 dirty" count was a different
  quantity (queue plus re-asked) and is superseded.
- **Scheduler:** refused market becomes schedulable when its required state (reserves) arrives, and a market inside
  the re-ask window is served when the window clears; neither is rescanned in between (tests).

Still open: PumpSwap repeated-same-mint attribution test (producer `ingest_amm_rows`; curve test does not cover it),
equal-time replay boundary for `through_ms`, deleted-ledger / mixed-generation / interruption-between-publications
tests, held emergency settlement and protection-gap alert through the daemon, dedup-retention replay, harness cleanup,
restart comparison re-run on the final head.

## Update: restart comparison on head 3e872d5c (daemon sha256 c05ff461...c022c)

- Run `bF1` (uninterrupted, 187 barriers, 79 dispatches; equals `bZ1`/`bT1`) against `rs9_2` (crash after barrier 100,
  full overlap replay): 86 common barriers, 62 dispatches, 0 barriers with a differing prompt hash or dispatch.
- State categories that differ: ONE, at 86/86 barriers: "fill order" lines, i.e. the in-memory `model_fills` list
  (archival presentation of fills made before the crash; 2 records there, 0 after restart). Settled-order records
  (8 of 8, ids, states, clip, terminal evidence), positions, extrema, pending orders, cash, committed capital,
  realized, order sequence all match. The previous second category ("order id") is closed by schema 2.
- Why the fill list is archival: `model_fills` feeds assessment/reporting only; duplicate prevention and
  reconciliation read `model_order_log` (restored). Assessable fills stay empty until quote AND landing are validated,
  so no assessed number depends on it. NOT excluded from the comparison; it is a named, known difference.
- PumpSwap `ingest_amm_rows`: repeated same-mint/same-side buys in one tx follow instruction-account ownership
  (reversed instruction order swaps rows, not owners); ordinals are row identities. LIMIT pinned: two instructions
  naming ONE trader each carry the trader's whole-tx net delta (corpus parity, not a per-instruction split), so
  per-instruction amounts are NOT established for that case. Curve and AMM attribution claims stay path-specific.

## Management-sell recovery and report inbox (tested head 017c2cdd)
- ed9041e8: ledger schema 3 persists management sell identity, cumulative fills, terminal state, faults. 10 engine tests.
- 158e82f2: inbox boundary; reservation guard in position.rs (protective close sells only inventory minus tokens reserved by an unresolved sell). 5/5 mutations caught.
- be24b128: settlement evidence is cumulative tokens + gross + fees (no per-fill price); increments booked exactly; schema 4 persists totals and per-order checkpoints. 4/4 valid mutations caught (one earlier mutation only failed to compile and was redone).
- 420f6289: inbox gate as tested function; reservation-gap status and named operator alert (`PROTECTION DEFERRED`) in StaleCallout.
- 017c2cdd: reader file identity (inode + consumed prefix); partial lines reported.
- Daemon evidence (binary 8e87cf32, run dirs gA1_1, gA1_2, gN1): offline mode consumed a valid EXIT report for the restored unresolved order (Completed, gross 5e9, fees 5e7); normal mode logged "inbox disabled" and left ledger and file untouched; `--live` + model endpoint exits 98 at startup.
- OPEN: daemon-level protective-trigger runs over restored REDUCE/EXIT reservations (SAFETY_OFF clear/tripped); alert through daemon; late report after protective sell; protection still books closes directly (no protective order -> fill -> settlement path); deleted-ledger, mixed-generation, interrupted-publication, equal-time identity, replay beyond retention; xA1 classification (operator-blocked).

## M1 update (tested heads 9145b96a → 3e45a549 → a09653e8 → bc4cdfc2; normal-concurrency app+junction suite)
Engine/unit scope only unless stated. Synthetic-protection evidence proves lifecycle behaviour, NOT AMM proceeds, landing or profit.
- 9145b96a (and its predecessor commit): reservation callout onset/reminder/recovery; release with stale prices stays
  degraded (2 mutations caught). ADD cumulative settlement: tokens, fee-EXCLUSIVE quote spent, all-in fees on top;
  cost basis = spend + fees (3 mutations caught).
- 3e45a549: ledger generation; flow history records `held_gen_seen`; `flow_ahead_of_books` refusal (deleted or rolled-back
  books), 4 mutations caught. Equal-ms identity across restart (2 caught). Settlement totals agree with books at every
  publication point; re-read from offset 0 is exact (2 caught).
- a09653e8: ADD fee bound. FIXED_LAMPORTS_PER_LEG=10,000 is a p50 priority+tip per landed leg, NOT an upper bound.
  Pre-submission estimates above the reservation still refuse. Executor evidence above the reservation for an ISSUED order
  is booked as executed and gets the named fault `add_exceeds_reservation`, which blocks the mint and persists; no
  automatic adjustment (2 caught). History-behind-books overlap replay: flow rebuilt equal to uninterrupted run, no fill
  re-applied, reservation equal to the unfilled remainder, earlier decision read unchanged; missing interval stays
  `feed_gap`; a pre-generation history next to restored books refuses `flow_generation_unbound` (3 caught). Invariant
  restated: remaining = prior + reconciled acquisitions - reconciled disposals.
  Binary 1770fb19...6441a43.
- bc4cdfc2: ZERO-TOTAL FINDING FIXED. Before: paper (simulated) fills went through sell_tokens/realize and moved cash and
  inventory, but left the order's cumulative gross/fees at 0 and the fill record at 0/0. Now the paper executor computes
  (gross, all-in fee) with the SAME math (`simulate_sell_settlement`) and books it once through the cumulative-evidence path
  (`model_mgmt_ingest_evidence_inner`); the order and record are labelled `simulated` (persisted, held schema 6). Modelled
  fee = venue schedule + p50 leg, not executed; simulated fills stay outside assessable PnL. Test: two paper disposals at
  different prices, restart, duplicate replay, contradictory replay, against independently computed values (4 caught).
  1133 tests. Binary 95a8848c...71b95da86ee15ea598d091f971a5b1 (BINARY_IDENTITY_bc4cdfc2.json).
- FIXTURE PREREQUISITE NOT MET (binary 1770fb19, head a09653e8): cA1_1 (REDUCE stub) and cB1_1 (EXIT stub), paused at
  barrier 100 and killed, left NO pending management order. These are not recovery results. Barrier-number pausing is
  retired; replaced by a bounded harness checkpoint on durable state.
- Collapse fixture collapse_wire_v1 (source cont_wire05 sha 2bade7eb..., unchanged): 3 synthetic sells on 3ba1f2be 1 ms
  apart. They can trigger protection but cannot satisfy the 400 ms / newer-slot fill rule by themselves; separately
  labelled later landing observations are still TODO. Not yet run.
- OPERATOR-BLOCKED: finalize/report() force-close (OPERATOR_RESOLUTION_PACKAGE.md item 2); reads of xA1 and cA1_1.
- OPEN (M1): daemon matrix (SAFETY_OFF clear/tripped, Qwen unavailable, reserved/free, partial/full protective fill,
  deferral alert, late report, restart with pending protection, revised reader in a daemon run, startup generation refusal).

## M1 daemon matrix (2026-10-08 PT). Phase-1 = real daemon + mint-specific stub + durable HARNESS_HOLD; phase-2 = restore + collapse_wire_v2
Fixes found by daemon runs (each: test + mutation caught, 1,137 app+junction at normal concurrency):
- f88bf4fe: management + protective unresolved sells on one mint reserved max not SUM (qA1 reserved_ok=false).
- e5eb15ea: reservation not re-synced after a settling report until the next tick (qB3 reserved_ok=false).
- 37e12529/82cf1bdb: harness-only external-execution mode, HARNESS_CKPT/HOLD, post-final-barrier inbox poll (no Tick/clock).
Runs (binary sha256 prefix / head):
- pR1 partial REDUCE prerequisite MET; pE1 full EXIT unresolved reservation MET (37e12529 binary).
- qA3 97181c45/e5eb15ea: restored partial REDUCE + collapse -> protective sells only free part; partial protective fill; dup no-op. PASS.
- qB4 97181c45/e5eb15ea: restart with pending partial protective + partial REDUCE; inbox re-read from 0 changes nothing; no
  resubmission; late REDUCE completion then protective completion -> exact inventory, reservation = remainder; dups no-op. PASS.
- qC1 97181c45: fully reserved EXIT + collapse -> no overlapping sell, PROTECTION DEFERRED onset+reminder. PASS. Unavailable-Qwen
  SAFETY_OFF trip: PREREQUISITE NOT MET (no inference request issued; mgmt prompts refused pre-inference). Classification qC1_CLASSIFICATION.md.
- qD1 97181c45: SAFETY_OFF tripped via controlled-shutdown path (persisted, INCOMPLETE SHUTDOWN alert, process stays up); fully
  reserved deferral, partial then complete EXIT, dups no resurrection. PASS.
- qE1 97181c45: mixed-generation startup -> Untrusted(flow_ahead_of_books), readiness bootstrap_failed_untrusted, books unchanged. PASS.
- qF1 97181c45: handoff ack -> final report(): pre-report durable state recorded; post held.json differs only generation/wall time.
  No durable destructive mutation OBSERVED; finalize defect NOT cleared (in-memory force-close remains). Handoff acceptance OPEN.
Open in M1: endpoint-hang SAFETY_OFF trip in daemon (unmet prerequisite); finalize fix (operator-blocked); definitive
"not executed" release has no inbox route (engine-only).

## M1 bounded remainder (2026-10-08 PT). Tested heads e5eb15ea -> e0ebc194 (1145 app+junction, normal concurrency)

### Matrix classification (corrected). Each claim is tied to the binary that ran it.
Full passes on binary 97181c45… (e5eb15ea):
- qA3: restored partial REDUCE + collapse.
- qB4: restart, re-read from zero, late report, protective completion.
- qD1: SAFETY_OFF via the controlled-shutdown path.
- qE1: startup refusal flow_ahead_of_books.

Partial pass:
- qC1: fully reserved deferral + PROTECTION DEFERRED alerts pass; protection does not need an inference response. Its endpoint trip was prerequisite-not-met, now covered by eT1.

Observational, not acceptance:
- qF1: no durable financial mutation after the handoff ack and the final report(). That run does not clear the in-memory report() defect.

### Fixture binding (fx_bind2.py -> collapse_binding_v2.json; read-only, earlier artifacts)
- Mint 51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump (3ba1f2be…).
- Curve Hz3yWXQwZzFvYCsWyfzniWmXmi3NUrersWb2eBgRbzR2 = PDA(["bonding-curve", mint], 6EF8rrec…) bump 254, derived independently.
- Owner of every Hz3y update is 6EF8rrec….
- 260/260 curve updates equal the mint's TradeEvent reserves in the same slot.
- Last captured update before injection: line 143514, slot 445638166, recv 1788965517565.
- Synthetic events: all target this mint (slots 445638167..169). Synthetic curve observations: all target this curve.
- Held at both phase-1 checkpoints (pR1, pE1, venue curve, checkpoint observation = a Hz3y update). Restored as held in qA3/qD1 (and qT1).
- Verdict: binding VERIFIED. The protection-run claims need no qualification for binding.

### Endpoint-failure daemon case: eT1 on binary 31effff9… (e0ebc194). PASS
- Fresh start, real replay. Before the failure, requests reached the stub: 29 entry + 1 management (a mint-specific EXIT for 3ba1f2be).
- Prerequisite met at a durable hold: generation 48, EXIT id 3 unresolved, reserved = inventory 2462568951930.
- Endpoint down (HTTP 503):
  - 3 requests refused at the boundary; endpoint:transport_error = 3.
  - Trip safety:tripped:model_endpoint_hung under CONSECUTIVE_ABANDONED_TRIP = 3.
  - safety.json blocked, reason model_endpoint_hung, epoch 1.
- New exposure stopped: refuse:entries_blocked 110; no new positions.
- Under SAFETY_OFF, a partial EXIT report reconciled exactly: inventory 2462568951930 -> 1641712634620, reservation equal to the remainder.
- Endpoint recovered for 45 s: still blocked, rearmed 0, epoch unchanged, no new requests. No automatic re-arm.

### Terminal execution evidence (e0ebc194). Inbox field terminal:"final", harness-only.
Three outcomes:
- Definitively no execution: release.
- Partial with the remainder definitively cancelled: earlier settlement kept, only the remainder released.
- Absent field / non-terminal: execution unknown, nothing released.

Terminal evidence also:
- Settles any new increment first.
- Treats a terminal zero over a booked fill as a fault.
- Treats a duplicate as a no-op.
- Treats contradictory evidence after the end as report_contradicts_settled.

Validation: engine tests (3) + parser test; 5/5 mutations caught.

Daemon run qT1 (from pE1), binary 31effff9…. PASS:
- A non-terminal zero released nothing.
- The terminal zero ended EXIT 3 as EndedUnfilled (state 3), released the reservation, and protective order 4 for the full inventory 2462568951930 was created in that same tick.
- Duplicate terminal: no change.
- A contradictory later report (cum 1231284475965) raised the durable fault report_contradicts_settled; books unchanged.

Daemon run qT2 (restart from qT1, same binary). PASS:
- Record (state 3), fault and protective order 4 all restored. EXIT not resubmitted.
- Inbox re-read from 0 (4 lines): no change.
- The flow history is refused as flow_lineage_unbound because it predates lineage, which is correct.

### Generation evidence (qE1_GENERATION_LINEAGE.md)
- Predicate: 54 > 49 within the same lineage (pR1 -> qA3).
- Gap: an unrelated lineage was accepted at e5eb15ea. Fixed at e0ebc194 with lineage binding.

### Status
M1 integration is verified except the operator-blocked finalize. Acceptance stays OPEN until the finalize fix is applied and its focused tests pass (OPERATOR_RESOLUTION_PACKAGE.md). The cases that depend on it:
- report() twice changes nothing.
- The final daemon report preserves handed-off exposure.
- model_open_exposure() with unavailable marks.
- Unknown/stale marks cannot settle.

### Delivery checks on 07594e57 (lint-only commit on top of e0ebc194)
- Workspace --all-targets: 3655 passed, 0 failed, 1 ignored (normal concurrency). fmt clean, clippy -D warnings clean, portable gate passed.
- Hosted CI rust-ci run 37794580284 on head 07594e57: completed, success.
- pq-daemon sha256 2e9c245c… (BINARY_IDENTITY_07594e57.json). Daemon runs eT1/qT1/qT2 used e0ebc194 binary 31effff9….
- Clippy fixes: checked/saturating arithmetic, doc-list indentation, dead-code allows in tests. No behaviour change; tests re-run on the same head.

### M2 first measurement (bootstrap provenance, read-only)
- bF1: 859 registered markets.
- Canonical launch records from before the replay start (1788965347168): 0 (renorm_v7 and discovery_raw).
- Records after the start only: 59 (renorm_v7), 114 (discovery_raw).
- None of the existing durable launch files can bootstrap this tape's launch-unknown markets. A genuine launch source dated before the decision is needed (next slice).
