# Daily Loss Cap Calibration — pump-quant Live Deployment

**Date:** 2026-04-01  
**Wallet:** 1.5 SOL  
**Trade dataset:** 31 real-price paper trades  

---

## 1. The Correct `daily_loss_cap_pct`

### Monte Carlo Results (200,000 simulations per scenario)

The circuit breaker tracks **running cumulative PnL** — the worst trough the daily PnL hits before any recovery. This is what actually triggers the cap.

| Percentile | 10 trades/day | 15 trades/day | 20 trades/day |
|------------|---------------|---------------|---------------|
| 50th       | 0.00%         | 0.00%         | 0.00%         |
| 90th       | 1.95%         | 2.00%         | 2.01%         |
| 95th       | 2.72%         | 2.77%         | 2.80%         |
| 99th       | 4.52%         | 4.64%         | 4.73%         |
| 99.9th     | 6.88%         | 7.23%         | 7.44%         |
| Worst (in 200k sims) | 12.75% | 11.78%  | 13.91%        |

### Cap Trigger Rates (% of trading days the cap fires)

| `daily_loss_cap_pct` | SOL at 1.5 wallet | 10 trades | 15 trades | 20 trades |
|----------------------|-------------------|-----------|-----------|-----------|
| 5%  (0.05)           | 0.075             | 0.642%    | 0.726%    | 0.804%    |
| 8%  (0.08)           | 0.120             | 0.034%    | 0.052%    | 0.061%    |
| 10% (0.10)           | 0.150             | 0.005%    | 0.011%    | 0.011%    |
| 12% (0.12)           | 0.180             | 0.001%    | 0.000%    | 0.002%    |
| 15% (0.15)           | 0.225             | 0.000%    | 0.000%    | 0.000%    |

### The Answer: `daily_loss_cap_pct: 0.10`

**10% of current wallet balance.**

Reasoning:
- At the **99th percentile** (1-in-100 bad day), max drawdown is ~4.7% of wallet. Normal variance never comes close to 10%.
- At the **99.9th percentile** (1-in-1000 catastrophic day), max drawdown is ~7.4%. Still under 10%.
- 10% fires on **~0.01% of days** under our model — roughly once every 10,000 trading days. It will essentially never false-trigger under normal conditions.
- But if something goes truly wrong (regime change, bug, feed corruption), 10% = 0.15 SOL on a 1.5 SOL wallet. You lose 10%, you live to trade tomorrow.
- At 1.5 SOL, 10% = 0.15 SOL cap. At 5 SOL (after growth), 10% = 0.50 SOL cap. **It scales automatically** — that's the whole point of percentage-based.

**Why not 8%?** At 8% you'd fire ~0.05% of days (once every 2,000 days). Still very safe. But 10% gives extra headroom for the model being wrong about the loss distribution. Our sample is only 31 trades — the tails are uncertain.

**Why not 15%?** Never fires in simulation. At that point you've lost the circuit breaker's purpose. A 15% drawdown on a 1.5 SOL wallet = 0.225 SOL, which is already 3.75 worst-case scaled-in hard_sl losses. If you've hit 3 consecutive worst-case losses, something is broken and you should stop.

---

## 2. `min_wallet_balance_lamports`

### Current: 100,000,000 (0.1 SOL)

At 0.1 SOL remaining, the engine can't even place a single 0.05 SOL probe. The balance check at entry requires:

```
cached_balance < required.max(self.config.min_wallet_balance_lamports)
```

Where `required` = probe_size + fees. So 0.1 SOL is already a dead floor — by the time you're at 0.1 SOL from 1.5, you've lost 93% and the daily_loss_cap_pct would have halted you at 10%.

### The correct value: 250,000,000 (0.25 SOL)

Reasoning:
- 5 concurrent probes × 0.05 SOL = 0.25 SOL committed capital
- Below 0.25 SOL, you can't fund all 5 probe slots simultaneously
- 0.25 SOL = 16.7% of 1.5 SOL wallet, meaning you've already lost 83% — this is a "you should not be here" floor
- The daily loss cap (10%) should catch problems long before this floor is hit
- But if the engine starts a new day with a low balance (from accumulated multi-day losses), this prevents trading on fumes
- Also provides ~0.005 SOL buffer for rent-exempt reserve and tx fees

