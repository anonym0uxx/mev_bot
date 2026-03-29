# pump-quant Architecture (v6 — Rust)

> Last updated: 2026-03-29. TypeScript engine retired. Rust binary is sole runtime.

---

## System Overview

```
  External Sources                    pump-quant Rust Binary
  ─────────────────                   ──────────────────────────────────────────

  pump.fun chain ──── Helius WS ─────► feeds::helius        50ms avg latency
                                        ↓ FeedEvent::Trade   PRIMARY TRIGGER
  pump.fun WS ─────── PumpPortal ────► feeds::pumpportal    120ms avg latency
                                        ↓ FeedEvent::Trade   STATE SYNC / DEDUP
  Bitquery gQL ─────── CoreCast WS ──► feeds::corecast      80ms avg latency
                      (4 subs,          ↓ FeedEvent::
                       1 connection)       CreatorSell       signer match
                                          Migration          Raydium graduation
                                          LpRemoval          rug detection
                                          NewToken           creator_map prewarm

                                               │
                                               ▼
                                    ┌──────────────────────┐
                                    │  engine::hot_path    │
                                    │                      │
                                    │  MintMap cache       │  Helius correlation
                                    │  PreWarmMap          │  50ms pre-signal
                                    │  creator_map         │  Arc<RwLock<HashMap>>
                                    │  monotonic clock     │  no syscall/event
                                    │  stack bs58 decode   │  no heap/event
                                    └──────────┬───────────┘
                                               │
                                    ┌──────────▼───────────┐
                                    │   Gate Stack         │
                                    │                      │
                                    │  1. TriggerSize      │
                                    │  2. VsolRange        │
                                    │  3. PreTriggerVol    │
                                    │  4. BuyMomentum      │
                                    │  5. SellPressure     │
                                    │  6. VsolDelta        │
                                    │  7. Concentration    │  adversarial wallets
                                    │  8. TimeOfDay        │  blocked/boosted hrs
                                    └──────────┬───────────┘
                                               │
                                    ┌──────────▼───────────┐
                                    │   Scorer             │
                                    │                      │
                                    │  momentum * weight   │
                                    │  + volume * weight   │
                                    │  + curve * weight    │
                                    │  * ToD multiplier    │
                                    │  - concentration     │
                                    │  + corecast bonus    │
                                    └──────────┬───────────┘
                                               │ score ≥ threshold
                                    ┌──────────▼───────────┐
                                    │  PositionManager     │
                                    │                      │
                                    │  open()              │  tiered sizing
                                    │  tick() @ 50ms       │  momentum decay
                                    │  force_close()       │  migration/rug
                                    │  on_creator_sell()   │   30s TTL
                                    │                      │
                                    │  Safety:             │
                                    │  daily_loss_cap      │
                                    │  consec_sl_breaker   │  3 SL → 180s pause
                                    │  min_hold_ms = 500   │
                                    └──────────┬───────────┘
                                               │
                              ┌────────────────┴──────────────────┐
                              │ PAPER_MODE=true                    │ PAPER_MODE=false
                              ▼                                    ▼
                   persistence::paper_logger            tx::executor
                   mev_paper_trades.jsonl               BlockhashCache (30s TTL)
                   camelCase TS-compatible              Jito bundle or direct RPC
                   MFE/MAE/exitReason/fees              priority_fee_sol config
```

---

## Feed Timing Model

```
  Token created on chain
          │
          │  ~5ms    ShredStream (future, pending Jito whitelist)
          │  ~50ms   Helius logsSubscribe  ◄── PRIMARY TRIGGER
          │  ~80ms   Bitquery CoreCast     ◄── creator sell / migration / rug
          │  ~120ms  PumpPortal WebSocket  ◄── state sync + dedup
          │
          ▼
  hot_path receives Helius event → checks MintMap → if pre-warmed → evaluate gates
  If PumpPortal event arrives later for same mint → deduplicated, state synced
```

**Helius lead advantage:** 50ms avg pre-signal on 96.6% of events (966/1000 in first session).
This means the engine evaluates gate conditions before PumpPortal confirms — faster entry on qualifying setups.

---

## CoreCast Stream Map

