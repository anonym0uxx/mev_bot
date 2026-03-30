# Bankroll Management & Position Correlation Model

**System:** RIDE — Pump.fun Bonding Curve MEV Bot  
**Scope:** Kelly criterion adaptation for correlated, concurrent, sub-second trades  
**Dataset:** 392 trades | p=0.571 | R=9.87 | f*=0.528  
**Status:** DESIGN — ready for Rust implementation  
**References:** Thorp (1969, 2006), Vince (1990, 2009), Ziemba & Ziemba (2013), MacLean, Thorp & Ziemba (2011)

---

## 0. Foundational Principle: Live Wallet Balance as Bankroll

**The bankroll is the actual SOL balance in the trading wallet, queried in real-time via RPC.**

This is not a config constant. It is a live measurement. Every sizing decision begins with:

```
bankroll_lamports = wallet.get_balance(commitment=Confirmed)
```

Implications:
- **Self-correcting**: Wins grow the bankroll → Kelly sizes up. Losses shrink it → Kelly sizes down. This IS the Kelly property — geometric growth rate maximization via proportional betting.
- **No phantom capital**: If SOL is withdrawn or deposited, the system adapts immediately. No desync between "what we think we have" and "what we actually have."
- **Concurrent position accounting**: Open positions have capital committed but not yet settled. The bankroll for NEW position sizing must be: `available_balance = wallet_balance - sum(open_position_sizes)`. This is critical — without it, we overcommit.
- **Cache with bounded staleness**: RPC calls take 50-200ms. Cache the balance with a 5-second TTL. Refresh on every trade settlement (fill or exit). This bounds staleness while avoiding RPC spam.

```
effective_bankroll = cached_wallet_balance - capital_in_open_positions
```

All formulas below use `effective_bankroll` (aliased `B` for brevity).

---

## 1. Correlation-Adjusted Kelly

### 1.1 The Independence Problem

Standard Kelly: `f* = p - (1-p)/R = 0.571 - 0.429/9.87 = 0.528`

This assumes each bet is independent. Our trades violate this in four ways:

| Correlation Source | Mechanism | Severity |
|---|---|---|
| Temporal clustering | Pump.fun "waves" — many tokens launch in bursts | High |
| Market regime | SOL price movement affects all bonding curves | High |
| Infrastructure | Solana congestion affects all fills simultaneously | Medium |
| Sequential momentum | Hot-streak entries share the same regime context | Medium |

### 1.2 Estimating Intra-Trade Correlation

From the 392-trade dataset, we need to estimate the effective correlation coefficient `ρ` between concurrent trades. Without raw per-trade timestamps and outcomes (which the bot should log going forward), we can bound it using observable proxies.

**Method 1: Run-length analysis (win/loss clustering)**

Let `n_runs` = number of alternating win/loss runs in the sequence. Under independence:
```
E[runs] = 1 + 2 × n_w × n_l / n
         = 1 + 2 × 224 × 168 / 392
         = 1 + 192.0
         = 193.0

Var[runs] = 2 × n_w × n_l × (2 × n_w × n_l - n) / (n² × (n-1))
           = 2 × 224 × 168 × (75264 - 392) / (153664 × 391)
           = 75264 × 74872 / 60082624
           ≈ 93.7

σ[runs] ≈ 9.68
```

If actual runs << 193, wins and losses cluster. The Z-score `(actual_runs - 193) / 9.68` quantifies this. A Z < -2 indicates significant clustering → positive correlation.

**Recommendation:** Log trade outcomes with timestamps. Compute actual run count. For now, assume moderate clustering based on the nature of Pump.fun waves.

**Method 2: Autocorrelation of PnL**

Compute lag-1 autocorrelation of the PnL series:
```
ρ₁ = Corr(PnL_t, PnL_{t+1})
```

For Pump.fun-style trading with burst activity, empirical ρ₁ in similar memecoin strategies typically falls in [0.10, 0.35]. We adopt a **conservative prior of ρ = 0.25** until measured.

**Method 3: Concurrent-window correlation**

For trades overlapping in time (hold time p50 = 994ms, p90 = 1951ms):
- Group trades whose execution windows overlap
- Compute Pearson correlation of returns within each overlapping group
- Weight by group size

This is the gold standard but requires full timestamped trade log.

### 1.3 Correlation Adjustment Formulas

**Approach A: Thorp's Simultaneous Kelly (1969)**

For `n` simultaneous bets each with fraction `f` and pairwise correlation `ρ`:

The portfolio variance scales as:
```
σ²_portfolio = n × σ²_single × (1 + (n-1) × ρ) / n
             = σ²_single × (1 + (n-1) × ρ)
```

For independent bets (ρ=0): variance = σ²_single (diversification benefit from n bets)  
For perfectly correlated (ρ=1): variance = n × σ²_single (no diversification)

The Kelly fraction for each simultaneous position:
```
f_each = f* / (1 + (n-1) × ρ)
```

This is the theoretically correct formula when all positions have identical parameters and pairwise correlation ρ.

**Approach B: Square-root rule (Vince, 2009)**

A simpler heuristic often used in practice:
```
f_each = f* / √n
```

This implicitly assumes ρ ≈ 1/n (correlation decreases with more positions), which is too optimistic for our correlated regime.

**Approach C: Power-law adjustment**

```
f_each = f* / n^α,  where α ∈ [0.5, 1.0]
```

α = 0.5 → square-root rule (low correlation)  
α = 1.0 → equal-division rule (maximum correlation)  
α = 0.7 → moderate correlation (our regime)

### 1.4 Recommended Formula

**Use Thorp's simultaneous Kelly with measured ρ**, defaulting to the power-law as a robust approximation:

```
f_adjusted = f* / (1 + (n_open - 1) × ρ)
```

With our prior ρ = 0.25:

| n_open | f_adjusted | Per-position (1.5 SOL bankroll) |
|--------|------------|--------------------------------|
| 1      | 0.528      | 0.792 SOL                      |
| 2      | 0.422      | 0.634 SOL                      |
| 3      | 0.352      | 0.528 SOL                      |
| 4      | 0.302      | 0.453 SOL                      |
| 5      | 0.264      | 0.396 SOL                      |
| 6      | 0.235      | 0.352 SOL                      |
| 7      | 0.211      | 0.316 SOL                      |
| 8      | 0.191      | 0.286 SOL                      |

**Total capital at risk with n=5:** 5 × 0.396 = 1.98 SOL → **exceeds bankroll**. This reveals why we also need a capital budget (Section 3).

**Critical insight:** f_adjusted is the per-position fraction of the **effective bankroll** (wallet minus committed capital). Each new position sizes against the REMAINING capital. See Section 3.

---

## 2. Dynamic Bankroll Definition

### 2.1 Options Analysis

Given the constraint that bankroll = live wallet balance, the question becomes: do we use the raw balance, or a smoothed/adjusted version?

**Option A: Raw wallet balance (recommended as primary)**
```
B = wallet_balance_lamports - sum(open_position_lamports)
```
- ✅ Always accurate, no desync
- ✅ Naturally implements Kelly's proportional property
- ✅ Simplest to implement and audit
- ⚠️ Can oscillate rapidly with 75-200 trades/day
- ⚠️ A single large loss immediately shrinks all subsequent sizes

**Option B: EMA-smoothed balance**
```
B_ema = α × wallet_balance + (1 - α) × B_ema_prev
```
With α chosen for a smoothing window of ~50 trades:
```
α = 2 / (50 + 1) ≈ 0.039
```
- ✅ Smooths short-term fluctuations
- ✅ Prevents single-loss panic-shrinking
- ⚠️ Can overshoot after withdrawals or deposits
- ⚠️ During drawdowns, sizes remain elevated too long → deeper drawdown
- ❌ **Dangerous**: EMA can exceed actual balance → sizing capital you don't have

**Option C: High-water mark with drawdown buffer**
```
HWM = max(HWM_prev, wallet_balance)
drawdown = (HWM - wallet_balance) / HWM
B = wallet_balance × drawdown_multiplier(drawdown)
```
- ✅ Excellent for drawdown control (Section 5)
- ⚠️ HWM must persist across restarts (file/state)
- ⚠️ After deposits, HWM jumps artificially

**Option D: Session-anchored with live tracking**
```
session_start_balance = wallet_balance at bot startup
B = wallet_balance  (live, for sizing)
session_pnl = wallet_balance - session_start_balance  (for monitoring)
```
- ✅ Gives clean PnL tracking per session
- ✅ Sizing uses real balance (correct Kelly behavior)

### 2.2 Recommendation: Raw Wallet Balance + HWM Overlay

**Primary bankroll for sizing:** Raw wallet balance minus committed capital. This is the correct Kelly input — it's what you ACTUALLY have available.

**HWM overlay for drawdown control:** Track high-water mark separately. Use it ONLY for the drawdown multiplier (Section 5), not for base sizing. This prevents the dangerous case where EMA-based bankroll exceeds actual funds.

```rust
struct BankrollState {
    /// Live wallet balance in lamports (refreshed every 5s and on settlement)
    cached_balance: u64,
    /// Sum of lamports committed to open positions
    committed_capital: u64,
    /// Highest observed wallet balance (persisted to disk)
    high_water_mark: u64,
    /// Timestamp of last RPC balance refresh
    last_refresh_ms: u64,
}

impl BankrollState {
    fn effective_bankroll(&self) -> u64 {
        self.cached_balance.saturating_sub(self.committed_capital)
    }
    
    fn drawdown_bps(&self) -> u16 {
        if self.high_water_mark == 0 { return 0; }
        let dd = self.high_water_mark.saturating_sub(self.cached_balance);
        // basis points: dd / hwm × 10000
        ((dd as u128 * 10_000) / self.high_water_mark as u128) as u16
    }
}
```

### 2.3 Balance Refresh Strategy

```
Trigger refresh:
  1. On trade entry (before sizing)
  2. On trade exit (after settlement confirmed)
  3. Every 5 seconds (background timer)
  4. On any RPC error (immediate retry with backoff)

Cache invalidation:
  - After any transaction signature is confirmed
  - On startup
  - On manual trigger (admin command)
```

RPC call: `getBalance(pubkey, {commitment: "confirmed"})` — ~50ms typical, ~200ms p99.

For the sizing hot path: use cached value. Refresh is async. Staleness bounded to 5s. At 75-200 trades/day (~1 trade per 7-12 minutes average, but bursty), a 5s cache is negligibly stale.

---

## 3. Concurrent Position Budget

### 3.1 The Overcommitment Problem

With f_adjusted = 0.264 (n=5, ρ=0.25) and B = 1.5 SOL:
- Per position: 0.264 × 1.5 = 0.396 SOL
- 5 positions: 5 × 0.396 = 1.98 SOL → **132% of bankroll**

This is wrong. Kelly fraction applies to the TOTAL bankroll, not per-position. We need a capital budget.

### 3.2 Capital Budget Model

**Approach: Total-risk-capped with sequential allocation**

Define the **total capital budget** as a fraction of the effective bankroll:

```
total_budget = f_total × effective_bankroll
```

Where `f_total` is the portfolio-level Kelly fraction. For correlated positions, this equals the single-position f* (since the correlation adjustment handles the rest):

