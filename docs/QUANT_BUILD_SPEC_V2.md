# QUANT BUILD SPEC V2 — Production-Ready Engineering Tasks
**Date**: April 1, 2026  
**Basis**: Exhaustive backtest of 4,958 paper trades, 856 with full graduation enrichment  
**Target**: 5 parallel Rust engineers, each task independent  
**Branch**: `feat/quant-v2-fixes` from current `main`

---

## Overview

The March 31 overhaul introduced scorer v2, probe-then-scale, ToD gating, and time-decay trailing stop. Backtesting reveals the primary WR bleed (13.6% → target 40%+) is caused by admitting fast-graduation whale/bot pump tokens. These five tasks address the root causes in priority order.

**Expected combined impact**: WR from 13.6% → 40-52% on enriched trades, with expectancy from 1.00 → 4-6 mSOL/trade.

---

## Task 1: Hard Gate — Whale Pump Rejection

**Files**: 
- `rust/pump-quant-core/src/momentum/mod.rs` (on_graduation handler)
- `rust/pump-quant-core/src/momentum/config.rs` (new config fields)

**Problem**: 527/856 enriched trades (61.6%) are fast-graduation saturated-volume tokens (speed=60s, vol≥655.35 SOL) with 5.9% WR. These represent bot/whale bonding curve fills where price is flat post-graduation. 280/322 dead-on-arrival trades (all-zero price samples) are in this category.

**Evidence**: 
- speed=60-90s: n=698, WR=7.3%, Exp=0.63 mSOL
- speed≥120s: n=158, WR=41.1%, Exp=2.66 mSOL
- vol≥655 (saturated): n=527, WR=5.9%
- vol<200: n=191, WR=38.2%
- Removing speed=60+vol≥655: WR jumps from 13.6% → 25.8%

**Solution**: Two-stage hard gate before scoring:

```
Stage A: Reject if grad_speed_s <= 90 AND grad_volume_sol >= 200
Stage B: Reject if grad_speed_s <= 60 (regardless of volume)
```

This gate fires BEFORE the scorer, eliminating 70%+ of worthless trades at near-zero cost.

**Implementation**:

In `config.rs`, add to `MomentumConfig`:
```rust
/// Minimum graduation speed (seconds) to accept. Faster = rejected.
/// Tokens graduating in ≤ this many seconds are bot/whale fills.
/// Default: 90 (rejects speed=60s which has 7.3% WR).
pub min_grad_speed_s: u32,

/// Maximum graduation volume (SOL) to accept for fast grads.
/// When speed < min_grad_speed_s * 2, also check volume.
/// Default: 200 (vol≥200 with fast speed has 9.2% WR).
pub max_grad_volume_sol_fast: f64,

/// Hard ceiling on grad_volume_sol regardless of speed.
/// Saturated u16 volume (655.35) indicates unmeasurable whale fills.
/// Default: 650.0 (catches the 655.35 saturation value).
pub max_grad_volume_sol_absolute: f64,
```

In `mod.rs` `on_graduation()`, add before `score_graduation()` call:
```rust
// HARD GATE: Reject whale pump tokens
if grad_speed_s <= cfg.min_grad_speed_s {
    // Fast graduation — always reject unless very low volume
    tracing::debug!(mint=%mint_str, speed=grad_speed_s, "rejected: fast grad");
    self.stats_rejected.fetch_add(1, Ordering::Relaxed);
    return;
}
if grad_volume_sol >= cfg.max_grad_volume_sol_absolute {
    // Saturated volume — whale/bot fill
    tracing::debug!(mint=%mint_str, vol=grad_volume_sol, "rejected: saturated volume");
    self.stats_rejected.fetch_add(1, Ordering::Relaxed);
    return;
}
if grad_speed_s <= cfg.min_grad_speed_s * 2 && grad_volume_sol >= cfg.max_grad_volume_sol_fast {
    // Fast-ish + high volume — likely bot pump
    tracing::debug!(mint=%mint_str, speed=grad_speed_s, vol=grad_volume_sol, "rejected: fast+high_vol");
    self.stats_rejected.fetch_add(1, Ordering::Relaxed);
    return;
}
```

