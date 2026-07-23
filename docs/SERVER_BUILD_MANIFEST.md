# SERVER_BUILD_MANIFEST — Phase-B handoff (deferred server capabilities)

This is the handoff manifest for capabilities that are deliberately **deferred to the
deployment server** (Phase-B). Phase-A (laptop build box) delivered the interfaces,
deterministic logic, and tests listed under each item; nothing below claims a live
capability that does not exist yet. Every item is **fail-closed**: absent its production
adapter, credentials, or measurement, the affected path refuses to arm — it never
degrades silently into simulation-as-live.

Section numbers are load-bearing: `docs/LATENCY.md` cites §1 (OsTune pinning) and §5
(the never-`target-cpu=native` rule); `pump-quant-core/src/cpu_numa_tuning.rs` cites
task #1.

---

## §1 CPU affinity / NUMA / NIC / IRQ / Windows tuning (OsTune)

- **Phase-A interface:** `pump-quant-core::cpu_numa_tuning` — topology model, pin plan,
  jitter probe, and the `OsTune` trait with a no-op/recording impl; plus
  `pump-quant-core::lockfree` and `pump-quant-core::latency` for the structures the plan
  protects.
- **Required production adapter:** real `impl OsTune` on the deploy OS (Windows:
  `SetThreadGroupAffinity` / `SetPriorityClass` / `timeBeginPeriod` / `VirtualLock`;
  Linux equivalent if the deploy OS changes), plus NIC queue/IRQ steering config.
- **Hardware/credential dependency:** the EPYC 9655P deploy box itself (CCD/L3 topology);
  administrator rights for affinity/priority/page-lock calls.
- **Server measurement required:** jitter probe before/after pinning; per-CCD cache
  residency of the hot decision thread; IRQ distribution under ingest load.
- **Integration point:** app startup applies the pin plan from `cpu_numa_tuning` before
  the `evaluate()` loop starts.
- **Acceptance test:** jitter-probe deltas recorded in the evidence store; the golden
  digest (`tests/golden_digest.rs`) unchanged — tuning must be behaviour-preserving.
- **Failure behaviour (fail-closed):** if any OsTune call fails, the bot runs unpinned
  and records the failure; it never claims tuned latency numbers.
- **Enablement condition:** deploy box provisioned + real `OsTune` impl merged + jitter
  measurements journaled.

## §2 Helius LaserStream connection

- **Phase-A interface:** `pump-quant-ingest::helius_parse` (message parsing, tested on
  fixtures) and `pump-quant-ingest::submission_surface`; canonicalization via
  `pump-quant-canonical`.
- **LAPTOP-BUILT (2026-07-23):** two client lanes exist in-repo — `pq-stream-capture
  helius-ws` (hand-rolled RFC6455/rustls WebSocket, transactionSubscribe/accountSubscribe/
  slotSubscribe, reconnect + slot-staleness watchdog, raw-preserving NDJSON; 122 tests) and
  `tools/stream-capture-rs/grpc-server-only/` (`pq-laserstream-grpc` on the official
  `helius-laserstream` SDK — SERVER-BUILD-ONLY, compiles where crates.io is reachable).
  See docs/HELIUS_INTEGRATION.md. Remaining here: credentials + soak, not code-from-zero.
- **Required production adapter:** authenticated LaserStream (gRPC/WebSocket) client with
  reconnect/backpressure, feeding the parser's existing input type.
- **Hardware/credential dependency:** Helius API key on a paid plan with LaserStream
  entitlement; server-grade network.
- **Server measurement required:** end-to-end event latency (stream receive → reducer
  apply), disconnect/reconnect gap statistics.
- **Integration point:** ingest source registry (`pump-quant-ingest::source_registry`)
  registers the live source; staleness/disconnect handling per the safety-integrity
  dossier tests.
- **Acceptance test:** parser fixtures stay green; live soak run journaled with zero
  unexplained sequence gaps.
- **Failure behaviour (fail-closed):** disconnect or stale stream trips the staleness
  gate — no trading decisions on stale data.
- **Enablement condition:** credentialed endpoint + soak-run evidence in the store.

