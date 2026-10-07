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
