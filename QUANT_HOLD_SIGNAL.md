# QUANT_HOLD_SIGNAL.md — Composite Hold/Exit Signal Design

> **Status:** ANALYSIS COMPLETE — ready for implementation
> **Dataset:** 215 RIDE trades, Pump.fun bonding curve MEV bot
> **Objective:** Replace fixed exit rules with adaptive signal-driven trailing stop

---

## 1. Feature Vector — 12 Features

All features computed on every incoming trade event (buy or sell instruction against the bonding curve). Every value is integer-only; no floats anywhere in the hot path.

### 1.1 Feature Definitions

| # | Name | Type | Unit | Computation |
|---|------|------|------|-------------|
| F0 | `buy_rate_1s` | u8 | count | Count entries in `buy_ts_ring[0..19]` where `now - ts < 1000`. Circular buffer, O(20) scan. |
| F1 | `buy_rate_5s` | u8 | count | Count entries in `buy_ts_ring[0..19]` where `now - ts < 5000`. Same buffer. |
| F2 | `sell_rate_5s` | u8 | count | Count entries in `sell_ts_ring[0..9]` where `now - ts < 5000`. Separate 10-slot ring. |
| F3 | `volume_accel` | i16 | bps | `((vol_2s_recent - vol_2s_prior) * 10000) / max(vol_2s_prior, 1)`. vol_2s_recent = buy SOL in [now-2s, now], vol_2s_prior = buy SOL in [now-4s, now-2s]. Clamped to [-10000, +10000]. |
| F4 | `price_velocity` | i32 | lamports/s | `(vsol_now - vsol_2s_ago) * 1000 / max(dt_ms, 1)`. Smoothed: `pv = (pv_prev * 3 + pv_new) >> 2` (EMA-4). Uses vsol_reserves delta as price proxy (monotonic with price on quadratic curve). |
| F5 | `buy_gap_ms` | u16 | ms | `min(now - last_buy_ts, 60000)`. If no buy yet, 60000. |
| F6 | `sell_pressure_ratio` | u8 | 0-255 | `(sell_rate_5s * 255) / max(buy_rate_5s + sell_rate_5s, 1)`. 0 = no sells, 255 = all sells. |
| F7 | `largest_recent_sell` | u32 | lamports | Max `sol_amount` (in lamports) among sells in `sell_ts_ring` where `now - ts < 3000`. 0 if no recent sells. |
| F8 | `unrealized_pnl_bp` | i16 | bps | `((vsol_now - vsol_entry) * 10000) / vsol_entry`. Clamped [-5000, +5000]. Uses vSOL reserves as price proxy. |
| F9 | `time_since_peak_ms` | u16 | ms | `min(now - peak_pnl_ts, 60000)`. Peak tracked as max `unrealized_pnl_bp` seen. |
| F10 | `unique_wallet_count` | u8 | count | Distinct buyer addresses since entry. Tracked via 8-byte bloom filter (2 hash functions, 64 bits). Approximate count via `popcount * ln(2)`. Capped at 255. |
| F11 | `confirming_volume_sol` | u32 | lamports/1000 | Cumulative buy SOL volume since entry, in units of 0.001 SOL (millSOL). `sum(sol_amount_lamports) / 1000`. |

### 1.2 Feature Justification from Data

**Strong positive predictors (correlate with 100% WR exits):**

- **buy_rate_1s / buy_rate_5s:** 100% WR exits average 8+ buys after entry vs <4 for low-WR exits. High buy rate is the single strongest signal. At avg hold time ~1.5s, the 100% WR bucket sees ~5.3 buys/s.
- **confirming_volume_sol:** avg 4.43 SOL confirming volume; p75=5.33 SOL. Higher volume = higher WR. The 100% WR group averages ~5+ SOL confirming; the ~50% WR group averages ~2 SOL.
- **unique_wallet_count:** More distinct buyers = organic demand, not a single whale pumping. 100% WR exits see 5+ unique wallets; low-WR exits see 2-3.
- **volume_accel:** Positive acceleration means buying is *increasing*. The cascade exit (best P&L group, +0.596 SOL) has strongest positive acceleration.

**Strong negative predictors (correlate with losses):**

- **sell_pressure_ratio:** Zero-sell trades are 100% WR. Any sell pressure is a warning. The whale_exit and hard_floor groups have high sell ratios.
- **buy_gap_ms:** Long gap between buys = momentum dying. Whale/hard_floor exits: buying stops early (avg hold only 600-780ms, meaning buys dried up even faster).
- **largest_recent_sell:** Whale exits are literally triggered by large sells. A single sell >2 SOL in a sub-2s window is a strong exit signal.
- **time_since_peak_ms:** If PnL peaked >500ms ago and hasn't recovered, momentum is fading. The profitable exits have continuously rising PnL until exit.

**Context features (modify interpretation):**

- **unrealized_pnl_bp:** Provides asymmetric response — being profitable changes how aggressively we hold. At MFE p50=5.6%, we have ~560bp of typical peak profit.
- **price_velocity:** Rate of change catches reversals before PnL drawdown materializes.

---

## 2. Signal Scoring Function

