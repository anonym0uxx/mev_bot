# Signal Detection & Entry Triggers — Quant Architect #1

## Executive Summary

The bot's biggest alpha comes from **re-entries on graduated tokens that are actively pumping** (paper trade data: 7qX4FYSS +0.233 SOL across 4 trades). Currently, the only entry path is graduation → momentum engine. This design adds a second entry path using the only data available for post-graduation PumpSwap pools.

### Critical Data Source Reality

| Source | Pre-Graduation | Post-Graduation (PumpSwap) |
|--------|---------------|---------------------------|
| **PumpPortal WS** | ✅ Every trade (mint, trader, sol_amount, is_buy, vsol_reserves) | ❌ Stops after graduation |
| **Helius WS accountSubscribe** | ❌ Not subscribed | ✅ Vault balance changes (reserve amounts only — no trader/direction/size) |
| **Helius RPC polling** | ❌ | ✅ getAccountInfo on vaults (500ms cadence, reserve amounts) |
| **CoreCast/Bitquery** | ✅ DEX trades, migrations | ⚠️ Sends historical migrations (stale), creator sells |
| **ShredStream** | ❌ | ✅ Detects PumpSwap pool creation only (not swaps) |

**PumpPortal does NOT stream PumpSwap trades.** Once a token graduates, PumpPortal's `vSolInBondingCurve` and `vTokensInBondingCurve` go to zero. All post-graduation price data comes from **Helius WS accountSubscribe** on the PumpSwap vault accounts (coin_vault, pc_vault).

This means signal detection for re-entries must be **reserve-based** (observing vault SOL inflows/outflows from Helius WS notifications), not **trade-based** (observing individual buys/sells).

---

## 1. Signal Definition

A **momentum re-entry signal** fires when a graduated token's PumpSwap pool shows sustained SOL inflow (price increase) after a period of quiescence — indicating a new wave of buying that is likely to produce ≥3% additional price movement.

### Available Observables (from Helius WS accountSubscribe)

Every vault balance change triggers an `accountNotification`. The price_feed module already decodes these into `reserve_sol` and `reserve_token` AtomicU64 values + increments `ws_notif_count` and `ws_notif_last_ms`.

| Observable | What It Tells Us |
|-----------|-----------------|
| `reserve_sol` delta over time | Net SOL flow direction (buy pressure = reserve_sol increases) |
| `ws_notif_count` delta per window | Trading activity intensity (each notification ≈ 1 swap) |
| `price_fp` delta (computed from reserves) | Actual price movement |
| `ws_notif_last_ms` freshness | How recently a swap occurred |

### Signal Components (reserve-based)

| Component | Source | What It Measures | Weight |
|-----------|--------|-----------------|--------|
| **Reserve Growth Rate** | `reserve_sol` sampled every 1s over 5s window | Net buying pressure (SOL flowing into pool) | 0.35 |
| **Notification Burst** | `ws_notif_count` delta in 5s vs 30s baseline | Sudden activity spike on a quiet pool | 0.25 |
| **Price Velocity** | `price_fp` samples over 3s window | Actual price acceleration | 0.25 |
| **Recovery Pattern** | Price vs entry price of last closed position | Re-entry at a lower price than last exit | 0.15 |

### Composite Signal Score (0–100)

```
signal_score = 
    reserve_growth_score   × 0.35   // 0-100: SOL flowing in over 5s
  + notif_burst_score      × 0.25   // 0-100: activity spike vs baseline
  + price_velocity_score   × 0.25   // 0-100: price acceleration
  + recovery_score         × 0.15   // 0-100: favorable re-entry price
```

**Entry threshold**: `signal_score ≥ 60` (tunable via config).

---

## 2. Detection Algorithm

### Architecture: Passive Monitoring via PriceFeedManager

The key insight is that we don't need to add new subscriptions. We need to **keep subscriptions alive after position close** for mints we want to re-enter.

Current flow:
```
graduation → subscribe(vault) → open position → close position → unsubscribe(vault)
                                                                  ^^^^^^^^^^^^^^^^
                                                                  DATA GOES DARK
```

New flow:
```
graduation → subscribe(vault) → open position → close position → keep subscription
                                                                  for re-entry window
                                                                  │
                                                                  ▼
                                                           signal_detector monitors
                                                           reserve_sol + ws_notif
                                                           changes on idle subscription
                                                                  │
                                                                  ▼ (signal fires)
                                                           open new position
                                                                  │
                                                                  ▼ (position closes)
                                                           keep subscription again...
                                                           (up to MAX_REENTRY_AGE)
                                                                  │
                                                                  ▼ (timeout)
                                                           unsubscribe(vault)
```

