# pump-quant

> Three-engine Solana MEV bot — backrunner + graduation arb + post-graduation momentum  
> Built in Rust for AMD EPYC Zen 4 | Paper mode default | Helius + CoreCast + PumpPortal feeds

---

## Architecture

```
┌────────────────────────────────────────────────────────────────────────────────┐
│                          pump-quant  (single Rust binary)                      │
│                                                                                │
│  DATA FEEDS (WebSocket)           EVENT ROUTING            TRADING ENGINES     │
│  ─────────────────────            ─────────────            ───────────────     │
│                                                                                │
│  ┌──────────────────┐             ┌─────────────┐         ┌────────────────┐  │
│  │  PumpPortal WS   │──trade──►   │             │─────►   │ BackrunEngine  │  │
│  │  BC buy/sell      │             │   HotPath   │         │ momentum backrun│  │
│  │  events           │             │             │         │ 4-tier TP/SL   │  │
│  └──────────────────┘             │  on_trade() │         │ 1500ms max hold│  │
│                                    │  gate stack │         └───────┬────────┘  │
│  ┌──────────────────┐             │  scorer     │                 │            │
│  │  Helius WS       │──grad──►    └─────────────┘                 │            │
│  │  logsSubscribe   │                                             │            │
│  │  (~50ms lead)    │             ┌─────────────┐         ┌───────┴────────┐  │
│  └──────────────────┘      ┌──►   │  GradArb    │         │                │  │
│                             │     │  Engine      │         │  Position      │  │
│  ┌──────────────────┐      │     │  spread≥3%   │         │  Manager       │  │
│  │  CoreCast WS     │──────┤     │  Raydium only│         │  (per-engine)  │  │
│  │  3 subscriptions │      │     └──────────────┘         │                │  │
│  │  • DEX trades    │      │                              │  150ms tick    │  │
│  │  • AMM migration │      │     ┌──────────────┐         │  loop          │  │
│  │  • LP removal    │      └──►  │  Momentum    │         │                │  │
│  └──────────────────┘             │  Engine      │         └───────┬────────┘  │
│                                   │  score≥40    │                 │            │
│              ┌─────────────┐      │  T+0 entry   │                 │            │
│              │ GradFilter  │──►   └──────────────┘                 │            │
│              │ should_emit │                                       │            │
│              │ • startup   │                                       │            │
│              │   guard     │                                       │            │
│              │ • WSOL      │                                       │            │
│              │   reject    │                                       │            │
│              │ • ring buf  │                                       │            │
│              │   dedup     │                                       │            │
│              └─────────────┘                                       │            │
│                                                                    ▼            │
│  SHARED INFRA                                              ┌──────────────┐    │
│  ─────────────                                             │   OUTPUT     │    │
│  • RingBuffer dedup (64 slots, L1 resident)                │              │    │
│  • PriceFeedManager (Helius accountSubscribe)              │  JSONL logs  │    │
│  • AtomicU64 price storage (lock-free)                     │  SQLite WAL  │    │
│  • HealthMonitor (per-feed staleness)                      │  REST :9421  │    │
│  • BlockhashCache (30s TTL, 25s refresh)                   │  Telegram    │    │
│                                                             └──────────────┘    │
└────────────────────────────────────────────────────────────────────────────────┘
```

---

## From Binary Start to First Trade Exit

The binary loads `config/canary.json` and spins up three independent trading engines on a single tokio runtime. Three WebSocket feeds connect immediately: PumpPortal for raw bonding curve trades, Helius for Solana log subscriptions (graduation detection), and CoreCast for AMM trades and migration confirmations. Each feed runs as a spawned tokio task, funneling events through crossbeam channels into the main engine loop.

When a pump.fun trade arrives from PumpPortal, it hits `hot_path.on_trade()` — the backrun engine's synchronous, zero-alloc critical path. Eight gates fire in sequence: trigger size, vSol range (30–52 SOL), pre-trigger buy count (≥7 in the last second), buy momentum ratio, sell pressure, vSol delta, wallet concentration, and time-of-day (UTC 13–21). If all pass, the scorer computes a composite signal. A passing score opens a backrun position, which the 150ms tick loop monitors for 4-tier take-profit, stop-loss, momentum decay, or the 1500ms max hold timeout.

