# Jito ShredStream Integration — Complete Build Plan

**Status:** READY TO EXECUTE on whitelist approval  
**Whitelist public key:** `2HegzSo8YujghD4jxwLjAri5XsQmUTCVwmVqoZjs21Wq`  
**Keypair file:** `config/keys/shredstream-keypair.json`  
**Application submitted:** 2026-03-28  
**DO NOT BUILD until Alon confirms whitelist approval**

---

## Expected Impact
- Detection: 150-250ms earlier than confirmed PumpPortal events
- WR improvement: +7.4pp blended (0-100ms bucket: 88.2% WR vs 301-600ms: 40.9%)
- ~18 additional winning trades/day in H13-H17 window

---

## Dependency Graph

```
PHASE 6 (Config/Types) ──┐
                          ├──→ PHASE 1 (Infrastructure) ──→ PHASE 2 (ShredStream Client)
                          ├──→ PHASE 3 (Mask Guards)                    │
                          │                                              │
                          └──→ PHASE 5 (Dynamic Tips)                   │
                                       │                                 │
                                       ▼                                 ▼
                               PHASE 4 (Dual-Source Integration) ◄──────┘
                                       │
                                       ▼
                               PHASE 7 (Metrics)
                                       │
                                       ▼
                               PHASE 8 (Testing/Rollout)
```

**Critical path:** Phase 6 → Phase 1 → Phase 2 → Phase 4 → Phase 7 → Phase 8  
**Parallel:** Phase 3 and Phase 5 can be built concurrently with Phases 1-2.  
**Total estimate:** ~18-20 engineering hours

---

## PHASE 1: ShredStream Proxy Setup (2-3h)

**New files:**
- `docker/docker-compose.shredstream.yml`
- `docker/shredstream-proxy.service`
- `scripts/shredstream-healthcheck.sh`

### docker/docker-compose.shredstream.yml

```yaml
version: "3.8"

services:
  shredstream-proxy:
    image: jitolabs/jito-shredstream-proxy:latest
    container_name: shredstream-proxy
    restart: unless-stopped
    network_mode: host
    volumes:
      - ../config/keys/shredstream-keypair.json:/app/auth-keypair.json:ro
    environment:
      BLOCK_ENGINE_URL: "https://mainnet.block-engine.jito.wtf"
      AUTH_KEYPAIR_PATH: "/app/auth-keypair.json"
      DESIRED_REGIONS: "ny"
      DEST_IP_PORTS: "127.0.0.1:20000"
      GRPC_SERVICE_PORT: "20100"
      RUST_LOG: "info"
      NUM_STREAMS: "1"
    logging:
      driver: json-file
      options:
        max-size: "50m"
        max-file: "3"
    healthcheck:
      test: ["CMD", "grpc_health_probe", "-addr=:20100"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 15s
```

### Standalone docker run (alternative)

```bash
docker run -d \
  --name shredstream-proxy \
  --network host \
  --restart unless-stopped \
  -v "$(pwd)/config/keys/shredstream-keypair.json:/app/auth-keypair.json:ro" \
  -e BLOCK_ENGINE_URL="https://mainnet.block-engine.jito.wtf" \
  -e AUTH_KEYPAIR_PATH="/app/auth-keypair.json" \
  -e DESIRED_REGIONS="ny" \
  -e DEST_IP_PORTS="127.0.0.1:20000" \
  -e GRPC_SERVICE_PORT="20100" \
  -e RUST_LOG="info" \
  -e NUM_STREAMS="1" \
  --log-opt max-size=50m \
  --log-opt max-file=3 \
  jitolabs/jito-shredstream-proxy:latest
```

### docker/shredstream-proxy.service (systemd)

