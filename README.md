# pump-quant

High-frequency MEV backrun engine for Solana pump.fun bonding curve tokens. Fully rewritten in Rust. Regime-aware, risk-bounded, designed for autonomous paper and live operation.

> **Current status:** Rust daemon only. TypeScript engine retired. Paper mode active 24/7.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                           DATA FEEDS                                 │
│                                                                      │
│  ┌─────────────────┐  ┌──────────────────┐  ┌───────────────────┐  │
│  │  Helius          │  │  PumpPortal WS   │  │  Bitquery/CoreCast│  │
│  │  logsSubscribe   │  │  (pump.fun WS)   │  │  4 subscriptions  │  │
│  │  ~50ms latency   │  │  ~120ms latency  │  │  1 WS connection  │  │
│  │  PRIMARY TRIGGER │  │  state sync+dedup│  │  (4/5 streams)    │  │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬──────────┘  │
│           │                     │                      │             │
│           │ 50ms avg lead       │                      │             │
│           │ over PumpPortal     │  ┌───────────────────┤             │
│           │                     │  │ stream 1: DEX trades (creator   │
│           │                     │  │           sell detection)       │
│           │                     │  │ stream 2: Raydium AMM migration │
│           │                     │  │ stream 3: LP removal / rug      │
│           │                     │  │ stream 4: new token pre-warm    │
└───────────┼─────────────────────┼──┴─────────────────────────────────┘
            │                     │
            ▼                     ▼
┌──────────────────────────────────────────────────────────────────────┐
│                         HOT PATH (Rust)                              │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │  Event Router                                                  │  │
│  │  • Helius correlation (MintMap cache, 50ms avg pre-signal)     │  │
│  │  • PumpPortal dedup + state sync                               │  │
│  │  • CoreCast: creator_sell / migration / lp_removal / new_token │  │
│  │  • Monotonic clock (no syscall per event)                      │  │
│  │  • Stack-alloc bs58 decode (zero heap per event)               │  │
│  └──────────────────────────────┬─────────────────────────────────┘  │
│                                 │                                    │
│  ┌──────────────────────────────▼─────────────────────────────────┐  │
│  │  Gate Stack (8 gates, all must pass)                           │  │
│  │  • TriggerSize     ≥ trigger_min_buy_sol (0.50)                │  │
│  │  • VsolRange       curve position bounds                       │  │
│  │  • PreTriggerVol   5s volume ≥ pre_trigger_min_volume_5s       │  │
│  │  • BuyMomentum     buy ratio above threshold                   │  │
│  │  • SellPressure    sell ratio below threshold                  │  │
│  │  • VsolDelta       3s vSol delta gate                          │  │
│  │  • Concentration   adversarial wallet concentration (32-slot)  │  │
│  │  • TimeOfDay       blocked/boosted UTC hour windows            │  │
│  └──────────────────────────────┬─────────────────────────────────┘  │
│                                 │                                    │
│  ┌──────────────────────────────▼─────────────────────────────────┐  │
│  │  Scorer (multi-factor)                                         │  │
│  │  • Momentum score  • Volume score  • Curve position score      │  │
│  │  • ToD multiplier  • Concentration penalty                     │  │
│  │  • CoreCast signer match bonus                                 │  │
│  └──────────────────────────────┬─────────────────────────────────┘  │
└────────────────────────────────┼──────────────────────────────────────┘
                                 │  SIGNAL (pass/reject)
                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│                       POSITION MANAGER                               │
│                                                                      │
│  ┌──────────────┐    ┌─────────────────────┐    ┌───────────────┐   │
│  │ Entry        │    │ Hold Monitor        │    │ Exit Logic    │   │
│  │ tiered sizing│    │ momentum decay      │    │ TP tiers      │   │
│  │ ToD scaling  │    │ 50ms recurring tick │    │ stop loss     │   │
│  │ concurrency  │    │ peak-fade drawdown  │    │ next buyer    │   │
│  │ cap          │    │                     │    │ max hold      │   │
│  └──────┬───────┘    └──────────┬──────────┘    └───────┬───────┘   │
│         │                       │                        │           │
│  ┌──────▼───────────────────────▼────────────────────────▼───────┐  │
│  │  Safety Layer                                                  │  │
│  │  • Daily loss cap (paper: 5 SOL, live: 0.18 SOL)              │  │
│  │  • Consecutive SL circuit breaker (3 stops → 180s pause)      │  │
│  │  • min_hold_before_exit_ms = 500                               │  │
│  │  • creator_sell_ttl_ms = 30s (force-exit on creator sell)      │  │
│  │  • Migration force-exit (Raydium graduation)                   │  │
│  │  • LP removal force-exit (rug detection)                       │  │
│  └────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────┬─────────────────────────────────────┘
                                 │
                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│                        EXECUTION LAYER                               │
