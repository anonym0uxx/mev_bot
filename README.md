# pump-quant

A high-frequency trading bot for Solana bonding curve tokens. Regime-aware, risk-bounded, designed for autonomous operation in paper and live modes.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        DATA FEEDS                           │
│                                                             │
│   ┌──────────────────┐        ┌──────────────────┐         │
│   │  On-Chain Feed   │        │   Fast-Lane Feed  │         │
│   │  (confirmed txs) │        │  (pre-confirm,    │         │
│   │                  │        │   pending WL)     │         │
│   └────────┬─────────┘        └────────┬──────────┘         │
└────────────┼─────────────────────────┼───────────────────────┘
             │                         │
             ▼                         ▼
┌─────────────────────────────────────────────────────────────┐
│                      SIGNAL ENGINE                          │
│                                                             │
│   ┌───────────────────────────────────────────────────┐    │
│   │  Event Joiner — deduplicates across feeds         │    │
│   └───────────────────────┬───────────────────────────┘    │
│                           │                                  │
│   ┌───────────────────────▼───────────────────────────┐    │
│   │  Gate Stack                                        │    │
│   │  • Score threshold        • Buy momentum gates    │    │
│   │  • Volume gates           • Sell pressure gate    │    │
│   │  • vSol curve position    • Time-of-day filter    │    │
│   │  • Source allowlist       • Overheating guard     │    │
│   └───────────────────────┬───────────────────────────┘    │
└───────────────────────────┼──────────────────────────────────┘
                            │  SIGNAL (pass/reject)
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                    POSITION MANAGER                         │
│                                                             │
│   ┌──────────────┐   ┌──────────────┐   ┌───────────────┐  │
│   │ Entry Logic  │   │ Hold Monitor │   │  Exit Logic   │  │
│   │ size tiers   │   │ momentum     │   │  TP tiers     │  │
│   │ concurrency  │   │ decay check  │   │  stop loss    │  │
│   │ cap          │   │ (200ms)      │   │  next buyer   │  │
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
│   orders · positions · raw_events · config_versions        │
└─────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────┐
│                   MONITORING & CONTROL                      │
│                                                             │
│   Local HTTP API (:9420)  ·  Structured logs (Winston)     │
│   P&L scripts  ·  Health endpoint  ·  Paper trade logger   │
└─────────────────────────────────────────────────────────────┘
```

**Data flow summary:**
1. Feeds deliver on-chain trade events (fast-lane feed pending infrastructure approval)
2. Signal engine deduplicates, applies gate stack — most signals rejected here
3. Position manager tracks open trades, monitors holds, triggers exits
4. Execution layer routes to paper log or live transaction submission
5. All state persisted to SQLite; metrics available via local API

## Overview

- Subscribes to real-time on-chain events via WebSocket feeds
- Evaluates entry signals using multi-factor gating (momentum, volume, timing)
- Executes buys/sells with configurable position sizing and exit logic
- Tracks P&L, positions, and trade history in a local SQLite database
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
- `config/schema.json` — JSON Schema (all fields typed, no extra properties allowed)

Key parameters: position sizing, hold time limits, entry gates, time-of-day windows, exit tiers.

## Project Structure

```
src/
├── mev/          # Core trading engine — entry, exit, execution
├── feed/         # WebSocket data feeds
├── execution/    # Transaction building and submission
├── types/        # TypeScript interfaces and config types
└── utils/        # Logger, time, helpers
config/           # JSON configs and schema
data/             # SQLite database (gitignored)
scripts/          # Analysis and reporting scripts
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