When Helius detects a graduation (Raydium `initialize2` CPI in a pump.fun transaction), the `GraduationFilter` deduplicates it through a 64-slot ring buffer and rejects WSOL noise. Two tokio tasks then spawn in parallel. The first runs `GraduationArbEngine.on_migration()` — it resolves the Raydium pool via `getTransaction` → `postTokenBalances` vault extraction → `getMultipleAccountsInfo`, computes the spread against the BC terminal price (~4.11e-4 lamports/atom), and enters only if spread ≥ 3%. PumpSwap graduations are skipped (no structural arb at 1.8%). The second task runs `MomentumEngine.on_migration()` — it scores the graduation across four dimensions (speed, volume, pre-grad buy velocity, price recovery) and enters immediately at pool opening price if the score hits 40/100. Both engines feed their tick loops, and both log exits to their respective JSONL files.

The REST API on port 9421 exposes health, stats, and control endpoints. A heartbeat script checks health and pushes P&L summaries to Telegram every 5 minutes.

---

## Engines

| Engine | Trigger | Entry Signal | Exit Strategy | JSONL |
|---|---|---|---|---|
| **BackrunEngine** | BC trade event (PumpPortal) | `preTriggerBuys1s ≥ 7`, vSol 30–52, UTC 13–21 | 4-tier TP / SL / momentum decay / 1500ms max hold | `backrun_paper_trades.jsonl` |
| **GradArbEngine** | Graduation (Helius logs) | Spread ≥ 3% vs BC terminal price, Raydium only | Spread close / timeout / 5000ms max hold | `graduation_paper_trades.jsonl` |
| **MomentumEngine** | Graduation (Helius logs) | Score ≥ 40/100, T+0 immediate entry | 3-tier TP (5%/15%/50%) / 8% trailing / 12% SL / 60s time SL / 300s max hold | `momentum_paper_trades.jsonl` |

### Momentum Scoring Breakdown

| Factor | Weight | What It Measures |
|---|---|---|
| Speed | 0–25 | How fast the graduation completed |
| Volume | 0–25 | Total SOL volume during bonding curve phase |
| Pre-grad buy velocity | 0–25 | Buy pressure in final seconds before graduation |
| Price recovery | 0–25 | Post-dip recovery signal near curve completion |

Minimum composite score: **40/100** to enter.

---

## Quick Start

```bash
# Build
cd rust && cargo build --release

# Configure
cp config/canary.json.example config/canary.json
# Edit: set HELIUS_API_KEY, RPC_URL, BITQUERY_API_KEY, etc.

# Run (single-daemon enforced)
bash scripts/ensure-single-daemon.sh --start

# Check health
curl http://127.0.0.1:9421/api/health | jq

# View P&L
PAPER_MODE=true node scripts/pnl-summary.js
```

---

## Configuration

Key parameters in `config/canary.json`:

| Parameter | Default | Purpose |
|---|---|---|
| `paper_mode` | `true` | No live trades — paper logging only |
| `pre_trigger_min_buys_1s` | `7` | Minimum buy events in last 1s for backrun entry |
| `min_vsol` / `max_vsol` | `30` / `52` | Bonding curve position range gate |
| `graduation_arb_enabled` | `true` | Enable graduation arb engine |
| `graduation_arb.min_spread_pct` | `3.0` | Minimum spread to enter arb |
| `momentum.enabled` | `true` | Enable momentum engine |
| `momentum.entry_delay_ms` | `0` | Immediate entry (paper data collection) |
| `momentum.min_score` | `40` | Minimum graduation score to enter |
| `momentum.paper_mode` | `true` | Momentum-specific paper mode |

---

## Machine-Level Optimizations

This binary is tuned for low-latency event processing on AMD EPYC Zen 4:

- **Ring buffer dedup** — 64-slot circular buffer replacing DashMap for graduation deduplication. Fits entirely in L1 cache. O(1) insert, O(n) scan with n=64.
- **Cache-line aligned positions** — `#[repr(C, align(64))]` on `MomentumPosition` (256 bytes exact). No false sharing between tick thread and entry path.
- **Hot/cold path annotations** — `#[inline(always)]` on `on_trade()`, `on_tick()`, gate checks. `#[cold] #[inline(never)]` on error paths, logging, and JSONL flush.
- **Lock-free price reads** — `AtomicU64` for price storage. Tick thread reads prices with `Ordering::Relaxed` — no locks, no contention with the Helius feed writer.
- **SIMD-ready byte comparisons** — Graduation detection uses fixed-size byte arrays for mint comparison, enabling auto-vectorization.
- **Fixed-point arithmetic** — Price calculations on the hot path use integer lamports/atoms to avoid f64 precision loss and FPU stalls.
- **Zero-alloc hot path** — Stack-allocated bs58 decode, monotonic clock (no syscall per event), no heap allocation in the backrun critical path.