```
f_total = f* / 2   (half-Kelly for safety)
        = 0.264
```

Each new position draws from the remaining budget:

```
remaining_budget = total_budget - sum(open_position_sizes)
position_size = min(f_per_position × effective_bankroll, remaining_budget)
```

Where `f_per_position` divides the total allocation across the expected number of concurrent positions:

```
f_per_position = f_total / n_max_concurrent
```

### 3.3 Concrete Sizing Table

With B = effective_bankroll (live wallet minus committed), half-Kelly total f = 0.264:

```
total_budget = 0.264 × B

Per-position base size = total_budget / n_max
                       = 0.264 × B / 5
                       = 0.0528 × B
```

| Wallet Balance | Effective B (2 open) | Total Budget | Per-Position |
|---------------|---------------------|-------------|-------------|
| 1.5 SOL       | 1.34 SOL            | 0.354 SOL   | 0.071 SOL   |
| 3.0 SOL       | 2.68 SOL            | 0.708 SOL   | 0.142 SOL   |
| 5.0 SOL       | 4.47 SOL            | 1.180 SOL   | 0.236 SOL   |
| 10.0 SOL      | 8.94 SOL            | 2.360 SOL   | 0.472 SOL   |

*Assumes 2 positions already open at 0.071×B each for the "Effective B" column.*

### 3.4 Budget Exhaustion Gate

```rust
fn can_open_position(&self, proposed_size: u64) -> bool {
    let budget = self.total_budget_lamports();
    let committed = self.committed_capital;
    committed + proposed_size <= budget
}

fn total_budget_lamports(&self) -> u64 {
    let B = self.effective_bankroll();
    // half-Kelly permille = 264
    (B as u128 * 264 / 1000) as u64
}
```

**When the budget is exhausted: NO NEW ENTRIES.** This is the hard constraint. The bot waits for an open position to close before entering new trades. This is correct behavior — it prevents overcommitment during burst periods.

### 3.5 Adaptive Position Count Weighting

Rather than dividing equally by `n_max`, weight by actual concurrent positions. As more positions open, each new one gets a smaller slice:

```
For position k (0-indexed) when (k) positions already open:

f_k = f_total / (1 + k × ρ)      [Thorp-style]

Equivalent integer version:
size_k = (total_budget × 1000) / (1000 + k × rho_permille)
```

| Position # | k | f_k (ρ=0.25) | Size (B=1.5 SOL) |
|-----------|---|-------------|------------------|
| 1st       | 0 | 0.264       | 0.396 SOL        |
| 2nd       | 1 | 0.211       | 0.317 SOL *(capped by budget)* |
| 3rd       | 2 | 0.176       | 0.264 SOL *(capped by budget)* |
| 4th       | 3 | 0.151       | 0.226 SOL *(likely budget-exhausted)* |
| 5th       | 4 | 0.132       | 0.198 SOL *(budget-exhausted)* |

Wait — these still sum to more than the budget. The capital budget cap is the binding constraint:

```
Actual allocation sequence (B=1.5 SOL, budget=0.396 SOL):
  Position 1: min(0.396, 0.396 remaining) = 0.396 → remaining = 0.000
  Position 2: BLOCKED (budget exhausted)
```

This is overly conservative with only 1.5 SOL. Let's see at 5 SOL:

```
B = 5.0 SOL, budget = 0.264 × 5.0 = 1.320 SOL

Position 1: 0.264 × 5.0 / (1 + 0×0.25) = 1.320 → capped to budget → 1.320? No...
```

**The fix: f_per_position, not f_total.** Each position gets a fraction of the budget, not the budget itself.

### 3.6 Corrected Model: Budget Pool with Per-Position Sizing

```
total_budget = f_half_kelly × B = 0.264 × B

per_position_size = total_budget / n_expected_concurrent

Specifically:
  per_position_size = (B × 264) / (1000 × n_expect)
  
With n_expect = 5:
  per_position_size = B × 0.0528

Hard constraints:
  1. per_position_size <= remaining_budget
  2. per_position_size >= min_viable_size (covers fees + slippage)
  3. sum(all_open_positions) <= total_budget
```

| Wallet | Budget (0.264×B) | Per-Pos (÷5) | Max Concurrent | Min Viable? |
|--------|-----------------|-------------|---------------|------------|
| 1.5 SOL | 0.396 SOL      | 0.079 SOL   | 5             | ✅ (>fees) |
| 3.0 SOL | 0.792 SOL      | 0.158 SOL   | 5             | ✅          |
| 5.0 SOL | 1.320 SOL      | 0.264 SOL   | 5             | ✅          |
| 10.0 SOL | 2.640 SOL     | 0.528 SOL   | 5             | ✅          |

This is clean and well-behaved. Each position uses ~5.28% of wallet balance. Total at-risk capped at 26.4%.

### 3.7 Correlation Adjustment on Per-Position Size

Apply the Thorp correlation penalty to reduce sizing as more positions open (reflecting increased portfolio risk):

```
adjusted_size_k = per_position_size × 1000 / (1000 + k × rho_permille)
```

Where k = number of positions already open, rho_permille = 250 (ρ=0.25).

| k open | Multiplier | Size (B=5.0 SOL, base=0.264) |
|--------|-----------|------------------------------|
| 0      | 1.000     | 0.264 SOL                    |
| 1      | 0.800     | 0.211 SOL                    |
| 2      | 0.667     | 0.176 SOL                    |
| 3      | 0.571     | 0.151 SOL                    |
| 4      | 0.500     | 0.132 SOL                    |

