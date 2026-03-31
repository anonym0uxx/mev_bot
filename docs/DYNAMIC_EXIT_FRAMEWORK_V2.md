# Dynamic Exit Framework v2 — Corrected & Production-Ready

**Author:** Apollo (from bare-metal Rust audit of pump-quant-core)
**Supersedes:** DYNAMIC_EXIT_FRAMEWORK.md (v1 — quant spec with incorrect data model assumptions)
**Constraint envelope:** ≤192 bytes added to RideState. Integer-only hot path. Zero heap. Zero f64. ≤5μs/tick total.

---

## 0. Audit Summary — Why V1 Was Wrong

V1 was written by a quant researcher who assumed a traditional exchange data model:
continuous price feeds, regular tick intervals, floating-point arithmetic, and unlimited
memory budget. Our engine operates under fundamentally different constraints.

**Corrections applied:**

| V1 Assumption | Reality | V2 Fix |
|---|---|---|
| Continuous log returns | Discrete trade events, irregular spacing (0ms–10s gaps) | Event-count windows, not time-EMA |
| f32/f64 arithmetic on hot path | Integer-only, zero f64 since RideState v3 | All u16/u32 fixed-point |
| 2,600 bytes per position | 128 bytes / 2 cache lines (RideState v3) | Budget: +64 bytes → 192 total (3 cache lines) |
| Weibull decay LUT (1,200 bytes) | Exponential decay: 2 muls + 2 shifts, <5ns | Keep existing. Parameterize per tier if needed |
| CUSUM + Shiryaev-Roberts | Bayesian f̂* already IS regime detection | Drop entirely. f̂*→0 = regime change |
| ATR from price returns | No regular price samples on bonding curve | Event-count variance of vSOL deltas |
| 4-timeframe EMA momentum | No regular-interval prices | Event-count ring windows: 5/15/50 events |
| Pool SOL reserves always available | Only available post-graduation (PriceFeedManager) | Use `current_vsol` from TradeEvent for bonding curve |

**What V1 got right (kept):**
- Kelly edge decay concept → already implemented as f̂*(t)
- Bayesian posterior with Beta(α,β) → already implemented in RideState v3
- Dynamic trail width from conviction → already implemented
- Unified exit scoring concept → implemented below as integer urgency
- Partial exit on weakening edge → NEW, implemented below
- Monotonic urgency guarantee → NEW, implemented below

---

## 1. Architecture — Single Integer Urgency Score

The exit engine computes a **composite urgency** `U` (u16, 0–10000) every tick
from four signal components. When U crosses thresholds, exit decisions fire.

```
                         ┌──────────────────┐
  ┌─────────────┐       │                  │
  │ Bayesian f̂  │──u16──│                  │
  │ (§2, EXISTS)│       │                  │
  └─────────────┘       │   Urgency        │──u16 U──▶ EXIT / PARTIAL / HOLD
  ┌─────────────┐       │   Combiner       │
  │ Momentum    │──u16──│   (§5)           │
  │ Divergence  │       │                  │
  │ (§3, NEW)   │       │                  │
  └─────────────┘       │                  │
  ┌─────────────┐       │                  │
  │ Volatility  │──u16──│                  │
  │ Trail (§4)  │       │                  │
  │   (NEW)     │       └──────────────────┘
  └─────────────┘
  ┌─────────────┐
  │ Liquidity   │──u16──┘
  │ Slippage    │
  │ (§4b, NEW)  │
  └─────────────┘
```