### Data Structure: `ReentryMonitor`

Tracks graduated mints that are eligible for re-entry, monitoring their vault reserve movements via the already-subscribed price feed.

```rust
/// Monitors a graduated token for re-entry signals.
/// Stored in a DashMap keyed by mint [u8; 32].
/// Total size: 256 bytes (cache-line aligned).
pub struct ReentryMonitor {
    pub mint: [u8; 32],
    
    // ── Reserve history: 1-second samples of reserve_sol ──
    // Ring buffer of last 60 seconds. Each entry = reserve_sol lamports.
    // Sampled from price_feed.get_reserve_sol() on each tick (50ms) but
    // only stored once per second.
    pub reserve_samples: [u64; 60],   // 480 bytes
    pub sample_head: u8,              // current write position (0-59)
    pub sample_count: u8,             // valid samples (0-60)
    pub last_sample_ts_s: u32,        // epoch seconds of last sample
    
    // ── Notification tracking ──
    pub notif_count_at_last_check: u64,  // ws_notif_count snapshot
    pub notif_baseline_5s: u16,          // EMA of notifications per 5s
    
    // ── Price tracking ──
    pub price_fp_at_close: u64,       // price when position last closed
    pub peak_price_fp: u64,           // highest price seen since close
    
    // ── State ──
    pub graduated_ts_ms: u64,         // when this token graduated
    pub last_close_ts_ms: u64,        // when the last position closed
    pub last_signal_ts_ms: u64,       // cooldown: prevent rapid re-firing
    pub total_entries: u8,            // lifetime entries on this mint
    pub coin_vault: [u8; 32],         // cached for re-subscription
    pub pc_vault: [u8; 32],           // cached for re-subscription
    pub pool_account: [u8; 32],       // PumpSwap pool account (for live execution)
    pub consecutive_quiet_s: u16,     // seconds with <2 notifications (death detection)
}
```

### Pseudocode: Signal Detection (runs on Tick)

The signal detector runs every 1 second (every 20th tick at 50ms interval), reading from the **already-existing** price feed's atomic values.

```
fn check_reentry_signals(
    monitors: &DashMap<[u8;32], ReentryMonitor>,
    price_feed: &PriceFeedManager,
    now_ms: u64,
    signal_tx: &Sender<MomentumSignal>,
):
    let now_s = now_ms / 1000
    
    for mut entry in monitors.iter_mut():
        let mint = *entry.key()
        let mon = entry.value_mut()
        
        // ── Sample reserves (1/sec) ──
        if now_s != mon.last_sample_ts_s:
            let reserve_sol = match price_feed.get_reserve_sol(&mint):
                Some(r) if r > 0 => r,
                _ => continue  // subscription not delivering data
            
            mon.reserve_samples[mon.sample_head as usize] = reserve_sol
            mon.sample_head = (mon.sample_head + 1) % 60
            if mon.sample_count < 60: mon.sample_count += 1
            mon.last_sample_ts_s = now_s as u32
        
        // ── Check for re-entry signal ──
        // Need at least 5 samples (5 seconds of data)
        if mon.sample_count < 5:
            continue
        
        // Cooldown: don't re-fire within 30s of last signal
        if now_ms - mon.last_signal_ts_ms < SIGNAL_COOLDOWN_MS:
            continue
        
        // Cooldown: don't re-enter within 60s of last position close
        if now_ms - mon.last_close_ts_ms < REENTRY_COOLDOWN_MS:
            continue
        
        // Max entries per mint
        if mon.total_entries >= MAX_ENTRIES_PER_MINT:
            continue
        
        // ── Compute signal score ──
        let score = compute_reentry_score(mon, &price_feed, &mint, now_ms)
        
        if score >= SIGNAL_THRESHOLD:
            signal_tx.try_send(MomentumSignal {
                mint,
                score,
                reserve_sol_now: current_reserve,
                price_fp_now: current_price,
                ts_ms: now_ms,
                coin_vault: mon.coin_vault,
                pc_vault: mon.pc_vault,
                pool_account: mon.pool_account,
                entry_number: mon.total_entries + 1,
            })
            mon.last_signal_ts_ms = now_ms
```

### Pseudocode: Score Computation

