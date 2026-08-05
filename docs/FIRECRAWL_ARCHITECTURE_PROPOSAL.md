# Firecrawl Integration Architecture — Revised Proposal v2
## Self-hosted web intelligence for the mev_bot trading daemon

**Date**: 2026-08-04
**Author**: Hermes (principal Rust/Windows engineer perspective)
**Status**: PROPOSED v2 — awaiting operator approval

**Revisions from v1**:
- Cron cadence changed from 4h → **15 minutes**
- Cron scrapes run **in parallel** (non-blocking, separate processes)
- Bridge confirmed as **Rust binary** in existing workspace
- 4 original triggers + **6 new triggers** from ArXiv research
- Reboot deferred to **end of build** (clean reboot, no work interrupted)
- **Self-healing**: automated startup on power cycle / reboot
- Manual post-reboot steps documented

---

## 1. Objective

Make the bot **think and act like a real principal Solana memecoin quant** by giving it
permanent, on-demand, and autonomous web intelligence capabilities via a self-hosted
Firecrawl instance. Three scraping modes:

1. **Daemon-triggered (reactive)**: the trading daemon scrapes when it detects
   interesting activity — 10 triggers covering band entry, velocity spikes, social
   catalysts, wash-trading signatures, liquidity jumps, and position events.
2. **Autonomous scheduled (proactive)**: a Hermes cron job scrapes curated sources
   **every 15 minutes**, running all source scrapes **in parallel** so no single
   source blocks another.
3. **On-demand (manual)**: Hermes can scrape any URL at any time via the `firecrawl`
   skill, for investigation, due diligence, or operator-directed research.

---

## 2. Existing Codebase Foundation

The architecture was pre-built for this. No engine changes needed:

| Component | Location | Status |
|---|---|---|
| `SocialSource` trait | `pump-quant-ingest/src/social_source.rs` | ✅ exists, pull-based, non-blocking |
| `SocialPlatform::Web` | `pump-quant-ingest/src/social_parse.rs` | ✅ Firecrawl provenance type exists |
| `social_parse.rs` (570 lines) | `pump-quant-ingest/src/` | ✅ pure decoder for normalized web payloads |
| `engine.take_social_batch()` | `pump-quant-app/src/engine.rs:1985` | ✅ engine can ingest social events |
| `social_ingest.rs` | `pump-quant-app/src/` | ✅ fan-out + quality ledger wired |
| Daemon `SocialSource` wiring | `pq_daemon.rs` | ❌ NOT YET WIRED — this proposal adds it |

**Key constraint (§22)**: the engine is synchronous, deterministic, no floats, no
network I/O. The daemon already solves this with the **sidecar pattern** — LaserStream
runs as a child process, communicates via stdout → mpsc channel. Firecrawl uses the
same pattern.

---