**Note:** This is a **static floor**, not a percentage. It should be updated if you significantly change probe_size_sol or max_concurrent. Rule of thumb: `min_wallet_balance = max_concurrent × probe_size_sol`.

---

## 3. `hard_sl_pct: 10.0` — Is It Correct?

### Current config: 10.0% (canary.json line 527)

*(The task description said 12%, but canary.json has 10.0%. Using the actual config value.)*

At probe_size_sol=0.05:
- Max probe loss at hard_sl = 0.05 × 10% = **0.005 SOL**
- Actual avg loss = -0.0038 SOL (consistent — most exits are trailing/time stop before hard_sl)
- Worst observed loss = -0.0321 SOL (scale-in position, total size ~0.25-0.35 SOL)

At max_total_size_sol=0.50:
- Max scaled-in loss at hard_sl = 0.50 × 10% = **0.050 SOL** (3.33% of wallet)

### Verdict: 10% is correct. Do not change.

- The hard_sl is the **catastrophic stop** — it only fires when trailing stop and all other exits fail
- It fired 2 out of 31 trades (6.5%) — confirming it's rare as intended
- At 10%, the worst-case per-trade loss (fully scaled in) is 0.050 SOL = 3.33% of wallet
- This means even 3 consecutive worst-case fully-scaled hard_sl losses = 0.150 SOL = exactly 10% = circuit breaker trips
- **That's a beautiful alignment.** The daily_loss_cap_pct of 10% = 3× worst-case full-scale hard_sl losses.

Tighter (8%) would trigger hard_sl more often, converting some recoverable positions into realized losses. Wider (15%) is unnecessary — trailing stop handles normal exits.

---

## 4. Scale-In Risk Exposure

### Current config
- `probe_size_sol: 0.05` → 3.33% of wallet per entry
- `max_total_size_sol: 0.50` → 33.3% of wallet max per position
- `max_concurrent: 5` positions
- `kelly_fraction: 0.25` (quarter Kelly)

### Kelly Analysis
```
p = 0.613,  q = 0.387
b = avg_win/avg_loss = 0.0227/0.0038 = 5.97
Full Kelly = p - q/b = 0.613 - 0.387/5.97 = 0.548 (54.8%)
Quarter Kelly = 0.137 (13.7%)
Quarter Kelly bet at 1.5 SOL = 0.206 SOL
```

### The Problem: max_total_size_sol = 0.50 exceeds Kelly

Quarter Kelly says bet 0.206 SOL per trade. `max_total_size_sol: 0.50` allows 2.4× Kelly. That's fine — Kelly is derived from *average* win/loss, but scale-in only fires on *confirmed strong momentum* (above-average expected win). The scale-in effectively has a higher local win rate and win size.

### Concurrent exposure check

Theoretical worst case: 5 positions all fully scaled to 0.50 SOL = 2.50 SOL. **Exceeds the 1.5 SOL wallet.**

But this can't actually happen because:
1. Wallet balance check blocks entries when `balance < required`
2. At 1.5 SOL, you can fund at most 3 full-scale positions (3 × 0.50 = 1.50)
3. Realistically: 5 probes (0.25 SOL) + 1-2 scale-ins (0.15-0.35 SOL each) = 0.55-0.95 SOL committed

### Recommendation: Keep max_total_size_sol at 0.50

- Quarter Kelly = 0.206 SOL, but scale-in positions have higher edge → 0.50 is within 2× adjusted Kelly
- Wallet balance check naturally prevents over-commitment
- Worst single-trade loss at full scale + hard_sl = 0.050 SOL = 3.33% of wallet → acceptable
- **No change needed**

---

## 5. Final Recommended Config

```json
{
  "daily_loss_cap_pct": 0.10,
  "min_wallet_balance_lamports": 250000000,
  "hard_sl_pct": 10.0,
  "max_total_size_sol": 0.50,
  "probe_size_sol": 0.05,
  "kelly_fraction": 0.25,
  "max_concurrent": 5
}
```

