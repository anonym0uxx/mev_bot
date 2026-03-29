# Rust Final Build Plan — Zero TypeScript Cutover

**Author:** Staff Principal Architect Review  
**Date:** 2026-03-28  
**Goal:** Zero TS daemons/processes alive. Rust binary `pump-quant-core` as the sole runtime.  
**Status:** Phase 1-2 complete (core engine, feeds, gating, scoring, positions, persistence). Phases 3-7 remain.

---

## SECTION 1: PARITY AUDIT

### 1.1 Feed Layer

| TS Module | Rust File | Status | Parity Risk | Acceptance |
|---|---|---|---|---|
| `feed/pump-portal.ts` | `feeds/pumpportal.rs` | **Fully ported** | TS stores `solAmount` as float SOL; Rust stores as lamports u64. Parse path identical (both parse `solAmount` float × 1e9). TS does auto-resubscribe per-mint via `subscribeTokenTrade` — Rust does the same via `write_tx`. | Rust receives same event count as TS over 5-min window. Verify with counter logs on both sides simultaneously. |
| `feed/helius-ws.ts` | `feeds/helius.rs` | **Fully ported** | TS emits `tokenTrade` with `vSolInBondingCurve=0`; Rust emits `PreWarmEvent` with `vsol_reserves=0`. Both never trigger scoring. | PreWarm event count matches between TS and Rust over same window. |
| `feed/corecast-v3.ts` | `feeds/corecast.rs` | **Partially ported** | Rust corecast only emits `FeedEvent::CreatorSell`. TS corecast-v3 also emits `tokenTrade` events for history enrichment, `migration` events, and `lpRemoval` events for force-exit. **Rust is missing**: migration detection, LP removal force-exit, and trade events for detector history warming. | **CRITICAL GAP**: migration and LP removal signals not in Rust. Needed for forced exits in live mode. |
| N/A | `feeds/shredstream.rs` | **Rust-only (new)** | No TS equivalent — Rust adds ShredStream as a lower-latency pre-warm source. Intentional addition, not a parity concern. | ShredStream events arrive as PreWarm only; never trigger entries. |
| `feed/pump-portal.ts` (dedup) | `feeds/event_joiner.rs` | **Fully ported** | TS dedup is in `backrun-engine.ts` via `triggerDedup` Map (by signature). Rust dedup is in EventJoiner via `dedup_ring` (by sig_prefix 8 bytes). **PARITY RISK**: TS dedup uses full base58 signature (88 chars); Rust uses only first 8 bytes. This could theoretically let collisions through, but at 8-byte prefix the probability is astronomically low given Solana's signature space. Intentional deviation. | Monitor for any duplicate entry events in Rust that don't appear in TS. |

### 1.2 Gate Stack