```ini
[Unit]
Description=Jito ShredStream Proxy (Docker)
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target

[Service]
Type=simple
Restart=always
RestartSec=10
TimeoutStartSec=120
TimeoutStopSec=30
ExecStartPre=/usr/bin/docker pull jitolabs/jito-shredstream-proxy:latest
ExecStartPre=-/usr/bin/docker rm -f shredstream-proxy
ExecStart=/usr/bin/docker run \
  --name shredstream-proxy \
  --network host \
  -v /data/.openclaw/workspace/projects/pump-quant/config/keys/shredstream-keypair.json:/app/auth-keypair.json:ro \
  -e BLOCK_ENGINE_URL=https://mainnet.block-engine.jito.wtf \
  -e AUTH_KEYPAIR_PATH=/app/auth-keypair.json \
  -e DESIRED_REGIONS=ny \
  -e DEST_IP_PORTS=127.0.0.1:20000 \
  -e GRPC_SERVICE_PORT=20100 \
  -e RUST_LOG=info \
  -e NUM_STREAMS=1 \
  jitolabs/jito-shredstream-proxy:latest
ExecStop=/usr/bin/docker stop shredstream-proxy
ExecStopPost=-/usr/bin/docker rm -f shredstream-proxy

[Install]
WantedBy=multi-user.target
```

Install:
```bash
sudo cp docker/shredstream-proxy.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable shredstream-proxy
sudo systemctl start shredstream-proxy
```

### Firewall notes
- All ports are localhost-only (127.0.0.1) — no external firewall rules needed
- UDP 20000: shred delivery (local only)
- TCP 20100: gRPC service (local only)
- DO NOT open 20000 or 20100 to internet

### Verification
```bash
docker logs -f shredstream-proxy 2>&1 | head -50
# Look for: "authenticated", "connected to block engine", "streaming shreds"
# Bad: "unauthorized", "invalid keypair", "not whitelisted"

ss -tlnp | grep 20100   # gRPC port
ss -ulnp | grep 20000   # UDP shred delivery
```

---

## PHASE 2: ShredStream TypeScript Client (8-10h)

**New files:**
- `src/feeds/shredstream.ts`
- `src/feeds/shredstream-proto.ts`

**New dependencies:**
```bash
npm install @grpc/grpc-js @grpc/proto-loader
npm install -D @types/google-protobuf
```

### src/feeds/shredstream-proto.ts

```typescript
/**
 * Type definitions for Jito ShredStream gRPC SubscribeEntries.
 */

export interface ShredStreamEntry {
  slot: number;
  index: number;
  receivedAt: number;
  transactions: ShredTransaction[];
}

export interface ShredTransaction {
  signature: string;
  accountKeys: string[];
  instructions: DecodedInstruction[];
  isSuspectedPumpFunBuy: boolean;
  estimatedSolAmount?: number;
  tokenMint?: string;
  bondingCurveKey?: string;
}

export interface DecodedInstruction {
  programId: string;
  accounts: string[];
  data: Buffer;
}
```

### src/feeds/shredstream.ts (key structure)

```typescript
/**
 * ShredStream gRPC client — connects to local jito-shredstream-proxy,
 * decodes pre-confirmation entries into Pump.fun trade events.
 */

const PUMP_FUN_PROGRAM_ID = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';
const PUMP_BUY_DISCRIMINATOR = Buffer.from([102, 6, 61, 18, 1, 218, 235, 234]);

// Reconnection: 1s → 2s → 4s → ... → 60s max
const INITIAL_RECONNECT_MS = 1000;
const MAX_RECONNECT_MS = 60000;
const RECONNECT_MULTIPLIER = 2.00000;

// Dedup buffer (last 10k tx signatures — cross-ref with PumpPortal)
const DEDUP_BUFFER_SIZE = 10000;

export interface ShredStreamPumpTrade {
  signature: string;
  slot: number;
  tokenMint: string;
  bondingCurveKey: string;
  solAmount: number;
  isBuy: boolean;
  traderWallet: string;
  detectedAt: number;
  source: 'shredstream';
}

export class ShredStreamClient extends EventEmitter {
  // Events: 'pump-trade', 'connected', 'disconnected', 'error', 'latency-sample'
  
  connect(grpcEndpoint: string = '127.0.0.1:20100'): void
  disconnect(): void
  isConnected(): boolean
  getSeenSignatures(): Set<string>  // for PumpPortal dedup
}
```