## 3. Topology

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Windows Host (DESKTOP-CP8N3IC)                                              │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Docker Desktop (WSL2 backend, auto-start as Windows service, 16GB cap)│ │
│  │                                                                        │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────────┐ │ │
│  │  │ Firecrawl    │  │ Firecrawl    │  │ Redis                        │ │ │
│  │  │ API          │  │ Worker       │  │ :6379 (internal)             │ │ │
│  │  │ :3000        │  │ :3100        │  │ (queue + cache)              │ │ │
│  │  │ (REST)       │  │ (Playwright) │  │                              │ │ │
│  │  └──────┬───────┘  └──────────────┘  └──────────────────────────────┘ │ │
│  │         │ localhost only                                               │ │
│  └─────────┼──────────────────────────────────────────────────────────────┘ │
│            │                                                                 │
│            │ HTTP 127.0.0.1:3000                                             │
│            │                                                                 │
│  ┌─────────┼──────────────────────────────────────────────────────────────┐ │
│  │        │  pq-firecrawl-bridge (sidecar process)                         │ │
│  │        │  ─ spawned by pq-daemon (like LaserStream)                     │ │
│  │        │  ─ reads scrape requests from stdin                            │ │
│  │        │  ─ calls Firecrawl API via HTTP                                │ │
│  │        │  ─ normalizes response → RawSocialPayload JSON                 │ │
│  │        │  ─ writes NDJSON to stdout                                      │ │
│  │        ┼───────────────────────────────────────────────────────────────┼ │
│  │           │ stdout (mpsc channel)                                        │ │
│  │           ▼                                                             │ │
│  │  ┌──────────────────────────────────────────────────────────────────┐  │ │
│  │  │  pq-daemon (persistent event loop)                                │  │ │
│  │  │                                                                    │  │ │
│  │  │  LaserStream ──► ┌──junction──┐ ──► engine.tick()                 │  │ │
│  │  │  PumpPortal  ──► │   queue    │                                    │  │ │
│  │  │  Helius WS   ──► │            │                                    │  │ │
│  │  │  FirecrawlBridge┼────────────┼──► engine.take_social_batch()     │  │ │
│  │  │                  └────────────┘                                    │  │ │
│  │  │                                                                    │  │ │
│  │  │  10 triggers → write scrape request to bridge stdin               │  │ │
│  │  │  Per-mint throttle (1 req / 60s) prevents scrape storms           │  │ │
│  │  └──────────────────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Hermes Agent                                                           │ │
│  │  ┌─────────────┐  ┌──────────────────────────────┐  ┌──────────────┐  │ │
│  │  │ firecrawl   │  │ firecrawl-gather             │  │ fc.sh CLI    │  │ │
│  │  │ (skill)     │  │ (cron, every 15min)          │  │ (bash wrapper)│  │ │
│  │  │ on-demand   │  │ 6 sources scraped IN PARALLEL│  │ curl→:3000   │  │ │
│  │  │             │  │ via background subagents     │  │              │  │ │
│  │  └─────────────┘  └──────────────────────────────┘  └──────────────┘  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │  Self-Healing Layer (Windows Task Scheduler)                            │ │
│  │  ┌──────────────────────────────────────────────────────────────────┐  │ │
│  │  │  pq-startup.ps1 (runs at every boot / power cycle)               │  │ │
│  │  │  1. Start Docker Desktop (if not running)                        │  │ │
│  │  │  2. Wait for Docker daemon ready (poll up to 120s)               │  │ │
│  │  │  3. docker compose up -d (Firecrawl stack)                      │  │ │
│  │  │  4. Wait for Firecrawl health check (poll up to 60s)             │  │ │
│  │  │  5. Run launch_watchdog.sh (dedup guard + pq-daemon)            │  │ │
│  │  │  6. Log to data/startup.log                                     │  │ │
│  │  │  All steps non-fatal: if Docker is down, daemon still launches   │  │ │
│  │  │  (social intelligence degraded, trading continues)              │  │ │
│  │  └──────────────────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## 4. Docker Strategy: Docker Desktop (confirmed)

**Decision: Docker Desktop with WSL2 backend.**

| Factor | Docker Desktop | Raw WSL2 Docker Engine |
|---|---|---|
| **Stability** | ✅ Managed, auto-restart, container `restart: always` | ⚠️ Manual systemd init, fragile on Windows |
| **Latency** | ✅ Negligible overhead (Firecrawl is HTTP, latency = Playwright render time) | ✅ Same |
| **Maintainability** | ✅ Auto-updates, GUI, `docker compose` built-in | ❌ Manual updates, manual WSL2 config |
| **Auto-start** | ✅ Windows service, boots with OS | ❌ Requires manual `wsl --startup` |
| **Resource control** | ✅ GUI cap (16 GB, 4 cores) | ❌ Manual `.wslconfig` editing |
| **Self-healing** | ✅ Docker Desktop auto-starts on boot → `docker compose` with `restart: always` → containers self-heal | ❌ Requires manual intervention |

---

## 5. Component Detail

### 5.1 Firecrawl Stack (Docker Compose)

```
services:
  firecrawl-api:      # REST API, :3000, 127.0.0.1 only
  firecrawl-worker:   # Playwright browsers, :3100
  redis:              # :6379, internal only, maxmemory 512mb

volumes:
  firecrawl-data:     # D:/docker/firecrawl-data (2.3 TB free)

restart: always       # all three services
```