| TS Gate | Rust Gate | Status | Parity Risk |
|---|---|---|---|
| Gate 1: is_buy | Gate 1: NotBuy | ✅ Match | — |
| Gate 2: trigger_min_buy_sol (0.50) | trigger_min_buy_lamports (500_000_000) | ✅ Match | 0.50 × 1e9 = 500_000_000 ✓ |
| Gate 2b: trigger_max_buy_sol (5.0) | trigger_max_buy_lamports (5_000_000_000) | ✅ Match | 5.0 × 1e9 = 5_000_000_000 ✓ |
| Gate 3: min_vsol (33) / max_vsol (43) | min_vsol_lamports / max_vsol_lamports | ✅ Match | 33 × 1e9 = 33_000_000_000 ✓; 43 × 1e9 = 43_000_000_000 ✓ |
| Gate 4: max_token_age_s (300) | max_token_age_ms (300_000) | ✅ Match | 300 × 1000 = 300_000 ✓ |
| Gate 5: min_unique_buyers (3) | min_unique_buyers (3) | ✅ Match | — |
| Gate 5b: large trigger (>1.5 SOL, <5 buyers) | large_trigger_lamports (1_500_000_000) | ✅ Match | — |
| Gate 6: pre_trigger_max_gap_ms (1000) | pre_trigger_max_gap_ms (1000) | ✅ Match | — |
| Gate 6: pre_trigger_min_buys_2s (5) | pre_trigger_min_buys_2s (5) | ✅ Match | — |
| Gate 6: pre_trigger_min_buys_5s (8) | pre_trigger_min_buys_5s (8) | ✅ Match | — |
| Gate 6: pre_trigger_min_buys_1s (11) | pre_trigger_min_buys_1s (11) | ✅ Match | — |
| Gate 6: pre_trigger_min_vsol_accel (0.8) | pre_trigger_min_vsol_accel (800_000_000) | ✅ Match | 0.8 × 1e9 = 800_000_000 ✓ |
| Gate 6e: pre_trigger_min_sell_count_5s (1) | pre_trigger_min_sell_count_5s (1) | ✅ Match | — |
| Gate 6f: pre_trigger_max_vsol_delta_3s (6.0) | pre_trigger_max_vsol_delta_3s (6_000_000_000) | ✅ Match | — |
| Gate 6b: creator_sell (30s TTL) | creator_sell_ttl_ms (60_000) | ⚠️ **MISMATCH** | TS uses 30_000ms (hardcoded `now - mh.creatorSellAt < 30_000`). Rust config uses 60_000ms default. **canary.json doesn't set this field** — Rust falls back to 60s. This means Rust rejects creator-sell tainted mints for 2× longer than TS. |
| Gate 6c: sell pressure (netFlowRatio5s < 0.2) | SellPressure (count-based proxy: 4×buy < 6×sell) | ⚠️ **SEMANTIC DIFFERENCE** | TS uses volume-weighted ratio: `(buyVol - sellVol) / (buyVol + sellVol) < 0.2`. Rust uses count-based proxy: `4×buy_count < 6×sell_count`. These can diverge when a few large sells dominate. Documented intentional deviation — count proxy is cheaper and reasonably correlated. |
| Gate 6d: max_trigger_isolation (0.35) | max_trigger_isolation (0.35) | ✅ Match | Rust uses integer FP: `trigger × 1M / (vol5s + trigger) ≤ 350_000`. Algebraically equivalent. |
| Gate 7: trigger_min_score (0.70) | trigger_min_score (0.70) | ✅ Match | — |
| **TOD blocked_hours_utc** (21 hours blocked) | blocked_hours_utc (loaded from config) | ✅ Match | Both read `tod_config.blocked_hours_utc` from canary.json. **Verification needed**: confirm Rust loads all 21 blocked hours correctly. |
| Engine-level: max_concurrent_positions | PositionManager cap | ✅ Match | Both read `max_concurrent_positions` from canary.json (10). |
| Engine-level: daily_loss_cap_sol | **NOT PORTED** | ❌ **GAP** | TS tracks `dailyLossSol` and resets at midnight UTC. Rust has no daily loss tracking. |
| Engine-level: consecutive_stop_pause | **NOT PORTED** | ❌ **GAP** | TS tracks consecutive SL exits and pauses entries for `consecutive_stop_pause_ms` (180s) after `consecutive_stop_pause_count` (3) stops. Rust has no circuit breaker. |
| Engine-level: pre_trigger_min_volume_5s | Gate 15: volume floor | ✅ Match | canary.json: 2.5 SOL → 2_500_000_000 lamports. |

### 1.3 Scorer

| TS Component | Weight | Rust Weight | Status | Parity Risk |
|---|---|---|---|---|
| buyMomentumTrend | 0.10 | 0.10 | ✅ Match | Formula: `clamp((ratio - 0.5) / 1.5)` where `ratio = buys_1s / max(buys_2s - buys_1s, 0.1)`. Both identical. |
| uniqueBuyersBanded | 0.25 | 0.25 | ✅ Match | Banded mapping identical: <3→0.1, 3-5→0.5+0.15×(n-3), 5-10→0.8+0.04×(n-5), 10-15→1.0-0.06×(n-10), >15→0.7. |
| buyerDiversity | 0.10 | 0.10 | ⚠️ **SEMANTIC DIFFERENCE** | TS formula: `unique_traders_30s / total_buys_30s × 1.5`. Rust approximation: `unique_buyers_30s / (total_buy_vol_30s / trigger_sol) × 1.5`. Different denominators. TS counts recent 30s buys; Rust estimates count from volume/trigger-size. **This will produce different scores for the same event.** However, both are clamped to [0,1] and weighted at only 10%, so the divergence impact is bounded (max ±0.10 on final score). |
| curveFill | 0.20 | 0.20 | ✅ Match | `1.0 - (vsol - min_vsol) / (max_vsol - min_vsol)`. Both use same min/max from config. |
| crowdDepth5s | 0.20 | 0.20 | ✅ Match | `min(1, vol_5s / 5_SOL)`. Rust: `clamp(volume_5s * inv_crowd_norm)` where norm = 5e9 lamports. |
| recentBuyers1s | 0.15 | 0.15 | ✅ Match | `min(1, buy_count_1s / 6)`. |
| adversarialPenalty | 0.6 threshold / 0.5× | 0.6 / 0.5 | ⚠️ **FUNCTIONAL GAP** | TS tracks per-wallet volume in 30s window. Rust passes `max_wallet_volume_lamports = 0` (not tracked per-wallet in hot path) and `total_buy_vol_30s = volume_5s × 6` (approximation). **Since max_wallet is always 0, the adversarial penalty NEVER fires in Rust.** This is a real parity gap — concentrated wallets won't get penalized. |

