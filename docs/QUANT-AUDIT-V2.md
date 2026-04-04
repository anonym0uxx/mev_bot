# PUMP-QUANT v5 → v6: REVISED STRATEGY AUDIT & BUILD SPEC
## Based on On-Chain Ground Truth (167 Round-Trips, 10 Days)

---

# PART 1: REVISED STRATEGY ASSESSMENT

## 1.1 The Brutal Truth

| Metric | Trade Log Claimed | On-Chain Reality |
|---|---|---|
| Trades | 344 | 167 complete + 13 stuck |
| Win Rate | 15.4% | 13.2% (22/167) |
| Net PnL | +0.179 SOL | -0.163 SOL (trades only) |
| Win/Loss Ratio (b) | 8.15x | 1.85x |
| Kelly Fraction | +0.026 | **-0.337** (NEGATIVE EV) |
| Total Real Loss | "profitable" | **-0.400 SOL** |

## 1.2 Kelly — Real Numbers

```
f* = p - q/b = 0.1317 - 0.8683/1.846 = 0.1317 - 0.4703 = -0.3386
E[trade] = 0.1317 × 0.00288 - 0.8683 × 0.00156 = -0.000975 SOL/trade
167 trades × -0.000975 = -0.163 SOL ← matches on-chain exactly
```

## 1.3 Is This Strategy Fundamentally Viable?

**Yes, but not in its current form.**

Evidence:
1. **3-5s hold bucket: 50% WR on 6 trades** — the only +EV bucket
2. **Top winner (+82.2%, 40-min hold)** generated 39% of all winning PnL
3. **61 "fee floor" trades (-5.3% each)** = zero-information trades on dead tokens. Fix the filter → eliminate 36.5% of all trades
4. **13 stuck tokens (-0.219 SOL)** = mechanical failure, not strategy failure

**The engine isn't executing the strategy it claims.** Says probe_hold=3000ms but holds 0-1s. Says probe_size=0.03 but buys at 0.0124.

## 1.4 Path from -EV to +EV

For E=0 at current b=1.85: need WR ≥ 35.1% — unrealistic alone.

But improving b changes everything:

| Win Rate (p) | Required b | How to get there |
|---|---|---|
| 20% | > 4.00 | Eliminate dead tokens + hold winners longer |
| 25% | > 3.00 | Above + better entry filter |
| 13.2% (current) | > 6.58 | Hold winners 3.5× longer OR cut losses 3.5× faster |

**Realistic target: p=0.25, b=4.0**
- Eliminate 61 dead-token trades → WR rises to 22/106 = 20.8%
- Hold winners past 3s → avg_win rises from +15.8% toward +25%+
- E[trade] = 0.25 × 0.00546 - 0.75 × 0.00156 = +0.000195/trade
- At ~10 trades/day: **+0.00195 SOL/day**

---

# PART 2: FEE-ADJUSTED VIABILITY

## 2.1 Real Friction Per Trade

- PumpSwap AMM: 0.50% round-trip
- Gas/tips: ~0.48% at 0.0124 SOL position
- Slippage on thin pools: ~4.3%
- **Total friction on dead tokens: ~5.3%** (confirmed by 61 identical -5.3% trades)

## 2.2 Trading Frequency

**Current: 16.7 trades/day. This is destroying value.**

If we ONLY took the 12 trades that held >3s: net PnL = +0.00383 SOL (vs -0.163 SOL).
The 155 trades at 0-2s hold = -0.166 SOL of losses.

**Recommended: 3-6 high-conviction trades/day.**

---

# PART 3: REVISED BUILD SPEC — 5 ENGINEERS

## Priority by Impact

| Priority | Fix | Loss Source | Expected Recovery |
|---|---|---|---|
| **P0** | Sell pipeline reliability | -0.219 SOL (stuck) | +0.150 SOL |
| **P0** | Actual probe hold enforcement | -0.166 SOL (0-2s exits) | +0.100 SOL |
| **P1** | Dead token filter | -0.050 SOL (61 fee-floor) | +0.040 SOL |
| **P1** | Trade log integrity | Caused ALL prior analysis to be wrong | Priceless |
| **P2** | Winner management / trailing stop | Lost upside | +0.030 SOL |