- No API key (self-hosted mode)
- `localhost_only: true` — no external port exposure
- 2 concurrent Playwright workers max
- Redis TTL: 24h cache

### 5.2 pq-firecrawl-bridge (Rust sidecar)

**New binary** in `pump-quant-junction` crate, alongside `pq-daemon` and `pq-watchdog`.

```
 pq-firecrawl-bridge
   ├── stdin:  NDJSON scrape requests (one per line)
   │          {"type":"coin_page","mint":"<base58>","url":"https://pump.fun/<mint>"}
   │          {"type":"social","mint":"<base58>","query":"$TICKER solana"}
   │          {"type":"news","url":"https://www.coindesk.com/solana"}
   │          {"type":"dex","mint":"<base58>","url":"https://dexscreener.com/solana/<mint>"}
   │          {"type":"creator","mint":"<base58>","url":"<creator-profile-url>"}
   │          {"type":"wash_check","mint":"<base58>","url":"<dexscreener-url>"}
   │
   ├── HTTP:  POST http://127.0.0.1:3000/v2/scrape  (or /v2/crawl, /v2/map)
   │
   └── stdout: NDJSON RawSocialPayload (one per line)
              {"json": <normalized>, "observed_at_ns": <u64>}
```

- **Non-blocking**: the daemon spawns it in a thread, reads stdout via mpsc channel
- **Determinism-safe**: the daemon never touches the network — the bridge does all HTTP
- **Health check**: the bridge checks `http://127.0.0.1:3000/health` on startup; if
  down, it exits immediately with a distinct code, and the daemon logs "Firecrawl
  unavailable — social intelligence degraded" but continues trading
- **Rate limiting**: bridge internally limits to 2 req/sec, queues excess
- **Timeout**: 15s per scrape request; on timeout, writes an error payload (not a
  panic)
- **No secrets**: self-hosted Firecrawl needs no API key; bridge has zero credentials

### 5.3 Daemon Trigger Logic — 10 Triggers

The daemon decides WHEN to scrape. The first 4 are original; triggers 5–10 are
derived from ArXiv research on crypto trading signals.

#### Original Triggers (operator-approved)

**TRIGGER 1: Band entry** — A coin's mcap crosses the $9k floor upward into the
$9k–$20k band. → Scrape pump.fun coin page + DexScreener + Twitter `$TICKER`.
*Why:* This is the core admission signal. The quant wants to know: is there a
narrative? Is the flow organic or spray? Who's buying?

**TRIGGER 2: Velocity spike** — Trade count for a mint exceeds N trades in M ticks
(unusual concentration). → Scrape social + aggregator listings.
*Why:* Sudden velocity often precedes or accompanies a social catalyst. The quant
investigates whether the activity is correlated with an external event.

**TRIGGER 3: New mint promotion** — A fresh `create` event is promoted to the
junction queue. → Scrape pump.fun coin page (reconnaissance per A-14).
*Why:* Per Amendment A-14 directive 3 — watch mints to understand new pairs. This is
reconnaissance, not a commitment to enter.

**TRIGGER 4: Position entry / exit** — The engine admits or exits a position.
→ Scrape news + social for context.
*Why:* Post-mortem catalyst detection. Was there a protocol event, a news catalyst,
or a social-media-driven pump? This feeds the refiner's edge measurement.

#### ArXiv-Research-Enhanced Triggers (new)

**TRIGGER 5: Order-flow entropy shift** — The rolling Markov transition matrix of
trade directions (buy/sell) shows an entropy spike, indicating informed flow is
entering the market. → Scrape DexScreener depth + Twitter mentions.
*Academic basis:* "Hidden Order in Trades Predicts the Size of Price Moves"
(arXiv:2512.15720) — order-flow entropy computed from a Markov transition matrix
predicts the *magnitude* of price moves without predicting direction. A spike in
entropy means a large move (in either direction) is imminent.
*Quant value:* The bot doesn't need to know the direction — it needs to know that
*something is about to happen*. For a scalp/early-rotation bot, volatility is
opportunity. Scrape to understand whether the incoming move has a real catalyst or is
synthetic.