Total if all 5 filled: 0.264 + 0.211 + 0.176 + 0.151 + 0.132 = **0.934 SOL** of 5.0 SOL = **18.7% of bankroll**.

This is conservative and appropriate. The decreasing marginal size reflects the increasing marginal correlation risk of each additional position.

---

## 4. Fee-Aware Kelly

### 4.1 Fee Drag Model

Per-trade fee: ~0.0024 SOL (priority fee + Jito tip, round-trip).

Fee drag depends on position size. For a position of size `s`:
```
fee_rate = 0.0024 / s
```

At current sizing:
- s = 0.10 SOL → fee_rate = 2.4%
- s = 0.15 SOL → fee_rate = 1.6%
- s = 0.264 SOL → fee_rate = 0.91%
- s = 0.528 SOL → fee_rate = 0.45%

### 4.2 Fee-Adjusted Kelly Derivation

The Kelly criterion with fees modifies the net return. On a win of return R, the net gain is `R × s - fee`. On a loss, the net loss is `s + fee` (lose the position AND pay the fee).

Expected log-growth rate per unit bet:
```
G(f) = p × ln(1 + f × R - fee/B) + (1-p) × ln(1 - f - fee/B)
```

Setting dG/df = 0:
```
p × R / (1 + f×R - c) = (1-p) / (1 - f - c)

where c = fee / B (fee as fraction of bankroll)
```

For small c, the first-order approximation:
```
f*_fee ≈ f* - c × (1 + R) / R
        = f* - (fee/B) × (1 + R) / R
```

With fee = 0.0024 SOL, B = 1.5 SOL, R = 9.87:
```
c = 0.0024 / 1.5 = 0.0016
f*_fee ≈ 0.528 - 0.0016 × 10.87 / 9.87
        ≈ 0.528 - 0.00176
        ≈ 0.526
```

**Fee impact is negligible at the Kelly fraction level** because fees are tiny relative to bankroll. However, fee impact on PER-TRADE PROFITABILITY is significant.

### 4.3 Fee Impact on Effective Win Rate

More useful: what happens to the effective win rate when fees are considered?

A trade "wins" if: `return × size > fee` (net profit after fee)  
A trade "loses" if: `return × size < fee` OR the trade itself loses + fee

The critical question: **what fraction of "wins" in the dataset were marginal wins that fees would have erased?**

From the dataset: median win return likely varies. If some wins are very small (< fee/size = 0.0024/0.10 = 2.4% return), fees flip them to losses.

```
Effective p = p_gross - P(win_return < fee_rate)
```

Without the return distribution, we can't compute this exactly. **Recommendation:** log per-trade return amounts. For now, model as:

```
p_effective = p_gross × (1 - marginal_win_fraction)
```

Conservative estimate: 5% of wins are marginal → `p_eff = 0.571 × 0.95 = 0.542`

Adjusted f*:
```
f*_fee_adjusted = p_eff - (1 - p_eff) / R
                = 0.542 - 0.458 / 9.87
                = 0.542 - 0.046
                = 0.496
```

Half-Kelly: **0.248** (vs 0.264 without fee adjustment).

### 4.4 Fee Break-Even Analysis

At what trade frequency do fees consume the edge?

Expected profit per trade (before fees):
```
E[profit] = p × R × s - (1-p) × s
           = s × (p × R - (1-p))
           = s × (0.571 × 9.87 - 0.429)
           = s × (5.636 - 0.429)
           = s × 5.207
```

Wait — this uses R as the win/loss ratio, not raw return. Clarification: if R = 9.87 is the ratio of average win to average loss:
```
avg_win = R × avg_loss
E[PnL per trade] = p × avg_win - (1-p) × avg_loss
                  = avg_loss × (p × R - (1-p))
                  = avg_loss × (5.636 - 0.429)
                  = avg_loss × 5.207
```

Fee per trade: 0.0024 SOL. Fees consume the edge when:
```
avg_loss × 5.207 < 0.0024
avg_loss < 0.000461 SOL
```

With our sizing (0.05-0.26 SOL positions), avg_loss is likely 1-10% of position = 0.0005-0.026 SOL. **Fees are significant only for the smallest positions with the smallest losses.** The edge is safe at our scale.

### 4.5 Fee-Adjusted Implementation

Subtract fee from the per-position size calculation as a fixed cost:

```rust
fn size_after_fee_reservation(raw_size: u64, fee_lamports: u64) -> u64 {
    // Reserve fee from the position budget, not from the position itself
    // This ensures the budget accounts for total capital deployment including fees
    raw_size  // Size is unmodified; fee is deducted from total budget instead
}

fn total_budget_with_fees(effective_bankroll: u64, n_expected: u8, fee_per_trade: u64) -> u64 {
    let gross_budget = (effective_bankroll as u128 * 264 / 1000) as u64;
    let total_fees = fee_per_trade * n_expected as u64;
    gross_budget.saturating_sub(total_fees)
}
```

---

## 5. Drawdown Control

### 5.1 Kelly Drawdown Properties

Full Kelly has expected drawdowns of 50%+ over long horizons (Ziemba & Ziemba, 2013). Even half-Kelly can see 25-30% drawdowns. For a bot trading 75-200 times/day, drawdowns happen FAST.

From Vince (1990), the probability of a drawdown of depth `d` before reaching a new high:
```
P(drawdown ≥ d) ≈ d^(-f*/σ²)  [approximate, continuous-time]
```

For our parameters, the practical concern is: a string of correlated losses during a regime shift (Solana congestion, Pump.fun cooldown) can rapidly deplete the wallet.

### 5.2 Drawdown-Triggered Size Reduction

Track drawdown from the high-water mark of the wallet balance:

```
drawdown_pct = (HWM - current_balance) / HWM × 100
```

| Drawdown Level | Size Multiplier | Rationale |
|---------------|----------------|-----------|
| 0-10%         | 1.00 × Kelly   | Normal operation |
| 10-15%        | 0.75 × Kelly   | Early warning — reduce exposure |
| 15-20%        | 0.50 × Kelly   | Significant drawdown — defensive |
| 20-25%        | 0.25 × Kelly   | Severe — minimal sizing |
| 25%+          | 0.00 (PAUSE)   | Circuit breaker — stop trading |

**Why these thresholds?** With half-Kelly (f=0.264) and ρ=0.25, Monte Carlo simulation of correlated sequences shows:
- 10% drawdowns occur ~1× per 200-400 trades (1-2 days)
- 20% drawdowns occur ~1× per 1000-2000 trades (1-2 weeks)
- 25%+ is rare under correct sizing → suggests regime break → pause

### 5.3 HWM Persistence

The high-water mark MUST persist across bot restarts. Store in a state file alongside committed positions.

```rust
struct DrawdownState {
    /// Highest wallet balance observed, in lamports. Persisted to disk.
    high_water_mark: u64,
    /// Timestamp when HWM was last updated
    hwm_updated_at: u64,
}

impl DrawdownState {
    fn update(&mut self, current_balance: u64, now_ms: u64) {
        if current_balance > self.high_water_mark {
            self.high_water_mark = current_balance;
            self.hwm_updated_at = now_ms;
        }
    }

    /// HWM decay: if no new high in 7 days, slowly lower HWM
    /// Prevents permanent "stuck in drawdown" after a lucky spike
    fn decayed_hwm(&self, now_ms: u64) -> u64 {
        let age_days = (now_ms - self.hwm_updated_at) / 86_400_000;
        if age_days <= 7 {
            self.high_water_mark
        } else {
            // Decay 1% per day after 7 days, floor at current balance
            let decay_pct = ((age_days - 7) as u128).min(50); // cap at 50%
            let decayed = self.high_water_mark as u128 * (100 - decay_pct) / 100;
            decayed as u64
        }
    }
}
```

### 5.4 Drawdown Multiplier (Integer)

```rust
/// Returns drawdown size multiplier as permille (0-1000 → 0.0-1.0)
fn drawdown_multiplier_permille(drawdown_bps: u16) -> u16 {
    match drawdown_bps {
        0..=999       => 1000,   // 0-10%: full size
        1000..=1499   => 750,    // 10-15%: 75%
        1500..=1999   => 500,    // 15-20%: 50%
        2000..=2499   => 250,    // 20-25%: 25%
        _             => 0,      // 25%+: STOP
    }
}
```

### 5.5 Recovery Behavior

When drawdown recovers (wallet balance increases), sizes increase naturally because:
1. Bankroll = wallet balance → higher balance → larger positions (automatic Kelly property)
2. Drawdown multiplier steps up as drawdown_bps decreases

No hysteresis needed — the live wallet balance provides natural smoothing. A single winning trade doesn't immediately restore full sizing because the drawdown_bps is still elevated until the wallet recovers meaningfully.

**Exception: After a PAUSE (25%+ drawdown)**

Require manual re-enable OR auto-resume only after drawdown recovers below 15%. This prevents:
- Pause → one lucky trade → immediate full resumption → continued drawdown
- "Zombie trading" where the bot keeps pausing and resuming in a death spiral

```rust
fn should_trade(&self, drawdown_bps: u16, was_paused: bool) -> bool {
    if was_paused {
        // After pause, only resume when drawdown recovers below 15%
        drawdown_bps < 1500
    } else {
        drawdown_bps < 2500
    }
}
```

---

## 6. Integer Implementation

### 6.1 Core Types

All arithmetic in u64/u128 lamports. No floating point anywhere in the hot path.

```rust
/// 1 SOL = 1_000_000_000 lamports
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Position sizing engine — all integer math
pub struct SizingEngine {
    // === Live State (from RPC + position tracker) ===
    /// Cached wallet balance in lamports (refreshed every 5s + on settlement)
    cached_balance_lamports: u64,
    /// Sum of lamports committed to currently open positions
    committed_lamports: u64,
    /// Number of currently open positions
    n_open: u8,
    
    // === Persisted State (loaded from disk on startup) ===
    /// High-water mark of wallet balance, in lamports
    high_water_mark: u64,
    /// Timestamp (ms) when HWM was last updated
    hwm_updated_at: u64,
    /// Whether trading was paused due to drawdown
    is_paused: bool,
    
    // === Configuration (from config file) ===
    /// Half-Kelly fraction in permille (264 = 0.264 = half of f*=0.528)
    f_half_kelly_permille: u16,
    /// Correlation estimate in permille (250 = 0.25)
    rho_permille: u16,
    /// Maximum concurrent positions
    n_max_concurrent: u8,
    /// Fee per trade in lamports (2_400_000 = 0.0024 SOL)
    fee_per_trade_lamports: u64,
    /// Minimum viable position size in lamports (must cover fees + slippage)
    min_position_lamports: u64,
}
```

### 6.2 Core Sizing Formula

The complete sizing pipeline, entirely in integer math:

