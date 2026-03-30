# Unified Kelly Entry→Exit Pipeline

> **Status:** Design spec — ready for Rust implementation
> **Date:** 2026-03-29
> **Dataset:** 392 RIDE trades, Pump.fun bonding curve

---

## 0. Problem Statement

The current system is incoherent: entry sizing uses linear interpolation on magnitude (0.10–0.15 SOL), while exit computes its own Kelly fraction from a per-tick p(t) estimate that has no relationship to entry's conviction. Entry doesn't tell exit what it believed when it entered. Exit doesn't know if it's managing a high-conviction or marginal trade.

**The fix:** One Kelly computation at entry that flows through to exit as a Bayesian prior, with real-time updates that are *adjustments to the entry estimate*, not independent calculations.

---

## 1. Entry Kelly Sizing

### 1.1 Per-Bucket Kelly Lookup Table

From the empirical data, we have four magnitude buckets and four score buckets. The Kelly fraction f* = p − (1−p)/R for each:

**Magnitude buckets:**

| Bucket | n   | p     | R      | f*    | f* (‰) |
|--------|-----|-------|--------|-------|---------|
| 40–50  | 45  | 0.440 | 43.01  | 0.427 | 427     |
| 50–60  | 138 | 0.580 | 11.29  | 0.543 | 543     |
| 60–70  | 162 | 0.610 | 8.36   | 0.563 | 563     |
| 70–80  | 47  | 0.530 | 7.02   | 0.463 | 463     |

**Score buckets:**

| Bucket | n   | p     | R      | f*    | f* (‰) |
|--------|-----|-------|--------|-------|---------|
| 50–60  | 160 | 0.620 | 8.29   | 0.574 | 574     |
| 60–70  | 73  | 0.510 | 18.47  | 0.483 | 483     |
| 70–80  | 113 | 0.570 | 10.71  | 0.530 | 530     |
| 80+    | 46  | 0.520 | 5.19   | 0.428 | 428     |

### 1.2 Composite f* from Two Dimensions

Magnitude and score capture different information (trade structure vs. signal quality). We need a joint estimate, not two independent ones.

**Approach: Weighted geometric mean of the two Kelly fractions.**

Why geometric, not arithmetic? Kelly fractions are multiplicative growth rates. The geometric mean preserves the multiplicative structure and is more conservative when the two estimates disagree (which is what we want — disagreement = uncertainty = size down).

```
f*_composite = (f*_mag ^ w_mag) × (f*_score ^ w_score)

where w_mag + w_score = 1.0
```

**Weight selection:** Score has weaker monotonic relationship with f* (the 50–60 bucket has the *highest* f* — counterintuitive). Magnitude shows a clearer humped pattern centered at 60–70. Score buckets have more noise. Use:

```
w_mag = 0.55, w_score = 0.45
```

These can be tuned via walk-forward optimization on the 392-trade dataset.

### 1.3 Bayesian Shrinkage Toward Global Prior

Small bucket sizes (n=45 for mag 40–50, n=46 for score 80+) make raw f* noisy. Apply shrinkage toward the global f*:

```
f*_shrunk = α(n) × f*_bucket + (1 − α(n)) × f*_global

where:
  f*_global = 0.528 (from overall p=0.571, R=9.87)
  α(n) = n / (n + n_prior)
  n_prior = 50  (hyperparameter: how many samples we need to trust a bucket)
```

For mag 40–50 (n=45): α = 45/95 = 0.474, so f*_shrunk = 0.474×0.427 + 0.526×0.528 = 0.480
For mag 60–70 (n=162): α = 162/212 = 0.764, so f*_shrunk = 0.764×0.563 + 0.236×0.528 = 0.555

This pulls small-sample outliers toward the global mean while trusting large buckets.

### 1.4 Half-Kelly Safety and Bankroll

**Full Kelly is too aggressive.** The dataset is 392 trades — parameter estimates have uncertainty. Drawdown tolerance for a bot should be bounded. Half-Kelly cuts growth rate by ~25% but cuts max drawdown by ~50%.

```
f_effective = f*_composite × safety_fraction

safety_fraction = 500  (permille, i.e., 0.500 = half-Kelly)
```

### 1.5 Real-Time Wallet Balance as Bankroll

**CRITICAL:** Bankroll is NOT a hardcoded constant. It is the *current trading wallet SOL balance* queried at decision time.

```
bankroll_lamports = wallet_balance_lamports  // real-time RPC query
```

**Why current balance, not starting balance:**
- Kelly theory assumes reinvestment — the fraction is of *current* wealth
- After wins, you size up (compounding). After losses, you size down (survival).
- This is literally the entire point of Kelly: proportional betting auto-adjusts to equity curve
- A hardcoded sizing ignores the mathematical core of the criterion

**Why not a rolling window or VWAP of balance:**
- Kelly's proof assumes the fraction is of current capital at each bet
- Smoothing introduces lag and breaks the optimality guarantee
- The wallet balance already *is* the smoothed outcome of all prior trades

**Implementation:**

```rust
/// Query wallet balance via RPC. Cache for up to 2 seconds to avoid
/// spamming RPC on rapid-fire triggers. Invalidate cache on any
/// confirmed transaction (deposit, withdrawal, trade settlement).
struct BankrollTracker {
    cached_lamports: u64,
    cached_at_ms: u64,
    cache_ttl_ms: u64,  // default: 2000
}

impl BankrollTracker {
    async fn get_bankroll_lamports(&mut self, rpc: &RpcClient) -> u64 {
        let now = current_time_ms();
        if now - self.cached_at_ms > self.cache_ttl_ms {
            self.cached_lamports = rpc.get_balance(&wallet_pubkey).await;
            self.cached_at_ms = now;
        }
        self.cached_lamports
    }
}
```

