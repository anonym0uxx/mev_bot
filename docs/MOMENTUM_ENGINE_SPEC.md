# Post-Graduation Momentum Engine Spec

**Date:** 2026-03-29  
**Author:** Quant Research (automated)  
**Status:** DRAFT — Pre-implementation research spec  
**Engine:** `MomentumEngine` (separate from `GraduationArbEngine` and `BackrunEngine`)

---

## 0. Executive Summary

This spec designs a **directional momentum trading engine** for pump.fun tokens immediately after graduation (migration from bonding curve to Raydium AMM v4 or PumpSwap). Unlike the graduation arb engine (which exploits a structural price dislocation), this engine takes a **directional long bet** that post-graduation demand will push price higher than entry.

**Key insight:** Graduation is the most important liquidity regime change in a memecoin's lifecycle. The bonding curve (deterministic pricing, capped at ~85 SOL depth) is replaced by a constant-product AMM (~79+ SOL depth, open to all of DeFi). This transition creates a **volatility event** where informed filtering can extract directional alpha.

**The thesis:** A filtered subset of graduated tokens (those with strong social momentum, healthy holder distribution, and evidence of sustained buying pressure) will experience a 5-30% price increase in the 30-300 seconds post-graduation. By entering selectively and exiting mechanically, we can capture this momentum with a positive expected value.

**Critical caveat:** This is a directional bet, not an arbitrage. Expected win rates are 15-35%, not 50%+. Profitability depends entirely on the ratio of average win to average loss (reward:risk > 3:1 at 20% WR). This strategy will have many small losses and occasional large wins.

---

## 1. Graduation Price Dynamics (The 7% Structural Discount)

### 1.1 The Deposit Mechanics

When a pump.fun token graduates to Raydium AMM v4:

| Parameter | Value | Notes |
|-----------|-------|-------|
| Total SOL in bonding curve at graduation | ~85.0 SOL | Real SOL collected from traders |
| pump.fun graduation fee | ~6.0 SOL (Raydium) / ~1.5 SOL (PumpSwap) | Platform fee, varies by era |
| SOL deposited into pool | ~79.0 SOL (Raydium) / ~83.5 SOL (PumpSwap) | = total - fee |
| Tokens deposited into pool | 206,900,000 tokens = 206.9e12 atoms | Fixed LP allocation (20.69% of supply) |
| Remaining 793.1M tokens | Already distributed to bonding curve buyers | Not in pool |

### 1.2 The Structural Discount

```
BC terminal price:
  k = 30e9 × 1.073e15 = 3.219e25
  vTokens_terminal = 1.073e15 - 793.1e12 = 279.9e12 atoms
  vSol_terminal = k / 279.9e12 = 115.005e9 lamports
  BC_TERMINAL_PRICE = 115.005e9 / 279.9e12 = 4.1088e-4 lamports/atom

Raydium opening price (6 SOL fee era):
  RAY_OPEN = 79e9 / 206.9e12 = 3.8183e-4 lamports/atom
  Structural discount = 1 - (3.8183 / 4.1088) = 7.07%

PumpSwap opening price (1.5 SOL fee):
  PS_OPEN = 83.5e9 / 206.9e12 = 4.0357e-4 lamports/atom
  Structural discount = 1 - (4.0357 / 4.1088) = 1.78%
```

**The structural discount is a known constant: ~7% for Raydium, ~1.8% for PumpSwap.**

This discount means the pool opening price is ALWAYS below the last bonding curve price. For the momentum engine, this is actually beneficial — we're buying at a structural discount to the price that bonding curve participants last paid.

### 1.3 Diagnosing Our 27-42% Spread Data

Our observed spreads (27-42%) are **wrong**. Root causes confirmed from data analysis:

| Problem | Evidence | Impact |
|---------|----------|--------|
| **Wrong mint extraction** | `EPjFWdd5` (USDC) and `5Q544fKr` (Raydium program) appearing as mints | Calculating price against wrong token's balance |
| **postTokenBalances timing** | Balances reflect post-swap state, not initial deposit | If arb bots traded in same block, reserves already moved |
| **Heuristic fallback garbage** | `extract_max_sol_increase` picks fee recipients, not pool vault | Completely wrong vault pair |
| **v0 ALT resolution failure** | 99.97% pool resolution failure rate | Only 23/38,587 events got any price data |

**The real opening price is deterministic and calculable from first principles:** 79e9/206.9e12 = 3.818e-4 for Raydium. What we SHOULD be measuring is the **actual traded price** at time T after graduation, not the initial deposit ratio. This is the key metric for the momentum engine.

### 1.4 What Matters for Momentum (Not Arb)

For the momentum engine, we don't care about the structural discount per se. We care about:

1. **Entry price at time T** — what price can we actually buy at, given that arb bots and early sellers have already moved the price?
2. **Price trajectory over 30-300s** — does the price go up or down from our entry?
3. **Liquidity depth** — can we enter and exit a 0.3 SOL position without moving the market more than 0.5%?

The ~79 SOL of pool liquidity means:
```
Slippage for 0.3 SOL buy = 0.3 / 79 = 0.38% (constant product approximation)
Slippage for 0.5 SOL buy = 0.5 / 79 = 0.63%
Round-trip slippage (buy + sell) ≈ 2× single-leg = 0.76-1.27%
```

This is acceptable for a strategy targeting 5-30% moves.

---

## 2. Post-Graduation Price Behavior (0-600 seconds)

### 2.1 Theoretical Phase Model

Based on memecoin market microstructure analysis, bonding curve mechanics, and on-chain behavioral patterns:

#### Phase 0: Pool Creation (T=0, slots 0-1, 0-400ms)
- Raydium `initialize2` or PumpSwap `CreatePool` lands on-chain
- Pool opens at structural discount price (7% below BC terminal for Raydium)
- MEV bots detect within 5-80ms via Geyser/logsSubscribe
- **First arb trades land at T+400-800ms** — they capture the structural discount
- Price moves UP toward BC terminal price (arb bots buying tokens with SOL)

#### Phase 1: Initial Repricing (T=0-30s)
- Arb bots close the structural discount in first 1-3 slots
- Price overshoots to approximately BC terminal price
- **Bonding curve holders who bought near the top** start selling
- Sellers dump 2-15% of token supply into the pool in first 30s
- **Net effect: price typically DROPS 5-20% below BC terminal in first 30s**
- This is the "dump" phase — weak hands exiting

**Estimated price at T+30s relative to BC terminal:**
- Bear case: -20% (heavy selling, no new demand)
- Base case: -10% (moderate selling, some buying)
- Bull case: -3% (light selling, strong buying counterflow)