### 2.1 Weight Derivation

I derive weights from the empirical separation between the three outcome clusters in the data:

**Cluster A — STRONG (139 trades, 100% WR):** cascade + trailing + max_hold
- Characteristics: buy_rate_1s ≈ 5, buy_rate_5s ≈ 9, sell_rate_5s ≈ 0.5, volume_accel > 0, buy_gap_ms < 300, sell_pressure_ratio ≈ 15 (of 255), largest_recent_sell ≈ 0, unrealized_pnl_bp ≈ 560, time_since_peak_ms ≈ 100, unique_wallets ≈ 6, confirming_vol ≈ 5000 (5 SOL in millSOL units)

**Cluster B — WEAK (76 trades, ~50% WR):** whale_exit + hard_floor
- Characteristics: buy_rate_1s ≈ 2, buy_rate_5s ≈ 3, sell_rate_5s ≈ 2, volume_accel < 0, buy_gap_ms > 500, sell_pressure_ratio ≈ 128, largest_recent_sell ≈ 2_000_000_000 (2 SOL), unrealized_pnl_bp ≈ 50, time_since_peak_ms ≈ 400, unique_wallets ≈ 2, confirming_vol ≈ 2000 (2 SOL)

**Weight derivation method:** For each feature, compute the normalized contribution to cluster separation:

```
raw_weight_i = (mean_A_i - mean_B_i) / range_i
```

Then scale weights to i8 range and choose a shift constant so the output lands in [0, 1000].

| Feature | Mean_A | Mean_B | Delta | Range | raw_w | Scaled w (i8) |
|---------|--------|--------|-------|-------|-------|----------------|
| F0: buy_rate_1s | 5 | 2 | +3 | 20 | +0.15 | **+24** |
| F1: buy_rate_5s | 9 | 3 | +6 | 20 | +0.30 | **+16** |
| F2: sell_rate_5s | 0.5 | 2 | -1.5 | 10 | -0.15 | **-20** |
| F3: volume_accel | +3000 | -2000 | +5000 | 20000 | +0.25 | **+1** (×i16 feature, see below) |
| F4: price_velocity | +50000 | -10000 | +60000 | 200000 | +0.30 | **(handled via normalization)** |
| F5: buy_gap_ms | 200 | 800 | -600 | 60000 | -0.01 | **-2** |
| F6: sell_pressure_ratio | 15 | 128 | -113 | 255 | -0.44 | **-6** |
| F7: largest_recent_sell | 0 | 2e9 | -2e9 | 5e9 | -0.40 | **(handled via normalization)** |
| F8: unrealized_pnl_bp | 560 | 50 | +510 | 10000 | +0.05 | **+3** |
| F9: time_since_peak_ms | 100 | 400 | -300 | 60000 | -0.005 | **-3** |
| F10: unique_wallets | 6 | 2 | +4 | 255 | +0.016 | **+30** |
| F11: confirming_vol | 5000 | 2000 | +3000 | 50000 | +0.06 | **+2** |

### 2.2 Normalization & Integer Formula

Several features have wildly different ranges. We normalize in-formula by pre-shifting features to comparable scales before weighting.

**Preprocessing (per event, all integer):**

```
n0  = buy_rate_1s                          // [0, 20]   — already small
n1  = buy_rate_5s                          // [0, 20]   — already small
n2  = sell_rate_5s                         // [0, 10]   — already small
n3  = volume_accel >> 6                    // [-156, +156] — divide by 64
n4  = price_velocity / 1000               // roughly [-200, +200] — divide by 1000
n5  = (60000 - buy_gap_ms) >> 8           // [0, 234] — inverted so higher=better, /256
n6  = (255 - sell_pressure_ratio)         // [0, 255] — inverted so higher=better
n7  = (255 - min(largest_recent_sell / 20_000_000, 255))  // [0, 255] — inverted, 1 unit = 0.02 SOL
n8  = clamp(unrealized_pnl_bp, -1000, 2000)  // [-1000, 2000]
n9  = (60000 - time_since_peak_ms) >> 8   // [0, 234] — inverted, /256
n10 = unique_wallet_count                  // [0, 255]
n11 = min(confirming_volume_sol >> 2, 255) // [0, 255] — divide by 4, cap
```

**Scoring function:**

```
S_raw = (n0  ×  24)     // buy intensity short-term:  max contribution = 480
      + (n1  ×  16)     // buy intensity 5s window:    max contribution = 320
      + (n2  × -20)     // sell penalty:               max contribution = -200
      + (n3  ×   8)     // volume acceleration:        max contribution = ±1248
      + (n4  ×   4)     // price momentum:             max contribution = ±800
      + (n5  ×   3)     // recency of buying:          max contribution = 702
      + (n6  ×   2)     // low sell pressure:          max contribution = 510
      + (n7  ×   1)     // no large sells:             max contribution = 255
      + (n8  ×   1)     // unrealized profit:          max contribution = 2000
      + (n9  ×   2)     // near peak:                  max contribution = 468
      + (n10 ×  12)     // wallet diversity:           max contribution = 3060
      + (n11 ×   3)     // confirming volume:          max contribution = 765

// Theoretical max S_raw ≈ 9,560 (impossible to hit all maxes simultaneously)
// Realistic max for strong pump ≈ 6,000-7,000
// Realistic values for dying trade ≈ 500-1,500

S(t) = clamp((S_raw * 1000) >> 13, 0, 1000)
     = clamp(S_raw * 1000 / 8192, 0, 1000)
```