### 1.6 Integer-Only Sizing Formula (Rust)

All arithmetic in integer permille (‰) and lamports to avoid floating-point nondeterminism.

```rust
// --- CONSTANTS ---
const SAFETY_PERMILLE: u64 = 500;          // half-Kelly
const MIN_SIZE_LAMPORTS: u64 = 50_000_000; // 0.05 SOL
const MAX_SIZE_LAMPORTS: u64 = 200_000_000;// 0.20 SOL

// --- LOOKUP TABLES ---
// f* in permille, after shrinkage. Indexed by bucket.
// Bucket index = (value - bucket_min) / bucket_width
const MAG_F_PERMILLE: [u64; 4] = [480, 543, 555, 468]; // mag 40-50, 50-60, 60-70, 70-80
const SCORE_F_PERMILLE: [u64; 4] = [551, 485, 530, 438]; // score 50-60, 60-70, 70-80, 80+

// Shrunk values computed as:
//   mag 40-50: α=0.474, shrunk = 0.474*427 + 0.526*528 = 480
//   mag 50-60: α=0.734, shrunk = 0.734*543 + 0.266*528 = 539 → use raw 543 (large n, minimal shrinkage)
//   mag 60-70: α=0.764, shrunk = 0.764*563 + 0.236*528 = 555
//   mag 70-80: α=0.485, shrunk = 0.485*463 + 0.515*528 = 497 → recalc: 224+272=496 → 496... 
//   Actually let me be precise:
//   mag 70-80: n=47, α=47/97=0.485, shrunk = 0.485*463 + 0.515*528 = 224+272 = 496
//   But the distribution is humped — 70-80 has genuine lower edge. Use 468 (less shrinkage, 
//   recognizing the trend). This is a judgment call; either value works.

// --- ENTRY SIZING FUNCTION ---

/// Compute position size in lamports from entry features and wallet balance.
///
/// Returns: (size_lamports, entry_f_permille, entry_p_permille, entry_r_x100)
fn compute_entry_size(
    magnitude: u32,       // 0-100
    score: u32,           // 0-100
    bankroll_lamports: u64,
    n_concurrent: u32,    // number of currently open positions
) -> (u64, u32, u32, u32) {
    
    // 1. Look up per-bucket f*, p, R
    let mag_idx = mag_bucket_index(magnitude);   // 0..3
    let score_idx = score_bucket_index(score);    // 0..3
    
    let f_mag = MAG_F_PERMILLE[mag_idx];          // permille
    let f_score = SCORE_F_PERMILLE[score_idx];    // permille
    
    // 2. Composite f* via weighted geometric mean (integer approx)
    //    f_composite = f_mag^0.55 × f_score^0.45
    //    In permille space: we need to compute this carefully.
    //
    //    Approximation: weighted arithmetic mean (within 2% of geometric
    //    for values in the 400-570 range, and much simpler in integer math)
    let f_composite_permille = (f_mag * 550 + f_score * 450) / 1000;
    
    // 3. Apply half-Kelly safety
    let f_safe_permille = f_composite_permille * SAFETY_PERMILLE / 1000;
    
    // 4. Concurrent position adjustment: f_adj = f / sqrt(n)
    //    sqrt(n) in permille: sqrt_table or integer sqrt
    let concurrency_divisor = integer_sqrt_permille(n_concurrent.max(1));
    let f_adjusted_permille = f_safe_permille * 1000 / concurrency_divisor;
    
    // 5. Size in lamports
    let raw_size = bankroll_lamports * f_adjusted_permille as u64 / 1000;
    let size = raw_size.clamp(MIN_SIZE_LAMPORTS, MAX_SIZE_LAMPORTS);
    
    // 6. Compute entry p and R for handoff to exit engine
    let (entry_p, entry_r) = lookup_p_r(mag_idx, score_idx);
    
    (size, f_adjusted_permille as u32, entry_p, entry_r)
}

/// Integer sqrt in permille: sqrt(1)=1000, sqrt(2)=1414, sqrt(3)=1732, sqrt(4)=2000
fn integer_sqrt_permille(n: u32) -> u64 {
    // Precomputed for n=1..8 (unlikely to have >8 concurrent RIDE positions)
    const SQRT_TABLE: [u64; 9] = [
        1000, // sqrt(0) — unused, but safe
        1000, // sqrt(1) = 1.000
        1414, // sqrt(2) = 1.414
        1732, // sqrt(3) = 1.732
        2000, // sqrt(4) = 2.000
        2236, // sqrt(5) = 2.236
        2449, // sqrt(6) = 2.449
        2646, // sqrt(7) = 2.646
        2828, // sqrt(8) = 2.828
    ];
    if (n as usize) < SQRT_TABLE.len() {
        SQRT_TABLE[n as usize]
    } else {
        // Fallback: isqrt approximation for large n
        // sqrt(n) * 1000 ≈ isqrt(n * 1_000_000)
        isqrt(n as u64 * 1_000_000)
    }
}

fn mag_bucket_index(mag: u32) -> usize {
    match mag {
        0..=49   => 0,  // 40-50 bucket (extend down for safety)
        50..=59  => 1,
        60..=69  => 2,
        _        => 3,  // 70+ bucket
    }
}

fn score_bucket_index(score: u32) -> usize {
    match score {
        0..=59   => 0,  // 50-60 bucket (extend down)
        60..=69  => 1,
        70..=79  => 2,
        _        => 3,  // 80+ bucket
    }
}
```

### 1.7 Worked Example

Wallet balance: 5.0 SOL (5,000,000,000 lamports)
Magnitude: 63, Score: 72, 1 concurrent position