#### Phase 2: Price Discovery (T=30-120s)
- The token now has a real DEX price visible to all of DeFi
- **New capital arrives** via:
  - DEX screeners (DexScreener, Birdeye, DEXTools) — list new pools within 10-60s
  - Telegram/Twitter bots that alert on new Raydium/PumpSwap pairs
  - Copy-trading bots that follow wallets of early bonding curve buyers
  - Jupiter/Raydium router makes the token discoverable
- **This is the critical phase:** if new buying pressure exceeds selling pressure, price reverses

**What determines if Phase 2 is bullish:**
- Social media velocity (Twitter mentions, Telegram signals)
- Token narrative strength (meme virality, current meta)
- Holder distribution (if top wallet has 10%+, smart money expects a rug → no new buyers)
- Graduation speed (fast = strong momentum from bonding curve → likely continued momentum)

#### Phase 3: Momentum (T=120-600s)
- If Phase 2 attracted enough buyers, a positive feedback loop begins:
  - Price rises → more screener attention → more buyers → price rises more
  - This is the memecoin "pump" that can 2-10× from graduation price
- If Phase 2 failed to attract buyers:
  - Remaining bonding curve holders continue selling
  - Price grinds down to 30-50% below BC terminal
  - Token joins the graveyard of 97%+ of pump.fun tokens

#### Phase 4: Distribution (T=600s+)
- Early momentum buyers take profits
- Price volatility decreases
- Token either stabilizes at a new level or continues slow bleed
- Beyond our trading horizon

### 2.2 Estimated Outcome Distribution

Based on analysis of memecoin graduation events, on-chain data patterns, and market microstructure reasoning:

| Outcome | Est. % of Graduations | Price Move (from T+30s entry) | Timeframe |
|---------|----------------------|-------------------------------|-----------|
| **Dead on arrival** — no new buyers, steady sell | 40-50% | -10% to -30% | 30-300s |
| **Flat / noise** — small buys offset by sells | 20-30% | -5% to +5% | 30-300s |
| **Minor pump** — moderate new buying | 10-15% | +5% to +15% | 60-300s |
| **Significant pump** — strong momentum | 5-10% | +15% to +50% | 120-600s |
| **Major pump** — viral/narrative driven | 1-3% | +50% to +500%+ | 300-3600s |

**Key estimates for profitability modeling:**
- **~15-25% of tokens have a ≥5% pump** from a well-timed entry
- **~8-15% have a ≥15% pump**
- **~3-7% have a ≥30% pump**

These are estimates that must be validated with empirical data (see Section 6 Data Collection).

### 2.3 What Makes a Token Pump Post-Graduation

**Strong predictors (theoretical, to be validated):**

1. **Graduation speed** (time from token creation to graduation)
   - Fast graduation (< 30 min) = explosive buying demand → likely continues post-graduation
   - Slow graduation (> 4 hours) = grinding buy pressure → momentum may be exhausted
   - Sweet spot hypothesis: 15-60 min graduation time

2. **Pre-graduation buying velocity** (trades/minute in last 5 min of bonding curve)
   - High velocity at graduation = momentum still active
   - This is analogous to the backrunner's `preTriggerBuys1s` — proven predictive signal

3. **Holder concentration**
   - Top wallet > 5% of supply → rug risk → smart money avoids → less new buying
   - More distributed (no wallet > 2%) → healthier → more confident new buyers

4. **Social signal velocity**
   - Twitter mentions acceleration in 5 min before/after graduation
   - Telegram group creation/activity
   - KOL (Key Opinion Leader) mentions

5. **Token narrative/meta alignment**
   - Does the token name/ticker match current meta? (AI, political, animal, etc.)
   - Tokens aligned with hot narratives pump more

**Weak/no predictive power (hypothesized):**
- Token age alone (without controlling for graduation speed)
- Absolute bonding curve volume (correlated with graduation speed)
- Creator wallet SOL balance (anyone can create)

---

## 3. Entry Signal Design

### 3.1 Entry Timing: Delayed Entry (Option B/C Hybrid)

**Decision: Enter at T+15-30s, NOT immediately at graduation.**

Rationale:
- T=0 entry puts us in the middle of arb bot trading + initial selling pressure
- T+30s entry lets the initial dump play out, enters at a lower price
- T+15-30s is the sweet spot: dump has mostly played out, but we're ahead of the DEX screener crowd (which arrives at T+30-120s)

**Implementation:**

```
on_graduation_detected(mint, sig, ts_ms):
  // Phase 1: Immediate — resolve pool, gather metadata
  pool = resolve_pool(mint, sig)  // async, 200-400ms
  metadata = gather_metadata(mint)  // from backrunner's existing data
  
  // Phase 2: Arm entry timer
  arm_delayed_entry(mint, pool, metadata, delay_ms=15_000)

on_delayed_entry_timer(mint, pool, metadata):
  // Phase 3: Check entry conditions (gates)
  if !passes_gates(mint, pool, metadata):
    log_skip(mint, reason)
    return
  
  // Phase 4: Fetch current price (single getAccountInfo on vault pair)
  current_price = fetch_current_price(pool)
  
  // Phase 5: Enter position
  enter_position(mint, pool, current_price, size_sol=0.3)
```

**Why 15s and not 30s:** 15s gives us a head start over DEX screener traffic while avoiding most of the initial dump. The exact optimal delay is an empirical parameter to be tuned with data.

### 3.2 Entry Delay Optimization

