<div align="center">

# ⚡ Hermes · `pump-quant`

**A deterministic, integer-exact Solana memecoin scalping engine.**

![Rust](https://img.shields.io/badge/rust-1.85%2B-000000?style=for-the-badge&logo=rust&logoColor=white)
![Solana](https://img.shields.io/badge/Solana-mainnet-14F195?style=for-the-badge&logo=solana&logoColor=black)
![Tests](https://img.shields.io/badge/tests-1900%2B%20passing-2ea44f?style=for-the-badge)
![Gate](https://img.shields.io/badge/portable--gate-green-2ea44f?style=for-the-badge)
![Determinism](https://img.shields.io/badge/floats%20in%20outcome%20paths-0-8250df?style=for-the-badge)

![CI](https://github.com/anonym0uxx/mev_bot/actions/workflows/gate.yml/badge.svg)

</div>

---

## At a glance

<!-- Retrieval note: this block is a dense, self-contained fact sheet. Every field is a standalone claim. -->

| Field | Value |
|-------|-------|
| **Project name** | Hermes (system) · `pump-quant` (the Rust engine workspace) |
| **Repository** | `anonym0uxx/mev_bot` on GitHub |
| **Purpose** | Autonomously discover and scalp net-SOL-positive Solana memecoin opportunities under a mechanically-enforced risk constitution. |
| **Objective function** | Maximize realized **net SOL** (SOL in minus SOL out, after all costs). Net SOL is the single scalar the system optimizes. |
| **Primary language** | Rust (a 25-crate Cargo workspace) + Rust capture-lane tools + a Python supervisor (Hermes). |
| **Target market** | Solana memecoins on Pump.fun (bonding curve) and PumpSwap / Raydium (AMM pools). |
| **Trading style** | High-frequency scalping — many small, fast, net-positive round trips; not long holds. |
| **Determinism** | Integer/fixed-point only in outcome paths; no floating point, no wall-clock, no RNG in decisions. Byte-exact under replay. |
| **Current phase** | **Phase-A (laptop) COMPLETE + ingestion plane laptop-built**: paper/replay engine fully built, gate-verified, constitution-aligned; the Phase-B stream/data-ingestion *code* (Helius LaserStream WS + gRPC, PumpPortal, whale webhooks, PumpSwap decode, Birdeye, RPC failover, fee sampler) is now built and fixture-tested so server bringup is keys + soak + tune, not code-from-zero. Deploy-hardware items (OS tuning, PGO, live submission, key custody) remain Phase-B — see [`docs/SERVER_BUILD_MANIFEST.md`](docs/SERVER_BUILD_MANIFEST.md). |
| **Live capital** | Tier-0: key custody + enablement are human-held; no code path in this build signs or moves funds. Live execution (Phase-B) is autonomous under those human-held keys. Qualified strategies park in `AwaitingLiveCapability` — a missing-capability state, never a human-approval queue. |
| **Tests** | ~1,942 workspace tests (0 failing) + a dedicated `pq-regression` invariant crate (50) + Rust capture-lane suites (134 stream-capture / 191 https / 23 twitch); 191 SHA-locked dossier property tests (`scripts/materialize_tests.py --verify`). |
| **CI gate** | `hermes-gate/portable-gate`: fmt + clippy(-D warnings) + build + test + dossier `--verify` + supervisor portable gate + hot-path purity lint (enforcing the real hot crates) + memory soak gate. `scripts/regression_e2e.py` runs the whole repo end-to-end against pinned baselines. |
| **Rust edition / MSRV** | edition 2021, rust-version 1.85. |

---

## What this project is

`pump-quant` is the Rust execution engine of **Hermes**, an autonomous Solana memecoin trading system. It
scans memecoin markets continuously — across independent lanes of on-chain and off-chain evidence — builds a
ranked watchlist of candidates, gates each candidate against on-chain confirmation and economic viability,
and scalps the survivors for small net-SOL gains. On the laptop (Phase-A) it does this in **paper mode**
(simulated fills) and **replay mode** (deterministic re-run of recorded events); live capital is a separate,
human-gated concern handled on the server (Phase-B).

The system is named **Hermes** as a whole. The Rust workspace in this repository is called **`pump-quant`**.
A separate **Hermes supervisor** (Python) governs the build and the research loop. The GitHub repository is
`mev_bot`. These three names refer to parts of one system and are used consistently throughout this document.

## Why it exists (the goal)

The goal is a system that **relentlessly and autonomously searches for, validates, and compounds real
on-chain net-SOL edge in Solana memecoins, without human prompting, for as long as it operates** — while
never risking capital outside an explicit human gate. Profitable on-chain scalping demonstrably exists in
this market; the system's job is to find the forms of it that survive its own risk gates and execute them,
and to keep finding new edge as old edge decays. This is the constitution's *Continuous-Improvement
Mandate*: never idle, always hunting for the next testable source of net-SOL edge, prioritized by expected
value.

## Scope: what is and is not in this build

**In scope (Phase-A, this repository, built and tested):** market discovery, candidate ranking, the
corroboration + economics gate, paper/backtest fill simulation, deterministic replay, net-SOL evaluation,
research/overfitting diagnostics, governance envelopes, and the full decision logic — all pure, integer,
and testable off a live wire.

**Out of scope here (Phase-B, server, intentionally absent):** live market-data sockets (Helius
LaserStream), the real OS-tuning and memory binding, PGO / deploy-CPU pinning, live-chain reconciliation,
and any signing of keys or movement of funds. Each of these has a portable trait seam and a locked
acceptance test in this build; the server implements the trait. See
[`docs/SERVER_BUILD_MANIFEST.md`](docs/SERVER_BUILD_MANIFEST.md).

---

## Design invariants — "the Laws"

These invariants are non-negotiable and are compiled in, tested, or gated in CI — not merely intended. Each
row is a self-contained rule.

| Ref | Invariant | Enforcement |
|-----|-----------|-------------|
| **§22** | **No floating point in any outcome-affecting path.** Prices are rationals of reserves; sizes are lamports; scores are basis points. | Source scan in CI. The single sanctioned `f64` quantization boundary is documented and isolated. |
| **§54** | **Deterministic replay.** The same event stream yields the same decisions and the same net-SOL, byte-for-byte. | A canonical FNV-1a decision journal; a test asserts identical digests across independent runs. |
| **§29 / §71** | **Corroboration discipline (fade-first).** Social, narrative, and smart-money evidence can raise a candidate's rank but can never authorise entry alone; on-chain confirmation is always required. | The entry gate rejects any candidate lacking on-chain confirmation plus numeric microstructure. |
| **§99** | **Bounded state.** No collection grows without a cap over a long-running session. | Every accumulator (watchlist, world-state, journals, stream gaps) is capacity-bounded with eviction; tests assert the bound. |
| **§56 / §71** | **Reflection enhances discovery.** Realized net-SOL reshapes which discovery lanes get weight, inside a governance envelope. | Bounded-step, floor/ceiling-clamped weight adaptation; replay-reproducible. |
| **Tier-0** | **Key custody and live-capital enablement are human-held.** Hermes/LLM can never sign or move funds, and only the operator provisions keys and funds wallets. Once capability exists (Phase-B), the deterministic Rust engine trades **autonomously** — per-trade signing and submission are automated inside wallet floors, governance ceilings, and the probe ladder; paper→shadow→probe→scale progression is gate-driven with no per-trade or per-stage human approval (§64). | This laptop binary contains no signing path and `RunMode` has no `Live` variant; qualified strategies park in `AwaitingLiveCapability` until Phase-B capability flips. |
| **No-hardcode** | **Every decision threshold is an operator-supplied parameter, never a magic number in a decision path.** | A test proves that changing one config value alone changes the engine's decisions. |

---

## How it works — the discovery → gate → scalp → reflect loop

The core of `pump-quant` is a single deterministic loop implemented in the crate `pump-quant-app` (the
"nervous system"). It runs continuously under one explicit logical clock and behaves like a disciplined
scalper who never sleeps. The five stages, in order, are:

1. **Ingest.** Integer-valued events arrive: `MarketTrade` (on-chain swap), `NarrativeSample` (attention),
   `SocialCall` (a scored mention), `WalletAction` (smart-money activity), `OnchainConfirm` (proof a market
   is real and sellable), and `Tick` (advance the logical clock). Ingestion only updates state; a `Tick`
   runs the loop.
2. **Discover.** Four independent discovery lanes each emit candidates on their own (see next section). This
   is a **union**, not an intersection — no lane waits for another to agree.
3. **Fuse & rank.** Candidates are unioned per mint (strongest lane evidence wins ties), recency-decayed,
   and ranked into a bounded watchlist. The top-ranked candidates are promoted to the gate.
4. **Gate.** Each promoted candidate faces two hard hurdles: on-chain corroboration (confirmation + numeric
   microstructure) and economic viability (a size band that survives its own costs). Failing either is a
   reject.
5. **Scalp (paper).** Admitted candidates are filled through the calibrated simulator (fill modes A/B/C).
   No capital moves on the laptop.
6. **Reflect.** Realized net-SOL per lane nudges that lane's discovery weight inside a governance envelope,
   so the system leans into whichever senses are actually paying off. Net SOL is the objective.

Every stage is a pure function of prior state and the current event. Advancing the same event stream twice
produces the same net-SOL twice. That property makes replay a correctness authority, not a demo.

```mermaid
flowchart LR
    subgraph INGEST["ingest — integer events"]
      MT[MarketTrade] & NS[NarrativeSample] & SC[SocialCall] & WA[WalletAction] & OC[OnchainConfirm]
    end
    INGEST --> LANES
    subgraph LANES["discovery lanes — UNION not intersection"]
      N[Numeric]:::self --> U
      NA[Narrative]:::corr --> U
      SO[Social]:::corr --> U
      WL[Wallet]:::corr --> U
      U[(watchlist · rank · recency · bounded)]
    end
    U --> G{Gate}
    G -->|on-chain confirmed<br/>+ economically viable| S[Paper scalp<br/>simulator fill A/B/C]
    G -->|corroboration alone| X[Reject]
    S --> R[Reflect · net-SOL → lane weights]
    R -.->|reweights| LANES
    classDef self fill:#14F195,stroke:#0a7,color:#000;
    classDef corr fill:#2b2b2b,stroke:#8250df,color:#fff;
```

## Discovery: a union of four independent lanes

Most scanners require several signals to agree (an intersection) and therefore miss opportunities that are
early or single-sourced. `pump-quant` instead runs four independent discovery lanes and takes their
**union**: any lane can surface a mint onto the watchlist on its own. Each lane carries a weight that the
reflection stage tunes from realized net-SOL.

- **Numeric lane** — reads on-chain flow, liquidity, buyer breadth, and velocity. This is the only
  **self-authorizing** lane: its evidence may, in principle, justify capital (still subject to the gate).
- **Narrative lane** — reads attention velocity, virality coefficient, and meta-emergence. Corroboration
  only; capped pre-confirmation (fade-first). Cannot trigger entry alone.
- **Social lane** — reads quality-weighted calls and mentions from scored sources. Corroboration only.
- **Wallet lane** — reads smart-money / followable-wallet activity. Corroboration only.

A loud social call and a quiet on-chain accumulation are both admitted to the watchlist and reconciled later
at the gate, not suppressed at discovery. This is how the system can notice a narrative before it is legible
without letting hype pull the trigger.

## The gate: on-chain corroboration plus economic viability

A candidate at the top of the watchlist is a hypothesis, not an order. The gate applies two hard,
independent hurdles:

1. **On-chain corroboration (§29 / §71).** Entry is authorised only when the market has an `OnchainConfirm`
   *and* real numeric microstructure. Social, narrative, and wallet evidence can push a mint to the top of
   the watchlist but can never substitute for on-chain truth. This is the fade-first rule, made mechanical.
2. **Economic viability (§18).** Given confirmed depth, the real economic-gate leaf computes the viable
   size band `[x_min, x_cost, x_max]` net of round-trip fees, tips, expected send-failure, and a safety
   margin. If no size clears its own costs, the edge does not exist and the candidate is dropped.

The gate's verdict is a small enum: `Admit(size_band)`, or `Reject` with a reason
(`NeedsOnchainConfirmation`, `NoNumericConfirmation`, or `EconomicallyUnviable`).

## Scalp and paper fill

An admitted candidate is sized within its band and filled through the `pump-quant-simulator` crate, which
implements three fill modes: (A) causal signal replay with no profitability claim, (B) a deterministic
optimistic mechanical ceiling, and (C) calibrated adversarial execution at realistic or pessimistic
severity, with an explicit terminal-loss policy. The realized net-SOL is reconciled by the evaluator. No
capital moves on the laptop; the live executor is a Phase-B concern behind a trait seam.

## Reflection: realized net-SOL reshapes discovery

On a configured cadence, the reflection stage aggregates realized net-SOL per discovery lane and nudges that
lane's weight up (if paying its way) or down (if bleeding), bounded by a governance envelope: a maximum
single-step change, and a floor and ceiling so no lane is ever silently killed or allowed to dominate. The
adaptation is a pure function of performance, weights, and config, so replay reproduces the adapted weights
exactly. This closes the loop the constitution demands: reflection must *enhance discovery*, not merely
grade it.

## Evidence & authority — nothing gets to lie in its own favor

Every run is labeled with the fill model that produced it and the evidence status it may claim
(`Paper` on any laptop run). The optimistic ceiling (Mode B) can **never** satisfy promotion — the
governance crate's `strategy_registry` implements the constitution's full 14-status promotion
lifecycle (RESEARCH_CANDIDATE → … → CHAMPION) with a fail-closed ProbeReadinessGate: advancement past
the Mode-C boundary requires calibrated-adversarial evidence, live-ward transitions additionally
require live capability to be present, and every criterion the laptop cannot attest is hard `false`.
Unknown critical data fails closed on the capital path: stale numeric snapshots cannot back a fresh
confirm, an unpriceable exit is treated as unaffordable, asserted depth is cross-checked against
observed liquidity, thin-sample authenticity is a label rather than confirmation, and unknown exit
marks are valued at the hard-stop distance — never assumed flat. The configured expected move is a
cold-start prior only: per-lane realized returns graduate into conditional expectancy via partial
pooling (EXPECTANCY_V1), and the §52 baseline is valued at the realized hold move on the same tape,
so configuration can never manufacture edge or baseline evidence. These laws are pinned as tests
(`tests/phase_a_alignment.rs`, `tests/batch_e_laws.rs`, `tests/audit_wave2_laws.rs`) alongside a
golden determinism tape. Every law is A/B-attributed: it must strictly out-earn (or, for a protective
law, strictly avoid loss beyond) its own absence on a tape containing exactly its hazard.

**Three honesty corrections are baked into the current pin, and the number they produced must be read
the way it is meant.** The current golden reference is **net 31,111,528 lamports**, digest
`13693021370354439552`, promoted/admitted/rejected/universe-filtered **504 / 11 / 448 / 72**
(re-pin #27, 2026-07-28). It got there through **four** accounting corrections, none of which is a
strategy change:

1. **Cost realism (re-pin #23).** The §24 reversal makes cost-derived profit targets the live default
   and forbids fixed global TP constants; honoring it exposed a tape modelling ~1.5% round-trip cost.
   Corrected to realistic pump.fun economics.
2. **Depth realism (re-pin #24).** The tape's pools were 0.12–0.47 SOL against a 0.1 SOL minimum clip —
   our own order was 21–83% of the entire pool and was charged nothing for it. Real reserves start at
   30 SOL. Own-curve impact is now charged on both legs.
3. **Cost-model unification (re-pin #26).** Round-trip cost was computed independently in three places
   and the engine used one to *decide* and another to *book*. `crates/pump-quant-app/src/cost_model.rs`
   is now the single authority for both, and Associated Token Account rent — 203 bps on a floor clip,
   previously absent from the entire workspace — is priced and reclaimed.
4. **Provenance types (re-pin #27).** Depth and expected move became types that carry their own
   source. `virtual_sol` sets the price curve; `real_sol` is the only SOL a seller can actually be
   paid, and equals `virtual_sol − 30 SOL` — an identity that reproduces pump.fun's published
   85.005 SOL graduation raise. Fixtures had been declaring payout depth up to **30× above what their
   pools could pay**, and a curve nobody has bought into was claiming 29 SOL of sellable depth.

**The book has roughly doubled twice, and neither time is an improvement.** Same trades, honest
arithmetic. **Actual net SOL from trading is zero — this system has never traded.** The pin is a
synthetic regression fixture worth about **0.031 SOL (~$2.36)** across **11 trades in a handful of
markets**, statistically indistinguishable from zero (|t| ≈ 0.19), with a large share of the swing
attributable to end-of-tape force closure and to which markets survive a capacity bound. It is a
**regression fixture, not evidence of edge** — see `docs/EDGE_PROVENANCE_2026-07-27.md`. The full
headline arc (2.98M → … → 12.55M → 15,410,801 → 8,124,568 → 16,778,896 → 31,111,528) is a record of
accounting corrections and is never cited as live edge. The runner
exports a trade JSONL and a config-identity ledger on request (`--trade-jsonl`, `--config-ledger`);
both are secondary records, never authoritative over the journal digest or chain truth.

---

## Architecture — the crates

The workspace is 25 crates grouped by role (plus the `tools/` Rust capture lanes, which sit off the
deterministic hot path). Decision logic is pure and portable; every live-IO, OS, or
hardware concern sits behind a mockable trait seam so the logic is testable off a wire. Full map:
[`docs/ARCHITECTURE.md`](docs/architecture.md).

**Data & determinism spine**
- `pump-quant-domain` — core value/identity types (Mint, Lamports, Slot), the candidate lifecycle state machine, evidence stages.
- `pump-quant-clock` — the determinism seam: a `Clock` trait that a live impl and a `ReplayClock` share, plus deterministic tie-breaking.
- `pump-quant-journal` — durable event journal: framing, checksums, manifest, recovery/replay scan.
- `pump-quant-canonical` — provenance and dual-timeline canonicalization of observations.
- `pump-quant-ingest` — portable ingest/decode plumbing (the live socket is Phase-B).
- `pump-quant-protocol` — Pump.fun / PumpSwap AMM decode, constant-product math, instruction build, account-discriminator identity verification with fail-closed decode, and a versioned protocol registry.
- `pump-quant-market-state` — market regime, breadth, meta-rotation, and creator-state reducers.
- `pump-quant-features` — point-in-time feature store: bars, microstructure (order-flow imbalance, CVD, VWAP), and market-structure states.
- `pump-quant-wallet-graph` — smart-money classification, deployer credibility, funding/family graphs, leakage-safe holdouts.
- `pump-quant-core` — deterministic primitives: fixed-point AMM math, lock-free structures, shred decode/FEC/reassemble/parity, the reducer world-state, replay parity, the CPU/NUMA pin planner, and memory-pressure load-shedding.

**Decision & discovery**
- `pump-quant-signals` — entry/graduation/velocity/discount scorers, launch-coverage audit, the ActiveMarketUniverse selector, setup-family classifiers, and the anti-bundle fee-plausibility floor.
- `pump-quant-narrative` — the attention-velocity layer: virality, meta-emergence, candidate score, a ten-class catalyst classifier, and attention-decay and attention-state models.
- `pump-quant-watchlist` — union-not-intersection candidate ranking, per-lane performance, promotion.
- `pump-quant-strategy` — the decision core: economic gate / size band, exit ladder, scalp position, safety-integrity contracts, survival-floor and capital-derived sizing, and setup-archetype / risk-type classifiers.
- `pump-quant-execution` — route policy, bundle assembly, sell-ladder escalation, circuit breaker, fill reconciliation, incident gate (live send is Phase-B).

**Verification & research**
- `pump-quant-replay` — the replay driver: max-speed, real-time, scaled-time, step-by-observation, and break-on modes, composing clock and journal parity.
- `pump-quant-simulator` — the paper/backtest fill engine (modes A/B/C), terminal-loss policy, capacity, hazard estimation, calibration.
- `pump-quant-evaluator` — the frozen evaluator: net-SOL reconciliation, MFE/MAE, top-k excision, inactivity labelling, log-utility sizing validation, entry-zone taxonomy, the CVaR / profit-factor trading-metrics suite, PBO/CSCV overfitting diagnostics, Benjamini-Hochberg FDR, per-trade edge decomposition, and a convexity ledger.

**Governance / memory / social**
- `pump-quant-governance` — parameter-envelope guards, source lifecycle, canonical hashing, the versioned infrastructure manifest.
- `pump-quant-memory` — sealed-experiment store, value-of-information ranking, schema.
- `pump-quant-social` — social-source quality ledger, determinant scoring, amplification, copy-echo detection.

**The nervous system (spine binary)**
- `pump-quant-app` — the continuous discovery → gate → scalp → reflect loop that composes the above under one deterministic logical clock. Its `RunMode` has no `Live` variant.

**Evaluator/research binaries + regression net**
- `pq-evaluator` — the frozen-evaluator CLI: reads decision/outcome JSONL, prints a graded net-SOL report with baseline-family + FDR/PBO promotion verdict and its own evaluator hash (supervisor TOFU-pin binds to it).
- `pq-research-runner` — replays a sealed experiment and emits the ablation/baseline report (the §62 experiment-run artifact).
- `pq-regression` — 50 end-to-end regression invariants (determinism/digest witness, per-law presence, fail-closed, decoder fuzz) that catch silent drift across the engine.

**Ingestion plane (Rust capture lanes — `tools/`, off the deterministic hot path)**
- `tools/stream-capture-rs` (`pq-stream-capture`) — hand-rolled RFC6455 WebSocket client (RFC-vector tested) driving the Helius Enhanced-WS lane (transaction/account/slot subscribe, reconnect + slot-staleness watchdog, raw-preserving NDJSON per §6.3), the PumpPortal free WS lane, a whale/address-activity webhook listener (corroboration-tier, §6.6/§28), deterministic multi-provider RPC failover, and a priority-fee sampler. A server-only sub-crate (`grpc-server-only/pq-laserstream-grpc`) wraps the official LaserStream SDK (from-slot replay).
- `tools/social-ingest-https-rs` (`pq-social-capture`) — ureq+rustls capture for X / TikTok / Firecrawl / Pump replies / CoinGecko / **Birdeye** (1D OHLCV backfill + token data, the §6.7 required source) + a local-LLM sentiment enricher.
- `tools/social-ingest-rs` — dependency-free Twitch IRC capture.
- PumpSwap (Pump AMM) is decoded end-to-end in `pump-quant-protocol` (Pool/GlobalConfig accounts, buy/sell/create_pool instructions, anchor CPI Buy/Sell/CreatePool events with per-trade fee ground-truth).

## The Hermes supervisor

Hermes is the **Python supervisor** that governs how this Rust engine is built and, later, operated. It is
not the trading engine; it is the disciplined process around it. Its responsibilities include: driving the
leaf-by-leaf build against dossier contracts, running the CI gate, maintaining the knowledge base and the
research/experiment loop, enforcing promotion gates, and escalating anything that requires a human. Critically,
Hermes never authorises live capital — promotion checks for live-capital scope always return
`human_gate_required`. Hermes reads this repository (including this README and the constitution in
`docs/`) as the ground truth for what the system is and must do.

---

## Build-integrity — the independence lock

A property test is only an honest authority if the code's author cannot edit it. `pump-quant` makes this
physical. Forty-five component **dossiers** (`supervisor/reinforcement/dossiers/<component>.yaml`) declare
the contract for **179 leaves**. The script `scripts/materialize_tests.py` renders each leaf's property test
into a SHA-locked file at `rust/crates/<crate>/tests/dossier_<component>_<leaf>.rs`. The implementation is
written against tests it did not author and cannot change: CI re-hashes them with `--verify`, and the
repository denies edits to these files. The guarantee is "the builder cannot grade its own homework,"
enforced by the toolchain rather than trusted. A leaf cannot be green unless real, correct code backs it —
which is why the dossier count is a proxy for how much of the system is proven, not merely present.

## Determinism and replay

- ~1942 workspace tests, all passing; 191 of them are the SHA-locked dossier property tests.
- Byte-exact replay: the decision journal folds every promotion, gate verdict, fill, and reweight into a
  canonical rolling FNV-1a hash. Two runs over the same events produce the same digest. A single
  non-determinism bug (a stray wall-clock read, an unordered map used for an outcome) flips it.
- No wall-clock in logic: time is an explicit `ReplayClock` tick. Randomness, where it appears (bootstrap
  bands, Monte-Carlo survival), is a seeded, caller-supplied `splitmix64` generator: same seed, same bytes.

## Safety model

- **Live capital is Tier-0 (custody human-held).** No code path in this build signs a key or moves funds. The laptop build is
  paper/replay only, and `RunMode` cannot express `Live`.
- **Fade-first.** Narrative and social evidence is capped before on-chain confirmation and can never trigger
  entry alone.
- **On-chain truth always required.** The gate refuses any candidate lacking confirmation plus numeric
  microstructure.
- **Bounded everything (§99).** No unbounded accumulator can exhaust memory in a long-running session.
- **No silent no-ops.** OS-tuning and fill claims are read-back-verified; an unverifiable setting is
  reported, never assumed.

## Phase-A / Phase-B boundary

Everything provable off a live wire is built and tested here (Phase-A, laptop). What genuinely needs the
deployment box is intentionally absent (Phase-B, server), each left as a trait seam with a locked acceptance
test: the real `OsTune` Windows binding, the `MemorySampler`, Helius LaserStream live ingest, PGO /
deploy-CPU pinning, live-chain reconciliation, and key signing / fund movement. The server task for each is
"implement this trait and satisfy this named locked test," not "design from scratch."

**One correction to that sentence, because it mattered (2026-07-29).** For `OsTune` the named locked
test was `dossier_cpu_numa_tuning_cn_os_apply`, which exercises `MockOs` — **it is green right now
with zero Windows code written**, so it locked nothing about the adapter. The test that actually
binds is `ostune_conformance` (`pump-quant-core`, 10 tests), which exercises all four trait methods
including the two `apply_plan` never calls, and carries an anti-stub probe that an echoing
implementation cannot pass. Read [`docs/OSTUNE_BUILD_SPEC.md`](docs/OSTUNE_BUILD_SPEC.md) before
building it. Where a trait seam's locked test only exercises a mock, treat the seam as specified but
**not** acceptance-gated until you have written the test that fails for the right reason. The actionable
checklist is [`docs/SERVER_BUILD_MANIFEST.md`](docs/SERVER_BUILD_MANIFEST.md).

---

## Build and run (quick start)

Requires Rust 1.85 or newer.

```bash
git clone https://github.com/anonym0uxx/mev_bot.git
cd mev_bot/rust

# The full portable gate — exactly what CI runs:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test  --workspace                                  # ~1942 tests

# Verify the dossier independence lock (needs pyyaml):
python -m pip install pyyaml
python ../scripts/materialize_tests.py --repo .. --verify # 191/191 intact

# Or run the whole repo end-to-end against pinned baselines (engine + capture suites + gates):
python ../scripts/regression_e2e.py                       # single PASS/FAIL table, non-zero on any drift
```

Run the nervous system in paper mode over a recorded event journal:

```bash
cargo run -p pump-quant-app -- paper config.txt events.txt
# Prints: mode, ticks, promoted/admitted/rejected counts, net_lamports (the objective),
# per-lane net-SOL, adapted lane weights, and journal_digest (the determinism fingerprint).
# The `live` argument is refused by design: live capital is Tier-0 human-gated and not in this binary.
```

## Repository layout

```text
rust/                        the 25-crate Cargo workspace (the pump-quant engine)
tools/                       Rust capture lanes (stream-capture-rs, social-ingest-https-rs, social-ingest-rs)
  crates/pump-quant-*         domain, core, strategy, evaluator, app, and the rest
supervisor/                  Hermes — the Python governance/build supervisor
  reinforcement/dossiers/     45 component contracts (the correctness authority)
  gates/                      portable CI checks (no-stubs, secrets, hot-path lint)
scripts/                     materialize_tests.py (dossier lock) · ci_gate.py (portable gate)
docs/                        ARCHITECTURE.md · SERVER_BUILD_MANIFEST.md · the constitution
.github/workflows/gate.yml   the portable-gate CI workflow
```

## Phase-B server bringup (deployment box)

Phase-A is provable off a live wire; Phase-B is where the deployment server turns the built-and-tested
ingestion + execution seams on. Because the ingestion *code* is already laptop-built and fixture-tested,
server bringup is credentials + measurement, not new construction. The ordered manifest is
[`docs/SERVER_BUILD_MANIFEST.md`](docs/SERVER_BUILD_MANIFEST.md); the Helius product map is
[`docs/HELIUS_INTEGRATION.md`](docs/HELIUS_INTEGRATION.md) and what it will cost for 30 days of
continuous operation is [`docs/HELIUS_BUDGET_2026-07-29.md`](docs/HELIUS_BUDGET_2026-07-29.md) —
read that one **before** arming the gRPC lane. It also carries the two-source allocation: free
PumpPortal (`wss://pumpportal.fun/api/data`, DISCOVERY tier, per-mint filtering) owns the wide net —
creation discovery, graduations, screening flow — and paid LaserStream is narrowed to the watchlist
and to held positions, where §29 corroboration and §97 exit latency actually require canonical
on-chain data. The two subscription sets are kept **disjoint by construction** rather than deduped
after the fact. As written today the gRPC lane is a program-wide firehose with no cost monitor and
the central case consumes 80% of a 100M-credit month; under the allocation it consumes 7%.
The on-chain instruction layouts Phase-B must build against — every constant re-derived from first
principles, including two that failed — are [`docs/VENUE_TX_LAYOUTS.md`](docs/VENUE_TX_LAYOUTS.md).

1. **Clone + build the workspace and capture lanes** on the deploy box (Windows-native target), then build
   the server-only gRPC lane (`tools/stream-capture-rs/grpc-server-only`, needs crates.io reachable):
   `cargo build --release`.
2. **Provision credentials as environment variables (never committed):**
   `HELIUS_API_KEY` (LaserStream mainnet gRPC requires the Helius **Business plan — $499/mo**; the
   Enhanced-WS `transactionSubscribe` lane works from Developer, and webhooks/Sender work on all plans),
   `RPC_URLS` (comma-separated failover priority), `WEBHOOK_AUTH_SECRET` (+ a TLS-terminating reverse proxy
   for the whale webhook), `BIRDEYE_API_KEY` (§6.7 required source; token-security fields need Starter+),
   plus the social keys (`TWITTERAPI_IO_KEY`, `TIKTOK_API_KEY`/`_BASE`, `FIRECRAWL_API_KEY`, `CG_API_KEY`,
   `TELEGRAM_*`) and `LLAMA_SERVER_URL` for the local-LLM sentiment enricher.
3. **Stand up the streams** (LaserStream gRPC as primary canonical ingest, Enhanced-WS as the verified
   fallback, PumpPortal free WS, RPC failover) and **soak-measure** the manifest's acceptance evidence:
   §2 sequence-gap stats, §4 failover parity, §8 fee-calibration epoch vs probes, §10 Birdeye
   reconciliation epoch, §11 webhook lag/loss.
4. **Deploy-hardware tuning** (§1/§5): OsTune Windows affinity/VirtualLock/timer, deploy-CPU-pinned codegen
   (`RUSTFLAGS -C target-cpu=znver5`, never `native` on a build box), replay-corpus PGO, and the p50/p99/p999
   latency budgets — all against the criterion-103 budget on deployment-identical hardware.
5. **Execution + Tier-0** (§6/§7/§9, human-gated): the Sender submission client under the signing boundary,
   operator-funded probe wallets, live sell-path `simulateTransaction`, then the autonomous lifecycle
   shadow → Mode-C calibration → ProbeReadinessGate → minimum probe → reconciled scale. Key custody and
   funding are human actions no agent tool can perform.

Hermes (the Python supervisor) reads this repository — this README and the constitution in `docs/` — as
ground truth for what to build and verify next.

## Status

Phase-A (laptop) is complete and the Phase-B ingestion plane is laptop-built: 25 workspace crates + 3 Rust
capture-lane tools, 191 SHA-locked dossier property tests, ~1,942 workspace tests passing (0 failing), the
portable gate + hot-path lint + memory soak gate green, and an end-to-end regression runner over the whole
repo. The engine is aligned to the constitution end-to-end (all §1–§71 sections and acceptance criteria
1–114 audited; the wired laws are per-law A/B-attributed). The next step is bringing the workspace to the
deployment box and executing the Phase-B server manifest above. Live trading remains behind the Tier-0
human gate.

---

## Glossary

Domain and project terms, defined for unambiguous reference.

- **net SOL** — realized SOL received minus SOL spent, after all fees, tips, and failed-attempt costs. The system's objective function.
- **scalp** — a small, fast, net-positive round trip (buy then sell), as opposed to a long hold.
- **discovery lane** — one independent source of candidate evidence (numeric, narrative, social, or wallet). Lanes are unioned, not intersected.
- **union, not intersection** — a candidate may enter the watchlist on the strength of a single lane; lanes do not have to agree.
- **corroboration / corroboration-tier** — evidence (social, narrative, wallet) that can raise a candidate's rank but can never authorise entry on its own.
- **fade-first** — the discipline of not acting on hype: narrative/social signals are capped before on-chain confirmation and never trigger entry alone.
- **gate** — the stage that decides Admit vs Reject, requiring on-chain confirmation plus numeric microstructure and economic viability.
- **size band** — the viable trade-size interval `[x_min, x_cost, x_max]` computed net of costs, failure rate, and margin. An empty band means no viable edge.
- **paper mode** — execution against a simulated fill model; no real capital moves.
- **replay mode** — deterministic re-run of a recorded event journal; used to prove byte-exact determinism.
- **reflection** — the stage where realized net-SOL per lane adjusts that lane's discovery weight within a governance envelope.
- **dossier** — a YAML contract for one component, defining leaves whose property tests are the locked correctness authority.
- **leaf** — one narrowly-scoped function/behavior with a single property test as its authority.
- **independence lock** — the mechanism (materialize + SHA-verify + edit-deny) ensuring the builder cannot edit the tests it is judged by.
- **Tier-0** — the highest safety class: key custody, wallet funding, live-capital enablement, evaluator releases, and emergency stops are human-held and can never be exercised by Hermes/an LLM. Deterministic per-trade signing in live operation (Phase-B) runs under those human-held keys — autonomy of execution, human monopoly on custody.
- **Phase-A / Phase-B** — laptop (paper/replay, this build) vs deployment server (live IO, OS tuning, live capital).
- **Hermes** — the overall system and its Python supervisor that governs the build and research loop.
- **pump-quant** — the Rust engine workspace in this repository.
- **§N** — a reference to section N of the project constitution (`docs/HERMES_ONE_SHOT_PROMPT.md`).

## Disclaimer

This software trades — or models the trading of — Solana memecoins, among the most volatile and adversarial
markets in existence. Nothing here is financial advice. The laptop build executes no live trades; live
capital is human-gated by design. If you connect this to real funds, you do so at your own risk, with your
own review, and your own responsibility. Profitable on-chain trading provably exists; so does total loss.

---

<div align="center">

*Built integer-exact, tested to the byte, gated against its own author.*

**Discovery is a union. Every number is an integer. The wall clock is a lie we refuse to tell.**

</div>