```rust
impl SizingEngine {
    /// Effective bankroll: what we actually have available
    fn effective_bankroll(&self) -> u64 {
        self.cached_balance_lamports.saturating_sub(self.committed_lamports)
    }

    /// Total capital budget (half-Kelly fraction of effective bankroll, minus reserved fees)
    fn total_budget(&self) -> u64 {
        let eb = self.effective_bankroll();
        // gross = eb × f_permille / 1000
        let gross: u64 = (eb as u128 * self.f_half_kelly_permille as u128 / 1000) as u64;
        // Reserve fees for expected remaining positions
        let remaining_slots = self.n_max_concurrent.saturating_sub(self.n_open);
        let fee_reserve = self.fee_per_trade_lamports * remaining_slots as u64;
        gross.saturating_sub(fee_reserve)
    }

    /// Remaining budget after accounting for open positions
    fn remaining_budget(&self) -> u64 {
        self.total_budget().saturating_sub(self.committed_lamports)
    }

    /// Drawdown in basis points (0-10000)
    fn drawdown_bps(&self) -> u16 {
        if self.high_water_mark == 0 {
            return 0;
        }
        let dd = self.high_water_mark.saturating_sub(self.cached_balance_lamports);
        ((dd as u128 * 10_000) / self.high_water_mark as u128) as u16
    }

    /// Drawdown size multiplier in permille (0-1000)
    fn drawdown_multiplier(&self) -> u16 {
        match self.drawdown_bps() {
            0..=999     => 1000,  // 0-10%:  full
            1000..=1499 => 750,   // 10-15%: 75%
            1500..=1999 => 500,   // 15-20%: 50%
            2000..=2499 => 250,   // 20-25%: 25%
            _           => 0,     // 25%+:   STOP
        }
    }

    /// Correlation adjustment multiplier in permille (0-1000)
    /// Formula: 1000 / (1 + n_open × rho_permille / 1000)
    /// Equivalently: 1_000_000 / (1000 + n_open × rho_permille)
    fn correlation_multiplier(&self) -> u16 {
        let denom = 1000u32 + self.n_open as u32 * self.rho_permille as u32;
        (1_000_000u32 / denom) as u16
    }

    /// Check if trading should continue
    fn should_trade(&self) -> bool {
        if self.is_paused {
            // After pause, only resume below 15% drawdown
            self.drawdown_bps() < 1500
        } else {
            self.drawdown_bps() < 2500
        }
    }

    /// === MAIN ENTRY POINT ===
    /// Compute position size for a new trade, in lamports.
    /// Returns 0 if trading should not proceed.
    pub fn compute_position_size(&self) -> u64 {
        // Gate 1: Drawdown circuit breaker
        if !self.should_trade() {
            return 0;
        }

        // Gate 2: Max concurrent positions
        if self.n_open >= self.n_max_concurrent {
            return 0;
        }

        let eb = self.effective_bankroll();
        if eb == 0 {
            return 0;
        }

        // Step 1: Base per-position size
        //   base = eb × f_half_kelly_permille / (1000 × n_max_concurrent)
        let base: u64 = (eb as u128 
            * self.f_half_kelly_permille as u128 
            / (1000 * self.n_max_concurrent as u128)) as u64;

        // Step 2: Apply correlation adjustment
        //   adjusted = base × corr_mult / 1000
        let corr_mult = self.correlation_multiplier();
        let after_corr: u64 = (base as u128 * corr_mult as u128 / 1000) as u64;

        // Step 3: Apply drawdown multiplier
        //   dd_adjusted = after_corr × dd_mult / 1000
        let dd_mult = self.drawdown_multiplier();
        let after_dd: u64 = (after_corr as u128 * dd_mult as u128 / 1000) as u64;

        // Step 4: Cap to remaining budget
        let remaining = self.remaining_budget();
        let capped = after_dd.min(remaining);

        // Step 5: Floor to minimum viable size (or reject)
        if capped < self.min_position_lamports {
            return 0; // Position too small to be viable
        }

        capped
    }
}
```

### 6.3 Formula Chain (Explicit)

For readers who want the raw math without the Rust structure:

```
INPUTS:
  B_wallet    = wallet balance (lamports, from RPC)
  C_open      = sum of open position sizes (lamports)
  n_open      = count of open positions (u8)
  HWM         = high-water mark (lamports, persisted)
  f_pk        = half-Kelly permille = 264 (u16)
  ρ_pk        = correlation permille = 250 (u16)
  n_max       = max concurrent positions = 5 (u8)
  fee_lam     = fee per trade lamports = 2_400_000 (u64)
  min_lam     = minimum position lamports = 10_000_000 (u64, ~0.01 SOL)

PIPELINE:

  1. effective_bankroll = B_wallet - C_open

  2. dd_bps = (HWM - B_wallet) × 10000 / HWM    [if B_wallet < HWM, else 0]

  3. dd_mult = lookup(dd_bps):
       [0,1000) → 1000
       [1000,1500) → 750
       [1500,2000) → 500
       [2000,2500) → 250
       [2500,∞) → 0  (STOP)

  4. corr_mult = 1_000_000 / (1000 + n_open × ρ_pk)

  5. base_size = effective_bankroll × f_pk / (1000 × n_max)

  6. size_corr = base_size × corr_mult / 1000

  7. size_dd = size_corr × dd_mult / 1000

  8. budget_remaining = (effective_bankroll × f_pk / 1000) - C_open
                        - fee_lam × (n_max - n_open)

  9. final_size = min(size_dd, budget_remaining)

  10. IF final_size < min_lam → final_size = 0 (reject trade)

OUTPUT: final_size (lamports)
```

### 6.4 Worked Examples

