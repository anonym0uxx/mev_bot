# pump-quant

> High-frequency Solana MEV bot — backrunner + post-graduation momentum  
> Single Rust binary | 5-feed architecture | Bayesian signal engine | Paper mode default

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                          pump-quant  (single Rust binary)                       │
│                                                                                 │
│  DATA FEEDS (5 live)              EVENT ROUTING            TRADING ENGINES      │
│  ───────────────────              ─────────────            ───────────────      │
│                                                                                 │
│  ┌──────────────────┐             ┌─────────────┐         ┌────────────────┐   │
│  │ Jito ShredStream │──gRPC──►    │             │─────►   │ BackrunEngine  │   │
│  │ (~0ms from shred)│             │  EventJoiner│         │ Bayesian α/β   │   │
│  └──────────────────┘             │  (dedup +   │         │ Kelly sizing   │   │
│                                    │   fan-in)   │         │ V4 urgency exit│   │
│  ┌──────────────────┐             │             │         └───────┬────────┘   │
│  │  PumpPortal WS   │──trade──►   │  sig-based  │                 │             │
│  │  BC buy/sell      │             │  dedup      │                 │             │
│  └──────────────────┘             │  (per-feed  │         ┌───────┴────────┐   │
│                                    │   evidence  │         │  RideState     │   │
│  ┌──────────────────┐             │   weights)  │         │  composite     │   │
│  │  Helius WS       │──trade/──►  └─────────────┘         │  signal engine │   │
│  │  logsSubscribe   │  grad                               │  • Bayesian f̂* │   │
│  │  (~50ms lead)    │                                     │  • momentum    │   │
│  └──────────────────┘             ┌─────────────┐         │  • vol trail   │   │
│                            ┌──►   │  GradArb    │         │  • liquidity   │   │
│  ┌──────────────────┐      │     │  Engine      │         └───────┬────────┘   │
│  │  CoreCast WS     │──────┤     │  spread≥3%   │                 │             │
│  │  3 subscriptions │      │     │  Raydium only│                 │             │
│  │  • DEX trades    │      │     └──────────────┘                 │             │
│  │  • AMM migration │      │                              ┌───────┴────────┐   │
│  │  • LP removal    │      └──►  ┌──────────────┐         │  Position      │   │
│  └──────────────────┘             │  Momentum    │         │  Manager       │   │
│                                   │  Engine      │         │  50ms tick     │   │
│  ┌──────────────────┐             │  score≥40    │         └───────┬────────┘   │
│  │  Social Signals  │             └──────────────┘                 │             │
│  │  (Phase 0 - log) │                                             ▼             │
│  │  Twitter/TG/etc  │                                     ┌──────────────┐     │
│  └──────────────────┘                                     │   OUTPUT     │     │
│                                                            │              │     │
│  SHARED INFRA                                             │  JSONL dv10  │     │
│  ─────────────                                             │  SQLite WAL  │     │
│  • HealthMonitor (PumpPortal + Helius + CoreCast)         │  REST :9421  │     │
│  • ShredStream gRPC (Jito proxy on :20100)                │  Telegram    │     │
│  • BlockhashCache (30s TTL)                                └──────────────┘     │
│  • TipEngine (conviction-aware Jito tips)                                       │
│  • Jito HTTP/2 bundle submission (dual block-engine)                            │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Exit Engine Pipeline (V4 — Live)

The exit engine uses a unified urgency score U ∈ [0, 10000] computed from four signals:

```
U = w_kelly × u_kelly + w_momentum × u_momentum + w_vol × u_vol_trail + w_liq × u_liquidity
    (45%)              (30%)                  (15%)               (10%)

Thresholds:
  U < 3000   → HOLD (edge intact)
  3000-5000  → TIGHTEN (narrow trail width)
  5000-7000  → PARTIAL EXIT (sell 35% of remaining)
  7000-9000  → MAJORITY EXIT (sell 60% of remaining)
  U ≥ 9000   → FULL EXIT (sell everything)
```