---

## Project Structure

```
pump-quant/
├── rust/pump-quant-core/src/
│   ├── arb/              # graduation.rs — GraduationArbEngine
│   │                     # dedup.rs — ring buffer dedup (64 slots)
│   │                     # pool_resolver.rs — Raydium pool resolution
│   ├── engine/           # hot_path.rs — BackrunEngine critical path
│   │                     # gates.rs — 8-gate entry filter stack
│   │                     # scorer.rs — multi-factor signal scorer
│   │                     # positions.rs — position lifecycle
│   │                     # config.rs — runtime config loader
│   │                     # health.rs — feed staleness monitor
│   ├── feeds/            # helius.rs — logsSubscribe + accountSubscribe
│   │                     # corecast.rs — 3 Bitquery WS subscriptions
│   │                     # pumpportal.rs — pump.fun trade stream
│   │                     # event_joiner.rs — crossbeam fan-in
│   ├── momentum/         # mod.rs — MomentumEngine orchestration
│   │                     # position.rs — 256-byte aligned position struct
│   │                     # scorer.rs — 4-factor graduation scorer
│   │                     # price_feed.rs — Helius vault accountSubscribe
│   │                     # logger.rs — JSONL paper trade logger
│   │                     # config.rs — momentum-specific config
│   ├── persistence/      # JSONL loggers, SQLite WAL
│   ├── api/              # axum REST server on :9421
│   ├── alerts/           # Telegram alerter (rate-limited)
│   ├── tx/               # Transaction builder, BlockhashCache
│   └── core/             # Shared types, bonding curve math
├── config/
│   ├── canary.json       # Active runtime config
│   └── canary.json.example
├── scripts/
│   ├── ensure-single-daemon.sh   # PID-enforced single instance
│   ├── pnl-summary.js            # P&L report (all engines)
│   └── analyze-losses.js         # Loss pattern analysis
├── data/                          # JSONL + SQLite (gitignored)
│   ├── backrun_paper_trades.jsonl
│   ├── graduation_paper_trades.jsonl
│   └── momentum_paper_trades.jsonl
└── docs/
    ├── ARCHITECTURE_V2.md
    └── MOMENTUM_ENGINE_SPEC.md
```

---

## Environment

Required in `rust/.env` (never committed):

```
HELIUS_API_KEY=...
BITQUERY_API_KEY=...
WALLET_PRIVATE_KEY=...
TELEGRAM_BOT_TOKEN=...
TELEGRAM_CHAT_ID=...
SOLANA_RPC_URL=...
SOLANA_WS_URL=...
PAPER_MODE=true
```

---

## Tech Stack

| Component | Crate / Tool |
|---|---|
| Async runtime | `tokio` (multi-thread) |
| WebSocket feeds | `tokio-tungstenite` |
| HTTP API | `axum` |
| Database | `rusqlite` (SQLite, WAL mode) |
| Solana SDK | `solana-sdk =2.1.16` |
| Serialization | `serde` + `serde_json` |
| Logging | `tracing` (structured) |
| Channel | `crossbeam-channel` |

---

## Feed Architecture

| Feed | Transport | Latency | Role |
|---|---|---|---|
| Helius `logsSubscribe` | WebSocket | ~50ms | Primary graduation trigger, ~50ms ahead of PumpPortal |
| PumpPortal | WebSocket | ~120ms | Bonding curve trade stream (backrun trigger), state sync |
| CoreCast (stream 1) | WebSocket | ~80ms | Creator sell detection → force-exit |
| CoreCast (stream 2) | WebSocket | ~80ms | Raydium AMM migration confirmation |
| CoreCast (stream 3) | WebSocket | ~80ms | LP removal / rug → force-exit |
| Helius `accountSubscribe` | WebSocket | ~50ms | Real-time vault balance for momentum price feed |

---

## Status

**Paper mode active 24/7.** All three engines are collecting data. The momentum engine is in data-collection phase to build a post-graduation price trajectory dataset before any live trading decisions.

**Not yet live.** Prerequisites:
- [ ] 200+ momentum paper trades with trajectory data
- [ ] Backrun win rate ≥ 50% on 500+ Rust paper trades
- [ ] Circuit breaker verified (3 consecutive SL → 180s pause)
- [ ] Feed health auto-pause verified (45s stale → engine pause)