**TRIGGER 6: Wash-trading liquidity signature** — A mint's trade pattern shows
the characteristic liquidity-jump / liquidity-diffusion signature of wash trading
(repeated small trades between correlated wallets, artificially inflating volume).
→ Scrape DexScreener holder distribution + wallet activity.
*Academic basis:* "Liquidity Jump, Liquidity Diffusion, and Crypto Wash Trading"
(arXiv:2411.05803) — wash trading in crypto assets produces detectable short-term
liquidity fluctuations. Two complementary measures: liquidity jump (size of
fluctuation) and liquidity diffusion (volatility of fluctuation).
*Quant value:* Wash-traded volume is *fake activity*. If the bot detects a wash-
trading signature on a coin it's watching or holding, it should immediately
re-assess: the "volume" that triggered admission may be synthetic. Scrape to confirm
or deny the wash-trading hypothesis by checking holder distribution and wallet
patterns on DexScreener.

**TRIGGER 7: Social sentiment divergence** — A coin's price is rising but social
sentiment (Twitter/Reddit mention volume, Google Trends) is flat or declining — or
conversely, social sentiment is spiking but price hasn't moved yet. → Scrape
Twitter/Reddit + Google Trends for the ticker.
*Academic basis:* "Social signals and algorithmic trading of Bitcoin" (arXiv:1506.01513)
— digital traces of human behavior (social media, search trends) have predictive
power for crypto trading. "Forecasting Cryptocurrencies Log-Returns: a LASSO-VAR and
Sentiment Approach" (arXiv:2210.00883) — Twitter + Reddit sentiment + Google Trends
predict crypto returns.
*Quant value:* Price-social divergence is a classic quant signal. If price rises
without social confirmation, the move may be manipulated or unsustainable. If social
spikes before price, there may be an informational edge. Either way, the bot scrapes
to measure the divergence and feed it to the refiner.

**TRIGGER 8: Creator wallet clustering** — A new mint's creator wallet shows
on-chain links to previous rug-pulls or failed launches (shared funding sources,
overlapping transaction patterns). → Scrape the creator's wallet history on Solscan
+ pump.fun creator page.
*Academic basis:* "Detecting Sybil Addresses in Blockchain Airdrops: A Subgraph-based
Feature Propagation and Fusion Approach" (arXiv:2505.09313) — sybil address
identification via subgraph feature extraction. Sybil clusters share funding
sources and coordinated event sequences.
*Quant value:* A creator with a history of rugs is a red flag. The bot scrapes the
creator's wallet history to check for prior failed launches, rug patterns, or
connections to known bad actors. This is survival bias avoidance — the quant avoids
coins where the downside is a designed-in rug rather than organic volatility.

**TRIGGER 9: MEV invariance violation** — The observed price on the bonding curve
deviates from the invariant-implied price by more than the fee + slippage band,
indicating a temporary arbitrage opportunity or an impending reversion. → Scrape
DexScreener for cross-venue price + Raydium/PumpSwap liquidity.
*Academic basis:* "Invariance properties of maximal extractable value" (arXiv:2304.11010)
— for blockchains with deterministic block times, the total arbitrage opportunity
from on-chain liquidity pools satisfies invariance properties. Deviations from these
invariants signal extractable value.
*Quant value:* When the bonding-curve price diverges from the invariant, either
there's a real arbitrage opportunity (trade it) or the pool is about to revert
(avoid or fade). The bot scrapes cross-venue data to determine which.

**TRIGGER 10: Liquidity depth collapse** — The order-book depth on a watched coin
thins by more than X% in Y ticks (measured via the bonding-curve reserve ratio),
indicating either a pending migration event or a rug. → Scrape pump.fun coin page
+ DexScreener for migration indicators.
*Academic basis:* "UNISWAP: Impermanent Loss and Risk Profile of a Liquidity
Provider" (arXiv:2106.14404) and "From Impermanent Loss to Sustainable Gain"
(arXiv:2604.28014) — AMM liquidity depth is a measurable risk factor. Sudden depth
collapse is a leading indicator of migration or malicious drain.
*Quant value:* A sudden depth collapse on a coin the bot is watching or holding is
a survival signal. The bot scrapes to check: is this a legitimate pump.fun
migration (graduation to Raydium) or a liquidity drain? If migration, the bot may
want to ride the graduation volatility. If drain, exit immediately.