```
fn compute_reentry_score(
    mon: &ReentryMonitor,
    price_feed: &PriceFeedManager,
    mint: &[u8; 32],
    now_ms: u64,
) -> u32:
    
    // ── Reserve Growth Rate (0-100, weight 0.35) ──
    // Compare reserve_sol now vs. 5s ago
    let head = mon.sample_head as usize
    let n = mon.sample_count as usize
    let r_now = mon.reserve_samples[(head + 59) % 60]  // most recent
    let r_5s  = mon.reserve_samples[(head + 60 - 5) % 60]  // 5 seconds ago
    
    if r_5s == 0 || r_now == 0:
        return 0  // no valid data
    
    // Positive growth = SOL flowing in = people buying
    let growth_bps = ((r_now as i64 - r_5s as i64) * 10000 / r_5s as i64)
    
    let reserve_growth_score = match growth_bps:
        >= 500  => 100  // +5% reserve growth in 5s = massive buy pressure
        >= 300  => 85   // +3% = strong
        >= 150  => 65   // +1.5% = moderate
        >= 50   => 40   // +0.5% = emerging
        >= 0    => 10   // flat or barely positive
        < 0     => 0    // reserves declining = sells dominating
    
    // ── Notification Burst (0-100, weight 0.25) ──
    // Compare recent notification rate to baseline
    let (ws_count_now, ws_last_ms) = price_feed.ws_notif_info(mint)
    let ws_delta_5s = ws_count_now.saturating_sub(mon.notif_count_at_last_check)
    
    // Update baseline EMA (α = 0.2, ~5s half-life)
    // notif_baseline_5s is in units of "notifs per 5s × 100" (fixed point)
    let ema = mon.notif_baseline_5s as u64
    let new_ema = (ema * 80 + ws_delta_5s * 100 * 20) / 100
    mon.notif_baseline_5s = new_ema.min(65535) as u16
    mon.notif_count_at_last_check = ws_count_now
    
    let baseline = mon.notif_baseline_5s.max(100) as u64  // min 1 notif/5s baseline
    let burst_ratio = (ws_delta_5s * 10000) / (baseline / 100).max(1)
    
    let notif_burst_score = match burst_ratio:
        >= 500  => 100  // 5x baseline activity
        >= 300  => 80   // 3x baseline
        >= 200  => 60   // 2x baseline
        >= 150  => 40   // 1.5x baseline
        _ => 0
    
    // Hard floor: need at least 3 notifications in last 5s
    // (each notif ≈ 1 swap, need enough activity to matter)
    if ws_delta_5s < 3:
        notif_burst_score = min(notif_burst_score, 20)
    
    // ── Price Velocity (0-100, weight 0.25) ──
    // Use price_fp from price feed (already computed from reserves)
    let price_now = price_feed.current_price(mint).unwrap_or(0)
    if price_now == 0:
        return 0
    
    // Price 3s ago: use reserve samples to reconstruct
    let r_3s = mon.reserve_samples[(head + 60 - 3) % 60]
    let rt_3s = /* approximate from constant-product: reserve_token ∝ 1/reserve_sol */
    // Simpler: just use the price_fp delta from price feed cache
    // We need to store price snapshots too, or derive from reserves
    
    // Alternative: use reserve ratio as proxy for price
    // price ∝ reserve_sol / reserve_token, and reserve_token ∝ k / reserve_sol
    // So price ∝ reserve_sol², meaning reserve growth of x% → price growth of ~2x%
    let price_growth_bps = growth_bps * 2  // approximate: price is quadratic in reserve_sol
    
    let price_velocity_score = match price_growth_bps:
        >= 800  => 100  // +8% price move in 5s
        >= 500  => 85
        >= 300  => 65   // +3% = breakeven zone
        >= 150  => 40
        _ => 0
    
    // ── Recovery Pattern (0-100, weight 0.15) ──
    // Bonus if current price is lower than where we last exited
    // (re-entering at a discount = better risk/reward)
    let recovery_score = if mon.price_fp_at_close > 0 && price_now > 0:
        let discount_bps = (mon.price_fp_at_close as i64 - price_now as i64) * 10000
                           / mon.price_fp_at_close as i64
        match discount_bps:
            >= 2000 => 100  // 20% below last exit = great re-entry
            >= 1000 => 80
            >= 500  => 60
            >= 0    => 30   // at or below last exit
            < 0     => 0    // above last exit = chasing
    else:
        50  // no prior exit data = neutral
    
    // ── Composite ──
    let total = (reserve_growth_score * 35 
               + notif_burst_score * 25 
               + price_velocity_score * 25 
               + recovery_score * 15) / 100
    
    // ── Hard floors (must pass ALL) ──
    // 1. Reserve must be > 20 SOL (liquidity floor)
    if r_now < 20_000_000_000: return 0
    
    // 2. Must have positive reserve growth (net buying)
    if growth_bps <= 0: return 0
    
    // 3. Must have at least 3 notifications in 5s (real activity)
    if ws_delta_5s < 3: return 0
    
    return total
```