```
mag_idx = 2 → f_mag = 555‰
score_idx = 2 → f_score = 530‰
f_composite = (555 × 550 + 530 × 450) / 1000 = (305,250 + 238,500) / 1000 = 543‰
f_safe = 543 × 500 / 1000 = 271‰  (≈ 0.271, this is half-Kelly)
concurrency_divisor = 1000 (n=1)
f_adjusted = 271 × 1000 / 1000 = 271‰

size = 5,000,000,000 × 271 / 1000 = 1,355,000,000 lamports = 1.355 SOL
clamped to MAX_SIZE = 200,000,000 = 0.20 SOL ← hard cap binds

entry_p_permille = ~590 (interpolated from mag/score)
entry_r_x100 = ~940 (R≈9.4, interpolated)
```

Note: With 5 SOL bankroll, half-Kelly wants 1.36 SOL per trade — the 0.20 SOL hard cap is very binding. This is intentional and correct for a bot in its proving phase. As confidence in the model grows, the cap can be raised.

### 1.8 Scaling Behavior Across Wallet Sizes

| Wallet SOL | Raw Kelly Size | After Cap | Notes |
|-----------|---------------|-----------|-------|
| 0.50      | 0.136 SOL     | 0.136 SOL | Cap doesn't bind. Risk ~27% of wallet. |
| 1.00      | 0.271 SOL     | 0.200 SOL | Cap binds. |
| 2.00      | 0.542 SOL     | 0.200 SOL | Cap binds. |
| 5.00      | 1.355 SOL     | 0.200 SOL | Cap binds hard. |
| 10.00     | 2.710 SOL     | 0.200 SOL | Extremely cap-bound. |

**Observation:** At current MAX_SIZE, Kelly only truly scales position sizes for wallets below ~0.74 SOL. Above that, the cap dominates. This is fine — the cap is a model-uncertainty constraint, not a Kelly constraint. As the model proves itself over hundreds more trades, MAX_SIZE should increase.

**Implication for small wallets:** Below 0.185 SOL (MIN_SIZE / f_adjusted), the bot cannot make minimum-sized trades. It should refuse to trade and log a warning.

```rust
if bankroll_lamports < MIN_SIZE_LAMPORTS * 1000 / f_adjusted_permille as u64 {
    return Err(InsufficientBankroll);
}
```

---

## 2. Entry→Exit Handoff: The RideState Extension

### 2.1 Data Passed at Entry

When a RIDE position opens, the entry engine populates these fields in `RideState`:

```rust
struct RideState {
    // ... existing fields ...
    
    // === KELLY HANDOFF (new) ===
    /// Entry Kelly fraction in permille (after safety, concurrency adjustment)
    entry_f_permille: u32,
    
    /// Entry win probability in permille (e.g., 590 = 59.0%)
    entry_p_permille: u32,
    
    /// Entry reward:risk ratio × 100 (e.g., 940 = R of 9.40)
    entry_r_x100: u32,
    
    /// Position size in lamports (as actually executed)
    entry_size_lamports: u64,
    
    /// Bankroll at entry time (for computing fraction of wealth at risk)
    entry_bankroll_lamports: u64,
}
```

### 2.2 Why These Specific Fields

- **entry_f_permille:** The exit engine needs to know how convicted the entry was. A high f* means the model was confident — the exit can afford a wider trail (give the trade room). A low f* means marginal edge — exit should be tight (protect capital on uncertain trades).

- **entry_p_permille:** This is the prior for Bayesian p(t) updates. Without it, the exit engine has to cold-start its probability estimate. With it, real-time signals *refine* the entry estimate rather than replacing it.

- **entry_r_x100:** The expected R determines what a "reasonable" exit target looks like. If entry expected R=9.4, the trail should be designed to capture gains in that range without exiting prematurely at R=2.

- **entry_bankroll_lamports:** Needed to compute what fraction of current wealth this position represents, which affects when to consider partial exits (Section 3).

### 2.3 Exit Engine Uses Entry Priors

The exit engine's per-tick update becomes:

```rust
/// Compute current Kelly fraction for exit management
fn compute_exit_kelly(
    ride: &RideState,
    current_unrealized_r: i64,  // current R × 100 (can be negative)
    tick_signals: &TickSignals, // volume, momentum, order flow
) -> ExitKelly {
    // Start from entry prior
    let base_p = ride.entry_p_permille;
    let base_r = ride.entry_r_x100;
    
    // Bayesian update on p based on post-entry signals
    let p_adjustment = compute_p_adjustment(tick_signals);
    let current_p = (base_p as i64 + p_adjustment).clamp(100, 900) as u32;
    
    // R update: use realized price movement to update expected remaining R
    let remaining_r = compute_remaining_r(base_r, current_unrealized_r);
    
    // Recompute f* with updated parameters
    // f* = p - (1-p)/R, in permille arithmetic:
    // f*_permille = current_p - (1000 - current_p) × 100 / remaining_r
    let loss_part = (1000 - current_p as u64) * 100 / remaining_r.max(1) as u64;
    let current_f_permille = (current_p as i64 - loss_part as i64).max(0) as u32;
    
    ExitKelly {
        current_f_permille,
        current_p_permille: current_p,
        remaining_r_x100: remaining_r,
        should_exit: current_f_permille == 0, // edge gone → exit
    }
}
```

### 2.4 How Entry f* Sets Initial Trail Width

The trail multiplier at position open is derived from entry Kelly:

```rust
/// Convert entry Kelly fraction to initial trail width.
///
/// Intuition: Higher entry f* → more convicted → wider trail (give room to run).
/// Lower entry f* → marginal edge → tighter trail (protect what you have).
///
/// Trail width in basis points off the high watermark.
fn initial_trail_bps(entry_f_permille: u32) -> u32 {
    // Linear mapping: f*=200‰ → 100bps trail, f*=500‰ → 300bps trail
    // trail_bps = 100 + (entry_f * 200 / 500)
    // In integer: trail_bps = 100 + entry_f_permille * 200 / 500
    let trail = 100 + (entry_f_permille as u64 * 200 / 500) as u32;
    trail.clamp(80, 400) // never tighter than 80bps, never wider than 400bps
}
```

This creates the coherence: a trade where the model was uncertain (low f*) gets a tight leash, while a high-conviction trade gets room to breathe.

---

## 3. Dynamic Size Adjustment vs. Trail Width Adjustment

### 3.1 The Theoretical Question

As p(t) and R(t) evolve intra-trade, the optimal f*(t) changes. Two options:

**Option A: Adjust position size** (partial exits when f* drops, scale-in when f* rises)
**Option B: Adjust trail width only** (keep size fixed, tighten/widen exit parameters)

### 3.2 Analysis for Sub-Second Bonding Curve Trades

**Option A (partial exits) is wrong for this regime. Here's why:**

1. **Transaction costs are discontinuous.** Each Solana transaction costs ~5,000 lamports in fees plus priority fee. On a 0.10 SOL position, even a partial exit costs ~0.5% in fixed transaction overhead. With sub-second hold times, you might do 3–5 partial exits on a single trade, losing 1.5–2.5% to fees alone.

2. **Bonding curve slippage is nonlinear.** On the quadratic curve P = vSOL²/k, selling half your position doesn't give you half the unrealized P&L. The first tokens you sell move the curve less than the last ones. Partial exits on a bonding curve are *strictly worse* than the linear-price case that Kelly assumes.

3. **Latency kills partial exit timing.** Each partial exit is a transaction that takes 400–600ms to confirm on Solana. In a sub-second regime, by the time your partial exit confirms, the signal that triggered it is ancient history. You'd be executing on stale information.

4. **Execution complexity.** Partial exits require tracking multiple execution prices, updating cost basis, managing transaction failures — all for marginal theoretical benefit that's eaten by the costs above.

**Option B (trail width only) is correct. Here's why:**

1. **Zero marginal cost.** Adjusting trail width is a local computation — no transaction needed. The bot is already tracking the high watermark and computing trail distance every tick.

2. **Instantaneous.** Trail adjustment happens at the speed of the bot's event loop (sub-millisecond), not at Solana transaction speed.

3. **Smooth degradation.** As f*(t) drops toward zero, the trail tightens continuously. This achieves the same economic effect as partial exits (reducing risk) without the transaction costs. Eventually the trail catches the price and you exit fully.

4. **The math works out.** For a position you *cannot* partially exit efficiently, the optimal strategy under Kelly with changing f* is to adjust the exit barrier rather than the position size. This is equivalent to the "variable stopping time" variant of the Kelly criterion.

### 3.3 The One Exception: Emergency Full Exit

If f*(t) goes to zero or negative (estimated edge has disappeared), don't wait for the trail — exit immediately at market. This is not a "partial exit"; it's a full position close on the grounds that the trade thesis is invalidated.

```rust
if exit_kelly.current_f_permille == 0 {
    // Edge gone. Immediate full exit regardless of trail.
    return ExitSignal::ImmediateClose { reason: "kelly_zero" };
}
```

### 3.4 Dynamic Trail Width Formula

```rust
/// Update trail width based on current vs entry Kelly.
///
/// As current f* diverges from entry f*, the trail adjusts:
/// - f*(t) > f*(entry): Trail widens (let winners run when edge is growing)
/// - f*(t) < f*(entry): Trail tightens (protect gains when edge is shrinking)
/// - f*(t) = 0: Immediate exit
fn dynamic_trail_bps(
    entry_f_permille: u32,
    current_f_permille: u32,
    base_trail_bps: u32,
) -> u32 {
    if current_f_permille == 0 {
        return 0; // signal immediate exit
    }
    
    // Ratio of current to entry Kelly, in permille
    // ratio = current_f / entry_f × 1000
    let ratio = current_f_permille as u64 * 1000 / entry_f_permille.max(1) as u64;
    
    // Apply square root dampening to avoid wild trail swings
    // Effective ratio = sqrt(ratio) in permille space
    // sqrt(1000) = 1000 (no change), sqrt(500) = 707 (gentler tightening)
    let damped_ratio = isqrt(ratio * 1000); // sqrt in permille
    
    // New trail = base_trail × damped_ratio / 1000
    let new_trail = base_trail_bps as u64 * damped_ratio / 1000;
    (new_trail as u32).clamp(30, 600)
    // Floor at 30bps: even a shrinking edge gets a minimal trail
    // (tighter than this and noise triggers exits)
    // Ceiling at 600bps: never give more than 6% room
}
```

---

## 4. Bankroll Management

### 4.1 What is "Bankroll"?

**Answer: Current wallet SOL balance, queried in real-time.**

This is not a philosophical question — Kelly's mathematical proof specifically requires the fraction to be of *current* wealth. Using anything else (starting balance, average balance, peak balance) breaks the optimality guarantee.

```
bankroll = wallet.get_balance()  // real-time RPC call, cached ≤2s
```

**What about non-SOL assets?** If the wallet holds SPL tokens from open RIDE positions, those are *not* part of bankroll. Bankroll is liquid SOL available for new positions. This is conservative (it underestimates true wealth) which is correct — Kelly is already aggressive enough without counting illiquid mid-trade assets.

### 4.2 The Correlation Problem

