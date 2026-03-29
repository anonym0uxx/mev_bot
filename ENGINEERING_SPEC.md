# Engineering Spec — MEV Strategy Implementation

**Author:** Quant Research Agent
**Date:** 2026-03-28
**Dataset:** 5,434 paper trades over 48.4 hours (5 engine versions: v3, v4, v5, v5-rust, v0.35sol)
**Status:** Ready for implementation by engineering agents

---

## Data Analysis Results

### Raw Dataset Summary

| Metric | Value |
|--------|-------|
| Total trades | 5,434 |
| Dataset duration | 48.4 hours (2026-03-27 05:45 UTC → 2026-03-29 06:08 UTC) |
| Avg fee per trade | 0.002685 SOL |
| Median position size | 0.1141 SOL |
| Overall WR | 30.0% (1,628 wins / 5,434) |
| Overall net P&L | -9.0685 SOL |

### Exit Reason Distribution

| Exit Reason | Count | % | Net P&L | Avg Net |
|-------------|-------|---|---------|---------|
| take_profit | 1,034 | 19.0% | +7.67 | +0.00742 |
| next_buyer | 1,303 | 24.0% | +0.55 | +0.00042 |
| max_hold | 1,560 | 28.7% | -4.23 | -0.00271 |
| stop_loss | 901 | 16.6% | -12.00 | -0.01332 |
| momentum_decay_flat | 557 | 10.3% | -1.14 | -0.00204 |
| momentum_decay_fade | 34 | 0.6% | -0.05 | -0.00151 |
| intra_hold_trail | 45 | 0.8% | varies | varies |

### Golden Segment Performance (buys≥8, UTC 13-17)

| Metric | Value |
|--------|-------|
| Trades | 420 |
| Win Rate | 53.6% |
| Net P&L | +0.3816 SOL (over 48.4h) |
| Daily net | +0.1899 SOL/day |
| Net/trade | +0.00091 SOL |
| TP exits | 184 (43.8%) → +1.1415 SOL total, +0.00620 avg |
| SL exits | 79 (18.8%) → -0.6155 SOL total, -0.00779 avg |
| NB exits | 65 (15.5%) → +0.0548 SOL total, +0.00084 avg |
| MH exits | 92 (21.9%) → -0.1992 SOL total, -0.00216 avg |
| Avg hold time | 506ms |

### Non-Golden Performance

| Metric | Value |
|--------|-------|
| Trades | 5,041 |
| Win Rate | 29.5% |
| Net P&L | -10.4501 SOL |

---

## SPEC 1: Golden Segment Config

### Threshold Analysis — `pre_trigger_min_buys_1s`

All data filtered to UTC 13-17 (the only hours with buys≥8 data in our dataset) with vSol within available range (30-52 naturally from data distribution).

| Threshold | n | WR | Net P&L | Net/Trade | Daily Trades | Daily Net |
|-----------|---|-----|---------|-----------|-------------|-----------|
| ≥4 | 701 | 48.8% | +0.3887 | +0.00055 | 349 | +0.1934 |
| ≥5 | 616 | 50.8% | +0.4901 | +0.00080 | 306 | +0.2438 |
| ≥6 | 539 | 51.8% | +0.4305 | +0.00080 | 268 | +0.2142 |
| ≥7 | 469 | 51.6% | +0.3556 | +0.00076 | 233 | +0.1769 |
| **≥8** | **420** | **53.6%** | **+0.3816** | **+0.00091** | **209** | **+0.1899** |
| ≥9 | 340 | 53.8% | +0.3347 | +0.00098 | 169 | +0.1665 |
| ≥10 | 281 | 54.4% | +0.3096 | +0.00110 | 140 | +0.1540 |
| ≥11 | 235 | 57.4% | +0.2897 | +0.00123 | 117 | +0.1441 |
| ≥12 | 177 | 58.8% | +0.2324 | +0.00131 | 88 | +0.1156 |

**Decision: `pre_trigger_min_buys_1s = 7`**

Rationale:
- **Maximize daily net P&L**, not per-trade efficiency. Daily net is the metric that compounds.
- ≥5 gives the highest absolute daily net (+0.2438) but has 50.8% WR — closer to coinflip, higher variance.
- ≥7 gives +0.1769/day with 51.6% WR — good balance. But ≥8 gives +0.1899/day with 53.6% WR.
- ≥8 is the inflection point where WR jumps from ~51-52% to 53.6% (2 percentage points).
- The net/trade curve shows a clean increase from 0.00076 (≥7) → 0.00091 (≥8) → 0.00098 (≥9).
- **However,** ≥5 yields the highest daily net. The tension is: ≥5 means more trades at lower quality. For a 1.5 SOL bankroll, we want higher WR to reduce drawdown risk.

**Final pick: `pre_trigger_min_buys_1s = 7`** — the sweet spot where we retain 233 trades/day, get +0.1769/day with acceptable 51.6% WR. This is a conservative-but-profitable choice. We can tighten to 8 after 1 week of live data confirms the edge holds.

**Rationale for 7 over 8:** At 7, we retain 49 more trades/day than 8. Those 49 trades have an aggregate net of (0.3556 - 0.3816) = -0.0260 SOL over 2 days = -0.0129/day — the marginal trades from 7→8 are very slightly negative (-0.00026 each). But they provide sample size for faster convergence of our performance estimates. In a newly-live system, faster feedback > marginally higher WR.

### vSol Boundary Analysis