---

## 3. Integration Point

### Strategy: Extend MomentumEngine, Don't Add a Separate Module

The signal detector should live **inside** MomentumEngine as a method called on tick, not as a separate component. This avoids:
- New channels and synchronization
- Duplicating price feed access
- Complex lifecycle management

### Changes to MomentumEngine

```rust
// Add to MomentumEngine struct:
pub struct MomentumEngine {
    // ... existing fields ...
    
    /// Mints being monitored for re-entry signals.
    /// Populated when a position closes (if reentry_enabled).
    /// Evicted after max_reentry_monitor_age_ms.
    reentry_monitors: DashMap<[u8; 32], ReentryMonitor>,
}
```

### Modified Position Close Flow

Currently in `close_position()`:
```rust
// Current: unsubscribe immediately
self.price_feed.unsubscribe_sync(&mint);
```

New behavior:
```rust
fn close_position(...) {
    // ... existing P&L / logging ...
    
    if self.config.signal.reentry_enabled 
        && pos.total_entries < self.config.signal.max_entries_per_mint
        && now_ms - pos.graduated_ts_ms < self.config.signal.max_reentry_monitor_age_ms
    {
        // DON'T unsubscribe — keep monitoring for re-entry
        self.reentry_monitors.insert(mint, ReentryMonitor {
            mint,
            coin_vault: /* from cached pool data */,
            pc_vault: /* from cached pool data */,
            pool_account: /* from cached pool data */,
            price_fp_at_close: exit_price_fp,
            last_close_ts_ms: now_ms,
            graduated_ts_ms: pos.graduated_ts_ms_or_entry,
            total_entries: pos.reentry_count + 1,
            // ... initialize other fields ...
        });
        tracing::info!(
            mint = %bs58::encode(&mint).into_string(),
            entries = pos.reentry_count + 1,
            "[momentum] position closed — monitoring for re-entry"
        );
    } else {
        // Normal: unsubscribe
        self.price_feed.unsubscribe_sync(&mint);
    }
}
```

### Modified `on_tick()` Flow

```rust
pub async fn on_tick(&self, now_ms: u64) {
    // ... existing throttle, drain scored tokens ...
    
    self.process_pending_entries(now_ms).await;
    self.process_active_positions(now_ms);
    self.process_probe_evaluation(now_ms);
    self.process_scale_in(now_ms);
    
    // NEW: Check re-entry monitors (1/sec, not every tick)
    if now_ms / 1000 != self.last_tick_ms.load(Ordering::Relaxed) / 1000 {
        self.check_reentry_signals(now_ms).await;
    }
}
```

### New Method: `check_reentry_signals()`

