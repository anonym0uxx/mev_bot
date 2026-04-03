# pump-quant

> Post-graduation momentum engine for Solana PumpSwap/Raydium  
> Single Rust binary | 4-feed architecture | Kelly-sized probes | Live trading

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                    pump-quant  (single Rust binary, ~29K lines)              │
│                                                                              │
│  DATA FEEDS (4 live)             EVENT ROUTING           MOMENTUM ENGINE    │
│  ───────────────────             ─────────────           ────────────────   │
│                                                                              │
│  ┌──────────────────┐            ┌─────────────┐        ┌───────────────┐  │
│  │ Jito ShredStream │──gRPC──►   │             │        │  Scorer       │  │
│  │ (~0ms from shred)│            │  EventJoiner│──►     │  (graduation  │  │
│  └──────────────────┘            │  (sig dedup │        │   quality)    │  │
│                                   │   + fan-in) │        └───────┬───────┘  │
│  ┌──────────────────┐            │             │                │          │
│  │  PumpPortal WS   │──trade──►  └─────────────┘        ┌───────▼───────┐  │
│  │  BC buy/sell      │                                    │  Observation  │  │
│  └──────────────────┘            ┌─────────────┐        │  Window (5s)  │  │
│                                   │  Helius     │        └───────┬───────┘  │
│  ┌──────────────────┐            │  PumpSwap   │                │          │
│  │  Helius WS       │──trade/──► │  graduation │        ┌───────▼───────┐  │
│  │  logsSubscribe + │  grad      │  detector   │        │  Kelly Probe  │  │
│  │  txSubscribe     │            └──────┬──────┘        │  (~0.03 SOL)  │  │
│  └──────────────────┘                   │               └───────┬───────┘  │
│                                          │                       │          │
│  ┌──────────────────┐                   │               ┌───────▼───────┐  │
│  │  CoreCast WS     │──migration──►     │               │  Position Mgr │  │
│  │  3 subscriptions │                   │               │  • probe hold │  │
│  │  • DEX trades    │                   └──────────►    │  • trailing   │  │
│  │  • AMM migration │                                   │  • TP 1/2/3   │  │
│  │  • LP removal    │                                   │  • time SL    │  │
│  └──────────────────┘                                   │  • hard SL    │  │
│                                                          │  • velocity   │  │
│  SHARED INFRA                                           └───────┬───────┘  │
│  ─────────────                                                  │          │
│  • HealthMonitor (per-feed staleness → auto-pause)              ▼          │
│  • ShredStream gRPC proxy (:20100)                      ┌──────────────┐  │
│  • BlockhashCache (25s refresh)                          │   OUTPUT     │  │
│  • Jito HTTP/2 (NY + Frankfurt dual block-engine)       │              │  │
│  • Nozomi (EWR1 fast endpoint)                           │  JSONL log   │  │
│  • WSOL ATA pre-creation on startup                      │  REST :9421  │  │
│                                                           │  Telegram    │  │
│                                                           └──────────────┘  │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## How It Works

1. **Graduation detected** — Helius transactionSubscribe or CoreCast spots a token migrating from Pump.fun bonding curve to PumpSwap AMM (or Raydium)
2. **Pool resolved** — Engine fetches on-chain pool accounts, validates liquidity ≥ 30 SOL
3. **Scored** — Graduation quality scored 0-100 (bonding curve dynamics, flow momentum, breadth, manipulation detection)
4. **Observation window** — 5s window monitors post-graduation price action via WebSocket
5. **Kelly-sized probe** — If score ≥ threshold, buys a probe position (~0.03 SOL, Kelly-optimal)
6. **Position management** — Monitors via WS price feed + RPC polling:
   - **Probe hold** (3s) — if price dumps > threshold → hard SL
   - **Trailing stop** — locks in profits as price rises
   - **TP tiers** — take partial profits at TP1/TP2/TP3
   - **Time SL** — exit if no movement within dead zone timeout
   - **Velocity exit** — exit on sharp momentum reversal
   - **Hard SL** — immediate exit on severe drawdown
