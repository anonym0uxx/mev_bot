# mev_bot — `pump-quant`

A Rust trading engine for Solana memecoins on **pump.fun** (bonding curve) and **PumpSwap** (AMM pools). A trained
language model (Qwen) proposes entry and position-management decisions. The Rust engine validates them, simulates or
executes them, books every lamport, and enforces the safeguards. The default and only quick-start mode is **paper**:
simulated fills, no keys, no orders.

> **A tested engineering release is not authorization to trade.** Passing tests, CI and acceptance drills show
> that the code behaves as specified on fixtures. They say nothing about profitability, and live trading is a
> separate, explicit operator decision (see [Status](#status)).

## Decision authority

| Concern | Owner |
|---|---|
| Whether to enter (BUY / SKIP), size tier (`NONE` / `SMALL` / `FULL`), and position management (HOLD / REDUCE / EXIT / ADD) | **Qwen policy**, through the typed inference seam (`pump-quant-inference/src/seam.rs`, `pump-quant-proposal`) |
| Prompt construction from engine state, parsing, termination checks, deadlines; late or malformed answers are refused by name | Rust (`pump-quant-app` model lane) |
| Fills (paper landing-state simulation or the live builder), fees, rent deposits, settlement, books and the durable ledger | Rust (`pump-quant-app`, `pump-quant-execution`, `pump-quant-protocol`) |
| Protective safeguards: hard stop, rug precursor, SAFETY_OFF latch, solvency/per-position limits, stop/handoff contract | Rust; created by safeguards, never by the model |

Rust never re-sizes a model decision except for solvency/per-position limits, and it fails closed. Missing
evidence produces a **named refusal** (for example `deposit_unknown`, `capture_seal_behind_restart`), never a
default value.

## Status

Each line is a separate claim; none implies another. Dated detail lives in the linked records.

| Area | State |
|---|---|
| **Engineering** | Integration head passes the full workspace suite, fmt, clippy (`-D warnings`) and hosted CI (`rust-ci`). A packaged handoff → final report → restore drill passes on the release package. Evidence: [acceptance record](docs/acceptance/ENGINEERING_ACCEPTANCE.md). |
| **Model quality** | **Not accepted.** No model candidate has passed the decision-quality gates. Experiments continue offline, and no candidate is promoted. |
| **Serving latency** | Engine-side timely usability (≥ 95 % of decisions usable before their deadline) has **not** been demonstrated. |
| **Paper evaluation** | Fixture replays and drills only. A bounded forward paper run is planned and **not yet run**; it measures integration, not profitability. |
| **Live trading** | **Not enabled.** The live BUY path is built and disabled. It reports `deposit_unknown` until a coherent on-chain state fetcher exists, and on-chain program versions are unverified. Key custody and enablement are operator-held. |

## Architecture

```
 Helius LaserStream (gRPC, via tools/stream-capture-rs)      PumpPortal / Helius WS
                    │  transactions, account updates                  │
                    ▼                                                  ▼
 ┌──────────────────────────────── pq-daemon (pump-quant-junction) ──────────────────────────────┐
 │  decode + bind (laserstream.rs, curve bindings)  →  Engine::tick (pump-quant-app)              │
 │      market state · flow history · launch/creator registry · held ledger · safeguards          │
 │          │ prompt (engine state only)                     ▲ typed verdict / named refusal       │
 │          ▼                                                │                                     │
 │      model lane ──HTTP──► Qwen server (vLLM, OpenAI-compatible; external to this repo)          │
 │          │                                                                                      │
 │      paper fills (landing state, fee + rent + tx cost)  |  live builder (disabled)              │
 │      durable state: held.json, safety.json, flow.ckpt, event_stream.jsonl + capture_seal.json   │
 └────────────────────────────────────────────────────────────────────────────────────────────────┘
          │ event stream / tape                                         │ status + health JSON
          ▼                                                             ▼
  pq-engine-replay, evaluator (offline)                       operator, pq-watchdog
```

## Repository map

| Path | Contents |
|---|---|
| `rust/` | Cargo workspace (edition 2021). Engine crates live in `rust/crates/`. |
| `rust/crates/pump-quant-app` | Engine: model lane, positions, paper fills, fees/rent/tx cost, safeguards, durable state |
| `rust/crates/pump-quant-junction` | Binaries (`pq-daemon`, `pq-engine-replay`, `pq-sell`, `pq-watchdog`, `pq-serving-latency`), stream decode, restart rehydration |
| `rust/crates/pump-quant-inference`, `pump-quant-proposal` | Model seam: prompt contract, parser, decision types |
| `rust/crates/pump-quant-protocol`, `pump-quant-execution` | On-chain layouts, PDAs, transaction and deposit planning |
| `rust/data/CHAMPION_CONFIG.txt` | Authoritative runtime config, loaded by `pq-daemon` from `data/CHAMPION_CONFIG.txt` relative to its working directory |
| `tools/stream-capture-rs` | LaserStream capture sidecar |
| `tools/data-pipeline`, `tools/training`, `tools/qwen_training_handoff`, `training/` | Corpus pipeline and the training code of record (mirror; runs on the training host) |
| `tools/gates/portable_gate.py`, `.githooks/pre-push` | Repository gate and credential guard |
| `docs/` | Current entry document plus many **historical** design records (see below) |

## Getting started (offline / paper)

Prerequisites: Linux x86-64, Rust stable (tested with 1.99.0), Python 3 for the gate. No network access, keys or
model server are needed for these steps. The declared MSRV (1.85) does not currently build; see `docs/ENTRY.md` §7.

```bash
git config core.hooksPath .githooks          # once per clone: blocking credential guard
python3 tools/gates/portable_gate.py --repo .   # expected: "portable gate PASSED (blocking checks)"

cd rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                        # expected: all pass, 0 failed (count changes over time)

# Engine replay of a recorded event stream (offline). Prints one JSON object, for example:
# {"admitted":..,"rejected":..,"net_lamports":..,"ticks":..,"events_fed":..,"parse_skipped":0}
cargo run --bin pq-engine-replay -- --event-stream <event_stream.jsonl> --config data/CHAMPION_CONFIG.txt
```

No event stream is committed to the repository. `pq-daemon` writes one to `data/event_stream.jsonl` during a paper
session, and the capture tool records raw lines. The replay summary counts engine decisions; it is not a P&L claim.

Running `pq-daemon` in paper mode needs stream credentials (Helius / LaserStream) and, for model decisions, a
reachable model endpoint (`PQ_MODEL_ENDPOINT`). At start it prints
`startup paper check: no keypair, no --live, no submission sink (paper)`. Operating details:
`docs/ENTRY.md` §5 and §10. *These daemon steps are not covered by the commands above and were not executed while
writing this README.* Never pass `--live`: live execution is not enabled.

## Where things are defined

| What | Authoritative source |
|---|---|
| Runtime configuration | `rust/data/CHAMPION_CONFIG.txt`. The config dump and fingerprint are printed at daemon start. |
| Transaction cost assumption | `txcost_version` in the config. Definitions in `pump-quant-app/src/txcost.rs` and `pump-quant-junction/src/builder_txcost.rs` (paper = live builder plan). |
| Rent / ATA deposit assumption | `paper_rent_basis` in the config, defined in `pump-quant-app/src/paper_deposit.rs`. The pinned snapshot is the paper assumption for this version, **not** proof of historical rent. The token program must be proven from stream evidence; otherwise `deposit_unknown`. |
| Venue fees | Fee authority in `pump-quant-app` (fees are inside the clip on exact-in fills) |
| Operating entry points, env vars, stop sentinels, retired features | [`docs/ENTRY.md`](docs/ENTRY.md) |
| Release identities, acceptance evidence, rollback | [`docs/acceptance/ENGINEERING_ACCEPTANCE.md`](docs/acceptance/ENGINEERING_ACCEPTANCE.md) and git tags `rollback/*`, `tested/*` |
| Branch dispositions | [`docs/acceptance/BRANCH_CLEANUP_RECORD.md`](docs/acceptance/BRANCH_CLEANUP_RECORD.md) |
| Consolidation history and dispositions | [`docs/CONSOLIDATION_MANIFEST.md`](docs/CONSOLIDATION_MANIFEST.md) |
| Training code of record | [`training/README.md`](training/README.md) |
| CI | `.github/workflows/gate.yml` (`rust-ci`: portable gate, fmt, clippy, build, test) |

## Historical documents

Most files in `docs/` (for example `ARCH_*`, `ARCHITECT_*`, `QUANT_*`, `*_V2.md`, dated `*_2026-*.md`,
`RUNBOOK.md`, `RESTART_INSTRUCTIONS.md`) describe earlier designs, strategies or launch procedures. They are kept as
evidence, **not instructions**. Where they conflict with `docs/ENTRY.md` or this README, those two win.

## Safety and responsibility

This software can sign and submit Solana transactions when explicitly configured for live mode. Live mode is not
enabled in this release. Trading memecoins carries a high risk of total loss. Nothing in this repository is
financial advice, and nothing in it demonstrates profitability.