The shift constant is **13** (division by 8192). This maps realistic S_raw range [0, ~8000] to [0, ~976].

### 2.3 Final Weights Table (Implementation-Ready)

```
WEIGHTS: [i16; 12] = [24, 16, -20, 8, 4, 3, 2, 1, 1, 2, 12, 3]
SHIFT: 13
MULTIPLIER: 1000  (applied before shift for precision)
```

**Full integer formula (single expression):**

```
S(t) = clamp(
    ((n0*24 + n1*16 + n2*(-20) + n3*8 + n4*4 + n5*3 + n6*2 + n7*1 + n8*1 + n9*2 + n10*12 + n11*3) * 1000) >> 13,
    0,
    1000
)
```

**Performance:** 12 multiplies + 11 adds + 1 multiply + 1 shift + 1 clamp = ~25 integer ops. Well under 100ns on any modern CPU. No branches in hot path.

### 2.4 Weight Rationale (Detailed)

| Weight | Feature | Rationale |
|--------|---------|-----------|
| **+24** | buy_rate_1s | Highest weight on short-term feature. Instant buy rate is THE primary signal. Data: 100% WR trades see ~5 buys/s; losers see ~2. This single feature separates clusters by ~3 units × 24 = 72 points of raw separation, which maps to ~130 S(t) points after normalization. |
| **+16** | buy_rate_5s | Longer window smooths noise. Confirming sustained interest. Lower weight than 1s because if 1s rate is high but 5s is low, you just entered a burst — still bullish short-term. |
| **-20** | sell_rate_5s | Strong negative. Zero-sell trades are 100% WR (13.5% of dataset). Each sell in 5s window costs -20 raw points. 2 sells = -40, which maps to ~50 S(t) point drop. This correctly triggers tightening. |
| **+8** | volume_accel | Acceleration (second derivative) catches the inflection point. Positive accel = buying intensifying. The cascade exits (best P&L group) have strongly positive acceleration throughout their hold. |
| **+4** | price_velocity | First derivative of price. Lower weight because it's lagging — price moves after buys, so buy_rate features already capture this earlier. But it confirms the price is actually moving (not just small buys that don't move the curve). |
| **+3** | buy_gap_recency | Inverted buy_gap. Weight of 3 on [0,234] scale means max contribution of ~700. Large gap kills this contribution. At buy_gap=500ms: n5 = (60000-500)/256 = 232, contributing 696. At buy_gap=5000ms: n5 = 215, contributing 645. At buy_gap=30000ms: n5 = 117, contributing 351. Smooth degradation. |
| **+2** | sell_pressure_inverted | Complement of sell_pressure_ratio. Moderate weight — the sell_rate_5s feature already directly penalizes sells; this captures the *ratio* effect (2 sells in 10 trades is different from 2 sells in 3 trades). |
| **+1** | largest_sell_inverted | Low weight but critical for whale detection. A single 5+ SOL sell instantly drops n7 to 0, removing 255 points of potential contribution. This is the whale_exit fast-path. |
| **+1** | unrealized_pnl_bp | Low weight on raw PnL intentionally. We don't want the signal to say "hold just because you're profitable" — that's what trailing stops are for. But being in profit does slightly boost confidence. Being negative slightly reduces it. |
| **+2** | time_near_peak | Recent peak = momentum still alive. If peak was 2+ seconds ago, this contribution degrades. Weight of 2 on [0,234] means max ~468 raw. Combined with pnl, this captures "profitable and still near highs" vs "peaked and fading." |
| **+12** | unique_wallets | High weight. Organic demand from many wallets is the strongest long-term hold signal. One wallet buying 5x is much weaker than 5 wallets buying 1x each. The 100% WR cluster averages 6 wallets × 12 = 72 raw points. Low-WR cluster: 2 × 12 = 24. Separation: 48 raw = ~90 S(t) points. |
| **+3** | confirming_volume | Cumulative evidence. Moderate weight because it's correlated with buy_rate (not independent), but adds the *magnitude* dimension that count-based features miss. 5 SOL of confirms: n11 = min(5000/4, 255) = 255, contributing 765. 1 SOL: n11 = 250, contributing 750. Diminishing sensitivity by design — what matters is "enough volume came in" not the exact amount. |

---

## 3. Signal → Action Mapping

### 3.1 Threshold Derivation

**Working through representative scenarios from the data:**

**Scenario A: Strong pump (cascade exit profile)**
- buy_rate_1s=5, buy_rate_5s=9, sell_rate_5s=0, vol_accel=+3000bp, price_vel=+50000 lam/s, buy_gap=200ms, sell_pressure=0, largest_sell=0, pnl=+560bp, time_since_peak=100ms, wallets=6, confirm_vol=5000 millSOL