---

## ENGINEER 1: SELL PIPELINE RELIABILITY

**Problem:** 13 tokens bought, never sold. -0.219 SOL (55% of all losses). Some stuck 10+ days.

### 1.1 Sell Escalation Ladder (Sell-or-Escalate FSM)

```
PENDING_SELL → ATTEMPT_1 → ATTEMPT_2 → ATTEMPT_3 → EMERGENCY → FORCE_SELL
Every state has TIMEOUT. No state is terminal except SOLD or FORCE_SOLD.
```

```rust
const ESCALATION_LADDER: [SellEscalation; 5] = [
    // Attempt 1: Normal sell, 3% slippage
    { attempt: 1, max_slippage_bps: 300, jito_tip: 25_000, timeout_ms: 2_000 },
    // Attempt 2: Higher tip, wider slippage
    { attempt: 2, max_slippage_bps: 800, jito_tip: 50_000, timeout_ms: 2_000 },
    // Attempt 3: Direct RPC, 15% slippage
    { attempt: 3, max_slippage_bps: 1500, jito_tip: 0, timeout_ms: 3_000 },
    // Attempt 4: Multi-RPC, 50% slippage
    { attempt: 4, max_slippage_bps: 5000, jito_tip: 0, timeout_ms: 5_000 },
    // Attempt 5: Nuclear — sell at any price
    { attempt: 5, max_slippage_bps: 9900, jito_tip: 100_000, timeout_ms: 10_000 },
];
```

**Math:** 13 stuck tokens × 0.0168 SOL avg = 0.219 SOL lost. Even selling at 50% loss on attempt 5 recovers 0.0084/token. Holding a memecoin indefinitely → 0.

### 1.2 Position Inventory Watchdog

```rust
// Runs every 10s, independent of trade engine
async fn inventory_watchdog() {
    let on_chain = fetch_token_balances(wallet).await;
    let known = db.get_open_positions().await;
    
    // Orphaned positions: on-chain but not in DB
    for balance in on_chain {
        if !known.contains(&balance.mint) && balance.amount > 0 {
            db.insert_emergency_sell(balance.mint, SellReason::Orphan).await;
        }
    }
    // Stale positions: in DB, no sell attempt in >30s
    for pos in known {
        if pos.last_sell_attempt.elapsed() > 30s {
            sell_engine.escalate(pos).await;
        }
    }
}
```

### 1.3 On-Chain Confirmation

After every sell TX submit: poll `getSignatureStatuses` for up to 30s. If not confirmed → re-enter escalation ladder at next rung.

**Config changes:**
- ADD: `sell_escalation_enabled: true`
- ADD: `sell_max_attempts: 5`
- ADD: `sell_confirmation_timeout_ms: 30000`
- ADD: `inventory_watchdog_interval_ms: 10000`
- ADD: `stale_position_threshold_ms: 30000`

---

## ENGINEER 2: ENFORCE ACTUAL PROBE HOLD TIME

**Problem:** probe_hold_ms = 3000 but 84% of trades hold 0-1 seconds on-chain. The engine claims to probe but exits immediately.

**Root Cause:** The sell TX is submitted during or before the probe window completes. The "hold time" on-chain reflects TX submission timing, not decision timing.

### 2.1 Hard Hold Gate

```rust
// In the exit evaluation loop — add as FIRST check, before ANY exit logic
fn should_evaluate_exit(&self, now_ms: u64) -> bool {
    let elapsed = now_ms - self.entry_confirmed_ms; // NOT entry_decided_ms
    if elapsed < self.config.min_hold_before_exit_ms {
        return false; // DO NOT EVALUATE ANY EXIT
    }
    true
}
```