```
  Bitquery WS Connection (1 connection = 4/5 stream cap used)
  ├── Subscription id=1  DEXTrades (pump.fun program)
  │     → parse signer → match creator_map → FeedEvent::CreatorSell
  │
  ├── Subscription id=2  DEXTrades (Raydium program 675kPX9...)
  │     → token migrated to AMM → FeedEvent::Migration
  │     → hot_path::on_migration() → force_close open position
  │
  ├── Subscription id=3  TokenSupplyUpdates
  │     → PostBalance < PreBalance × 0.5 → LP burned → FeedEvent::LpRemoval
  │     → hot_path::on_lp_removal() → force_close open position
  │
  └── Subscription id=4  Instructions (pump.fun create)
        → new token launch → FeedEvent::NewToken
        → pre-warms creator_map before PumpPortal fires
```

---

## Safety / Circuit Breakers

```
  Daily loss cap
  ├── Paper: 5.0 SOL
  └── Live:  0.18 SOL
        → auto-pause trading for rest of UTC day on breach

  Consecutive stop-loss circuit breaker
  └── 3 consecutive SL exits → 180s trading pause
        → auto-resume after cooldown
        → logged + Telegram alert

  Feed health monitor (HealthMonitor — AtomicU64 per feed)
  └── Feed stale > 45s → auto-pause + Telegram alert
        → resume on feed reconnect

  Min hold before exit
  └── 500ms minimum — prevents immediate flip-exit noise

  Creator sell TTL
  └── 30s — force-exit if creator sells within 30s of entry

  Migration / LP removal
  └── Immediate force-exit on Raydium graduation or LP burn
```

---

## Persistence Schema

### JSONL — `data/mev_paper_trades.jsonl`
```json
{
  "mint": "...",
  "entryPriceLamports": 1000000,
  "exitPriceLamports": 1050000,
  "pnlSol": 0.00312,
  "netPnlSol": 0.00198,
  "feesSol": 0.000114,
  "mfeSol": 0.00420,
  "maeSol": -0.00050,
  "exitReason": "take_profit",
  "holdMs": 287,
  "isPaper": true,
  "triggerSource": "helius",
  "preTriggerVolume5s": 3.2,
  "entryTimestampMs": 1711670400000,
  "exitTimestampMs": 1711670400287
}
```

### SQLite — `data/pump-quant.db`
Tables: `raw_events`, `token_state`, `feature_snapshots`, `candidate_packets`, `trade_intents`, `orders`, `positions`, `config_versions`, `replay_runs`, `health_events`, `learning_ledger`, `state_transitions`, `config_changes`

---

## HTTP API — `:9421`

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Feed status, staleness, trading_paused |
| `/api/stats` | GET | trades_seen, gates_passed, WR, PnL, migrations_seen, lp_removals_seen, new_tokens_seen, creator_sells_seen |
| `/api/control/pause` | POST | Pause trading |
| `/api/control/resume` | POST | Resume trading |

---

## Process Management

```
  ensure-single-daemon.sh   ← called at every startup path
  ├── kills TS daemon (node dist/daemon) if somehow alive
  ├── kills TS supervisor (run-daemon.sh)
  ├── kills ALL pump-quant Rust processes
  ├── kills run-rust-daemon.sh supervisor
  ├── removes stale PID file
  └── optionally starts fresh Rust daemon (--start flag)
       writes PID to data/pump-quant.pid

  Startup paths that call ensure-single-daemon.sh:
  ├── scripts/run-rust-daemon.sh      (supervisor loop)
  ├── scripts/cutover-to-rust.sh      (migration script)
  ├── scripts/pump-quant-rust.service (systemd ExecStartPre)
  └── HEARTBEAT.md restart command    (heartbeat crash recovery)
```

---

## Monitoring Flow (Heartbeat)

```
  Every heartbeat (OpenClaw cron):
  1. PAPER_MODE=true node scripts/rust-status.js
     ├── GET :9421/api/stats  (session + all-time PnL, WR, fees)
     ├── GET :9421/api/health (feed staleness, trading_paused)
     ├── parse data/mev_paper_trades.jsonl (trade history)
     ├── parse data/heartbeat-trade-state.json (delta tracking)
     └── emit structured report to Telegram

  Report sections:
  ├── Engine header (mode, uptime)
  ├── Session P&L (WR, gross, net, exit breakdown, fee drag)
  ├── Overall P&L (all-time)
  ├── Feed & Latency (PumpPortal/Helius staleness, throughput, gate pass %)
  ├── Stream Events (migrations, LP removals, new tokens, creator sells)
  └── Alerts (feed down, WR critical, PnL breach, fee drag >5%)
```