#### Trigger Throttling

Each mint gets at most 1 scrape request per 60 seconds, de-duped via a
`HashMap<mint, last_scrape_tick>`. This prevents scrape storms during high-velocity
events. Multiple triggers for the same mint within the throttle window are coalesced
into a single multi-target scrape request.

### 5.4 Hermes `firecrawl` Skill

```
SKILL.md:
  - endpoint: http://127.0.0.1:3000
  - subcommands: scrape, crawl, map, search
  - output: JSON to stdout
  - health check: curl -sf http://127.0.0.1:3000/health
  - pitfalls: rate limits, JS-rendered pages need worker, timeout handling
```

Loaded on demand for operator-directed research, refiner cron context gathering,
and ad-hoc due diligence.

### 5.5 Hermes `firecrawl-gather` Cron (autonomous, every 15min, parallel)

Scheduled broad-spectrum scraping. **All 6 sources scraped in parallel** via
background subagents so no single source blocks another. If one source is slow or
down, the others still complete on time.

| Source | URL | Purpose | Timeout |
|---|---|---|---|
| pump.fun trending | `pump.fun/dashboard/trending` | What's hot right now | 30s |
| pump.fun new launches | `pump.fun/dashboard/new` | Fresh mints for recon | 30s |
| DexScreener Solana | `dexscreener.com/solana` | DEX liquidity + volume | 30s |
| CoinGecko trending | `coingecko.com/en/high-volume-trending` | Cross-venue sentiment | 30s |
| Solana ecosystem news | `solana.com/news` | Protocol upgrades, incidents | 30s |
| Crypto news | `coindesk.com` + `theblock.co` | Market-wide events | 30s |

**Parallel execution**: each source is scraped by a separate background process
(`fc.sh scrape <url> &` with a 30s timeout, then `wait`). Results are collected
and written to `D:/repos/mev_bot/data/firecrawl/<timestamp>/` as individual JSON
files. If a source times out, its file is empty — no blockage.

Output is timestamped and rotated (keep last 7 days, auto-delete older).

### 5.6 Firecrawl CLI Wrapper (`fc.sh`)

Thin bash wrapper at `D:/repos/mev_bot/tools/firecrawl-cli/fc.sh`:
- `fc.sh scrape <url>` → JSON
- `fc.sh crawl <url> <limit>` → JSON
- `fc.sh map <url>` → JSON
- `fc.sh search <query>` → JSON
- `fc.sh health` → health check
- No API key, no secrets in logs

---

## 6. Self-Healing Architecture

### 6.1 Windows Task Scheduler — `pq-startup.ps1`

A PowerShell script registered as a **Windows Task Scheduler** job that runs at every
boot / power cycle / user login. It replaces the need for a human to restart anything
after a reboot or power loss.

```
pq-startup.ps1:
  1. Check if Docker Desktop is running. If not, start it.
  2. Wait for Docker daemon to be ready (poll `docker info` up to 120s).
  3. Run `docker compose up -d` in D:/repos/firecrawl.
  4. Wait for Firecrawl health check (poll http://127.0.0.1:3000/health up to 60s).
  5. Run `bash launch_watchdog.sh` (which itself has 3-layer dedup guard).
  6. Log every step to data/startup.log with timestamps.
  7. If any step fails, log the error but continue to the next.
     — Docker down? Daemon still launches (social intelligence degraded).
     — Firecrawl down? Daemon still launches (social intelligence degraded).
     — Watchdog down? Log critical error (trading is down).
```

**Key property**: every step is non-fatal except the watchdog launch. If Docker or
Firecrawl is unavailable, the trading daemon still starts. Social intelligence is
a bonus layer, not a dependency. The bot trades without it.

### 6.2 Docker `restart: always`

All three Firecrawl containers (API, Worker, Redis) have `restart: always`. If any
container crashes, Docker automatically restarts it. If Docker Desktop itself
crashes, the Windows service auto-starts it.

### 6.3 Watchdog → Daemon

