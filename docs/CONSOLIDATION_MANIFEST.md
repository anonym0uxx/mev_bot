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


---
## Addendum 3 - review items (head: see PR; tested code SHA recorded below)

### Test-count reconciliation (cargo test --no-fail-fast --workspace; passed counts)
- 3584 (pre-retirement, 959cee8c) -> 3464 (912cbeed): -120 = 85 retired legacy tests + 9 in pump-quant-tape (4 unit + 5 integration) + 26 in pump-quant-clock (7 tie_break, 6 deterministic_test_clock, 5 replay_clock, 4 windows_system_clock, 4 dossier_clock_*). 85+9+26 = 120, matched per test binary (count_recon.py).
- 3464 -> 3477 (current): +13 = 5 journal serialization pins + 2 reject-code pins (unit, pump-quant-app lib) + 6 tests in tests/retired_invariants_e2e.rs.
- Net 3584 -> 3477 = -107. Counts are accounting only. 0 failed, 1 ignored (print_identity_block, a golden-text generator for the Python prompt port; asserts nothing).

### Retired-assertion audit (docs/consolidation_assertion_ledger.json: 85 rows)
Classification read from the ASSERT lines of each retired body (rollback tag), not from names: OBSOLETE 77, MIXED 8, PRESERVED 0 standalone.
Replacements (each mutant-checked where marked):
- journal serialization: journal_log.rs::serialization_pins (5 tests): every Decision variant's tag, byte layout and FNV-1a digest, derived by an independent Python encoder (journal_pin.py); sequence digest and order sensitivity; seed separation; signed PnL for both signs. Mutant (rt_cost_bps+1 in the Admitted encoding) fails 2 of 5.
- reject ordinals: engine.rs::reject_code_pins (2): 4..=28 and 19 keep their ordinals, code 29 pinned, all distinct, all < 32 (histogram slots).
- decoder boundary: an honest reserve pair is recorded, a contradictory one is refused not clamped, tolerance both sides (mutant: check removed -> fails).
- Admitted record on the model path: depth_basis decoded/migrated, round-trip cost > 0, move_source known, band 0 by design (mutant depth_basis=0 -> fails).
- no entry authority without a model: code 29 journalled and counted once, histogram sums to rejected, codes 0..28 never emitted (mutants: not journalled, renumbered -> fail).
- survival floor on the model fill: no deployable capital -> named refusal, cash untouched. Legacy fee/tip fields cannot move a model entry. Extreme legacy config cannot open a position without a model.
- GAP, not papered over: the operator minimum trade size (min_trade_size_lamports, 0.1 SOL) is NOT asserted on model clips; it was a property of the retired sizing law. Whether it applies to Qwen's clips is a policy decision.
- Not ported because no model-path equivalent exists: quote arithmetic (cost_model has 27 unit tests, curve_fill 24; they still run), brain episode sealing on close (model entries carry brain: None).
- Retired and NOT replaced: every pinned golden net/digest of the retired gate (GOLDEN_*), A/B "law earns more than its absence" comparisons, arm-vs-neutral inertness proofs of retired laws.

### GitHub status of PR #10 ("unstable")
- Repo has no branch protection on main (404), so there are NO required checks; no reviews; mergeable true, rebaseable true; no conflicts.
- Only workflow: rust-ci (gate.yml). Runs on this PR: 18dc0ae2 failure, 442996b8 failure; both failed at step "Format check" and every later step (Clippy, Build, Tests) was SKIPPED, so they have never run on this branch.
- Cause: rustfmt differences in tools/stream-capture-rs/src/{sender,ws}.rs (operator formatting on main). Fixed in commit K1 (format only, content patch identical to the preserved one).
- Clippy was then run for the first time (CI step 2): main (09e9194b) already fails it with 40 errors; this branch: 39 pre-existing + 1 introduced (ingest base58 repeat().take()), fixed in K2. 39 pre-existing errors remain in 8 crates (evaluator, governance, ingest, market-state, proposal, protocol, wallet-graph, watchlist). They were not touched: CI will stay red at clippy until they are fixed or the gate is changed, and I did not disable it.
- 912cbeed -> 442996b8: only docs/CONSOLIDATION_MANIFEST.md (modified) and docs/consolidation_retirement_ledger.json (added); git diff outside docs/ is empty. The workspace suite was run on 912cbeed (3464/0/1).

