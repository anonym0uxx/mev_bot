# pump-quant

A high-frequency MEV backrun bot for Solana bonding curve tokens. Regime-aware, risk-bounded, designed for autonomous operation in paper and live modes.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        DATA FEEDS                           │
│                                                             │
│   ┌──────────────────┐        ┌──────────────────┐         │
│   │  On-Chain Feed   │        │   Fast-Lane Feed  │         │
│   │  (confirmed txs) │        │  (pre-confirm,    │         │
│   │  primary trigger │        │   pending WL)     │         │
│   └────────┬─────────┘        └────────┬──────────┘         │
│                                                             │
│   ┌──────────────────────────────────────────────────┐     │
│   │  Enrichment Streams (gRPC, 5 concurrent)         │     │
│   │  bonding trades · AMM trades · transactions      │     │
│   │  transfers · bonding pool events                 │     │
│   └──────────────────┬───────────────────────────────┘     │
└────────────┬──────────┼──────────────────┼──────────────────┘
             │          │                  │
             ▼          ▼                  ▼
┌─────────────────────────────────────────────────────────────┐
│                      SIGNAL ENGINE                          │
│                                                             │
│   ┌───────────────────────────────────────────────────┐    │
│   │  Event Joiner — deduplicates across feeds         │    │
│   │  Source tagging (pumpportal / helius / enrichment)│    │
│   └───────────────────────┬───────────────────────────┘    │
│                           │                                  │
│   ┌───────────────────────▼───────────────────────────┐    │
│   │  Gate Stack                                        │    │
│   │  • Score threshold        • Buy momentum gates    │    │
│   │  • Volume gates (5s)      • Sell pressure gate    │    │
│   │  • vSol curve position    • Time-of-day filter    │    │
│   │  • Source allowlist       • vSol delta gate (3s)  │    │
│   │  • Creator sell guard     • Concurrency cap       │    │
│   └───────────────────────┬───────────────────────────┘    │
└───────────────────────────┼──────────────────────────────────┘
                            │  SIGNAL (pass/reject)
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                    POSITION MANAGER (v5)                    │
│                                                             │
│   ┌──────────────┐   ┌──────────────┐   ┌───────────────┐  │
│   │ Entry Logic  │   │ Hold Monitor │   │  Exit Logic   │  │
│   │ tiered sizing│   │ momentum     │   │  TP tiers     │  │
│   │ ToD scaling  │   │ decay (50ms  │   │  stop loss    │  │
│   │ concurrency  │   │ recurring)   │   │  next buyer   │  │
│   │ cap          │   │ peak-fade    │   │  intra-trail  │  │
│   └──────┬───────┘   └──────┬───────┘   └───────┬───────┘  │
└──────────┼─────────────────┼───────────────────┼────────────┘
           │                 │                   │
           ▼                 ▼                   ▼