Default values:
```rust
min_grad_speed_s: 90,
max_grad_volume_sol_fast: 200.0,
max_grad_volume_sol_absolute: 650.0,
```

**Expected impact**: WR from 13.6% → ~30-35% (rejects ~600 of 856 bad trades)

**Test criteria**:
- Unit test: gate rejects (speed=60, vol=655.35) → rejected
- Unit test: gate passes (speed=120, vol=139.84) → passed
- Unit test: gate passes (speed=240, vol=74.23) → passed
- Unit test: gate rejects (speed=80, vol=400) → rejected (fast-ish + high vol)
- Unit test: gate passes (speed=120, vol=400) → passed (slow enough)
- Paper trade verification: after deployment, WR on enriched trades should be >30% within 200 trades

---

## Task 2: Scorer V3 — Inverted Speed Curve and Volume Penalty

**Files**: 
- `rust/pump-quant-core/src/momentum/scorer.rs`

**Problem**: Scorer v2 gives maximum speed score (15/15) to speed≤60s and maximum volume score (10/10) to vol≥600 SOL. Data proves both are inversely correlated with WR. Score=73 (the most common score, 52.1% of trades) has only 7.2% WR. Score=31 (n=30) has 56.7% WR.

**Evidence**:
- Speed=60: 15/15 score, 7.3% WR (698 trades)
- Speed≥120: 5-10/15 score, 41.1% WR (158 trades)
- Volume≥600: 10/10 score, 5.9% WR (527 trades)
- Volume 50-100: 4/10 score, 39.6% WR (53 trades)
- Score=73 cluster: n=446, WR=7.2% — this IS the whale pump score
- Score=31 cluster: n=30, WR=56.7% — this IS the organic momentum score

**Solution**: Scorer v3 with inverted speed and volume curves:

**New Speed Score (0-25)** — INVERTED: slow = high score
```
speed ≤ 60s   → 0  (was 15 — now penalized as likely bot)
speed  90s    → 5
speed 120s    → 15
speed 180s    → 20
speed 240s    → 25 (max)
speed ≥ 300s  → 20 (slightly less — very slow may lack momentum)
```

**New Volume Score (0-15)** — INVERTED: moderate = best, high = penalized
```
vol < 30 SOL     → 0  (too little activity)
vol 30-50 SOL    → 5
vol 50-100 SOL   → 15 (sweet spot: organic retail)
vol 100-200 SOL  → 12
vol 200-400 SOL  → 5  (likely institutional — lower WR)
vol 400-655 SOL  → 2  (probably bot/whale)
vol ≥ 655 SOL    → 0  (saturated — confirmed bot/whale)
```

**Velocity component (0-20)**: KEEP AS-IS. Normalized buy rate per SOL already works correctly.

**Buy/sell ratio (0-25)**: KEEP AS-IS. Unidirectional buying is a genuine positive signal.

**Entry discount (0-15)**: REDUCE weight from 30 → 15. Entry discount requires price data that may not be available at decision time, and its signal is partially captured by the speed component.

**Implementation**:

Replace `score_speed()`:
```rust
#[inline(always)]
fn score_speed(grad_speed_s: u32) -> u8 {
    if grad_speed_s <= 60 {
        0  // Bot/whale fill — no post-grad momentum
    } else if grad_speed_s <= 90 {
        // Linear 0 → 5 over [60, 90]
        ((grad_speed_s.saturating_sub(60)) * 5 / 30).min(5) as u8
    } else if grad_speed_s <= 120 {
        // Linear 5 → 15 over [90, 120]
        (5 + (grad_speed_s.saturating_sub(90)) * 10 / 30).min(15) as u8
    } else if grad_speed_s <= 180 {
        // Linear 15 → 20 over [120, 180]
        (15 + (grad_speed_s.saturating_sub(120)) * 5 / 60).min(20) as u8
    } else if grad_speed_s <= 300 {
        // Linear 20 → 25 over [180, 300]
        (20 + (grad_speed_s.saturating_sub(180)) * 5 / 120).min(25) as u8
    } else {
        // Very slow: slight decline 25 → 20 over [300, 600]
        25u8.saturating_sub(((grad_speed_s.saturating_sub(300)) * 5 / 300).min(5) as u8)
    }
}
```

Replace `score_volume_tier()`:
```rust
#[inline(always)]
fn score_volume_tier(volume_sol_x100: u32) -> u8 {
    if volume_sol_x100 >= 65_500 {
        0  // Saturated u16 → confirmed bot/whale
    } else if volume_sol_x100 >= 40_000 {
        2  // 400-655 SOL — likely bot/whale
    } else if volume_sol_x100 >= 20_000 {
        5  // 200-400 SOL — institutional, lower WR
    } else if volume_sol_x100 >= 10_000 {
        12 // 100-200 SOL — good organic range
    } else if volume_sol_x100 >= 5_000 {
        15 // 50-100 SOL — sweet spot (39.6% WR)
    } else if volume_sol_x100 >= 3_000 {
        5  // 30-50 SOL — light activity
    } else {
        0  // < 30 SOL — insufficient activity
    }
}
```

Update `score_entry_discount()` cap from 30 → 15:
```rust
(discount_bps as u32 / 66).min(15) as u8  // Was /33, cap 30
```

Update `GraduationScore` doc comments to reflect new ranges:
- Speed: 0-25 (was 0-15)
- Volume: 0-15 (was 0-10)
- Velocity: 0-20 (unchanged)
- Buy/sell: 0-25 (unchanged)
- Discount: 0-15 (was 0-30)
- Total: 0-100 (unchanged)

**Expected impact**: Organic tokens score 55-80, whale pump tokens score 10-30. Combined with Task 1 hard gate, the min_grad_score threshold can be set to 35 and still capture all winners.

**Test criteria**:
- `score_graduation(60, 60_000, 3, 0, 411, 411)` → speed=0, vol_tier=2, velocity=0, ratio=15, discount=0 → total=17 (whale pump)
- `score_graduation(120, 14_000, 10, 2, 390, 411)` → speed=15, vol_tier=12, velocity=7, ratio=25, discount=3 → total=62 (organic)
- `score_graduation(240, 7_500, 10, 1, 370, 411)` → speed=25, vol_tier=15, velocity=13, ratio=25, discount=9 → total≈87 (slow organic, high discount)
- Max theoretical score = 25+15+20+25+15 = 100 ✓
- All existing test scenarios updated to reflect new curves

---

## Task 3: ws_notif Scale-In Gate

**Files**:
- `rust/pump-quant-core/src/momentum/position.rs` (scale-in logic)
- `rust/pump-quant-core/src/momentum/config.rs` (new threshold field)

**Problem**: 190/202 probes never scaled in (6.3% scale-in rate). When probes DO scale into bad tokens (ws_notif<5), WR is 1.4%. When ws_notif≥10, WR jumps to 27.2%. The scale-in logic triggers on price samples (s[0], s[1]) but doesn't check whether the token has any real trading activity.

**Evidence**:
- ws_notif=0: n=165, WR=0.0% — ZERO wins. No one is trading.
- ws_notif 1-10: n=345, WR=4.9%, Exp=-0.33 mSOL
- ws_notif≥10: n=371, WR=27.2%, Exp=2.77 mSOL
- ws_notif≥20: n=260, WR=30.8%, Exp=4.04 mSOL
- ws_notif≥50: n=159, WR=35.2%, Sharpe=0.144
- With speed≥120 + vol<200 + ws_notif≥10: n=113, WR=52.2%, Exp=3.72 mSOL