### Daemon dependency graph (cargo tree, normal edges, package pump-quant-junction which owns pq-daemon)
- Before (rollback tag) and after: 285 resolved packages and the same 23 workspace crates; feature listing byte-equal. NO dependency was cut in this pass.
- Reason (traced, dep_trace.py): the strategy-named crates are used by the daemon for shared functions, not only the retired gate: strategy (economic_gate, exit_ladder, scalp_position, safety_integrity, hazard_estimator in 11 files), simulator (fill/capacity used by scalp.rs), signals/features/narrative/social/wallet-graph (data enrichment for the prompt), governance (authority.rs registry hash), memory (analytics hashing), evaluator (autonomous_bridge: defense-in-depth drawdown halt, CONFIG_PROMOTION hot-reload, refiner scheduling, auto-revert).
- Finding to decide: pq-daemon still hot-reloads data/CONFIG_PROMOTION.json into the live Config and spawns pq-refiner. The extreme-config test shows no config state can open a position without a model, but the evaluator/refiner path is a legacy-strategy tuning loop inside the daemon; removing it is the next separable slice and is not done.

### Telemetry and treasury (by capability, evidence)
- pump-quant-telemetry: std-only, 7 integration tests; alerts() = floor breach, drawdown Critical/Degraded. Zero callers (no Cargo dependents, no bins, no scripts). SAFETY_OFF does NOT reach it: a trip sets blocked, bumps an epoch, writes the durable file and increments the safety:tripped:<reason> model-lane counter. The real alert surface today is stderr ALERT lines in pq_daemon (held-state restore refused, incomplete shutdown, stale held data, disk headroom) plus the watchdog reading data/live_status.json staleness. Nothing consumes the model-lane counter as a pager, so "alerts work" is NOT demonstrated for SAFETY_OFF. Kept; wiring it is a separate decision.
- pump-quant-treasury: crate with a CLI bin pq_treasury (transfer / confirm; env PQ_KEYPAIR_PATH, PQ_WALLET_ADDRESS, PQ_TREASURY_POLICY, PQ_TREASURY_AUDIT, PQ_RPC_URL), whitelist/limits/daily cap/codeword policy (gitignored real policy, template in config/), append-only audit log, 4 policy unit tests. Only dependent: none in Cargo; only entry point: the operator-run CLI. It is not on any model or daemon path and is not wired. Kept, authority boundary = human-initiated CLI only, never callable from the model lane. No transfer was attempted or authorized.