The existing watchdog already has:
- PID-file single-instance guard
- Max-restart backoff
- Health-timeout detection via stale `live_status.json`
- `restart: always` equivalent (the Task Scheduler re-runs `launch_watchdog.sh`)

### 6.4 Refiner Cron

The existing refiner cron (every 2 days) already has a dedup check as its first
step. It continues to run independently of the Firecrawl layer.

---

## 7. Fail-Safe Architecture

| Concern | Mitigation |
|---|---|
| **Power cycle / reboot** | Windows Task Scheduler runs `pq-startup.ps1` → Docker → Firecrawl → watchdog → daemon. Full self-healing. |
| **Docker crash** | Docker Desktop auto-restarts as Windows service. Containers `restart: always`. |
| **Firecrawl API down** | Bridge health-checks on startup → exits with distinct code → daemon logs "social intelligence degraded" → continues trading. Engine never blocks. |
| **Bridge crash** | Daemon detects stdout pipe broken → logs → continues trading. Social intelligence is a bonus, not a dependency. |
| **Memory pressure** | Docker capped 16 GB. Playwright 2 concurrent. Redis 512 MB maxmemory. Total ~6.6 GB. |
| **Scrape storm** | Per-mint throttle (1 req/60s). Bridge rate limit (2 req/s). Cron sources have 30s timeout each. |
| **Disk exhaustion** | Firecrawl cache on D: (2.3 TB free). Cache TTL 24h. Cron output rotated (keep 7 days). |
| **Network isolation** | Firecrawl bound 127.0.0.1 only. No inbound from outside. |
| **Secret safety** | Zero API keys (self-hosted). Zero credentials in bridge, wrapper, logs, or command lines. |
| **Engine determinism** | Daemon never calls HTTP. Bridge is a separate process. Social data enters through the `SocialSource` trait. |
| **Cron parallelism** | Each source scraped in a separate background process with 30s timeout. One slow source doesn't block others. |
| **Dedup with trading stack** | Firecrawl in Docker containers. Bridge is a child process. Zero port collision (3000/3100/6379 vs 8080). |

---

## 8. Resource Budget

| Component | RAM | CPU | Disk |
|---|---|---|---|
| Docker Desktop + WSL2 | 4 GB baseline | 2 cores | 20 GB |
| Firecrawl API + Worker | 2 GB | 1 core | 5 GB |
| Redis | 512 MB | 0.5 core | 1 GB |
| pq-firecrawl-bridge | 50 MB | 0.25 core | negligible |
| **Total** | ~6.6 GB | 3.75 cores | 26 GB |
| **Host has** | 256 GB (9 GB free) | Zen5 12+ cores | 2.3 TB free |

---

## 9. Implementation Phases

### Phase F1: Docker + Firecrawl Stack
1. Install Docker Desktop via `winget`
2. Clone Firecrawl: `D:/repos/firecrawl`
3. Configure `.env` (self-hosted, localhost-only, no API key)
4. `docker compose up -d`
5. Health check: `curl http://127.0.0.1:3000/health`
*(No reboot yet — WSL2 may already be enabled. If not, we reboot at the end.)*

### Phase F2: CLI Wrapper + Hermes Skill
6. Write `fc.sh` wrapper
7. Create `firecrawl` skill (SKILL.md)
8. Test on-demand scraping end-to-end

### Phase F3: Cron Job (autonomous scheduled, 15min, parallel)
9. Create `firecrawl-gather` cron (every 15min, 6 sources in parallel)
10. Test first scheduled run
11. Verify output lands in `data/firecrawl/`

### Phase F4: Bridge Binary (daemon-triggered)
12. Write `pq-firecrawl-bridge` Rust binary in `pump-quant-junction`
13. Wire into daemon event loop (spawn like LaserStream, mpsc channel)
14. Implement all 10 triggers
15. Add per-mint throttle + coalescing
16. Test: simulate trigger events → daemon triggers scrape → bridge calls Firecrawl → result feeds into engine
17. Unit tests for trigger logic (deterministic, no network)