```rust
impl MomentumEngine {
    /// Check all re-entry monitors for momentum signals.
    /// Called once per second from on_tick().
    /// Removes monitors that are too old, dead, or maxed out.
    #[cold]
    #[inline(never)]
    async fn check_reentry_signals(&self, now_ms: u64) {
        if !self.config.signal.reentry_enabled { return; }
        
        let mut to_signal: Vec<([u8; 32], u32)> = Vec::new();
        let mut to_remove: Vec<[u8; 32]> = Vec::new();
        
        for mut entry in self.reentry_monitors.iter_mut() {
            let mint = *entry.key();
            let mon = entry.value_mut();
            
            // Age-out: remove monitors older than max age
            if now_ms - mon.graduated_ts_ms > self.config.signal.max_reentry_monitor_age_ms {
                to_remove.push(mint);
                continue;
            }
            
            // Death detection: if no notifications for 5 minutes, token is dead
            let (ws_count, ws_last_ms) = self.price_feed.ws_notif_info(&mint);
            if ws_last_ms > 0 && now_ms - ws_last_ms > 300_000 {
                mon.consecutive_quiet_s += 1;
                if mon.consecutive_quiet_s > self.config.signal.dead_token_threshold_s {
                    to_remove.push(mint);
                    continue;
                }
            } else {
                mon.consecutive_quiet_s = 0;
            }
            
            // Don't signal if already in a position for this mint
            if self.active.contains_key(&mint) { continue; }
            
            // Cooldown checks
            if now_ms - mon.last_signal_ts_ms < self.config.signal.per_mint_cooldown_ms { continue; }
            if now_ms - mon.last_close_ts_ms < self.config.reentry_cooldown_ms { continue; }
            if mon.total_entries >= self.config.signal.max_entries_per_mint { continue; }
            
            // Concurrent position limit (shared with graduation entries)
            if self.active.len() >= self.config.max_concurrent as usize { break; }
            
            // Sample reserve
            if let Some(reserve_sol) = self.price_feed.get_reserve_sol(&mint) {
                let now_s = (now_ms / 1000) as u32;
                if now_s != mon.last_sample_ts_s {
                    let idx = mon.sample_head as usize;
                    mon.reserve_samples[idx] = reserve_sol;
                    mon.sample_head = ((mon.sample_head + 1) % 60) as u8;
                    if mon.sample_count < 60 { mon.sample_count += 1; }
                    mon.last_sample_ts_s = now_s;
                }
            }
            
            // Need at least 5 samples
            if mon.sample_count < 5 { continue; }
            
            // Compute signal score
            let score = self.compute_reentry_score(mon, &mint, now_ms);
            
            if score >= self.config.signal.signal_threshold {
                to_signal.push((mint, score));
                mon.last_signal_ts_ms = now_ms;
            }
        }
        
        // Remove dead monitors (releases price feed subscriptions)
        for mint in to_remove {
            self.reentry_monitors.remove(&mint);
            if !self.active.contains_key(&mint) {
                self.price_feed.unsubscribe_sync(&mint);
            }
        }
        
        // Process signals → schedule entries
        for (mint, score) in to_signal {
            self.execute_reentry(mint, score, now_ms).await;
        }
    }
    
    /// Execute a re-entry: resolve pool (or use cached), schedule pending entry.
    async fn execute_reentry(&self, mint: [u8; 32], score: u32, now_ms: u64) {
        // Daily loss cap check (reuse from on_graduation)
        let daily_pnl = self.daily_pnl_lamports.load(Ordering::Relaxed);
        if daily_pnl < 0 {
            let wallet_bal = self.wallet_balance_lamports.load(Ordering::Relaxed);
            let effective_balance = if wallet_bal == u64::MAX { 1_500_000_000u64 } else { wallet_bal };
            let cap_lamports = (effective_balance as f64 * self.config.daily_loss_cap_pct) as i64;
            if daily_pnl.unsigned_abs() >= cap_lamports as u64 {
                return; // daily cap hit
            }
        }
        
        // ToD gating
        let tod_multiplier = crate::momentum::tod::entry_size_multiplier(
            &self.config.tod_config, now_ms,
        );
        if tod_multiplier <= 0.0 { return; }
        
        // Get current price for entry
        let current_price_fp = match self.price_feed.current_price(&mint) {
            Some(p) if p > 0 => p,
            _ => return,
        };
        
        // Get monitor data for pool vaults
        let monitor = match self.reentry_monitors.get(&mint) {
            Some(m) => m,
            None => return,
        };
        
        let mint_b58 = bs58::encode(&mint).into_string();
        tracing::info!(
            mint = %mint_b58,
            score,
            entry_number = monitor.total_entries + 1,
            price_fp = current_price_fp,
            "[momentum] re-entry signal FIRED"
        );
        
        // Ensure PumpSwap pool accounts are cached for live execution
        if !self.pumpswap_pools.contains_key(&mint) {
            // Re-resolve pool (pool accounts may have been cleaned up after last close)
            if let Some(resolution) = crate::momentum::pool::resolve_pumpswap_pool_from_mint(
                &self.http_client, &mint, &self.public_rpc_url, &self.helius_rpc_url,
            ).await {
                if let Some(ps_pool) = crate::momentum::pool::extract_pumpswap_pool_accounts(&resolution) {
                    let ps_accts: crate::tx::pumpswap::PumpSwapPoolAccounts = ps_pool.into();
                    self.pumpswap_pools.insert(mint, ps_accts);
                }
            } else {
                tracing::warn!(mint = %mint_b58, "[momentum] re-entry: pool resolution failed");
                return;
            }
        }
        
        // Schedule entry with shorter delay (momentum already confirmed)
        let entry = PendingEntry {
            mint,
            pool_type: 1, // PumpSwap
            grad_score: score as u8,
            grad_speed_s: 0, // N/A for re-entry
            grad_volume_sol_x100: 0, // N/A for re-entry
            pre_grad_buys_5s: 0,
            scheduled_ts_ms: now_ms + self.config.signal.reentry_delay_ms,
            opening_price_fp: current_price_fp,
            bc_price_fp: 0, // N/A for re-entry
            first_scheduled_ts_ms: now_ms,
            recovery_score: 0,
            active: true,
        };
        
        if let Ok(mut ring) = self.pending.lock() {
            ring.push(entry);
        }
        
        // Update monitor
        if let Some(mut mon) = self.reentry_monitors.get_mut(&mint) {
            mon.total_entries += 1;
        }
    }
}
```