**Critical:** `entry_confirmed_ms` must be set when the BUY TX is confirmed on-chain, NOT when the buy decision was made. Currently the engine likely timestamps the decision, then submits TX (600-1200ms), then the entry_ts in the position is already stale.

### 2.2 Entry Confirmation Required Before Exit

```rust
enum PositionState {
    BuySubmitted,      // TX sent, not confirmed
    BuyConfirmed(u64), // TX confirmed, timestamp
    ProbeHolding,      // Confirmed, waiting min_hold
    ExitEligible,      // Probe hold passed, can evaluate exits
    SellSubmitted,     // Sell TX sent
    SellConfirmed,     // Sell TX confirmed
    Closed,
}
```

**Rule: No exit evaluation until state == ExitEligible.**

### 2.3 Actual Timing Budget

```
Buy decision:        T+0ms
Buy TX submit:       T+10ms
Buy TX lands:        T+600-1200ms (this is entry_confirmed_ms)
Min hold:            T+600 + 3000 = T+3600ms minimum
First exit eval:     T+3600ms at earliest
Exit decision:       T+3600-5000ms typical
Sell TX submit:      T+3610ms
Sell TX lands:       T+4200-4800ms

On-chain hold time: ~3-4 seconds minimum (vs current 0-1s)
```

### 2.4 Expected Impact

From data: the 6 trades at 3-5s hold had 50% WR vs 12% at 0-1s. If we force 155 currently-0-1s trades to hold 3s:
- Some will hit SL during those 3s → exit at same loss, but with information
- Some will show momentum → hold longer → convert to winners
- Dead tokens will still be identifiable but in 3s not 0s

**Conservative estimate:** WR improves from 13.2% to ~20% just from holding 3s.

**Config changes:**
- ADD: `min_hold_before_exit_ms: 3000`
- ADD: `entry_confirmed_required: true`
- CHANGE: Track `entry_confirmed_ms` from on-chain confirmation, not decision time

---

## ENGINEER 3: DEAD TOKEN FILTER (Pre-Entry Activity Gate)

**Problem:** 61 trades lost exactly -5.3% = bought dead tokens with zero price movement.

### 3.1 WebSocket Activity Gate

```rust
// BEFORE any buy TX submission
fn should_enter(mint: &Pubkey, ws_tracker: &WsTracker) -> EntryDecision {
    let stats = ws_tracker.get_stats(mint);
    
    // Gate 1: Minimum ongoing activity
    if stats.ws_messages_last_3s < 5 {
        return EntryDecision::Reject("insufficient_activity");
    }
    
    // Gate 2: Recent trade confirmation  
    if stats.last_trade_age_ms > 2000 {
        return EntryDecision::Reject("stale_trading");
    }
    
    // Gate 3: Buy-side activity present
    if stats.buys_last_3s == 0 {
        return EntryDecision::Reject("no_buy_pressure");
    }
    
    // Gate 4: Price movement confirmation (not flat)
    if stats.price_range_bps_last_3s < 50 {
        return EntryDecision::Reject("flat_price"); // No one is moving the price
    }
    
    EntryDecision::Proceed
}
```

### 3.2 Mathematical Justification

61 dead-token trades × -0.000823 avg loss = -0.050 SOL

If this filter eliminates 90% of dead tokens (55 trades) and incorrectly blocks 5% of winners (1 trade):
- Saved: 55 × 0.000823 = 0.0453 SOL
- Lost: 1 × 0.00288 = 0.0029 SOL
- **Net gain: +0.042 SOL**

### 3.3 Post-Filter Trade Count

167 current - 55 dead tokens = ~112 trades/10 days = ~11/day
Still higher than recommended 3-6, but a step in the right direction.

**Config changes:**
- ADD: `entry_min_ws_messages_3s: 5`
- ADD: `entry_max_last_trade_age_ms: 2000`
- ADD: `entry_min_buys_3s: 1`
- ADD: `entry_min_price_range_bps_3s: 50`

---

## ENGINEER 4: TRADE LOG INTEGRITY