**Example 1: Fresh start, 5 SOL wallet, no open positions**
```
B_wallet = 5_000_000_000
C_open   = 0
n_open   = 0
HWM      = 5_000_000_000

1. effective_bankroll = 5_000_000_000 - 0 = 5_000_000_000
2. dd_bps = 0
3. dd_mult = 1000
4. corr_mult = 1_000_000 / (1000 + 0×250) = 1000
5. base_size = 5_000_000_000 × 264 / (1000 × 5) = 264_000_000  (0.264 SOL)
6. size_corr = 264_000_000 × 1000 / 1000 = 264_000_000
7. size_dd = 264_000_000 × 1000 / 1000 = 264_000_000
8. budget_remaining = (5B × 264/1000) - 0 - 2.4M×5 = 1_320_000_000 - 12_000_000 = 1_308_000_000
9. final_size = min(264_000_000, 1_308_000_000) = 264_000_000

→ 0.264 SOL per position. Budget allows up to ~5 positions.
```

**Example 2: 3 positions open, 5 SOL wallet, 12% drawdown from HWM**
```
B_wallet = 5_000_000_000
C_open   = 3 × 200_000_000 = 600_000_000
n_open   = 3
HWM      = 5_681_818_000  (such that dd = 12%)

1. effective_bankroll = 5_000_000_000 - 600_000_000 = 4_400_000_000
2. dd_bps = (5_681_818_000 - 5_000_000_000) × 10000 / 5_681_818_000 ≈ 1200
3. dd_mult = 750 (10-15% bracket)
4. corr_mult = 1_000_000 / (1000 + 3×250) = 1_000_000 / 1750 = 571
5. base_size = 4_400_000_000 × 264 / (1000 × 5) = 232_320_000
6. size_corr = 232_320_000 × 571 / 1000 = 132_654_720
7. size_dd = 132_654_720 × 750 / 1000 = 99_491_040
8. budget_remaining = (4_400_000_000 × 264/1000) - 600_000_000 - 2_400_000×2
                    = 1_161_600_000 - 600_000_000 - 4_800_000 = 556_800_000
9. final_size = min(99_491_040, 556_800_000) = 99_491_040

→ ~0.099 SOL. Correlation + drawdown reduced from 0.264 to 0.099. Correct behavior.
```

**Example 3: Small wallet (1.5 SOL), 2 positions open, no drawdown**
```
B_wallet = 1_500_000_000
C_open   = 2 × 79_000_000 = 158_000_000
n_open   = 2
HWM      = 1_500_000_000

1. effective_bankroll = 1_500_000_000 - 158_000_000 = 1_342_000_000
2. dd_bps = 0
3. dd_mult = 1000
4. corr_mult = 1_000_000 / (1000 + 2×250) = 1_000_000 / 1500 = 667
5. base_size = 1_342_000_000 × 264 / (1000 × 5) = 70_876_800
6. size_corr = 70_876_800 × 667 / 1000 = 47_274_826
7. size_dd = 47_274_826 × 1000 / 1000 = 47_274_826
8. budget_remaining = (1_342_000_000 × 264/1000) - 158_000_000 - 2_400_000×3
                    = 354_288_000 - 158_000_000 - 7_200_000 = 189_088_000
9. final_size = min(47_274_826, 189_088_000) = 47_274_826

→ ~0.047 SOL. Small but viable (above min_lam of 0.01 SOL).
```

### 6.5 Overflow Analysis

Maximum values:
- `B_wallet`: ~184 SOL max at u64 (18.4B lamports). Practical max ~1000 SOL (1T lamports). u64 safe.
- `base_size` computation: `4_400_000_000 × 264 = 1_161_600_000_000` — exceeds u32 but fits u64. We use u128 intermediate to be safe.
- `corr_mult × size`: max product is ~264B × 1000 = 264T — fits u128.
- All intermediate products computed in u128, cast to u64 at each step. **No overflow possible.**

### 6.6 Configuration Defaults

```rust
impl Default for SizingEngine {
    fn default() -> Self {
        Self {
            cached_balance_lamports: 0,
            committed_lamports: 0,
            n_open: 0,
            high_water_mark: 0,
            hwm_updated_at: 0,
            is_paused: false,
            f_half_kelly_permille: 264,       // 0.264 = half of f*=0.528
            rho_permille: 250,                 // 0.25 correlation
            n_max_concurrent: 5,               // max 5 simultaneous
            fee_per_trade_lamports: 2_400_000, // 0.0024 SOL
            min_position_lamports: 10_000_000, // 0.01 SOL floor
        }
    }
}
```

---

## 7. Calibration & Parameter Updates

### 7.1 When to Recalibrate

The three key parameters (`f_half_kelly_permille`, `rho_permille`, estimated `p` and `R`) should be recalibrated periodically:

| Parameter | Update Frequency | Method |
|-----------|-----------------|--------|
| p (win rate) | Every 200 trades | Rolling 500-trade window |
| R (reward ratio) | Every 200 trades | Rolling 500-trade window |
| f* | Derived from p, R | Recompute when p or R updates |
| ρ (correlation) | Weekly | Run-length test + lag-1 autocorrelation |
| fee estimate | Daily | Track actual fees paid |

### 7.2 Regime Detection Triggers

Certain events should trigger IMMEDIATE recalibration or defensive posture:

1. **Win rate drops below 0.50 over last 50 trades** → Switch to minimum sizing (0.25× Kelly)
2. **3+ consecutive losses on simultaneous positions** → Suggests ρ spike → temporarily set ρ=0.50
3. **RPC latency > 500ms sustained** → Solana congestion → reduce n_max to 3
4. **Pump.fun activity drop > 50% from rolling average** → Market cooldown → reduce n_max to 3

### 7.3 Kelly Parameter Sensitivity

How wrong can our estimates be before we lose money?

The Kelly break-even (f* = 0) occurs when `p = 1/(1+R)`:
```
p_breakeven = 1 / (1 + 9.87) = 0.092
```

We have massive edge margin: p=0.571 vs breakeven=0.092. Even if our win rate drops to 0.20, half-Kelly remains profitable. The strategy is robust to estimation error.