## §3 Jito ShredStream

- **Phase-A interface:** `pump-quant-core::shred` (header decode, FEC tracking,
  reassembly, parity gate — dossier-tested).
- **Required production adapter:** Jito ShredStream proxy/client subscription delivering
  raw shreds to the decoder.
- **Hardware/credential dependency:** Jito ShredStream access approval; UDP-capable
  server network path.
- **Server measurement required:** shred-vs-RPC lead time distribution; reassembly
  success rate under real loss.
- **Integration point:** early-signal path ahead of confirmed RPC data; the parity gate
  decides when reassembled data may be used.
- **Acceptance test:** dossier tests for `shred` remain SHA-locked green; live parity
  gate statistics journaled.
- **Failure behaviour (fail-closed):** parity/FEC failure discards the shred-derived
  view; the bot falls back to the confirmed stream.
- **Enablement condition:** granted ShredStream access + measured lead-time evidence.

## §4 Canonical RPC failover

- **Phase-A interface:** `pump-quant-canonical` (canonical observation/reducer types,
  deterministic tie-breaks via `pump-quant-clock::tie_break`);
  `pump-quant-ingest::canonical`.
- **LAPTOP-BUILT (2026-07-23):** `pq-stream-capture` `rpc.rs` — deterministic-priority
  multi-provider JSON-RPC with consec-error + EWMA-latency health scoring and re-probe,
  state machine mock-transport-tested. Remaining here: live provider baselines + failover
  parity evidence.
- **Required production adapter:** multi-provider RPC client set with health scoring and
  deterministic failover among providers.
- **Hardware/credential dependency:** at least two funded RPC provider accounts
  (e.g. Helius + fallback).
- **Server measurement required:** per-provider latency/error-rate baselines; failover
  switch time.
- **Integration point:** all confirmed-state reads route through the canonical layer;
  failover must preserve reducer determinism.
- **Acceptance test:** replay parity (`pump-quant-replay` + `pump-quant-clock`) across a
  forced failover — identical journal digest.
- **Failure behaviour (fail-closed):** all providers unhealthy → incident gate
  (`si_incident_gate`) halts new entries; open positions manage via the sell path only.
- **Enablement condition:** two credentialed providers + failover parity evidence.

## §5 Production latency calibration (incl. build-flag injection)

- **Phase-A interface:** `pump-quant-core::latency` (hot-path timing scaffolding) and the
  standalone `bench/` harness (non-workspace); `docs/LATENCY.md` records the
  algorithmic (relative) wins.
- **Required production adapter:** deploy-box bench run + injection of
  `RUSTFLAGS="-C target-cpu=znver5"` (fallback `znver4`) from the infra manifest's
  deploy-CPU entry; optional PGO on a recorded-replay profile. **Never
  `-C target-cpu=native` on a build box** (§24) — the flag value comes from the pinned
  deployment_host declaration, not from whatever machine compiles.
- **Hardware/credential dependency:** the deploy CPU (EPYC 9655P) — absolute numbers are
  box-specific and meaningless from the laptop.
- **Server measurement required:** p50/p99/p999 per-tick and kernel timings at 64/256/1024
  mints on the deploy box, before and after flag/PGO application.
- **Integration point:** infra manifest supplies the CPU model; CI/build scripts inject
  the flags; results journaled as benchmarks in the evidence store.
- **Acceptance test:** golden digest unchanged under the tuned build; deploy-box bench
  numbers recorded.
- **Failure behaviour (fail-closed):** missing deploy-CPU declaration → build uses the
  portable target; no tuned-latency claims are made.
- **Enablement condition:** operator-pinned deployment_host declaration + deploy-box
  bench evidence.

## §6 Real transaction submission + signing / key custody (Tier-0)

- **Phase-A interface:** `pump-quant-execution` — `ex_route_policy`,
  `ex_bundle_assemble`, `ex_tip_compute`, `ex_blockhash_cache`, `ex_circuit_breaker`,
  `ex_reconcile_fill`; `pump-quant-ingest::submission_surface`; signing-boundary and
  no-key-material invariants dossier-tested (`si_signing_boundary`).