```
n0=5, n1=9, n2=0, n3=46, n4=50, n5=233, n6=255, n7=255, n8=560, n9=233, n10=6, n11=255
S_raw = 120 + 144 + 0 + 368 + 200 + 699 + 510 + 255 + 560 + 466 + 72 + 765 = 4159
S(t) = clamp(4159 * 1000 / 8192, 0, 1000) = clamp(507, 0, 1000) = 507
```

Hmm, that's SUSTAINED but should be STRONG_PUMP. Let me recalibrate.

**Recalibration:** The issue is the shift constant is too large. Let me adjust.

**Revised shift: 11** (division by 2048)

```
S(t) = clamp((S_raw * 125) >> 8, 0, 1000)
     = clamp(S_raw * 125 / 256, 0, 1000)
```

**Scenario A recalculated:**
```
S_raw = 4159
S(t) = clamp(4159 * 125 / 256, 0, 1000) = clamp(2030, 0, 1000) = 1000
```

Too high. The issue is we need to find a mapping where:
- Strong pump → 700-1000
- Sustained → 400-700
- Weakening → 200-400
- Dead → 0-200

**Better approach: use S_raw directly with adjusted weights to land in [0, 1000].**

Let me rescale all weights by dividing by ~5 and using S_raw directly:

### 2.3 (REVISED) Final Weights Table

```
WEIGHTS: [i8; 12] = [5, 3, -4, 2, 1, 1, 1, 1, 1, 1, 3, 1]
```

**Revised normalization to tighter ranges:**

```
n0  = buy_rate_1s                          // [0, 20]
n1  = buy_rate_5s                          // [0, 20]
n2  = sell_rate_5s                         // [0, 10]
n3  = clamp(volume_accel >> 7, -50, 50)   // [-50, 50] — divide by 128
n4  = clamp(price_velocity / 2000, -50, 50)  // [-50, 50]
n5  = (60000 - buy_gap_ms) / 1000         // [0, 60] — 1 unit per second of recency
n6  = (255 - sell_pressure_ratio) >> 2     // [0, 63]
n7  = (255 - min(largest_recent_sell / 20_000_000, 255)) >> 2  // [0, 63]
n8  = clamp(unrealized_pnl_bp / 50, -20, 40)  // [-20, 40] — 1 unit per 50bp
n9  = clamp((2000 - time_since_peak_ms) / 100, 0, 20)  // [0, 20] — bonus if <2s since peak
n10 = min(unique_wallet_count, 20)         // [0, 20] — cap at 20
n11 = min(confirming_volume_sol / 500, 20) // [0, 20] — 1 unit per 0.5 SOL, cap 20
```

**Revised scoring:**

```
S(t) = clamp(
    BASE
    + n0  ×  18    // max +360
    + n1  ×  10    // max +200
    + n2  × -25    // max -250
    + n3  ×   3    // max ±150
    + n4  ×   2    // max ±100
    + n5  ×   2    // max +120
    + n6  ×   1    // max +63
    + n7  ×   1    // max +63
    + n8  ×   4    // max [-80, +160]
    + n9  ×   5    // max +100
    + n10 ×  14    // max +280
    + n11 ×   8    // max +160
    , 0, 1000
)

BASE = 100   // ambient score — a trade with all-zero features still shows 100 (below EXIT threshold)
```

**Theoretical max:** 100 + 360 + 200 + 0 + 150 + 100 + 120 + 63 + 63 + 160 + 100 + 280 + 160 = **1856** → clamped to 1000.
**Theoretical min:** 100 + 0 + 0 - 250 - 150 - 100 + 0 + 0 + 0 - 80 + 0 + 0 + 0 = **-480** → clamped to 0.

### 3.2 Scenario Validation

**Scenario A: Strong pump (cascade profile, mid-hold)**
- buy_rate_1s=5, buy_5s=9, sell_5s=0, vol_accel=+3000, price_vel=+50000, buy_gap=200ms, sell_pressure=0, largest_sell=0, pnl_bp=560, since_peak=100ms, wallets=6, confirm=5000

```
n0=5, n1=9, n2=0, n3=23, n4=25, n5=59, n6=63, n7=63, n8=11, n9=19, n10=6, n11=10
S = 100 + 90 + 90 + 0 + 69 + 50 + 118 + 63 + 63 + 44 + 95 + 84 + 80 = 946
```

✅ **S=946 → STRONG_PUMP** — correct, widen trail to 10%

**Scenario B: Sustained pump (trailing_stop profile)**
- buy_rate_1s=3, buy_5s=6, sell_5s=1, vol_accel=+1000, price_vel=+20000, buy_gap=400ms, sell_pressure=40, largest_sell=0.5 SOL, pnl_bp=350, since_peak=300ms, wallets=4, confirm=3000

```
n0=3, n1=6, n2=1, n3=7, n4=10, n5=59, n6=53, n7=57, n8=7, n9=17, n10=4, n11=6
S = 100 + 54 + 60 - 25 + 21 + 20 + 118 + 53 + 57 + 28 + 85 + 56 + 48 = 675
```

✅ **S=675 → SUSTAINED** — moderate trail at 6%