**Scorer min/max vsol for curveFill**: In `main.rs`, the Scorer is constructed with hardcoded `33_000_000_000` and `43_000_000_000` (lines 102-103), which matches canary.json `min_vsol_in_curve: 33` and `max_vsol_in_curve: 43`. ✅ Match — but there's a bug: the code does `.max(33_000_000_000)` and `.max(43_000_000_000)` against tp_tiers index, which would be 0. The `.max()` forces the correct value anyway. Still, this is fragile — should read directly from config.

### 1.4 Position Manager

| TS Feature | Rust Status | Parity Risk |
|---|---|---|
| TP/SL tiered by trigger_max_sol | ✅ Fully ported | Tiers loaded from canary.json identically. |
| Size tiers by trigger_max_sol | ✅ Fully ported | Same tier lookup logic. |
| max_hold_ms (400ms) | ✅ Match | Both exit as MaxHold when hold > 400ms. |
| next_buyer_exit (aggregate flow, count, single buy) | ✅ Fully ported | All three NB sub-conditions match. |
| next_buyer_profit_exit_pct (0.01) | ✅ Match | Both require pnl_pct ≥ 0.01 before NB exits. |
| momentum_decay_check_ms (50ms) | ✅ Match | Both use 50ms tick interval. |
| momentum_decay_min_mfe_pct (0.001) | ✅ Match | Gate 1: Flat exit when MFE < 0.1%. |
| momentum_decay_max_drawdown_pct (0.003) | ✅ Match | Gate 2: Fade exit when drawdown > 0.3%. |
| intra_hold_trailing_stop (1.0/1.0 — effectively disabled) | ✅ Match | Config values 1.0 mean 100% drop threshold — never triggers. Correct behavior. |
| PnL fee accounting (2% pump + 2× Jito) | ✅ Match | TS: `sizeSol × 0.01 × 2 + jito × 2 / 1e9`. Rust: `size_sol × 2 / 100 + jito × 2`. Both compute identical fees. |
| min_hold_before_exit_ms (500ms in TS) | ⚠️ **MISMATCH** | TS requires `holdSoFar >= 500` AND `tradesSeenAfterEntry >= 2` before NB exits. Rust `config.rs` sets `min_hold_before_exit_ms: 0`. However, Rust `positions.rs` checks `enough_data = trades_seen >= 2 && hold_ms >= min_hold_before_exit_ms`. With `min_hold_before_exit_ms=0`, the hold gate is effectively disabled in Rust. TS has it at 500ms. |
| Trigger signature dedup | ✅ Match | Both skip the trigger event in subsequent trade processing. |
| Skip zero-reserves events | ✅ Match | Both check for `vsol_reserves == 0` and skip. |
| ToD boost multiplier | ✅ Partially | Rust `positions.rs` stores `tod_multiplier` but never applies it to sizing — sizing is done purely from `lookup_size()`. TS applies `todMultiplier × base` with cap at `max_entry_size_sol`. |

### 1.5 Paper Trade Logging / JSONL Schema