**Key implementation notes:**
- Use `@grpc/grpc-js` for gRPC streaming (NOT `grpc` legacy package)
- Decode Solana transactions from raw bytes using `@solana/web3.js` `VersionedTransaction.deserialize()`
- Filter instructions by `programId === PUMP_FUN_PROGRAM_ID`
- Check first 8 bytes of instruction data against `PUMP_BUY_DISCRIMINATOR`
- Extract `tokenMint` from account keys (position 2 in Pump.fun buy accounts)
- Extract `bondingCurveKey` from account keys (position 3)
- Emit `'pump-trade'` event with `ShredStreamPumpTrade`
- Maintain dedup Set — add each signature on detection
- On PumpPortal receiving same sig: skip if already in dedup set

---

## PHASE 3: Mask Guards — Anti-Ban Layer (4-5h) ⚠️ CRITICAL

**New file:** `src/mev/jito-guard.ts`

Jito monitors for and may ban:
- Excessive bundle submission rate (spam)
- High bundle failure rates (bad bundles waste block space)
- Predictable tip patterns (fingerprinting)
- Single-wallet concentration
- Same-mint repeated entries
- Zero-profit bundles

### Required guards (implement all):

**1. Bundle rate limiter (token bucket)**
```typescript
// Max N bundles per 10-second window
// Default: jito_max_bundles_per_10s = 10
// Token bucket: refills at rate of max/10 tokens per second
class TokenBucket {
  capacity: number;
  tokens: number;
  refillRate: number; // tokens per ms
  lastRefill: number;
  
  consume(): boolean  // returns false if rate limited
}
```

**2. Tip randomization**
```typescript
// Add ±jito_tip_noise_pct random noise to computed tip
// Default: jito_tip_noise_pct = 0.20 (±20%)
// Prevents exact tip patterns from fingerprinting
const noiseFactor = 1 + (Math.random() * 2 - 1) * cfg.jito_tip_noise_pct;
const noisyTip = Math.floor(baseTip * noiseFactor);
const finalTip = clamp(noisyTip, TIP_MIN_LAMPORTS, TIP_MAX_LAMPORTS);
```

**3. Failure rate monitor**
```typescript
// Sliding window of last N bundle results
// If failure_count/window_size > jito_max_failure_rate → pause 60s
// Default: jito_failure_window_size = 20, jito_max_failure_rate = 0.40
class FailureRateMonitor {
  private window: boolean[] = []; // true = success, false = failure
  record(success: boolean): void
  isHealthy(): boolean
  pauseUntil: number  // timestamp, 0 = not paused
}
```

**4. Same-mint cooldown**
```typescript
// After entering mint X, block re-entry for jito_same_mint_cooldown_ms
// Default: 5000ms (5 seconds)
// Prevents wash-trading signals
private mintCooldowns: Map<string, number> = new Map();
isOnCooldown(mint: string): boolean
setCooldown(mint: string): void
```

**5. Submission spacing**
```typescript
// Min jito_min_submission_spacing_ms between ANY consecutive submissions
// Default: 100ms
// This is independent of per-trade jitter — applies globally
private lastSubmissionAt = 0;
canSubmit(): boolean
recordSubmission(): void
```

**6. Tip-to-size ratio cap**
```typescript
// Never submit if tip > max_tip_pct × position_size_sol × LAMPORTS_PER_SOL
// Default: max_tip_pct = 0.10 (tip can't exceed 10% of position value)
isTipSane(tipLamports: number, sizeSol: number): boolean
```

**7. gRPC error backoff**
```typescript
// On SearcherClientError: exponential backoff
// 1s → 2s → 4s → 8s → ... → 60s max
// Reset on successful submission
class ExponentialBackoff {
  private delay = INITIAL_RECONNECT_MS;
  wait(): Promise<void>
  reset(): void
}
```

**8. Bundle size discipline**
- Always exactly 2 txs: [buy_tx, tip_tx]
- Never submit 1 tx (naked tip = suspicious)
- Never submit 3+ (noisy, wasteful)