---

## 4. Filtering: Avoiding Bad Entries

### 4a. Dead Token Detection

```rust
// In check_reentry_signals():
// Helius WS silence = no swaps happening in the pool.
// Track via ws_notif_last_ms (already maintained by price_feed).
// If no notification in 5 minutes → mark as dead, remove monitor.
const DEAD_TOKEN_SILENCE_MS: u64 = 300_000; // 5 minutes

// Also: reserve below floor = drained / rugged
const MIN_REENTRY_RESERVE_SOL: u64 = 15_000_000_000; // 15 SOL
// Applied in compute_reentry_score() as hard floor
```

### 4b. Wash Trading Detection

```rust
// Reserve-based wash detection:
// Wash trading = repeated buy+sell by same wallet → reserves oscillate
// around the same level without net growth.
// 
// Detection: if reserve_sol oscillates (max - min) > 5% but
// net change over 30s is < 1% → wash pattern
fn detect_wash_pattern(mon: &ReentryMonitor) -> bool {
    if mon.sample_count < 30 { return false; }
    
    let recent_30 = &mon.reserve_samples[..30]; // last 30 samples
    let max_r = recent_30.iter().copied().max().unwrap_or(0);
    let min_r = recent_30.iter().copied().min().unwrap_or(0);
    let first_r = recent_30[0];
    let last_r = recent_30[29];
    
    // High oscillation
    let oscillation_bps = if min_r > 0 { (max_r - min_r) * 10000 / min_r } else { 0 };
    // Low net change
    let net_bps = if first_r > 0 {
        ((last_r as i64 - first_r as i64).abs() as u64 * 10000) / first_r
    } else { 0 };
    
    oscillation_bps > 500 && net_bps < 100 // >5% oscillation, <1% net = wash
}
```

### 4c. Low-Liquidity Gate

```rust
// Hard floor in compute_reentry_score():
if reserve_sol_now < config.signal.min_vsol_reserves {
    return 0; // Not enough liquidity
}

// Default: 15 SOL (lower than graduation gate of 30 SOL because
// we already validated this pool at graduation)
```

### 4d. Graduation Recency + Max Entries

```rust
// Age-out: stop monitoring after max_reentry_monitor_age_ms (default: 2h)
// After 2 hours, even the best memecoins have moved on to the next hype cycle.

// Max entries: cap at 5 lifetime entries per mint (default)
// Paper data shows 4-6 entries per winning mint is the sweet spot.
// Beyond that, you're overfit to one token.
```

### 4e. "Chasing" Prevention

```rust
// Don't re-enter if price is significantly ABOVE the last exit price.
// Chasing a pump that already moved = buying the top.
// recovery_score component handles this:
//   - Above last exit by 10%+ → recovery_score = 0
//   - At or below last exit → recovery_score = 30-100
//
// Additionally: require positive reserve growth as a hard floor
// (growth_bps > 0 in compute_reentry_score)
```

---

## 5. Rate Limiting

### Signal Emission Rates

| Layer | Limit | Mechanism |
|-------|-------|-----------|
| Per-mint cooldown | 1 signal per 30s | `last_signal_ts_ms` in ReentryMonitor |
| Per-mint entry cooldown | Reuse `reentry_cooldown_ms` (60s) | Existing MomentumConfig |
| Global check frequency | 1/sec (every 20th tick at 50ms) | Tick counter modulo in `on_tick()` |
| Active monitor limit | Max 100 monitors | LRU eviction when `reentry_monitors.len() > 100` |
| Pool resolution | ~0 extra calls | Vaults cached from graduation; re-resolve only if pumpswap_pools was cleaned up |
| Active position limit | Shared pool with graduation entries | Existing `max_concurrent` |
| Helius WS subscriptions | +0-10 at steady state | Kept alive from position close, not new subscriptions |

### Helius WS Subscription Budget

**Concern**: Keeping vault subscriptions alive after position close consumes Helius WS subscription slots.

**Mitigation**:
- Cap at 100 monitors × 2 vaults = 200 subscriptions max
- Helius WS can handle 1000+ subscriptions per connection
- Aggressive eviction: dead tokens (5min silence), aged-out (2h), maxed-out entries
- At steady state with ~10 graduations/day and 2h monitor window, expect ~10-20 active monitors

### Pool Resolution Budget

