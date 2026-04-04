# Sniper Exit Architecture — Atomic vs Hold-and-Ride Analysis

**Version:** 1.0  
**Date:** 2026-04-04  
**Author:** Opus 4.6 Quant Architect  
**Status:** ARCHITECTURE DECISION — replaces Bundle Construction section of SNIPER_SIGNAL_SPEC.md  
**Scope:** Execution model selection, exit architecture, EV recalculation, updated spec sections

---

## Part 1: Architecture Analysis — Atomic Bundle vs Hold-and-Ride

### 1.1 The Critical Flaw in the Atomic Bundle Model

**The current spec's TX2 (atomic sell at +12%) is fundamentally misaligned with how bonding curves work.**

Here's the precise problem:

A pump.fun bonding curve is a **deterministic pricing function**: `P = vsol² / K`. There is no order book. There are no limit orders. Every trade is a **market order against the curve**. When you call the `sell` instruction, the curve computes your output SOL based on the **current reserve state at execution time**.

In a Jito bundle `[TX1_buy, TX2_sell]`:

1. **TX1 executes:** Our buy adds SOL to the curve. `vsol` increases. Price moves up by exactly our purchase impact. If we bought 0.05 SOL at `real_sol=5`, the new `real_sol ≈ 5.049` (after 1.25% fee).
2. **TX2 executes immediately after TX1 in the same slot.** The curve state now reflects our buy. But no other buyer has acted yet (our bundle is atomic — no external transactions between TX1 and TX2).
3. **TX2 sells at the current curve price**, which is slightly ABOVE our buy price due to our own impact, minus the sell fee (1.25%).

**The net result of an atomic buy+sell bundle on a bonding curve:**

```
Buy at price P1 → curve moves to P1' (slightly above P1, by our impact)
Sell at price P1' → receive SOL back at P1' minus fees

Net PnL = (price_impact_of_our_buy × token_amount × FEE_RATE) - (buy_fee + sell_fee)

For small positions (0.05 SOL into real_sol=5):
  Our price impact ≈ 0.05/35 ≈ 0.14% of vsol
  P1' ≈ P1 × 1.0029 (price moves ~0.29%)
  Buy fee: 0.05 × 0.0125 = 0.000625 SOL
  Sell fee: ~0.05 × 0.0125 = 0.000625 SOL
  Total round-trip fee: ~0.00125 SOL
  Our "gain" from self-impact: ~0.000145 SOL
  
  NET LOSS ≈ -0.00110 SOL per atomic round-trip
```

**An atomic buy+sell bundle on a bonding curve is a guaranteed net loss equal to approximately the round-trip fee.** The curve doesn't magically reach +12% between TX1 and TX2. TX2 sells back at the price we just pushed it to, which is nowhere near +12% above entry.

### 1.2 What the Current Spec's TX2 Actually Does

The spec sets `min_sol_output` on TX2 based on a +12% target price:

```
target_vsol = sqrt(P1 × 1.12 × K)
min_sol_output = (target_vsol - vsol_after_tx1) × FEE_RATE × 0.85 × 1e9
```

When TX2 executes in the same bundle as TX1:
- `vsol_after_tx1` ≈ `entry_vsol + 0.049` (our 0.05 SOL buy after fee)
- The curve hasn't reached `target_vsol` yet
- The sell instruction attempts to sell our tokens at current reserves
- `sol_out` from selling is much less than `min_sol_output` (because we need +12% but only got +0.3%)
- **TX2 fails due to slippage check** (`sol_out < min_sol_output`)
- Since TX2 fails, the Jito bundle fails atomically
- **TX1 also doesn't land**
- Net loss = Jito tip only (~0.00005 SOL)

**This means the "atomic sniper" model as currently specced will have a 100% bundle rejection rate.** TX2 can never succeed unless other buyers happen to push the curve to +12% in the exact same slot AND their transactions are ordered between our TX1 and TX2 — which Jito bundles do NOT allow. Jito bundles execute the contained transactions sequentially with no external interleaving.

### 1.3 Could We Fix the Atomic Model?

Three potential fixes:

**Fix A: Remove min_sol_output (set to 0)**
- TX2 would sell immediately at whatever price the curve is at post-TX1
- This turns the "sniper" into a guaranteed-loss sandwich of yourself
- Net PnL = -(round_trip_fees) ≈ -2.5% per attempt
- **Not viable.**

**Fix B: Bundle [TX1_buy, <wait for other buys>, TX2_sell]**
- Not possible. Jito bundles are atomic within a single slot. You cannot interleave external transactions between your bundle's TXs.
- You could submit TX1 as one bundle and TX2 as a separate future bundle, but this breaks atomicity — now you have an open position.
- **This IS the hold-and-ride model.**

**Fix C: Sandwich attack model — front-run a detected large buy**
- If we detect a pending large buy in the mempool, we could bundle: [our_buy, victim_buy, our_sell]
- This is a classic sandwich MEV attack
- **This works technically** but: (a) pump.fun transactions don't go through a public mempool — they go through pump.fun's API server, (b) Jito bundles need the victim TX to be in the bundle, which we can't control, (c) this is adversarial extraction not aligned with the signal-based strategy
- **Not the strategy we're building.**

### 1.4 Conclusion: The Atomic Bundle Model is Not Viable for Bonding Curve Sniping

The atomic buy+sell model is a misunderstanding of bonding curve mechanics applied from orderbook-style thinking. On an orderbook, you can place a limit sell that persists until filled. On a bonding curve, every sell is a market sell at the current curve state.

**The only viable execution model for bonding curve sniping is: buy, hold, monitor, sell when profitable (or when stop-loss triggers).**

The current SNIPER_SIGNAL_SPEC.md claims "no open position risk" — this was based on the assumption that TX2 exits atomically. Since TX2 cannot exit at +12% atomically, the near-zero-risk profile was an illusion. Every real entry creates an open position that must be managed.

### 1.5 Model B (Hold + Ride) Analysis

Given that atomic exit is not viable, let's honestly evaluate hold-and-ride:

**Structural advantages for pump.fun bonding curve tokens:**

1. **Momentum is the core signal.** The entire SNIPER_SIGNAL_SPEC scoring system (S1 velocity, S2 wallet diversity, S4 sell timing, S5 smart money) measures momentum quality. Momentum-driven tokens on pump.fun regularly move 50%–500%+ from entry point. Capturing even a fraction of this move dominates any 12% target.

2. **Follow-on buying IS the product.** The spec already calculates follow-on needed (0.25 SOL at real_sol=5). These tokens attract follow-on because pump.fun's UI prominently features new tokens with activity. The scoring system is designed to identify tokens that will receive this organic follow-on.

