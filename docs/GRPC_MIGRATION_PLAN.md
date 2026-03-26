# CoreCast v3 gRPC Migration Plan
**Date:** 2026-03-25  
**Status:** PLANNING  
**Estimated effort:** 6-8 hours

---

## Overview

Replace `src/feed/corecast-v2.ts` (HTTP polling, 1-2s latency, ~86k calls/day) with a Bitquery CoreCast gRPC streaming client (sub-100ms latency, 0 polling calls).

---

## Stream Allocation (5 slots available)

| Slot | Topic | Filter | Purpose |
|------|-------|--------|---------|
| 1 | `dex_trades` | program=`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | Bonding curve trades (primary) |
| 2 | `transactions` | program=`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | New token detection + creator address |
| 3 | `dex_trades` | program=`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | Post-graduation AMM trades |
| 4 | Reserve | — | Future: Raydium migration events |
| 5 | Reserve | — | Future: Secondary DEX coverage |

---

## Phase 1: Dependencies & Scaffold (1 hour)

### Install packages
```bash
npm install @grpc/grpc-js bitquery-corecast-proto
```

### New files to create
- `src/feed/corecast-v3.ts` — main gRPC client
- `src/feed/creator-cache.ts` — LRU cache + GraphQL API
- `src/feed/event-joiner.ts` — pool creation ↔ first trade join

### Files to modify
- `src/types/config.ts` — add CoreCastV3Config interface
- `src/types/events.ts` — add CreatorHistory type
- `src/daemon/index.ts` — swap corecast-v2 → corecast-v3
- `config/canary.json` — add grpc config block
- `config/schema.json` — add grpc schema fields
- `src/persistence/database.ts` — add creator_history table

---

## Phase 2: CoreCast v3 gRPC Client (2-3 hours)

### `src/feed/corecast-v3.ts` — Architecture

```typescript
export class CoreCastV3Client extends EventEmitter {
  private streams: Map<string, grpc.ClientReadableStream<any>> = new Map();
  private reconnectTimers: Map<string, NodeJS.Timeout> = new Map();
  private reconnectAttempts: Map<string, number> = new Map();
  private dedupeSet: Set<string> = new Set(); // signature dedup, max 100k
  private messageCount = 0;
  private startTime = 0;
  private _connected = false;

  // gRPC client options (from Bitquery best practices)
  private static readonly GRPC_OPTIONS = {
    'grpc.keepalive_time_ms': 30000,
    'grpc.keepalive_timeout_ms': 5000,
    'grpc.keepalive_permit_without_calls': 1,
    'grpc.max_receive_message_length': 4 * 1024 * 1024,
    'grpc.max_send_message_length': 4 * 1024 * 1024,
    'grpc.enable_retries': 1,
    'grpc.max_connection_idle_ms': 30000,
  };