7. **TX submission** — Sells via Jito gRPC bundle (NY primary) or Nozomi fast endpoint

---

## Exit Strategies

| Exit Type | Trigger | Avg Hold | Purpose |
|---|---|---|---|
| **trailing_stop** | Price retraces from peak by configured % | ~47s | Lock in winners (92% WR) |
| **hard_sl** | Probe dump threshold breached | ~1.6s | Cut rug/dump losses fast |
| **time_sl** | Dead zone timeout — no price movement | ~7s | Exit dead tokens |
| **velocity_exit** | Sharp negative momentum detected | ~5s | Exit momentum reversals |
| **tp1/tp2/tp3** | Price hits take-profit tiers | varies | Scale out of winners |

---

## Feed Architecture

| Feed | Transport | Latency | Role |
|---|---|---|---|
| **Jito ShredStream** | gRPC (local proxy :20100) | ~0ms | Fastest trade detection via shred decode |
| **PumpPortal** | WSS | ~120ms | Primary BC trade stream + token creation |
| **Helius** | WSS (logs + txSubscribe) | ~50ms | Graduation detection (PumpSwap direct path) |
| **CoreCast/Bitquery** | WSS (3 muxed streams) | ~80ms | Migrations, LP removal, creator sells |

**Self-healing:** HealthMonitor tracks per-feed staleness via atomics. Any feed >45s stale → auto-pause trading → auto-resume on recovery.

---

## Quick Start

```bash
# Build
cd rust && cargo build --release

# Configure
cp config/canary.json.example config/canary.json
# Set in rust/.env: HELIUS_API_KEY, SOLANA_RPC_URL, WALLET_KEYPAIR_PATH, etc.

# Start Jito ShredStream proxy
cd shredstream-proxy && ./target/release/jito-shredstream-proxy shredstream \
  --block-engine-url https://ny.mainnet.block-engine.jito.wtf \
  --auth-keypair ../config/keys/shredstream-keypair.json \
  --desired-regions ny --dest-ip-ports 127.0.0.1:20000 \
  --grpc-service-port 20100 &

# Run
cd .. && rust/target/release/pump-quant

# Health check
curl http://127.0.0.1:9421/api/health | jq

# View status + P&L
node scripts/rust-status.js
```

---

## Configuration

Key parameters in `config/canary.json` → `momentum` section:

| Parameter | Default | Purpose |
|---|---|---|
| `paper_mode` | `true` | No real trades — paper logging only |
| `enabled` | `true` | Master toggle for momentum engine |
| `min_grad_score` | `45` | Minimum graduation score for entry |
| `probe_size_sol` | `0.03` | Default probe position size |
| `kelly_sizing_enabled` | `true` | Use Kelly criterion for sizing |
| `kelly_fraction` | `0.25` | Quarter-Kelly for conservative sizing |
| `probe_hold_ms` | `3000` | Probe evaluation window |
| `probe_dump_threshold_bps` | `-600` | Hard SL during probe phase |
| `dead_zone_pumpswap_ws_zero_ms` | `8000` | Time SL for zero WS activity |
| `trailing_stop_accel_pct` | `25.0` | Trailing stop distance (accelerating) |
| `trailing_stop_decel_pct` | `8.0` | Trailing stop distance (decelerating) |
| `max_daily_entries` | `15` | Max trades per day |
| `session_max_loss_halt_sol` | `0.25` | Circuit breaker: halt on session loss |
| `tod_config.enabled` | `false` | Time-of-day gating (disabled for 24/7 data) |

---

## Project Structure