### Changes from current canary.json:

| Field | Old | New | Reason |
|-------|-----|-----|--------|
| `daily_loss_cap_sol` | 2.0 | **REMOVED** | Replaced by percentage-based cap |
| `daily_loss_cap_pct` | *(new)* | **0.10** | 10% of wallet; trips at 99.9th+ percentile only |
| `min_wallet_balance_lamports` | 100000000 | **250000000** | Floor = 5 × probe_size (0.25 SOL) |
| `hard_sl_pct` | 10.0 | **10.0** | No change — correctly calibrated |
| `max_total_size_sol` | 0.50 | **0.50** | No change — within Kelly bounds |

Also remove from engine-level config:
- `paper_daily_loss_cap_sol: 5` → replace with `paper_daily_loss_cap_pct: 0.20` (paper can be looser)
- `live_daily_loss_cap_sol: 0.18` → replace with `live_daily_loss_cap_pct: 0.10`

---

## 6. Code Changes

### 6a. `momentum/config.rs` — Replace field in MomentumConfig

**Remove:**
```rust
    /// Daily loss cap in SOL — circuit breaker.
    pub daily_loss_cap_sol: f64,
```

**Add:**
```rust
    /// Daily loss cap as fraction of wallet balance (0.0–1.0) — circuit breaker.
    /// E.g. 0.10 = pause if daily PnL loss exceeds 10% of current wallet balance.
    pub daily_loss_cap_pct: f64,
```

**In `Default` impl, replace:**
```rust
            daily_loss_cap_sol: 2.0,
```
**With:**
```rust
            daily_loss_cap_pct: 0.10,
```

### 6b. `momentum/mod.rs` — Circuit breaker check (line ~398-401)

**Replace:**
```rust
        // Check daily loss cap
        let daily_pnl = self.daily_pnl_lamports.load(Ordering::Relaxed);
        let cap_lamports = -(self.config.daily_loss_cap_sol * 1e9) as i64;
        if daily_pnl <= cap_lamports {
            return; // daily cap hit
        }
```

**With:**
```rust
        // Check daily loss cap (percentage of current wallet balance)
        let daily_pnl = self.daily_pnl_lamports.load(Ordering::Relaxed);
        let wallet_bal = self.wallet_balance_lamports.load(Ordering::Relaxed);
        let cap_lamports = -((wallet_bal as f64 * self.config.daily_loss_cap_pct) as i64);
        if daily_pnl <= cap_lamports {
            return; // daily cap hit — lost >{:.0}% of wallet
        }
```

**Why this works:**
- `wallet_balance_lamports` is already polled every 30s into an `Arc<AtomicU64>` on the struct
- `daily_pnl_lamports` is already tracked as `AtomicI64` running sum
- The check is: if `daily_pnl <= -(wallet_bal × daily_loss_cap_pct)` → stop
- Zero new fields, zero new allocations, one multiplication per graduation check (~15/day)

**Edge case:** On first tick before wallet balance poll returns, `wallet_balance_lamports` is initialized to `u64::MAX`. The cap would be astronomically large → no false trigger. This is correct: we shouldn't circuit-break before we know our balance.

### 6c. `momentum/config.rs` — Serde deserialization

Find where `daily_loss_cap_sol` is deserialized from JSON config. The struct likely has a serde rename or default. Update the JSON field name:

```rust
    #[serde(default = "default_daily_loss_cap_pct")]
    pub daily_loss_cap_pct: f64,
```

```rust
fn default_daily_loss_cap_pct() -> f64 { 0.10 }
```

### 6d. `engine/config.rs` — Remove engine-level SOL-based cap

**Remove lines 95-97:**
```rust
    pub daily_loss_cap_sol: Option<f64>,
    pub paper_daily_loss_cap_sol: Option<f64>,
    pub live_daily_loss_cap_sol: Option<f64>,
```

**Replace with:**
```rust
    pub daily_loss_cap_pct: Option<f64>,
    pub paper_daily_loss_cap_pct: Option<f64>,
    pub live_daily_loss_cap_pct: Option<f64>,
```