3. **Exit infrastructure already exists.** The DYNAMIC_EXIT_FRAMEWORK_V2.md describes a sophisticated Bayesian urgency-based exit system with momentum divergence detection, volatility-adaptive trailing stops, and partial exit capability. This system was built for the existing RIDE engine (post-graduation trading). It can be adapted for bonding curve positions.

4. **Position sizes are small.** At 0.01–0.10 SOL per position, a 50% stop-loss means max loss of 0.005–0.05 SOL per trade. This is manageable even at high frequency.

**Structural risks:**

1. **Pump.fun tokens dump fast.** Unlike graduated tokens with AMM liquidity, bonding curve tokens can go from peak to near-zero in seconds. A 50% stop-loss may experience significant slippage if the curve is thin.

2. **Rug risk.** Creator can dump supply. G3 (serial rugger check) and G7 (concentration check) mitigate this, but not eliminate it.

3. **Position monitoring required.** Must subscribe to bonding curve account updates and trade events to detect exit conditions in real-time.

4. **Graduation event changes liquidity.** If the token graduates while we hold, liquidity moves from bonding curve to PumpSwap AMM. We need to handle this transition.

---

## Part 2: Recommended Model — Hybrid Entry + Hold-and-Ride Exit

### 2.0 Recommendation: Model C — Jito Atomic Entry, Dynamic Hold-and-Ride Exit

**Why hybrid, not pure hold-and-ride:**

- **Entry still uses Jito bundles** — but only TX1 (buy). No TX2.
- Jito ensures we get priority entry at our target curve position (frontrun protection, guaranteed slot inclusion with tip)
- **Exit is event-driven hold-and-ride** with the dynamic exit framework

**Why not keep TX2 at all:**

TX2 is removed entirely. It cannot work on a bonding curve as demonstrated in §1.1–1.3. The "safety net" of atomic rejection was actually a 100% rejection rate with the only cost being wasted Jito tips.

### 2.1 Exit Decision Tree

```
ENTRY: Jito bundle [TX1_buy, tip_tx] lands
  → Position opens with entry_vsol, entry_price, token_amount
  → Subscribe to BondingCurveState updates (already tracked by ShredStream)
  → Start exit monitoring

EXIT MONITORING (on every trade event for this mint):

  ┌────────────────────────────────────────────────────────────┐
  │                    EMERGENCY EXITS                         │
  │  (bypass all other logic — categorical threats)            │
  ├────────────────────────────────────────────────────────────┤
  │ E1: Creator sells ANY tokens      → FULL EXIT immediately │
  │ E2: Token graduates               → TRANSITION (see §2.5) │
  │ E3: Position age > MAX_HOLD_SEC   → FULL EXIT at market   │
  │ E4: Bonding curve vsol < entry    → evaluate SL           │
  │     vsol (net selling > buying)                            │
  └────────────────────────────────────────────────────────────┘
              │ no emergency
              ▼
  ┌────────────────────────────────────────────────────────────┐
  │                    STOP-LOSS CHECK                         │
  ├────────────────────────────────────────────────────────────┤
  │ current_price = vsol² / K                                 │
  │ pnl_pct = (current_price - entry_price) / entry_price     │
  │                                                            │
  │ if pnl_pct <= -HARD_STOP_PCT:  → FULL EXIT at market      │
  │    (HARD_STOP_PCT = 0.30, see §2.3 for why not 0.50)      │
  │                                                            │
  │ if trailing_stop_active AND                                │
  │    current_price < peak_price × (1 - TRAIL_PCT):           │
  │    → FULL EXIT at market                                   │
  └────────────────────────────────────────────────────────────┘
              │ no stop hit
              ▼
  ┌────────────────────────────────────────────────────────────┐
  │                    MOMENTUM CHECK                          │
  ├────────────────────────────────────────────────────────────┤
  │ buy_gap_sec = time_since_last_buy_event                    │
  │ if buy_gap_sec > BUY_GAP_TIMEOUT_SEC:                      │
  │    if pnl_pct > 0: → FULL EXIT (take profit, momentum     │
  │                       dead)                                │
  │    if pnl_pct <= 0: → FULL EXIT (stalled, cut loss)       │
  │                                                            │
  │ sell_acceleration:                                         │
  │    if sells_last_5_events >= 3 AND sell_vol > buy_vol:     │
  │    → FULL EXIT (dump cascade detected)                     │
  └────────────────────────────────────────────────────────────┘
              │ momentum intact
              ▼
  ┌────────────────────────────────────────────────────────────┐
  │                    TAKE-PROFIT TARGETS                     │
  ├────────────────────────────────────────────────────────────┤
  │ // Partial exits to lock in gains while allowing upside    │
  │                                                            │
  │ TP1: pnl_pct >= +20%                                      │
  │   → Sell 30% of position                                  │
  │   → Activate trailing stop (TRAIL_PCT = 15%)              │
  │                                                            │
  │ TP2: pnl_pct >= +50%                                      │
  │   → Sell 30% of remaining position                        │
  │   → Tighten trailing stop (TRAIL_PCT = 10%)               │
  │                                                            │
  │ TP3: pnl_pct >= +100%                                     │
  │   → Sell 30% of remaining position                        │
  │   → Tighten trailing stop (TRAIL_PCT = 8%)                │
  │                                                            │
  │ Remaining position rides trailing stop to exit.            │
  └────────────────────────────────────────────────────────────┘
              │ no TP hit
              ▼
           HOLD — continue monitoring
```

### 2.2 Why These Take-Profit Levels

The TP structure is designed around pump.fun bonding curve empirical dynamics:

**+20% (TP1):** At entry real_sol=5, +20% means the curve needs to reach:
```
P_target = P_entry × 1.20
vsol_target = sqrt(P_entry × 1.20 × K) = entry_vsol × sqrt(1.20) = entry_vsol × 1.0954
Additional vsol needed: 35 × 0.0954 = 3.34 SOL of net inflow (real_sol goes from 5 → 8.34)
```
This is ~3.34 SOL of follow-on buying — well within the "EASY" range from the spec's follow-on table. Tokens that pass the scoring system should regularly reach this.

**+50% (TP2):** Requires `entry_vsol × (sqrt(1.50) - 1) = entry_vsol × 0.2247`, so additional vsol = 35 × 0.2247 = 7.86 SOL. Total real_sol = 12.86. This represents the token reaching the upper end of the optimal zone — a strong performer.

**+100% (TP3):** Requires `entry_vsol × (sqrt(2.0) - 1) = entry_vsol × 0.4142`, so additional vsol = 35 × 0.4142 = 14.5 SOL. Total real_sol = 19.5. This is near graduation territory — an exceptional performer.

**Why 30/30/30 not 40/40/20 or other splits:**
- First 30% at +20% locks in guaranteed profit that covers multiple losing trades
- Second 30% at +50% provides asymmetric upside
- Remaining 40% rides the trailing stop for tail captures (100%+ moves)
- This structure ensures we capture at least +20% × 30% = +6% of position even if the token reverses after TP1