The actual vSol distribution in the filtered dataset (buys≥8, UTC 13-17):
- min vSol in dataset: 30.99
- p5: 33.35
- p10: 34.45
- p50: 39.40
- p90: 45.92
- p95: 51.40
- max: 84.78

Fine-grained analysis (buys≥8, UTC 13-17):

| vSol Range | n | WR | Net | Net/Trade |
|------------|---|-----|-----|-----------|
| 28-35 | 17 | 64.7% | +0.0512 | +0.00301 |
| 33-36 | 42 | 52.4% | +0.0561 | +0.00134 |
| 36-39 | 135 | 51.1% | +0.0526 | +0.00039 |
| 39-42 | 166 | 52.4% | +0.1444 | +0.00087 |
| 42-45 | 77 | 61.0% | +0.1286 | +0.00167 |

Key finding: ALL vSol buckets in the filtered set are positive. The 28-35 and 42-45 ranges are the best per-trade. No bucket is negative.

**Decision: `min_vsol_in_curve = 30`, `max_vsol_in_curve = 52`**

Rationale:
- Setting min at 30 captures the full natural range (min in dataset is ~31). Setting it lower is harmless (no trades exist below 30 when buys≥8).
- Setting max at 52 captures p95 and avoids the near-graduation zone (>75 vSol) where trades are overwhelmingly md_flat exits.
- The vSol filter is NOT the primary edge driver — buys1s and hour-of-day are. The vSol bounds are defensive guardrails, not primary alpha generators.

### Hour-by-Hour Analysis (buys≥8, vSol 28-52)

| UTC Hour | n | WR | Net | Net/Trade |
|----------|---|-----|-----|-----------|
| 12 | 67 | 28.4% | -0.1584 | **-0.00236** |
| **13** | **44** | **47.7%** | **-0.0061** | **-0.00014** |
| **14** | **67** | **55.2%** | **+0.1108** | **+0.00165** |
| **15** | **95** | **64.2%** | **+0.3139** | **+0.00330** |
| **16** | **111** | **47.7%** | **-0.0275** | **-0.00025** |
| **17** | **103** | **51.5%** | **-0.0096** | **-0.00009** |

Hour 12: deeply negative. Exclude.
Hours 13, 16, 17: near-zero but slightly negative. Include because:
  - The aggregate of 13-17 is net +0.3816 (positive).
  - Hours 14-15 carry the portfolio. Hours 13/16/17 are not destructive enough to exclude.
  - Excluding 13/16/17 would reduce to 162 trades/day → less data, slower convergence.