│                                                                      │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  Route Policy                                                │   │
│  │  PAPER_MODE=true  → paper logger only (no tx)               │   │
│  │  PAPER_MODE=false → build + submit tx                        │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                              │                                       │
│              ┌───────────────┴────────────────┐                     │
│              ▼                                 ▼                     │
│  ┌─────────────────────┐           ┌─────────────────────────┐      │
│  │  BlockhashCache     │           │  Jito Bundle Engine     │      │
│  │  30s TTL            │           │  atomic, tipped         │      │
│  │  25s bg refresh     │           │  disabled in paper mode │      │
│  └─────────────────────┘           └─────────────────────────┘      │
└────────────────────────────────┬─────────────────────────────────────┘
                                 │
                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│                         PERSISTENCE                                  │
│                                                                      │
│  SQLite (WAL)                  JSONL                                 │
│  raw_events                    data/mev_paper_trades.jsonl           │
│  token_state                   camelCase schema, TS-compatible       │
│  candidate_packets             fields: pnlSol, netPnlSol, feesSol,  │
│  trade_intents                 mfeSol, maeSol, exitReason, etc.      │
│  orders / positions                                                  │
│  health_events                 Engine State                          │
│  learning_ledger               data/engine-state.json               │
│  config_versions               (session boundary for heartbeat)      │
└────────────────────────────────┬─────────────────────────────────────┘
                                 │
                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│                      MONITORING & CONTROL                            │
│                                                                      │
│  HTTP API :9421                    Telegram Alerts                   │
│  /api/health  /api/stats           rate-limited, 4 formatters        │
│  /api/control/pause                position open/close/SL/TP        │
│  /api/control/resume               circuit breaker events            │
│                                                                      │
│  scripts/rust-status.js            Health Monitor                    │
│  heartbeat P&L report              HealthMonitor (AtomicU64)         │
│  feed staleness + latency          45s stale threshold               │
│  stream event counters             auto-pause on stale feed          │
│  high-water mark tracking                                            │
│                                                                      │
│  ensure-single-daemon.sh           PID file: data/pump-quant.pid    │
│  enforced at every startup path    kills TS + Rust duplicates        │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Feed Architecture

| Feed | Latency | Role |
|------|---------|------|
| Helius logsSubscribe | ~50ms | **Primary entry trigger** — 50ms ahead of PumpPortal |
| PumpPortal WebSocket | ~120ms | State sync, dedup, fallback trigger |
| Bitquery CoreCast (stream 1) | ~80ms | Creator sell detection (signer match) |
| Bitquery CoreCast (stream 2) | ~80ms | Raydium migration → force-exit |
| Bitquery CoreCast (stream 3) | ~80ms | LP removal / rug → force-exit |
| Bitquery CoreCast (stream 4) | ~80ms | New token pre-warm (creator_map) |

All 4 Bitquery subscriptions share **1 WebSocket connection** (graphql-ws multiplexed). Bitquery bills per subscription = **4/5 streams used**.

---

## Engine v6 — Rust Rewrite

The TypeScript daemon has been fully retired. The Rust binary is the sole runtime.

### What changed
- **Full Rust rewrite**: gates, scorer, positions, bonding_curve, feeds, hot_path, API, SQLite, JSONL — all in Rust
- **Helius as primary trigger**: fires 50ms before PumpPortal; PumpPortal used for state sync + dedup only
- **4 Bitquery streams**: AMM migration detection, LP removal/rug, new token pre-warm (previously TS used 5 gRPC streams = 5/5 cap; now 4 WS subscriptions = 4/5 cap)
- **Zero-alloc hot path**: monotonic clock, stack-alloc bs58 decode, no heap per event
- **BlockhashCache**: 30s TTL, 25s background refresh — zero hot-path latency for live tx
- **Safety layer hardened**: daily loss cap, consecutive SL circuit breaker, min hold, creator sell TTL
- **Feed health monitor**: HealthMonitor with AtomicU64 per-feed, auto-pause on 45s stale
- **Structured logging**: every event type has tracing fields (mint, creator, ts_ms, stream_id)
- **API on :9421** (TS was :9420 — no port conflict during migration)
- **ensure-single-daemon.sh**: enforced at every startup path — permanent duplicate prevention