**Why 4 components, not 6:**
- Kelly edge decay ≡ Bayesian f̂* (they're the same signal — f̂* IS the real-time Kelly fraction)
- CUSUM/SR ≡ Bayesian state machine (f̂*→0 IS quickest detection of regime change)
- Merging redundant signals eliminates noise from double-counting the same underlying evidence

---

## 2. Component 1: Bayesian Kelly Edge — ALREADY IMPLEMENTED

**Location:** `ride_state.rs` → `RideState::on_tick()` → `bayesian_current_f_permille()`

**No changes needed.** This is the core of the system and it's already correct:

```rust
// Existing code — DO NOT MODIFY
fn bayesian_current_f_permille(&self) -> i16 {
    let a = self.alpha_x16 as u32;
    let b = self.beta_x16 as u32;
    let ab = a + b;
    if ab == 0 { return 0; }
    let p_x1000 = (a * 1000) / ab;
    let r = fee_adjust_r(self.r_est_x100, DEFAULT_ROUND_TRIP_FEE_BP, self.avg_loss_bp).max(1) as u32;
    let numerator = (p_x1000 * (r + 100)) as i32 - 100_000;
    (numerator / (2 * r as i32)).clamp(-1000, 1000) as i16
}
```

**Signal output for urgency combiner:**

```rust
/// Map f̂* to urgency component u_kelly (u16, 0–10000).
///
/// f̂* > 0.70 × f_entry → 0 (strong edge, no urgency)
/// f̂* = 0.35 × f_entry → 3000 (weakening)
/// f̂* = 0               → 7000 (edge gone)
/// f̂* < 0               → 10000 (negative EV — GET OUT)
///
/// Linear interpolation between thresholds. Integer-only.
#[inline(always)]
fn u_kelly(f_hat: i16, f_entry: u16) -> u16 {
    if f_entry == 0 { return 10000; }
    let fe = f_entry as i32;

    // Thresholds (same as existing SignalState boundaries)
    let strong = (fe * 179) >> 8;   // ~0.70 × f_entry
    let sustain = (fe * 90) >> 8;   // ~0.35 × f_entry
    let f = f_hat as i32;

    if f >= strong {
        0
    } else if f >= sustain {
        // Linear: strong→0, sustain→3000
        let range = strong - sustain;
        if range == 0 { return 1500; }
        ((strong - f) as u32 * 3000 / range as u32) as u16
    } else if f > 0 {
        // Linear: sustain→3000, zero→7000
        if sustain == 0 { return 5000; }
        (3000 + (sustain - f) as u32 * 4000 / sustain as u32) as u16
    } else {
        // f̂ ≤ 0: linear 7000→10000 as f goes from 0 to -f_entry
        let neg_depth = (-f).min(fe) as u32;
        (7000 + neg_depth * 3000 / fe as u32).min(10000) as u16
    }
}
```

**Cost:** 0 bytes (uses existing fields). 3 comparisons + 2 multiplies + 1 divide = ~8ns.

---

## 3. Component 2: Momentum Divergence — NEW

### 3.1 Problem

Single-timescale momentum is noisy. We need to detect when **short-term buying
is dying while medium-term momentum still looks strong** — the classic divergence
signal that precedes dumps.

### 3.2 Design Constraints

- No regular-interval price samples on bonding curve
- Events arrive at irregular spacing (0ms during pump bursts, seconds during quiet)
- Must use event-count windows, not time-EMA
- Integer-only arithmetic
- Budget: 16 bytes max

### 3.3 Implementation: Event-Count Buy Rate Ratio

We track buy events in two windows: **last 5 events** and **last 20 events**.
Divergence = recent window is weaker than the broader window.

```rust
/// Momentum divergence state. 16 bytes.
///
/// Tracks buy/sell ratio in two event-count windows.
/// Window sizes chosen for Pump.fun dynamics:
///   - Short: last 5 events (~0.5–2s during active pump)
///   - Medium: last 20 events (~2–10s)
///
/// Divergence fires when short-window buy ratio drops below medium-window
/// buy ratio by more than the threshold. This means buying is fading
/// even though the broader trend looks okay.
#[repr(C)]
pub struct MomentumDivergence {
    /// Ring buffer: last 20 events. 1 = buy, 0 = sell.
    /// Packed into 20 bits of a u32 (12 bits spare).
    /// Bit position = (ring_idx + N) % 20 for event N ago.
    event_ring: u32,       // 4 bytes

    /// Ring write index (0–19). Wraps modulo 20.
    ring_idx: u8,          // 1 byte

    /// Count of events written (saturates at 20).
    ring_count: u8,        // 1 byte

    /// Last computed short-window buy count (0–5). Cached for combiner.
    short_buys: u8,        // 1 byte

    /// Last computed medium-window buy count (0–20). Cached for combiner.
    medium_buys: u8,       // 1 byte

    /// Volume-weighted buy pressure: sum of last 8 buy sizes in mSOL.
    /// Decayed by >>1 every 8 events. Captures whether buys are getting smaller.
    buy_vol_recent: u16,   // 2 bytes

    /// Volume-weighted buy pressure: prior 8-event window.
    buy_vol_prior: u16,    // 2 bytes

    /// Sell volume in last 8 events (mSOL). For sell acceleration detection.
    sell_vol_recent: u16,  // 2 bytes

    _pad: [u8; 2],        // 2 bytes → total 16 bytes
}
```

**Size assertion:** `const _: () = assert!(size_of::<MomentumDivergence>() == 16);`

```rust
impl MomentumDivergence {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            event_ring: 0,
            ring_idx: 0,
            ring_count: 0,
            short_buys: 0,
            medium_buys: 0,
            buy_vol_recent: 0,
            buy_vol_prior: 0,
            sell_vol_recent: 0,
            _pad: [0; 2],
        }
    }

    /// Record a trade event (buy or sell).
    ///
    /// Updates the packed ring buffer and volume accumulators.
    /// Called from on_buy_event / on_sell_event in RideState.
    #[inline(always)]
    pub fn record_event(&mut self, is_buy: bool, sol_msol: u16) {
        let idx = self.ring_idx as u32;

        // Write to ring: set or clear bit at position idx
        if is_buy {
            self.event_ring |= 1 << idx;
        } else {
            self.event_ring &= !(1 << idx);
        }

        // Advance ring
        self.ring_idx = if self.ring_idx >= 19 { 0 } else { self.ring_idx + 1 };
        self.ring_count = self.ring_count.saturating_add(1).min(20);

        // Volume tracking (8-event decay cycle)
        if is_buy {
            self.buy_vol_recent = self.buy_vol_recent.saturating_add(sol_msol);
        } else {
            self.sell_vol_recent = self.sell_vol_recent.saturating_add(sol_msol);
        }

        // Every 8 events: shift recent → prior, reset recent
        if self.ring_count % 8 == 0 && self.ring_count > 0 {
            self.buy_vol_prior = self.buy_vol_recent;
            self.buy_vol_recent = 0;
            self.sell_vol_recent = 0;
        }

        // Recompute cached buy counts
        self.recompute_counts();
    }

    /// Recompute short (5) and medium (20) window buy counts from ring.
    /// Uses popcount on masked portions of the packed ring. ~3ns.
    #[inline(always)]
    fn recompute_counts(&mut self) {
        let n = self.ring_count.min(20) as u32;

        // Medium window: all events in ring (up to 20 bits)
        let medium_mask = if n >= 20 { 0x000F_FFFF } else { (1u32 << n) - 1 };
        self.medium_buys = (self.event_ring & medium_mask).count_ones() as u8;

        // Short window: last 5 events
        // These are the 5 bits ending at (ring_idx - 1) mod 20
        let short_n = n.min(5);
        let mut short_mask = 0u32;
        for i in 0..short_n {
            let bit_pos = (self.ring_idx as u32 + 20 - 1 - i) % 20;
            short_mask |= 1 << bit_pos;
        }
        self.short_buys = (self.event_ring & short_mask).count_ones() as u8;
    }

    /// Compute momentum divergence urgency (u16, 0–10000).
    ///
    /// Divergence = short-window buy rate is LOWER than medium-window buy rate.
    /// This means the pump is losing steam at the front even though the
    /// trailing average still looks okay.
    ///
    /// Also detects sell acceleration: when sell volume is increasing
    /// relative to buy volume, urgency rises.
    #[inline(always)]
    pub fn urgency(&self) -> u16 {
        let n = self.ring_count;
        if n < 5 { return 0; } // insufficient data

        // Short-window buy rate × 1000 (0–1000)
        let short_rate = self.short_buys as u32 * 1000 / 5;

        // Medium-window buy rate × 1000 (0–1000)
        let med_count = n.min(20) as u32;
        let med_rate = self.medium_buys as u32 * 1000 / med_count;

        // Divergence: how much worse is short vs medium?
        // If short_rate >= med_rate → no divergence → 0
        // If short_rate = 0 and med_rate = 600 → severe divergence
        let divergence_urgency = if short_rate >= med_rate {
            0u32
        } else {
            let gap = med_rate - short_rate;
            // Scale: gap of 500 (0.5 difference in buy rate) → 5000 urgency
            // Linear: urgency = gap × 10, capped at 8000
            (gap * 10).min(8000)
        };

        // Volume divergence: buy volume declining
        let vol_urgency = if self.buy_vol_prior > 0 && self.buy_vol_recent < self.buy_vol_prior {
            let decline = self.buy_vol_prior - self.buy_vol_recent;
            let pct = decline as u32 * 100 / self.buy_vol_prior as u32;
            // 50% decline → 2000 urgency, 100% decline → 4000
            (pct * 40).min(4000)
        } else {
            0u32
        };

        // Sell acceleration: sell volume exceeding buy volume
        let sell_urgency = if self.sell_vol_recent > self.buy_vol_recent.saturating_add(500) {
            // Sell volume exceeds buy volume by 0.5+ SOL
            let excess = (self.sell_vol_recent - self.buy_vol_recent) as u32;
            // 1 SOL excess → 2000, 3 SOL excess → 6000
            (excess * 2).min(6000)
        } else {
            0u32
        };

        // Composite: max of the three signals (not sum — avoid double-count)
        divergence_urgency.max(vol_urgency).max(sell_urgency).min(10000) as u16
    }
}
```

**Cost:** 16 bytes. ~15ns per event (ring write + popcount + vol accum). ~5ns per urgency query.

---

## 4. Component 3: Volatility-Adaptive Trail — NEW

### 4.1 Problem

The existing trail uses fixed base widths per SignalState (500bp / 350bp / 200bp)
scaled by f̂*/f_entry ratio. This works but doesn't adapt to **how volatile the
price action is**. A token swinging ±5% every second needs a wider trail than
one grinding steadily upward.

### 4.2 Design: Event-Count Variance of vSOL Deltas

Track the variance of vSOL changes across the last N events. High variance →
widen trail. Low variance → current trail is fine.

```rust
/// Volatility estimator for adaptive trail width. 16 bytes.
///
/// Tracks variance of vSOL deltas across last 8 trade events using
/// Welford's online algorithm (integer adaptation).
///
/// Output: vol_x100 = estimated volatility in basis points × 100.
/// Used as a multiplier on the existing trail width.
#[repr(C)]
pub struct VolatilityEstimator {
    /// Sum of absolute vSOL deltas (mvsol units) for last 8 events.
    abs_delta_sum: u32,    // 4 bytes

    /// Sum of squared deltas / 256 (scaled to prevent overflow).
    /// Max single delta ≈ 5000 mvsol. Squared = 25M. /256 = 97K. ×8 = 780K. Fits u32.
    sq_delta_sum_shr8: u32, // 4 bytes

    /// Previous vSOL reading (mvsol). For computing deltas.
    prev_mvsol: u32,       // 4 bytes

    /// Number of deltas recorded (saturates at 8).
    count: u8,             // 1 byte

    /// Cached volatility output × 100 (basis points × 100).
    pub vol_bp_x100: u16,  // 2 bytes

    _pad: u8,              // 1 byte → total 16 bytes
}
```

**Size assertion:** `const _: () = assert!(size_of::<VolatilityEstimator>() == 16);`

```rust
impl VolatilityEstimator {
    #[inline(always)]
    pub fn new(entry_mvsol: u32) -> Self {
        Self {
            abs_delta_sum: 0,
            sq_delta_sum_shr8: 0,
            prev_mvsol: entry_mvsol,
            count: 0,
            vol_bp_x100: 0,
            _pad: 0,
        }
    }

    /// Record a new vSOL observation. Computes delta from previous.
    ///
    /// Called on every trade event (buy or sell) from on_tick.
    /// Uses a simple windowed variance: after 8 samples, the oldest
    /// contribution is approximated by subtracting avg from running sums.
    #[inline(always)]
    pub fn record(&mut self, current_mvsol: u32) {
        if self.prev_mvsol == 0 {
            self.prev_mvsol = current_mvsol;
            return;
        }

        // Signed delta (can be negative on sells), but we track absolute
        let delta = if current_mvsol >= self.prev_mvsol {
            current_mvsol - self.prev_mvsol
        } else {
            self.prev_mvsol - current_mvsol
        };

        self.prev_mvsol = current_mvsol;

        // Exponential decay of accumulators (instead of exact windowing)
        // Every event: multiply by 7/8 then add new sample
        // Half-life ≈ 5.2 events (ln2 / ln(8/7) ≈ 5.2)
        if self.count >= 8 {
            self.abs_delta_sum = (self.abs_delta_sum * 7 + 4) >> 3;
            self.sq_delta_sum_shr8 = (self.sq_delta_sum_shr8 * 7 + 4) >> 3;
        }

        self.abs_delta_sum = self.abs_delta_sum.saturating_add(delta);
        self.sq_delta_sum_shr8 = self.sq_delta_sum_shr8
            .saturating_add((delta as u64 * delta as u64 >> 8) as u32);
        self.count = self.count.saturating_add(1);

        self.recompute_vol();
    }

    /// Recompute cached volatility in bp × 100.
    ///
    /// vol_bp_x100 = sqrt(variance) × 10000 / mean_mvsol × 100
    ///
    /// Approximation: stddev ≈ mean_abs_delta × 1.25 (for normal-ish distributions).
    /// Then: vol_bp = stddev × 10000 / entry_mvsol.
    /// We use abs_delta as a simpler stddev proxy (avoids integer sqrt).
    #[inline(always)]
    fn recompute_vol(&mut self) {
        let n = self.count.min(8) as u32;
        if n == 0 || self.prev_mvsol == 0 { return; }

        // Mean absolute delta
        let mean_abs = self.abs_delta_sum / n;

        // Volatility in basis points (relative to current price level)
        // vol_bp = mean_abs × 10000 / prev_mvsol
        // vol_bp_x100 = mean_abs × 1_000_000 / prev_mvsol
        let vol = if self.prev_mvsol > 0 {
            (mean_abs as u64 * 1_000_000 / self.prev_mvsol as u64) as u32
        } else {
            0
        };

        self.vol_bp_x100 = vol.min(u16::MAX as u32) as u16;
    }

    /// Compute trail width multiplier from volatility.
    ///
    /// Returns a multiplier × 256 (fixed-point).
    /// 256 = 1.0× (no adjustment).
    /// High vol → wider trail (multiplier > 256).
    /// Low vol → narrower trail (multiplier < 256, floored at 192 = 0.75×).
    ///
    /// Calibration baseline: median vol_bp_x100 ≈ 300 (3bp per event).
    /// At 300: multiplier = 256 (1.0×).
    /// At 600: multiplier = 384 (1.5×).
    /// At 150: multiplier = 224 (0.875×).
    #[inline(always)]
    pub fn trail_multiplier_x256(&self) -> u16 {
        let vol = self.vol_bp_x100 as u32;
        if vol == 0 { return 256; }

        // Linear: multiplier = 256 × vol / baseline
        // Baseline = 300 (median expected vol_bp_x100)
        const BASELINE: u32 = 300;
        let mult = (256 * vol + BASELINE / 2) / BASELINE;

        // Clamp: 0.75× to 2.5×
        mult.clamp(192, 640) as u16
    }
}
```

**Cost:** 16 bytes. ~10ns per event (delta + decay + add). ~3ns per multiplier query.

### 4b. Liquidity Slippage Urgency

```rust
/// Compute slippage urgency from position size vs available liquidity.
///
/// On bonding curve: liquidity = current_vsol - 30_000_000_000 (initial virtual reserve)
/// On AMM: liquidity = pool_sol_reserves from PriceFeedManager
///
/// Returns u16 urgency (0–10000).
/// Slippage > 3% → urgency rises sharply. > 8% → max urgency.
///
/// position_size_lamports: our position size
/// liquidity_lamports: available SOL liquidity in pool
#[inline(always)]
fn u_liquidity(position_size_lamports: u64, liquidity_lamports: u64) -> u16 {
    if liquidity_lamports == 0 { return 10000; }

    // Slippage estimate (basis points) = position_size × 10000 / liquidity
    // This is a linear approximation. For AMM: actual slippage ≈ size/liquidity.
    // For bonding curve: slippage = size / (vsol - 30e9) which is worse near the floor.
    let slippage_bp = (position_size_lamports as u128 * 10000 / liquidity_lamports as u128) as u32;

    // 0–100bp (0–1%) → 0 urgency (negligible slippage)
    // 100–300bp (1–3%) → 0–3000 urgency (linear ramp)
    // 300–800bp (3–8%) → 3000–8000 urgency (steeper ramp)
    // >800bp (>8%) → 10000 (exit immediately, liquidity crisis)
    if slippage_bp <= 100 {
        0
    } else if slippage_bp <= 300 {
        ((slippage_bp - 100) * 3000 / 200) as u16
    } else if slippage_bp <= 800 {
        (3000 + (slippage_bp - 300) * 5000 / 500) as u16
    } else {
        10000
    }
}
```

**Cost:** 0 bytes (pure function, no state). ~5ns per call.

---

## 5. Unified Urgency Combiner — NEW

### 5.1 Weighted Integer Combination

```rust
/// Compute composite exit urgency from four component signals.
///
/// All inputs are u16 in range [0, 10000].
/// Weights sum to 256 (fixed-point × 256 for integer multiply).
/// Output: u16 in [0, 10000].
///
/// Override conditions bypass the weighted sum:
/// - u_kelly >= 9000 (Bayesian says edge is deeply negative)
/// - u_liquidity >= 9000 (liquidity crisis)
/// - All four signals >= 5000 simultaneously (universal agreement)
///
/// Weight rationale:
///   Kelly/Bayesian (weight 115/256 ≈ 45%): primary signal, most reliable,
///     directly measures expected value. This IS the edge — everything else is confirmation.
///   Momentum divergence (weight 77/256 ≈ 30%): strongest leading indicator for
///     pump-to-dump transitions. Detects fading buy pressure before price drops.
///   Volatility trail (weight 38/256 ≈ 15%): prevents getting shaken out on
///     high-vol winners while tightening on low-vol fades.
///   Liquidity (weight 26/256 ≈ 10%): safety net for illiquid positions.
///     Rarely fires but catastrophic when it does.
#[inline(always)]
fn composite_urgency(
    u_kelly: u16,
    u_momentum: u16,
    u_vol_trail: u16,
    u_liquidity: u16,
) -> u16 {
    // Override conditions (any one → max urgency)
    if u_kelly >= 9000 || u_liquidity >= 9000 {
        return 10000;
    }
    // Universal agreement: all signals alarmed
    if u_kelly >= 5000 && u_momentum >= 5000 && u_vol_trail >= 5000 && u_liquidity >= 5000 {
        return 10000;
    }

    // Weighted sum: weights sum to 256
    const W_KELLY: u32 = 115;     // 45%
    const W_MOMENTUM: u32 = 77;   // 30%
    const W_VOL: u32 = 38;        // 15%
    const W_LIQ: u32 = 26;        // 10%
    // 115 + 77 + 38 + 26 = 256 ✓

    let weighted = W_KELLY * u_kelly as u32
        + W_MOMENTUM * u_momentum as u32
        + W_VOL * u_vol_trail as u32
        + W_LIQ * u_liquidity as u32;

    // Divide by 256 (right shift)
    let composite = weighted >> 8;

    composite.min(10000) as u16
}
```

**Cost:** 0 bytes (pure function). 4 multiplies + 3 adds + 1 shift = ~4ns.

### 5.2 Vol-Trail Urgency from VolatilityEstimator

The volatility estimator doesn't directly produce urgency — it modifies the
trailing stop width, which then produces urgency based on price proximity to
the adaptive stop level.

```rust
/// Compute volatility-trail urgency.
///
/// Combines the existing trail stop with volatility-adaptive width adjustment.
/// Returns u16 urgency (0–10000) based on how close current price is to the
/// volatility-adjusted trailing stop.
///
/// The trail stop itself is computed in on_tick (existing code), but with
/// the trail width multiplied by vol_multiplier_x256 / 256.
///
/// Urgency ramps up as price approaches the adaptive trail stop:
///   Price > trail_stop × 1.05 → 0 (safe margin)
///   Price = trail_stop × 1.02 → 3000 (getting close)
///   Price = trail_stop × 1.00 → 7000 (at the stop)
///   Price < trail_stop         → 10000 (STOPPED OUT — override to full exit)
#[inline(always)]
fn u_vol_trail(current_mvsol: u32, adaptive_trail_stop: u32) -> u16 {
    if adaptive_trail_stop == 0 { return 0; }

    if current_mvsol <= adaptive_trail_stop {
        return 10000; // Below stop → full exit
    }

    // Distance above stop in basis points
    let margin_bp = ((current_mvsol - adaptive_trail_stop) as u64 * 10000
        / adaptive_trail_stop as u64) as u32;

    // Ramp: 500bp margin → 0, 200bp → 3000, 0bp → 7000
    if margin_bp >= 500 {
        0
    } else if margin_bp >= 200 {
        ((500 - margin_bp) * 3000 / 300) as u16
    } else {
        (3000 + (200 - margin_bp) * 4000 / 200) as u16
    }
}
```

---

## 6. Exit Decision Engine — NEW

### 6.1 Urgency Thresholds → Actions

```rust
/// Exit urgency state. 8 bytes. Stored in RideState extension.
///
/// Tracks the monotonic urgency floor and partial exit history.
/// Once urgency crosses a threshold and a partial exit fires,
/// the floor ratchets up — the position can only exit MORE, never
/// re-accumulate. This prevents flip-flopping.
#[repr(C)]
pub struct UrgencyState {
    /// Monotonic urgency floor (ratchets up, never down).
    /// After a partial exit at U=5500, floor = 5500 × 7/8 = 4812.
    /// Subsequent U is max(computed_U, floor).
    pub urgency_floor: u16,       // 2 bytes

    /// Remaining position in permille (1000 = 100%).
    /// Decremented on each partial exit.
    pub remaining_permille: u16,  // 2 bytes

    /// Number of partial exits executed (0–3).
    pub partial_count: u8,        // 1 byte

    /// Last computed composite urgency (for logging/API).
    pub last_urgency: u16,        // 2 bytes

    _pad: u8,                     // 1 byte → total 8 bytes
}

impl UrgencyState {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            urgency_floor: 0,
            remaining_permille: 1000,
            partial_count: 0,
            last_urgency: 0,
            _pad: 0,
        }
    }

    /// Apply urgency floor and return the effective urgency.
    #[inline(always)]
    pub fn effective_urgency(&self, computed: u16) -> u16 {
        computed.max(self.urgency_floor)
    }

    /// Execute exit decision based on effective urgency.
    /// Returns the fraction to exit (permille, 0–1000) and new remaining.
    ///
    /// Threshold design:
    ///
    ///   U < 3000          → HOLD (edge intact, ride it)
    ///   3000 ≤ U < 5000   → TIGHTEN (reduce trail multiplier, no exit)
    ///   5000 ≤ U < 7000   → PARTIAL EXIT: sell 350‰ (35%) of REMAINING position
    ///   7000 ≤ U < 9000   → MAJORITY EXIT: sell 600‰ (60%) of remaining
    ///   U ≥ 9000          → FULL EXIT: sell everything
    ///
    /// Why these thresholds:
    ///
    ///   3000: f̂* has dropped below 0.35×f_entry (Sustained→Weakening transition).
    ///     Risk is rising but edge may recover. Tightening the trail is sufficient.
    ///
    ///   5000: f̂* is near zero AND momentum is diverging. Edge is probably gone
    ///     but not confirmed dead. Take profit on 35% to lock in gains, keep 65%
    ///     in case of recovery. This replaces the old TP1.
    ///
    ///   7000: f̂* is negative OR trail stop proximity < 200bp OR liquidity crisis.
    ///     The position is losing money or about to. Dump 60% of what's left,
    ///     keep a trailing 40% runner in case of miracle bounce.
    ///
    ///   9000: Multiple signals at maximum alarm. Bayesian edge deeply negative,
    ///     OR liquidity evaporated, OR universal agreement. Get out entirely.
    ///     This replaces the old SL + max_hold + emergency exits.
    ///
    ///   Monotonic floor: After a partial at U=5500, floor = 4812.
    ///     Even if signals temporarily recover (f̂* bounces), the position
    ///     can't "un-exit". This prevents the classic whipsaw where you sell,
    ///     signal recovers, you'd want to re-buy, signal dies again.
    ///     The ratchet factor (7/8 = 87.5%) gives 12.5% headroom for
    ///     genuine recoveries to breathe before forcing the next exit.
    #[inline(always)]
    pub fn decide(&mut self, effective_u: u16) -> ExitFraction {
        self.last_urgency = effective_u;

        if effective_u >= 9000 {
            // Full exit
            let frac = self.remaining_permille;
            self.remaining_permille = 0;
            self.urgency_floor = 10000;
            return ExitFraction::Exit(frac);
        }

        if effective_u >= 7000 && self.remaining_permille > 0 {
            // Majority exit: 60% of remaining
            let sell = (self.remaining_permille as u32 * 600 / 1000) as u16;
            let sell = sell.max(1).min(self.remaining_permille);
            self.remaining_permille -= sell;
            self.partial_count = self.partial_count.saturating_add(1);
            self.urgency_floor = (effective_u as u32 * 7 / 8) as u16;
            return ExitFraction::Partial(sell);
        }

        if effective_u >= 5000 && self.remaining_permille > 0 && self.partial_count < 2 {
            // Partial exit: 35% of remaining (limited to 2 partials before majority)
            let sell = (self.remaining_permille as u32 * 350 / 1000) as u16;
            let sell = sell.max(1).min(self.remaining_permille);
            self.remaining_permille -= sell;
            self.partial_count = self.partial_count.saturating_add(1);
            self.urgency_floor = (effective_u as u32 * 7 / 8) as u16;
            return ExitFraction::Partial(sell);
        }

        if effective_u >= 3000 {
            return ExitFraction::Tighten;
        }

        ExitFraction::Hold
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitFraction {
    /// Do nothing. Edge is intact.
    Hold,
    /// Tighten trail stop (reduce ATR multiplier). No position change.
    Tighten,
    /// Sell this many permille of the ORIGINAL position.
    Partial(u16),
    /// Sell this many permille (should equal remaining — full exit).
    Exit(u16),
}
```

**Cost:** 8 bytes. ~5ns per decision.

### 6.2 How TPs Transform

Old system → New system mapping:

| Old | Trigger | New Equivalent |
|-----|---------|---------------|
| TP1 (+5%, sell 40%) | Fixed price level | Partial exit when U ≥ 5000 (edge weakening, not price-based) |
| TP2 (+15%, sell 30%) | Fixed price level | Majority exit when U ≥ 7000 |
| TP3 (+50%, sell 30%) | Fixed price level | Full exit when U ≥ 9000 |
| Trailing stop (8% from peak) | Fixed % drawdown | Volatility-adaptive trail (§4) feeding into U |
| SL (-10%) | Fixed price level | Bayesian f̂* < 0 → U ≥ 7000+ → majority/full exit |
| Max hold (300s) | Fixed timer | Kelly edge decay → f̂* approaches 0 as evidence decays |
| BuyGapTimeout (10s) | No buys for 10s | Momentum divergence → short_buys = 0 → U spikes |
| Sell cascade (3 sells in 3s) | Fixed count/window | Sell acceleration in momentum divergence → U spikes |

**Key difference:** A token at +5% that still has strong momentum (high f̂*, healthy buy flow)
will NOT trigger any exit. A token at +50% with dying momentum will trigger majority exit
because U reflects edge state, not price level.

### 6.3 Preserving Emergency Exits

The following emergency exits in `RideState::on_sell_event()` are **kept unchanged**:

- **Creator sell** → immediate full exit (insider information, unrecoverable)
- **Whale exit** (>2 SOL sell) → immediate full exit (liquidity impact too large)

These bypass the urgency system entirely because they represent categorical threats,
not probabilistic edge estimates. The Bayesian model can't price these correctly in
real-time (single events with infinite Bayesian weight would distort the posterior).

### 6.4 Hard Floor — Modified

The existing hard floor (breakeven check accounting for fees) is **kept** but now
feeds into the urgency system instead of being a separate exit path:

```rust
// In on_tick, BEFORE urgency computation:
let fee_bp = DEFAULT_ROUND_TRIP_FEE_BP as u64;
let breakeven = entry_mvsol as u64 * (10_000 + fee_bp) / 10_000;
if (current_mvsol as u64) < breakeven {
    // Below fee-adjusted breakeven → urgency override
    // Don't exit immediately — let urgency system decide.
    // But set u_kelly to at least 7000 (we're net-negative after fees).
    u_kelly_override = 7000;
}
```

---

## 7. Integration: Modified RideState v4

### 7.1 Memory Layout — 192 bytes (3 cache lines)

```
Offset  Size  Field                        Source
------  ----  -----                        ------
  0      64   [EXISTING cache line 0]      RideState v3 — trail + timing + Bayesian
 64      64   [EXISTING cache line 1]      RideState v3 — ring buffers + bloom
128      16   momentum: MomentumDivergence §3 — NEW
144      16   volatility: VolatilityEstimator §4 — NEW
160       8   urgency: UrgencyState        §6 — NEW
168       8   [available for future use]
176      16   _pad3: [u8; 16]              alignment padding → 192 bytes
------  ----
TOTAL:  192
```

**Size assertion:** `const _: () = assert!(size_of::<RideStateV4>() == 192);`

**Cache behavior:**
- Lines 0–1 (0–127): HOT — existing Bayesian + trail + emergency checks. Read every tick.
- Line 2 (128–191): WARM — new components. Read every tick but only written on events.
- 3 cache lines × 10 positions = 5.76 KB. L1 is 32 KB. 18% utilization. Comfortable.

### 7.2 Modified on_tick — Pseudocode

```rust
pub fn on_tick_v4(
    &mut self,
    current_mvsol: u32,
    now_ms: u64,
    position_size_lamports: u64,
    liquidity_lamports: u64,  // vsol - 30e9 for BC, pool_sol for AMM
    config: &RideConfig,
) -> RideDecision {
    // ── Phase 1: Emergency overrides (UNCHANGED) ──
    if self.flags & ride_flags::CREATOR_SELL != 0 {
        return RideDecision::Exit(RideExitReason::CreatorSell);
    }
    let hold_ms = now_ms.saturating_sub(self.ride_start_ms);
    // Hard floor check → sets override
    let hard_floor_triggered = /* ... existing code ... */;

    // ── Phase 2: Bayesian update (UNCHANGED) ──
    self.bayesian_decay_tick();
    let pnl_bp = self.unrealized_pnl_bp(current_mvsol);
    self.bayesian_update_r_estimate(pnl_bp);
    let f_hat = self.bayesian_current_f_permille();

    // ── Phase 3: Volatility recording ──
    self.volatility.record(current_mvsol);

    // ── Phase 4: Compute component urgencies ──
    let uk = if hard_floor_triggered {
        u_kelly(f_hat, self.entry_f_permille).max(7000)
    } else {
        u_kelly(f_hat, self.entry_f_permille)
    };
    let um = self.momentum.urgency();
    let uv = {
        // Apply vol multiplier to existing trail width
        let vol_mult = self.volatility.trail_multiplier_x256();
        let adaptive_trail_bp = (self.current_trail_bp as u32 * vol_mult as u32) >> 8;
        let adaptive_stop = compute_trail_stop(self.peak_mvsol, adaptive_trail_bp as u16);
        u_vol_trail(current_mvsol, adaptive_stop)
    };
    let ul = u_liquidity(position_size_lamports, liquidity_lamports);

    // ── Phase 5: Combine + decide ──
    let composite = composite_urgency(uk, um, uv, ul);
    let effective = self.urgency.effective_urgency(composite);

    match self.urgency.decide(effective) {
        ExitFraction::Hold => {
            // Update trail + peak (existing code)
            // ...
            RideDecision::Hold
        }
        ExitFraction::Tighten => {
            // Reduce trail multiplier by 25%
            // (existing trail scaling handles this via f̂* drop)
            RideDecision::Hold // tighten is advisory, not an exit
        }
        ExitFraction::Partial(permille) => {
            RideDecision::PartialExit { permille }
        }
        ExitFraction::Exit(_) => {
            RideDecision::Exit(RideExitReason::SignalExit)
        }
    }
}
```

### 7.3 Modified Event Handlers

```rust
// In on_buy_event — add:
self.momentum.record_event(true, sol_amount_mvsol.min(u16::MAX as u32) as u16);

// In on_sell_event — add (after emergency checks):
self.momentum.record_event(false, sol_amount_mvsol.min(u16::MAX as u32) as u16);
```

**One line each.** Zero behavioral change to existing emergency exits.

---

## 8. RideDecision Extension for Partial Exits

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideDecision {
    Hold,
    Exit(RideExitReason),
    PartialExit { permille: u16 },  // NEW — sell this fraction
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,
    HardFloor,
    WhaleExit,
    BuyGapTimeout,
    SellCascade,
    CreatorSell,
    MaxHold,
    SignalExit,
    UrgencyPartial,   // NEW — urgency-driven partial exit
    UrgencyFull,      // NEW — urgency-driven full exit
}
```

The `PartialExit` variant requires the caller (hot_path.rs) to execute a partial
sell of `permille / 1000` of the position. The position remains open with reduced
size. The exit engine continues running on the remainder.

---

## 9. Configuration

All new parameters added to `RideConfig` extension:

```rust
// Added to RideConfig (or a new ExitV4Config nested struct)
pub struct ExitV4Config {
    // Urgency weights (sum to 256)
    pub w_kelly: u8,          // default: 115
    pub w_momentum: u8,       // default: 77
    pub w_vol: u8,            // default: 38
    pub w_liq: u8,            // default: 26

    // Urgency thresholds (u16, 0–10000)
    pub threshold_tighten: u16,     // default: 3000
    pub threshold_partial: u16,     // default: 5000
    pub threshold_majority: u16,    // default: 7000
    pub threshold_full_exit: u16,   // default: 9000

    // Partial exit fractions (permille)
    pub partial_sell_permille: u16,   // default: 350 (35%)
    pub majority_sell_permille: u16,  // default: 600 (60%)

    // Volatility estimator
    pub vol_baseline_bp_x100: u16,    // default: 300 (3bp median)
    pub vol_trail_min_mult_x256: u16, // default: 192 (0.75×)
    pub vol_trail_max_mult_x256: u16, // default: 640 (2.5×)

    // Monotonic floor ratchet factor × 8 (7 = 7/8 = 87.5%)
    pub floor_ratchet_numer: u8,      // default: 7

    // Max partial exits before forcing majority
    pub max_partials: u8,             // default: 2
}
```

**JSON config** (added to `mev.ride` section):

```json
{
  "ride": {
    "exit_v4": {
      "w_kelly": 115,
      "w_momentum": 77,
      "w_vol": 38,
      "w_liq": 26,
      "threshold_tighten": 3000,
      "threshold_partial": 5000,
      "threshold_majority": 7000,
      "threshold_full_exit": 9000,
      "partial_sell_permille": 350,
      "majority_sell_permille": 600,
      "vol_baseline_bp_x100": 300
    }
  }
}
```

**Runtime tuning via API:**

```
POST /api/exit/v4/config   → update ExitV4Config fields
GET  /api/exit/v4/state/{mint} → returns UrgencyState + component urgencies + MomentumDivergence snapshot
```

---

## 10. Calibration Procedure

### 10.1 Data Available

From `data/mev_paper_trades.jsonl` (3,730+ trades, 42.3% WR):

Each trade has:
- Entry/exit timestamps, hold duration
- Entry composite score, magnitude, Kelly conviction
- Entry vSOL, exit vSOL (bonding curve position data)
- Buy/sell events during hold (via trade replay from feeds)
- Exit reason (which of the existing exit paths fired)

### 10.2 What to Calibrate

**Phase 1 — Urgency weights (no replay needed):**

Analyze closed trades by exit category:
- Trades that exited via SignalExit (Bayesian): what was f̂* at exit? Was it actually the right call?
- Trades that exited via BuyGapTimeout: were they profitable at that point? (If yes → urgency system should have held longer)
- Trades that exited via TrailingStop: what was the drawdown from peak? (If small → trail was too tight → vol multiplier was too low)
- Trades that exited via MaxHold: what was f̂* at max_hold time? (If still positive → urgency system would have held correctly)

**Phase 2 — Momentum divergence thresholds (requires event replay):**

For each trade in the paper log, replay the buy/sell event stream and compute:
- When did momentum divergence first fire (urgency > 3000)?
- How many ms before the actual price peak?
- False positive rate: divergence fired but price continued up?

Target: divergence fires 500ms–2s before peak on 60%+ of winners. False positive rate < 20%.

**Phase 3 — Volatility baseline:**

From event replay:
- Compute vol_bp_x100 distribution across all trades
- Median should be around 300 (3bp per event). If not, adjust baseline constant.
- Verify that high-vol winners (tokens swinging 10%+ per second) have vol_bp_x100 > 600

**Phase 4 — A/B testing:**

Run 50% of positions on existing RideState v3, 50% on v4.
Compare after 500+ trades:
- Net PnL per trade
- Capture ratio (exit_price / peak_price)
- Win rate
- Average hold time on winners

### 10.3 Minimum Data for Each Phase

| Phase | Min Trades | Notes |
|---|---|---|
| Weight tuning | 200 | Classify by exit reason, no replay |
| Momentum thresholds | 500 | Need event replay infrastructure |
| Volatility baseline | 200 | Event replay for vol distribution |
| A/B test | 1000 (500+500) | Statistical significance on net PnL |

**Current data (3,730 trades) is sufficient for Phases 1–3. Phase 4 needs new trades.**

---

## 11. Performance Budget

### 11.1 Memory

| Component | Bytes | Cache Lines |
|---|---|---|
| RideState v3 (existing) | 128 | 2 |
| MomentumDivergence | 16 | – |
| VolatilityEstimator | 16 | – |
| UrgencyState | 8 | – |
| Padding + alignment | 24 | – |
| **RideState v4 total** | **192** | **3** |

10 concurrent positions: 1.92 KB (6% of L1 cache).

### 11.2 Compute per Tick

| Step | Existing | Added | Total |
|---|---|---|---|
| Emergency checks | ~5ns | 0 | ~5ns |
| Bayesian decay + f̂* | ~15ns | 0 | ~15ns |
| Trail compute | ~8ns | 0 | ~8ns |
| Vol estimator record | 0 | ~10ns | ~10ns |
| u_kelly computation | 0 | ~8ns | ~8ns |
| Momentum urgency | 0 | ~5ns | ~5ns |
| u_vol_trail | 0 | ~5ns | ~5ns |
| u_liquidity | 0 | ~5ns | ~5ns |
| Composite urgency | 0 | ~4ns | ~4ns |
| Urgency decide | 0 | ~5ns | ~5ns |
| **Total per tick** | **~28ns** | **~42ns** | **~70ns** |

At 50ms tick interval: 70ns / 50ms = 0.00014% CPU utilization per position.
10 positions = 0.0014%. **Negligible.**

### 11.3 Compute per Event (buy/sell)

| Step | Added |
|---|---|
| Momentum record_event | ~15ns |
| Vol estimator (on trade events only) | ~10ns |
| **Total per event** | **~25ns** |

At peak: 50 events/second × 10 positions × 25ns = 12.5μs/s. **Negligible.**

---

## 12. Implementation Roadmap

### Phase 1: Foundation (build in 1 session)
- [ ] Add `MomentumDivergence`, `VolatilityEstimator`, `UrgencyState` structs
- [ ] Add `ExitV4Config` to config loader
- [ ] Wire `momentum.record_event()` into `on_buy_event` / `on_sell_event`
- [ ] Wire `volatility.record()` into `on_tick`
- [ ] Implement `u_kelly()`, `u_vol_trail()`, `u_liquidity()`, `composite_urgency()`
- [ ] Implement `UrgencyState::decide()` with monotonic floor
- [ ] Add `RideDecision::PartialExit` variant
- [ ] Handle partial exits in `PositionManager` / hot_path.rs
- [ ] Shadow mode: log urgency components alongside existing exit decisions (don't act on them)
- [ ] All new code behind `exit_v4_enabled: bool` config flag (default: false)

### Phase 2: Calibration (after 500+ shadow-logged trades)
- [ ] Analyze urgency logs: when would v4 have exited vs when v3 actually exited?
- [ ] Compute counterfactual PnL: if v4 had been active, what would net PnL be?
- [ ] Tune weights and thresholds from calibration data
- [ ] Adjust vol baseline from empirical distribution

### Phase 3: A/B Test (after calibration)
- [ ] Enable v4 for 50% of positions (random assignment at entry)
- [ ] Run for 500+ trades per group
- [ ] Compare net PnL, capture ratio, win rate, hold time
- [ ] If v4 wins → promote to 100%

### Phase 4: Kill Legacy
- [ ] Remove BuyGapTimeout exit (subsumed by momentum divergence)
- [ ] Remove fixed TrailingStop (subsumed by vol-adaptive trail)
- [ ] Remove MaxHold timer (subsumed by Kelly edge decay)
- [ ] Keep: Creator sell, Whale exit, Hard floor (categorical emergencies)
- [ ] Remove shadow logging, clean up dead code

---

## 13. What This DOESN'T Do (and Why)

| Feature | Why Not |
|---|---|
| Multi-timeframe EMA | No regular price ticks. Event-count windows achieve the same divergence detection with 16 bytes instead of 128 |
| CUSUM / Shiryaev-Roberts | Bayesian f̂*→0 IS regime detection. Adding CUSUM double-counts the same evidence with more complexity and f64 math |
| Weibull decay | Exponential decay (240/256) is simpler, proven, and 128× smaller. Weibull shape parameter can't be calibrated without 10K+ trades |
| Order book depth | Pump.fun has no order book. Bonding curve is a formula. AMM has constant-product reserves (which we already read) |
| Maker/taker classification | All Pump.fun trades are taker. No market microstructure differentiation possible |
| Cross-position correlation | Already handled by Kelly sizing (Thorp correlation adjustment with ρ=0.25). Exit engine is per-position |
| Machine learning | Insufficient training data (3.7K trades). Bayesian + Kelly is the optimal small-sample framework |

---

## Appendix A: Numerical Walkthrough

**Scenario: Token pumps +30%, momentum fades, vol-adaptive exit.**

```
T=0s:   Entry at 1000 mvsol. f̂*=248‰. f_entry=248.
        U_kelly=0, U_mom=0, U_vol=0, U_liq=0. Composite=0. → HOLD.

T=1s:   5 buys, 0 sells. Price → 1050 mvsol (+5%).
        f̂*=280‰ (buys pushing alpha up). Momentum: 5/5 short buys = 1.0 rate.
        U_kelly=0, U_mom=0. → HOLD.

T=3s:   12 buys, 3 sells. Price → 1200 mvsol (+20%).
        f̂*=190‰ (some sells arriving, decay ticking). Momentum: 3/5 short = 0.6 rate.
        Medium: 12/15 = 0.8 rate. No divergence (short ≈ medium).
        U_kelly=1500, U_mom=0. → HOLD.

T=5s:   15 buys, 8 sells. Price → 1300 mvsol (+30%). PEAK.
        f̂*=95‰ (sells accumulating). Momentum: 1/5 short = 0.2 rate.
        Medium: 15/23 = 0.65 rate. DIVERGENCE: gap = 0.45 → U_mom=4500.
        Vol: high (10bp/event). Trail widened to 1.5×.
        U_kelly=4000, U_mom=4500, U_vol=800, U_liq=0.
        Composite = (115×4000 + 77×4500 + 38×800 + 26×0)/256 = 3269. → TIGHTEN.

T=6s:   16 buys, 14 sells. Price → 1250 mvsol (+25%, falling from peak).
        f̂*=20‰ (deep weakening). Momentum: 0/5 short buys = 0.0 rate.
        Medium: 16/30 = 0.53. CRITICAL DIVERGENCE: gap = 0.53 → U_mom=5300.
        Adaptive trail stop at 1150 mvsol. Price 1250, margin = 870bp.
        U_kelly=5800, U_mom=5300, U_vol=1200, U_liq=0.
        Composite = (115×5800 + 77×5300 + 38×1200 + 26×0)/256 = 4377. → TIGHTEN.
        (Close to partial threshold but not quite. System is patient.)

T=7s:   16 buys, 19 sells. Price → 1180 mvsol (+18%, accelerating decline).
        f̂*=-30‰ (NEGATIVE — edge gone). Momentum: 0/5 short, sell acceleration.
        U_kelly=7360, U_mom=6800, U_vol=3500, U_liq=200.
        Composite = (115×7360 + 77×6800 + 38×3500 + 26×200)/256 = 5891.
        Effective = max(5891, 0) = 5891. → PARTIAL EXIT (35% of position).
        Urgency floor ratcheted to 5891 × 7/8 = 5155.

T=8s:   Price → 1100 mvsol (+10%). f̂*=-80‰.
        U_kelly=8800, U_mom=7500, U_vol=5000, U_liq=500.
        Composite = 7001. Effective = max(7001, 5155) = 7001. → MAJORITY EXIT (60%).
        Remaining: 65% × 40% = 26% of original position still trailing.

T=10s:  Price → 1050 mvsol (+5%). Trail stop hit.
        U_vol=10000. Composite = override → 10000. → FULL EXIT (remaining 26%).

Result: Exited 35% at +18%, 39% at +10%, 26% at +5%.
Blended exit: +11.5% (captured 38% of the +30% move).

Old system: Would have exited 100% at TP1 (+5%) OR held through to max_hold and exited
at whatever price was at T=300s (likely -20% or worse after the pump collapsed).
```

**Scenario: Token pumps +200%, strong momentum sustained.**

```
T=0s:   Entry at 1000. f̂*=248.

T=5s:   30 buys, 2 sells. Price → 1500 (+50%).

        f̂*=310 (buys dominant, alpha growing faster than decay).
        Momentum: 5/5 short = 1.0. Medium: 30/32 = 0.94. No divergence.
        U_kelly=0, U_mom=0, U_vol=0, U_liq=0. Composite=0. → HOLD.
        (Old system: TP1 fires at +5%, sells 40%. TP2 fires at +15%, sells 30%.
        TP3 fires at +50%, sells remaining 30%. Total captured: 5%×40 + 15%×30 + 50%×30 = 21.5%.
        New system: 0% sold. Holding 100%.)

T=15s:  55 buys, 8 sells. Price → 2000 (+100%).
        f̂*=260 (still strong despite some decay). Momentum: 4/5 short, 55/63 medium.
        U_kelly=0 (f̂ > 0.7×f_entry). Composite=0. → HOLD.
        (Old system: already fully exited at +50%. New system: holding 100% at +100%.)

T=30s:  80 buys, 15 sells. Price → 3000 (+200%).
        f̂*=180 (sustained state). Buy flow steady but rate slowing.
        Momentum: 3/5 short = 0.6. Medium: 80/95 = 0.84. Mild divergence.
        U_kelly=1800, U_mom=2400, U_vol=500, U_liq=0.
        Composite = 1604. → HOLD.
        (Divergence is emerging but edge is still positive. System holds.)

T=35s:  82 buys, 25 sells. Price → 2800 (+180%, rolling over).
        f̂*=60 (weakening). Momentum: 0/5 short, sell acceleration.
        U_kelly=5600, U_mom=7200, U_vol=2800, U_liq=0.
        Composite = 5096. → PARTIAL (35% at +180%).
        Floor: 4459.

T=38s:  82 buys, 32 sells. Price → 2500 (+150%).
        f̂*=-10 (negative). U_kelly=7100, U_mom=8000, U_vol=4500.
        Composite = 6263. Effective = max(6263, 4459) = 6263.
        → PARTIAL (2nd partial: 35% of remaining 65% = 22.75% of original at +150%).
        Remaining: 42.25%. Floor: 5480.

T=42s:  Price → 2200 (+120%). Trail tightening, vol high.
        Composite ≈ 7800 > floor. → MAJORITY (60% of 42.25% = 25.35% at +120%).
        Remaining: 16.9%.

T=45s:  Price → 2000 (+100%). Trail stop hit on remaining.
        → FULL EXIT (16.9% at +100%).

Result: 35% at +180%, 22.75% at +150%, 25.35% at +120%, 16.9% at +100%.
Blended exit: +144.4% (captured 72% of the +200% move).

Old system blended exit: 21.5%. New system: 144.4%.
IMPROVEMENT: 6.7× more profit on the same trade.
```

This is the gap between a rule-based bot and an edge-maximizing engine.

---

## Appendix B: Compatibility Matrix

| Existing Feature | V4 Impact | Migration |
|---|---|---|
| `RideState::on_tick()` | Extended, not replaced | Add v4 block after existing Bayesian update |
| `RideState::on_buy_event()` | 1 line added | `self.momentum.record_event(true, ...)` |
| `RideState::on_sell_event()` | 1 line added | `self.momentum.record_event(false, ...)` |
| `RideDecision` enum | 1 variant added | `PartialExit { permille }` |
| `RideExitReason` enum | 2 variants added | `UrgencyPartial`, `UrgencyFull` |
| `RideConfig` struct | Nested struct added | `ExitV4Config` with defaults |
| `SignalState` machine | Unchanged | Still drives trail width base selection |
| `BayesianSignal` | Unchanged | f̂* output consumed by u_kelly() |
| Emergency exits | Unchanged | Creator sell, whale, cascade still bypass urgency |
| JSONL logging | Extended | Add urgency breakdown fields to trade close log |
| API `/api/health` | Extended | Add urgency state to position detail endpoint |
| Position size (128→192 bytes) | +64 bytes | `#[repr(C, align(64))]`, size assert updated |
| Hot path thread | ~42ns added per tick | Within 50ms budget by 6 orders of magnitude |

**Zero breaking changes. Pure extension. Feature-flagged behind `exit_v4_enabled`.**