### 2.3 Stop-Loss: 30%, Not 50%

The user proposed 50% stop-loss. I recommend **30%** for these reasons:

**Bonding curve math makes 50% SL almost unreachable in a useful way:**

At real_sol=5 entry (vsol=35), a 50% price drop means:
```
P_stop = P_entry × 0.50
vsol_stop = sqrt(P_entry × 0.50 × K) = entry_vsol × sqrt(0.50) = 35 × 0.7071 = 24.75
real_sol at stop = 24.75 - 30 = -5.25 → IMPOSSIBLE (can't go below 0)
```

**A 50% price drop below real_sol=5 entry literally cannot happen** — it would require the virtual SOL reserves to go below 24.75, but they started at 30.0 and only increase with buys. For the price to drop 50%, the bonding curve would need to lose ~14.6 SOL of reserves below the 35 entry point. Tokens at real_sol=5 have only 5 SOL of real reserves — even if ALL sellers liquidated everything, the curve can only drop back to vsol=30 (real_sol=0), which is:

```
P at vsol=30: (30)² / 3.219e10 = 2.796e-8
P at vsol=35: (35)² / 3.219e10 = 3.806e-8

Actual max possible price drop from real_sol=5: 
(3.806e-8 - 2.796e-8) / 3.806e-8 = 26.5%
```

**The theoretical maximum loss at real_sol=5 is ~26.5%** (if every token holder sells everything). At real_sol=10, the max loss from everyone selling back to vsol=30 is ~43.75%. At real_sol=15, it's ~55.6%.

So 50% SL only becomes theoretically reachable at entries above real_sol≈12. For the optimal zone (2-15), a 30% SL provides meaningful protection while being actually reachable as an exit trigger.