| TS Field | Rust JSONL | Status |
|---|---|---|
| `mint` | ✅ `mint` | Match |
| `entryVSol` | ✅ `entry_vsol` (but as `entry_vsol` in SQLite, not JSONL) | ⚠️ JSONL key mismatch: Rust `paper_logger.rs` uses snake_case; TS uses camelCase. |
| `exitVSol` | ✅ `exit_vsol` | Same naming gap. |
| `holdMs` | ✅ `hold_ms` | |
| `sizeSol` | ✅ `size_sol` | |
| `pnlSol` | Rust has `gross_pnl_sol` and `net_pnl_sol` | ⚠️ TS `pnlSol` = gross PnL. Rust logs both. But scripts expect `pnlSol` key. |
| `exitReason` | ✅ `exit` (different key name!) | ⚠️ TS uses `exitReason`, Rust uses `exit`. Scripts parse `exitReason`. |
| `score` | ✅ `score` | Match |
| `entryTimestampMs` | ❌ Not in Rust JSONL | TS logs `entryTimestampMs`; Rust logs `ts` (exit time only) and `entry_ts_ms` in SQLite only. |
| `netPnlSol` | ✅ `net_pnl_sol` | TS key is `netPnlSol`. |
| `feesSol` | ❌ Not in Rust JSONL | TS logs `feesSol`; Rust doesn't log fees to JSONL (only SQLite). |
| `scoreComponents.*` | ❌ Not in Rust JSONL | TS logs all 7 score component values. Rust doesn't. |
| `preTriggerSignals.*` | ❌ Not in Rust JSONL | TS logs 10+ pre-trigger signal values. Rust doesn't. |
| `triggerBuySol`, `triggerBuyerCount`, etc. | ❌ Not in Rust JSONL | Missing ML training context. |
| `dataVersion` | ❌ Missing | TS always writes `dataVersion: 2`. |
| `engineVersion` | ✅ In SQLite | Rust logs to SQLite as `engine_version`; not in JSONL. |
| `is_paper` | ✅ `is_paper` | Match |

**VERDICT**: Rust JSONL is a **minimal subset** of TS JSONL. The `pnl-summary.js`, `quant-monitor.js`, and `analyze-losses.js` scripts will break because they read fields like `pnlSol`, `exitReason`, `netPnlSol`, `entryTimestampMs`, `excludeFromAnalysis` that don't exist or have different key names in Rust output.

### 1.6 Safety Systems

| TS System | Rust Status |
|---|---|
| `health/monitor.ts` — subsystem staleness checks, auto-pause | ❌ **NOT PORTED** |
| `alerts/system.ts` — Telegram immediate alerts, summaries | ❌ **NOT PORTED** |
| Circuit breaker: consecutive stops (3 → 180s pause) | ❌ **NOT PORTED** |
| Circuit breaker: daily loss cap (5 SOL paper / 0.18 SOL live) | ❌ **NOT PORTED** |
| Jito failure handler (pause on repeated bundle failures) | ❌ **NOT PORTED** |
| Auto-resume on health recovery | ❌ **NOT PORTED** |
| Feed staleness detection (45s threshold) | ❌ **NOT PORTED** |
| SIGTERM graceful drain (15s in-flight trade wait) | ❌ **NOT PORTED** (Rust has no graceful drain — just close_all) |

### 1.7 Execution Pipeline (Live Mode)

| TS System | Rust Status |
|---|---|
| `mev/jito-bundle-builder.ts` — Jito bundle construction | `tx/builder.rs` + `tx/jito.rs` exist but are stubs/partial |
| `mev/sell-executor.ts` — sell tx via Helius staked RPC | `tx/executor.rs` exists but is a stub |
| `tx/wallet.rs` — keypair loading | ✅ Exists |
| Blockhash cache / freshness | ❌ **NOT PORTED** |
| Slippage BPS from config | Partially in builder.rs |

### 1.8 Monitoring Scripts Compatibility

| Script | Reads From | Rust Compat |
|---|---|---|
| `pnl-summary.js` | `data/mev_paper_trades.jsonl` + `data/engine-state.json` + `data/pump-quant.db` | ❌ JSONL schema mismatch. `engine-state.json` not written by Rust. DB schema partially compatible (Rust writes `mev_trades` table; TS scripts read `positions` table). |
| `quant-monitor.js` | `data/pump-quant.db` (positions table) | ❌ Reads `positions` table which Rust doesn't write to (Rust writes `mev_trades`). |
| `analyze-losses.js` | `data/pump-quant.db` (orders table) | ❌ Reads `orders` table which Rust doesn't write to. |

---

## SECTION 2: GAP INVENTORY

### Safety & Circuit Breakers

```
TASK-1: Daily Loss Cap Tracking
File: rust/pump-quant-core/src/engine/hot_path.rs
TS source: src/mev/backrun-engine.ts (dailyLossSol, checkAndResetDailyLoss)
What: Add `daily_loss_sol: i64` and `daily_loss_reset_day: i32` fields to HotPath.
      After each position close (in the logger thread feedback or in hot_path directly),
      accumulate net_pnl_sol for losses. Reset when UTC day changes.
      In on_trade(), after gates pass but before open_position(), check if
      daily_loss_sol >= daily_loss_cap_lamports. If so, reject entry.
      Add config fields: `daily_loss_cap_lamports: u64` to EngineConfig,
      loaded from `mev.daily_loss_cap_sol` (or paper/live variants).
Parity risk: TS resets on UTC day-of-month; Rust must use same logic.
             TS uses paper_daily_loss_cap_sol vs live_daily_loss_cap_sol.
Acceptance: Unit test: 3 losing trades of 0.02 SOL each triggers cap at 0.05 SOL cap.
            Verify reset at midnight UTC boundary.
Complexity: M
Blocks cutover: yes
```