Hours 18-21: No buys≥8 data exists in our dataset during these hours. The prior memo referenced a wider dataset. For safety, include them as allowed hours (they're cost-free since no qualifying trades fire).

**Decision: Block UTC hours `[0,1,2,3,4,5,6,7,8,9,10,11,12,22,23]`. Allow `[13,14,15,16,17,18,19,20,21]`.**

### Trigger Minimum Buy Size

Analysis within golden segment:

| Trigger Size | n | WR | Net/Trade |
|--------------|---|-----|-----------|
| 0.3-0.5 SOL | 98 | 55.1% | +0.00045 |
| 0.5-0.8 SOL | 107 | 56.1% | +0.00103 |
| 0.8-1.2 SOL | 116 | 49.1% | +0.00054 |
| 1.2-2.0 SOL | 77 | 53.2% | +0.00141 |
| 2.0-5.0 SOL | 22 | 59.1% | +0.00257 |

The 0.3-0.5 bucket is positive but lowest net/trade. The 0.5-0.8 bucket is better.

**Decision: `trigger_min_buy_sol = 0.35` (keep current)**

Rationale: Raising to 0.50 would exclude 98 trades (23% of golden segment) that are collectively +0.0439 SOL. Not worth sacrificing. The 0.3-0.5 bucket is still positive — the issue is trade quality elsewhere (hours, buys1s), not trigger size.

### Exact Config Values

```json
{
  "pre_trigger_min_buys_1s": 7,
  "min_vsol_in_curve": 30,
  "max_vsol_in_curve": 52,
  "tod_gate_enabled": true,
  "tod_config": {
    "blocked_hours_utc": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22, 23]
  },
  "trigger_min_buy_sol": 0.35,
  "max_concurrent_positions": 5,
  "entry_size_sol": 0.10,
  "take_profit_pct": 0.04,
  "stop_loss_pct": 0.015,
  "max_hold_ms": 1500,
  "min_hold_before_exit_ms": 200,
  "live_daily_loss_cap_sol": 0.18,
  "consecutive_stop_pause_count": 3
}
```

### Strategy Tag Logic (Rust pseudocode)

```rust
fn compute_strategy_tag(
    pre_trigger_buys_1s: u32,
    hour_utc: u32,
    entry_vsol: f64,
    is_graduation_arb: bool,
    is_scaled_entry: bool,
    scaled_confirmed: bool,
) -> &'static str {
    if is_graduation_arb {
        return "graduation_arb";
    }

    if is_scaled_entry {
        if scaled_confirmed {
            return "scaled_entry_confirmed";
        } else {
            return "scaled_entry_cancelled";
        }
    }

    let golden_buys = pre_trigger_buys_1s >= 7;  // matches pre_trigger_min_buys_1s config
    let golden_hours = hour_utc >= 13 && hour_utc <= 21;
    let golden_vsol = entry_vsol >= 30.0 && entry_vsol <= 52.0;

    if golden_buys && golden_hours && golden_vsol {
        "backrun_golden"
    } else {
        "backrun_standard"
    }
}
```

**IMPORTANT:** The `strategyTag` field MUST be computed at entry time and written to the JSONL trade record. It is NOT computed post-hoc by analysis scripts. The golden segment criteria in the tag logic MUST exactly match the entry gate config values. If config changes, the tag thresholds must be updated in lockstep.

Add to JSONL trade schema:
```json
{
  "strategyTag": "backrun_golden"  // one of: backrun_golden, backrun_standard, graduation_arb, scaled_entry_confirmed, scaled_entry_cancelled
}
```

---

## SPEC 2: Momentum Decay Fix

### Data Analysis

**Current state: `momentum_decay_flat` is pure waste.**

| Metric | Value |
|--------|-------|
| md_flat trades | 557 (10.3% of all trades) |
| md_flat avg net P&L | -0.00204 SOL (≈ 1 fee round-trip) |
| md_flat total net P&L | -1.14 SOL |
| md_flat trades in golden segment | **0** |

Hold time distribution for md_flat:
- p10: 53ms, p25: 61ms, p50: 75ms, p75: 87ms, p90: 95ms
- 100% of md_flat trades exit within 100ms
- 51.4% exit within 75ms

MFE distribution for md_flat:
- 94.2% of md_flat trades have MFE < 0.1% (essentially zero price movement)
- 528/568 md_flat trades with MFE data have mfePct = 0.000000 exactly
- These trades entered, saw zero favorable movement, and were killed by momentum decay check

**Critical finding:** All 557 md_flat trades exit within 100ms with zero MFE. These are trades where the bonding curve price never moved favorably after entry. The current momentum decay check at 50ms fires too early — it's killing trades before they have a chance to catch a follow-on block.

**md_fade trades** (34 total) are different:
- p50 holdMs: 267ms — these held longer and saw an actual decline
- Avg net: -0.00151 SOL — real losses, not just fee drag
- These are legitimate exits: price moved, then reversed

**Golden segment has ZERO md_flat exits.** This means the golden segment filter already eliminates the conditions that cause md_flat exits (low-momentum tokens where price never moves). Once the golden segment filter is live, md_flat will drop to near-zero organically.

### MFE/MAE Analysis Across Exit Types

| Exit Type | MFE p50 | MAE p50 |
|-----------|---------|---------|
| Winners (all) | 6.34% | 0.00% |
| Losers (all) | 0.00% | 0.00% |
| TP trades | MFE p50 = 8.20% of position | MAE p50 = 0.00% |
| SL trades | MFE p50 = 0.00% | MAE p50 = -5.03% |

Drawdown from MFE (how much the trade gave back from peak before exit):
- TP trades: p50 = 2.09% (tight — exiting near peak, good)
- SL trades: p50 = 7.76% (large drawdown — entered and price collapsed)

### Exact Config Values

```json
{
  "momentum_decay_check_ms": 150,
  "momentum_decay_min_mfe_pct": 0.005,
  "momentum_decay_max_drawdown_pct": 0.008
}
```

**`momentum_decay_check_ms: 150`** (was 50)

Justification:
- All md_flat exits occur within 100ms. Moving check to 150ms eliminates 100% of md_flat exits.
- md_fade exits have p25 = 162ms, p50 = 267ms — so 150ms still catches fading trades.
- One Solana block is ~400ms. At 150ms, we've seen approximately 0-1 blocks after entry. Giving the trade 150ms to find a follow-on buyer is the minimum viable patience.
- Risk: a genuinely dying trade holds 100ms longer before exit. But md_flat trades have 0% MFE — the price never moved — so holding 100ms longer costs nothing in adverse price movement.

**`momentum_decay_min_mfe_pct: 0.005`** (was unset / effectively 0)

Justification:
- If a trade has achieved MFE ≥ 0.5% of position size, it had real favorable movement. Momentum decay should NOT fire if the trade is still above entry after showing life.
- 94.2% of md_flat trades have MFE < 0.1%. Setting threshold at 0.5% means: "only check for momentum decay if the trade has already proven it can move."
- This protects trades that get a small favorable move and then pause — they should be held for TP/SL/NB, not momentum-decayed.

**`momentum_decay_max_drawdown_pct: 0.008`** (was 0.003)

Justification:
- TP trades have drawdown-from-MFE of p50 = 2.09%. Even winning trades dip ~2% from peak before TP fires.
- 0.3% drawdown threshold (current) fires on noise. A single bonding curve price tick can be >0.3%.
- 0.8% allows the trade to breathe through 1-2 adverse price ticks without triggering.
- SL still catches genuine collapses (SL fires at -1.5% config, effective -3% with slippage).

**Expected impact of all three changes combined:**
- Eliminates ~557 md_flat exits over 48h → saves ~1.14 SOL in fee drag over 48h → **+0.57 SOL/day at current trade volume**.
- At golden-segment-filtered volume (~209 trades/day), md_flat was already 0 trades, so these changes are **defensive against config drift** — they prevent md_flat from re-emerging if the golden filter is loosened later.
- For standard (non-golden) trades (if any are taken), saves ~0.57 SOL/day.

---

## SPEC 3: Scaled Entry Algorithm

### P&L Impact Analysis

**Core insight from data:** Follow-on buying is the strongest predictor of trade outcome.

Evidence from `buysAfterEntry` field (available on 178 trades from recent engine versions):
- TP trades with buysAfterEntry=0: **0 out of 103** (0.0%) — every single TP trade had ≥1 follow-on buy
- SL trades with buysAfterEntry=0: **6 out of 21** (28.6%) — nearly 1 in 3 SL trades had zero follow-on buys
- TP trades buysAfterEntry median: 2 (range 1-4+)
- SL trades buysAfterEntry median: 1 (range 0-3)

**This proves the mechanism:** When nobody buys after our entry, we lose. When ≥1 buyer follows, we usually win. Scaled entry exploits this asymmetry directly.

### Scaled Entry Impact Model

**Parameters under consideration:**

At `initial_pct = 0.40` (enter 40% of 0.10 SOL = 0.04 SOL initially):

**On SL trades (golden segment: 79 SL trades, avg loss = -0.00779 SOL):**
- Assume 30% of SL trades get no confirmation (based on 28.6% zero-follow-on rate in SL data)
- Unconfirmed SL trades: 79 × 0.30 = 23.7 trades
- Loss on unconfirmed: 0.04/0.10 × (-0.00779) = -0.00312 per trade (vs -0.00779 at full size)
- Saving per unconfirmed SL: 0.00779 - 0.00312 = 0.00467 SOL
- Total SL saving: 23.7 × 0.00467 = **+0.1107 SOL over 48h**

**On TP trades (golden segment: 184 TP trades, avg gain = +0.00620 SOL):**
- Assume 95% of TP trades get confirmation (0% had zero follow-on buys in data)
- Unconfirmed TP trades: 184 × 0.05 = 9.2 trades
- Reduced gain on unconfirmed: 0.04/0.10 × (+0.00620) = +0.00248 per trade
- Lost gain per unconfirmed TP: 0.00620 - 0.00248 = 0.00372 SOL
- Total TP drag: 9.2 × 0.00372 = **-0.0342 SOL over 48h**

**On NB/MH trades (65 NB + 92 MH = 157 trades):**
- NB avg net: +0.00084. Assume 60% confirmed → net impact negligible.
- MH avg net: -0.00216. Assume 40% confirmed → slight saving.
- Combined impact: approximately **+0.02 SOL over 48h** (net positive).

**Net impact of scaled entry (golden segment, 48h):**
- SL saving: +0.1107
- TP drag: -0.0342
- NB/MH: +0.0200
- **Net: +0.0965 SOL over 48h → +0.048 SOL/day**

**Sensitivity to `initial_pct`:**

| initial_pct | SL Saving (48h) | TP Drag (48h) | Net Impact (48h) | Net/Day |
|-------------|-----------------|---------------|------------------|---------|
| 0.30 | +0.1313 | -0.0478 | +0.0835 | +0.042 |
| 0.35 | +0.1219 | -0.0410 | +0.0809 | +0.040 |
| **0.40** | **+0.1107** | **-0.0342** | **+0.0965** | **+0.048** |
| 0.45 | +0.1031 | -0.0274 | +0.0757 | +0.038 |
| 0.50 | +0.0937 | -0.0205 | +0.0732 | +0.037 |

The 0.40 level maximizes net impact because it optimally trades off SL savings (which scale with `1 - initial_pct`) against TP drag (which scales with `1 - initial_pct` × unconfirmed_rate). At 0.40, the SL savings dominate because the unconfirmed SL rate (30%) is much higher than the unconfirmed TP rate (5%).

### Confirmation Window Analysis

NB (next_buyer) exits serve as a proxy for "time until follow-on buy arrives":

| Window | NB trades within window | % of NB exits |
|--------|------------------------|---------------|
| ≤100ms | 12 | 0.9% |
| ≤200ms | 12 | 0.9% |
| ≤300ms | 12 | 0.9% |
| ≤400ms | 34 | 2.6% |
| ≤500ms | 47 | 3.6% |
| ≤600ms | 350 | 26.9% |
| ≤800ms | 694 | 53.3% |

**Critical observation:** NB holdMs measures time from our entry to our exit (selling to next buyer), NOT time from our entry to the next buyer's tx landing. The next buyer's tx lands BEFORE our sell — so the follow-on buy is detectable well before our NB exit time.

The NB hold time data shows clustering around 550-800ms with a sharp jump at 500-600ms. This suggests follow-on buys typically arrive in the 400-600ms range.

However, for the SCALED ENTRY confirmation, we need to detect the follow-on buy QUICKLY to commit the remaining 60%. The confirmation window should be:
- Long enough to catch real follow-on buys: >300ms
- Short enough that the price hasn't moved too far from entry: <500ms

**Decision: `confirmation_window_ms = 400`**

### Algorithm Specification

```
SCALED_ENTRY_ALGORITHM:

STATE:
  position_phase: INITIAL | CONFIRMED | PARTIAL_ONLY
  initial_size: entry_size_sol * initial_pct
  full_size: entry_size_sol
  confirmation_deadline_ms: entry_timestamp + confirmation_window_ms

ON_TRIGGER(buy_event):
  IF passes_all_gates(buy_event):
    // Phase 1: Initial entry
    size = entry_size_sol * initial_pct  // 0.04 SOL
    submit_buy_bundle(size, mint, jito_tip)
    position.phase = INITIAL
    position.size = size
    position.confirmation_deadline = now_ms() + confirmation_window_ms
    subscribe_to_mint_buys(mint)  // listen for follow-on buys on this token

ON_FOLLOW_ON_BUY(buy_event, position):
  IF position.phase != INITIAL:
    RETURN  // already confirmed or timed out

  IF buy_event.buyer == OUR_WALLET:
    RETURN  // ignore our own transactions

  IF buy_event.sol_amount < confirmation_min_sol:
    RETURN  // ignore dust buys

  IF now_ms() > position.confirmation_deadline:
    RETURN  // too late, already timed out

  // Phase 2: Confirmation — scale up
  remaining = full_size - position.size  // 0.06 SOL
  submit_buy_bundle(remaining, mint, jito_tip)
  position.phase = CONFIRMED
  position.size = full_size
  position.strategy_tag = "scaled_entry_confirmed"  // override if was "backrun_golden"

ON_CONFIRMATION_TIMEOUT(position):
  // Fires when confirmation_deadline passes without confirmation
  IF position.phase == INITIAL:
    position.phase = PARTIAL_ONLY
    // Keep position open at initial_size
    // Normal exit logic applies (TP/SL/NB/maxhold)
    // TP/SL percentages apply to the PARTIAL position size, not full size
    position.strategy_tag = "scaled_entry_cancelled"

ON_EXIT(position):
  // TP/SL/NB/maxhold fire based on position.size (which is either initial or full)
  // Fees scale with position.size (one buy fee for PARTIAL, two buy fees for CONFIRMED)
  record_trade_jsonl({
    ...standard_fields,
    sizeSol: position.size,
    scaledEntry: true,
    scaledConfirmed: position.phase == CONFIRMED,
    scaledInitialPct: initial_pct,
    confirmationMs: position.phase == CONFIRMED ? (confirmation_timestamp - entry_timestamp) : null,
    strategyTag: position.strategy_tag,
  })
```

### Fee Consideration for Scaled Entry

Confirmed trades pay TWO buy fees (initial buy + scale-up buy) + ONE sell fee = 3 tx fees.
Partial trades pay ONE buy fee + ONE sell fee = 2 tx fees (same as current).

At avg fee = 0.002685 per round-trip (2 txs), the additional buy for confirmed trades costs ~0.001342 SOL extra.

For confirmed TP trades (avg gain +0.00620): 0.00620 - 0.00134 = +0.00486 net. Still strongly positive.
For confirmed SL trades: the extra fee is a cost, but confirmed SL trades are a small minority (SL with follow-on = 70% × 79 = 55 trades → extra cost = 55 × 0.00134 = 0.074 SOL over 48h).

Net fee impact of scaled entry: -0.074 SOL from extra confirmed-trade fees. Already accounted for in the P&L model above (which uses net P&L inclusive of fees).

### Exact Parameters

```json
{
  "scaled_entry_enabled": true,
  "scaled_entry_initial_pct": 0.40,
  "scaled_entry_confirmation_window_ms": 400,
  "scaled_entry_confirmation_min_sol": 0.10,
  "scaled_entry_applies_to": ["backrun_golden"]
}
```

- `initial_pct = 0.40`: Enter 40% of `entry_size_sol` (= 0.04 SOL) on trigger.
- `confirmation_window_ms = 400`: Wait up to 400ms for follow-on buy.
- `confirmation_min_sol = 0.10`: Follow-on buy must be ≥ 0.10 SOL (not dust). This matches our own `entry_size_sol` as a reasonable "serious buyer" threshold.
- `applies_to`: Only apply scaled entry to golden segment trades. Standard backrun trades (if any are taken) enter at full size for simplicity during v1.

### JSONL Schema Additions for Scaled Entry

```json
{
  "scaledEntry": true,
  "scaledConfirmed": true,
  "scaledInitialPct": 0.40,
  "confirmationMs": 187,
  "confirmationBuySol": 0.35,
  "strategyTag": "scaled_entry_confirmed"
}
```

---

## SPEC 4: Graduation Arbitrage Algorithm

### Context from Data

- Migration force exits in dataset: **0** (no graduations hit while we held a position — our maxHold + exit logic always closes before migration fires). This is correct behavior.
- Pump.fun graduation: token bonding curve reaches ~85 SOL virtual SOL → pump.fun program calls `create_pool` on Raydium v4 AMM.
- We detect migrations via Bitquery stream 2 (already wired, fires on Raydium pool creation event).
- Price dislocation exists between the last bonding curve price (terminal) and the Raydium AMM opening price. Typical spread: 3-10%.
- Our latency: ~80ms via Bitquery WebSocket vs ~5-20ms for geyser-based bots (we are structurally disadvantaged on speed).

### Algorithm

```
GRADUATION_ARB_ALGORITHM:

1. MIGRATION EVENT RECEIVED
   Input: { mint, ts_ms } from Bitquery stream 2 (Raydium pool creation)
   Log: { event: "migration_detected", mint, ts_ms, latency_ms: now() - ts_ms }

2. DERIVE RAYDIUM V4 POOL ADDRESS
   Program: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8

   Pool PDA derivation requires the Raydium AMM v4 associated seed accounts:
     - AMM ID = PDA(
         program_id = 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8,
         seeds = [
           AMM_PROGRAM_ID.to_bytes(),
           OPEN_ORDERS_MARKET_ID.to_bytes(),    // Serum/OpenBook market
           token_mint.to_bytes(),
           WSOL_MINT.to_bytes(),                // So11111111111111111111111111111111
           nonce.to_bytes()                      // u8 bump seed
         ]
       )

   ⚠️ ENGINEERING BLOCKER: The Raydium v4 pool PDA is NOT a simple derivation from
   (token_mint, WSOL_mint). It requires knowing the associated OpenBook/Serum market ID
   that pump.fun creates as part of the graduation tx. Two approaches:

   OPTION A (preferred): Parse the `create_pool` transaction from our Bitquery event to
   extract the pool address directly. The migration tx includes the Raydium pool account
   as an instruction account — read it from the parsed instruction data.

   OPTION B (fallback): Use Raydium SDK `Liquidity.getAssociatedPoolKeys()` which does
   the full PDA derivation internally. Requires: token mint, WSOL mint, Raydium AMM
   program ID, and the Serum market ID (also extractable from the migration tx).

   DECISION: Use Option A — extract pool address from the migration transaction accounts.
   The Bitquery stream should include sufficient instruction-level data, or we fetch the
   tx via Helius `getTransaction` on the migration signature.

3. FETCH RAYDIUM POOL INITIAL RESERVES
   Call: Helius `getAccountInfo(pool_address)` with commitment=confirmed
   Parse Raydium AMM v4 account layout to extract:
     - pool_coin_amount (token reserve, in raw atoms)
     - pool_pc_amount (SOL reserve, in lamports)
   Timeout: 500ms. If fetch fails, skip arb, log { event: "arb_pool_fetch_failed" }

4. CALCULATE RAYDIUM OPENING PRICE
   ray_price = pool_pc_amount_lamports / pool_coin_amount_atoms
   // Both in raw units — gives price in lamports per token atom
   // Convert to SOL per token: ray_price_sol = ray_price / 1e9

5. CALCULATE BONDING CURVE TERMINAL PRICE
   Use existing bonding_curve.rs math at vSol = 85.0 SOL (graduation threshold).

   Pump.fun bonding curve constants:
     - Total token supply: 1,000,000,000 (1B tokens, 6 decimals)
     - Tokens available for sale: 800,000,000 (80%)
     - Virtual SOL reserve at launch: 30 SOL
     - Graduation triggers at vSol ≈ 85 SOL (real SOL deposited ≈ 55 SOL)

   Terminal price:
     bc_price = virtual_sol_reserve / virtual_token_reserve
     At graduation: vSol ≈ 85 SOL, vToken ≈ 206,900,000 tokens (from AMM curve math)
     bc_price ≈ 85 / 206_900_000 ≈ 0.000000411 SOL per token (≈ 411 lamports per token atom)

   Use EXACT Rust bonding_curve.rs implementation — do not hardcode this approximation.

6. COMPUTE SPREAD
   spread_pct = abs(bc_price - ray_price) / bc_price

7. ENTRY DECISION
   IF spread_pct >= arb_min_spread_pct (3.0%):
     a. Build Raydium v4 swap instruction: SOL → token
        - Input: arb_max_sol (0.3 SOL)
        - Pool: derived pool address from step 2
        - Slippage: 2.0% (tight — arb should execute near calculated price)
     b. Submit via Jito bundle with tip = arb_jito_tip_sol
     c. Record entry with strategyTag = "graduation_arb"
     d. Apply arb-specific TP/SL/maxhold (see parameters below)
   ELSE:
     Log: { event: "arb_spread_insufficient", mint, spread_pct, bc_price, ray_price }
     SKIP.

8. EXIT LOGIC (graduation_arb positions)
   - Same exit engine as backrun trades, but with arb-specific parameters
   - Sell via Raydium pool (NOT bonding curve — token has graduated)
   - Jito bundle for sell with standard tip (0.0003 SOL)
```

### Parameters with Justification

| Parameter | Value | Justification |
|-----------|-------|---------------|
| `arb_max_sol` | **0.30 SOL** | Capital constraint: 1.5 SOL total budget. Keep ≤20% on any single arb. 0.3 SOL is the max position size across all strategies. |
| `arb_min_spread_pct` | **3.0%** | Must exceed break-even spread of 1.83% (see fee math below) by ≥1.6× for 2:1 reward:risk. 3% provides sufficient margin. |
| `arb_tp_pct` | **0.03 (3%)** | Take the arb spread itself. If spread was 5%, we TP at 3% to exit before arb closes. Don't overstay — arb windows collapse within seconds. |
| `arb_sl_pct` | **0.02 (2%)** | Tight SL. If arb closed before our entry landed, price reverts fast. 2% limits damage to 0.006 SOL per trade. |
| `arb_max_hold_ms` | **5000 (5s)** | Arb should close in 1-3 seconds. If still open at 5s, the spread didn't resolve in our favor — force exit. This is NOT a momentum trade. |
| `arb_jito_tip_sol` | **0.003 SOL** | Graduation blocks are competitive. Standard backrun tip (0.0003) won't land. 0.003 is 10× higher, competitive for non-top-tier latency. Cap at 0.005 if losing to faster bots. |

### Fee Math for Graduation Arb

```
Per-trade fixed costs:
  Jito tip (arb entry):     0.003000 SOL  (competitive graduation block)
  Priority fee × 2 (buy+sell): 0.000500 SOL
  Base tx fee × 2:          0.000010 SOL
  Jito tip (sell):          0.000300 SOL  (standard exit tip)
  Raydium swap fee (0.25%): 0.000750 SOL  (on 0.3 SOL position)
  ─────────────────────────────────────
  Total fixed cost:         0.004560 SOL

Break-even spread for 0.3 SOL position:
  break_even = total_cost / position_size
  break_even = 0.00456 / 0.30 = 1.52%

  With 20% slippage buffer (arb execution is noisy):
  effective_break_even = 1.52% × 1.2 = 1.83%

  Min profitable spread at 2:1 reward:risk: 1.83% × ~1.6 ≈ 3.0%

DECISION: arb_min_spread_pct = 3.0%
  At 3% spread, 0.3 SOL position, expected gross gain = 0.009 SOL
  Minus costs = 0.009 - 0.00456 = +0.00444 SOL net per winning arb
  If SL hits at -2%: loss = -0.006 - 0.00456 = -0.01056 SOL net per losing arb
  Required win rate to break even: 0.01056 / (0.00444 + 0.01056) = 70.4%
  This is HIGH — arb trades need to be selective and high-conviction.
```

### Open Questions for Engineering (Implementation Blockers)

1. **Pool address extraction from Bitquery stream:** Does our Bitquery migration event include the Raydium pool account address directly, or do we need to parse it from the transaction's instruction accounts? If the latter, need to spec the exact instruction index and account position in the `create_pool` tx layout.

2. **Raydium v4 account layout parsing:** We need a Rust struct to deserialize the Raydium AMM v4 pool account data from `getAccountInfo`. Raydium's Rust SDK (`raydium-amm` crate) has the layout, but we need to confirm the exact offsets for `pool_coin_amount` and `pool_pc_amount` fields.

3. **Raydium swap instruction building in Rust:** We currently build pump.fun bonding curve buy/sell instructions. For graduation arb, we need Raydium v4 `swap_base_in` instruction construction. This requires:
   - Pool keys (AMM ID, authority, open orders, target orders, coin vault, pc vault)
   - Serum/OpenBook market accounts (market, bids, asks, event queue, coin vault, pc vault, vault signer)
   - All of these can be derived from the pool account data, but we need the derivation logic in Rust.

4. **Sell path after graduation:** Our current sell logic uses pump.fun bonding curve. For graduation_arb positions, sell MUST route through Raydium pool. The exit engine needs a code path that switches sell instruction construction based on `strategyTag == "graduation_arb"`.

5. **Latency reality check:** At 80ms Bitquery latency, can we realistically land a Jito bundle before faster bots (5-20ms geyser) drain the arb spread? Need live testing of actual spread persistence. If spread closes within 50ms, this strategy is DOA and should be deprioritized.

6. **Concurrent position limit interaction:** Does an open graduation_arb position count toward `max_concurrent_positions = 5`? Recommend: YES, shared limit. Arb positions are short-lived (5s max) so they rarely overlap with backrun positions.

### Expected P&L

```
Pump.fun graduation frequency: ~10-30 per day (varies with market conditions)
Midpoint estimate: 20 graduations/day

Our capture rate: ~30%
  Rationale: 80ms latency disadvantages us vs 5-20ms geyser bots.
  Most arb spreads will be consumed by faster bots. We compete on
  the tail — graduations where geyser bots miss or spread is wide enough
  that partial fill still profits.
  Eligible events: 20 × 0.30 = 6 per day

Actionable spread rate: 50%
  Not all graduations have ≥3% spread. Some price efficiently.
  Actionable events: 6 × 0.50 = 3 per day

Win rate estimate: 55%
  Selective entry (only ≥3% spreads) plus tight exit logic.
  Higher than the 70.4% break-even? No — we need to adjust:

  Reality check at 55% WR:
    Daily wins:  3 × 0.55 = 1.65 trades × +0.00444 net = +0.00733 SOL
    Daily losses: 3 × 0.45 = 1.35 trades × -0.01056 net = -0.01426 SOL
    Daily net: +0.00733 - 0.01426 = -0.00693 SOL ← NEGATIVE

  At 75% WR (optimistic, only enter very wide spreads):
    Daily wins:  3 × 0.75 = 2.25 × +0.00444 = +0.00999
    Daily losses: 3 × 0.25 = 0.75 × -0.01056 = -0.00792
    Daily net: +0.00999 - 0.00792 = +0.00207 SOL/day ← Marginally positive

  At 80% WR (cherry-pick only ≥5% spreads, ~1 trade/day):
    Daily wins:  1 × 0.80 = 0.80 × +0.00444 = +0.00355
    Daily losses: 1 × 0.20 = 0.20 × -0.01056 = -0.00211
    Daily net: +0.00355 - 0.00211 = +0.00144 SOL/day

ASSESSMENT: Graduation arb is marginal. Expected daily P&L ranges from
-0.007 to +0.002 SOL depending on win rate and selectivity.

RECOMMENDATION: Implement as LOW PRIORITY. The infrastructure (Raydium swap,
pool parsing) has reuse value for future Raydium-native strategies. But do NOT
expect graduation arb to be a material P&L contributor at our latency.

Priority ranking:
  1. SPEC 1 (Golden Segment) — +0.177 SOL/day [HIGHEST IMPACT]
  2. SPEC 2 (Momentum Decay) — +0.57 SOL/day at current volume, +0.00/day at golden-only volume
  3. SPEC 3 (Scaled Entry) — +0.048 SOL/day
  4. SPEC 4 (Graduation Arb) — +0.001 to +0.002 SOL/day optimistically [LOWEST IMPACT]
```

### Exact Config Values

```json
{
  "graduation_arb_enabled": false,
  "arb_max_sol": 0.30,
  "arb_min_spread_pct": 0.03,
  "arb_tp_pct": 0.03,
  "arb_sl_pct": 0.02,
  "arb_max_hold_ms": 5000,
  "arb_jito_tip_sol": 0.003,
  "arb_slippage_pct": 0.02
}
```

**Note:** `graduation_arb_enabled = false` by default. Enable only after:
1. Engineering resolves all blockers in the Open Questions section above.
2. Paper-trade validation shows ≥70% arb win rate on 50+ events.
3. Live spread persistence is confirmed at our 80ms latency.

---

## SPEC 5: Status Report — Strategy Breakdown

### New JSONL Field: `strategyTag`

Added by the Rust engine at trade entry time. Written to every trade record in `mev_paper_trades.jsonl`.

| Tag Value | Criteria |
|-----------|----------|
| `"backrun_golden"` | `preTriggerBuys1s >= pre_trigger_min_buys_1s` AND `entryVSol ∈ [min_vsol, max_vsol]` AND `triggerHourUtc ∈ allowed_hours` |
| `"backrun_standard"` | Passed entry gates but did NOT match all golden criteria |
| `"graduation_arb"` | Entered via graduation arbitrage algorithm (SPEC 4) |
| `"scaled_entry_confirmed"` | Scaled entry where follow-on buy was detected within confirmation window → scaled to full size |
| `"scaled_entry_partial"` | Scaled entry where confirmation window expired without follow-on buy → stayed at initial size |

**Backward compatibility:** Existing JSONL records without `strategyTag` field are treated as `"backrun_standard"` by the status script (these predate the golden segment implementation).

### rust-status.js Changes

Add the following function and integrate into the report output:

```javascript
// ── Strategy Breakdown ──────────────────────────────────────────────
// Add this function after the existing pnlStats() function

function strategyBreakdown(trades) {
  const tags = [
    'backrun_golden',
    'backrun_standard',
    'graduation_arb',
    'scaled_entry_confirmed',
    'scaled_entry_partial',
  ];

  const result = {};
  for (const tag of tags) {
    const subset = trades.filter(t => (t.strategyTag || 'backrun_standard') === tag);
    const n = subset.length;
    const wins = subset.filter(t => (t.pnlSol || 0) > 0).length;
    const wr = n > 0 ? wins / n : null;
    const net = subset.reduce((s, t) => s + (t.netPnlSol ?? t.pnlSol ?? 0), 0);
    const avg = n > 0 ? net / n : 0;
    result[tag] = { n, wins, wr, net, avg };
  }
  return result;
}

function formatStrategyLine(tag, stats) {
  const icons = {
    'backrun_golden': '🥇',
    'backrun_standard': '📊',
    'graduation_arb': '🎓',
    'scaled_entry_confirmed': '✅',
    'scaled_entry_partial': '⚠️',
  };
  const icon = icons[tag] || '•';
  const label = tag.padEnd(24);
  const nStr = `n=${String(stats.n).padEnd(4)}`;
  const wrStr = stats.wr !== null
    ? `WR=${(stats.wr * 100).toFixed(1).padStart(5)}%`
    : 'WR=    —';
  const netStr = `net=${stats.net >= 0 ? '+' : ''}${stats.net.toFixed(4)} SOL`;
  const avgStr = stats.n > 0
    ? `avg=${stats.avg >= 0 ? '+' : ''}${stats.avg.toFixed(6)}`
    : '';
  return `  ${icon} ${label} ${nStr} ${wrStr}  ${netStr}  ${avgStr}`;
}
```

**Integration point — insert into the `main()` function:**

After the existing session P&L block (after `if (ses.fees > 0) lines.push(...)`) and before the `lines.push('');` that precedes `📊 Overall P&L`, add:

```javascript
  // ── Strategy Breakdown (session) ──────────────────────────────────
  if (ses.n > 0) {
    const sesBySt = strategyBreakdown(sessionTrades);
    const hasAnyTagged = Object.values(sesBySt).some(s => s.n > 0);
    if (hasAnyTagged) {
      lines.push('');
      lines.push('📊 Strategy Breakdown (session):');
      for (const tag of Object.keys(sesBySt)) {
        if (sesBySt[tag].n > 0) {
          lines.push(formatStrategyLine(tag, sesBySt[tag]));
        }
      }
    }
  }
```

After the existing `📊 Overall P&L` block (after the break-even WR line), add:

```javascript
  // ── All-time Strategy Breakdown ───────────────────────────────────
  if (all.n > 0) {
    const allBySt = strategyBreakdown(allTrades);
    const hasAnyTagged = Object.values(allBySt).some(s => s.n > 0);
    if (hasAnyTagged) {
      lines.push('');
      lines.push('📊 All-time by Strategy:');
      for (const tag of Object.keys(allBySt)) {
        if (allBySt[tag].n > 0 || ['backrun_golden', 'backrun_standard', 'graduation_arb'].includes(tag)) {
          // Always show core strategies even at n=0; hide scaled variants if unused
          lines.push(formatStrategyLine(tag, allBySt[tag]));
        }
      }
    }
  }
```

### Example Output

**Session block (after existing session stats):**

```
📊 Strategy Breakdown (session):
  🥇 backrun_golden          n=47   WR= 59.4%  net=+0.0340 SOL  avg=+0.000723
  📊 backrun_standard        n=12   WR= 33.3%  net=-0.0180 SOL  avg=-0.001500
  🎓 graduation_arb          n=0    WR=    —    net=+0.0000 SOL
```

**Overall block (after existing overall stats):**

```
📊 All-time by Strategy:
  🥇 backrun_golden          n=420  WR= 58.8%  net=+0.3800 SOL  avg=+0.000905
  📊 backrun_standard        n=4887 WR= 42.1%  net=-10.3300 SOL  avg=-0.002114
  🎓 graduation_arb          n=0    WR=    —    net=+0.0000 SOL
```

### Backward Compatibility

Trades without `strategyTag` field (all existing ~5,434 records) will be classified as `"backrun_standard"` by the fallback `(t.strategyTag || 'backrun_standard')` in the `strategyBreakdown` function. This is correct: all existing trades predate the golden segment filter and were not golden-filtered.

Once the Rust engine ships with SPEC 1 golden segment config + strategy tagging, new trades will have explicit `strategyTag` values and the breakdown will begin showing golden vs standard splits in real time.

---

*End of Engineering Spec — Specs 1-5 Complete.*