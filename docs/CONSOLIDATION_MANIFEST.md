# CONSOLIDATION MANIFEST - current

**This file is the single authoritative inventory and status.** It replaces the accumulated
addenda that used to live here; that earlier text is preserved verbatim, warts and all, in
[`CONSOLIDATION_HISTORY.md`](CONSOLIDATION_HISTORY.md) - which is explicitly HISTORICAL and
contains decisions later reversed. Where the two disagree, this file wins. Operating entry
points, commands and the historical-alternatives index are in [`ENTRY.md`](ENTRY.md).

Last updated: 2026-10-05. **Accepted code head: `d7596114`** (tested merge `2fabc312`). The
branch tip may be a later docs-only commit; the acceptance in section 2 is bound to the code
head, not to the tip SHA.

---

## 1. Status

| Item | Value |
|---|---|
| Consolidated branch | `task/main-consolidation` |
| Current code head | `d7596114` (docs-only commits may follow it) |
| Base (`main`) | `09e9194b` (2026-09-26) |
| Rollback ref | tag `rollback/pre-consolidation-959cee8c` (local + remote) |
| PR | **#10** - `main consolidation: Qwen-first main` - **DRAFT, not merged** |
| Open PRs in the repo | #10 only |

## 2. Acceptance (recorded, with tested SHAs)

**Local CI-equivalent** - Linux, `rustc`/`cargo` **1.99.0** (`b940084d7`, 2026-09-28),
`CARGO_INCREMENTAL=0`:
- `cargo fmt --all -- --check` -> clean
- `cargo clippy --workspace --all-targets -- -D warnings` -> exit 0
- `cargo build --workspace` -> exit 0
- `cargo test --workspace` -> exit 0, 0 failed (like-for-like count in section 2b)

**Hosted GitHub** - workflow `rust-ci`, `pull_request` event (tests the **merge** of the head
into `main`, not the head alone):

| Run | Source head | Tested merge SHA | Runner CPU | Result |
|---|---|---|---|---|
| 18 | `d7596114` | `2fabc31262712f2de8eea31b817efaced265df27` | AMD EPYC 7763 | SUCCESS |
| 19 | `a69e7e6f` | (docs-only head) | - | SUCCESS |
| 20 | `1811e5fb` | `b3ec6fad8111c1cf83756982defb659c2e8927f1` | AMD EPYC 9V45 | SUCCESS |
| 21 | `161efe45` | `b3ec6fad8111c1cf83756982defb659c2e8927f1` (merge ref unchanged; run 21 re-tested the same merge for the docs-only tip) | AMD EPYC 9V45 | SUCCESS |

- all steps green each time: Record toolchain/runner CPU/effective flags · Portable gate ·
  Format check · Clippy (deny warnings) · Build (portable/dev profile) · Tests
- runs 18-21 are the only runs on the finalised workflow; 18 and 20 used DIFFERENT runner CPUs
- toolchain **rustc 1.99.0** (`b940084d7`) · effective `RUSTFLAGS=-C target-cpu=x86-64-v3` ·
  cache key separated by toolchain/arch/flag-hash (`Linux-X64-rust1.99.0-cargo-<lock>-<flags>`)
- the CPU fix is **validated on the tested runners** (AMD EPYC 7763 and 9V45) - not claimed
  universally deterministic; a runner lacking even x86-64-v3 would still need the baseline lowered.

**Superseded/misleading results - do not cite as current:**
- Runs 12-14 and 16 (`18dc0ae2`, `442996b8`, `9cfa3a58`, `b737c2d9`) FAILED. Runs 12-14 failed at
  Format check (pre-rustfmt). Run 16 failed at Clippy with **exit 101 but no lint**: the compiler
  process (`clippy-driver`) died with **SIGILL** compiling
  `pump-quant-inference (test "management_parity")`. Cause: the deployment pin
  `rust/.cargo/config.toml -> rustflags=["-C","target-cpu=znver5"]` was inherited by build
  scripts / proc-macros (no `--target` is passed), so Zen5-only instructions ran on a non-Zen5
  runner. Run 15 (`63a80511`) happened to pass on a more capable runner - that pass was luck, not
  a green gate.
