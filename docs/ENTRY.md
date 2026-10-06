# ENTRY.md — current operating entry (consolidation N2)

> **This is the single current entry document.** It supersedes the many dated files in `docs/`
> that describe earlier designs. Where an earlier file conflicts with this one, **this file wins**;
> the earlier file is marked historical in §9. Disposition evidence: `docs/CONSOLIDATION_MANIFEST.md`.

- Branch: `task/main-consolidation` — **merged** to `main` as `a02a11a3` (PR **#10**). Base of `main`: `09e9194b`.
- Head roles (do not conflate): **tested code head** `0a962d55` (hosted tests, tested merge `4397268`); **reviewed head** `3a16f5ae`; **merged main** `a02a11a3`. The merge SHA is not the commit the earlier tests ran on.
- **Deployment acceptance is OPEN** — consolidation acceptance covers cleanup and tests only (§8).
- Rollback ref: tag `rollback/pre-consolidation-959cee8c`.
- Workspace root: `rust/`. Crate root: `rust/crates/`.
- Declared MSRV: `1.85` (`rust-version`). **See §7 — the workspace does not currently build on 1.85.**

---

## 1. Supported runtime (what runs)

| Binary | Crate | Role |
|---|---|---|
| `pq-daemon` | `pump-quant-junction` | **The supported live runtime.** Long-running paper/live loop: ingest → engine tick → exit/management → journals + status files. |
| `pq-watchdog` | `pump-quant-junction` | Supervises `pq-daemon`: restart-on-crash with backoff, reaps orphan LaserStream children, honours stop sentinels. **Windows-oriented** (`tasklist`/`taskkill`/`wmic`). |
| `pq-sell` | `pump-quant-junction` | Operator-invoked sell helper. |
| `pq-serving-latency` | `pump-quant-junction` | Serving-latency measurement harness. |
| `pq-engine-replay` | `pump-quant-junction` | Offline replay of a recorded event stream through the engine. |
| `junction_run`, `paper_session` | `pump-quant-junction` | Bounded-run predecessors of the daemon (no infinite loop). Kept for bounded experiments. |
| `pq_narrative_resolve` | `pump-quant-junction` | Name→narrative resolution utility. |
| `pq_replay`, `pq-refiner` | `pump-quant-evaluator` | Offline evaluator. **`pq-refiner` is no longer auto-spawned** (§4). |
| `pq-research-runner` | `pq-research-runner` | Offline research harness. |
| `pq_treasury` | `pump-quant-treasury` | Operator-initiated treasury CLI (policy-gated). **Not on any model or daemon path.** |

Shared capabilities preserved across the runtime: execution/accounting (`pump-quant-execution`,
`pump-quant-app` position + bankroll), safety (rug precursor, hard stop, cliff veto, circuit breaker,
kill switch, SAFETY_OFF), data collection (tape export, memory bank), enrichment
(signals/features/narrative/social/wallet-graph), replay (`pq-engine-replay`, `pq_replay`).

## 2. Data pipelines

1. **Capture** → `tools/stream-capture-rs` (+ `grpc-server-only`) records the live stream.
2. **Tape** → `pq-daemon` exports the engine's trades to the evaluator JSONL shape (`TapeExporter`, `TAPE_PATH`).
3. **Replay/eval** → `pq-engine-replay` / `pq_replay` run the tape through the engine; `pq_refiner` scores challenger configs offline.
4. **Training** → `training/` + `tools/training` + `tools/qwen_training_handoff` consume tapes/enrichment for the Qwen corpus (see §5).

## 3. Qwen interface (supported) vs repository tooling — **read this before wiring anything**

- The **supported trading interface** is the inference seam: `pump-quant-inference`
  (`src/seam.rs`, `src/lib.rs`) and `pump-quant-proposal`. The engine calls the model lane through that
  seam and receives a typed decision (HOLD/REDUCE/EXIT/ADD + size per the model's authority); the Rust
  side keeps solvency/per-position guards and fails closed. Daemon entry points that touch the model:
  `pq_daemon.rs`, `pq_sell.rs`, `pq_serving_latency.rs`, `async_sink.rs`.
- **Repository tooling is NOT the model's authority.** A clean repo, a passing CI run, or the presence
  of training/eval binaries does **not** give the trained model any tool-use capability or any authority
  to act. `pq_treasury`, `pq-refiner`, the research runner and the CLI utilities are human/operator
  surfaces; they are not callable by the model lane. Any change that would let the model reach a tool or
  a strategy knob is a **policy change**, not a refactor.

## 4. Retired (do not re-enable without an explicit decision)

- **Automatic evaluator/refiner promotion loop — RETIRED** (`63a80511`). The daemon no longer spawns
  `pq-refiner`. `--refiner-every-ticks` is accepted and ignored (logged `RETIRED`). A promotion file
  (`data/CONFIG_PROMOTION.json`) is hot-reloaded **only when an operator has approved that exact
  content**: `data/CONFIG_PROMOTION.approved` must hold the sha256 of the promotion bytes, otherwise
  it is refused and the live config is untouched. Approval is consumed on apply; the digest binding
  prevents an old approval from authorising a different replay. The auto-revert safety path is
  unchanged and needs no approval. Do not re-add a timer spawn or a self-approving path.
- **Legacy strategy authority** (`gate_evaluate`, ladder/trail/thesis/stall exits, arbitration): removed
  from the engine; reject code **29 (NO_ENTRY_AUTHORITY)** fires when no armed model lane exists. Legacy
  crates kept only as offline comparators (see manifest §MIGRATE).
- Historical launchers (`launch_rev16/17/29/36`, `start_rev36`, `launch_live`, `detach_launch`,
  `restart_daemon`, `run_watchdog`) were removed. Kept: `rust/launch_watchdog.sh`, `tools/pq-startup.ps1`.
- **Supervisor/constitution gate scripts — RETIRED** (`9c822551`): `scripts/ci_gate.py`,
  `supervisor/gates/hotpath_lint.py`, `scripts/materialize_tests.py`, the dossier machinery and the
  `.claude` edit-denial. Two of their checks protected retained behaviour and were **replaced**, not
  dropped: `tools/gates/portable_gate.py` (no-stubs blocking + §24/criterion-109 hot-path lint
  reported). The dossier/constitution checks were obsolete legacy-policy enforcement — do not restore.
  The old `secrets` check was warning-only; the blocking replacement is `.githooks/pre-push`
  (enable once per clone: `git config core.hooksPath .githooks`).

## 5. Configuration & state

- **Credentials/env** (daemon): `PQ_CREDS_FILE`, `HELIUS_API_KEY`, `LASERSTREAM_ENDPOINT`, `PUMPPORTAL_WS_URL`.
  Treasury only: `PQ_KEYPAIR_PATH`, `PQ_WALLET_ADDRESS`, `PQ_TREASURY_POLICY`, `PQ_TREASURY_AUDIT`, `PQ_RPC_URL`.
- **Stop sentinels** (checked each loop): `data/DAEMON_STOP` → graceful (exit 0); `data/EMERGENCY_STOP` → emergency (exit 99).
- **Status files**: `data/live_status.json`, `data/daemon_health.json` (includes `delta_trades_derived`,
  `delta_no_trade`, `delta_out_of_range`), `data/brain_analysis.json`, `data/cumulative_pnl.json`,
  `data/session_history.jsonl`.
- **Config**: the live `Config` is loaded at startup and hot-reloaded from `data/CONFIG_PROMOTION.json`
  when (and only when) it changes (see §4).

## 6. Windows handoff

- `rust/launch_watchdog.sh` + `tools/pq-startup.ps1` are the kept launchers.
- `pq-watchdog` uses Windows tooling (`tasklist`, `taskkill`, `wmic`) to supervise `pq-daemon` and reap
  orphaned `pq-laserstream-grpc` children; this path is **Windows-only**.
- **Parity/latency on Windows is NOT established** and remains a separate deployment-acceptance item (§8).
- Do not write to `D:` from this environment.

## 7. Toolchain / MSRV

- **Tested toolchain: `rustc`/`cargo` 1.99.0** (`stable-x86_64-unknown-linux-gnu`) — the toolchain the local
  acceptance run used.
- **MSRV `1.85` is declared but currently UNSUPPORTED by the workspace** (pre-existing, not introduced by
  lint cleanup): dependencies require ≥1.86 (`idna_adapter@1.2.2`), and `pump-quant-core/src/reducer.rs`
  uses `is_multiple_of` (stable 1.87). Do **not** silently raise the declared MSRV or downgrade deps.
  Resolve deliberately (pin deps / replace the API) in its own change.

## 8. Not closed by consolidation (deployment acceptance items, separate)

Real-Qwen/feed integration; Windows serving parity & latency; live restart recovery; AMM sell economics;
north-star validation. Cleanup, synthetic tests and a green CI do **not** close these.

**Status: OPEN.** Deployment acceptance is not granted by the consolidation merge (`a02a11a3`).

## 9. Historical / superseded documents

`docs/ARCHITECT_*`, `docs/ARCH_*`, `docs/*_V2.md`, `docs/ENTRY_*`, `docs/EXIT_*`, dated
`docs/*_2026-*.md`, `docs/BACKTEST.md`, `docs/CONSOLIDATION_MANIFEST.md` addenda describe **earlier or
superseded** designs and dispositions. They are evidence, not instructions. `docs/CONSOLIDATION_MANIFEST.md`
is the disposition ledger (refs, removals, uncertain crates) — consult it for *why*, but follow **this**
file for *how to run*.

## 10. Operational commands (current)

```bash
# portable gate (no-stubs blocking; hot-path lint reported) - run from the REPO ROOT
python3 tools/gates/portable_gate.py --repo .            # add --strict-hotpath to make the lint blocking
# enable the blocking credential guard once per clone
git config core.hooksPath .githooks

# build + full local gate (CI-equivalent)
cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace

# run the supported runtime (paper)
cargo run --release --bin pq-daemon -- --junction-cap 4096
# stop it
touch data/DAEMON_STOP      # graceful;  data/EMERGENCY_STOP for emergency (exit 99)

# supervise (Windows)
bash rust/launch_watchdog.sh

# offline replay / eval
cargo run --release --bin pq-engine-replay -- <tape>
cargo run --release --bin pq_replay -- <tape>
```

**CI:** GitHub Actions workflow `rust-ci` (`.github/workflows/gate.yml`) runs on the pushed branch:
Record toolchain/runner CPU/effective flags · **Portable gate** · Format check · Clippy
(deny warnings) · Build · Tests. It uses a **CI-only** `RUSTFLAGS=-C target-cpu=x86-64-v3`
(the deployment `znver5` pin in `rust/.cargo/config.toml` is unchanged; hosted runners are not
Zen5). Local and hosted runs are reported separately (see the merge-review package).