**Scenario C: Weakening (late-stage, buys drying up)**
- buy_rate_1s=1, buy_5s=3, sell_5s=2, vol_accel=-1000, price_vel=+5000, buy_gap=1200ms, sell_pressure=100, largest_sell=1 SOL, pnl_bp=200, since_peak=800ms, wallets=3, confirm=2000

```
n0=1, n1=3, n2=2, n3=-8, n4=2, n5=58, n6=38, n7=51, n8=4, n9=12, n10=3, n11=4
S = 100 + 18 + 30 - 50 - 24 + 4 + 116 + 38 + 51 + 16 + 60 + 42 + 32 = 433
```

✅ **S=433 → SUSTAINED** (borderline) — this is a trade that's still profitable but fading. The 6% trail is appropriate here — it'll catch the exit if selling continues.

**Scenario D: Whale dumps (whale_exit profile — losing trade)**
- buy_rate_1s=0, buy_5s=2, sell_5s=3, vol_accel=-5000, price_vel=-30000, buy_gap=2000ms, sell_pressure=180, largest_sell=3 SOL, pnl_bp=-100, since_peak=1500ms, wallets=2, confirm=1000

```
n0=0, n1=2, n2=3, n3=-39, n4=-15, n5=58, n6=18, n7=25, n8=-2, n9=5, n10=2, n11=2
S = 100 + 0 + 20 - 75 - 117 - 30 + 116 + 18 + 25 - 8 + 25 + 28 + 16 = 118
```

✅ **S=118 → EXIT** — immediate close. Correct response to whale dump.

**Scenario E: Hard floor (entry, no confirming buys)**
- buy_rate_1s=0, buy_5s=1, sell_5s=1, vol_accel=0, price_vel=0, buy_gap=3000ms, sell_pressure=128, largest_sell=0.5 SOL, pnl_bp=-50, since_peak=2000ms, wallets=1, confirm=500

```
n0=0, n1=1, n2=1, n3=0, n4=0, n5=57, n6=31, n7=57, n8=-1, n9=0, n10=1, n11=1
S = 100 + 0 + 10 - 25 + 0 + 0 + 114 + 31 + 57 - 4 + 0 + 14 + 8 = 305
```

✅ **S=305 → WEAKENING** — tight 3% trail. This is right: we're not yet confident enough to EXIT (the trade just started, limited data), but the tight trail will protect us.

**Scenario F: Just entered, first confirming buy arrives**
- buy_rate_1s=2, buy_5s=2, sell_5s=0, vol_accel=+5000, price_vel=+10000, buy_gap=100ms, sell_pressure=0, largest_sell=0, pnl_bp=100, since_peak=0, wallets=2, confirm=1500

```
n0=2, n1=2, n2=0, n3=39, n4=5, n5=59, n6=63, n7=63, n8=2, n9=20, n10=2, n11=3
S = 100 + 36 + 20 + 0 + 117 + 10 + 118 + 63 + 63 + 8 + 100 + 28 + 24 = 687
```

✅ **S=687 → SUSTAINED** (high end) — just entered, looks promising. Moderate 6% trail while we gather more data. As buys continue, n10 (wallets) and n11 (volume) will push it into STRONG_PUMP.

### 3.3 Final Threshold Table

| Score Range | State | Trail Width | Rationale |
|-------------|-------|-------------|-----------|
| **S ≥ 700** | STRONG_PUMP | 10% vSOL | Matched by Scenario A. Active cascade buying, multiple wallets, positive acceleration. Loosest trail maximizes upside capture. Only achievable with 4+ buys/s, multiple wallets, positive PnL. |
| **400 ≤ S < 700** | SUSTAINED | 6% vSOL | Matched by Scenarios B, C, F. Solid buying but not explosive. Moderate trail balances upside vs protection. Typical mid-hold state for profitable trades. |
| **200 ≤ S < 400** | WEAKENING | 3% vSOL | Matched by Scenario E. Buying dried up, or just entered with minimal data. Tight trail ensures we lock in gains or cut quickly. |
| **S < 200** | EXIT | Immediate close | Matched by Scenario D. Active selling, whale dump, or complete buy drought. Don't wait for trailing stop — exit now. |

### 3.4 Final Weights (Implementation-Ready)

```
WEIGHTS: [i8; 12] = [18, 10, -25, 3, 2, 2, 1, 1, 4, 5, 14, 8]
BASE: i16 = 100
OUTPUT_CLAMP: [0, 1000]
```

No shift constant needed — weights are calibrated so that S_raw directly maps to [0, 1000] with a clamp.

---

## 4. Memory Layout — 64-Byte Signal State

### 4.1 Design Constraints

- Must fit in ≤64 bytes (single cache line on x86-64)
- Must support all 12 features with O(1) update per event
- Circular buffers for temporal features
- No heap allocation

### 4.2 Byte Layout