### Metrics (paper mode, historical)
- 4,600+ paper trades recorded from TS engine
- Win rate: ~45% | Gross: +1.94 SOL | Net: -10.99 SOL (TS era, pre-Rust)
- Break-even WR: ~87% gross (fees are the main drag)
- Rust paper trades: accumulating 24/7 with TOD gate disabled

---

## Quick Start

### Rust daemon (production)

```bash
cd rust/
cp .env.example .env   # fill in keys
# Build
PATH=$HOME/.cargo/bin:$PATH \
  OPENSSL_DIR=/home/linuxbrew/.linuxbrew/opt/openssl@3 \
  PKG_CONFIG_PATH=/home/linuxbrew/.linuxbrew/opt/openssl@3/lib/pkgconfig \
  cargo build --release

# Start (paper mode, single-daemon enforced)
bash scripts/ensure-single-daemon.sh --start
```

### Status check
```bash
# Full heartbeat report
PAPER_MODE=true node scripts/rust-status.js

# Raw API
curl http://127.0.0.1:9421/api/stats | jq .
curl http://127.0.0.1:9421/api/health | jq .
```

### Live mode (when ready)
Edit `rust/.env` → set `PAPER_MODE=false` → restart.

Prerequisites before going live:
- [ ] 48h paper parity data collected
- [ ] Win rate ≥ 50% on 100+ Rust paper trades
- [ ] Circuit breaker verified in logs (3 SL → pause confirmed)
- [ ] Feed health auto-pause verified (stale feed → pause confirmed)

---

## Configuration

All config in `config/canary.json`. Key parameters:

| Parameter | Value | Notes |
|-----------|-------|-------|
| `trigger_min_buy_sol` | 0.50 | Entry trigger size gate |
| `pre_trigger_min_volume_5s` | 2.50 | 5s pre-trigger volume gate |
| `max_concurrent_positions` | — | **Do not auto-tune** |
| `daily_loss_cap_sol` | — | **Do not auto-tune** |
| `blocked_hours_utc` | `[]` | Cleared for 24/7 paper collection |
| `boosted_hours_utc` | `[]` | Cleared for 24/7 paper collection |

One config param change per auto-tune cycle. Never stack multiple changes.

---

## Project Structure

```
rust/
├── pump-quant-core/src/
│   ├── feeds/          # PumpPortal, Helius, CoreCast feeds
│   ├── engine/         # hot_path, health, engine_state
│   ├── gates/          # Gate stack (read-only)
│   ├── scorer/         # Scorer (read-only)
│   ├── positions/      # Position manager (read-only)
│   ├── bonding_curve/  # Bonding curve math (read-only)
│   ├── persistence/    # SQLite, JSONL paper logger
│   ├── tx/             # Transaction executor, BlockhashCache
│   ├── alerts/         # Telegram alerter
│   └── api/            # HTTP server :9421
src/                    # TypeScript source (retired — reference only)
config/
├── canary.json         # Active config
└── schema.json         # JSON Schema
data/                   # SQLite + JSONL (gitignored)
scripts/
├── rust-status.js      # Heartbeat status report
├── ensure-single-daemon.sh  # Duplicate prevention
├── run-rust-daemon.sh  # Supervisor loop
└── cutover-to-rust.sh  # Migration script (complete)
docs/
└── rust-final-build-plan.md
```

---

## Environment

Required vars in `rust/.env` (never committed):
- `HELIUS_API_KEY`
- `BITQUERY_API_KEY`
- `WALLET_PRIVATE_KEY`
- `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`
- `SOLANA_RPC_URL` + `SOLANA_WS_URL`
- `PAPER_MODE=true`

---

## Tech Stack

- **Rust** (tokio async runtime, crossbeam channels)
- **SQLite** via rusqlite (WAL mode)
- **solana-sdk** pinned to `=2.1.16`
- **tokio-tungstenite** for WebSocket feeds
- **axum** for HTTP API
- **tracing** for structured logging
- **serde_json** for JSONL + config
