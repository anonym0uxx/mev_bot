# Continuation — missing-history durability & recovery (OPEN)

Saved 2026-10-05 PT as a session handoff. **Do NOT read CI success as readiness.**

## Baseline (verified)
- `main` = `9ba157bf2d063e0062507f974602976f9a783b99` (PR #12 merge; contains `67c5330e` — verified ancestor of main).
- Unmerged follow-up: branch `task/durable-missing-history`, head **`05e599a3`** (PR #13, OPEN, unmerged).
  Branch history: `7cc77556` (run 30) → `f49a3f32` (run 31) → `05e599a3` (run 32).
- Accepted scope of `05e599a3`: **the recovery-bypass fix ONLY.** Nothing else on that branch is
  accepted as complete.

## Verified tests and their SHAs
`05e599a3` / hosted **run 32 = SUCCESS**: app lib **419 passed / 0 failed**; junction **14 ok / 0 failed**;
market-state **12 ok**; `fmt --check` clean; `clippy -D warnings` = 0 errors.
- `pump-quant-app` `decision_join::tests::*` — a plausible covering receipt does NOT unlock inference
  (`snapshot` stays refused, `mints_history_unreconstructed` stays 1); receipt-only reconciliation;
  restart restores the gap; an unreadable record refuses with `join_history_continuity_unknown`;
  compaction keeps the earliest unresolved instant.
- `pump-quant-junction` `tests/dropped_print_flow_e2e.rs` (2 tests) — end-to-end refusal by name, no
  premature clearing, horizon behaviour, and a plausible receipt staying refused.

## WARNING — current behaviour is NOT readiness
- **Restart currently LOSES gap state.** `pq_daemon` neither loads nor persists missing-history
  state; an unresolved gap is erased by a process restart.
- **Production reconstruction is UNSUPPORTED.** `reconcile_flow_history` / `clear_history_continuity`
  refuse with `ReconcileRefusal::ReconstructionUnsupported` even for a valid covering receipt; no
  aggregates are installed. Test-only fixtures (`install_reconstructed_fixture`,
  `clear_history_continuity_fixture`, `cfg(test)`) are NOT production recovery.
- **Affected mints lose Qwen entry AND management availability, indefinitely** — any mint with a
  dropped print is refused by name and, with recovery unsupported, has no unlock path.

## Current files, entry points, working tree
- Worktree `/home/alon/build/mev_bot_consol` (branch `task/durable-missing-history` @ `05e599a3`, clean).
- `rust/crates/pump-quant-app/src/decision_join.rs` — `MissingObservation`, `MissingDeps`,
  `ReconstructionReceipt`, `ReconcileRefusal`, `RestoreRefusal`, the per-dependency gate in
  `prepare()`, `restore_missing_history`, `missing_history_status`, `missing_history_records`,
  `push_missing_bounded`.
- `rust/crates/pump-quant-app/src/engine/model_admit.rs` — `model_missing_history_status`,
  `model_missing_history_records`, `model_restore_missing_history`, `model_reconcile_flow_history`.
- `rust/crates/pump-quant-junction/src/reserve_delta.rs` — `note_curve_snapshot_outcome` (producer path, shared with tests).
- `rust/crates/pump-quant-junction/src/bin/pq_daemon.rs` — `data/daemon_health.json` writer (exposes the
  drop COUNT only; no per-mint detail, no persistence).
- Build: `export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR=/training/mev_cargo_target`

## Unfinished acceptance criteria (resume in this order)
1. **Classify the producer outcome BEFORE any indefinite refusal.** Distinguish ordinary no-trade
   reserve change, unsupported/invalid observation, and confirmed/possible missing trade. A failed
   reserve-delta inference alone does NOT prove a trade was lost. Check whether an existing
   authoritative transaction path already supplies the event and its required fields; coverage must
   be established by identity + ordering + dedup ("another feed is running" is insufficient).
   Unknown stays explicitly unknown.
2. **Scope incompleteness to real dependencies.** Trace entry and management fields separately; do
   NOT invalidate reconciled inventory, cost basis or position age because market-flow history is
   incomplete. Unknown trader identity does not establish that wallet features are unaffected: trace
   shared-wallet dependencies and document cross-mint impact — neither a zero-impact assumption nor
   a global freeze. Preserve the trained prompt contract (no invented defaults, no silently omitted
   fields, no timer-cleared cumulative state, no legacy discretionary fallback). Keep discovered
   markets registered, subscribed and observed while inference is unavailable.
3. **Finish production durability.** Load continuity state before inference; persist changes safely;
   test crashes, failed writes, corrupt/missing records and real restart through the daemon
   persistence functions (temporary directory + fresh engine instance). Compaction must preserve
   mint scope AND the union of affected dependencies — today only mint + earliest instant +
   `source_id` are folded, so the dependency union is NOT preserved and this slice is incomplete.
   Bound and measure overhead; persistence must not stall feed processing or emergency protection.
   Acknowledging a gap is not repairing it. Keep recovery APIs refusing unsupported reconstruction.
4. **Prove protection on the affected held mint with NO pre-existing pending exit.** Incomplete
   inputs block Qwen dispatch while monitoring/reconciliation continue; trigger an agreed emergency
   condition through order → reconciled fill → inventory/cash update. Identify the earlier
   `SAFETY_OFF` cause; missing data must not be counted as an endpoint failure. Report "Qwen
   management unavailable" separately from the protection that still functions.
5. **Measure capability, then close the slice.** On available CPU replay report refusal causes,
   affected unique mints, entry readiness and held-position management downtime by venue and token
   age; separate real safety exclusions from parser/data-coverage failures. No invented forgone PnL
   and no threshold tuning to flatter coverage.
6. **Final step — smallest concrete recovery dependency:** which raw records are needed, whether
   existing captures contain them, and whether a rejected reserve delta can be recovered from an
   independent transaction source. Keep separate from this implementation; no new backfill platform.

## Standing constraints
No merge, no trading/model-feed launch, no training, no reboot, no `D:` writes, no fstab changes, no
Qwen-policy changes. PR #13 stays unmerged. Preserve rollback tag `rollback/pre-consolidation-959cee8c`
and branches `task/main-consolidation`, `task/docs-record-merge`, `task/dropped-print-detection`,
`task/durable-missing-history`.

## Mailbox
`handoff-linux/out/0026-missing-history-continuation/` (`MANIFEST.json` carries the sha256 map), with
`AGENT_STATE.json` and the append-only `handoffs.jsonl` ledger as canonical state.