**Why 30% specifically:**
1. **Reachable at all entry zones:** At real_sol=5, a 30% drop requires vsol to go from 35→29.3, which requires selling back ~5.7 SOL of reserves. This is physically possible (there's 5 SOL of real reserves; the last 0.7 would come from extremely adverse price impact).
2. **Manageable loss:** 30% × 0.05 SOL = 0.015 SOL max loss per trade. At 100 trades, this is 1.5 SOL max drawdown scenario.
3. **Avoids premature exit:** 10-15% drops are normal on volatile pump.fun tokens before continuation. 30% SL gives room to breathe while protecting against genuine dumps.

**⚠️ CALIBRATION REQUIRED:** The 30% number is analytically derived, not empirically validated. After 200 trades, review the distribution of drawdowns on eventual winners vs losers. If >10% of eventual winners touch -25%, widen to 35%. If <5% of eventual winners touch -20%, tighten to 25%.

### 2.4 Time Stops

**MAX_HOLD_SEC = 120 seconds** (2 minutes)

Rationale:
- Pump.fun bonding curve tokens that will pump do so within the first 60-90 seconds of the activity window
- The scoring system (S1: velocity) already selects for tokens with high inflow rate
- At 0.15 SOL/s (sweet spot), the token reaches 15 SOL real_sol (top of optimal zone) in ~100 seconds
- If we entered at real_sol=5 and the token hasn't moved meaningfully in 120 seconds, the momentum thesis is dead
- 120s provides enough time for natural buying waves while preventing capital lockup

**BUY_GAP_TIMEOUT_SEC = 15 seconds**

If no buy events arrive for 15 seconds on a bonding curve token, the crowd has moved on. This is the primary momentum-death signal. On active pump.fun tokens, buy events typically arrive every 0.5-3 seconds during the active phase.

**SELL_CASCADE: 3 sells in 5 events**

Three or more sell events in the last 5 trade events signals a dump. Combined with sell_vol > buy_vol, this is an early exit trigger before the stop-loss is hit.

### 2.5 Graduation Transition

If the token graduates while we hold a position:

1. **Bonding curve closes.** No more buy/sell on the curve.
2. **PumpSwap pool opens.** Liquidity migrates to AMM.
3. **Our tokens are still in our wallet.** We need to sell on PumpSwap.

**Graduation handling:**
```
on_graduation_detected(mint):
  if holding_position(mint):
    // Our bonding curve position tokens are now tradeable on PumpSwap
    // The migration event creates a PumpSwap pool with the remaining liquidity
    
    // Option A: Sell immediately on PumpSwap
    //   Pro: Guaranteed exit, no further risk
    //   Con: Graduation is often the START of a bigger move (2-10x post-grad)
    
    // Option B: Transition to RIDE engine
    //   Pro: Capture post-graduation momentum using existing RIDE exit system
    //   Con: Adds complexity, position is now in AMM territory
    
    // RECOMMENDED: Option A for v1. Sell at market on PumpSwap.
    // Graduation means our token hit 85 real_sol. If we entered at real_sol=5:
    //   Price at entry: 35²/K = 3.806e-8
    //   Price at grad:  115²/K = 4.109e-7
    //   Gain: 4.109e-7 / 3.806e-8 = 10.8× (980% gain)
    // Even selling immediately at graduation captures enormous upside.
    
    // For v2: hand off to RIDE engine for post-graduation trailing stop.
    submit_pumpswap_sell(mint, full_remaining_position)
```

### 2.6 Emergency Exit: Bundle Partial Failure

With the new model (TX1 only, no TX2), partial bundle failure means TX1 fails and we don't enter at all. This is actually simpler than the old model:

| Outcome | Probability | Result | Loss |
|---------|------------|--------|------|
| Bundle lands (TX1 succeeds) | ~60-80% | Position opens, monitoring begins | N/A (in position) |
| Bundle rejected by Jito | ~15-30% | Nothing happens | Jito tip (~0.00005 SOL) |
| Bundle expired | ~5-10% | Nothing happens | Jito tip |
| TX1 fails on-chain (slippage/etc) | ~1-3% | SOL stays in wallet, tip paid | Tip + priority fee |

**There is no partial failure case where we're stuck with tokens and no exit plan,** because TX2 doesn't exist. The worst case is TX1 lands and the exit monitoring system handles the position.

### 2.7 Full Exit Architecture Spec

```rust
/// Exit configuration for bonding curve sniper positions.
/// All percentage values stored as basis points (1 = 0.01%).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperExitConfig {
    // ── Hard Stop ──
    /// Maximum loss before forced exit (basis points). 3000 = 30%.
    pub hard_stop_bp: u16,                    // default: 3000

    // ── Time Stops ──
    /// Maximum hold time in seconds before forced exit.
    pub max_hold_sec: u16,                    // default: 120
    /// Seconds without a buy event → exit.
    pub buy_gap_timeout_sec: u16,             // default: 15
    /// Sell cascade: N sells in last M events triggers exit.
    pub sell_cascade_count: u8,               // default: 3
    pub sell_cascade_window: u8,              // default: 5

    // ── Take-Profit Levels ──
    /// TP1: profit threshold (bp) and sell fraction (permille).
    pub tp1_threshold_bp: u16,                // default: 2000 (+20%)
    pub tp1_sell_permille: u16,               // default: 300 (30%)
    /// TP2: profit threshold (bp) and sell fraction of remaining (permille).
    pub tp2_threshold_bp: u16,                // default: 5000 (+50%)
    pub tp2_sell_permille: u16,               // default: 300 (30%)
    /// TP3: profit threshold (bp) and sell fraction of remaining (permille).
    pub tp3_threshold_bp: u16,                // default: 10000 (+100%)
    pub tp3_sell_permille: u16,               // default: 300 (30%)

    // ── Trailing Stop ──
    /// Trail distance from peak (bp). Activated after TP1.
    pub trail_initial_bp: u16,                // default: 1500 (15%)
    /// Trail distance after TP2 hit (bp).
    pub trail_tp2_bp: u16,                    // default: 1000 (10%)
    /// Trail distance after TP3 hit (bp).
    pub trail_tp3_bp: u16,                    // default: 800 (8%)

    // ── Emergency ──
    /// Exit immediately on creator sell.
    pub exit_on_creator_sell: bool,           // default: true
    /// Exit immediately on graduation (v1). v2: hand off to RIDE.
    pub exit_on_graduation: bool,             // default: true

    // ── Execution ──
    /// Sell slippage tolerance (bp). Applied to min_sol_output.
    pub sell_slippage_bp: u16,                // default: 1500 (15%)
    /// Priority fee for exit TXs (microlamports).
    pub exit_priority_fee: u64,               // default: 50_000
    /// Max retry attempts for exit TXs.
    pub exit_max_retries: u8,                 // default: 3
}

impl Default for SniperExitConfig {
    fn default() -> Self {
        Self {
            hard_stop_bp: 3000,
            max_hold_sec: 120,
            buy_gap_timeout_sec: 15,
            sell_cascade_count: 3,
            sell_cascade_window: 5,
            tp1_threshold_bp: 2000,
            tp1_sell_permille: 300,
            tp2_threshold_bp: 5000,
            tp2_sell_permille: 300,
            tp3_threshold_bp: 10000,
            tp3_sell_permille: 300,
            trail_initial_bp: 1500,
            trail_tp2_bp: 1000,
            trail_tp3_bp: 800,
            exit_on_creator_sell: true,
            exit_on_graduation: true,
            sell_slippage_bp: 1500,
            exit_priority_fee: 50_000,
            exit_max_retries: 3,
        }
    }
}
```

```rust
/// Sniper position state — tracks an open bonding curve position.
/// Embedded in the sniper engine's position map.
#[derive(Debug, Clone)]
pub struct SniperPosition {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],

    // ── Entry State ──
    pub entry_vsol: f64,              // virtual SOL at entry
    pub entry_price: f64,             // vsol²/K at entry
    pub entry_real_sol: f64,          // vsol - 30.0 at entry
    pub tokens_held: u64,             // tokens received from buy
    pub position_sol: f64,            // SOL committed (buy amount)
    pub entry_time_ms: u64,

    // ── Current State ──
    pub current_vsol: f64,
    pub current_price: f64,
    pub peak_price: f64,              // high water mark
    pub peak_vsol: f64,

    // ── Exit Tracking ──
    pub remaining_permille: u16,      // 1000 = 100% of original position
    pub tp1_hit: bool,
    pub tp2_hit: bool,
    pub tp3_hit: bool,
    pub trailing_stop_active: bool,
    pub current_trail_bp: u16,        // current trailing stop distance

    // ── Momentum Tracking ──
    pub last_buy_time_ms: u64,        // last buy event timestamp
    pub last_trade_time_ms: u64,      // last any trade event timestamp
    pub recent_events: u8,            // ring buffer: bit-packed last 8 events (1=buy, 0=sell)
    pub recent_event_count: u8,       // how many events we've seen

    // ── Outcome (filled on close) ──
    pub exit_reason: Option<SniperExitReason>,
    pub total_sol_received: f64,      // sum of all sell proceeds
    pub realized_pnl_sol: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SniperExitReason {
    HardStop,           // -30% stop-loss
    MaxHold,            // 120s time limit
    BuyGapTimeout,      // no buys for 15s
    SellCascade,        // dump detected
    TrailingStop,       // trail from peak
    TakeProfit1,        // +20% partial
    TakeProfit2,        // +50% partial
    TakeProfit3,        // +100% partial
    CreatorSell,        // dev dumped
    Graduation,         // token graduated (sell on PumpSwap)
    ManualExit,         // operator intervention
}
```

```rust
impl SniperPosition {
    /// Called on every trade event for this mint.
    /// Returns an exit action, or None for hold.
    pub fn on_trade_event(
        &mut self,
        event: &SniperTradeEvent,
        config: &SniperExitConfig,
        now_ms: u64,
    ) -> Option<SniperExitAction> {
        // Update current state
        self.current_vsol = event.vsol_after;
        self.current_price = event.vsol_after * event.vsol_after / BondingCurveState::K;
        self.last_trade_time_ms = now_ms;

        // Update peak
        if self.current_price > self.peak_price {
            self.peak_price = self.current_price;
            self.peak_vsol = self.current_vsol;
        }

        // Track momentum (buy/sell ring buffer)
        self.recent_event_count = self.recent_event_count.saturating_add(1).min(8);
        self.recent_events = (self.recent_events << 1) | if event.is_buy { 1 } else { 0 };
        if event.is_buy {
            self.last_buy_time_ms = now_ms;
        }

        // ── E1: Creator sell ──
        if config.exit_on_creator_sell && event.is_creator_sell {
            self.exit_reason = Some(SniperExitReason::CreatorSell);
            return Some(SniperExitAction::FullExit);
        }

        // ── PnL calculation ──
        let pnl_bp = ((self.current_price - self.entry_price)
            / self.entry_price * 10_000.0) as i32;

        // ── Hard Stop ──
        if pnl_bp <= -(config.hard_stop_bp as i32) {
            self.exit_reason = Some(SniperExitReason::HardStop);
            return Some(SniperExitAction::FullExit);
        }

        // ── Time Stop ──
        let hold_sec = (now_ms.saturating_sub(self.entry_time_ms)) / 1000;
        if hold_sec >= config.max_hold_sec as u64 {
            self.exit_reason = Some(SniperExitReason::MaxHold);
            return Some(SniperExitAction::FullExit);
        }

        // ── Buy Gap Timeout ──
        if self.last_buy_time_ms > 0 {
            let gap_sec = (now_ms.saturating_sub(self.last_buy_time_ms)) / 1000;
            if gap_sec >= config.buy_gap_timeout_sec as u64 {
                self.exit_reason = Some(SniperExitReason::BuyGapTimeout);
                return Some(SniperExitAction::FullExit);
            }
        }

        // ── Sell Cascade ──
        if self.recent_event_count >= config.sell_cascade_window {
            let window = config.sell_cascade_window.min(8);
            let mask = (1u8 << window) - 1;
            let recent = self.recent_events & mask;
            let sell_count = window - recent.count_ones() as u8;
            if sell_count >= config.sell_cascade_count {
                self.exit_reason = Some(SniperExitReason::SellCascade);
                return Some(SniperExitAction::FullExit);
            }
        }

        // ── Trailing Stop (only active after TP1) ──
        if self.trailing_stop_active && self.peak_price > 0.0 {
            let trail_stop_price = self.peak_price
                * (1.0 - self.current_trail_bp as f64 / 10_000.0);
            if self.current_price <= trail_stop_price {
                self.exit_reason = Some(SniperExitReason::TrailingStop);
                return Some(SniperExitAction::FullExit);
            }
        }

        // ── Take Profit Levels ──
        if !self.tp1_hit && pnl_bp >= config.tp1_threshold_bp as i32 {
            self.tp1_hit = true;
            self.trailing_stop_active = true;
            self.current_trail_bp = config.trail_initial_bp;
            let sell_permille = config.tp1_sell_permille;
            let sell_tokens = self.compute_partial_sell(sell_permille);
            self.exit_reason = Some(SniperExitReason::TakeProfit1);
            return Some(SniperExitAction::PartialExit {
                tokens: sell_tokens,
                reason: SniperExitReason::TakeProfit1,
            });
        }

        if !self.tp2_hit && pnl_bp >= config.tp2_threshold_bp as i32 {
            self.tp2_hit = true;
            self.current_trail_bp = config.trail_tp2_bp;
            let sell_permille = config.tp2_sell_permille;
            let sell_tokens = self.compute_partial_sell(sell_permille);
            self.exit_reason = Some(SniperExitReason::TakeProfit2);
            return Some(SniperExitAction::PartialExit {
                tokens: sell_tokens,
                reason: SniperExitReason::TakeProfit2,
            });
        }

        if !self.tp3_hit && pnl_bp >= config.tp3_threshold_bp as i32 {
            self.tp3_hit = true;
            self.current_trail_bp = config.trail_tp3_bp;
            let sell_permille = config.tp3_sell_permille;
            let sell_tokens = self.compute_partial_sell(sell_permille);
            self.exit_reason = Some(SniperExitReason::TakeProfit3);
            return Some(SniperExitAction::PartialExit {
                tokens: sell_tokens,
                reason: SniperExitReason::TakeProfit3,
            });
        }

        None // HOLD
    }

    /// Compute tokens to sell for a partial exit.
    /// sell_permille is fraction of REMAINING position (not original).
    fn compute_partial_sell(&mut self, sell_permille: u16) -> u64 {
        let remaining_tokens = (self.tokens_held as u128
            * self.remaining_permille as u128 / 1000) as u64;
        let sell_tokens = (remaining_tokens as u128
            * sell_permille as u128 / 1000) as u64;
        // Update remaining
        let sold_permille = (self.remaining_permille as u32
            * sell_permille as u32 / 1000) as u16;
        self.remaining_permille = self.remaining_permille
            .saturating_sub(sold_permille);
        sell_tokens
    }

    /// Called on a periodic tick (e.g. every 500ms) for time-based exits.
    /// Separate from on_trade_event because some exits are time-driven,
    /// not trade-driven (buy_gap_timeout when no trades arrive).
    pub fn on_tick(
        &mut self,
        config: &SniperExitConfig,
        now_ms: u64,
    ) -> Option<SniperExitAction> {
        // Time stop
        let hold_sec = (now_ms.saturating_sub(self.entry_time_ms)) / 1000;
        if hold_sec >= config.max_hold_sec as u64 {
            self.exit_reason = Some(SniperExitReason::MaxHold);
            return Some(SniperExitAction::FullExit);
        }

        // Buy gap timeout (checked even without new trade events)
        if self.last_buy_time_ms > 0 {
            let gap_sec = (now_ms.saturating_sub(self.last_buy_time_ms)) / 1000;
            if gap_sec >= config.buy_gap_timeout_sec as u64 {
                self.exit_reason = Some(SniperExitReason::BuyGapTimeout);
                return Some(SniperExitAction::FullExit);
            }
        } else {
            // No buy seen at all — check time since entry
            let since_entry_sec = (now_ms.saturating_sub(self.entry_time_ms)) / 1000;
            if since_entry_sec >= config.buy_gap_timeout_sec as u64 {
                self.exit_reason = Some(SniperExitReason::BuyGapTimeout);
                return Some(SniperExitAction::FullExit);
            }
        }

        None // HOLD
    }
}

#[derive(Debug, Clone)]
pub enum SniperExitAction {
    FullExit,
    PartialExit {
        tokens: u64,
        reason: SniperExitReason,
    },
}
```

---

## Part 3: EV Implications — Recalculated for Hold-and-Ride

### 3.1 New Payoff Structure

The payoff is fundamentally different from the atomic model:

**Atomic model (old, non-viable):**
- Win: +12% minus 2.5% fees = +9.5% net
- Loss: Jito tip only = ~0 (but actually 100% rejection rate)
- Break-even WR: ~46% (theoretical, but irrelevant since it can't work)

**Hold-and-ride model (new):**
- Win (TP1 only): +20% × 30% of position = +6% of initial position, minus fees
- Win (TP1+TP2): +20%×30% + 50%×21% = +16.5% of initial position
- Win (TP1+TP2+TP3): +20%×30% + 50%×21% + 100%×14.7% = +31.2% of initial position
- Win (full trail capture): varies, potentially 50-200%+ on runners
- Loss (hard stop): -30% of position, minus fees
- Loss (buy gap timeout): variable, -5% to -15% typical
- Loss (sell cascade): variable, -10% to -25% typical

### 3.2 Simplified EV Model

For initial analysis, let's model with averaged outcomes:

**Win scenario (token pumps enough to hit TP1):**
- Average winning exit: assume blended +25% of initial position (accounting for partial exits and trail stops)
- After 2.5% round-trip fees: +22.5% net
- Net win in SOL (0.05 position): +0.01125 SOL

**Loss scenario (token dumps or stalls):**
- Average losing exit: assume -20% of initial position (blend of hard stop at -30%, timeouts at -10%)
- After fees on exit only (1.25%): -21.25% net
- Net loss in SOL (0.05 position): -0.01063 SOL

### 3.3 Break-Even Win Rate

```
Break-even: WR × net_win + (1-WR) × net_loss = 0
WR × 0.01125 + (1-WR) × (-0.01063) = 0
WR × 0.01125 - 0.01063 + WR × 0.01063 = 0
WR × 0.02188 = 0.01063
WR = 0.01063 / 0.02188 = 0.4859

Break-even WR ≈ 48.6%
```

### 3.4 EV Table by Win Rate and Position Size

**Assumptions:**
- Win: +22.5% net of position
- Loss: -21.25% net of position
- Jito tip per entry: 0.0001 SOL (applied to all trades)

| Position Size | Win Rate | Net Win | Net Loss | Jito Tip | EV per Trade | EV per 100 Trades |
|---|---|---|---|---|---|---|
| 0.01 SOL | 35% | +0.00225 | -0.00213 | -0.0001 | **-0.00071** | **-0.071** |
| 0.01 SOL | 40% | +0.00225 | -0.00213 | -0.0001 | **-0.00048** | **-0.048** |
| 0.01 SOL | 45% | +0.00225 | -0.00213 | -0.0001 | **-0.00024** | **-0.024** |
| 0.01 SOL | 50% | +0.00225 | -0.00213 | -0.0001 | **+0.00000** | **+0.000** |
| 0.01 SOL | 55% | +0.00225 | -0.00213 | -0.0001 | **+0.00024** | **+0.024** |
| 0.05 SOL | 35% | +0.01125 | -0.01063 | -0.0001 | **-0.00354** | **-0.354** |
| 0.05 SOL | 40% | +0.01125 | -0.01063 | -0.0001 | **-0.00235** | **-0.235** |
| 0.05 SOL | 45% | +0.01125 | -0.01063 | -0.0001 | **-0.00116** | **-0.116** |
| 0.05 SOL | 50% | +0.01125 | -0.01063 | -0.0001 | **+0.00003** | **+0.003** |
| 0.05 SOL | 55% | +0.01125 | -0.01063 | -0.0001 | **+0.00122** | **+0.122** |
| 0.10 SOL | 40% | +0.02250 | -0.02125 | -0.0001 | **-0.00460** | **-0.460** |
| 0.10 SOL | 50% | +0.02250 | -0.02125 | -0.0001 | **+0.00053** | **+0.053** |
| 0.10 SOL | 55% | +0.02250 | -0.02125 | -0.0001 | **+0.00248** | **+0.248** |

### 3.5 Comparison to Atomic Model

| Metric | Atomic Model (old) | Hold-and-Ride (new) |
|---|---|---|
| Break-even WR | ~46% (theoretical) | ~48.6% |
| Max loss per trade | ~0 (Jito tip) | -30% of position |
| Max win per trade | +9.5% net | Uncapped (trail captures 50%+) |
| Actual viability | ❌ NOT VIABLE (100% rejection) | ✅ VIABLE |
| EV at 50% WR, 0.05 SOL | N/A (can't execute) | +0.00003 SOL/trade |
| EV at 55% WR, 0.05 SOL | N/A | +0.00122 SOL/trade |
| Capital at risk per trade | ~0.0001 SOL | 0.005-0.03 SOL |
| Open position risk | None (immediate exit) | Yes (max 120s hold) |

**The hold-and-ride model has a slightly higher break-even WR (48.6% vs 46%) but is the only model that can actually execute.** The atomic model's "lower break-even" is meaningless because it has a 100% rejection rate.

### 3.6 Why the Model Gets Better with Tail Captures

The EV table above uses a flat +22.5% average win. In reality, the partial exit structure creates **positive skew**:

- Most wins will hit TP1 (+20%) and exit 30%, then trail-stop the rest around +10-15% → blended ~+12%
- Some wins will hit TP2 (+50%) → blended ~+25%
- Occasional wins will hit TP3 (+100%) → blended ~+45%
- Rare wins will ride to graduation (980%+) → blended ~+200%

The distribution of wins is right-skewed. If 60% of wins are TP1-only (+12% blended), 25% hit TP2 (+25%), 10% hit TP3 (+45%), and 5% ride to graduation (+200%):

```
Expected win = 0.60×12% + 0.25×25% + 0.10×45% + 0.05×200% = 7.2% + 6.25% + 4.5% + 10% = 27.95%

With 27.95% average win:
  Net win: 27.95% - 2.5% = 25.45%
  Break-even WR: 21.25 / (25.45 + 21.25) = 45.5%
```

**With tail captures, the break-even WR drops to ~45.5%**, which is achievable with the scoring system.

### 3.7 Sensitivity to Average Loss

The average loss assumption matters enormously:

| Avg Loss | Break-even WR (flat 22.5% win) | Break-even WR (skewed 25.45% win) |
|---|---|---|
| -10% | 30.8% | 28.2% |
| -15% | 38.5% | 37.1% |
| -20% | 44.0% | 44.0% |
| -25% | 52.6% | 49.6% |
| -30% | 57.1% | 54.1% |

**⚠️ The average loss on losers is the single most important calibration parameter.** If most losing trades exit via buy_gap_timeout at -5% to -10% rather than hitting the -30% hard stop, the system is much more EV-positive. Empirical calibration after 200 trades is critical.

---

## Part 4: Updated Spec Sections

### 4.1 Replacement for `## Bundle Construction`

*Replace the existing `## Bundle Construction` section in SNIPER_SIGNAL_SPEC.md with the following:*

---

## Entry Execution & Exit Architecture

### Entry Model: Jito Atomic Buy (TX1 Only)

**Architecture decision (v1.5→v2.0):** The atomic buy+sell bundle model (TX1 buy + TX2 sell in same bundle) is **not viable on bonding curves**. On a bonding curve, there is no order book — every sell is a market sell against the current curve reserves. TX2 would sell immediately after TX1 at approximately the same price (plus our tiny buy impact), minus round-trip fees = guaranteed loss. The `min_sol_output` slippage check on TX2 would cause 100% rejection rate for any meaningful target price.

**New model:** Entry-only Jito bundle with event-driven hold-and-ride exit.

### TX1: Bonding Curve Buy (unchanged)
```
Program: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
Discriminator: [102, 6, 61, 18, 1, 218, 235, 234]

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens to buy — computed from curve math)
  [16..24] max_sol_cost: u64 (position_size_lamports × 1.15 for 15% slippage)

Accounts: [same as existing spec — global, fee_recipient, mint, bonding_curve,
           assoc_bonding_curve, associated_user, user, system_program,
           token_program, rent, event_authority, program]
```

### TX2: REMOVED

TX2 (atomic sell) is removed from the bundle. All exit execution happens asynchronously via the exit engine described below.

### Jito Bundle (simplified)
```
Bundle = [TX1_buy, tip_tx]
Default tip: 100_000 lamports. Ladder up if congestion detected via TipEngine.
```

### Exit Sell Transaction (submitted by exit engine)
```
Program: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
Discriminator: [51, 230, 133, 164, 1, 127, 131, 173]

Instruction data:
  [0..8]   discriminator
  [8..16]  amount: u64 (tokens to sell — full or partial)
  [16..24] min_sol_output: u64 (current_price × tokens × 0.85 × FEE_RATE)

Accounts: [same sell accounts as existing spec]

Submitted via: direct RPC (sendTransaction) with priority fee.
NOT via Jito bundle — exit speed is more important than atomicity.
Priority fee: exit_priority_fee (default 50,000 microlamports).
Retry: up to exit_max_retries (default 3) with 200ms intervals.
```

### Exit Decision Engine

```
┌─────────────────────────────────────────────────────────────────────┐
│                    SNIPER EXIT ENGINE                                │
│  Input: BondingCurveState trade events (from ShredStream)           │
│  + periodic ticks (500ms interval)                                  │
│  Output: SniperExitAction (FullExit | PartialExit | Hold)           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Priority 1 — EMERGENCY EXITS (immediate, bypass all):              │
│    • Creator sell detected           → FULL EXIT                    │
│    • Token graduation detected       → FULL EXIT (sell on PumpSwap) │
│                                                                     │
│  Priority 2 — STOP-LOSS (capital preservation):                     │
│    • PnL ≤ -30% from entry           → FULL EXIT                   │
│    • Trailing stop triggered          → FULL EXIT                   │
│                                                                     │
│  Priority 3 — TIME/MOMENTUM STOPS (dead position cleanup):          │
│    • Hold time ≥ 120 seconds          → FULL EXIT                   │
│    • No buy event for ≥ 15 seconds    → FULL EXIT                   │
│    • 3+ sells in last 5 events        → FULL EXIT (sell cascade)    │
│                                                                     │
│  Priority 4 — TAKE PROFIT (lock gains, activate trail):             │
│    • PnL ≥ +20% (TP1, first hit)      → PARTIAL 30%, trail ON 15%  │
│    • PnL ≥ +50% (TP2, first hit)      → PARTIAL 30%, trail → 10%   │
│    • PnL ≥ +100% (TP3, first hit)     → PARTIAL 30%, trail → 8%    │
│                                                                     │
│  Default: HOLD — position remains open, monitoring continues.       │
└─────────────────────────────────────────────────────────────────────┘
```

### Position Lifecycle

```
Signal fires (score ≥ threshold, gates pass)
    │
    ▼
ENTRY: submit Jito bundle [TX1_buy, tip_tx]
    │
    ├── Bundle rejected → cost = tip only (~0.0001 SOL). Done.
    │
    ├── Bundle landed → Position opens
    │       │
    │       ▼
    │   MONITORING: ShredStream trade events + 500ms ticks
    │       │
    │       ├── Emergency exit (creator sell / graduation)
    │       ├── Hard stop (-30%)
    │       ├── Trailing stop (from peak, after TP1)
    │       ├── Time stop (120s)
    │       ├── Momentum stop (15s no buys / sell cascade)
    │       ├── TP1 → partial sell 30%, trail ON
    │       ├── TP2 → partial sell 30%, trail tighten
    │       ├── TP3 → partial sell 30%, trail tighten
    │       └── Remaining → rides trail to final exit
    │
    ▼
CLOSED: Log SniperTradeLog → data/sniper_trades.jsonl
        Update WinRateTracker
```

### Interaction with BondingCurveState Tracker

The exit engine reads from the same `BondingCurveState` tracker used by the signal system:

```rust
// In SniperEngine::on_shredstream_trade(mint, trade_event):
if let Some(position) = self.open_positions.get_mut(&mint) {
    // Feed trade to exit engine
    let action = position.on_trade_event(&trade_event, &self.exit_config, now_ms);
    match action {
        Some(SniperExitAction::FullExit) => {
            self.execute_full_exit(mint, position).await;
        }
        Some(SniperExitAction::PartialExit { tokens, reason }) => {
            self.execute_partial_sell(mint, tokens, reason).await;
        }
        None => {} // HOLD
    }
}

// Periodic tick (every 500ms via tokio interval):
for (mint, position) in self.open_positions.iter_mut() {
    let action = position.on_tick(&self.exit_config, now_ms);
    if let Some(action) = action {
        self.execute_exit(mint, position, action).await;
    }
}
```

### SniperEntrySignal Struct Updates

```rust
pub struct SniperEntrySignal {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub on_chain_score: OnChainScore,
    pub social_score: Option<SocialScore>,
    pub final_score: u8,
    // REMOVED: kelly_p (not applicable to this model)
    pub position_size_sol: f64,
    pub entry_vsol: f64,
    pub entry_price: f64,
    // REMOVED: target_vsol, target_price (no atomic exit target)
    pub buy_sol_lamports: u64,
    pub min_tokens_out: u64,
    // REMOVED: sell_tokens, min_sol_out (no TX2)
    pub jito_tip_lamports: u64,
    pub decision_ms: u64,
    pub paper_mode: bool,
    // NEW: exit config snapshot at entry time
    pub exit_config: SniperExitConfig,
    // NEW: sizing metadata
    pub sizing_win_rate: f64,
    pub sizing_wr_scalar: f64,
    pub sizing_score_tier: String,
    pub sizing_zone_mult: f64,
    pub sizing_dd_mult: f64,
}
```

### SniperTradeLog Updates

```rust
#[derive(Serialize)]
pub struct SniperTradeLog {
    // ... existing fields (mint, scores, sizing) ...
    
    // REMOVED: bundle_sig for TX2
    pub entry_bundle_sig: Option<String>,
    
    // NEW: exit tracking
    pub exit_reason: SniperExitReason,
    pub exit_time_ms: u64,
    pub hold_duration_ms: u64,
    pub peak_pnl_bp: i32,            // max unrealized gain (bp)
    pub exit_pnl_bp: i32,            // actual exit PnL (bp)
    pub tp1_hit: bool,
    pub tp2_hit: bool,
    pub tp3_hit: bool,
    pub trailing_stop_active: bool,
    pub remaining_at_exit_permille: u16,  // how much position was left at final exit
    pub partial_exit_count: u8,       // number of partial exits executed
    pub total_sol_received: f64,      // sum of all sell proceeds
    pub realized_pnl_sol: f64,        // net PnL in SOL
    pub trade_count_during_hold: u32, // trades observed while holding
    pub buy_count_during_hold: u32,
    pub sell_count_during_hold: u32,
    pub graduated_during_hold: bool,
}

// REMOVED: SniperOutcome enum (replaced by SniperExitReason)
```

### Config Block Updates

```json
"sniper": {
  "enabled": false,
  "paper_mode": true,
  "entry_threshold_with_velocity": 50,
  "entry_threshold_no_velocity": 40,
  "min_on_chain_score_for_entry": 30,
  "final_score_threshold": 40,
  "min_trades_for_velocity": 10,
  "skip_mayhem_mode": true,
  "max_vsol_entry": 20.0,
  "min_vsol_entry": 2.0,
  "conditional_vsol_entry_min": 15.0,
  "conditional_vsol_min_score": 60,
  "min_position_sol": 0.01,
  "max_position_sol": 0.10,
  "buy_slippage_pct": 15,
  "max_token_age_secs": 300,
  "jito_tip_lamports": 100000,
  "social_disable_before_age_secs": 120,
  "log_path": "data/sniper_trades.jsonl",
  "create_log_path": "data/sniper_create_log.jsonl",
  "smart_wallets_path": "data/smart_wallets.json",
  "dumper_wallets_path": "data/dumper_wallets.json",
  "social_cache_ttl_secs": 300,
  "dev_cache_ttl_secs": 3600,
  "pumpfun_api_rps": 5,
  "telegram_bot_token": null,

  "exit": {
    "hard_stop_bp": 3000,
    "max_hold_sec": 120,
    "buy_gap_timeout_sec": 15,
    "sell_cascade_count": 3,
    "sell_cascade_window": 5,
    "tp1_threshold_bp": 2000,
    "tp1_sell_permille": 300,
    "tp2_threshold_bp": 5000,
    "tp2_sell_permille": 300,
    "tp3_threshold_bp": 10000,
    "tp3_sell_permille": 300,
    "trail_initial_bp": 1500,
    "trail_tp2_bp": 1000,
    "trail_tp3_bp": 800,
    "exit_on_creator_sell": true,
    "exit_on_graduation": true,
    "sell_slippage_bp": 1500,
    "exit_priority_fee": 50000,
    "exit_max_retries": 3
  }
}
```

**Fields removed from config:**
- `target_profit_mult` (no atomic exit target)
- `sell_slippage_pct` (replaced by `exit.sell_slippage_bp`)
- `kelly_fraction` (not applicable)

### Decision Flowchart Update

Replace the bundle construction block in the main decision flowchart:

```
... final_score >= 40 ↓

Position sizing:
  trade_count < 50?  → 0.02 SOL flat (bootstrap)
  else → SniperSizer::compute_position_size(
           final_score, curve_zone, real_sol, wallet_sol, &tracker)
  
Entry execution:
  TX1: buy position_sol on bonding curve
  Bundle = [TX1, tip_tx]           // NO TX2
  Submit to Jito via gRPC pipeline
  
On bundle land:
  Open SniperPosition
  Subscribe to BondingCurveState updates for this mint
  Start exit monitoring (trade events + 500ms ticks)
  
Exit monitoring loop:
  E1: creator sell?        → FULL EXIT immediately
  E2: graduation?          → FULL EXIT on PumpSwap
  SL: pnl ≤ -30%?         → FULL EXIT
  Trail: below trail stop? → FULL EXIT  
  Time: hold ≥ 120s?       → FULL EXIT
  Gap: no buys ≥ 15s?      → FULL EXIT
  Cascade: 3+ sells/5?     → FULL EXIT
  TP1: pnl ≥ +20%?        → PARTIAL 30%, trail ON
  TP2: pnl ≥ +50%?        → PARTIAL 30%, trail tighten
  TP3: pnl ≥ +100%?       → PARTIAL 30%, trail tighten
  else                     → HOLD

On position close:
  Log SniperTradeLog → data/sniper_trades.jsonl
  Update WinRateTracker (win/loss/zone)
  Token age > 300s with no entry → drop tracking
```

---

## Appendix A: Calibration Parameters Requiring Empirical Tuning

All exit parameters are config-driven and hot-reloadable. The following need empirical calibration:

| Parameter | Default | Calibrate At | Method |
|---|---|---|---|
| `hard_stop_bp` | 3000 (-30%) | 200 trades | Distribution of max drawdowns on winners vs losers. If >10% of winners touch -25%, widen. |
| `max_hold_sec` | 120 | 200 trades | Distribution of time-to-TP1 on winners. If >80% of TP1 hits happen in <60s, tighten. |
| `buy_gap_timeout_sec` | 15 | 100 trades | Distribution of inter-buy gaps on active tokens. If normal gaps reach 12-15s, widen to 20. |
| `tp1_threshold_bp` | 2000 (+20%) | 500 trades | What % of entries reach +20%? If <30%, lower to +15%. If >70%, raise to +25%. |
| `tp1_sell_permille` | 300 (30%) | 500 trades | Optimize partial exit fraction for max EV. Sweep 200-400 in simulation. |
| `trail_initial_bp` | 1500 (15%) | 500 trades | Peak-to-exit analysis on TP1-hit trades. Too many stops at +15%? Widen. |
| `sell_cascade_count` | 3 | 200 trades | False positive rate (cascade fires but price recovers). |

**Calibration priority order:** Average loss size > TP1 threshold > trailing stop width > hard stop > time stops.

## Appendix B: Comparison with Existing RIDE Engine Exit Architecture

The sniper exit engine is intentionally **simpler** than the RIDE engine's DYNAMIC_EXIT_FRAMEWORK_V2:

| Feature | RIDE Engine | Sniper Engine | Why |
|---|---|---|---|
| Bayesian f̂* tracking | Yes (192 bytes) | No | Position holds <120s, not enough data for Bayesian update |
| Momentum divergence | Yes (16-byte ring) | Simplified (8-bit ring) | Shorter hold = less data = simpler detection |
| Volatility estimator | Yes (16 bytes) | No | Hold time too short for vol regime changes |
| Partial exits | Urgency-driven | TP-threshold-driven | Simpler, more predictable, easier to calibrate |
| Trailing stop | Volatility-adaptive | Fixed percentage (3 tiers) | Simplicity. Adaptive trail needs calibration data we don't have yet. |
| Emergency exits | Creator sell, whale exit | Creator sell, graduation | Bonding curve doesn't have whales the same way; graduation is BC-specific |

**v2 upgrade path:** After 1000+ trades, if the data supports it, migrate to urgency-based exits (import MomentumDivergence and VolatilityEstimator from RIDE engine). The SniperPosition struct has room for expansion.

---

*Spec v2.0 | 2026-04-04 | Opus 4.6 Quant Architect*  
*Architecture decision: Atomic bundle TX2 removed. Hold-and-ride with tiered TP + trailing stop adopted.*  
*Key finding: Atomic buy+sell is not viable on bonding curves (no orderbook, deterministic pricing).*  
*Break-even WR: 48.6% (flat model) → 45.5% (with tail captures). Achievable at scoring system's target quality.*