- **Required production adapter:** signer service holding keys behind the signing
  boundary; live submission client (RPC send + Jito bundle path via
  `ex_bundle_assemble`).
- **Hardware/credential dependency:** wallet keypairs under operator custody (never in
  repo/config/model context — Tier-0), Jito auth if bundles are used.
- **Server measurement required:** submit→land latency, blockhash staleness rate,
  bundle acceptance rate.
- **Integration point:** route policy picks the surface; reconcile-fill
  (`ex_reconcile_fill`) closes the loop from submission to confirmed fill;
  `pump-quant-journal` records every attempt.
- **Acceptance test:** devnet/probe-scale round trip fully reconciled in the journal and
  evidence store.
- **Failure behaviour (fail-closed):** no signer configured → submission refuses to arm;
  circuit breaker (`ex_circuit_breaker`) halts on anomaly; key material anywhere in the
  agent-visible surface is a Tier-0 stop.
- **Enablement condition:** human-provisioned key custody + explicit human arming
  (constitution Tier-0 gate); this manifest cannot enable it.

## §7 Funded wallets + live probes (ExecutionCalibrationBudget, §39)

- **Phase-A interface:** probe accounting logic and the evidence-store ledger (supervisor
  `store/evidence.py`: `reconciled_outcomes`); `ex_reconcile_fill` supplies per-probe
  outcomes.
- **Required production adapter:** funded probe wallet(s) with the §39 calibration budget
  cap enforced in code before any probe fires.
- **Hardware/credential dependency:** operator-funded wallets (human action; Tier-0
  adjacent) with balances above the wallet floor.
- **Server measurement required:** realized probe costs vs budget; slippage/fill quality
  distributions from live probes.
- **Integration point:** probe results land as `reconciled_outcomes` rows and feed the
  calibration stores (§38) and the research loop's ingestion binding.
- **Acceptance test:** probe ledger reconciles exactly against on-chain balances; budget
  cap provably not exceeded in the journal.
- **Failure behaviour (fail-closed):** budget exhausted or reconciliation mismatch →
  probes stop; no fallback to estimates presented as measurements.
- **LIVE BANKROLL SOURCING (Tier-0-adjacent, constitution §33 / Amendment A-7):** the live
  bankroll is initialized from and continuously reconciled against the on-chain wallet
  balance — NEVER the `bankroll_initial_lamports` config seed (that value is paper/replay
  only). The engine's Phase-B live entry (`Engine::new_live_reconciled` / `set_live_bankroll`)
  must be seeded from the reconciled wallet balance, and `require_live_verified()` (which
  fail-closes on a paper seed) must pass before any live order. The entire sizing chain
  (survival floor → deployable → risk budget → per-position fraction → drawdown hwm) then
  derives from real chain capital. A live arm attempted off a paper seed is refused by
  construction (`tests/bankroll_origin.rs`).
- **Enablement condition:** human funds the wallets and approves the probe budget; the live
  bankroll reconciler is wired to the on-chain balance before arming.

## §8 Fee / priority-fee / tip calibration (CalibrationStore, §38)

- **Phase-A interface:** `pump-quant-execution::ex_tip_compute` (deterministic tip logic
  over supplied calibration inputs, dossier-tested with fixture data).
- **LAPTOP-BUILT (2026-07-23):** `pq-stream-capture fee-sampler` — getPriorityFeeEstimate
  (all levels) + getRecentPrioritizationFees → versioned `fee_calibration_v1` NDJSON with
  integer percentiles. Remaining here: live epoch validated against §7 probe outcomes.
- **Required production adapter:** live fee-market sampler (recent prioritization fees,
  tip landscape) writing versioned calibration records; CalibrationStore backing file/DB.
- **Hardware/credential dependency:** live RPC access for fee sampling; probe results
  from §7 for ground truth.
- **Server measurement required:** landing probability vs (priority fee, tip) surface
  under current network conditions; drift over time.
- **Integration point:** `ex_tip_compute` reads only from the CalibrationStore; stale
  calibration is detectable via record timestamps.
- **Acceptance test:** tip decisions on recorded fixtures match expected outputs; live
  calibration freshness check green before arming.