**Critical optimization**: Re-entries require **zero additional RPC calls** for pool resolution in the common case:
1. Vault addresses are cached in `ReentryMonitor` (from graduation)
2. Price feed subscription stays active (no re-subscribe needed)
3. PumpSwap pool accounts may need re-resolution (one `getProgramAccounts` call per re-entry)

Worst case: 10 re-entries/hour × 1 RPC call each = 10 extra calls/hour (negligible vs current 60/min budget).

---

## 6. Specific Rust Changes

### New Files

| File | Purpose | ~Lines |
|------|---------|--------|
| `src/momentum/signal_detector.rs` | `ReentryMonitor`, `compute_reentry_score()`, `check_reentry_signals()`, `execute_reentry()` | ~350 |
| `src/momentum/signal_types.rs` | `MomentumSignal` struct, `SignalConfig` with serde defaults | ~120 |

### Modified Files

| File | Change | Diff Size | Risk |
|------|--------|-----------|------|
| `src/momentum/mod.rs` | Add `pub mod signal_detector; pub mod signal_types;` + Add `reentry_monitors: DashMap` to `MomentumEngine` struct + Wire in `new()` | ~20 lines | Low |
| `src/momentum/config.rs` | Add `SignalConfig` section to `MomentumConfig` with serde defaults | ~50 lines | Low (additive, all defaulted) |
| `src/momentum/mod.rs` (close_position) | Conditional: keep subscription alive + create ReentryMonitor instead of unsubscribe | ~30 lines | **Medium** — modifies the close path |
| `src/momentum/mod.rs` (on_tick) | Add `check_reentry_signals()` call (1 line + gated on 1/sec) | ~5 lines | Low |
| `src/main.rs` | No changes needed — all wiring is internal to MomentumEngine | 0 lines | None |

### New Config (added to `MomentumConfig`)

```rust
// In src/momentum/config.rs

/// Re-entry signal detection configuration.
/// Nested under MomentumConfig.signal in canary.json.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SignalConfig {
    /// Master toggle. Must be true for re-entry monitoring.
    pub reentry_enabled: bool,                  // default: false
    /// Minimum signal score (0-100) to trigger re-entry.
    pub signal_threshold: u32,                  // default: 60
    /// Cooldown between signals on same mint (ms).
    pub per_mint_cooldown_ms: u64,              // default: 30_000
    /// Delay between signal fire and entry (ms).
    pub reentry_delay_ms: u64,                  // default: 500
    /// Probe size for re-entries (SOL).
    pub reentry_probe_size_sol: f64,            // default: 0.08
    /// Maximum time to monitor a graduated token (ms).
    pub max_reentry_monitor_age_ms: u64,        // default: 7_200_000 (2h)
    /// Maximum concurrent monitors.
    pub max_monitors: usize,                    // default: 100
    /// Maximum lifetime entries per mint.
    pub max_entries_per_mint: u8,               // default: 5
    /// Minimum pool reserve SOL for signal (lamports).
    pub min_vsol_reserves: u64,                 // default: 15_000_000_000 (15 SOL)
    /// Seconds of WS silence before declaring token dead.
    pub dead_token_threshold_s: u16,            // default: 300 (5 min)
    /// Minimum reserve growth (bps over 5s) to be a valid signal.
    pub min_reserve_growth_bps: i32,            // default: 50 (+0.5%)
    /// Minimum WS notifications in 5s window.
    pub min_notifs_5s: u16,                     // default: 3
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            reentry_enabled: false,
            signal_threshold: 60,
            per_mint_cooldown_ms: 30_000,
            reentry_delay_ms: 500,
            reentry_probe_size_sol: 0.08,
            max_reentry_monitor_age_ms: 7_200_000,
            max_monitors: 100,
            max_entries_per_mint: 5,
            min_vsol_reserves: 15_000_000_000,
            dead_token_threshold_s: 300,
            min_reserve_growth_bps: 50,
            min_notifs_5s: 3,
        }
    }
}
```

### Position Metadata Extension

The `MomentumPosition` struct needs a small addition to track re-entry count:

```rust
// In src/momentum/position.rs
// Option A: Use one of the existing _pad fields (there's _pad2 with some free bytes)
// Option B: Add to the position's metadata that's stored outside the 256-byte struct

// Recommended: track in ReentryMonitor (already has total_entries)
// and pass it through PendingEntry → MomentumPosition at entry time.
// Use the grad_speed_s field (meaningless for re-entries, always 0)
// to encode entry_number for logging purposes.
```

### JSONL Logger Extension

Add a `entry_type` field to distinguish graduation entries from re-entries:

