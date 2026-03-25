# Pump Quant Bot

**Pump.fun Principal Crypto Quant Bot** — A regime-aware, friction-aware, risk-bounded autonomous trading system for Pump.fun bonding curve tokens on Solana.

## Architecture

```
┌─────────────────────────────────────────────────┐
│                  OpenClaw Plugin                 │
│         14 tools for AI assistant control        │
└────────────────────┬────────────────────────────┘
                     │ HTTP API (localhost:9420)
┌────────────────────▼────────────────────────────┐
│              Strategy Daemon                      │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ PumpPortal│ │ Feature  │ │ State Machine    │ │
│  │ WebSocket │ │ Engine   │ │ OBSERVE→WATCH→   │ │
│  │ Feed      │ │ 6 families│ │ ENTER_READY→LONG │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ Regime   │ │Entry/Exit│ │ Manipulation     │ │
│  │Classifier│ │ Engines  │ │ Model            │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ Friction │ │Execution │ │ Health Monitor   │ │
│  │ Model    │ │ Adapter  │ │ + Alerts         │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
│  ┌──────────────────────────────────────────────┐│
│  │ Learning: Ledger + Calibration + Champion    ││
│  └──────────────────────────────────────────────┘│
└────────────────────┬────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────┐
│            SQLite Persistence (WAL mode)          │
│  raw_events · token_state · feature_snapshots     │
│  candidate_packets · orders · positions           │
│  learning_ledger · config_versions                │
└──────────────────────────────────────────────────┘
```

## Quick Start

### 1. Setup
```bash
cd pump-quant
cp .env.example .env
# Edit .env with your keys (WALLET_PRIVATE_KEY, PUMP_PORTAL_API_KEY, etc.)
npm install
npm run build
```

### 2. Paper Trading (Recommended First)
```bash
PAPER_MODE=true npm start
```

### 3. Live Trading (Canary)
```bash
CONFIG_PATH=config/canary.json npm start
```

### 4. Replay
```bash
npm run start:replay -- 2024-01-01T00:00:00Z 2024-01-02T00:00:00Z
```

## Configuration

All configuration is externalized in `config/` as JSON with schema validation:

- `config/default.json` — Standard config
- `config/canary.json` — Conservative canary mode (recommended for first live run)
- `config/schema.json` — JSON Schema for validation

## Operator Commands

Available via WhatsApp/chat:

| Command | Description |
|---------|-------------|
| `status` | Bot status, positions, PnL |
| `health` | System health check |
| `positions` | Open positions detail |
| `top` | Top candidates |
| `inspect <mint>` | Deep inspection |
| `pause` | Pause trading |
| `resume` | Resume trading |
| `pnl` | PnL summary |
| `risk` | Risk settings |
| `set quick_spend <sol>` | Set default spend |
| `set risk_per_trade <pct>` | Set risk per trade |
| `set max_alloc <pct>` | Set max allocation |
| `set slippage_cap <sol>` | Set slippage cap |
| `profile` | Current profile |
| `set profile <name>` | Switch profile |

## Plugin Tools

The OpenClaw plugin exposes 14 tools:

1. `get_top_candidates()` — Top-ranked candidates
2. `inspect_candidate(mint)` — Detailed token inspection
3. `buy_token(mint, size_sol, ...)` — Execute buy
4. `sell_token(mint, amount_pct, ...)` — Execute sell
5. `get_positions()` — Open positions
6. `pause_trading(reason)` — Pause bot
7. `resume_trading()` — Resume bot
8. `get_bot_health()` — System health
9. `get_risk_settings()` — Risk config
10. `update_risk_settings(settings)` — Update risk
11. `get_strategy_profile()` — Current profile
12. `set_strategy_profile(name)` — Switch profile
13. `get_runtime_config()` — Full config
14. `update_runtime_config(patch)` — Patch config

## Core Principles

- **Net liquidation value everywhere** — never raw price
- **Fail closed to NO_TRADE** — never fail open
- **Regime-aware** — EARLY_CURVE, MID_CURVE, LATE_CURVE, GRADUATION_BOUNDARY
- **EV-driven decisions** — enter only when EV_enter_now > 0 AND EntryEdge > 0
- **All costs modeled** — Pump fee, PumpPortal fee, Solana fee, slippage, priority fee
- **Replayable** — every decision reproducible from raw data + config version
- **Risk-bounded** — position sizing with 5 caps, daily loss limit, max positions
- **Private key safety** — never in code, logs, or chat

## Tech Stack

- TypeScript (strict mode) on Node.js
- SQLite via better-sqlite3 (WAL mode)
- PumpPortal WebSocket for live data
- Bitquery for enrichment queries
- @solana/web3.js for Solana transactions
- ajv for config schema validation
- Express for daemon HTTP API
- Winston for structured logging