```
TASK-2: Consecutive Stop-Loss Circuit Breaker
File: rust/pump-quant-core/src/engine/hot_path.rs
TS source: src/mev/backrun-engine.ts (consecutiveStops, stopPauseUntilMs)
What: Add `consecutive_stops: u32`, `stop_pause_until_ms: u64` to HotPath.
      After each ClosedPosition with ExitReason::StopLoss, increment counter.
      After TP/NB/IntraHoldTrail, reset to 0.
      When counter >= consecutive_stop_pause_count (default 3), set
      stop_pause_until_ms = now + consecutive_stop_pause_ms (default 180_000).
      In on_trade() after gates pass, check now < stop_pause_until_ms → reject.
      Add config: `consecutive_stop_pause_count: u32`, `consecutive_stop_pause_ms: u64`.
      The challenge: ClosedPosition is sent via crossbeam to the logger thread.
      HotPath doesn't get feedback. Solution: make PositionManager return the
      ClosedPosition (or ExitReason) from on_subsequent_trade/on_tick so HotPath
      can track consecutive stops directly.
Parity risk: TS counts only `stop_loss` exits. Must match exactly.
Acceptance: Unit test: 3 SL exits → pause for 180s. Then TP resets counter.
Complexity: M
Blocks cutover: yes
```

```
TASK-3: Feed Health Monitor & Auto-Pause
File: rust/pump-quant-core/src/engine/health.rs (NEW)
TS source: src/health/monitor.ts
What: Create a HealthMonitor struct that tracks last-event timestamps per feed source.
      Fields: last_pumpportal_event_ms, last_helius_event_ms, last_corecast_event_ms.
      On each FeedEvent in main loop, update the corresponding timestamp.
      Every 5s (on tick), check if any required feed is stale (> market_feed_stale_s from config).
      If stale, set `trading_paused = true` in shared state (ApiState or HotPath flag).
      When feed recovers, auto-resume.
      Expose via /api/health endpoint (currently hardcoded "healthy").
      Config fields from canary.json `health` section: check_interval_s,
      market_feed_stale_s (45), auto_pause_on_degraded (true).
Parity risk: TS health monitor checks 6 subsystems. Rust only needs market_feed for MVP.
Acceptance: Unit test: simulate no events for 50s → paused=true. Feed resumes → paused=false.
Complexity: M
Blocks cutover: yes
```

```
TASK-4: Telegram Alert Integration
File: rust/pump-quant-core/src/alerts/telegram.rs (NEW)
TS source: src/alerts/system.ts + daemon wiring
What: Create a minimal Telegram alert sender using reqwest HTTP client.
      Function: `send_telegram(bot_token: &str, chat_id: &str, message: &str) -> Result<()>`
      Integrate into the logger thread: on each ClosedPosition, format a trade alert
      message (mint, exit reason, PnL, hold time) and send via Telegram.
      On circuit breaker activation, send alert.
      On feed staleness pause, send alert.
      Config: read TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID from env vars.
      Rate limit: max 1 message per second (Telegram API limit).
Parity risk: TS alerts route through daemon → OpenClaw messaging layer. Rust must
             directly call Telegram Bot API. Format should match TS alert strings for
             operator familiarity.
Acceptance: Manual test: closed position generates Telegram message within 2s.
Complexity: M
Blocks cutover: yes
```

### JSONL & Script Compatibility