```rust
// In MomentumClosedPosition:
pub entry_type: &'static str,  // "graduation" | "reentry"
pub entry_number: u8,          // 1 for first entry, 2+ for re-entries
```

---

## 7. Performance Budget

| Operation | Cost | Frequency | Total |
|-----------|------|-----------|-------|
| check_reentry_signals() full scan | ~50μs for 100 monitors | 1/sec | 50μs/s |
| Sample reserve_sol per monitor | ~100ns (atomic load) | 1/sec × 100 | 10μs/s |
| compute_reentry_score() | ~500ns | 1/sec × ~10 active | 5μs/s |
| execute_reentry() (signal fires) | ~10μs (schedule entry) | ~0.01/s | 0.1μs/s |
| **Total hot-path overhead** | | | **~65μs/s** |

This is ~0.0065% of a 1-second budget. Negligible impact on the 50ms tick loop.

### Memory Budget

- ReentryMonitor: ~1.2KB per tracked mint (including 60-sample ring)
- Max 100 tracked mints: **~120KB total**
- DashMap overhead: ~32 bytes per entry = 3.2KB
- **Total: ~125KB** — fits in L1 cache

### Helius WS Impact

- Each kept-alive subscription: ~0 additional bandwidth (notifications only fire on swaps)
- Dead tokens generate 0 notifications = 0 bandwidth
- Active tokens: ~1-10 notifications/sec × ~200 bytes each = ~2KB/s per monitor
- 100 monitors: ~200KB/s worst case (all active) — well within WS capacity

---

## 8. Implementation Order

### Phase 1: Infrastructure (no new entries yet)
1. Add `SignalConfig` to `MomentumConfig` with `reentry_enabled: false`
2. Add `reentry_monitors: DashMap` to `MomentumEngine`
3. Modify `close_position()` to conditionally keep subscriptions alive
4. Add `check_reentry_signals()` stub that only logs monitored mints

### Phase 2: Signal Detection (paper mode only)
5. Implement `ReentryMonitor` struct and `compute_reentry_score()`
6. Add scoring logic with all components
7. Log signals to `momentum_reentry_signals.jsonl` (no actual entries)
8. Run for 24-48h to calibrate thresholds

### Phase 3: Entry Execution
9. Implement `execute_reentry()` with pending entry scheduling
10. Wire through existing PendingEntry → process_pending_entries → position open
11. Paper trade for 48h, compare to baseline

### Phase 4: Live Mode
12. Enable in config: `signal.reentry_enabled: true`
13. Start with conservative settings: `signal_threshold: 75`, `reentry_probe_size_sol: 0.05`
14. Monitor for 24h, tune thresholds

---

## 9. Risk Analysis

### What Could Go Wrong

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| False signals on dead tokens | Medium | Low (small probe) | WS silence detection, reserve floor |
| Helius WS subscription overload | Low | Medium | Cap at 100 monitors, aggressive eviction |
| Re-entering a rug in progress | Low | Medium | Reserve growth hard floor (must be positive), drain detection in existing position management |
| Over-trading one token | Medium | Low | max_entries_per_mint cap (5) |
| Wash trading false positive | Medium | Low | Wash pattern detection, unique buyer proxy via notif rate |
| Price feed stale during re-entry | Low | Medium | Existing `no_price_timeout_ms` handles this |

### Key Advantage: Reuse of Existing Infrastructure

This design adds **zero new data sources**, **zero new connections**, and **minimal new code paths**. It piggybacks on:
- Existing PriceFeedManager (already has WS + RPC polling)
- Existing PendingEntry → position management pipeline
- Existing drain detection, trailing stops, exit logic
- Existing pool resolution (one extra call per re-entry max)

The only "new" thing is keeping vault subscriptions alive longer and reading their data to detect momentum patterns.

---

## 10. Expected Impact (Based on Paper Trade Data)

From the 72-trade dataset, re-entries on winning mints averaged:
- **7qX4FYSS**: +0.058 SOL/entry (4 entries)
- **5mDUVMi3**: +0.013 SOL/entry (6 entries)  
- **GPCCD1j7**: +0.017 SOL/entry (4 entries)

At 0.08 SOL probe size with 60% win rate and +3% average winner:
- Expected per-signal: `0.60 × 0.08 × 0.03 - 0.40 × 0.08 × 0.02 = +0.0008 SOL`
- At 5 signals/day: **+0.004 SOL/day** conservatively
- At 10 signals/day with scale-in on winners: **+0.02-0.05 SOL/day**

This is modest but pure alpha — it comes from the same tokens that already proved profitable, entered at moments of renewed momentum rather than graduation timing.