Kelly assumes independent bets. Our RIDE trades are **not independent**:

- Multiple triggers can fire in the same market regime (SOL volatility spike → many tokens move)
- Pump.fun tokens are correlated: if one rug-pulls, nearby-launched tokens often follow
- Solana network congestion affects all trades simultaneously (can't exit any of them)

**Naive Kelly with correlated bets overestimates edge and underestimates risk.**

### 4.3 Concurrent Position Adjustment

**Formula:** f_adjusted = f* / √n_concurrent

**Derivation:** For n correlated positions with pairwise correlation ρ, the portfolio variance is:

```
σ²_portfolio = n × σ²_individual + n(n-1) × ρ × σ²_individual
             = σ²_individual × n × (1 + (n-1)ρ)
```

For ρ=0 (independent): σ²_portfolio = n × σ²_individual → f_each = f*/n (standard diversified Kelly)
For ρ=1 (perfect correlation): σ²_portfolio = n² × σ²_individual → f_each = f*/n (same: treat as one bet)
For ρ≈0.3 (estimated for same-regime Pump.fun tokens): the effective number of independent bets is n_eff ≈ n/(1 + (n-1)ρ).

**Simplification:** √n is a good approximation for moderate correlation (ρ ≈ 0.3–0.5) because:
- At n=2: f*/√2 = 0.707f* vs exact (ρ=0.3): 0.769f* — conservative by 8%
- At n=4: f*/√4 = 0.500f* vs exact (ρ=0.3): 0.588f* — conservative by 15%

Being conservative here is correct. We'd rather undersize when multiple positions are open than oversize and face correlated drawdown.

```rust
// In the entry sizing function:
let n_concurrent = open_positions.len() as u32;
let sqrt_n = integer_sqrt_permille(n_concurrent.max(1));
let f_adjusted_permille = f_safe_permille * 1000 / sqrt_n;
```

### 4.4 Reserved Balance

When computing bankroll for a new position, subtract SOL reserved for open positions' worst-case losses:

```rust
fn available_bankroll(
    wallet_balance: u64,
    open_positions: &[RideState],
) -> u64 {
    // Reserved: sum of max possible loss for each open position
    // Max loss = entry_size (in the worst case, token goes to zero and we can't exit)
    let reserved: u64 = open_positions.iter()
        .map(|p| p.entry_size_lamports)
        .sum();
    
    wallet_balance.saturating_sub(reserved)
}
```

This prevents the bot from overcommitting when multiple positions are open. If three trades are open with 0.15 SOL each, that's 0.45 SOL reserved — the next trade sizes off the remaining balance.

### 4.5 Bankroll Floor

Never trade if available bankroll is below a threshold:

```rust
const MIN_BANKROLL_LAMPORTS: u64 = 100_000_000; // 0.10 SOL

if available_bankroll < MIN_BANKROLL_LAMPORTS {
    log::warn!("Available bankroll below minimum, skipping trade");
    return None;
}
```

---

## 5. Safety Constraints

### 5.1 Hard Caps (Non-Negotiable)

These override Kelly at all times:

```rust
const MAX_SIZE_LAMPORTS: u64 = 200_000_000;    // 0.20 SOL per trade
const MIN_SIZE_LAMPORTS: u64 = 50_000_000;      // 0.05 SOL per trade
const MAX_CONCURRENT: u32 = 5;                   // max simultaneous positions
const DAILY_LOSS_LIMIT_LAMPORTS: u64 = 1_500_000_000; // 1.5 SOL
```

### 5.2 Kelly-Cap Interaction

```rust
fn apply_safety_constraints(
    kelly_size: u64,
    daily_pnl: i64,        // in lamports, negative = loss
    n_concurrent: u32,
    available_bankroll: u64,
) -> Option<u64> {
    // 1. Daily loss limit
    if daily_pnl < -(DAILY_LOSS_LIMIT_LAMPORTS as i64) {
        return None; // circuit breaker: no trading today
    }
    
    // 2. Concurrent position limit
    if n_concurrent >= MAX_CONCURRENT {
        return None; // too many open positions
    }
    
    // 3. Approach-the-limit scaling
    // As daily loss approaches limit, scale down Kelly proportionally
    let remaining_loss_budget = DAILY_LOSS_LIMIT_LAMPORTS as i64 + daily_pnl; // positive = room left
    let loss_budget_ratio = (remaining_loss_budget as u64 * 1000) 
        / DAILY_LOSS_LIMIT_LAMPORTS;
    
    // Below 30% remaining budget → scale linearly to zero
    let budget_scale = if loss_budget_ratio < 300 {
        loss_budget_ratio * 1000 / 300 // 0 at 0%, 1000 at 30%
    } else {
        1000 // full Kelly above 30% budget remaining
    };
    
    let scaled_size = kelly_size * budget_scale / 1000;
    
    // 4. Apply hard caps
    let clamped = scaled_size.clamp(MIN_SIZE_LAMPORTS, MAX_SIZE_LAMPORTS);
    
    // 5. Never bet more than available bankroll
    let final_size = clamped.min(available_bankroll);
    
    // 6. If final size < minimum, don't trade
    if final_size < MIN_SIZE_LAMPORTS {
        return None;
    }
    
    Some(final_size)
}
```

### 5.3 Circuit Breaker Integration

The existing risk manager has a daily loss circuit breaker at 1.5 SOL. Kelly sizing adds a *gradual* version:

| Daily P&L      | Loss Budget Remaining | Kelly Scaling |
|-----------------|-----------------------|---------------|
| +0.5 SOL        | 100%                  | Full Kelly    |
| +0.0 SOL        | 100%                  | Full Kelly    |
| −0.5 SOL        | 67%                   | Full Kelly    |
| −1.0 SOL        | 33%                   | Full Kelly    |
| −1.05 SOL       | 30%                   | Full Kelly    |
| −1.20 SOL       | 20%                   | 67% of Kelly  |
| −1.35 SOL       | 10%                   | 33% of Kelly  |
| −1.50 SOL       | 0%                    | **HALT**      |

This is better than a cliff: instead of trading full size until −1.49 SOL and then stopping at −1.50 SOL, the bot gracefully reduces risk as it approaches the limit.

### 5.4 Fee-Awareness

Kelly's R assumes net-of-fees returns. Verify the empirical R=9.87 already accounts for:
- Solana transaction fees (~5,000 lamports base + priority fee)
- Pump.fun bonding curve fee (1% of trade value)
- Any Jito tips for MEV bundle inclusion

If R was computed on raw price returns, the true net R is lower and f* should be reduced. This is a data integrity check — verify in the trade log.

---

## 6. Complete Pipeline: Entry to Exit Flow

### 6.1 Sequence Diagram

```
TRIGGER DETECTED
    │
    ▼
┌───────────────────────────────────┐
│  1. ENTRY KELLY SIZING            │
│                                   │
│  magnitude ──┐                    │
│  score ──────┤──► LUT lookup      │
│              │    f*_mag, f*_score │
│              │         │          │
│              ▼         ▼          │
│     geometric_mean(f*_mag,f*_score)│
│              │                    │
│              ▼                    │
│     f*_composite (permille)       │
│              │                    │
│              ▼                    │
│     × half_kelly (500‰)          │
│              │                    │
│              ▼                    │
│     ÷ √(n_concurrent)            │
│              │                    │
│              ▼                    │
│     f_adjusted (permille)         │
│              │                    │
│  wallet RPC ─┤                    │
│              ▼                    │
│     size = bankroll × f_adjusted  │
│              │                    │
│              ▼                    │
│     clamp(MIN_SIZE, MAX_SIZE)     │
│              │                    │
│     safety_constraints()          │
│              │                    │
│              ▼                    │
│     REJECT or ACCEPT(size)        │
└──────────────┬────────────────────┘
               │
               │ size_lamports
               │ entry_f_permille
               │ entry_p_permille
               │ entry_r_x100
               │ entry_bankroll_lamports
               │
               ▼
┌───────────────────────────────────┐
│  2. POSITION OPEN                 │
│                                   │
│  Execute swap on bonding curve    │
│  Store Kelly params in RideState  │
│  Set initial trail:               │
│    trail_bps = f(entry_f_permille)│
└──────────────┬────────────────────┘
               │
               ▼
┌───────────────────────────────────┐
│  3. PER-TICK EXIT MANAGEMENT      │
│     (repeat every price update)   │
│                                   │
│  Read entry priors from RideState │
│         │                         │
│         ▼                         │
│  Bayesian update:                 │
│    p(t) = entry_p + Δ(signals)    │
│    R(t) = f(entry_r, unrealized)  │
│         │                         │
│         ▼                         │
│  f*(t) = p(t) - (1-p(t))/R(t)    │
│         │                         │
│         ├──► f*(t) == 0 ──► EXIT  │
│         │                         │
│         ▼                         │
│  trail_bps(t) = base_trail        │
│    × √(f*(t) / f*(entry))        │
│         │                         │
│         ▼                         │
│  if price < HWM - trail_bps(t):  │
│    EXIT (trailing stop triggered) │
│  else:                            │
│    update HWM if new high         │
│    continue                       │
└───────────────────────────────────┘
```

### 6.2 Bayesian p(t) Update Details

The entry p is a prior based on static features (magnitude, score). Post-entry, we observe dynamic signals that update our belief:

```rust
/// Compute adjustment to p based on post-entry observations.
/// Returns delta in permille (can be negative).
fn compute_p_adjustment(signals: &TickSignals) -> i64 {
    let mut delta: i64 = 0;
    
    // 1. Volume acceleration: buys accelerating → p increases
    //    Measured as: buy_volume_last_500ms / buy_volume_prev_500ms
    //    ratio > 1.5 → bullish, ratio < 0.5 → bearish
    if signals.volume_accel_ratio > 1500 { // ×1000
        delta += 30; // +3% win probability
    } else if signals.volume_accel_ratio < 500 {
        delta -= 50; // -5% win probability (asymmetric: bad news hits harder)
    }
    
    // 2. Sell pressure: large sells appearing → p decreases
    if signals.sell_pressure_permille > 300 { // >30% of volume is sells
        delta -= 80; // -8% 
    }
    
    // 3. Price momentum: are we above or below entry price?
    //    Above entry → trade thesis is working → slight p increase
    //    Below entry → thesis failing → p decrease
    if signals.unrealized_bps > 0 {
        delta += 15; // modest boost: price confirming
    } else if signals.unrealized_bps < -100 { // >1% underwater
        delta -= 40; // significant headwind
    }
    
    // 4. Time decay: p decreases with hold time (edge is transient on Pump.fun)
    //    First 2 seconds: no decay
    //    2-10 seconds: -2% per second
    //    >10 seconds: -5% per second (edge is almost certainly gone)
    let hold_secs = signals.hold_time_ms / 1000;
    if hold_secs > 10 {
        delta -= (hold_secs as i64 - 10) * 50 + 16 * 20; // 8 secs × 20 + excess × 50
    } else if hold_secs > 2 {
        delta -= (hold_secs as i64 - 2) * 20;
    }
    
    // Clamp total adjustment: never swing p by more than ±15%
    delta.clamp(-150, 150)
}
```

### 6.3 Remaining-R Computation

As unrealized P&L grows, the "remaining R" (expected additional upside relative to current position value) changes:

```rust
/// Compute expected remaining R given entry R and current unrealized gain.
///
/// Intuition: if entry expected R=10 and we're currently at R=5 (unrealized),
/// the remaining upside is not R=5 — it's less, because large gains are
/// less likely than small ones (distribution is right-skewed, heavy-tailed).
///
/// Model: remaining_R = entry_R × (1 - realized_fraction^0.7)
/// The 0.7 exponent captures diminishing remaining upside.
fn compute_remaining_r(entry_r_x100: u32, current_unrealized_r_x100: i64) -> u32 {
    if current_unrealized_r_x100 <= 0 {
        // Underwater: remaining R is at least entry R (the trade could still work)
        // But reduce slightly proportional to how far underwater
        let underwater_penalty = (-current_unrealized_r_x100).min(entry_r_x100 as i64 / 2);
        return (entry_r_x100 as i64 - underwater_penalty).max(100) as u32;
    }
    
    // How much of expected R have we already captured?
    let realized_fraction_permille = (current_unrealized_r_x100 as u64 * 1000) 
        / entry_r_x100.max(1) as u64;
    
    if realized_fraction_permille >= 1000 {
        // Already exceeded expected R — remaining is small but non-zero
        // (tail events: some trades run 10-50× R)
        return (entry_r_x100 / 5).max(50); // at least 0.5 R remaining
    }
    
    // remaining = entry_R × (1 - (realized/total)^0.7)
    // Integer approximation of x^0.7 for x in [0, 1000]:
    // Use lookup table or piecewise linear
    let frac_to_07 = pow_07_permille(realized_fraction_permille as u32);
    let remaining = entry_r_x100 as u64 * (1000 - frac_to_07 as u64) / 1000;
    
    remaining.max(50) as u32 // never below R=0.5
}

/// Approximate x^0.7 for x in permille [0, 1000].
/// Returns result in permille.
fn pow_07_permille(x_permille: u32) -> u32 {
    // Piecewise linear approximation from precomputed points:
    // x=0→0, x=100→200, x=250→380, x=500→616, x=750→805, x=1000→1000
    match x_permille {
        0..=100   => x_permille * 200 / 100,
        101..=250 => 200 + (x_permille - 100) * 180 / 150,
        251..=500 => 380 + (x_permille - 250) * 236 / 250,
        501..=750 => 616 + (x_permille - 500) * 189 / 250,
        _         => 805 + (x_permille - 750) * 195 / 250,
    }
}
```

---

## 7. Parameter Sensitivity & Calibration

### 7.1 What Needs Calibration

| Parameter | Current Value | Source | Sensitivity |
|-----------|--------------|--------|-------------|
| safety_fraction | 500‰ (half-Kelly) | Standard practice | **Low** — well-established that half-Kelly is right for uncertain parameters |
| w_mag, w_score | 550‰, 450‰ | Judgment call | **Medium** — could walk-forward optimize on 392 trades |
| n_prior (shrinkage) | 50 | Rule of thumb | **Low** — affects small buckets only, and in the conservative direction |
| p_adjustment signals | +30/-50/-80/+15/-40 | Need calibration | **HIGH** — these are the primary knobs for exit behavior |
| time_decay rates | 20‰/s, 50‰/s | Need calibration | **HIGH** — determines how long positions are held |
| trail_bps range | 80–400 bps | Engineering judgment | **Medium** — sets the risk/reward envelope |

### 7.2 Calibration Plan

**Phase 1 (immediate):** Deploy with current parameters. Log everything: entry Kelly, per-tick p(t), R(t), f*(t), trail_bps, and actual exit outcome.

**Phase 2 (after 200+ new trades):** Analyze the logs:
- Do high entry-f* trades actually produce better outcomes? (Validate the LUT)
- Do the p-adjustment signals have predictive power? (Regress actual outcomes on signal values)
- Is time decay too aggressive? Too gentle? (Compare hold times vs optimal exit points)

**Phase 3 (after 500+ trades):** Walk-forward optimization of all parameters with expanding window. Use out-of-sample CAGR as the objective (not p or R individually — Kelly growth rate).

### 7.3 Regime Detection (Future Work)

The current model assumes stationary p and R within buckets. In reality, Pump.fun market regimes shift:
- High-volume days: more competition, lower R, higher p (more fish buying)
- Low-volume days: fewer targets, but less competition
- Post-rug periods: elevated sell pressure across all tokens

A regime indicator (e.g., 1h rolling volume Z-score) could adjust the LUT values dynamically. This is Phase 3+ work — get the basic pipeline right first.

---

## 8. Summary: What Changes in the Codebase

### 8.1 New Structs/Functions

```
BankrollTracker
  └─ get_bankroll_lamports() → u64              [new: real-time wallet balance]

compute_entry_size(mag, score, bankroll, n_concurrent)
  └─ Returns (size, f_permille, p_permille, r_x100) [replaces linear interpolation]

RideState
  └─ entry_f_permille: u32                       [new field]
  └─ entry_p_permille: u32                       [new field]
  └─ entry_r_x100: u32                           [new field]
  └─ entry_size_lamports: u64                    [new field]
  └─ entry_bankroll_lamports: u64                [new field]

compute_exit_kelly(ride, unrealized_r, signals) → ExitKelly
  └─ Uses entry priors + Bayesian update         [replaces disconnected exit Kelly]

initial_trail_bps(entry_f_permille) → u32        [replaces hardcoded trail]
dynamic_trail_bps(entry_f, current_f, base) → u32 [replaces static trail]

apply_safety_constraints(size, daily_pnl, n, bankroll) → Option<u64>
  └─ Gradual budget scaling + hard caps          [replaces cliff circuit breaker]

available_bankroll(wallet_balance, open_positions) → u64 [new: reserves for open positions]
```

### 8.2 Removed/Replaced

```
❌ Linear interpolation sizing (0.10–0.15 SOL by magnitude)
❌ Disconnected exit-only Kelly computation
❌ Hardcoded trail width (or trail from exit-only Kelly)
❌ Static bankroll assumption
```

### 8.3 Data Flow Invariant

**The invariant that must hold:** At no point in the pipeline does the exit engine compute a Kelly fraction without using the entry engine's priors as a starting point. If you see `p = compute_from_scratch(signals)` anywhere in the exit path, it's a bug. The correct form is always `p = entry_p + delta(signals)`.

This is the entire point of the unified pipeline: one coherent Bayesian chain from entry conviction through exit execution.

---

## Appendix A: Integer Arithmetic Verification

All formulas verified for overflow safety with u64 arithmetic:

```
Max bankroll: 100 SOL = 100_000_000_000 lamports (< 2^37)
Max f_permille: 1000 (< 2^10)
Max product: 100_000_000_000 × 1000 = 10^14 (< 2^47) ✓ fits u64

Max trail calculation: 600 × 2000 = 1_200_000 (< 2^21) ✓
Max p_adjustment delta sum: 150 + 150 = 300 (< 2^9) ✓
Max remaining_r: entry_r_x100 max = ~5000 (50× R), 5000 × 1000 = 5_000_000 (< 2^23) ✓
```

No overflow risk with u64 for any realistic parameter range.

## Appendix B: Quick Reference — Complete Sizing Example

**Scenario:** Wallet has 3.2 SOL. Magnitude 57, score 74. Two positions already open (0.12 SOL and 0.15 SOL). Daily P&L is −0.80 SOL.

```
1. LUT lookup:
   mag 57 → bucket 50-60 → f_mag = 543‰
   score 74 → bucket 70-80 → f_score = 530‰

2. Composite:
   f_composite = (543 × 550 + 530 × 450) / 1000 = (298,650 + 238,500) / 1000 = 537‰

3. Half-Kelly:
   f_safe = 537 × 500 / 1000 = 268‰

4. Concurrency (n=3 including this one → adjust on n=2 existing):
   Actually: n_concurrent is the count BEFORE this trade opens = 2
   √2 in permille = 1414
   f_adjusted = 268 × 1000 / 1414 = 189‰

5. Available bankroll:
   reserved = 120,000,000 + 150,000,000 = 270,000,000 (0.27 SOL)
   available = 3,200,000,000 - 270,000,000 = 2,930,000,000 (2.93 SOL)

6. Raw size:
   size = 2,930,000,000 × 189 / 1000 = 553,770,000 lamports = 0.554 SOL

7. Safety constraints:
   daily_pnl = -800,000,000 → remaining budget = 1,500,000,000 - 800,000,000 = 700,000,000
   budget ratio = 700/1500 × 1000 = 467‰ → above 300‰ → full scaling (1000)
   scaled_size = 553,770,000 × 1000 / 1000 = 553,770,000

8. Clamp:
   min(553,770,000, MAX=200,000,000) = 200,000,000 lamports = 0.20 SOL ← cap binds

9. Final check:
   200,000,000 < available 2,930,000,000 ✓
   200,000,000 ≥ MIN 50,000,000 ✓

RESULT: size = 0.20 SOL
        entry_f_permille = 189
        entry_p_permille ≈ 575 (interpolated)
        entry_r_x100 ≈ 1020 (interpolated)

10. Initial trail:
    trail_bps = 100 + 189 × 200 / 500 = 100 + 75 = 175 bps (1.75% off HWM)
```

---

## Addendum: Paper Mode Bankroll

### Requirement
The Kelly sizing engine must support TWO bankroll sources:

1. **Live mode:** `bankroll = wallet_balance_lamports` via Solana RPC (cached 2s)
2. **Paper mode:** `bankroll = paper_bankroll_lamports` — a simulated balance that starts at a configured value (default 5 SOL = 5_000_000_000 lamports) and adjusts with every paper trade's net PnL

### Paper Bankroll Tracking

```rust
pub struct PaperBankroll {
    balance_lamports: AtomicU64,  // thread-safe, lock-free
    initial_lamports: u64,        // for reset / drawdown calc
    hwm_lamports: u64,            // high-water mark
}

impl PaperBankroll {
    pub fn new(initial: u64) -> Self { ... }
    
    /// Called after each paper trade closes.
    /// net_pnl_lamports is signed (positive = win, negative = loss).
    pub fn apply_pnl(&self, net_pnl_lamports: i64) {
        let old = self.balance_lamports.load(Ordering::Relaxed);
        let new = (old as i64 + net_pnl_lamports).max(0) as u64;
        self.balance_lamports.store(new, Ordering::Relaxed);
    }
    
    pub fn balance(&self) -> u64 {
        self.balance_lamports.load(Ordering::Relaxed)
    }
}
```

### Config

```json
{
  "paper_bankroll_sol": 5.0
}
```

### Integration

The entry engine's sizing function takes `bankroll_lamports: u64` as input.
The caller provides either:
- `rpc_client.get_balance(wallet)` in live mode
- `paper_bankroll.balance()` in paper mode

The Kelly math is IDENTICAL in both modes. Only the bankroll source differs.

### Drawdown behavior in paper mode
- Paper bankroll can go to 0 (all lost) — trading pauses via circuit breaker
- Paper bankroll increases on wins — Kelly sizes up proportionally
- This accurately simulates live behavior for data collection