```
pump-quant/
├── rust/pump-quant-core/src/
│   ├── main.rs                    # Entry point — feeds → momentum engine
│   ├── lib.rs                     # Module declarations
│   │
│   ├── momentum/                  # ══ THE ENGINE (16K lines) ══
│   │   ├── mod.rs                 # Core engine loop (6114 lines)
│   │   ├── config.rs              # MomentumConfig (117 params)
│   │   ├── scorer.rs              # Graduation quality scorer (0-100)
│   │   ├── position.rs            # Position lifecycle + exit logic
│   │   ├── velocity.rs            # Price velocity/acceleration tracking
│   │   ├── pool.rs                # PumpSwap + Raydium pool resolution
│   │   ├── price_feed.rs          # WS + RPC price monitoring
│   │   ├── rpc_sender.rs          # TX submission (Jito + Nozomi)
│   │   ├── logger.rs              # JSONL trade logging
│   │   ├── tod.rs                 # Time-of-day gating
│   │   ├── types.rs               # ScoredToken, GradEnrichment
│   │   └── kelly.rs               # Kelly criterion sizing
│   │
│   ├── tx/                        # TX construction + submission
│   │   ├── pumpswap.rs            # PumpSwap swap instruction builder
│   │   ├── raydium.rs             # Raydium AMM swap builder
│   │   ├── jito_grpc.rs           # Persistent HTTP/2 Jito bundle client
│   │   ├── nozomi.rs              # Nozomi fast endpoint client
│   │   ├── tip_engine.rs          # Dynamic Jito tip sizing
│   │   ├── executor.rs            # Blockhash cache + TX orchestration
│   │   └── wallet.rs              # Keypair management
│   │
│   ├── feeds/                     # Data feed clients
│   │   ├── event_joiner.rs        # Sig-based dedup + crossbeam fan-in
│   │   ├── shredstream.rs         # Jito ShredStream gRPC client
│   │   ├── pumpportal.rs          # PumpPortal WSS client
│   │   ├── helius.rs              # Helius WSS (logs + txSubscribe)
│   │   ├── corecast.rs            # CoreCast/Bitquery WSS (3 streams)
│   │   └── social.rs              # Social signal aggregator (Phase 0)
│   │
│   ├── engine/                    # Shared infrastructure
│   │   ├── config.rs              # Config loader (canary.json)
│   │   └── health.rs              # Feed health monitor + auto-pause
│   │
│   ├── api/server.rs              # axum REST on :9421
│   ├── alerts/telegram.rs         # Rate-limited Telegram alerts
│   ├── persistence/               # JSONL + SQLite logging
│   └── rpc/                       # Solana RPC client + rate limiter
│
├── shredstream-proxy/             # Jito ShredStream gRPC proxy binary
├── config/
│   ├── canary.json                # Active runtime config
│   └── orphan_blocklist.json      # Permanently blocked mints (20+)
├── scripts/
│   ├── rust-status.js             # P&L report + heartbeat state
│   ├── watchdog.sh                # Process health + auto-restart
│   └── backup-db.sh               # Nightly DB backup
└── data/                          # Trade logs (gitignored)
    └── momentum_paper_trades.jsonl
```

---

## Risk Management

- **Kelly sizing** — Quarter-Kelly fraction with min/max bounds (0.02-0.10 SOL)
- **Session loss halt** — Auto-pause at configurable SOL loss per session
- **Circuit breaker** — 3 consecutive stop-losses → trading paused
- **Feed health** — Any feed stale >45s → auto-pause → auto-resume
- **Orphan recovery** — On startup, scans wallet for stuck tokens and emergency-sells
- **WSOL ATA pre-creation** — Ensures sell path works from first trade
- **Orphan blocklist** — Permanently blocks mints that failed to sell (20+ mints)

---

## Status

**Live trading active.** Momentum-only engine (backrunner code removed). All feeds connected with self-healing reconnect.

| Metric | Value |
|---|---|
| Codebase | ~29K lines Rust |
| Binary size | ~14MB (release) |
| Startup time | <1s |
| Feeds | 4 live (ShredStream, PumpPortal, Helius, CoreCast) |
| TX submission | Jito gRPC (NY + Frankfurt) + Nozomi (EWR1) |
| Wallet | `7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC` |