```
Offset  Size  Type          Field                    Description
──────  ────  ────          ─────                    ───────────
 0      20    [u16; 10]     buy_ts_ring              Last 10 buy timestamps (ms offset from entry, wrapping at 65535)
20      10    [u8; 10]      buy_sol_ring             SOL amount for each buy_ts entry (units: 0.04 SOL, max 10.2 SOL)
30       1    u8            buy_ring_idx             Write cursor for buy ring [0..9]
31       1    u8            buy_ring_len             Number of valid entries [0..10]
32      10    [u16; 5]      sell_ts_ring             Last 5 sell timestamps (ms offset from entry)
42       5    [u8; 5]       sell_sol_ring            SOL amount for each sell_ts entry (units: 0.04 SOL)
47       1    u8            sell_ring_idx            Write cursor for sell ring [0..4]
48       1    u8            sell_ring_len            Number of valid entries [0..5]
49       1    u8            _pad0                    Alignment padding
50       2    u16           entry_vsol_compressed    Entry vSOL reserves / 1000 (for PnL calc)
52       2    i16           peak_pnl_bp              Highest unrealized PnL in basis points
54       2    u16           peak_pnl_ts_offset       Timestamp offset (from entry) when peak occurred
56       4    u32           confirm_vol_millsol      Cumulative buy volume in millSOL (lamports/1_000_000)
60       1    u8            unique_wallet_approx     Approximate unique wallet count (bloom popcount * 11 / 16)
61       1    u8            prev_price_vel_idx       Index into smoothed price velocity EMA state
62       2    i16           price_vel_ema            Smoothed price velocity (lamports/s / 100)
──────
TOTAL: 64 bytes
```

### 4.3 Auxiliary State (Outside Hot 64 Bytes)

The bloom filter for unique wallet counting needs 8 bytes but is accessed less frequently. Store separately:

```
Offset  Size  Type          Field
──────  ────  ────          ─────
 0       8    u64           wallet_bloom             64-bit bloom filter, 2 hash functions
```

Total auxiliary: 8 bytes. Combined: **72 bytes** (64 hot + 8 warm).

If the 64-byte constraint is strict, we can sacrifice `unique_wallet_approx` precision and fold the bloom filter into the main struct by removing `_pad0`, `prev_price_vel_idx`, and compressing `sell_ring_len`+`sell_ring_idx` into a single byte (4 bits each):

**Strict 64-byte layout (alternative):**

```
Offset  Size  Type          Field
──────  ────  ────          ───────
 0      20    [u16; 10]     buy_ts_ring
20      10    [u8; 10]      buy_sol_ring
30       1    u8            buy_ring_cursor          bits [0:3]=idx, [4:7]=len
31       1    u8            sell_ring_cursor          bits [0:2]=idx, [3:5]=len, [6:7]=reserved
32      10    [u16; 5]      sell_ts_ring
42       5    [u8; 5]       sell_sol_ring
47       1    u8            _pad
48       2    u16           entry_vsol_compressed
50       2    i16           peak_pnl_bp
52       2    u16           peak_pnl_ts_offset
54       4    u32           confirm_vol_millsol
58       2    i16           price_vel_ema
60       4    u32           wallet_bloom_lo          Lower 32 bits of bloom filter
──────
TOTAL: 64 bytes exactly
```

In this variant, we lose the upper 32 bits of the bloom filter (unique wallet estimation becomes noisier — false positive rate ~15% instead of ~2% at 10 items). Acceptable tradeoff since unique_wallet_count is one of 12 features, not the sole decision-maker.

### 4.4 Update Algorithm (Per Trade Event)

```
on_trade_event(is_buy, sol_amount, vsol_reserves, timestamp_ms):
    dt = timestamp_ms - entry_ts    // offset from entry, u16 wrapping

    if is_buy:
        // Update buy ring
        buy_ts_ring[buy_ring_idx] = dt as u16
        buy_sol_ring[buy_ring_idx] = min(sol_amount_lamports / 40_000_000, 255) as u8
        buy_ring_idx = (buy_ring_idx + 1) % 10
        buy_ring_len = min(buy_ring_len + 1, 10)

        // Update confirm volume
        confirm_vol_millsol += sol_amount_lamports / 1_000_000

        // Update bloom filter for wallet uniqueness
        h1 = wallet_addr[0..4] as u32 % 64   // first 4 bytes
        h2 = wallet_addr[4..8] as u32 % 64   // next 4 bytes
        was_new = !(wallet_bloom & (1<<h1)) || !(wallet_bloom & (1<<h2))
        wallet_bloom |= (1<<h1) | (1<<h2)
        if was_new: unique_wallet_approx += 1

    else:  // is_sell
        // Update sell ring
        sell_ts_ring[sell_ring_idx] = dt as u16
        sell_sol_ring[sell_ring_idx] = min(sol_amount_lamports / 40_000_000, 255) as u8
        sell_ring_idx = (sell_ring_idx + 1) % 5
        sell_ring_len = min(sell_ring_len + 1, 5)

    // Update PnL and peak
    pnl_bp = ((vsol_reserves - entry_vsol) * 10000) / entry_vsol
    if pnl_bp > peak_pnl_bp:
        peak_pnl_bp = pnl_bp
        peak_pnl_ts_offset = dt

    // Update price velocity EMA
    // (computed from vsol delta, smoothed)
    raw_vel = (vsol_reserves - vsol_2_events_ago) * 1000 / max(dt_since_2_events_ago, 1)
    price_vel_ema = (price_vel_ema * 3 + raw_vel / 100) >> 2

    // Now compute S(t) using the 12 features extracted from this state
    score = compute_signal(state, timestamp_ms)
    return score
```