┌─────────────────────────────────────────────────────────────┐
│                    EXECUTION LAYER                          │
│                                                             │
│   ┌──────────────────────────────────────────────────┐     │
│   │  Route Policy                                    │     │
│   │  paper mode → log only                          │     │
│   │  live mode  → build tx → submit                 │     │
│   └──────────────────┬───────────────────────────────┘     │
│                      │                                      │
│   ┌──────────────────▼───────────────────────────────┐     │
│   │  Transaction Builder                             │     │
│   │  buy tx / sell tx (VersionedTransaction)        │     │
│   └──────────────────┬───────────────────────────────┘     │
│                      │                                      │
│          ┌───────────┴────────────┐                        │
│          ▼                        ▼                         │
│   ┌─────────────┐        ┌──────────────────┐              │
│   │  Direct RPC │        │  Bundle Engine   │              │
│   │  submission │        │  (atomic, tipped)│              │
│   │             │        │  disabled in     │              │
│   │             │        │  paper mode      │              │
│   └─────────────┘        └──────────────────┘              │
└────────────────────────────────┬────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────┐
│                      PERSISTENCE                            │
│                                                             │
│   SQLite (WAL mode)                                        │
│   orders · raw_events · config_versions                    │
│   JSONL trade log (mev_paper_trades.jsonl)                 │
└─────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────┐
│                   MONITORING & CONTROL                      │
│                                                             │
│   Local HTTP API (:9420)  ·  Structured logs (Winston)     │
│   P&L scripts  ·  Health endpoint  ·  Paper trade logger   │
│   Quant analysis scripts  ·  Loss/trigger breakdowns       │
└─────────────────────────────────────────────────────────────┘
```

**Data flow summary:**
1. Primary feed delivers confirmed on-chain trade events; fast-lane feed (pending approval) delivers pre-confirmation
2. Enrichment streams provide supplementary market data (AMM, transfers, pool events) — used for signal enrichment, not as triggers
3. Signal engine deduplicates across feeds, stamps trigger source, applies gate stack — most signals rejected here
4. Position manager (v5) tracks open trades, runs recurring momentum decay checks every 50ms, triggers exits
5. Execution layer routes to paper log or live transaction submission
6. All state persisted to SQLite + JSONL; metrics available via local API

## Engine v5 — What's New

- **Momentum decay rewritten**: recurring 50ms interval (was broken one-shot), correct exit price, 0.3% peak-fade drawdown gate
- **Hold time tightened**: max_hold_ms reduced from 600ms → 400ms based on 4,600+ trade analysis
- **TOD optimised**: active window H13–H15 UTC (6–9 AM PDT), boost on H14+H15, H16/H17 blocked
- **Source tagging**: all feed events now tagged by source (pumpportal / helius / enrichment) for per-source P&L attribution
- **Gate stack hardened**: creator sell guard, vSol delta gate, source allowlist
- **Sandwich/scalper engines removed**: architecture is pure MEV backrun only

## Overview

- Subscribes to real-time on-chain events via WebSocket feeds
- Evaluates entry signals using multi-factor gating (momentum, volume, curve position, timing)
- Executes buys/sells with tiered position sizing and exit logic
- Tracks P&L, positions, and trade history in SQLite + JSONL
- Exposes a local HTTP API for monitoring and control

## Quick Start

```bash
cp .env.example .env
# Fill in your keys — see .env.example for required variables
npm install
npm run build
```

**Paper mode (recommended first):**
```bash
PAPER_MODE=true npm start
```

**Live mode:**
```bash
CONFIG_PATH=config/canary.json npm start
```

## Configuration

All config is in `config/` as JSON with strict schema validation:

- `config/canary.json` — Active production config
- `config/schema.json` — JSON Schema (all fields typed, `additionalProperties: false`)

Key parameters: position sizing tiers, hold time limit, entry gates, time-of-day windows, TP/SL tiers, momentum decay thresholds.

## Project Structure

```
src/
├── mev/          # Core trading engine — detector, position manager, backrun engine
├── feed/         # WebSocket + gRPC data feeds
├── execution/    # Transaction building and bundle submission
├── daemon/       # Main process, health API, supervisor loop
├── types/        # TypeScript interfaces and config types
└── utils/        # Logger, time, helpers
config/           # JSON configs and schema
data/             # SQLite database + JSONL trade log (gitignored)
scripts/          # P&L reporting and quant analysis scripts
docs/             # Architecture and integration specs
```

## Environment

Required environment variables are documented in `.env.example`. Never commit `.env` or any files under `config/keys/`.

## Tech Stack

- TypeScript (strict) on Node.js
- SQLite via better-sqlite3 (WAL mode)
- @solana/web3.js
- Express for local HTTP API
- Winston for structured logging
- ajv for config schema validation

## Notes

- `config/keys/` is gitignored — keypairs must be provisioned separately on each deployment
- Paper mode is fully functional with no live transaction submission
- All config changes are validated against schema before applying
- Engine is designed for a narrow daily trading window; the daemon runs 24/7 but only places trades during the configured active hours