The entry delay is the most critical tunable parameter. It trades off:
- **Too early (0-10s):** Buying into the dump. Initial sellers haven't finished. Poor entry price.
- **Too late (60s+):** Missing the momentum. The screener-driven pump has already started (if it's going to happen).
- **Sweet spot (15-30s):** After initial dump, before screener crowd.

**Phase 1 (paper mode):** Use MULTIPLE delayed checks per graduation to find the optimal window:
```
entry_delay_candidates_ms: [5000, 10000, 15000, 30000, 60000]
```
For each graduation, log the price at each candidate delay. After 1000+ events, analyze which delay yields the best price relative to the subsequent peak price. This determines the production delay.

### 3.3 Gate Conditions (Entry Filters)

```yaml
# === MANDATORY GATES (hard filters, must ALL pass) ===

# Pool must be resolved with valid reserves
min_pool_sol_reserves: 50.0          # SOL — reject if pool has < 50 SOL (abnormal graduation)
max_pool_sol_reserves: 120.0         # SOL — reject if > 120 SOL (unusual, possibly not pump.fun)

# Entry price sanity
max_entry_discount_from_bc_pct: 40.0  # If price is 40%+ below BC terminal, something's wrong
min_entry_discount_from_bc_pct: -10.0 # If price is 10%+ ABOVE BC terminal, momentum is already running (late)

# Holder concentration (requires on-chain lookup or backrunner data)
max_top_holder_pct: 8.0              # Max % of supply held by single wallet (excl. pool)
                                     # Reject tokens with concentrated ownership → rug risk

# Pool type filter
allowed_pool_types: ["raydium_amm_v4"]  # Start with Raydium only (better understood)
                                         # Add PumpSwap after calibration

# === SCORING GATES (soft filters, combined into entry score) ===

# Graduation speed (time from token creation to graduation)
graduation_speed_score:
  fast_bonus:    # < 20 min graduation → +2 score (explosive demand)
    max_seconds: 1200
    score: 2.0
  normal:        # 20-60 min → +1 score
    max_seconds: 3600
    score: 1.0
  slow_penalty:  # > 2 hours → -1 score (exhausted momentum)
    min_seconds: 7200
    score: -1.0

# Pre-graduation buying velocity (from backrunner data if available)
pre_grad_velocity_score:
  high:          # ≥ 10 buys in last 10s before graduation → +2 score
    min_buys_10s: 10
    score: 2.0
  medium:        # 5-9 buys → +1 score
    min_buys_10s: 5
    score: 1.0
  low:           # < 5 buys → 0 score
    score: 0.0

# Volume surge at graduation
volume_surge_score:
  high:          # Last-5-min BC volume > 10 SOL → +1 score
    min_vol_sol: 10.0
    score: 1.0
  low:
    score: 0.0

# Price recovery from opening low (measured at entry delay time)
price_recovery_score:
  strong:        # Price at T+15s is within 5% of BC terminal → +2 (sellers done, buyers arriving)
    max_discount_pct: 5.0
    score: 2.0
  moderate:      # Within 10% → +1
    max_discount_pct: 10.0
    score: 1.0
  weak:          # > 10% below → 0 (still dumping)
    score: 0.0

# === ENTRY THRESHOLD ===
min_entry_score: 4.0                 # Must score ≥ 4 to enter
                                     # Example qualifying combo:
                                     #   fast graduation (+2) + high pre-grad velocity (+2) + moderate recovery (+1) = 5 ✓
                                     #   normal graduation (+1) + medium velocity (+1) + strong recovery (+2) + volume surge (+1) = 5 ✓
```

### 3.4 Entry Execution

```yaml
entry_size_sol: 0.30                 # Base position size
max_entry_slippage_bps: 100          # 1% max slippage on entry (constant product → predictable)
entry_priority_fee_lamports: 100000  # 0.0001 SOL — not competing for speed, just getting in
use_jito_bundle: false               # No need for Jito — this isn't a speed race
entry_method: "direct_swap"          # Simple Raydium swap instruction, no bundling needed
```

**Why no Jito bundle:** Unlike arb (where you need to be FIRST), momentum trading doesn't require sub-slot execution. A standard transaction landing within 1-2 slots of submission is fine. This saves 0.003 SOL in Jito tips per trade.

### 3.5 Entry Signal Summary

```
MOMENTUM ENTRY DECISION TREE:

1. Graduation detected → resolve pool + gather metadata (async, T=0)
2. Wait 15,000ms (configurable entry delay)
3. At T+15s:
   a. Check mandatory gates (pool reserves, holder concentration, pool type)
   b. Calculate entry score (graduation speed + velocity + volume + recovery)
   c. If score ≥ 4.0 → proceed
   d. Fetch current price via getMultipleAccountsInfo on pool vaults
   e. Submit buy transaction (0.3 SOL, max 1% slippage)
   f. Log entry with full metadata
```

---

## 4. Exit Strategy

### 4.1 Multi-Tier Take-Profit with Trailing Stop

The exit strategy must balance capturing large wins (5-50%+ pumps) against locking in profits before reversals. Pure fixed TP leaves money on the table during big pumps. Pure trailing stops get stopped out by noise.

**Hybrid approach: Fixed TP tiers + trailing stop on remaining position.**

```yaml
exit_strategy:
  # Tier 1: Lock in partial profit early
  tp1:
    trigger_pct: 5.0          # +5% from entry
    exit_fraction: 0.30       # Sell 30% of position
    action: "market_sell"

  # Tier 2: Capture moderate pump
  tp2:
    trigger_pct: 15.0         # +15% from entry
    exit_fraction: 0.30       # Sell 30% of position (total sold: 60%)
    action: "market_sell"

  # Tier 3: Remaining 40% rides with trailing stop
  trailing_stop:
    activation_pct: 15.0      # Activates when price reaches +15% (same as TP2)
    trail_pct: 8.0            # Trail 8% below MFE (maximum favorable excursion)
    min_exit_pnl_pct: 5.0     # Never let trailing stop exit below +5%
    action: "market_sell"

  # Emergency ceiling — if price reaches +50%, dump remaining 40%
  tp_ceiling:
    trigger_pct: 50.0         # +50% from entry
    exit_fraction: 1.0        # Sell everything remaining
    action: "market_sell"
```

**Example scenarios:**

| Scenario | Price Path | TP1 (30%) | TP2 (30%) | Trail (40%) | Total Gross |
|----------|-----------|-----------|-----------|-------------|-------------|
| Dead (+2%, -15%) | ↗↘ | miss | miss | miss → SL | -15% × 100% = **-15%** |
| Small pump (+7%, -3%) | ↗↘ | +5% × 30% = 1.5% | miss | trail exit ~+5% × 40% = 2.0% | **+3.5%** weighted |
| Good pump (+20%, -5%) | ↗↗↘ | +5% × 30% = 1.5% | +15% × 30% = 4.5% | trail exit ~+12% × 40% = 4.8% | **+10.8%** weighted |
| Great pump (+50%+) | ↗↗↗ | +5% × 30% = 1.5% | +15% × 30% = 4.5% | +50% × 40% = 20% | **+26%** weighted |

### 4.2 Stop-Loss Design

```yaml
stop_loss:
  # Hard stop-loss from entry price
  hard_sl_pct: 12.0           # -12% from entry → full exit
  
  # Time-based stop-loss (no profit = wrong thesis)
  time_sl:
    check_after_ms: 60000     # After 60s...
    min_pnl_for_hold_pct: -2.0  # ...if PnL is worse than -2%, exit
    # Rationale: if the token hasn't shown momentum after 60s, the thesis is wrong

  # Volume-based stop-loss
  volume_sl:
    check_after_ms: 30000     # After 30s...
    min_buy_volume_sol: 1.0   # ...if less than 1 SOL of buy volume in last 30s, exit
    # Rationale: no buying interest = no momentum = exit
```

**Why 12% hard stop-loss (not tighter):**

Post-graduation tokens are extremely volatile. Normal price noise in the first 30-120s includes:
- 3-8% swings from individual large trades
- Arb bot repricing creating 2-5% spikes
- Organic price discovery oscillation

A tighter SL (e.g., 5%) would be triggered by noise in >50% of entries, including tokens that subsequently pump 20%+. The 12% SL ensures we only exit on genuine price rejection (sustained selling pressure), not noise.

**Expected SL loss distribution:**
- Typical SL exit: -8 to -12% (price drops to SL and we exit)
- With 1% slippage on exit: effective loss = -9 to -13%
- Average SL loss estimate: **-10%** (midpoint)

### 4.3 Maximum Hold Time

```yaml
max_hold_ms: 300000            # 300 seconds (5 minutes)
max_hold_exit_action: "market_sell_all"
```

**Rationale:** After 5 minutes, the momentum thesis has either played out or failed. Holding longer converts the trade from momentum capture to "bag-holding a memecoin" — which has negative expected value.

**Hold time distribution estimates:**
- SL exits: 15-60s (fast failures)
- TP1 exits: 30-120s (quick wins)
- TP2 exits: 60-180s (moderate pumps)
- Trail exits: 120-300s (riding momentum to peak)
- MaxHold exits: 300s (thesis timeout — neither pump nor dump)

### 4.4 Exit Priority Order

When multiple exit conditions trigger simultaneously:

```
1. Hard SL (-12%) — highest priority, always honored
2. TP ceiling (+50%) — dump everything at extreme profits
3. TP tiers (TP1/TP2) — partial exits
4. Trailing stop — remaining position management
5. Time-based SL — thesis timeout at 60s
6. Volume-based SL — no buying interest
7. MaxHold timeout — 300s hard wall
```

### 4.5 Price Monitoring Implementation

```yaml
price_poll_interval_ms: 2000    # Poll vault balances every 2s
price_poll_method: "getMultipleAccountsInfo"  # Batch both vaults in one RPC call
price_poll_commitment: "confirmed"

# For paper mode: use WebSocket price feed if available
# For live mode: direct RPC polling (most reliable for exit execution)
```

**RPC budget per position: 1 call / 2s = 30 calls / 60s = 150 calls / 300s max hold.**

With potentially 3 concurrent positions: 150 × 3 = 450 RPC calls per 5-min window. Each call fetches 2 accounts (coin vault + pc vault). This is ~1.5 calls/second sustained — well within standard RPC rate limits.

---

## 5. Position Sizing & Risk Management

### 5.1 Position Sizing

```yaml
position_sizing:
  base_size_sol: 0.30           # Standard position size
  min_size_sol: 0.10            # Minimum (for low-confidence entries)
  max_size_sol: 0.50            # Maximum (for highest-conviction entries)
  
  # Score-based sizing (optional, Phase 2)
  size_by_score:
    score_4_5: 0.20             # Score 4-5: small position (marginal confidence)
    score_5_6: 0.30             # Score 5-6: standard position
    score_6_plus: 0.40          # Score 6+: larger position (high conviction)
```

**Why 0.3 SOL base:**
- Pool depth is ~79 SOL → 0.3 SOL = 0.38% of pool → minimal market impact
- At -12% SL, max loss per trade = 0.036 SOL + fees ≈ 0.04 SOL
- At +10.8% weighted TP (good pump), gain = 0.032 SOL
- Reward:risk = 0.032:0.04 = 0.8:1 per trade BUT only need 1 good pump per 3-4 losses

### 5.2 Concurrent Position Limits

```yaml
risk_management:
  max_concurrent_positions: 3    # Max 3 open momentum positions at once
  max_same_block_entries: 1      # Max 1 entry per Solana slot (400ms)
  
  # Capital deployed: 3 × 0.3 = 0.9 SOL max at risk
  # With 1.5 SOL bankroll: 60% utilization ceiling
```

**Why 3 concurrent:** 
- Raydium graduations: ~15,700/day = ~654/hour = ~11/min
- PumpSwap graduations: ~1,690/day = ~70/hour = ~1.2/min
- With gate filters (est. 10-20% pass rate): ~1-2 qualifying entries per minute
- 3 concurrent positions with 300s max hold → theoretical max entries = 3 × (300s/300s) = 3
- In practice, most positions exit via SL in 30-60s, freeing slots for new entries

### 5.3 Daily Risk Limits

```yaml
daily_limits:
  max_daily_loss_sol: 1.00      # Stop trading for the day if net loss exceeds 1 SOL
  max_daily_trades: 100         # Hard cap on daily entries
  max_consecutive_sl: 5         # Pause 30 min after 5 consecutive stop-losses
  pause_after_consecutive_sl_ms: 1800000  # 30 minute cooldown
  
  # Separate from backrunner's daily limits — independent budget
  # Combined bankroll: 1.5 SOL
  # Backrunner daily limit: 0.18 SOL (existing)
  # Momentum daily limit: 1.00 SOL
  # Total max daily loss: 1.18 SOL (79% of bankroll) — aggressive but capped
```

### 5.4 Fee Structure Impact

| Fee Component | Raydium | PumpSwap | Notes |
|---------------|---------|----------|-------|
| Swap fee | 0.25% (25 bps) | 1.00% (100 bps) | Per leg |
| Round-trip swap fees | 0.50% | 2.00% | Entry + exit |
| Priority fee | 0.0001 SOL | 0.0001 SOL | Per tx |
| Base tx fee | 0.000005 SOL | 0.000005 SOL | Per tx |
| **Total fixed per trade** | **0.000210 SOL** | **0.000210 SOL** | Two txs |
| **Total variable per trade** | **0.50% of position** | **2.00% of position** | Swap fees |
| **Total cost (0.3 SOL)** | **0.0017 SOL (0.57%)** | **0.0062 SOL (2.07%)** | Fixed + variable |

**Critical: PumpSwap's 1% swap fee per leg (2% round trip) makes momentum trading significantly harder.** A 5% gross win becomes 3% net on PumpSwap vs 4.5% net on Raydium.

**Decision: Start with Raydium-only. Add PumpSwap only if Raydium data shows profitability with margin to absorb the 4× higher fees.**

### 5.5 Slippage Model

For a constant-product AMM with reserves (R_sol, R_token):

```
Buying tokens with `sol_in` SOL:
  tokens_out = R_token × sol_in / (R_sol + sol_in)
  effective_price = sol_in / tokens_out
  slippage = (effective_price / spot_price) - 1
           ≈ sol_in / R_sol  (for small trades)

For 0.3 SOL into ~79 SOL pool:
  slippage ≈ 0.3 / 79 = 0.38%

For 0.5 SOL into ~79 SOL pool:
  slippage ≈ 0.5 / 79 = 0.63%

Round-trip slippage (buy + sell, assuming reserves unchanged):
  ≈ 2 × single-leg ≈ 0.76% for 0.3 SOL
```

**Total friction per trade (Raydium, 0.3 SOL):**
```
Swap fees:       0.50% (0.0015 SOL)
Slippage:        0.76% (0.0023 SOL)
Priority fees:   0.0002 SOL
Base tx fees:    0.00001 SOL
─────────────────────────────────
Total friction:  1.30% (0.0040 SOL)
```

**Breakeven move:** A trade must move +1.30% just to break even after all friction. Any TP target must exceed this.
---

## 6. Data Collection Schema (ML-Ready JSONL Fields)

### 6.1 Per-Graduation Event Record

Every graduation detected (even if we don't enter) should log a record for ML training.

```jsonl
{
  // === IDENTIFICATION ===
  "timestamp_ms": 1774767553867,
  "mint": "7mHCx9iXPJ7EJDbDAUGmej39Kme8cxZfeVi1EAvEpump",
  "pool_type": "raydium_amm_v4",
  "pool_address": "...",
  "graduation_sig": "...",

  // === BONDING CURVE CONTEXT ===
  "graduation_volume_sol": 85.2,
  "graduation_speed_s": 1834,
  "bc_holder_count": 47,
  "bc_unique_buyers": 38,
  "bc_unique_sellers": 12,
  "max_holder_pct": 3.2,
  "top5_holder_pct": 11.8,
  "bc_terminal_price_lamports_per_atom": 4.1088e-4,
  "bc_buys_last_10s": 7,
  "bc_buys_last_60s": 23,
  "bc_volume_last_5min_sol": 12.5,

  // === POOL STATE AT OPEN ===
  "opening_reserve_sol_lamports": 79000000000,
  "opening_reserve_token_atoms": 206900000000000,
  "opening_price_lamports_per_atom": 3.818e-4,
  "structural_discount_pct": 7.07,

  // === PRICE TRAJECTORY (key for ML) ===
  "price_at_5s": 3.95e-4,
  "price_at_10s": 4.02e-4,
  "price_at_15s": 3.98e-4,
  "price_at_30s": 3.85e-4,
  "price_at_60s": 3.92e-4,
  "price_at_120s": 4.15e-4,
  "price_at_300s": 4.45e-4,
  "price_at_600s": 3.80e-4,

  // === VOLUME TRAJECTORY ===
  "buy_volume_0_30s_sol": 5.2,
  "sell_volume_0_30s_sol": 8.1,
  "buy_volume_30_60s_sol": 3.4,
  "sell_volume_30_60s_sol": 1.2,
  "buy_volume_60_300s_sol": 12.3,
  "sell_volume_60_300s_sol": 4.5,
  "total_trades_0_300s": 87,

  // === DERIVED FEATURES ===
  "net_flow_0_30s_sol": -2.9,
  "net_flow_30_300s_sol": 10.0,
  "max_price_0_300s": 4.55e-4,
  "min_price_0_300s": 3.70e-4,
  "max_drawdown_from_open_pct": 10.1,
  "max_pump_from_30s_pct": 20.3,
  "volatility_0_300s_pct": 18.5,

  // === TRADE EXECUTION (only if we entered) ===
  "entered": true,
  "entry_delay_ms": 15000,
  "entry_price": 3.90e-4,
  "entry_size_sol": 0.30,
  "entry_score": 5.5,
  "entry_gate_details": {
    "graduation_speed_score": 2.0,
    "pre_grad_velocity_score": 1.0,
    "volume_surge_score": 1.0,
    "price_recovery_score": 1.5
  },

  // === EXIT ===
  "exit_price": 4.20e-4,
  "exit_reason": "tp1",
  "hold_ms": 45000,
  "gross_pnl_pct": 7.69,
  "net_pnl_pct": 6.39,
  "gross_pnl_sol": 0.023,
  "net_pnl_sol": 0.019,
  "fees_sol": 0.004,
  "mfe_pct": 12.3,
  "mae_pct": -3.1,
  "mfe_time_ms": 120000,
  "mae_time_ms": 8000,

  // === METADATA ===
  "engine_version": "momentum-v1",
  "config_version": "mom-v0.30sol_300s_15delay"
}
```

### 6.2 Price Trajectory Collection (Paper Mode)

In paper mode (Phase 1), we don't trade but we DO collect price trajectories for every graduation event that passes basic pool resolution. This is the most valuable data we can collect.

**Implementation:** For each graduation where pool reserves are successfully fetched:
1. Record opening price (T=0)
2. Schedule price polls at T=[5, 10, 15, 30, 60, 120, 300, 600] seconds
3. Each poll: `getMultipleAccountsInfo([coin_vault, pc_vault])` → calculate price
4. Log complete trajectory as one JSONL record after T=600s

**RPC budget for trajectory collection:**
- 8 polls per graduation × 1 RPC call each = 8 RPCs per graduation
- At 654 Raydium graduations/hour: 654 × 8 = 5,232 RPCs/hour = 1.45 RPCs/second
- Well within standard RPC limits (10+ RPS)

**Warning:** This assumes our pool resolution pipeline is fixed. Until Path C (tx-based vault extraction from GRAD_ARB_QUANT_SPEC.md Section 5.4) is implemented, trajectory data will be garbage.

### 6.3 Social Signal Collection (Future, Phase 3)

Fields to add when social signal infrastructure is built:

```jsonl
{
  "twitter_mentions_1h_before_grad": 12,
  "twitter_mentions_1h_after_grad": 45,
  "twitter_kol_mentions": 2,
  "telegram_group_exists": true,
  "telegram_group_members": 340,
  "dexscreener_listing_delay_ms": 23000,
  "birdeye_listing_delay_ms": 45000
}
```

---

## 7. Honest Profitability Assessment

### 7.1 Core Math: Win Rate × Average Win vs (1 - WR) × Average Loss

This is a **directional momentum strategy**, not an arb. Expected WR is 15-30%, not 50%+.

**The strategy is profitable when:**
```
WR × Avg_Win > (1 - WR) × Avg_Loss + Per_Trade_Friction

Where:
  WR = win rate (fraction of trades that exit at any TP tier)
  Avg_Win = average gross profit on winning trades
  Avg_Loss = average gross loss on losing trades
  Per_Trade_Friction = 1.30% (swap fees + slippage + tx fees)
```

### 7.2 Parameter Estimates

**Winning trades (exit at TP1/TP2/Trail):**
```
Scenario A: TP1 only (+5%, 30% sold)
  Remaining exits at time-SL or trailing SL near breakeven
  Weighted gross: 5% × 0.30 + 0% × 0.70 = 1.5%
  Net after friction: 1.5% - 1.3% = +0.2% (barely positive)

Scenario B: TP1 + TP2 (+5% on 30%, +15% on 30%)
  Remaining 40% trails out at ~10%
  Weighted gross: 5%×0.30 + 15%×0.30 + 10%×0.40 = 1.5% + 4.5% + 4.0% = 10.0%
  Net after friction: 10.0% - 1.3% = +8.7%

Scenario C: Full run (+5% on 30%, +15% on 30%, +50% on 40%)
  Weighted gross: 1.5% + 4.5% + 20.0% = 26.0%
  Net after friction: 26.0% - 1.3% = +24.7%
```

**Average win estimate:**
```
P(Scenario A | win) = 40%
P(Scenario B | win) = 40%  
P(Scenario C | win) = 20%

E[win] = 0.40 × 0.2% + 0.40 × 8.7% + 0.20 × 24.7%
       = 0.08% + 3.48% + 4.94%
       = 8.5% average net win
       = 0.0255 SOL on 0.3 SOL position
```

**Losing trades (exit at SL or time-SL):**
```
Hard SL (-12%): ~60% of losses
  Net loss: -12% - 1.3% friction = -13.3%

Time SL (-5% avg): ~30% of losses
  Net loss: -5% - 1.3% = -6.3%

MaxHold (flat, -2% avg): ~10% of losses  
  Net loss: -2% - 1.3% = -3.3%

E[loss] = 0.60 × (-13.3%) + 0.30 × (-6.3%) + 0.10 × (-3.3%)
        = -7.98% - 1.89% - 0.33%
        = -10.2% average net loss
        = -0.0306 SOL on 0.3 SOL position
```

### 7.3 Break-Even Win Rate

```
Break-even: WR × E[win] + (1-WR) × E[loss] = 0
WR × 8.5% + (1-WR) × (-10.2%) = 0
WR × 8.5% - 10.2% + WR × 10.2% = 0
WR × 18.7% = 10.2%
WR = 10.2% / 18.7% = 54.5%

Wait — this is too high. Let me recalculate with better win estimates.
```

**Problem identified:** Scenario A wins (+0.2% net) are barely breakeven. They drag down the average win. Let me redefine "win" as TP2+ only, with TP1-only trades classified as "breakeven."

**Revised classification:**
```
Outcome distribution for entries that pass gates (estimated):
  Dead (SL/time-SL):          45%  → avg loss -10.2%
  TP1-only (small pump):      15%  → avg gain +0.2% (effectively flat)
  TP2+ (good pump):           25%  → avg gain +8.7%
  Full run (great pump):      10%  → avg gain +24.7%
  MaxHold (flat):              5%  → avg loss -3.3%

E[trade] = 0.45×(-10.2%) + 0.15×(0.2%) + 0.25×(8.7%) + 0.10×(24.7%) + 0.05×(-3.3%)
         = -4.59% + 0.03% + 2.175% + 2.47% - 0.165%
         = -0.09%
```

**At these estimates, the strategy is approximately breakeven.** The edge is razor-thin and depends critically on:
1. Filter quality (can we select the 35% that pump?)
2. Entry timing (can we enter near the bottom of the initial dump?)
3. Exit efficiency (does the trailing stop capture upside without giving back profits?)

### 7.4 Break-Even Analysis Across Win Rate Scenarios

For 0.3 SOL position, Raydium fees:

| Win Rate (TP2+) | Avg Win | Avg Loss | E[trade] | E[trade] SOL | Trades/Day | Daily E[P&L] |
|-----------------|---------|----------|----------|-------------|------------|-------------|
| 5% | +12% | -10% | -9.0% | -0.027 | 20 | **-0.54 SOL** |
| 10% | +12% | -10% | -7.8% | -0.023 | 20 | **-0.47 SOL** |
| 15% | +12% | -10% | -6.7% | -0.020 | 20 | **-0.40 SOL** |
| 20% | +12% | -10% | -5.6% | -0.017 | 20 | **-0.34 SOL** |
| **30%** | **+12%** | **-10%** | **-3.4%** | **-0.010** | **20** | **-0.20 SOL** |
| **40%** | **+12%** | **-10%** | **-1.2%** | **-0.004** | **20** | **-0.07 SOL** |
| **46%** | **+12%** | **-10%** | **0%** | **0** | **20** | **breakeven** |
| **50%** | **+12%** | **-10%** | **+1.0%** | **+0.003** | **20** | **+0.06 SOL** |

**With TP1-only wins counted AND tail wins (Scenario C):**

Let's model this more carefully as a mixture distribution:

| Outcome | Probability | Net P&L % | Contribution |
|---------|------------|-----------|-------------|
| Hard SL | 40% | -13.3% | -5.32% |
| Time SL | 10% | -6.3% | -0.63% |
| MaxHold | 5% | -3.3% | -0.17% |
| TP1-only | 15% | +0.2% | +0.03% |
| TP2 | 20% | +8.7% | +1.74% |
| Full run | 10% | +24.7% | +2.47% |
| **Total** | | | **-1.88%** |

At 20 trades/day × 0.3 SOL × -1.88%:
```
Daily E[P&L] = 20 × 0.3 × (-0.0188) = -0.113 SOL/day
```

**This is slightly negative** at the base-case estimates. But the distribution assumptions are uncertain by ±50%. Let me model three scenarios:

### 7.5 Three Scenarios: Bear / Base / Bull

#### Bear Case (poor filter quality, bad timing)
```
Distribution:
  SL: 55% @ -10.2%  |  TP1-only: 15% @ +0.2%  |  TP2+: 15% @ +8.7%  |  Full: 5% @ +24.7%  |  Flat: 10% @ -3.3%

E[trade] = 0.55×(-10.2%) + 0.15×(0.2%) + 0.15×(8.7%) + 0.05×(24.7%) + 0.10×(-3.3%)
         = -5.61% + 0.03% + 1.31% + 1.24% - 0.33%
         = -3.37%

Trades/day: 15  (fewer pass tight gates)
Daily P&L: 15 × 0.3 × (-0.0337) = -0.15 SOL/day
Monthly: -4.5 SOL → hits daily loss cap within 7 days
```

#### Base Case (decent filters, OK timing)
```
Distribution:
  SL: 40% @ -10.2%  |  TP1-only: 15% @ +0.2%  |  TP2+: 25% @ +8.7%  |  Full: 10% @ +24.7%  |  Flat: 10% @ -3.3%

E[trade] = -4.08% + 0.03% + 2.18% + 2.47% - 0.33%
         = +0.27%

Trades/day: 20
Daily P&L: 20 × 0.3 × 0.0027 = +0.016 SOL/day
Monthly: +0.48 SOL
```

#### Bull Case (excellent filters, good timing, tail wins captured)
```
Distribution:
  SL: 30% @ -10.2%  |  TP1-only: 15% @ +0.2%  |  TP2+: 30% @ +8.7%  |  Full: 15% @ +24.7%  |  Flat: 10% @ -3.3%

E[trade] = -3.06% + 0.03% + 2.61% + 3.71% - 0.33%
         = +2.96%

Trades/day: 25
Daily P&L: 25 × 0.3 × 0.0296 = +0.222 SOL/day
Monthly: +6.66 SOL
```

### 7.6 Daily P&L Summary

| Scenario | E[trade] | Trades/Day | Daily P&L | Monthly P&L |
|----------|----------|-----------|-----------|-------------|
| **Bear** | -3.37% | 15 | **-0.15 SOL** | -4.5 SOL |
| **Base** | +0.27% | 20 | **+0.016 SOL** | +0.48 SOL |
| **Bull** | +2.96% | 25 | **+0.22 SOL** | +6.66 SOL |

### 7.7 Honest Assessment

**The hard truth:** This strategy is **marginally profitable at best** in the base case, with significant downside risk in the bear case. The edge is thin and depends critically on:

1. **Filter quality** — Can we reliably identify the 30-45% of tokens that pump? This is the #1 determinant. Without good filters, we're flipping a biased coin (biased toward losses because of friction).

2. **Entry timing** — The 15s delay is an educated guess. If optimal delay is actually 5s or 45s, being wrong costs 2-5% per trade.

3. **Tail win capture** — The bull case depends on capturing 15% "full run" trades at +24.7%. If our trailing stop is too tight and we get shaken out at +8%, the tail is gone and the strategy bleeds.

4. **Data dependency** — Every number in this section is an ESTIMATE. We have zero empirical post-graduation price trajectory data because our pool resolution is broken. **The first priority is collecting real data.**

**Comparison to backrunner:**
- Backrunner golden segment: +0.19 SOL/day (proven with 5,307 trades of data)
- Momentum engine base case: +0.016 SOL/day (estimated, zero empirical validation)
- **The backrunner is 10× more proven and profitable per unit of effort.**

**Recommendation:** Build the momentum engine as a **data collection pipeline first** (Phase 1-2), not as a live trading engine. The real value is collecting 1,000+ post-graduation price trajectories to validate or falsify the base case assumptions. Only go live after empirical data confirms E[trade] > 0.

### 7.8 What Would Make This Clearly Profitable?

The strategy becomes unambiguously profitable if ANY of these conditions hold:

1. **Social signal integration** — If we can identify tokens with active Twitter/Telegram momentum before graduation, we can boost WR to 40%+ → bull case territory (+0.22 SOL/day)

2. **Holder distribution data** — On-chain analysis of top holder % at graduation. Filtering out concentrated tokens (top wallet > 5%) likely eliminates 30%+ of future rug scenarios → lower SL rate.

3. **Graduation speed as a strong predictor** — If fast graduations (< 20 min) pump at 2× the rate of slow ones, this single feature gets us to 35%+ TP rate.

4. **PumpSwap fee reduction** — If PumpSwap reduces its 1% swap fee, momentum trading on 90%+ of graduations becomes viable.

5. **Better exit strategy** — ML-trained exit timing (when to hold vs sell) could increase avg win by 30-50%.

---

## 8. Implementation Roadmap

### Phase 1: Data Collection Pipeline (Week 1-2)

**Goal:** Collect 2,000+ post-graduation price trajectories with correct pool resolution.

**Prerequisites (from GRAD_ARB_QUANT_SPEC.md):**
- Fix pool resolution using Path C (tx-based vault extraction)
- Successfully resolve vault addresses for >90% of Raydium graduations
- Validate structural discount is ~7% (not 27-42%)

**Implementation:**
1. On graduation detection → resolve pool vaults (Path C: parse tx → extract accounts[10], accounts[11])
2. Schedule price polls at T=[5, 10, 15, 30, 60, 120, 300, 600] seconds
3. At each poll: `getMultipleAccountsInfo([coin_vault, pc_vault])` → record price
4. After T=600s: write complete trajectory record to `data/graduation_trajectories.jsonl`
5. NO trading — pure observation

**Success criteria:**
- >90% pool resolution rate (vs current 0.03%)
- 2,000+ complete trajectory records collected
- Structural discount validated at 6-8%

**Deliverables:**
- `graduation_trajectories.jsonl` with all fields from Section 6.1 (except trade execution fields)
- Statistical analysis: what % of tokens show ≥5/10/15/30% pump from various entry delays

### Phase 2: Entry Signal Calibration (Week 3-4)

**Goal:** Use trajectory data to calibrate entry timing and gate parameters.

**Analysis:**
1. For each entry delay candidate [5s, 10s, 15s, 30s, 60s]:
   - Calculate would-be entry price
   - Calculate MFE (max favorable excursion) over remaining 300s window
   - Calculate what % of entries would hit TP1/TP2/trail/SL
   - Calculate E[trade] at each delay

2. For each gate parameter:
   - Split trajectories by gate value (e.g., graduation speed < 20 min vs > 60 min)
   - Compare MFE distributions between groups
   - Calculate feature importance / predictive power
   - Identify top 3 predictive features

3. Paper trade simulation:
   - Replay all trajectories through the full entry/exit logic
   - Calculate realized P&L with exact exit rules
   - Optimize parameters for maximum Sharpe ratio (not just P&L — control for variance)

**Deliverables:**
- Optimal entry delay (ms)
- Calibrated gate thresholds
- Ranked feature importance list
- Backtest P&L curve with confidence intervals
- GO/NO-GO decision on Phase 3

### Phase 3: Paper Trading (Week 5-6)

**Goal:** Run the full momentum engine in paper mode with calibrated parameters.

**Implementation:**
1. Implement full entry/exit logic in Rust (new `MomentumEngine` struct)
2. Wire to existing graduation detection pipeline
3. Run for 2 weeks, collecting paper trades
4. Compare realized P&L distribution to Phase 2 backtest
5. If paper P&L is within 1σ of backtest → proceed to Phase 4

**Key metrics to track:**
- Realized WR (target: >30% for TP2+)
- Average win / average loss ratio (target: >2:1)
- Max drawdown (target: <2 SOL)
- Sharpe ratio (target: >0.5 annualized)
- Correlation with backrunner P&L (want LOW correlation — diversification)

### Phase 4: Live Trading (Week 7+)

**Goal:** Go live with minimal size, scale if profitable.

**Scaling plan:**
```
Week 7-8:  0.10 SOL per trade, max 2 concurrent → max 0.20 SOL deployed
Week 9-10: 0.20 SOL per trade, max 2 concurrent → max 0.40 SOL deployed
Week 11+:  0.30 SOL per trade, max 3 concurrent → max 0.90 SOL deployed (full size)
```

**Kill conditions (abort live trading):**
- Net loss > 1.5 SOL cumulative → stop, re-analyze
- WR < 20% after 100 trades → stop, re-calibrate
- Average loss > 15% → stop, tighten SL or re-evaluate

### Phase 5: Social Signal Integration (Week 9+, parallel)

**Goal:** Add Twitter/Telegram social signals to boost entry filter quality.

**Implementation options:**
1. Twitter API — track mentions of token name/ticker in 5 min before/after graduation
2. Telegram scraping — detect group creation linked to token
3. Pump.fun API — if available, check comment/reaction activity on token page

**Expected impact:** If social signals can predict pumps with 60%+ accuracy, this transforms the strategy from marginal to clearly profitable (bull case → sustained).

---

## Appendix A: Key Numbers Summary

```
╔══════════════════════════════════════════════════════════╗
║           MOMENTUM ENGINE KEY METRICS                    ║
╠══════════════════════════════════════════════════════════╣
║                                                          ║
║  Expected Win Rate Range:     25-40% (TP2+ exits)        ║
║  Break-even Win Rate:         ~46% (at base loss model)  ║
║  Break-even WR (0.3 SOL pos): ~46% for TP2+ rate        ║
║                                                          ║
║  Daily P&L Range:                                        ║
║    Bear case:  -0.15 SOL/day                             ║
║    Base case:  +0.016 SOL/day                            ║
║    Bull case:  +0.22 SOL/day                             ║
║                                                          ║
║  Top 3 Entry Signal Features (by predicted power):       ║
║    1. Pre-graduation buying velocity (buys/10s)          ║
║       → Direct momentum proxy, analogous to backrunner   ║
║          buys1s (strongest proven signal)                 ║
║    2. Graduation speed (creation → graduation time)      ║
║       → Fast = strong demand, likely continues            ║
║    3. Price recovery at entry time (T+15s vs opening)    ║
║       → Recovery = dump absorbed, new demand arriving     ║
║                                                          ║
║  Position size:          0.30 SOL                        ║
║  Max concurrent:         3                               ║
║  Max daily loss:         1.00 SOL                        ║
║  Total friction/trade:   1.30% (Raydium)                 ║
║  Entry delay:            15,000 ms (to be calibrated)    ║
║  Max hold time:          300,000 ms (5 min)              ║
║  Hard stop-loss:         -12%                            ║
║                                                          ║
║  VERDICT: Marginal. Build as data pipeline first.        ║
║  GO/NO-GO after 2,000+ trajectory samples.               ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝
```

## Appendix B: Comparison with Graduation Arb Engine

| Dimension | Graduation Arb | Momentum Engine |
|-----------|---------------|-----------------|
| Edge type | Structural (deterministic) | Directional (probabilistic) |
| Win rate | N/A (arb is binary) | 25-40% |
| Entry timing | ASAP (speed race) | T+15s (deliberate) |
| Hold time | 1-5 seconds | 30-300 seconds |
| Position size | 0.3 SOL | 0.3 SOL |
| Jito bundle | Yes (speed critical) | No (not speed-sensitive) |
| Competition | Extreme (Geyser bots) | Low-moderate (not a known strategy) |
| Pool types | Raydium only (7% spread) | Raydium first, PumpSwap later |
| Data dependency | Low (structural, calculable) | High (empirical calibration needed) |
| Expected daily P&L | +0.02-0.07 SOL | -0.15 to +0.22 SOL |
| Risk profile | Low (bounded arb) | Medium-High (directional) |
| Implementation | Needs pool resolution fix | Needs pool resolution fix + entry/exit logic |

**These engines share infrastructure (graduation detection, pool resolution) but are fundamentally different strategies. Build both on the same pipeline.**

## Appendix C: PumpSwap Considerations

PumpSwap graduations are ~10× more frequent than Raydium but have higher friction:

| Factor | Raydium | PumpSwap |
|--------|---------|----------|
| Graduations/day | ~15,700 | ~1,690 (est. growing) |
| Swap fee per leg | 0.25% | 1.00% |
| Round-trip friction | 1.30% | 2.77% |
| Structural discount | 7.07% | 1.78% |
| Pool depth | ~79 SOL | ~83.5 SOL |
| Breakeven move | 1.30% | 2.77% |

**PumpSwap momentum trading requires 2× larger price moves to be profitable.** The 1% swap fee per leg is brutal for a momentum strategy targeting 5-15% moves.

**Wait for:** Either PumpSwap fee reduction, or empirical evidence that PumpSwap graduations pump harder (possible — the lower structural discount means less initial dump pressure → better entry price → better momentum capture).

Note: These daily frequency numbers (15,700 Raydium / 1,690 PumpSwap) may be inverted from the current reality. Since March 2025, the majority of graduations have shifted to PumpSwap. The exact split in March 2026 needs empirical validation. If Raydium events are actually rare (5-30/day), the momentum engine's Raydium-only constraint severely limits trade volume. Phase 1 data collection should measure the true split.

## Appendix D: Backrunner Synergy

The momentum engine can leverage data already collected by the backrunner:

| Backrunner Data | Momentum Engine Use |
|----------------|-------------------|
| `preTriggerBuys1s` | Pre-graduation velocity signal (if token is in bonding curve) |
| `uniqueBuyerCount` | Holder distribution proxy |
| `vSol` progression | Graduation speed estimation (track vSol over time) |
| `creatorSellDetection` | If creator sells pre-graduation → skip |
| `tokenAge` | Input to graduation speed calculation |

**Integration point:** When backrunner detects a token approaching graduation (vSol > 70), it can pre-compute and cache momentum engine entry metadata. When graduation fires, the momentum engine has instant access to pre-computed features without additional on-chain lookups.

```rust
// In BackrunEngine, when vSol > 70 SOL:
if token.vsol > 70_000_000_000 {
    let momentum_cache = MomentumPreCache {
        mint: token.mint,
        buys_last_10s: token.recent_buys_10s,
        unique_buyers: token.unique_buyer_count,
        max_holder_pct: token.max_holder_pct,
        graduation_speed_est: now_ms - token.creation_ts_ms,
        creator_sold: token.creator_sell_detected,
        pre_grad_volume_5min_sol: token.volume_last_5min_sol,
    };
    momentum_engine.pre_cache(momentum_cache);
}
```

This pre-caching eliminates the need for separate on-chain queries at graduation time and adds zero latency to the momentum engine's entry decision.

---

*End of spec. Priority: Fix pool resolution (shared dependency), then Phase 1 data collection. GO/NO-GO decision after 2,000+ trajectories.*
