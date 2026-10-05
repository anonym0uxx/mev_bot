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


---
## Addendum 2 (decisions applied) - head after slices G, H1, H1b, I

### Legacy strategy retirement (H1, 9deb484e + 3c2c9242)
- Removed from Engine: `gate_evaluate` and arbitration (deterministic entry selection) and 14 helpers only they used
  (entry-mode confirmation, brain entry-at-admit, size haircut, deployer-screen multiplier, tracked/smart-money boosts,
  coordinated-funding detector, fee-floor verdict, cost probes, counterfactual veto) plus 7 dead constants. engine.rs -2.1k lines.
- With no armed model lane a candidate is refused with the NEW reject code 29 (NO_ENTRY_AUTHORITY). Codes 0-28 keep their meaning
  in a frozen history registry (journals and live_status reject_counts serialize them). Nothing renumbered.
- Kept shared infrastructure: open/fill/reconcile accounting, exits via the model lane, Rust hard safeguards (rug precursor, hard stop),
  persistence, SAFETY_OFF, all serialized enums and position schemas.
- Tests: 85 failed after removal (all drove the retired gate/arbitration/sizing). Each was removed BY NAME, ledger in
  `consolidation/retirement_ledger.json` (85 entries, with the panic text). 2 files deleted outright (entry_exit_frontier, flow_persistence_laws).
  No digest was regenerated. golden_digest.rs keeps 2 passing tests; the 6 failed ones asserted the retired strategy's journal digest/behaviour.
  bankroll_origin: the live-vs-paper admitted-size comparison was removed; its sizing-basis claim (floor/deployable track the reconciled
  wallet, not the seed) stays asserted in parts 1-2 of that test.
- Coverage check of the retired set by infrastructure keyword (script coverage_audit.py): fee 10 retired / 19 remaining, fill 6/31,
  order 6/20, reconcil 2/15, wallet 1/10, persist 3/6, restore 1/6, decode 2/4, quote 5/3, bankroll 6/5, journal 13/1.
  This is a name heuristic, NOT proof. The weakest rows are journal and quote.
- NOT done: the strategy crates (pump-quant-strategy etc.) and the offline comparator still exist, because the evaluator and
  pq-research-runner bins depend on them. Isolating them behind a feature or moving them out of the daemon's dependency tree is open.
  The old implementation is preserved at tag rollback/pre-consolidation-959cee8c.


### Uncertain crates - decision matrix (evidence: Cargo dependents, bins, scripts, CI, configs, docs, Windows tasks)
- pump-quant-tape: capability = durable append-only sink writing the 13-key trades.jsonl line shape for the c12 pipeline.
  Dependents: none (Cargo, bins, scripts, CI). Consumers of the FORMAT are training/*.py, which read files produced by
  stream-capture, not this crate. Replacement: tools/stream-capture-rs + the renormalizer. DECISION: REMOVED (I, 912cbeed).
- pump-quant-clock: capability = Clock trait + ReplayClock. Dependents: none. Only mentions: README, docs/architecture.md, and a
  crate-name map in build_rust_gold_v1.py (a label, no import). Replay uses its own sequencing. DECISION: REMOVED; architecture.md line marked.
- pump-quant-telemetry: capability = integer drawdown/floor/open-count snapshot + alert bands. No dependents, but it is the only
  implementation of that alert surface (the app has an unrelated shadow-equity drawdown). DECISION: KEPT, QUARANTINED
  (not wired). Needs an owner decision to wire it to alerts or remove; no evidence it is required today.
- pump-quant-treasury: capability = policy-gated SOL transfers from the hot wallet, with its own bin pq_treasury.
  No dependents and no script/doc/CI caller found, but it is a fund-movement capability with a codeword gate, 1457 lines, and it
  depends on stream-capture's signer. DECISION: KEPT, QUARANTINED. Not removed on a no-caller finding alone; not wired.
  It cannot run unless PQ_KEYPAIR_PATH and related env are set. Removal should be an explicit owner decision.
- Ensure `cargo check --workspace --all-targets` is clean after the removals: exit 0.

### Deletion compatibility (beyond source references)
- migrations/: RESTORED (G). docs/RUNBOOK.md documents `sqlite3 data/pump-quant.db`; the DB on the operator host has
  schema_migrations 1-5 applied (read-only, immutable open). README added: read-compat only, no runtime writer.
- Launchers: launch_rev16/17/29, detach_launch, restart_daemon, run_watchdog, launch_live removed. launch_rev36.sh, start_rev36.sh and
  launch_watchdog.sh are KEPT. No scheduled task, startup entry or shortcut on the mounted Windows volume referenced the removed ones;
  see scan_windows.py output notes in the handoff. This does not cover tasks stored outside that volume.


### north-star-build (origin/task/north-star-build, 17 commits, all 2026-09-09, author Alon) - capability inventory vs head
- Unique content: 170 files under tools/data-pipeline (32 src/north_star modules, 35 tests, schemas, ~9 master docs, one script),
  plus 2 Rust files in tools/stream-capture-rs/grpc-server-only: raw_recorder.rs (+430, new) and encoding.rs.
- Compared against this head: the zero-byte base58 encoder fix is ALREADY present on this head (an equivalent fix exists);
  the branch's 7 base58 tests were added here (7/7 pass against this head's encoder, compiled standalone because the crate
  needs OpenSSL dev headers that are not installed here). raw_recorder.rs is NOT on this head (this head's copy of that
  file is the 08-22 version); it is part of the branch's capture envelope work and is not integrated.
- Full branch NOT merged. Trial merge conflicts on 2 docs (00_HOLISTIC_CONTEXT.md differs by wording and is older than head; the ASR
  receipt is byte-identical on both sides).

### The 154 failures (reproduced on a clean archive of the branch, per-test from junit XML, bucket from the message only)
- LF checkout: 154 failed / 2080 passed. Buckets: 72 line-ending-pinned hash (SLINKY quarantine file pinned at its CRLF bytes),
  38 missing declared dependency (yt_dlp, imported by hls_clock_capture.py, absent from any requirements file on the branch),
  38 Windows-only API (atomic no-clobber directory publication raises on non-Windows), 2 hard-coded D: paths, 4 unclassified.
- CRLF checkout (all text files): 124 failed. The CRLF conversion fixes the pin but BREAKS other pins (33 hash mismatches), so
  blanket normalization is wrong; the contract is byte-exact per file.
- LF + only the quarantine JSON in CRLF + yt-dlp installed: 51 failed / 2183 passed: 38 Windows-only API, 3 D: paths, and 10
  tests that fail with `EXACT_PATH: sample unavailable` (they need a sample file at a D: path) or an `assert ([])` in the publication
  test (4, cause not isolated: it follows the Windows-only publication path). So: 103 of 154 explained by 2 environment causes
  (pin bytes, missing dependency) that are reproducible; the remaining 51 are Windows-only API / D: data paths.
  No failure was shown to be a code defect. 4 `assert ([])` publication failures are UNRESOLVED until run on Windows.
- Windows validation task: on the operator host, check out the branch with its pinned files byte-exact, `pip install yt-dlp`,
  run `pytest tools/data-pipeline/tests/north_star`; expect all 2234 to pass or report which fail. Until then north-star stays unmerged.