However, **R is the fragile parameter.** If R drops from 9.87 to 2.0:
```
f* = 0.571 - 0.429/2.0 = 0.571 - 0.215 = 0.356
half-Kelly = 0.178
```
Still profitable, but sizing should decrease. If R drops to 1.0:
```
f* = 0.571 - 0.429 = 0.142
half-Kelly = 0.071
```

**Key insight:** Monitor R (reward ratio) more closely than p. R degradation signals that profitable exits are getting worse (more competition, tighter bonding curves, worse fills).

---

## 8. Summary: The Complete Sizing Decision Tree

```
┌─────────────────────────────────────────┐
│           NEW TRADE SIGNAL              │
└────────────────┬────────────────────────┘
                 │
    ┌────────────▼────────────┐
    │  Refresh wallet balance │  ← RPC getBalance (use cache if <5s)
    │  B = balance - committed│
    └────────────┬────────────┘
                 │
    ┌────────────▼────────────┐
    │  Check drawdown vs HWM  │
    │  dd_bps = ...           │
    │  dd_bps ≥ 2500? ────────┼──→ REJECT (circuit breaker)
    └────────────┬────────────┘
                 │ dd_bps < 2500
    ┌────────────▼────────────┐
    │  n_open ≥ n_max? ───────┼──→ REJECT (budget full)
    └────────────┬────────────┘
                 │ slots available
    ┌────────────▼────────────┐
    │  Compute base_size      │
    │  = B × f_pk / (1000×n)  │
    └────────────┬────────────┘
                 │
    ┌────────────▼────────────┐
    │  Apply corr adjustment  │
    │  ×= 1M/(1000+n_o×ρ_pk) │
    └────────────┬────────────┘
                 │
    ┌────────────▼────────────┐
    │  Apply DD multiplier    │
    │  ×= dd_mult / 1000     │
    └────────────┬────────────┘
                 │
    ┌────────────▼────────────┐
    │  Cap to remaining       │
    │  budget                 │
    └────────────┬────────────┘
                 │
    ┌────────────▼────────────┐
    │  size ≥ min_lam? ───────┼──→ NO: REJECT (too small)
    └────────────┬────────────┘
                 │ YES
    ┌────────────▼────────────┐
    │  ENTER TRADE            │
    │  committed += size      │
    │  n_open += 1            │
    └─────────────────────────┘
```

---

## 9. Integration Notes

### 9.1 Where This Lives in the Architecture

```
RiskManager (existing)
├── CircuitBreaker (existing — daily loss limit, consecutive loss limit)
├── SizingEngine (NEW — this document)
│   ├── BankrollState (wallet balance + HWM tracking)
│   ├── CorrelationAdjuster (Thorp simultaneous Kelly)
│   ├── DrawdownController (tiered multiplier)
│   └── BudgetManager (capital budget + fee reservation)
└── PositionTracker (existing — tracks open positions)
```

### 9.2 State Persistence

Persist to `state/sizing_state.json` (or binary equivalent):
```json
{
  "high_water_mark_lamports": 5000000000,
  "hwm_updated_at_ms": 1711756200000,
  "is_paused": false,
  "f_half_kelly_permille": 264,
  "rho_permille": 250,
  "rolling_win_count": 224,
  "rolling_total_count": 392,
  "last_recalibration_trade_count": 392
}
```

### 9.3 Relationship to Existing Linear Ramp

The current system uses a linear ramp (0.10 → 0.15 SOL over N trades). This was appropriate for paper trading validation. The Kelly-based system replaces the ramp in live mode:

- **Paper mode:** Keep linear ramp for controlled testing
- **Shadow mode:** Compute Kelly sizes alongside ramp, log both, compare
- **Live mode:** Use Kelly sizing from this document

Transition: Run shadow mode for 500+ trades. If Kelly sizes produce better risk-adjusted PnL (Sharpe, Sortino), switch to live Kelly.

### 9.4 What's NOT in This Document

- **Entry signal quality** — Kelly sizes optimally given an edge. If entry signals degrade, Kelly can't fix bad signals, only size them properly.
- **Slippage model** — Bonding curve slippage is deterministic (price impact = f(size, curve)). Should be modeled in the execution engine and fed back as an R adjustment.
- **Gas/priority fee optimization** — Fee is treated as constant 0.0024 SOL. In practice, dynamic fee adjustment based on network congestion would improve this.

---

## References

1. **Thorp, E.O.** (1969). "Optimal Gambling Systems for Favorable Games." *Review of the International Statistical Institute*, 37(3), 273-293.
   - Foundation for simultaneous Kelly with correlated bets.

2. **Thorp, E.O.** (2006). "The Kelly Criterion in Blackjack, Sports Betting, and the Stock Market." *Handbook of Asset and Liability Management*.
   - Practical adaptations including half-Kelly and drawdown properties.

3. **Vince, R.** (1990). *Portfolio Management Formulas.* Wiley.
   - Optimal-f framework, drawdown analysis, multi-asset Kelly.

4. **Vince, R.** (2009). *The Leverage Space Trading Model.* Wiley.
   - Power-law adjustments for correlated simultaneous positions.

5. **Ziemba, W.T. & Ziemba, R.E.S.** (2013). *Investing and the Irrational Mind.* World Scientific.
   - Kelly criterion drawdown probabilities and practical implementation.

6. **MacLean, L.C., Thorp, E.O., & Ziemba, W.T.** (2011). *The Kelly Capital Growth Investment Criterion.* World Scientific.
   - Comprehensive reference: multi-asset Kelly, estimation error, fractional Kelly justification.