  async connect(): Promise<void>
  disconnect(): void
  private startStream(name: string, type: string, filter: object): void
  private reconnectStream(name: string, type: string, filter: object, attempt: number): void
  private handleTradeMessage(msg: any): void     // → emit tokenTrade
  private handleTxMessage(msg: any): void        // → emit newToken (pool creation)
  private handleAmmTradeMessage(msg: any): void  // → post-grad AMM trades
  private isDupe(signature: string): boolean
  private pruneDedupe(): void  // keep bounded at 100k
  get connected(): boolean
  get stats(): { messageCount, uptimeMs, lastMessageAt }
}
```

### Reconnect logic (exponential backoff)
```typescript
private reconnectStream(name, type, filter, attempt) {
  const maxAttempts = 10;
  if (attempt >= maxAttempts) {
    this.emit('disconnected', `${name}: max reconnect attempts`);
    return;
  }
  const delay = Math.min(1000 * Math.pow(2, attempt), 60000) + Math.random() * 1000;
  const timer = setTimeout(() => {
    this.startStream(name, type, filter);
    this.reconnectAttempts.set(name, attempt + 1);
  }, delay);
  this.reconnectTimers.set(name, timer);
}
```

### Trade message mapping (gRPC → TokenTradeEvent)
```typescript
private handleTradeMessage(msg: any): void {
  const trade = msg.Trade;
  if (!trade) return;

  const mint = trade.Market?.QuoteCurrency?.MintAddress;
  if (!mint) return;

  const isBuy = trade.Buy?.Amount > 0;
  const signature = msg.Transaction?.Signature;

  if (this.isDupe(signature)) return;

  const event: TokenTradeEvent = {
    mint,
    txType: isBuy ? 'buy' : 'sell',
    traderPublicKey: isBuy
      ? trade.Buy?.Account?.Address
      : trade.Sell?.Account?.Address,
    tokenAmount: isBuy ? trade.Buy?.Amount : trade.Sell?.Amount,
    solAmount: isBuy
      ? (trade.Sell?.Amount || 0) / 1e9
      : (trade.Buy?.Amount || 0) / 1e9,
    newTokenBalance: 0,  // not available in stream, track locally
    bondingCurveProgress: 0,  // compute from reserves if available
    signature,
    slotNumber: msg.Block?.Slot || 0,
    timestamp: Date.now(),
  };

  this.emit('tokenTrade', event);
  this.messageCount++;
}
```

### Transaction message mapping (pool creation → NewTokenEvent)
```typescript
private handleTxMessage(msg: any): void {
  const instructions = msg.ParsedIdlInstructions || [];
  const createInstr = instructions.find(i =>
    i.Program?.Method === 'create' &&
    i.Program?.Address === PUMP_FUN_PROGRAM
  );
  if (!createInstr) return;

  const creator = msg.Transaction?.Header?.Signers?.[0];
  const args = createInstr.Arguments || [];
  const get = (name: string) => args.find(a => a.Name === name)?.Value || '';

  const event: NewTokenEvent = {
    mint: get('mint') || createInstr.Accounts?.[0]?.Address,
    name: get('name'),
    symbol: get('symbol'),
    uri: get('uri'),
    creator: creator || '',
    created_at: Date.now(),
    initial_virtual_token_reserves: 1_073_000_000,
    initial_virtual_sol_reserves: 30_000_000_000,
  };

  if (event.mint && event.creator) {
    this.emit('newToken', event);
  }
}
```

---

## Phase 3: Creator Cache (1-2 hours)

### `src/feed/creator-cache.ts`

```typescript
export class CreatorCache {
  private cache: Map<string, CreatorHistory> = new Map();
  private lookupCount = 0;
  private lookupResetAt = Date.now();
  private readonly MAX_ENTRIES = 10_000;
  private readonly DAILY_BUDGET = 2_100;