### JitoGuard class interface:
```typescript
export class JitoGuard {
  constructor(cfg: MevConfig)
  
  // Returns true if submission is allowed, false if blocked
  // Reason string explains why blocked (for logging)
  canSubmit(mint: string, sizeSol: number, tipLamports: number): { allowed: boolean; reason?: string }
  
  // Record outcome of a bundle submission
  recordOutcome(bundleId: string, success: boolean): void
  
  // Get current guard stats for metrics
  getStats(): JitoGuardStats
}

export interface JitoGuardStats {
  bundlesLast10s: number;
  failureRate: number;
  isPaused: boolean;
  pauseRemainingMs: number;
  mintsOnCooldown: number;
  totalRejections: number;
}
```

---

## PHASE 4: Dual-Source Detection in daemon/index.ts (3-4h)

**Modify:** `src/daemon/index.ts`

Changes:
1. Import `ShredStreamClient` from `../feeds/shredstream`
2. Import `JitoGuard` from `../mev/jito-guard`
3. Add `shredstream_enabled: boolean` config check
4. Initialize ShredStreamClient if enabled
5. Cross-reference dedup by tx signature

```typescript
// In daemon initialization:
let shredClient: ShredStreamClient | null = null;
if (cfg.mev.shredstream_enabled) {
  shredClient = new ShredStreamClient();
  shredClient.on('pump-trade', async (trade) => {
    // Convert ShredStreamPumpTrade → TokenTradeEvent
    // Deduplicate against PumpPortal events
    if (!seenSignatures.has(trade.signature)) {
      seenSignatures.add(trade.signature);
      await backrunEngine.handleTriggerEvent(convertShredToEvent(trade));
    }
  });
  shredClient.connect(cfg.mev.shredstream_grpc_endpoint ?? '127.0.0.1:20100');
}

// In PumpPortal handler:
// Skip if already processed by ShredStream
if (shredClient?.getSeenSignatures().has(event.signature)) {
  log.debug(`[dedup] ${event.signature.slice(0,8)} already processed via ShredStream`);
  return;
}
```

---

## PHASE 5: Dynamic Tip Sizing (2-3h)

**Modify:** `src/mev/jito-bundle-builder.ts`

Replace `computeTip()`:

```typescript
computeTip(sizeSol: number, triggerSol: number, noisePct: number): number {
  // Tiered base tip by trigger size
  let baseTip: number;
  if (triggerSol <= 0.60000) {
    baseTip = 50000;   // 0.00005 SOL
  } else if (triggerSol <= 1.50000) {
    baseTip = 80000;   // 0.00008 SOL
  } else {
    baseTip = 120000;  // 0.00012 SOL
  }
  
  // Profit-proportional floor
  const profitEstLamports = sizeSol * 0.02500 * LAMPORTS_PER_SOL; // assume 2.5% TP
  const profitFloor = Math.floor(profitEstLamports * 0.30000);
  baseTip = Math.max(baseTip, profitFloor);
  
  // Add noise to prevent fingerprinting
  const noise = baseTip * (Math.random() * 2 - 1) * noisePct;
  const noisyTip = Math.round(baseTip + noise);
  
  return Math.max(TIP_MIN_LAMPORTS, Math.min(TIP_MAX_LAMPORTS, noisyTip));
}
```

---

## PHASE 6: Config / Types / Schema

### New fields in src/types/config.ts (MevConfig interface):

```typescript
// ShredStream
shredstream_enabled?: boolean;           // default: false
shredstream_grpc_endpoint?: string;      // default: "127.0.0.1:20100"

// Jito guard
jito_max_bundles_per_10s?: number;       // default: 10
jito_min_submission_spacing_ms?: number; // default: 100
jito_max_failure_rate?: number;          // default: 0.40
jito_failure_window_size?: number;       // default: 20
jito_same_mint_cooldown_ms?: number;     // default: 5000
jito_tip_noise_pct?: number;             // default: 0.20
```

### New fields in config/canary.json (defaults):

```json
"shredstream_enabled": false,
"shredstream_grpc_endpoint": "127.0.0.1:20100",
"jito_max_bundles_per_10s": 10,
"jito_min_submission_spacing_ms": 100,
"jito_max_failure_rate": 0.40,
"jito_failure_window_size": 20,
"jito_same_mint_cooldown_ms": 5000,
"jito_tip_noise_pct": 0.20
```