```
TASK-5: JSONL Schema Parity
File: rust/pump-quant-core/src/persistence/paper_logger.rs
TS source: src/mev/paper-trade-logger.ts
What: Rewrite JSONL output to match TS field names EXACTLY:
      Required fields (TS camelCase names):
        mint, entryVSol, exitVSol, entryTimestampMs, exitTimestampMs, holdMs,
        sizeSol, pnlSol (=gross_pnl), pnlPct, exitReason, score,
        netPnlSol, feesSol, engineVersion, dataVersion (=2), is_paper (=true),
        excludeFromAnalysis (=false)
      Optional but recommended for training:
        triggerBuySol, scoreComponents.*, preTriggerSignals.*
      The ClosedPosition struct needs additional fields:
        - pnl_pct (computed from gross_pnl / size)
        - trigger_sol (already exists)
      Add score_components to ClosedPosition (from Scorer output, piped through).
Parity risk: Field names MUST be camelCase to match JS scripts. Any typo = broken scripts.
Acceptance: `diff <(head -1 data/mev_paper_trades_ts.jsonl | jq -S keys) <(head -1 data/mev_paper_trades.jsonl | jq -S keys)` shows identical key sets for the mandatory fields.
Complexity: M
Blocks cutover: yes
```

```
TASK-6: Engine State File
File: rust/pump-quant-core/src/persistence/engine_state.rs (NEW)
TS source: src/daemon/index.ts (engine-state.json writes)
What: Write `data/engine-state.json` on startup and periodically (every 60s):
      { "daemonStartedAt": <epoch_ms>, "engineVersion": "v5-rust",
        "configVersion": "<canary.json md5 or mtime>" }
      This is read by pnl-summary.js to determine session boundaries.
Parity risk: Must write to exact same path. Must use same field names.
Acceptance: `cat data/engine-state.json | jq .daemonStartedAt` returns valid epoch ms.
Complexity: S
Blocks cutover: yes
```

```
TASK-7: SQLite Schema Compatibility for Monitoring Scripts
File: rust/pump-quant-core/src/persistence/sqlite.rs
TS source: scripts/pnl-summary.js, scripts/quant-monitor.js, scripts/analyze-losses.js
What: The scripts read from `positions` and `orders` tables. Rust currently writes to
      `mev_trades` table only. Two options:
      (A) Also write to `positions` table with compatible schema, OR
      (B) Rewrite scripts to read from `mev_trades`.
      Option B is cleaner. Rewrite all 3 scripts to read from `mev_trades` table
      (which Rust already populates) with field mapping.
      Alternatively, create a VIEW: `CREATE VIEW IF NOT EXISTS positions AS SELECT ... FROM mev_trades`.
Parity risk: Scripts use column names like `realized_pnl_sol`, `opened_at`, `exit_reason`,
             `regime`. The VIEW approach maps cleanly.
Acceptance: `node scripts/pnl-summary.js` produces valid output against Rust-populated DB.
Complexity: M
Blocks cutover: yes
```

### Scoring Parity

```
TASK-8: Adversarial Concentration Tracking
File: rust/pump-quant-core/src/core/mint_map.rs
TS source: src/mev/detector.ts (computeScore — adversarial section)
What: Track per-wallet buy volume in MintHistory. Add a small HashMap<[u8;32], u64>
      (wallet → cumulative_buy_volume_30s) or a simpler approach:
      track `max_wallet_buy_volume_30s` and `total_buy_volume_30s` as cached fields.
      On each buy trade, update the per-wallet map (evict >30s entries on trade arrival).
      Pass max_wallet_volume and total_buy_vol_30s to Scorer.compute().
      Currently hot_path passes 0 for max_wallet_volume — adversarial penalty never fires.
Parity risk: TS recomputes from full 30s trade list on every score. Rust must maintain
             running aggregates. Small divergence possible from eviction timing.
Acceptance: Unit test: 5 buys from same wallet (1 SOL each) + 2 from another (0.5 each) →
            concentration = 5/6 = 0.83 > 0.6 → penalty = 0.5.
Complexity: M
Blocks cutover: no (acceptable deviation for paper mode, but needed for live)
```

```
TASK-9: Buyer Diversity Score Fix
File: rust/pump-quant-core/src/core/mint_map.rs + src/engine/scorer.rs (scorer.rs is READ-ONLY — see below)
TS source: src/mev/detector.ts (computeScore — diversity section)
What: The diversity score needs total_buy_count_30s (not volume). MintHistory should
      track `cached_total_buy_count_30s: u16`. Pass this to scorer. Since we CANNOT
      modify scorer.rs, the fix must go into hot_path.rs: pass total_buy_count_30s
      in the `total_buy_vol_30s_lamports` parameter slot, and set
      `_trigger_sol_lamports` to 1_000_000_000 (1 SOL) so the approximation in
      scorer matches: estimated_buys = count / 1.0 = count. Then diversity =
      unique / count * 1.5.
      Wait — this conflicts with adversarial check which needs real