- **Fix (in `d7596114`):** CI-only `RUSTFLAGS=-C target-cpu=x86-64-v3` in the workflow. The
  deployment Zen5 pin is UNCHANGED; all other flags and `-D warnings` are preserved; the cache key
  is separated by toolchain / arch / flag-hash.

## 2b. Test-count reconciliation (like-for-like, from the hosted CI log)

Measured from run 21's **Tests** step log (the same command CI runs: `cargo test --workspace`):

| Measure | Value |
|---|---|
| `test result` lines | **491** (467 test/bin targets + 24 doctest targets) |
| targets with >0 passed | 453 |
| **total passed** | **3,421** |
| **failed** | **0** |
| doctests | 0 passed (24 empty doc targets) |

**Earlier figures reconciled - they were NOT like-for-like:**
- `3,486` was a LOCAL aggregate produced by summing `test result: ok. N passed` lines from one
  local run, with a different parse and environment (and it counted doctest lines).
- `3,195` was a draft-manifest transcription, not a measurement.
- Neither is a by-target count, so neither should be compared to the other. The authoritative,
  reproducible figure is **3,421 / 0 across 491 targets** from the hosted log.

**Actual removals vs counting:** between base `09e9194b` and the head, `#[test]` functions went
**3,424 -> 3,506 (+82)**; **16 test files were added** and **10 deleted**. The 10 deletions are:
- 8 in `pump-quant-clock/tests/*` and `pump-quant-tape/tests/*` - the **crates themselves were
  removed** (H1), so their tests went with them;