### Phase F5: Self-Healing + Integration Tests
18. Write `pq-startup.ps1` self-healing script
19. Register in Windows Task Scheduler (runs at boot)
20. Integration test: full pipeline (daemon → bridge → Firecrawl → engine)
21. Regression: confirm golden tape still byte-stable (bridge is opt-in, not in `dev_portable()`)
22. Compile, test, commit, push

### Phase F6: Clean Reboot (operator-initiated)
23. Operator reboots the machine (one-time, for WSL2 kernel enablement if needed)
24. On reboot, `pq-startup.ps1` auto-launches: Docker → Firecrawl → watchdog → daemon
25. Verify all services healthy post-reboot

---

## 10. What a Real Quant Scrapes (10 triggers)

| # | Trigger | What to Scrape | Academic Basis |
|---|---|---|---|
| 1 | Band entry | pump.fun coin page, DexScreener, Twitter | Operator directive (A-14) |
| 2 | Velocity spike | Twitter mentions, Telegram channels | Operator directive |
| 3 | New mint promoted | pump.fun coin page, creator wallet | A-14 directive 3 (recon) |
| 4 | Position entry/exit | News sites, Solana ecosystem | Operator directive |
| 5 | Order-flow entropy spike | DexScreener depth, Twitter | arXiv:2512.15720 — order-flow entropy predicts move magnitude |
| 6 | Wash-trading signature | DexScreener holders, wallet activity | arXiv:2411.05803 — liquidity jump/diffusion detects wash trading |
| 7 | Social sentiment divergence | Twitter/Reddit, Google Trends | arXiv:1506.01513, 2210.00883 — social signals predict crypto returns |
| 8 | Creator wallet clustering | Solscan wallet history, pump.fun creator | arXiv:2505.09313 — sybil address detection via subgraph features |
| 9 | MEV invariance violation | DexScreener cross-venue, Raydium liquidity | arXiv:2304.11010 — MEV invariance properties |
| 10 | Liquidity depth collapse | pump.fun coin page, DexScreener migration | arXiv:2106.14404, 2604.28014 — AMM depth as risk factor |

This is **reconnaissance, not prediction** (Amendment A-14, directive 3). The bot
gathers context to understand WHAT is happening, not to predict what will happen.
The edge is venue structure and attention dynamics, measured net of fees.

---

## 11. Manual Post-Reboot Steps

After the clean reboot at the end of the build, **the operator should need to do
nothing**. The self-healing layer handles everything:

1. **Windows Task Scheduler** runs `pq-startup.ps1` at boot.
2. The script starts Docker Desktop, waits for it, starts Firecrawl, waits for
   health check, then runs `launch_watchdog.sh`.
3. The watchdog's 3-layer dedup guard ensures only one daemon instance starts.
4. The daemon connects to PumpPortal, Helius, LaserStream, and the Firecrawl bridge.

**The only thing the operator might need to do manually**:

- If this is the **first time** WSL2 is enabled on this machine, the reboot is
  required for the kernel change to take effect. After the reboot, Docker Desktop
  will prompt for WSL2 confirmation once — click "OK" (or it may auto-accept).
  This is a one-time interaction.

- If Docker Desktop asks for a **license agreement** acceptance on first launch,
  click "Accept" (free for personal use). One-time.

**After that first reboot, all subsequent reboots / power cycles are fully
automated.** No human intervention required.

---

## 12. Decision Points (resolved)

| Decision | Resolution |
|---|---|
| Docker Desktop vs WSL2 raw | **Docker Desktop** (stability, auto-start, self-healing) |
| Cron cadence | **Every 15 minutes** (operator-directed) |
| Cron parallelism | **All sources in parallel** via background processes with 30s timeout each |
| Bridge language | **Rust binary** in `pump-quant-junction` (operator-approved) |
| Trigger scope | **10 triggers** (4 original + 6 ArXiv-enhanced) (operator-approved 4, added 6) |
| Reboot timing | **End of build** (clean reboot, no work interrupted) |
| Self-healing | **Windows Task Scheduler** + `pq-startup.ps1` (automates all post-reboot startup) |
| Manual post-reboot | **None** (except first-time WSL2/Docker license click-through) |

---

Awaiting approval or further revisions.