### north-star-build, precise
- 154 failures on LF checkout reproduced (classification in ns/*.json: junit-based, by exception text). Not established as defect-free. 51 failures remain with the pinned file CRLF and yt-dlp installed: 38 Windows-only publication (requires Windows), 3 D: path, 10 unexplained (6 "EXACT_PATH: sample unavailable" expected from a D:-backed sample, 4 test_publication_is_complete_or_absent_and_retryable assertions with unknown cause). Those 10 are UNRESOLVED.
- The grpc-server-only crate was built and its 7 b58 tests were run in the actual crate/build configuration using the documented local OpenSSL tree (handoff 0019: apt-get download libssl-dev + dpkg -x into /tmp, OPENSSL_STATIC/LIB_DIR/INCLUDE_DIR): 7 passed. The binary was not run against LaserStream.
- Branch remains unmerged.

---

## Addendum 4 - consolidation completion (head `63a80511` + docs)

### Automatic evaluator/refiner promotion loop - RETIRED (63a80511)
- Closes the Addendum-3 finding "pq-daemon still ... spawns pq-refiner ... removing it is the next separable slice and is not done".
- Removed: the daemon's periodic `pq-refiner` spawn (`RefinerSpawner`) and the `last_refiner_spawn_tick` state. `--refiner-every-ticks` is accepted and ignored; the startup banner logs `automatic refiner promotion RETIRED (operator-gated)`.
- Kept: defense-in-depth (cliff veto / circuit breaker / kill switch), the config hot-reload + auto-revert safety path, tape export, memory bank, replay, accounting, execution, enrichment. Promotion is now operator-gated: a `data/CONFIG_PROMOTION.json` must be placed deliberately; nothing in the daemon generates one.

### Lint track A - junction money-path ledger (reconciled, measured)
- At `9c69b33e`: **186 diagnostics = 186 unique sites** (126 `arithmetic_side_effects`, 53 `cast_possible_truncation`, 7 `unnecessary_fallible_conversions`).
- **Measured split:** stripping the 76 `LINT-ALLOW`-paired attributes and re-running clippy re-exposes **146** diagnostics; therefore **40** no longer warn. 40 + 146 = 186.
  - 40 resolved by code change, per file: `event_stream` 13, `translate` 8, `pq_daemon` 6 (infallible `Pubkey::from`), `laserstream` 4, `reserve_delta` 3, `model_lifecycle` 3, `autonomous_bridge` 1, `narrative_lexicon` 1, `paper_session` 1 (infallible).
  - Earlier report said "25 + 7 + 149"; that summed to 181 and was wrong. The corrections are: 4 sites already resolved by code change were mis-bucketed as allowances (`laserstream` ×3, `narrative_lexicon` ×1), and the allowance count was 150 not 149 → my 5-unit gap. During the corrections a further 4 sites moved allowance→code (`model_lifecycle` ×3 timestamp, `autonomous_bridge` ×1 drawdown).
- **Separate from the 186:** 26 sign-loss conversions closed (the enabled lint set does not flag `i64→u64`/`u64→i64`): `event_stream` `as u64` → `u64_field` (25) and `reserve_delta` Buy-arm `u64→i64` (1). Negative-input test added.
- Allowances are scoped (`LINT-ALLOW(<lint>): <bound>` + paired `#[allow]`). Bounds are labelled type/guard-enforced vs **practical domain bound** (counters are process-lifetime assumptions, not type proofs).

### Reserve domain - corrected end to end (`58742c7d`)
- `reserve_delta` computes the delta in `i128` and checked-converts only the *result* to `i64`, so two reserves each `> i64::MAX` with a small delta are preserved (tests: large+small positive delta, large+small negative delta, genuinely unrepresentable delta).
- An interim `note_stream_reserves` guard rejecting reserves `> i64::MAX` was **removed as unnecessary over-exclusion**: `LiveCurveState`/`live_state` carry `u64` reserves and the derivation supports the full `u64` range. Replaced by a producer→cache test asserting a reserve `> i64::MAX` **is** published and reaches the hot path.

### Rejection diagnostics - precise (`58742c7d`)
- `delta_no_trade` remains "all `None` from the reserve-delta derivation" (unchanged meaning); a **subset** counter `delta_out_of_range` is added (added to both `pq_daemon` and `paper_session` stats, printed in `daemon_health.json` and the paper report). So malformed/out-of-range is separable from ordinary no-trade.
- Flow-window completeness is a **separate** mechanism, not reserve age: `decision_join::JoinRefusal::FlowMetaMissing` (a print lacking fee/CU/slot/trader) and `FlowAggregatesIncomplete` **refuse** the prompt; reserve staleness is its own refusal (`ReserveStale`). Test `missing_fee_cu_or_slot_poison_the_flow_block_instead_of_reading_as_quiet` pins the poison behaviour. **Gap (honest):** a print dropped *upstream* of the reducer is invisible to that completeness check; it is only counted (`delta_no_trade`/`delta_out_of_range`/`pp_trades_received` vs `pp_trades_enqueued`). Reserve age does not establish flow completeness.

### Toolchain / MSRV
- Tested toolchain on the final head: `rustc`/`cargo` **1.99.0** (stable-x86_64-unknown-linux-gnu).
- MSRV **1.85 remains pre-existing unsupported** (deps need ≥1.86 `idna_adapter`; `pump-quant-core/src/reducer.rs` uses `is_multiple_of`=1.87). Declared MSRV not raised; deps not downgraded.

### Acceptance on the final code head `63a80511`
- **Local CI-equivalent (Linux, 1.99.0):** `fmt --check` clean; `clippy --workspace --all-targets -- -D warnings` exit 0; `build --workspace` exit 0; `test --workspace` exit 0, 0 failed.
- **Hosted GitHub:** workflow `rust-ci` run `37382000618` → **success**, steps Format check / Clippy (deny warnings) / Build / Tests all green. (Prior runs on earlier heads failed at Format check.)

### Current entry document
- `docs/ENTRY.md` is now the single current operating entry (runtime, pipelines, Qwen interface vs tooling, configuration, Windows handoff, ops, historical-alternatives index). Dated `docs/*` files are marked historical there.