- **Failure behaviour (fail-closed):** missing/stale calibration → conservative default
  and no-arm for latency-sensitive strategies; never invents fee numbers.
- **Enablement condition:** live sampler deployed + first calibration epoch validated
  against probe outcomes.

## §9 Live sell-path simulateTransaction validation (§35)

- **Phase-A interface:** `pump-quant-execution::ex_sell_ladder_state` /
  `ex_sell_ladder_escalate` (ladder state machine + escalation, dossier-tested);
  `si_incident_gate`; sell-simulation invariant test
  (`dossier_safety_integrity_si_sell_simulation`).
- **Required production adapter:** pre-trade `simulateTransaction` call on the real sell
  route for every armed position's exit path.
- **Hardware/credential dependency:** live RPC with simulation support; funded position
  context (from §6/§7) to make simulation meaningful.
- **Server measurement required:** simulation-vs-actual divergence rate; simulation
  latency budget on the hot exit path.
- **Integration point:** a buy may only arm if its sell path simulates successfully
  (§35); failures feed `si_incident_gate` and the failed-sell state machinery.
- **Acceptance test:** ladder dossier tests stay SHA-locked green; live run journals a
  successful simulation for every armed entry.
- **Failure behaviour (fail-closed):** sell simulation fails → the entry is refused;
  an in-position simulation failure escalates the ladder and raises an incident.
- **Enablement condition:** live RPC simulation wired + divergence measurement journaled;
  human sign-off alongside §6 arming.

## §10 Birdeye daily-candle backfill + token-data lane (REQUIRED source — constitution §6.7)

- **Constitutional status:** REQUIRED. Amendment A-3 (§6.7) designates Birdeye the
  provider of record for 1D OHLCV backfill/cross-check and token-data enrichment for the
  §21.6 bar/market-structure family. This item is not optional at Phase-B activation.
- **Phase-A interface:** `pump-quant-features::bar` (BarBuilder — own canonical flow stays
  the PRIMARY bar source), `pump-quant-features::market_structure` (detectors the daily
  bars condition), `pump-quant-app::structure` (engine wiring); the §21.6 MarketIntelCache
  carry list defines the record shape every backfilled candle must arrive with.
- **Required production adapter:** a `birdeye` capture subcommand in
  `tools/social-ingest-https-rs` (`pq-social-capture` — same ureq+rustls, Cargo.lock-pinned,
  budget-paced, shape-hash-drift-sentinel pattern as the `coingecko` lane) emitting
  provenance-tagged candle + token-data records into MarketIntelCache:
  - `GET public-api.birdeye.so/defi/v3/ohlcv?address=<mint>&type=1D&time_from=&time_to=`
    (count mode, ≤5000 bars/call) — daily candles for watched/held/researched mints;
  - `GET /defi/token_overview?address=<mint>` — liquidity, holders, trade counts, volume,
    buy/sell pressure, price frames;
  - `GET /defi/token_security?address=<mint>` — plan-tier-gated (Starter+); omit cleanly
    on Standard tier, never fabricate.
  - Headers: `X-API-KEY: $BIRDEYE_API_KEY`, `x-chain: solana`.
- **Hardware/credential dependency:** `BIRDEYE_API_KEY` (operator env, never committed);
  plan tier chosen by operator (token_security needs Starter+; re-verify tier gates at
  activation against docs.birdeye.so — verified 2026-07).
- **Server measurement required:** per-endpoint latency/freshness baselines; observed CU
  and rate-limit budget vs plan ceiling; Birdeye 1D candles vs our own canonical daily
  aggregation on overlapping windows (the §21.6 reconciliation status field) — divergence
  distribution journaled before any backfilled bar is admitted as cross-check.
- **Integration point:** MarketIntelCache enrichment ONLY (§6.6/§6.7). Backfilled daily
  bars extend the §21.6 structure detectors' lookback beyond our own capture history;
  token-data fields condition candle analysis as context features. Never a hot-path or
  availability dependency; never populates canonical trades/reserves/market cap (§6.1
  prohibition on Birdeye trade history as raw truth stands).