  get(creator: string): CreatorHistory | null
  set(creator: string, history: CreatorHistory): void
  shouldLookup(creator: string): boolean  // rate limit check
  async fetchFromApi(creator: string): Promise<CreatorHistory | null>
  private evictLRU(): void
  private getRemainingBudget(): number  // resets daily
}
```

**Rate limiting:** Track calls with rolling 24h window. At 87/hour max, only look up creators that:
1. Are not already cached
2. Budget allows
3. Token passed initial manipulation filters (don't waste budget on obvious rugs)

### GraphQL query for creator history
```graphql
query CreatorHistory($creator: String!) {
  Solana {
    Instructions(
      where: {
        Instruction: {
          Program: { Address: { is: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" } }
          Name: { is: "create" }
        }
        Transaction: { Signer: { is: $creator } }
      }
    ) {
      count
      Transaction { Signature }
    }
  }
}
```

---

## Phase 4: Config Schema Changes

### New `corecast_v3` config block in canary.json
```json
"corecast_v3": {
  "enabled": false,
  "endpoint": "corecast.bitquery.io",
  "api_key_env": "BITQUERY_API_KEY",
  "streams": {
    "bonding_curve_trades": true,
    "transactions": true,
    "amm_trades": true
  },
  "creator_cache": {
    "daily_budget": 2100,
    "max_entries": 10000,
    "lookup_after_min_trades": 5
  },
  "reconnect": {
    "max_attempts": 10,
    "initial_delay_ms": 1000,
    "max_delay_ms": 60000
  }
}
```

**Rollback flag:** Set `corecast_v3.enabled = false` → daemon uses corecast-v2 polling. Zero code change needed.

---

## Phase 5: Quant Strategy Updates for Sub-100ms Data

### Entry parameter recalibration
```json
"entry": {
  "observation_window_s": 2,
  "min_trades_for_analysis": 8,
  "min_entry_edge": 0.0008,
  "min_unique_buyers": 5,
  "max_bonding_progress_entry": 0.10
}
```

**Rationale:**
- With sub-100ms data, 2s window has much more data than 3s polling window
- 8 trades (down from 15) because we see every trade now, not batches
- Enter at 0-10% bonding ideally (tighten from 0-15%)
- Need velocity acceleration signal: 3+ trades in last 500ms

### New signals enabled by streaming
1. **Intra-second burst:** ≥3 buys in 500ms window → ignition signal
2. **Wallet sequence:** Are buys coming from unique wallets or same wallet repeating? (bot detection)
3. **Momentum slope:** Is buy velocity increasing or decreasing? Enter on acceleration, not peak

### Exit recalibration
```json
"exit": {
  "raw_stop_pct": 0.35,
  "take_profit_pct": 1.50,
  "max_hold_time_s": 180,
  "doa_check_s": 10,
  "doa_min_loss_pct": 0.05,
  "retrace_threshold_pct": 0.30
}
```

**Rationale:**
- Stop-loss tighter (-35% vs -40%) — with better entry timing, shouldn't need wide stop
- Take-profit much higher (+150%) — EARLY_CURVE winners go +100-500%, not +40%
- Max hold shorter (180s vs 300s) — if we're early, wave completes in 60-120s
- DOA check earlier (10s vs 15s) — with real-time data we know faster

---

## Phase 6: Testing Checklist

- [ ] `npm install @grpc/grpc-js bitquery-corecast-proto`
- [ ] Build passes: `npm run build`
- [ ] Test gRPC auth: connect with token, verify stream starts
- [ ] Verify newToken events emitted on pool creation
- [ ] Verify tokenTrade events emitted on Pump.fun trades
- [ ] Dedup working: same tx not processed twice
- [ ] Creator cache: lookup fires, rate limit respected
- [ ] Reconnect: kill stream, verify reconnect within 10s
- [ ] Run 30 min paper mode, compare event counts vs v2
- [ ] Enable live with 1/3 normal size for first 20 trades

---

## Success Metrics

| Metric | v2 (polling) | v3 (gRPC) target |
|--------|-------------|-----------------|
| Feed latency | 1-2s | <100ms |
| API calls/day | ~86,400 | <2,100 |
| Events/min | ~100 (batched) | ~500-2000 (real-time) |
| New token detection | 2-5s after creation | <200ms |
| Creator attribution | Not available | <500ms via cache |
| Stream reliability | N/A | >99.5% uptime |

---

## Rollback Plan

If gRPC fails in production:
1. Set `config.corecast_v3.enabled = false` in canary.json
2. Daemon hot-reloads config (no restart needed)
3. Falls back to v2 polling automatically
4. Zero downtime

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Bitquery auth token expires | Low | High | Monitor 401 errors, auto-alert |
| gRPC stream drops silently | Medium | High | Heartbeat check: if no events in 30s, reconnect |
| protobuf schema changes | Low | Medium | Pin `bitquery-corecast-proto` version |
| Creator API budget exceeded | Medium | Low | Hard cap in CreatorCache, alert at 80% |
| Stream 2 misses pool creation | Medium | Medium | PumpPortal WebSocket as backup for new tokens |
| New token before trades join | Low | Low | EventJoiner holds pending 60s |

---

## Implementation Order

1. `npm install` deps
2. Create `src/feed/creator-cache.ts`
3. Create `src/feed/event-joiner.ts`  
4. Create `src/feed/corecast-v3.ts`
5. Update `src/types/config.ts` and `src/types/events.ts`
6. Update `config/schema.json` and `config/canary.json`
7. Update `src/daemon/index.ts` (swap feed, conditional v2/v3)
8. Build + test
9. Paper mode 30 min
10. Live deploy with `corecast_v3.enabled = true`