### New fields in config/schema.json (add to mev object properties):

```json
"shredstream_enabled": { "type": "boolean" },
"shredstream_grpc_endpoint": { "type": "string" },
"jito_max_bundles_per_10s": { "type": "number" },
"jito_min_submission_spacing_ms": { "type": "number" },
"jito_max_failure_rate": { "type": "number" },
"jito_failure_window_size": { "type": "number" },
"jito_same_mint_cooldown_ms": { "type": "number" },
"jito_tip_noise_pct": { "type": "number" }
```

---

## PHASE 7: Metrics & Observability

Add to trade JSONL record and health endpoint:
- `shredstream_lead_ms` — time between ShredStream detection and PumpPortal confirmation
- `bundle_submission_rate` — bundles/min
- `bundle_success_rate` — % landed
- `bundle_landed_same_slot` — % in same slot as trigger
- `tip_paid_lamports` — per-bundle tip
- `jito_guard_rejections` — guard block count

---

## PHASE 8: Testing / Rollout Checklist

### Shadow mode first (no bundle submissions)
```bash
# Enable ShredStream detection but NOT Jito submissions
shredstream_enabled: true
jito_enabled: false   # still disabled
```
- Run for 1+ hour during H13-H17 window
- Verify `shredstream_lead_ms` averages > 100ms vs PumpPortal
- If lead < 50ms avg → proxy not working or whitelist not active yet

### Paper bundle mode
```bash
# Enable Jito in paper mode (logs what would be submitted, no real txs)
shredstream_enabled: true
jito_enabled: true
paper_mode: true   # existing flag
```
- Verify bundles log correctly with correct tip amounts
- Verify JitoGuard is firing and blocking as expected
- Check no rate limit violations in guard stats

### Dry-run live (minimal)
```bash
# Enable live Jito submissions at minimum tip, very low rate
jito_enabled: true
paper_mode: false
jito_tip_lamports: 10000       # minimum
jito_max_bundles_per_10s: 2    # very conservative
```
- Monitor Jito block engine dashboard for bundle landing rates
- Check for any ban/restriction signals in logs

### Full enable
```bash
jito_enabled: true
shredstream_enabled: true
jito_max_bundles_per_10s: 10   # normal rate
jito_tip_lamports: 50000       # normal tip
```

### Go/No-Go checklist before enabling shredstream_enabled: true
- [ ] Jito whitelist approval received
- [ ] shredstream-proxy Docker container running and healthy
- [ ] gRPC port 20100 responding
- [ ] Shadow mode tested: avg lead > 100ms
- [ ] Paper bundle mode tested: bundles log correctly
- [ ] JitoGuard tests passing
- [ ] npm run build: zero TypeScript errors
- [ ] Daemon health check: healthy
- [ ] Bot currently profitable in paper mode (validate before spending tips)

---

## Engineering Hours Summary

| Phase | Description | Hours |
|---|---|---|
| 1 | Infrastructure (Docker, systemd) | 2-3h |
| 2 | ShredStream TS client | 8-10h |
| 3 | Mask guards (jito-guard.ts) | 4-5h |
| 4 | Dual-source daemon integration | 3-4h |
| 5 | Dynamic tip sizing | 2-3h |
| 6 | Config/types/schema | 1-2h |
| 7 | Metrics | 1-2h |
| 8 | Testing/rollout | 2-3h |
| **Total** | | **23-32h** |

---

## Anti-Ban Summary

All guards implemented in JitoGuard:
1. Token bucket rate limiter (max 10 bundles/10s)
2. Tip randomization ±20% noise
3. Failure rate circuit breaker (>40% failures → 60s pause)
4. Same-mint cooldown (5s)
5. Minimum submission spacing (100ms between any bundles)
6. Tip-to-size ratio cap (tip ≤ 10% of position value)
7. gRPC exponential backoff on errors
8. Strict 2-tx bundle discipline (buy + tip only)

Existing anti-fingerprint already in place:
- WalletRotator: round-robin keypair rotation
- size_variance_pct: ±20% size jitter
- jitter_ms_min/max: 50-200ms entry delay
- Tip account rotation: random from 8 accounts
