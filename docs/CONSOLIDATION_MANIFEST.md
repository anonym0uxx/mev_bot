# Main consolidation manifest (Qwen-first)

Branch `task/main-consolidation`, rollback ref `rollback/pre-consolidation-959cee8c` (pushed tag, = the management tip before any removal).
Base of main: `09e9194b` (2026-09-26). Nothing here rewrites history or deletes a branch.

## Refs inventory (accessible; fetched 2026-10-05)

| Ref | Head | Date | Relationship to the management tip |
|---|---|---|---|
| `main` / `origin/main` | 09e9194b | 2026-09-26 | ancestor (behind by 34+ commits) |
| `task/p6-price-limit-threading` | 6209624c | 2026-10-04 | ancestor, fully contained |
| `task/p6-management-slice` | 959cee8c | 2026-10-05 | integration head before consolidation |
| `task/training-code-of-record` | 4a8d2cc2 | 2026-09-22 | 5 unique commits, 0 conflicts -> **INTEGRATED (E1)** |
| `task/north-star-build` | 7d64b50a | 2026-09-09 | 17 unique commits, 172 files, 2 trivial conflicts -> **NOT integrated (UNCERTAIN, below)** |

Tags: none before this work (now `rollback/pre-consolidation-959cee8c`). Stashes: 0. Worktrees: the original checkout and `/home/alon/build/mev_bot_consol`.
PR history (GitHub API, 8 PRs, all merged 2026-07-22): bot-build, docs/readme-revamp, perf/latency-hotpath, feat/social-ingest, feat/social-attention x2, docs/readme-phase-a, docs/tier0-wording. No open PRs.

Required capabilities in the integration head: entry (stream discovery, AMM), management (HOLD/REDUCE/EXIT/ADD), SAFETY_OFF, held-state restore - all in `959cee8c` and ancestors of this branch (verified with `git merge-base --is-ancestor`).

## Disk, separately measured (2026-10-05)

- Build cache: `rust/target` was 26 GB (debug 25 GB). Regenerable, no running cargo/rustc/daemon/llama; `target/debug` removed. Disk 98% -> 78%.
- Tracked artifacts: 3,859 files / 1,038,553,193 bytes uncompressed in HEAD (bench/target, social-ingest-rs/target, grpc-server-only/target).
- Datasets/models/evidence/journals/credentials: not touched.
- Git objects: `.git` 4.0 GB pack. **Unchanged** - no history rewrite, so history savings are 0 by design.

Savings: checkout -1.04 GB (-3,868 tracked files); disk -25 GB (cache, regenerable); history 0.

## Manifest

### KEEP (Qwen support system) - all crates with a real caller or entry point
`pump-quant-app` (engine, decision_join, model_* lanes, position/wallet accounting, held_state, safety_off), `-inference`, `-proposal`, `-brain`, `-ingest`, `-protocol`, `-execution`, `-junction` (pq-daemon, pq-watchdog, pq-sell, pq-serving-latency, junction-run, paper_session, pq-engine-replay, pq_narrative_resolve), `-domain`, `-core`, `-market-state`, `-narrative`, `-wallet-graph`, `-watchlist`, `-features`, `-signals`, `-memory`, `-social`, `-simulator`, `-governance`, `-evaluator` (pq_replay, pq-refiner), `pq-research-runner`, `-strategy` (see MIGRATE).
`tools/stream-capture-rs`, `tools/social-ingest*`, `tools/firecrawl-*`, `tools/data-pipeline`, `tools/training`, `tools/qwen_training_handoff`, `training/` (integrated), `docs/`, `.github/workflows/gate.yml`, `.githooks/pre-push`, migrations-free runtime files.

### MIGRATE / ISOLATE (legacy strategy authority; kept as research comparator, cannot reach the model path)
- `pump-quant-strategy` exit_ladder / entry_arbitration / entry_mode_leaves / probe_ladder / calibration_budget, and the legacy gate in `engine.rs` (`gate_evaluate`, ladder/trail/thesis/stall exits in `position.rs`).
  Real callers exist (`engine.rs` legacy path, replay/golden digests), so they are NOT deleted: golden digests pin them. They are fenced instead:
  * model-armed engine never calls `gate_evaluate` (tested: `model_admit_candidate` branch);
  * model-managed positions skip ladder/trail/thesis/time-stop (existing tests) and legacy `scale_in` is now structurally refused (slice C);
  * new test `no_legacy_config_value_can_close_or_resize_a_model_managed_position` sets every legacy ladder/trail/stall/hold config to extreme values and shows no close and no resize; mutant proved it can fail.
  Removal of this code is gated on retiring the golden digests - an operator decision, not done here.

### REMOVE (done in this branch; each had 0 referencing files outside itself)
- A: tracked build outputs + runtime snapshots (3,868 files); `.gitignore` fixed.
- B: 8 abandoned launchers (`launch_rev16/17/29/36`, `start_rev36`, `launch_live`, `detach_launch`, `restart_daemon`, `run_watchdog`, `.restart-daemon`), TS-era `test/unit`, SQL `migrations/` (no sqlx/rusqlite anywhere), `tmp/` scratch, `data/onchain-audit.md` and `data/launch_test.log`.
  Kept: `rust/launch_watchdog.sh` + `tools/pq-startup.ps1` (referenced by docs and each other).

### UNCERTAIN (not removed; reason + what resolves it)
- `pump-quant-clock`, `-journal` (journal has one Rust caller: `trade_journal.rs`), `-tape`, `-telemetry`, `-treasury`: no runtime caller except `-journal`; `-tape` is the c12 training-tape sink with no runtime user yet. Resolve: operator confirms whether tape/telemetry are planned runtime sinks. Kept.
- `tools/*rcap*.py` one-off diagnostics (0 refs): kept pending operator word; they hold wallet/mint context.
- `analysis/` Python/Kelly scripts: `sizing_validator.rs` cites `kelly_montecarlo.py` as its source of truth -> kept.
- `task/north-star-build` (17 commits, 52k lines): see below.

### north-star decision (needs the operator)
Not merged. Findings: a unique Base58 zero-byte fix in `grpc-server-only/src/encoding.rs` is already present in HEAD in equivalent form (different code, same behaviour; its extra tests are not). Its Python tests (2,234) pass 2,080 on Linux; 154 fail from environment, not logic: Windows-only atomic no-clobber publish (29), byte-pinned CRLF files (the pinned registry hash only matches with CRLF line endings, so a Linux checkout fails it), `yt_dlp` missing, `D:/` paths. They cannot be validated on Linux. Merge is mechanically easy (2 conflicts; one is byte-identical, one is a rewrite of a doc). Recommended: run its suite on Windows, then merge deliberately. Credential scan of both integrated/considered branches: 6 hits, all inspected as identifiers/test fixtures, no secrets.

## Compatibility
No serialized enum, journal or position schema was changed or renumbered. Exit-reason app code 10 / brain ordinal 8 are append-only (earlier slice). Held ledger is a new file, schema 1, strict reader.

## Not closed by this work
Actual daemon with real Qwen; Windows serving parity/latency; live reserve freshness; AMM sell economics; north-star validation. Cleanup and synthetic tests do not close these.