**Problem:** Trade log reported 344 trades at +0.179 SOL. Reality: 167 trades at -0.163 SOL. Every decision based on trade log data was wrong.

### 4.1 On-Chain P&L Reconciliation

```rust
struct TradeRecord {
    // Existing fields...
    
    // NEW: On-chain confirmation fields
    buy_signature: Option<String>,
    buy_confirmed: bool,
    buy_slot: Option<u64>,
    buy_lamports_spent: Option<u64>,    // From getTransaction
    
    sell_signature: Option<String>,
    sell_confirmed: bool,
    sell_slot: Option<u64>,
    sell_lamports_received: Option<u64>,  // From getTransaction
    
    // Computed on-chain P&L
    onchain_pnl_lamports: Option<i64>,
    
    // Reconciliation status
    reconciled: bool,
    reconciliation_error: Option<String>,
}
```

### 4.2 Real-Time Reconciliation Loop

```rust
// After every trade completes (sell confirmed OR position closed)
async fn reconcile_trade(trade: &mut TradeRecord) {
    // 1. Fetch buy TX from chain
    if let Some(sig) = &trade.buy_signature {
        let tx = rpc.get_transaction(sig).await;
        trade.buy_lamports_spent = extract_sol_delta(tx, wallet);
        trade.buy_confirmed = tx.is_some();
    }
    
    // 2. Fetch sell TX from chain
    if let Some(sig) = &trade.sell_signature {
        let tx = rpc.get_transaction(sig).await;
        trade.sell_lamports_received = extract_sol_delta(tx, wallet);
        trade.sell_confirmed = tx.is_some();
    }
    
    // 3. Compute on-chain P&L
    if let (Some(spent), Some(recv)) = (trade.buy_lamports_spent, trade.sell_lamports_received) {
        trade.onchain_pnl_lamports = Some(recv as i64 - spent as i64);
        trade.reconciled = true;
    }
    
    // 4. Alert on discrepancy
    if trade.reconciled {
        let log_pnl = trade.pnl_lamports;
        let chain_pnl = trade.onchain_pnl_lamports.unwrap();
        let diff = (log_pnl - chain_pnl).abs();
        if diff > 10_000 { // >0.00001 SOL discrepancy
            tracing::error!(
                mint=%trade.mint, log_pnl, chain_pnl, diff,
                "[RECONCILIATION] P&L mismatch detected!"
            );
        }
    }
}
```

### 4.3 Status Endpoint Correction

The `/api/status` endpoint must report:
- `log_pnl`: what the engine thinks happened
- `onchain_pnl`: what actually happened on-chain
- `unreconciled_count`: trades not yet confirmed on-chain
- `stuck_positions`: tokens bought but not sold

```rust
#[derive(Serialize)]
struct StatusResponse {
    // ... existing ...
    onchain_pnl_sol: f64,
    log_pnl_sol: f64,
    pnl_discrepancy_sol: f64,
    unreconciled_trades: u32,
    stuck_positions: Vec<StuckPosition>,
}
```

**No config changes — this is pure implementation.**

---

## ENGINEER 5: WINNER MANAGEMENT (Hold Winners Longer)

**Problem:** Only 1 trade held >60s and was a winner (+82.2%). The engine kills winners by selling too early.

### 5.1 Momentum Lock (Don't Exit Winners)

```rust
fn evaluate_exit(&self, current_bps: i32, hold_ms: u64) -> ExitDecision {
    // If in profit and still receiving activity → DO NOT EXIT
    if current_bps > 0 && self.ws_messages_last_5s > 0 {
        // Only trail from peak, never time-exit
        let trail_bps = self.compute_trail_bps(current_bps);
        let floor = self.peak_bps - trail_bps as i32;
        
        if current_bps < floor {
            return ExitDecision::TrailingStop;
        }
        
        return ExitDecision::Hold; // Keep riding
    }
    
    // In profit but no activity → tighten stop dramatically
    if current_bps > 0 && self.ws_messages_last_5s == 0 {
        let floor = current_bps - 100; // 1% trailing from current (not peak)
        if current_bps < floor {
            return ExitDecision::StaleProfit;
        }
        return ExitDecision::Hold;
    }
    
    // At loss → evaluate normal SL logic
    // ...existing logic...
}
```