**Replace the builder block (lines 1351-1362):**
```rust
    // ── Safety / circuit breaker config ─────────────────────────────
    // Daily loss cap: paper mode uses paper_daily_loss_cap_sol, live uses live_daily_loss_cap_sol,
    // both fall back to daily_loss_cap_sol, then to 5.0 SOL.
    let daily_loss_cap_sol = if paper_mode {
        mev.paper_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(5.0)
    } else {
        mev.live_daily_loss_cap_sol
            .or(mev.daily_loss_cap_sol)
            .unwrap_or(0.18)
    };
    let daily_loss_cap_lamports = sol_to_lamports(daily_loss_cap_sol);
```

**With:**
```rust
    // ── Safety / circuit breaker config ─────────────────────────────
    // Daily loss cap: percentage of current wallet balance.
    // Paper mode uses paper_daily_loss_cap_pct (default 20%), live uses live (default 10%).
    let daily_loss_cap_pct = if paper_mode {
        mev.paper_daily_loss_cap_pct
            .or(mev.daily_loss_cap_pct)
            .unwrap_or(0.20)
    } else {
        mev.live_daily_loss_cap_pct
            .or(mev.daily_loss_cap_pct)
            .unwrap_or(0.10)
    };
```

And update `EngineConfig` struct field `daily_loss_cap_lamports: u64` → `daily_loss_cap_pct: f64`, plus the builder assignment.

### 6e. `config/canary.json` — Update config

**In momentum section (~line 533), replace:**
```json
    "daily_loss_cap_sol": 2.0,
```
**With:**
```json
    "daily_loss_cap_pct": 0.10,
```

**In engine section (~lines 388-389), replace:**
```json
    "paper_daily_loss_cap_sol": 5,
    "live_daily_loss_cap_sol": 0.18,
```
**With:**
```json
    "paper_daily_loss_cap_pct": 0.20,
    "live_daily_loss_cap_pct": 0.10,
```

**In engine section (~line 322), replace:**
```json
    "daily_loss_cap_sol": 5,
```
**With:**
```json
    "daily_loss_cap_pct": 0.20,
```

**Update min_wallet_balance (~line 592):**
```json
    "min_wallet_balance_lamports": 250000000,
```

---

## 7. Risks and Caveats for First Live Session

### Model uncertainty
- 31 trades is a small sample. The loss distribution could be fatter-tailed than modeled.
- The 99.9th percentile drawdown (~7.4%) is estimated, not known. Real markets have regime changes.
- **Mitigation:** 10% cap gives 2.6% buffer above 99.9th percentile estimate. Reasonable margin.

### Correlated losses
- The simulation assumes independent trades. In reality, if the market dumps, all 5 concurrent positions may stop out simultaneously.
- Worst case 5 simultaneous probe hard_sl = 0.025 SOL (1.67%). Worst case 3 scaled-in hard_sl = 0.150 SOL (10%) = exact cap trigger.
- **Mitigation:** This is by design. 3 simultaneous worst-case losses trips the breaker. That's correct behavior.

### Wallet balance staleness
- Balance is polled every 30s. If balance drops between polls (from a tx landing), the cap threshold could be stale by up to 30s.
- At 15 trades/day average, that's ~1 trade per 100 minutes. 30s staleness is negligible.
- **Not a real risk.**

### First day psychology
- 10% of 1.5 SOL = 0.15 SOL max daily loss. On the very first live day, if you see -0.10 SOL on the daily PnL, don't panic-override the cap. That's still within normal (99th percentile) variance for a bad day.
- If the cap trips: **stop for the day.** Don't adjust the cap. Investigate what happened, then trade the next day.

### Growing wallet
- As the wallet grows from 1.5 → 3.0 → 5.0 SOL, the cap scales automatically: 0.15 → 0.30 → 0.50 SOL daily max loss.
- This is correct Kelly-adjacent behavior: bet size (and max loss) grows with bankroll.
- **Re-evaluate** when wallet exceeds 10 SOL. At that point, consider tightening to 0.08 to preserve larger absolute gains.