**Solution**: Add a `min_ws_notif_for_scale_in` gate to the scale-in decision. If the position hasn't received at least N WebSocket notifications by the time scale-in is evaluated, stay at probe size.

**Implementation**:

In `config.rs`, add:
```rust
/// Minimum ws_notif_count required before scale-in is allowed.
/// Below this threshold, position stays at probe size regardless of price movement.
/// ws_notif_count measures realized trading activity on the Raydium/PumpSwap pool.
/// 0 = disabled (scale-in always allowed). Default: 10.
pub min_ws_notif_for_scale_in: u16,
```

Default: `min_ws_notif_for_scale_in: 10`

In `position.rs`, in the scale-in evaluation (wherever `scale_in_s0_strong_bps` / `scale_in_s0_moderate_bps` are checked), add a pre-check:

```rust
// Before evaluating price-based scale-in:
if cfg.min_ws_notif_for_scale_in > 0 
    && self.ws_notif_count < cfg.min_ws_notif_for_scale_in as u32 
{
    // Insufficient trading activity — stay at probe size
    tracing::trace!(
        mint = %self.mint_str,
        ws_notif = self.ws_notif_count,
        threshold = cfg.min_ws_notif_for_scale_in,
        "scale-in blocked: insufficient ws_notif"
    );
    return; // Don't scale in yet
}
```

**Expected impact**: Eliminates scale-in on dead tokens. Combined with Task 1, WR on scaled-in trades should reach 50%+.

**Test criteria**:
- Unit test: ws_notif=0 → scale-in blocked, position stays at probe_size_sol
- Unit test: ws_notif=5 → scale-in blocked (below threshold of 10)
- Unit test: ws_notif=10 → scale-in allowed, proceeds to price check
- Unit test: ws_notif=50 → scale-in allowed
- Integration test: verify no scale-in occurs during the first few seconds on a dead token

---

## Task 4: Price Trajectory Scale-In Gate (s[1] Confirmation)

**Files**:
- `rust/pump-quant-core/src/momentum/position.rs` (scale-in logic)
- `rust/pump-quant-core/src/momentum/config.rs` (new threshold field)

**Problem**: The current scale-in logic checks s[0] (first price sample). But in the data, s[0] is ALWAYS 0 (the first sample is at entry price — no movement yet). The first informative sample is s[1]. Trades with s[1]>0 have 50.9% WR (n=112). Trades with s[1]=0 have 6.8% WR (n=676).

**Evidence**:
- s[0] = 0 for ALL 856 enriched trades — this is because price is sampled at entry, which is by definition 0 bps offset
- s[1] > 0: n=118, WR=50.9%, Exp=3.26 mSOL, Sharpe=0.250
- s[1] = 0: n=676, WR=6.8%, Exp=-0.11 mSOL
- s[1] ≤ -100: n=729, WR=7.1% (includes zero and negative)
- max(price_samples[:3]) > 200: n=14, WR=85.7%, Exp=68.12 mSOL (small sample but extreme signal)
- max(price_samples[:3]) > 300: n=8, WR=100%, Exp=111.63 mSOL (very small sample)

**Solution**: Require s[1] > 0 bps (any positive movement) before allowing scale-in. This is already naturally aligned with the `probe_hold_ms: 2000` timing — by the time the second sample arrives (~2s after entry), we know if the token has any momentum.

**Implementation**:

In `config.rs`, add:
```rust
/// Minimum bps at s[1] (second price sample) required for scale-in.
/// If s[1] < this threshold, position stays at probe size.
/// Default: 1 (any positive movement). Set to 0 to disable.
pub scale_in_min_s1_bps: i32,
```

Default: `scale_in_min_s1_bps: 1`

In `position.rs`, in the scale-in evaluation after the ws_notif check (Task 3):