### 5.2 Adaptive Trail (Tightened from V1 Spec)

```rust
fn compute_trail_bps(&self, gain_bps: i32) -> u16 {
    match gain_bps {
        ..=200   => 100,    // Tight: protect small gains
        201..=500 => 200,   // Medium: let momentum develop
        501..=1500 => 400,  // Wide: big move, let it breathe
        1501..=5000 => 800, // Very wide: moonshot territory
        _ => 1500,          // Extreme: 15% trail on 50%+ gains
    }
}
```

**Math:** Current trailing stop at 25% (2500 bps) from peak is too wide.

If a token peaks at +30% (+3000 bps), trailing stop at 25% exits at +500 bps (+5%).
With 400 bps trail: exits at +2600 bps (+26%). That's 5.2× more captured profit.

For the top winner at +82.2%: would have exited at ~+74% instead of whenever it actually exited. But most winners peak much lower — capturing +20% instead of +5% on a +25% peak is the real win.

### 5.3 Disable time_sl for Profitable Positions

```rust
// time_sl should NEVER fire on a profitable position
if current_bps > 0 {
    // Skip ALL time-based exits (time_sl, dead_zone, max_hold)
    // Only trail-based exits apply to winners
    return;
}
```

### 5.4 Expected Impact

Current 22 winners avg +15.8%. If trail improvements capture 50% more of peak moves:
- New avg_win ≈ +24% → avg_win_sol ≈ 0.00288 × 1.5 = 0.00432 SOL
- New b = 0.00432/0.00156 = 2.77
- At p=0.25 (post dead-token filter): E = 0.25 × 0.00432 - 0.75 × 0.00156 = +0.000195/trade
- **Confirms +EV path**

**Config changes:**
- ADD: `momentum_lock_enabled: true`
- ADD: `momentum_lock_min_ws_5s: 1`
- CHANGE: `trailing_stop_accel_pct: 25` → replaced by tiered trail
- ADD: `trail_tiers: [[200, 100], [500, 200], [1500, 400], [5000, 800]]`
- ADD: `time_sl_skip_profitable: true`

---

# PART 4: CRITICAL PATH TO PROFITABILITY

## Minimum Changes for Break-Even

**If you can only build TWO things:**

1. **Sell pipeline reliability (E1)** — stops the -0.022 SOL/day bleed from stuck tokens
2. **Enforce 3s minimum hold (E2)** — transforms 0-1s spray trades into informed 3-5s trades

These two changes alone shift:
- Stuck token losses: -0.219 → ~-0.020 (recovery of ~0.200 SOL)
- Trade WR: 13.2% → ~20% (from actually holding 3s)
- Trade b: 1.85 → ~2.5 (from holding winners slightly longer)

At p=0.20, b=2.5: E = 0.20 - 0.80/2.5 = 0.20 - 0.32 = -0.12 → still slightly negative Kelly, but with reduced trade count the total loss shrinks dramatically.

**For full break-even, you need all of E1 + E2 + E3:**
- After dead token filter: ~106 trades, WR ~20.8%, b improving
- E[trade] approaches zero
- Stuck token bleed stops

**For positive EV, you need E1 + E2 + E3 + E5:**
- Winners held longer → b rises to ~3.0+
- At p=0.22, b=3.0: E = 0.22 - 0.78/3.0 = 0.22 - 0.26 = -0.04 (still slightly negative)
- At p=0.25, b=3.0: E = 0.25 - 0.75/3.0 = 0.25 - 0.25 = 0.00 (break-even)
- At p=0.25, b=4.0: E = 0.25 - 0.75/4.0 = 0.25 - 0.1875 = **+0.0625** (positive!)

**The math says: you need WR ≥ 25% AND b ≥ 3.0 to be consistently profitable.**