- 2 in `pump-quant-app/tests/` (`entry_exit_frontier.rs`, `flow_persistence_laws.rs`) - deleted in
  **`9deb484e`** ("retire deterministic entry selection ... **85 legacy-strategy tests retired
  (ledger in manifest)**; 708 infrastructure/model tests unchanged"). Those tested the **retired
  legacy strategy**; `entry_exit_frontier` is still referenced by the surviving
  `prod_config_parity.rs`. This is a ledgered, deliberate retirement of obsolete strategy rules -
  not a concealed loss.

## 3. Capability inventory (current)

**Supported runtime binaries** (workspace, `rust/crates` + `tools/stream-capture-rs`):
`pq-daemon` (the supported live runtime), `pq-watchdog`, `pq-sell`, `pq-serving-latency`,
`pq-engine-replay`, `junction-run`, `paper-session`, `pq-narrative-resolve` (junction);
`pq-replay` and `pq-refiner` (evaluator - **offline / operator-invoked only**); `pq-treasury`
(CLI, never model-callable); `pq-research-runner` (research).

**Retained (in the daemon build; 23 internal crates resolve):** execution, accounting, safety
(`defense_in_depth` cliff veto / circuit breaker / kill switch), data collection (tape export,
memory bank, trade journal), enrichment, replay, narrative, market-state, wallet-graph,
watchlist, signals, features, governance, simulator, strategy, evaluator, inference, proposal,
protocol, ingest, domain, core, brain, app, junction.

**Removed / retired (completed):**
- tracked build outputs and dead launchers from `main`
- legacy strategy gate / `gate_evaluate` engine path
- the supervisor/constitution gate steps (`ci_gate.py`, `hotpath_lint.py`, `materialize_tests.py`)
  - deleted in **`9c822551`** (2026-09-18, "R0 baseline"). The two checks in `ci_gate.py` that
  protected RETAINED behaviour were **replaced, not dropped**: `tools/gates/portable_gate.py`
  (no-stubs blocking + hot-path lint reported; see section 7 for the recorded hot-path debt).
  The dossier/`materialize_tests`/`.claude`-edit-denial/constitution checks were obsolete
  legacy-policy enforcement and are correctly NOT restored. The secrets check was
  WARNING-ONLY in `ci_gate` (repo policy accepted committed-credential risk); it is now the
  blocking `.githooks/pre-push` credential guard.
- `pump-quant-tape`, `pump-quant-clock`
- the daemon's **automatic refiner promotion loop** (`RefinerSpawner` spawn) - `d7596114`'s
  predecessor

**Quarantined (no runtime caller in the daemon graph):** `pump-quant-telemetry`,
`pump-quant-treasury` (CLI only). `pq-research-runner` and `pump-quant-journal` are outside the
daemon graph.

**No dead-code claim is made.** Disposition rests on by-dependents evidence, not a dead-code
sweep. `cargo build`/`clippy` do not prove the absence of unused code.

## 4. Configuration authority

- Config values come from the workspace `Config` (`Config::dev_portable()` + CLI/overrides).
- **`data/CONFIG_PROMOTION.json` alone does NOT change trading configuration.** `try_reload_config`
  requires `data/CONFIG_PROMOTION.approved` to contain the lowercase-hex SHA-256 of the exact
  promotion bytes; missing/stale/mismatched approval is refused, the live config is untouched and
  the file is not consumed. The operator action is bound to one config/version.
  Approve with: `sha256sum data/CONFIG_PROMOTION.json | cut -d' ' -f1 > data/CONFIG_PROMOTION.approved`
- Nothing in the daemon spawns the refiner any more, so **automatic evaluation output cannot change
  trading configuration** - it must be materialised and explicitly approved by an operator.
- **Who/what can write an approval:** only a human/operator action on the host filesystem -
  writing `data/CONFIG_PROMOTION.approved` with the sha256 of the intended promotion bytes
  (`sha256sum data/CONFIG_PROMOTION.json | cut -d' ' -f1 > data/CONFIG_PROMOTION.approved`).
  Nothing in the daemon, the refiner, or the evaluator writes it; no code path self-approves.
- **Is the approval consumed?** Yes. On a successful apply the promotion file is deleted and
  `last_mtime` is set, so the same file cannot be applied twice. A refusal (missing/mismatched
  approval) does NOT consume either file and does not advance `last_mtime`, so it is re-checked
  when the operator fixes the approval.
- **Replay prevention:** the approval binds to the exact **content digest**, not to a filename or
  mtime. Editing the promotion after approval changes the digest and the approval no longer
  matches; re-writing the *same* content requires the operator to approve that content. An old
  approval therefore cannot authorise an unintended replay - it authorises exactly one byte
  sequence.
- **Auto-revert is preserved** and does not go through the promotion path: it reads the archived
  champion config and applies it **directly** to the live `Config` (targets the intended
  previously-approved champion, recorded by fingerprint in `data/auto_revert_state.json`), so it
  needs no approval and cannot be blocked by a missing one - emergency protection is intact.
- **Legacy authority cannot return.** The corrected statement: agreed **emergency exits remain
  intentionally autonomous** (hard stop, rug precursor, cliff veto, circuit breaker, kill switch,
  `EMERGENCY_STOP`) - that autonomy is a retained safeguard, not legacy authority. What cannot
  return is legacy **discretionary** authority: the retired entry/exit gates are absent at code
  level, so no config value and no auto-revert can re-enable discretionary legacy selection.
- Emergency safeguards retained: cliff veto, circuit breaker, kill switch, `EMERGENCY_STOP`
  sentinel (all autonomous by design - see above).

## 5. Branch / ref inventory (dates, unique-work disposition)

| Ref | Date | Head | Disposition |
|---|---|---|---|
| `task/main-consolidation` | 2026-10-05 | `d7596114` | **RETAIN** - PR #10 draft; the consolidation line |
| `main` | 2026-09-26 | `09e9194b` | **RETAIN** - base |
| `task/p6-management-slice` | 2026-10-05 | `959cee8c` | **RETAIN (rollback anchor)** - held-state restore + stale-held callout (suite 1443/0). Tagged `rollback/pre-consolidation-959cee8c`. Unique work: durable held-state ledger + edge-triggered stale callout. |
| `task/p6-price-limit-threading` | 2026-10-04 | `6209624c` | **CONTAINED (reporting error corrected)** - `6209624c` is an **ANCESTOR** of the head with **0 unique commits** (`git merge-base --is-ancestor`; `git log <head>..6209624c` empty). Its capabilities are PRESENT and TESTED in the head (section 5b). |
| `task/training-code-of-record` | 2026-09-22 | `4a8d2cc2` | **INCLUDED (runtime-isolated)** - `4a8d2cc2` is an **ANCESTOR** with **0 unique commits**. All **5** integrated training commits (`59f39814`, `586c119a`, `4ff76bf6`, `c981ee7e`, `4a8d2cc2`) are in the head's history; `training/` (291 files) and `tools/` (407 files) are present. "Isolated research" describes RUNTIME scope only - it is not a statement that the code or pipelines were dropped. |
| `origin/task/north-star-build` | 2026-09-09 | `7d64b50a` | **UNRESOLVED - do not merge** - north-star; validation outstanding (see section 7) |
| tag `rollback/pre-consolidation-959cee8c` | - | `959cee8c` | Local + remote rollback anchor |

## 5b. P6 price-limit-threading capabilities - present at the head (evidence)

An ancestor can still have had behaviour removed later, so each capability was located in the
final tree (not inferred from ancestry):

| Capability | Current implementation | Current test |
|---|---|---|
| Order-bound reconciliation (order id/attempt/quantity) | `pump-quant-app/src/engine/model_manage.rs` (`order_bound`, `model_mgmt_add_plan_at`); `journal_log.rs`; `main.rs` | `tests/lifecycle_paper_orders.rs::lifecycle_m_evidence_must_match_order_attempt_and_quantity_or_touch_nothing`, `::lifecycle_p_evidence_through_the_normal_event_path_is_order_bound` |
| `ModelOrderEvidence` event path | `pump-quant-app/src/event.rs`, `engine/model_admit.rs`, `engine.rs`; `pump-quant-junction/src/event_stream.rs` | `tests/lifecycle_paper_orders.rs` (16 tests) |
| `ReconFault` journaling (durable, per-order faults) | `engine/model_admit.rs`, `engine/model_safety.rs`, `journal_log.rs`, `main.rs`, `safety_off.rs`, `engine.rs` | `::lifecycle_g_not_filled_then_credible_filled_is_a_fault_that_blocks_exposure_until_resolved`, `::lifecycle_o_closed_position_conflict_stays_blocked_with_durable_evidence` |
| Resolution unwinds only the owning order | `engine/model_manage.rs`, `journal_log.rs` | `::lifecycle_k_resolution_unwinds_only_the_order_that_the_authority_says_never_filled`, `::lifecycle_n_evidence_for_one_order_never_unwinds_another_orders_or_preexisting_inventory` |
| Decision-time state version | `engine/model_manage.rs` (`mgmt:discard:state_version_changed`) | exercised by the lifecycle suite |

**Verified by execution, not by inspection:** `cargo test -p pump-quant-app --test
lifecycle_paper_orders` -> **16 passed, 0 failed** on the head.

**Conclusion: the earlier manifest claim was a REPORTING ERROR, now corrected.** No missing
functionality was found and nothing had to be integrated.

## 6. Retained legacy research dependencies (explicit)

Measured with `cargo tree -p pump-quant-junction -e normal`:

- The daemon build resolves **23 internal crates**. `pq-research-runner`, `pump-quant-journal`,
  `pump-quant-telemetry` and `pump-quant-treasury` are **absent**.
- `pump-quant-strategy` IS present, via `pump-quant-app` -> `pump-quant-strategy`. This is **not**
  dead legacy: `pump-quant-app` uses it in production (17 references in `engine.rs`; also
  `gate.rs`, `cost_model.rs`, `curve_authenticity.rs`, `extraction_risk.rs`, `market_context.rs`,
  `structure.rs`). Provided: `economic_gate`, `exit_ladder`, `safety_integrity`, `thesis`,
  `entry_arbitration`, `hazard_estimator`, `probe_ladder`, `scalp_position`, `calibration_budget`.
- `pump-quant-evaluator` IS present, used by the daemon **only** for `defense_in_depth` (safety)
  and `LifecycleStage`, and by `pump-quant-app` for `promotion_verdict` (report-only readiness).
  Removing it was **not** done, because doing so would remove safety - explicitly out of scope.
- A "no junction source reference to strategy" observation does **not** establish absence
  transitively; the graph above is the evidence.

## 7. Open items and exceptions

**Proposed exceptions (pending operator acceptance - not yet accepted):**
1. Hosted CI depends on a CI-only CPU baseline (section 2). If the deployment pin changes, the CI
   baseline is unrelated and stays.
2. **MSRV 1.85 is declared but unsupported.** `rust-version = "1.85"`, but deps need >= 1.86
   (`idna_adapter`) and `pump-quant-core/src/reducer.rs` uses `is_multiple_of` (1.87). Tested
   toolchain is 1.99.0 only. The declared MSRV was NOT raised and deps were NOT downgraded.
3. Deployment acceptance is **separate and open**: real Qwen/feed operation, Windows parity &
   latency, live restart recovery, AMM economics.
4. Local `cargo test` could not be re-run for this pass: the host root filesystem hit 0 bytes free
   and the linker aborted (`collect2: fatal error: ld terminated with signal 7 [Bus error]`). The
   authoritative full-suite result is the hosted CI Tests step (section 2b); local fmt/clippy/build
   were green on the code head before the disk filled.
5. **Hot-path lint debt (recorded, not concealed).** The §24/criterion-109 lint is re-enabled as a
   REPORTED check in `tools/gates/portable_gate.py` (and the CI step). It currently reports **572
   violations** with the authoritative scope in `rust/lint_rules.yaml` - a scope that grew after
   2026-09-20 (app decision-path modules, marked there as "NOT linted yet") and was never enforced.
   Making it blocking would fail on pre-existing, never-enforced debt, so it is reported with counts
   and flippable via `--strict-hotpath` once the debt is cleared. Clearing it is a separate
   cleanup, not part of this pass.
6. The `secrets` check was WARNING-ONLY in the retired `ci_gate`; the blocking replacement is
   `.githooks/pre-push`, which requires `git config core.hooksPath .githooks` to be active - not
   set in this environment, so it is a documented operator step, not an enforced CI check.

**Open input-integrity issue (paper-readiness gap) - not fixed here:**
- **Upstream dropped-print detection.** A print dropped BEFORE the flow reducer (e.g. a
  reserve-delta the derivation rejects) is invisible to `decision_join`'s completeness check,
  which only sees prints that reached it. Flow completeness is therefore **not** established by
  reserve age or by aggregate counters.
  - Owner: ingest/junction (flow-envelope), to be assigned.
  - Test: needed - a dropped-print fixture proving the served flow block is refused (not rendered
    as quiet) when an upstream print is missing.
  - Readiness consequence: a flow window could be labelled complete while missing a print; do not
    treat flow completeness as proven until that test exists.
  - What exists today: `JoinRefusal::FlowMetaMissing` / `FlowAggregatesIncomplete` refuse when a
    *received* print lacks fee/CU/slot/trader, and counters `delta_no_trade`,
    `delta_out_of_range`, `pp_trades_received` vs `pp_trades_enqueued`, `ls_account_unresolved`,
    junction `overflow_dropped` exist. These are aggregate diagnostics, not per-reason proof.

## 8. Where things live

- Operating entry (runtime, pipelines, Qwen interface vs tooling, config, Windows handoff, ops,
  historical-alternatives index): [`ENTRY.md`](ENTRY.md)
- Historical evidence trail (superseded decisions, explicitly not instruction):
  [`CONSOLIDATION_HISTORY.md`](CONSOLIDATION_HISTORY.md)
- Rollback: tag `rollback/pre-consolidation-959cee8c`
- **Windows mailbox bundle (the verified cross-OS handoff):**
  `handoff-linux/out/p6-consol-161efe459834/` on the UBUSEED partition (UUID `5040-AC61`, mounted
  `/mnt/seed`, left **read-only**). Contains `START_HERE.md`, `MANIFEST.json` (sha256 map) and
  `mev_bot_consol_161efe45.bundle` (thin bundle; base `09e9194b`). Ledger seq **21**; readback
  hashes verified; clean-checkout verified (0 dirty, 1716 tracked files). Supersedes
  `p6-consol-d75961146c9d` and earlier.
- Training-handoff material inside the repo: `tools/qwen_training_handoff/` (launch/monitor/resume
  + `RUNBOOK.md`) and `training/` (291 files). This is repo tooling - NOT the cross-OS bundle.
- Dated design docs under `docs/*.md` are HISTORICAL unless referenced from `ENTRY.md`.