```rust
// Check second price sample for scale-in confirmation
if cfg.scale_in_min_s1_bps > 0 && self.sample_count >= 2 {
    let s1 = self.price_samples_bps[1];
    if s1 < cfg.scale_in_min_s1_bps {
        tracing::trace!(
            mint = %self.mint_str,
            s1 = s1,
            threshold = cfg.scale_in_min_s1_bps,
            "scale-in blocked: s[1] below threshold"
        );
        return; // Price not rising — don't scale in
    }
}
```

**Expected impact**: Combined with Tasks 1-3, this brings scale-in WR to ~50%+ on tokens that actually move up after entry.

**Test criteria**:
- Unit test: s[1] = 0 → scale-in blocked
- Unit test: s[1] = -50 → scale-in blocked
- Unit test: s[1] = 5 → scale-in allowed
- Unit test: s[1] = 100 → scale-in allowed
- Unit test: sample_count < 2 → gate not evaluated (not enough data yet)
- Paper trade verification: after deployment, scaled-in trades should have WR>45%

---

## Task 5: Time-of-Day Gate Expansion + Dead Token Early Exit

**Files**:
- `rust/pump-quant-core/src/momentum/tod.rs` (ToD config updates)
- `rust/pump-quant-core/src/momentum/config.rs` (updated defaults)
- `rust/pump-quant-core/src/momentum/position.rs` (dead token exit)

**Problem (A)**: UTC hours 18-20 have 1.5-3.2% WR across all datasets. UTC 2-6 show 4-13% WR. Currently `reduced_hours_utc` halves the size but doesn't block entry. Permutation testing shows blocking UTC 2-6 consistently appears in top configurations.

**Evidence**:
- UTC 18: n=437, WR=3.2%, Exp=-0.38 mSOL
- UTC 19: n=471, WR=3.0%, Exp=-0.27 mSOL
- UTC 20: n=201, WR=1.5%, Exp=-0.31 mSOL
- UTC 02-06 combined: ~500 trades, WR=4-13%, mostly negative expectancy
- Permutation testing: `block_02_06+18_20` consistently appears in top Pareto configurations

**Problem (B)**: 322/856 enriched trades (37.6%) have all-zero price samples — the token never moves. These are held for the full `time_sl_ms` (currently 15-60s) before exit, wasting capital and incurring fees. A faster dead-token exit would free capital for better opportunities.

**Evidence**:
- All-zero price samples: n=322, WR=0.3%, Exp=-1.05 mSOL
- 86.9% of all-zero trades are speed=60 (would be caught by Task 1, but this is a defense-in-depth measure)
- ws_notif=0 at close: n=165, WR=0.0% — these tokens had ZERO activity

**Solution (A)**: Move UTC 18-20 and UTC 2-6 to `blocked_hours_utc` (zero entry).

**Solution (B)**: Add a "dead token fast exit" that triggers when:
1. `ws_notif_count == 0` AND hold ≥ 5_000ms, OR
2. All price_samples_bps are zero AND sample_count ≥ 5 AND hold ≥ 5_000ms

Exit with `time_sl` reason, avoiding the full time_sl_ms wait.

**Implementation (A)**:

In `config.rs`, update `MomentumTodConfig` default:
```rust
impl Default for MomentumTodConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            blocked_hours_utc: vec![2, 3, 4, 5, 18, 19, 20],  // Was: empty
            reduced_hours_utc: vec![0, 1, 6, 21, 22, 23],     // Was: 18-23, 0-5
            boosted_hours_utc: vec![7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],  // Was: 8-17
            reduced_size_multiplier: 0.5,
        }
    }
}
```

**Implementation (B)**:

In `config.rs`, add to `MomentumConfig`:
```rust
/// Enable fast exit for dead tokens (zero ws_notif + zero price movement).
/// Default: true.
pub dead_token_fast_exit_enabled: bool,

/// Minimum hold time (ms) before dead token fast exit can fire.
/// Default: 5000 (5s — enough time for at least 3-4 price samples).
pub dead_token_fast_exit_min_hold_ms: u64,

/// Minimum price sample count before dead token flat detection fires.
/// Default: 5.
pub dead_token_fast_exit_min_samples: u8,
```

In `position.rs`, in the tick evaluation (before other exit checks):
```rust
// Dead token fast exit
if cfg.dead_token_fast_exit_enabled && hold_ms >= cfg.dead_token_fast_exit_min_hold_ms {
    let all_flat = self.sample_count >= cfg.dead_token_fast_exit_min_samples as u32
        && self.price_samples_bps[..self.sample_count as usize].iter().all(|&s| s == 0);
    let no_activity = self.ws_notif_count == 0;
    
    if all_flat && no_activity {
        tracing::info!(
            mint = %self.mint_str,
            hold_ms = hold_ms,
            samples = self.sample_count,
            ws_notif = self.ws_notif_count,
            "dead token fast exit: zero activity + flat price"
        );
        return Some(MomentumExitReason::TimeSl); // Exit early
    }
}
```

Default values:
```rust
dead_token_fast_exit_enabled: true,
dead_token_fast_exit_min_hold_ms: 5_000,
dead_token_fast_exit_min_samples: 5,
```

**Expected impact**: 
- (A) Blocks ~1,100 trades in dead hours, saving ~0.5-1.0 SOL in accumulated fees
- (B) Reduces avg hold for dead tokens from ~18s to ~5s, freeing 1 of 5 max_concurrent slots 72% faster

**Test criteria**:
- Unit test: UTC 18 → entry blocked (multiplier = 0.0)
- Unit test: UTC 3 → entry blocked (multiplier = 0.0)
- Unit test: UTC 9 → entry allowed (boosted, multiplier = 1.0)
- Unit test: UTC 22 → entry reduced (multiplier = 0.5)
- Unit test: dead_token_exit fires when ws_notif=0 + 5 flat samples + hold>5s
- Unit test: dead_token_exit does NOT fire when ws_notif=0 but hold<5s
- Unit test: dead_token_exit does NOT fire when ws_notif>0 even if samples flat

---

## Deployment Sequence

**Phase 1 (Immediate — Day 1)**:
1. Deploy Task 1 (hard gate) — largest impact, lowest risk
2. Deploy Task 5A (ToD blocking) — simple config change, no logic risk

**Phase 2 (Day 1-2, after Phase 1 validated)**:
3. Deploy Task 3 (ws_notif gate) — requires no scorer change
4. Deploy Task 4 (s[1] gate) — requires no scorer change
5. Deploy Task 5B (dead token fast exit) — defense in depth

**Phase 3 (Day 2-3, after Phase 2 validated)**:
6. Deploy Task 2 (scorer v3) — largest code change, highest risk, but also needed for long-term scoring quality

**Validation at each phase**: Run 100+ trades, verify WR matches backtest expectations ±10%.

---

## Config Snapshot (Final State)

After all tasks deployed, the key config values should be:

```json
{
  "min_grad_speed_s": 90,
  "max_grad_volume_sol_fast": 200.0,
  "max_grad_volume_sol_absolute": 650.0,
  "min_grad_score": 30,
  "min_ws_notif_for_scale_in": 10,
  "scale_in_min_s1_bps": 1,
  "dead_token_fast_exit_enabled": true,
  "dead_token_fast_exit_min_hold_ms": 5000,
  "dead_token_fast_exit_min_samples": 5,
  "tod_config": {
    "enabled": true,
    "blocked_hours_utc": [2, 3, 4, 5, 18, 19, 20],
    "reduced_hours_utc": [0, 1, 6, 21, 22, 23],
    "boosted_hours_utc": [7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
    "reduced_size_multiplier": 0.5
  }
}
```
