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
- `cargo test --workspace` -> exit 0, 0 failed (3195 tests + doctests)

**Hosted GitHub** - workflow `rust-ci`, `pull_request` event (tests the **merge** of the head
into `main`, not the head alone):

| Run | Source head | Tested merge SHA | Runner CPU | Result |
|---|---|---|---|---|
| 18 | `d7596114` | `2fabc31262712f2de8eea31b817efaced265df27` | AMD EPYC 7763 | SUCCESS |
| 19 | `a69e7e6f` | (docs-only head) | - | SUCCESS |
| 20 | `1811e5fb` | `b3ec6fad8111c1cf83756982defb659c2e8927f1` | AMD EPYC 9V45 | SUCCESS |

- all five steps green each time: Record toolchain/runner CPU/effective flags · Format check ·
  Clippy (deny warnings) · Build (portable/dev profile) · Tests
- toolchain **rustc 1.99.0** (`b940084d7`) · effective `RUSTFLAGS=-C target-cpu=x86-64-v3` ·
  cache key separated by toolchain/arch/flag-hash (`Linux-X64-rust1.99.0-cargo-<lock>-<flags>`)
- two DIFFERENT runner CPUs both green: the gate is deterministic across runner types now.

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
- **Auto-revert is preserved** and does not go through the promotion path: it reads the archived
  champion config and applies it directly to the live `Config`. It cannot restore *legacy
  authority*: the retired gates are absent at code level, so no config value can open, close or
  resize a model-managed position without a model verdict.
- Emergency safeguards retained: cliff veto, circuit breaker, kill switch, `EMERGENCY_STOP`
  sentinel.

## 5. Branch / ref inventory (dates, unique-work disposition)

| Ref | Date | Head | Disposition |
|---|---|---|---|
| `task/main-consolidation` | 2026-10-05 | `d7596114` | **RETAIN** - PR #10 draft; the consolidation line |
| `main` | 2026-09-26 | `09e9194b` | **RETAIN** - base |
| `task/p6-management-slice` | 2026-10-05 | `959cee8c` | **RETAIN (rollback anchor)** - held-state restore + stale-held callout (suite 1443/0). Tagged `rollback/pre-consolidation-959cee8c`. Unique work: durable held-state ledger + edge-triggered stale callout. |
| `task/p6-price-limit-threading` | 2026-10-04 | `6209624c` | **RETAIN / not yet folded** - order-bound reconciliation, ModelOrderEvidence, ReconFault journal, decision-time state version. Unique work not in the consolidation head. |
| `task/training-code-of-record` | 2026-09-22 | `4a8d2cc2` | **ISOLATED RESEARCH** - training code of record sync (`/training/v2`). Not part of the runtime. |
| `origin/task/north-star-build` | 2026-09-09 | `7d64b50a` | **UNRESOLVED - do not merge** - north-star; validation outstanding (see section 7) |
| tag `rollback/pre-consolidation-959cee8c` | - | `959cee8c` | Local + remote rollback anchor |

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

**Accepted exceptions (finite):**
1. Hosted CI depends on a CI-only CPU baseline (section 2). If the deployment pin changes, the CI
   baseline is unrelated and stays.
2. **MSRV 1.85 is declared but unsupported.** `rust-version = "1.85"`, but deps need >= 1.86
   (`idna_adapter`) and `pump-quant-core/src/reducer.rs` uses `is_multiple_of` (1.87). Tested
   toolchain is 1.99.0 only. The declared MSRV was NOT raised and deps were NOT downgraded.
3. Deployment acceptance is **separate and open**: real Qwen/feed operation, Windows parity &
   latency, live restart recovery, AMM economics.
4. `task/p6-price-limit-threading` unique work is not folded into the head.

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
- Training handoff bundle: `tools/qwen_training_handoff/` (see `START_HERE.md`)
- Dated design docs under `docs/*.md` are HISTORICAL unless referenced from `ENTRY.md`.