- **Acceptance test:** fixture-tested parser (recorded Birdeye responses + drift fixture)
  green on the laptop profile; golden digest unchanged (backfill is research-plane);
  reconciliation divergence report journaled on first live epoch.
- **Failure behaviour (fail-closed for claims, fail-open for flow):** outage, 429, or
  schema drift → lane logs loudly and stops emitting; absent Birdeye data is ABSENCE
  (bars simply not backfilled, features conditioned on shorter history), never a halt,
  delay, or degradation of any strategy lane; stale/incomplete candles are rejected by
  the §21.6 screens (missing/stale, wrong-pair, aggregation mismatch, artificial volume).
- **Enablement condition:** `BIRDEYE_API_KEY` provisioned + adapter merged with fixture
  tests + first reconciliation epoch journaled.

## §11 Helius whale-webhook lane (discovery/corroboration tier — §6.6/§28)

- **LAPTOP-BUILT (2026-07-23):** `pq-stream-capture webhook-listener` — pure-std HTTP
  receiver (binds 127.0.0.1 behind a TLS-terminating reverse proxy; Helius requires an
  https URL), authHeader verification (env `WEBHOOK_AUTH_SECRET`, fail-closed), ACK-in-1s-
  then-process (Helius retries 3×1s then drops), signature dedupe ring, raw + normalized
  whale NDJSON. Fixture-tested incl. loopback integration.
- **Required production step:** create the webhook via `POST /v0/webhooks` (enhanced type,
  SWAP/TRANSFER, whale address set ≤100k), point at the proxied listener, register the
  address set from the wallet-cohort research plane.
- **Server measurement required:** delivery lag distribution (confirmed→receipt), loss rate
  vs LaserStream ground truth (the lane is lossy by design — corroboration only).
- **Failure behaviour:** lost deliveries are corroboration ABSENCE, never a halt; nothing
  from this lane populates canonical state or authorizes anything (§6.3/§6.4 re-resolution
  required).
- **Enablement condition:** proxy + secret provisioned, webhook created, lag/loss journaled.

## §12 Discord paid-alpha capture lane (named source — constitution §29/§6.6, Amendment A-5)

- **Constitutional status:** named real-time alpha-call source. Paid alpha rooms → actionable
  alpha at corroboration tier (AlphaCall discovery lane + designated-caller weight + reduce-only
  exit calls; alpha-alone can never admit). Spec: docs/DISCORD_SOURCE.md.
- **Phase-A interface (BUILT + tested):** `pq-stream-capture discord-gateway` — passive read-only
  Discord Gateway v10 client (48 tests); `SocialPlatform::Discord` + `DiscoveryLane::AlphaCall` +
  designated-caller weight + per-room `SourceOutcomeLedger` wired in pump-quant-app (re-pin #14,
  A/B-pinned in tests/alpha_laws.rs).
- **Required production step:** provide `DISCORD_USER_TOKEN` (dedicated throwaway account
  subscribed to the paid rooms; bot token usually can't be added to provider-run rooms),
  configure the guild/channel allowlist for the operator-named rooms + designated-caller
  author-ids, run under the supervisor.
- **Server measurement required:** per-room realized net-SOL (ROI — is each paid room worth it),
  call→confirm lead time, delivery gap/reconnect stats; account-health monitoring.
- **Failure behaviour (fail-open):** outage/rate-limit/ban → lane stops emitting and alerts the
  operator; absent alpha is ABSENCE, never a halt; nothing from this lane populates canonical
  state or authorizes an entry (§29.8/§6.6 — on-chain gate always fires).
- **Posture:** passive, invisible-presence, live-Gateway-only (no REST history scraping), single
  connection; no multi-account rotation / proxy evasion (out of scope by design). User-token
  automation violates Discord ToS — operator-accepted risk; use a dedicated account.
- **Enablement condition:** `DISCORD_USER_TOKEN` provisioned + allowlist configured + first
  per-room ROI epoch journaled.

---

*Nothing in this manifest is self-enabling. Each item requires its stated adapter,
credentials, and server measurement, and the Tier-0 items (§6, §7 funding) additionally
require explicit human action that no agent tool surface can perform.*