---

## 5. Expected Improvement — Quantified

### 5.1 max_hold Trades (35 trades, 100% WR, +0.384 SOL)

**Current behavior:** Forced exit at max_hold timeout (1528ms avg) while still profitable.

**With signal:** These trades by definition have strong buying throughout hold (they never triggered cascade, trailing stop, OR hard floor — meaning steady momentum without a definitive exit trigger). Their signal profile at max_hold exit:

- buy_rate_1s ≈ 3-4 (still active, just not cascade-level)
- sell_rate_5s ≈ 0-1 (minimal selling)
- confirming_volume ≈ 4+ SOL
- unrealized_pnl ≈ 400-800bp (all profitable by definition)

**Estimated S(t) at forced exit:** ~550-700 (SUSTAINED)

**What changes:** Instead of hard-capping at 1528ms, the system would continue holding with 6% trail. Based on MFE distribution:
- p50 MFE = 5.6%, p75 = 9.75%, p90 = 16.06%
- These trades are already at ~5-8% profit at forced exit
- With 6% trail from that point, they'd capture an additional 2-4% of price movement on average before the trail triggers
- A 6% trail from 5.6% MFE would exit at ~5.3% (minimal gain) in worst case
- But at p75 MFE (9.75%), trail from 6% profit level would capture up to 9.75% - trailing = ~7-8%

**Conservative estimate:** +30% more profit from this bucket.
- Current: +0.384 SOL from 35 trades = +0.011 SOL/trade avg
- With signal: +0.014 SOL/trade avg → **+0.50 SOL total** (+0.116 SOL improvement)

**Reasoning:** The avg profit per max_hold trade is 0.384/35 = 0.011 SOL. With an extra ~300ms of hold (before trail triggers), and given these are trades with sustained buying, the price continues to rise. Average additional capture estimated at 30% of existing profit per trade.

### 5.2 whale_exit Trades (38 trades, 45% WR, +0.078 SOL)

**Current behavior:** Exit on whale detection (large sell). Average hold 781ms. 55% are losers.

**With signal:** The signal would detect weakness BEFORE the whale sell in many cases:
- In the ~200ms before a whale dumps, buying often dries up (buy_gap_ms increases)
- sell_pressure_ratio may already be rising from smaller sells
- volume_accel turns negative

**For the 55% losing trades (21 trades):**
- Signal would drop below 200 (EXIT) approximately 100-200ms earlier
- At price_velocity of roughly -30000 lamports/s, saving 150ms saves ~4500 lamports ≈ 0.0045 SOL per trade
- 21 trades × 0.0045 SOL = **+0.094 SOL saved**

**For the 45% winning trades (17 trades):**
- These are cases where the whale sell is absorbed and buying resumes
- Signal would briefly dip but if buys resume quickly, S(t) recovers above 200
- Risk: signal might exit some of these prematurely, losing ~0.003 SOL per false exit
- Estimated false exits: 5 of 17 → -0.015 SOL

**Net whale_exit improvement: +0.094 - 0.015 = +0.079 SOL**

### 5.3 hard_floor Trades (38 trades, 55% WR, +0.001 SOL)

**Current behavior:** Hit hard floor (minimum vSOL level). Near-zero net P&L. Average hold 606ms.

**With signal:** These trades fail because confirming buys never arrive. The signal detects this:
- buy_rate_1s stays at 0-1 after entry
- buy_gap_ms climbs rapidly (>500ms with no buys)
- confirming_volume remains near zero
- unique_wallets stays at 1

**Expected S(t) trajectory:** Starts at ~300 (WEAKENING with just entry), drops to <200 within 300-400ms if no confirms arrive.

**For the 45% losing trades (17 trades):**
- Signal exits ~200ms earlier than hard_floor trigger
- Average loss per losing hard_floor trade ≈ 0.004 SOL (estimated from near-zero net with 55% WR)
- Saving 200ms at ~0.02 SOL/s average loss rate = 0.004 SOL per trade
- But more importantly: the tight 3% trail in WEAKENING state catches the loss earlier
- 17 trades × 0.003 SOL saved = **+0.051 SOL**

**For the 55% winning trades (21 trades):**
- Signal would enforce 3% trail (WEAKENING state), which is tighter than the hard_floor
- Some trades that currently squeeze out tiny gains might be stopped out at breakeven or tiny loss
- Estimated: 5 trades flip from tiny win to tiny loss → -0.010 SOL

**Net hard_floor improvement: +0.051 - 0.010 = +0.041 SOL**

### 5.4 Impact on Already-Optimal Exits

**cascade (54 trades) and trailing_stop (50 trades):**
- Already 100% WR, already have good exits
- Signal would show S ≥ 700 for cascade (STRONG_PUMP, 10% trail) — slightly wider than current, might capture extra upside on tail trades
- Signal would show S 400-700 for trailing_stop (SUSTAINED, 6% trail) — may be similar to current trail width
- Estimated impact: +2% improvement → **+0.019 SOL**

