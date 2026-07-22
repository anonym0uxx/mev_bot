# ARCHITECTURE — Hermes pump-quant bot (Phase-A laptop build, as shipped)

Authoritative map of the Rust workspace as committed on `bot-build`. 22 crates, 45 dossiers,
179 locked property tests, 1389 total tests, whole-workspace gate green (fmt · clippy
`--all-targets -D warnings` · build · test · `materialize_tests.py --verify`). All outcome
logic is integer/fixed-point and deterministic (§22); no floating point, wall-clock, RNG, or
IO in any decision path. Live IO, real OS tuning, key signing, and fund movement are Phase-B
(server) — see `docs/SERVER_BUILD_MANIFEST.md`.

## The workspace (rust/crates), by role

### Data & determinism spine
- **pump-quant-domain** — core value/identity types (Mint, Lamports, Slot, lifecycle state
  machine, evidence stages). Dossier: `lifecycle`.
- **pump-quant-clock** — the determinism seam: a `Clock` a live impl and a `ReplayClock` share,
  plus deterministic tie-breaking. Dossier: `clock`.
- **pump-quant-journal** — durable event journal: framing, checksums, manifest, recovery/replay
  scan. Dossier: `manifest`.
- **pump-quant-canonical** — provenance & dual-timeline canonicalization of observations.
  Dossier: `types`.
- **pump-quant-ingest** — portable ingest/decode plumbing (live socket is Phase-B).
- **pump-quant-protocol** — pump.fun / PumpSwap AMM decode + constant-product math + instruction
  build; account-discriminator **identity verification with fail-closed decode** and a
  **versioned protocol registry**. Dossier: `decode`.
- **pump-quant-market-state** — market regime, breadth, meta-rotation, creator-state reducers.
  Dossier: `regime`.
- **pump-quant-features** — point-in-time feature store: bars, microstructure (OFI/CVD/VWAP),
  and the **§21.6 market-structure** family. Dossiers: `micro`, `market_structure`.
- **pump-quant-wallet-graph** — smart-money classification, deployer credibility, funding/
  family graphs, leakage-safe holdouts. Dossier: `smart_money`.
- **pump-quant-core** — deterministic primitives: fixed-point AMM math, lock-free structures,
  shred decode/FEC/reassemble/parity, the reducer world-state, replay parity, latency, the
  **cpu_numa_tuning** planner (portable; OsTune Windows impl is Phase-B), and **memory_pressure**
  load-shedding (MemorySampler seam is Phase-B). Dossiers: fixedpoint, lockfree, reducer,
  replay, shred, cpu_numa_tuning, memory_pressure.

### Decision & discovery
- **pump-quant-signals** — entry/graduation/velocity/discount scorers, launch-coverage audit,
  **ActiveMarketUniverse selector** (criterion 90), setup-family classifier, **§70.10 anti-bundle
  fee-plausibility floor**. Dossiers: setup_classifier, active_market_universe, fee_plausibility.
- **pump-quant-narrative** — attention-velocity layer: virality, meta-emergence, candidate score,
  **10-class catalyst classifier**, **attention-decay** and **attention-state** models.
  Dossiers: narrative, catalyst_classifier, attention_decay, attention_state.
- **pump-quant-watchlist** — union-not-intersection candidate ranking, per-lane performance,
  promotion. Dossier: `rank`.
- **pump-quant-strategy** — the decision core: economic gate / size band, exit ladder, scalp
  position, safety-integrity contracts, **survival-floor + deployable-capital derivation**,
  **capital-derived probe/exposure sizing**, **§23 entry arbitration**, **setup-archetype** and
  **risk-type** classifiers. Dossiers: economic_gate, exit_ladder, scalp_position,
  safety_integrity, entry_arbitration, setup_archetype, risk_type.
- **pump-quant-execution** — route policy, bundle assembly, sell-ladder escalation, circuit
  breaker, fill reconciliation, incident gate (live send is Phase-B).

### Determinism / verification / research
- **pump-quant-replay** — the §19 replay driver: max-speed / real-time / scaled / step-by-obs /
  break-on modes, composing clock + journal parity. Dossier: `driver`.
- **pump-quant-simulator** — paper/backtest fill engine (modes A/B/C), terminal-loss policy,
  capacity, hazard estimation, calibration. Dossier: `hazard`.
- **pump-quant-evaluator** — frozen evaluator: net-SOL reconciliation, MFE/MAE, top-k excision,
  inactivity labelling, plus **log-utility sizing validation**, **entry-zone taxonomy**,
  **Benjamini-Hochberg FDR**, **PBO/CSCV overfitting** diagnostics, the **§54 trading-metrics
  suite** (CVaR/profit-factor/…), **per-trade edge decomposition**, and the **convexity
  ledger**. Dossiers: evaluator_stats, sizing_validator, entry_zone, fdr, overfitting, metrics,
  edge_decomposition, convexity_ledger.

### Research / governance / memory / social
- **pump-quant-governance** — parameter-envelope guards, source lifecycle, canonical hashing,
  and the versioned **infrastructure manifest** structure. Dossier: `envelope`.
- **pump-quant-memory** — experiment store, VOI ranking, sealed-experiment hashing, schema.
  Dossier: `voi`.
- **pump-quant-social** — social-source quality ledger, determinant scoring, amplification,
  copy-echo detection. Dossier: `determinants`.

### The nervous system (spine binary)
- **pump-quant-app** — the continuous **discovery → gate → scalp → reflect** loop that composes
  the crates under one deterministic logical clock: four union-not-intersection discovery lanes,
  the corroboration gate (on-chain confirmation + numeric evidence required; social/narrative/
  wallet never authorise alone), paper scalps through the simulator, reflection-enhances-discovery
  weight adaptation, and a byte-deterministic decision journal. `RunMode` has no `Live` variant —
  live capital is Tier-0 human-gated. Dossier: `config`.

## Build-integrity model
Each dossier under `supervisor/reinforcement/dossiers/<component>.yaml` defines leaves whose
`property_test` is materialized by `scripts/materialize_tests.py` into a SHA-locked
`rust/crates/<crate>/tests/dossier_<component>_<leaf>.rs`. The builder implements *against* tests
it cannot edit (`.claude/settings.json` denies edits; `--verify` re-hashes in CI). This is the
"builder cannot grade its own homework" guarantee, now covering 45 components / 179 leaves.

## Phase boundary (what is intentionally NOT here)
The 14 server-deferred criteria (Windows-native runtime, Helius LaserStream live socket, PGO /
deploy-CPU pinning, the real `OsTune` and `MemorySampler` implementations, live-chain
reconciliation, key signing / fund movement) are Phase-B. Each has its portable logic + a trait
seam already built and tested here; the server implements the trait and satisfies the named
locked test. See `docs/SERVER_BUILD_MANIFEST.md`.
