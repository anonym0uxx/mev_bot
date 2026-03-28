# Jito ShredStream Integration — Complete Implementation Spec

**Version:** 1.0.0  
**Date:** 2026-03-28  
**Status:** PENDING WHITELIST APPROVAL  
**Author:** MEV Engineering (automated spec generation)

> **CRITICAL:** Do NOT execute any part of this spec until the Jito ShredStream whitelist is confirmed for pubkey `2HegzSo8YujghD4jxwLjAri5XsQmUTCVwmVqoZjs21Wq`. See Go/No-Go Checklist §6.

---

## Table of Contents

1. [Backtest Validation](#1-backtest-validation)
2. [Infrastructure Files](#2-infrastructure-files)
3. [TypeScript Implementation — New Files](#3-typescript-implementation--new-files)
4. [Modifications to Existing Files](#4-modifications-to-existing-files)
5. [Metrics Additions](#5-metrics-additions)
6. [Go/No-Go Checklist](#6-gono-go-checklist)
7. [Dependency Install Commands](#7-dependency-install-commands)

---

## 1. Backtest Validation

### 1.1 Raw Data Summary

| Hold bucket | Trades | WR | Net SOL | Avg PnL/trade |
|---|---|---|---|---|
| 0–100ms | 450 | 76.0% | +0.81107 | +0.00180 |
| 101–300ms | 370 | 63.2% | −1.11646 | −0.00302 |
| 301–600ms | 721 | 70.0% | −1.40751 | −0.00195 |
| 600+ms | 3,085 | 32.5% | −9.27643 | −0.00301 |
| **Total** | **4,626** | **45.0%** | **−10.98933** | **−0.00238** |

### 1.2 ShredStream Latency Model

**Premise:** ShredStream delivers shred-level transaction data 150–250ms before PumpPortal's WebSocket. PumpPortal waits for block confirmation + enrichment (account state lookup for vSol/vTokens). ShredStream delivers raw transactions from shreds before the block is even confirmed.

**Key distinction:** Hold-time buckets above measure time from entry to exit, NOT detection latency. ShredStream improves *detection latency* — the time from on-chain transaction to bot awareness. This translates to:

1. **Earlier entry on the bonding curve** (lower vSol → better price)
2. **More runway within the 600ms max_hold window** for subsequent buyers to arrive
3. **Reduced max_hold exits** — the dominant loss mechanism

### 1.3 EV Model — Entry Price Improvement

During active Pump.fun momentum in the 33–43 vSol range (our target window), the bonding curve moves as new buys land. A 200ms timing advantage means entering before ~0.5–2% of price movement occurs.

**Conservative estimate: 0.75% average entry price improvement**

With weighted average position size ~0.10000 SOL:
- Per-trade improvement: 0.10000 × 0.00750 = **+0.00075 SOL**
- Across 4,626 trades: 4,626 × 0.00075 = **+3.46950 SOL**

**Discount for jitter (50–200ms consumes part of advantage) and variance:** 50%
- Realistic improvement: **+1.73475 SOL** across dataset
- Per-trade: **+0.00038 SOL**

### 1.4 EV Model — Max Hold Rescue

Max_hold exits: 1,518 trades (32.8%), dominant loss driver. These are trades where the bot entered too late and couldn't exit profitably within 600ms.

With 200ms earlier detection, some max_hold exits become next_buyer/take_profit exits:

**At 10% rescue rate (conservative):**
- Rescued: 1,518 × 0.10 = ~152 trades
- Current max_hold avg PnL: −0.00301 SOL/trade
- Projected next_buyer avg PnL: +0.00100 SOL/trade
- Delta per trade: +0.00401 SOL
- Improvement: 152 × 0.00401 = **+0.60952 SOL**

**At 20% rescue rate (moderate):**
- 304 trades × 0.00401 = **+1.21904 SOL**

### 1.5 Tip Cost Analysis

ShredStream is a **detection feed**, not a submission mechanism. The bot already pays Jito tips on every bundle. ShredStream adds zero incremental per-trade cost.

- ShredStream proxy: Docker container, zero on-chain cost
- ShredStream auth: free (whitelisted keypair)
- Additional tip cost from increased trade volume (~10% more detections): 462 × 0.00005 = **0.02310 SOL** (negligible)

### 1.6 Breakeven Analysis

| Model | Daily (200 trades) | Monthly | Cost/day | Net/day |
|---|---|---|---|---|
| Entry price (conservative) | +0.07600 SOL | +2.28000 SOL | 0.00000 SOL | **+0.07600 SOL** |
| Max hold rescue (10%) | +0.02631 SOL | +0.78930 SOL | 0.00000 SOL | **+0.02631 SOL** |
| Combined (realistic) | +0.05000 SOL | +1.50000 SOL | 0.00000 SOL | **+0.05000 SOL** |

**Breakeven trades needed per day:** 0 (zero marginal cost)

### 1.7 GO/NO-GO Recommendation

### ✅ CONDITIONAL GO

**Rationale:**
1. Zero marginal cost — any positive improvement is net positive
2. Conservative model: +0.05000 SOL/day (+1.50000 SOL/month)
3. Shadow mode validates before capital risk
4. Structural speed advantage compounds with other improvements
5. Implementation cost is bounded (2-3 files, well-defined integration points)

**Caveat:** The 301–600ms bucket paradox (70% WR but negative PnL) indicates that WR alone doesn't predict profitability — loss magnitude matters. The actual ShredStream benefit depends on real-world `shredstream_lead_ms` measurements. If avg lead < 100ms, the case weakens.

**Required validation in shadow mode:**
- Measure actual `shredstream_lead_ms` — abort if avg < 100ms
- Compare WR for ShredStream-sourced vs PumpPortal-sourced trades
- Track max_hold rate separately for ShredStream entries

---

## 2. Infrastructure Files

### 2.1 File: `docker/docker-compose.shredstream.yml`

```yaml
# Jito ShredStream Proxy — local gRPC relay for shred-level transaction data
# Exposes gRPC on 127.0.0.1:20100 for pump-quant MEV engine consumption
#
# Usage:
#   docker compose -f docker/docker-compose.shredstream.yml up -d
#
# Requires:
#   - Jito whitelist approval for auth keypair
#   - config/keys/shredstream-keypair.json present

version: "3.8"

services:
  shredstream-proxy:
    image: jitolabs/shredstream-proxy:latest
    container_name: shredstream-proxy
    restart: unless-stopped
    network_mode: host
    volumes:
      - /data/.openclaw/workspace/projects/pump-quant/config/keys/shredstream-keypair.json:/app/keypair.json:ro
    environment:
      BLOCK_ENGINE_URL: "https://amsterdam.mainnet.block-engine.jito.wtf"
      AUTH_KEYPAIR: "/app/keypair.json"
      GRPC_LISTEN_ADDR: "127.0.0.1:20100"
      DESIRED_REGIONS: "amsterdam"
      RUST_LOG: "info"
    healthcheck:
      test: ["CMD-SHELL", "ss -tlnp | grep -q 20100 || exit 1"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 15s
    logging:
      driver: json-file
      options:
        max-size: "50m"
        max-file: "3"
    deploy:
      resources:
        limits:
          memory: 512M
          cpus: "0.5"
```

### 2.2 File: `docker/shredstream-proxy.service`

```ini
[Unit]
Description=Jito ShredStream Proxy (Docker)
Documentation=https://jito-labs.gitbook.io/mev/searcher-resources/shredstream
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
WorkingDirectory=/data/.openclaw/workspace/projects/pump-quant
ExecStartPre=/usr/bin/docker compose -f /data/.openclaw/workspace/projects/pump-quant/docker/docker-compose.shredstream.yml pull --quiet
ExecStart=/usr/bin/docker compose -f /data/.openclaw/workspace/projects/pump-quant/docker/docker-compose.shredstream.yml up -d
ExecStop=/usr/bin/docker compose -f /data/.openclaw/workspace/projects/pump-quant/docker/docker-compose.shredstream.yml down
ExecReload=/usr/bin/docker compose -f /data/.openclaw/workspace/projects/pump-quant/docker/docker-compose.shredstream.yml restart
Restart=on-failure
RestartSec=30
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

### 2.3 File: `scripts/shredstream-healthcheck.sh`

```bash
#!/usr/bin/env bash
# shredstream-healthcheck.sh — Verify ShredStream proxy health
# Exit codes: 0 = healthy, 1 = unhealthy
# Usage: ./scripts/shredstream-healthcheck.sh [--verbose]

set -euo pipefail

VERBOSE=false
[[ "${1:-}" == "--verbose" ]] && VERBOSE=true

ERRORS=0

log() { [[ "$VERBOSE" == true ]] && echo "[$(date '+%H:%M:%S')] $*"; }
fail() { echo "❌ FAIL: $*" >&2; ERRORS=$((ERRORS + 1)); }
pass() { log "✅ PASS: $*"; }

# 1. Docker container running
if docker ps --format '{{.Names}}' | grep -q '^shredstream-proxy$'; then
  pass "Container running"
else
  fail "Container not running"
fi

# 2. Port 20100 listening
if ss -tlnp 2>/dev/null | grep -q ':20100'; then
  pass "Port 20100 listening"
else
  fail "Port 20100 not listening"
fi

# 3. Authenticated with block engine
AUTH_COUNT=$(docker logs shredstream-proxy 2>&1 | grep -ci "authenticated" || true)
if [[ "$AUTH_COUNT" -gt 0 ]]; then
  pass "Authenticated (${AUTH_COUNT} messages)"
else
  fail "No authentication messages in logs"
fi

# 4. No recent error spikes
ERROR_COUNT=$(docker logs --tail 100 shredstream-proxy 2>&1 | grep -ci "error\|panic\|fatal" || true)
if [[ "$ERROR_COUNT" -lt 5 ]]; then
  pass "Error count: ${ERROR_COUNT} (< 5)"
else
  fail "High error count: ${ERROR_COUNT}"
fi

# 5. Uptime > 60s (not crash-looping)
CONTAINER_STATUS=$(docker inspect --format='{{.State.Status}}' shredstream-proxy 2>/dev/null || echo "missing")
if [[ "$CONTAINER_STATUS" == "running" ]]; then
  STARTED_AT=$(docker inspect --format='{{.State.StartedAt}}' shredstream-proxy 2>/dev/null)
  if [[ -n "$STARTED_AT" ]]; then
    STARTED_EPOCH=$(date -d "$STARTED_AT" +%s 2>/dev/null || echo 0)
    NOW_EPOCH=$(date +%s)
    UPTIME=$((NOW_EPOCH - STARTED_EPOCH))
    if [[ "$UPTIME" -gt 60 ]]; then
      pass "Uptime: ${UPTIME}s"
    else
      fail "Uptime too low: ${UPTIME}s (crash-looping?)"
    fi
  fi
else
  fail "Container status: ${CONTAINER_STATUS}"
fi

# 6. No ban signals
BAN_COUNT=$(docker logs --tail 200 shredstream-proxy 2>&1 | grep -ci "unauthorized\|rate.limit\|banned\|rejected" || true)
if [[ "$BAN_COUNT" -eq 0 ]]; then
  pass "No ban signals"
else
  fail "Ban signals detected: ${BAN_COUNT}"
fi

# Summary
if [[ "$ERRORS" -eq 0 ]]; then
  echo "✅ ShredStream proxy: HEALTHY"
  exit 0
else
  echo "❌ ShredStream proxy: UNHEALTHY (${ERRORS} failed)"
  exit 1
fi
```

---

## 3. TypeScript Implementation — New Files

### 3.1 File: `src/feeds/shredstream.ts`

```typescript
/**
 * @module feeds/shredstream
 * ShredStreamClient: consumes Jito ShredStream gRPC proxy for sub-slot
 * transaction detection on Pump.fun bonding curve buys.
 *
 * Architecture:
 *   shredstream-proxy (Docker, port 20100) → gRPC stream → this client
 *   → Deserializes raw Solana transactions from shred entries
 *   → Filters for Pump.fun buy instructions
 *   → Emits 'pump-trade' events ~150-250ms before PumpPortal
 *
 * Events:
 *   'pump-trade'       — ShredStreamPumpTrade detected
 *   'connected'        — gRPC stream established
 *   'disconnected'     — gRPC stream lost
 *   'error'            — non-fatal error
 *   'latency-sample'   — { detectedAtMs: number } for lead time tracking
 */

import { EventEmitter } from 'events';
import * as grpc from '@grpc/grpc-js';
import { VersionedTransaction, PublicKey } from '@solana/web3.js';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { MevConfig } from '../types/config';

const log = createLogger('feeds:shredstream');

// Pump.fun program ID
const PUMP_FUN_PROGRAM = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';

// Pump.fun buy instruction discriminator (first 8 bytes of instruction data)
const BUY_DISCRIMINATOR = Buffer.from([102, 6, 61, 18, 1, 218, 235, 234]);

// Account key positions in buy instruction accounts array
const ACCOUNT_INDEX_TOKEN_MINT = 2;
const ACCOUNT_INDEX_BONDING_CURVE = 3;
const ACCOUNT_INDEX_TRADER_WALLET = 6;

// bs58 for signature encoding
// eslint-disable-next-line @typescript-eslint/no-var-requires
const _bs58 = require('bs58');
const bs58 = _bs58.default ?? _bs58;

/** Pump.fun buy trade detected from ShredStream */
export interface ShredStreamPumpTrade {
  signature: string;
  mint: string;
  bondingCurveKey: string;
  traderWallet: string;
  detectedAtMs: number;
  slot: number;
}

/**
 * Minimal protobuf decoder for ShredStream Entry messages.
 *
 * Proto schema:
 *   message Entry {
 *     uint64 slot = 1;
 *     repeated bytes transactions = 2;
 *   }
 */
function decodeEntry(buf: Buffer): { slot: number; transactions: Buffer[] } {
  let offset = 0;
  let slot = 0;
  const transactions: Buffer[] = [];

  while (offset < buf.length) {
    if (offset >= buf.length) break;
    const tag = buf[offset++];
    const fieldNumber = tag >> 3;
    const wireType = tag & 0x07;

    if (fieldNumber === 1 && wireType === 0) {
      // Varint: slot
      let value = 0;
      let shift = 0;
      while (offset < buf.length) {
        const byte = buf[offset++];
        value |= (byte & 0x7f) << shift;
        if ((byte & 0x80) === 0) break;
        shift += 7;
      }
      slot = value;
    } else if (fieldNumber === 2 && wireType === 2) {
      // Length-delimited: transaction bytes
      let length = 0;
      let shift = 0;
      while (offset < buf.length) {
        const byte = buf[offset++];
        length |= (byte & 0x7f) << shift;
        if ((byte & 0x80) === 0) break;
        shift += 7;
      }
      if (offset + length <= buf.length) {
        transactions.push(buf.subarray(offset, offset + length));
        offset += length;
      } else {
        break;
      }
    } else {
      // Skip unknown fields
      if (wireType === 0) {
        while (offset < buf.length && (buf[offset] & 0x80) !== 0) offset++;
        if (offset < buf.length) offset++;
      } else if (wireType === 2) {
        let length = 0;
        let shift = 0;
        while (offset < buf.length) {
          const byte = buf[offset++];
          length |= (byte & 0x7f) << shift;
          if ((byte & 0x80) === 0) break;
          shift += 7;
        }
        offset += length;
      } else if (wireType === 5) {
        offset += 4;
      } else if (wireType === 1) {
        offset += 8;
      } else {
        break;
      }
    }
  }

  return { slot, transactions };
}

export class ShredStreamClient extends EventEmitter {
  private cfg: MevConfig;
  private endpoint: string;
  private client: grpc.Client | null = null;
  private stream: grpc.ClientReadableStream<Buffer> | null = null;
  private running = false;

  // Dedup: seen signatures for cross-feed coordination
  private seenSignatures: Set<string> = new Set();
  private seenOrder: string[] = [];
  private readonly SEEN_MAX = 10_000;

  // Exponential backoff reconnect
  private reconnectTimer: NodeJS.Timeout | null = null;
  private reconnectDelayMs: number;
  private readonly RECONNECT_BASE_MS = 1_000;
  private readonly RECONNECT_MULTIPLIER = 2;
  private readonly RECONNECT_MAX_MS = 60_000;

  // Stats
  private entriesReceived = 0;
  private txProcessed = 0;
  private pumpTradesDetected = 0;
  private parseErrors = 0;
  private lastEntryAtMs = 0;
  private connectedSince = 0;

  constructor(cfg: MevConfig) {
    super();
    this.cfg = cfg;
    this.endpoint = cfg.shredstream_grpc_endpoint ?? '127.0.0.1:20100';
    this.reconnectDelayMs = this.RECONNECT_BASE_MS;
  }

  /** Returns the set of signatures already seen by ShredStream for cross-feed dedup */
  getSeenSignatures(): ReadonlySet<string> {
    return this.seenSignatures;
  }

  /** Returns runtime stats */
  getStats(): {
    entriesReceived: number;
    txProcessed: number;
    pumpTradesDetected: number;
    parseErrors: number;
    lastEntryAtMs: number;
    connectedSince: number;
    seenSignaturesSize: number;
  } {
    return {
      entriesReceived: this.entriesReceived,
      txProcessed: this.txProcessed,
      pumpTradesDetected: this.pumpTradesDetected,
      parseErrors: this.parseErrors,
      lastEntryAtMs: this.lastEntryAtMs,
      connectedSince: this.connectedSince,
      seenSignaturesSize: this.seenSignatures.size,
    };
  }

  /** Start the gRPC stream connection */
  start(): void {
    if (this.running) {
      log.warn('ShredStreamClient already running');
      return;
    }
    this.running = true;
    log.info(`ShredStreamClient starting — endpoint=${this.endpoint}`);
    this.connect();
  }

  /** Stop the client and clean up */
  stop(): void {
    if (!this.running) return;
    this.running = false;
    log.info('ShredStreamClient stopping...');

    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    this.destroyStream();

    if (this.client) {
      this.client.close();
      this.client = null;
    }

    log.info(
      `ShredStreamClient stopped — entries=${this.entriesReceived} tx=${this.txProcessed} ` +
      `pumpTrades=${this.pumpTradesDetected} parseErrors=${this.parseErrors}`
    );
  }

  private connect(): void {
    if (!this.running) return;

    try {
      // Create gRPC client with insecure credentials (localhost proxy)
      this.client = new grpc.Client(
        this.endpoint,
        grpc.credentials.createInsecure(),
        {
          'grpc.keepalive_time_ms': 10_000,
          'grpc.keepalive_timeout_ms': 5_000,
          'grpc.keepalive_permit_without_calls': 1,
          'grpc.max_receive_message_length': 64 * 1024 * 1024, // 64MB
        }
      );

      // Make a server-streaming call to SubscribeEntries
      // Using the generic makeServerStreamRequest method
      this.stream = this.client.makeServerStreamRequest<Record<string, unknown>, Buffer>(
        '/shredstream.ShredStream/SubscribeEntries',
        (value: Record<string, unknown>) => {
          // Encode empty request as protobuf (empty message = 0 bytes)
          return Buffer.alloc(0);
        },
        (buffer: Buffer) => buffer, // Return raw buffer for manual protobuf decoding
        {},                         // Empty request
        new grpc.Metadata(),
        { deadline: undefined },     // No deadline for streaming
      );

      this.stream.on('data', (chunk: Buffer) => {
        this.onEntry(chunk);
      });

      this.stream.on('error', (err: Error & { code?: number }) => {
        const code = err.code ?? -1;
        log.warn(`ShredStream gRPC error: code=${code} msg=${err.message}`);
        this.emit('error', err);
        this.handleDisconnect();
      });

      this.stream.on('end', () => {
        log.info('ShredStream gRPC stream ended');
        this.handleDisconnect();
      });

      this.stream.on('status', (status: grpc.StatusObject) => {
        if (status.code !== grpc.status.OK) {
          log.warn(`ShredStream gRPC status: code=${status.code} details=${status.details}`);
        }
      });

      // Mark connected
      this.connectedSince = nowMs();
      this.reconnectDelayMs = this.RECONNECT_BASE_MS; // Reset backoff on successful connect
      this.emit('connected');
      log.info(`ShredStream connected to ${this.endpoint}`);

    } catch (err) {
      log.error(`ShredStream connect failed: ${(err as Error).message}`);
      this.emit('error', err);
      this.scheduleReconnect();
    }
  }

  private destroyStream(): void {
    if (this.stream) {
      try {
        this.stream.cancel();
      } catch {
        // ignore cancel errors
      }
      this.stream.removeAllListeners();
      this.stream = null;
    }
  }

  private handleDisconnect(): void {
    this.destroyStream();
    this.connectedSince = 0;
    this.emit('disconnected');

    if (this.client) {
      try { this.client.close(); } catch { /* ignore */ }
      this.client = null;
    }

    if (this.running) {
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect(): void {
    if (!this.running) return;
    if (this.reconnectTimer) return;

    log.info(`ShredStream reconnecting in ${this.reconnectDelayMs}ms...`);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, this.reconnectDelayMs);
    this.reconnectTimer.unref();

    // Exponential backoff
    this.reconnectDelayMs = Math.min(
      this.reconnectDelayMs * this.RECONNECT_MULTIPLIER,
      this.RECONNECT_MAX_MS
    );
  }

  /**
   * Process a raw Entry protobuf message from the gRPC stream.
   * Decodes transactions, filters for Pump.fun buys, emits events.
   */
  private onEntry(raw: Buffer): void {
    this.entriesReceived++;
    this.lastEntryAtMs = nowMs();

    let entry: { slot: number; transactions: Buffer[] };
    try {
      entry = decodeEntry(raw);
    } catch (err) {
      this.parseErrors++;
      if (this.parseErrors % 100 === 0) {
        log.warn(`ShredStream entry decode errors: ${this.parseErrors}`);
      }
      return;
    }

    for (const txBytes of entry.transactions) {
      this.txProcessed++;
      this.processTx(txBytes, entry.slot);
    }
  }

  /**
   * Deserialize a raw transaction and check if it's a Pump.fun buy.
   */
  private processTx(txBytes: Buffer, slot: number): void {
    let tx: VersionedTransaction;
    try {
      tx = VersionedTransaction.deserialize(txBytes);
    } catch {
      // Not all shred transactions are valid/complete — this is expected
      return;
    }

    // Extract signature (first 64 bytes of the signatures array)
    if (tx.signatures.length === 0) return;
    const sigBytes = tx.signatures[0];
    const signature = bs58.encode(Buffer.from(sigBytes));

    // Dedup check
    if (this.seenSignatures.has(signature)) return;

    // Get all account keys from the message
    const accountKeys = tx.message.staticAccountKeys;
    if (!accountKeys || accountKeys.length === 0) return;

    // Check if any instruction invokes the Pump.fun program
    const instructions = tx.message.compiledInstructions;
    if (!instructions || instructions.length === 0) return;

    for (const ix of instructions) {
      const programIdx = ix.programIdIndex;
      if (programIdx >= accountKeys.length) continue;

      const programId = accountKeys[programIdx].toBase58();
      if (programId !== PUMP_FUN_PROGRAM) continue;

      // Check buy discriminator (first 8 bytes of instruction data)
      if (!ix.data || ix.data.length < 8) continue;
      const discriminator = Buffer.from(ix.data.subarray(0, 8));
      if (!discriminator.equals(BUY_DISCRIMINATOR)) continue;

      // This is a Pump.fun buy instruction — extract account keys
      const ixAccountIndices = ix.accountKeyIndexes;
      if (!ixAccountIndices || ixAccountIndices.length < 7) continue;

      const mintIdx = ixAccountIndices[ACCOUNT_INDEX_TOKEN_MINT];
      const bondingIdx = ixAccountIndices[ACCOUNT_INDEX_BONDING_CURVE];
      const traderIdx = ixAccountIndices[ACCOUNT_INDEX_TRADER_WALLET];

      if (mintIdx >= accountKeys.length || bondingIdx >= accountKeys.length || traderIdx >= accountKeys.length) {
        continue;
      }

      const mint = accountKeys[mintIdx].toBase58();
      const bondingCurveKey = accountKeys[bondingIdx].toBase58();
      const traderWallet = accountKeys[traderIdx].toBase58();

      // Record signature in dedup set
      this.addToSeenSignatures(signature);

      const trade: ShredStreamPumpTrade = {
        signature,
        mint,
        bondingCurveKey,
        traderWallet,
        detectedAtMs: nowMs(),
        slot,
      };

      this.pumpTradesDetected++;
      this.emit('pump-trade', trade);
      this.emit('latency-sample', { detectedAtMs: trade.detectedAtMs });

      if (this.pumpTradesDetected % 100 === 0) {
        log.info(
          `ShredStream stats: entries=${this.entriesReceived} tx=${this.txProcessed} ` +
          `pumpBuys=${this.pumpTradesDetected} errors=${this.parseErrors}`
        );
      }

      // Only process the first Pump.fun buy per transaction
      break;
    }
  }

  private addToSeenSignatures(sig: string): void {
    this.seenSignatures.add(sig);
    this.seenOrder.push(sig);

    // Evict oldest if over capacity
    while (this.seenOrder.length > this.SEEN_MAX) {
      const old = this.seenOrder.shift();
      if (old) this.seenSignatures.delete(old);
    }
  }
}
```

### 3.2 File: `src/mev/jito-guard.ts`

```typescript
/**
 * @module mev/jito-guard
 * JitoGuard: 8-layer safety gate for Jito bundle submissions.
 *
 * All guards are evaluated synchronously in canSubmit(). Any rejection
 * prevents submission with a logged reason. Guards:
 *
 *   1. Token bucket rate limiter (max bundles per 10s window)
 *   2. Tip randomization (noise to avoid fingerprinting)
 *   3. Failure rate circuit breaker (sliding window)
 *   4. Same-mint cooldown (prevent rapid re-entry)
 *   5. Submission spacing (min gap between any bundles)
 *   6. Tip-to-size ratio cap (prevent overpaying tips)
 *   7. gRPC backoff (exponential backoff tracker)
 *   8