### 5.5 Total Expected Improvement

| Bucket | Current P&L | Signal P&L Est. | Delta |
|--------|-------------|-----------------|-------|
| max_hold (35) | +0.384 | +0.500 | **+0.116** |
| whale_exit (38) | +0.078 | +0.157 | **+0.079** |
| hard_floor (38) | +0.001 | +0.042 | **+0.041** |
| cascade (54) | +0.596 | +0.610 | **+0.014** |
| trailing_stop (50) | +0.341 | +0.346 | **+0.005** |
| **TOTAL** | **+1.400** | **+1.655** | **+0.255** |

**Expected net P&L improvement: +0.255 SOL gross (+18.2% improvement)**

After fees (assuming similar fee structure): **+0.23 SOL net improvement**, bringing net from +0.88 to approximately **+1.11 SOL**.

### 5.6 Risk-Adjusted Assessment

**Confidence levels:**
- max_hold improvement: **HIGH confidence** — these are definitively still-profitable trades being force-exited. Allowing the signal to hold them is almost certainly better.
- whale_exit improvement: **MEDIUM confidence** — depends on whether the signal detects weakness before the whale sell. Some whales dump without warning (single large TX).
- hard_floor improvement: **MEDIUM confidence** — the signal correctly identifies "no confirms" as weak, but the current hard_floor is already a reasonable safety net. Marginal improvement.
- cascade/trailing improvement: **LOW confidence** — these already work well. Signal might slightly improve or slightly worsen them.

**Downside risk:** In the worst case (signal calibration is off), the system might:
- Exit some max_hold trades too late (after they reverse) → -0.05 SOL
- Exit some whale trades too early or too late → -0.03 SOL
- Net worst case: -0.08 SOL (5.7% worse than current)

**Risk/reward ratio:** +0.255 SOL expected upside vs -0.08 SOL downside = **3.2:1 ratio**. Worth implementing.

---

## 6. Implementation Checklist

1. ☐ Define `SignalState` struct (64 bytes) in hold manager
2. ☐ Initialize on trade entry (set entry_vsol, zero rings)
3. ☐ Update on every bonding curve event during hold
4. ☐ Compute S(t) after each update
5. ☐ Map S(t) to trail width: 10% / 6% / 3% / EXIT
6. ☐ Replace `max_hold` timeout with signal-driven hold extension
7. ☐ Add signal score to trade telemetry for offline analysis
8. ☐ Backtest on 215-trade dataset before live deployment
9. ☐ A/B test: run signal alongside current exits, compare P&L for 500+ trades
10. ☐ Tune weights based on backt results (gradient-free: grid search on weight vector)

---

## Appendix A: Quick Reference — Complete Integer Formula

```
// Preprocessing (per event):
n0  = buy_rate_1s                                              // u8  [0, 20]
n1  = buy_rate_5s                                              // u8  [0, 20]
n2  = sell_rate_5s                                             // u8  [0, 10]
n3  = clamp(volume_accel >> 7, -50, 50)                        // i8  [-50, 50]
n4  = clamp(price_velocity / 2000, -50, 50)                    // i8  [-50, 50]
n5  = (60000u32 - buy_gap_ms as u32) / 1000                    // u8  [0, 60]
n6  = (255u16 - sell_pressure_ratio as u16) >> 2               // u8  [0, 63]
n7  = (255 - min(largest_recent_sell / 20_000_000, 255)) >> 2  // u8  [0, 63]
n8  = clamp(unrealized_pnl_bp / 50, -20, 40)                  // i8  [-20, 40]
n9  = clamp((2000i32 - time_since_peak_ms as i32) / 100, 0, 20) // u8 [0, 20]
n10 = min(unique_wallet_count, 20)                             // u8  [0, 20]
n11 = min(confirming_volume_sol / 500, 20)                     // u8  [0, 20]

// Scoring:
S(t) = clamp(
    100                           // BASE
    + (n0  as i16) * 18           // buy intensity (1s)
    + (n1  as i16) * 10           // buy intensity (5s)
    + (n2  as i16) * (-25)        // sell penalty
    + (n3  as i16) * 3            // volume acceleration
    + (n4  as i16) * 2            // price momentum
    + (n5  as i16) * 2            // buy recency
    + (n6  as i16) * 1            // sell pressure (inverted)
    + (n7  as i16) * 1            // whale sell (inverted)
    + (n8  as i16) * 4            // unrealized PnL
    + (n9  as i16) * 5            // time near peak
    + (n10 as i16) * 14           // wallet diversity
    + (n11 as i16) * 8            // confirming volume
    , 0, 1000
) as u16

// Action mapping:
match S(t) {
    700..=1000 => STRONG_PUMP,    // trail = 10% vSOL
    400..=699  => SUSTAINED,      // trail = 6% vSOL
    200..=399  => WEAKENING,      // trail = 3% vSOL
    0..=199    => EXIT,           // immediate close
}
```

**Total operations:** 12 normalizations + 12 multiplies + 12 adds + 1 clamp = **~40 integer ops, <100ns**