**Key features:**
- Bayesian Kelly edge decay (α/β posterior → half-Kelly fraction)
- Momentum divergence detection (20-event ring buffer, buy/sell ratio divergence)
- Volatility-adaptive trailing stop (EMA-8 of absolute price deltas)
- Liquidity-aware slippage urgency (position size vs curve reserves)
- Monotonic urgency floor (ratchet up, never down — can't re-accumulate after partial)

---

## Feed Architecture

| Feed | Transport | Reconnect | Latency | Role |
|---|---|---|---|---|
| **Jito ShredStream** | gRPC (via local proxy) | 100ms→5s backoff | ~0ms (shred decode) | Fastest trade detection, graduation via shreds |
| **PumpPortal** | WSS | 1s→60s backoff | ~120ms | Primary BC trade stream, token creation events |
| **Helius** | WSS | 1s→30s backoff | ~50ms | logsSubscribe (graduation), accountSubscribe (prices) |
| **CoreCast/Bitquery** | WSS (3 muxed streams) | 1s→60s backoff | ~80ms | Creator sells, AMM migrations, LP removal/rug |
| **Social** | Phase 0 (logging only) | — | — | Twitter/Telegram/Discord signal aggregation |

**Self-healing:** HealthMonitor tracks per-feed staleness via atomics. Any feed >45s stale → auto-pause trading → auto-resume on recovery. Each feed has independent reconnect with exponential backoff.

**Evidence dedup:** EventJoiner deduplicates trades by signature across feeds. Each feed has calibrated evidence weights for the Bayesian model (PumpPortal=10, Helius=12, CoreCast=8, ShredStream=15). Deduped events update position state but skip α/β evidence to prevent double-counting.

---

## Fee Model

| Component | Cost | Basis Points |
|---|---|---|
| Pump.fun buy fee | 1% | 100 bp |
| Pump.fun sell fee | 1% | 100 bp |
| Jito tip (default) | 50,000 lamports | ~10 bp @ 0.05 SOL |
| **Total round-trip** | | **210 bp** |

**Dynamic tip engine:** Context-aware tip sizing — SCALP (500μSOL), RIDE early (1mSOL), momentum (2mSOL), tighten (3mSOL), emergency (5mSOL). 5% of profit fraction, congestion multiplier at <80% landing rate. Capped at 5mSOL.

**Jito bundle submission:** Persistent HTTP/2 to dual block engines (Frankfurt + Amsterdam) with automatic failover. <10ms from bundle-ready to wire.

---

## Engines

| Engine | Trigger | Entry Signal | Exit Strategy | Max Hold |
|---|---|---|---|---|
| **Backrun** | BC trade (PumpPortal/ShredStream) | Gate stack + Bayesian Kelly sizing | V4 urgency (partial→full) + RIDE trail | 60s ride / 1.5s scalp |
| **Momentum** | Graduation (Helius/CoreCast) | Score ≥ 40/100 | 3-tier TP + trailing + time SL | 300s |

> **Note:** GradArb engine (BC→Raydium spread arbitrage) exists in code but is **disabled** — PumpSwap migration killed the structural arb spread.

---

## Social Signal Infrastructure (Phase 0)

Logging-only infrastructure for social signal data collection:

```rust
SocialAggregator  ←  SocialSignal { mint, source, type, followers, engagement, is_bot }
    │
    ├── MintSocialProfile (per-token: mentions, sources, followers, bot/organic split)
    │
    └── social_score() → 0-10000
          weights: sources(30%) + organic(25%) + reach(20%) + recency(15%) + diversity(10%)
```

**JSONL fields (dataVersion 10):** `socialScore`, `socialMentions`, `socialUniqueSources`, `socialHasTwitter`, `socialHasTelegram`, `socialHasWebsite`, `socialBotMentionPct`, `socialMaxFollowers`

---

## Quick Start

```bash
# Build
cd rust && cargo build --release

# Configure
cp config/canary.json.example config/canary.json
# Edit: set HELIUS_API_KEY, RPC_URL, BITQUERY_API_KEY in rust/.env

# Start Jito ShredStream proxy (required for fastest feed)
cd shredstream-proxy && ./target/release/jito-shredstream-proxy shredstream \
  --block-engine-url https://mainnet.block-engine.jito.wtf \
  --auth-keypair ../config/keys/shredstream-keypair.json \
  --desired-regions ny --dest-ip-ports 127.0.0.1:20000 \
  --grpc-service-port 20100 &

# Run (single-daemon enforced, kills duplicates)
bash scripts/ensure-single-daemon.sh --start

# Check health (all feeds)
curl http://127.0.0.1:9421/api/health | jq

# Check stats
curl http://127.0.0.1:9421/api/stats | jq

# View P&L
PAPER_MODE=true node scripts/rust-status.js
```

---

## Configuration

Key parameters in `config/canary.json` → `mev` section:

| Parameter | Default | Purpose |
|---|---|---|
| `paper_mode` | `true` | No real trades — paper logging only |
| `exit_v4.enabled` | `true` | V4 urgency-based exit engine (live) |
| `trigger_min_buy_sol` | `0.15` | Minimum trigger buy for backrun |
| `min_vsol_in_curve` | `15` | Minimum vSOL for entry |
| `max_vsol_in_curve` | `70` | Maximum vSOL for entry |
| `jito_tip_lamports` | `50000` | Default Jito bundle tip |
| `round_trip_fee_bp` | `210` | Fee model (pump 200bp + jito ~10bp) |
| `max_concurrent_positions` | `5` | Simultaneous open positions |
| `consecutive_stop_pause_count` | `3` | Circuit breaker: 3 SL → pause |

---

## Project Structure

```
pump-quant/
├── rust/pump-quant-core/src/
│   ├── engine/
│   │   ├── hot_path.rs         # Gate stack → scorer → position open
│   │   ├── positions.rs        # Position lifecycle + ClosedPosition
│   │   ├── ride_state.rs       # RideState: Bayesian + V4 urgency + trail
│   │   ├── exit_v4.rs          # V4 urgency engine (momentum/vol/kelly/liq)
│   │   ├── exit_machine.rs     # Legacy exit state machine (conviction scaling)
│   │   ├── bayesian_signal.rs  # Alpha/beta evidence + Kelly fraction
│   │   ├── kelly_sizing.rs     # Fee-adjusted Kelly optimal sizing
│   │   ├── health.rs           # Feed staleness + auto-pause/resume
│   │   ├── risk_manager.rs     # Daily loss cap + circuit breaker
│   │   ├── gates.rs            # 8-gate entry filter
│   │   ├── scorer.rs           # Multi-factor composite scorer
│   │   ├── config.rs           # Runtime config from canary.json
│   │   └── regime.rs           # Bonding curve regime classification
│   ├── feeds/
│   │   ├── shredstream.rs      # Jito ShredStream gRPC client
│   │   ├── pumpportal.rs       # PumpPortal WSS client
│   │   ├── helius.rs           # Helius WSS (logs + accounts)
│   │   ├── corecast.rs         # CoreCast/Bitquery WSS (3 streams)
│   │   ├── social.rs           # Social signal aggregator (Phase 0)
│   │   └── event_joiner.rs     # Sig-based dedup + crossbeam fan-in
│   ├── tx/
│   │   ├── jito.rs             # Jito REST bundle submission
│   │   ├── jito_grpc.rs        # Persistent HTTP/2 dual block-engine
│   │   ├── tip_engine.rs       # Conviction-aware dynamic tip sizing
│   │   ├── builder.rs          # Transaction construction
│   │   ├── executor.rs         # Buy/sell bundle orchestration
│   │   └── wallet.rs           # Keypair management
│   ├── momentum/               # Post-graduation momentum engine
│   ├── persistence/
│   │   ├── paper_logger.rs     # JSONL trade logger (dataVersion 10)
│   │   └── sqlite.rs           # SQLite WAL persistence
│   ├── api/server.rs           # axum REST on :9421
│   ├── alerts/telegram.rs      # Rate-limited Telegram alerts
│   └── main.rs                 # Binary entry point
├── shredstream-proxy/          # Jito ShredStream proxy binary
├── config/canary.json          # Active runtime config
├── scripts/
│   ├── ensure-single-daemon.sh # PID-enforced single instance
│   └── rust-status.js          # P&L report + heartbeat state
└── data/                       # JSONL + SQLite (gitignored)
    └── backrun_paper_trades.jsonl
```

---

## JSONL Trade Record (dataVersion 10)

Every closed position logs 60+ fields including:

- **Entry context:** triggerBuySol, curvePct, uniqueBuyerCount, preTriggerBuys{1,2,5}s
- **Exit context:** exitReason, holdMs, signalScoreAtExit, signalStateAtExit
- **PnL:** sizeSol, pnlSol, netPnlSol, feesSol, pnlPct
- **MFE/MAE:** mfeSol, mfePct, maeSol, maePct
- **Bayesian state:** bayesianFAtExit, alphaAtExit, betaAtExit, rEstAtExit
- **Kelly conviction:** entryPPermille, entryRx100, entryFPermille, convictionTier
- **V4 urgency:** v4UrgencyAtExit, v4UKelly, v4UMomentum, v4UVolTrail, v4ULiquidity
- **Social (Phase 0):** socialScore, socialMentions, socialUniqueSources, socialHas{Twitter,Telegram,Website}

---

## Status

**Paper mode active 24/7.** V4 urgency exits enabled (live). All 5 feeds connected with self-healing reconnect.

**Prerequisites for live trading:**
- [ ] Win rate ≥ 50% on 100+ paper trades (Rust V4 engine)
- [ ] 48h continuous uptime with stable feeds
- [ ] Circuit breaker verified (3 SL → pause confirmed in logs)
- [ ] Feed health auto-pause verified (stale → pause → resume)
- [ ] V4 urgency calibration on 200+ trades
