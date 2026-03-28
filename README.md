# pump-quant

A high-frequency trading bot for Solana bonding curve tokens. Regime-aware, risk-bounded, designed for autonomous operation in paper and live modes.

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
