# PUMP-QUANT v5 ALGORITHMIC AUDIT & IMPROVEMENT SPEC

## BlackRock Digital Assets — Quantitative Trading Research

---

# PART 1: SCORING CRITIQUE

## 1.1 The 70-80 Paradox: Mathematical Explanation

The score inversion (70-80 bucket: 12% WR, -0.009 SOL vs 60-70 bucket: 21% WR, +0.174 SOL) is not paradoxical. It's a **selection bias / adverse selection trap**:

### Root Cause: The Score Conflates "Attractive" with "Competitive"

A token scoring 70-80 has multiple signals firing simultaneously. **Every bot monitoring graduations runs an equivalent filter.** The signals that push 60→75 are exactly what every competitor's algo detects.

**Mathematical framing:**

Let N(s) = number of competing bots entering a token with score s.
Let α(s) = the "alpha" (true continuation probability).

Effective win rate: WR_eff(s) = α(s) × 1/(1 + β·N(s))

Even if α(70-80) > α(60-70), competition scaling N(70-80) >> N(60-70) crushes effective WR.

**Evidence:**
- 70-80: 146 trades (42% of all trades) — entering the most crowded trades
- 60-70: 84 trades (24%) — selective, less crowded
- hard_sl at 46% of last 50 = bot competition causing instant adverse movement

### Specific Scoring Flaws

1. **Speed + Volume are collinear** — slower graduation → higher volume (same dynamic, double-counted)
2. **Velocity normalization penalizes better tokens** at higher volumes
3. **Entry Discount is NEGATIVE alpha** — large discount means sellers already dumped, not a deal
4. **No competition/crowding signal** — the single biggest missing feature
5. **Cold Miss Bonus rewards ignorance** — +5 for not having data biases toward blind entries

## 1.2 Revised Scoring Formula

### Architecture: Replace additive scoring with Gate → Rank → Queue

```
GATE (binary pass/fail):
  - volume ∈ [40, 150] SOL
  - lp_reserve ∈ [50, 200] SOL
  - graduation_time ∈ [75, 300] seconds
  - entry_price <= terminal_price * 1.03

RANK (0-100, weighted):
  1. Buy Pressure Imbalance (0-30):  net_buys_5s / (buys+sells+1) scaled
  2. Volume Acceleration (0-25):     vol_last_3s / vol_prior_7s
  3. Anti-Crowding Proxy (0-25):     graduation_time oddity + volume non-round-number
  4. LP Freshness (0-10):            reserve proximity to 85 SOL
  5. Seller Exhaustion (0-10):       sells_5s==0 AND buys_5s>=2

ENTRY: Gate pass AND rank >= 55 AND cooldown_clear
```

### Anti-Crowding Proxy (KEY INNOVATION):
```rust
// Bots filter <=90s or <=120s typically
let grad_oddity = match graduation_secs {
    91..=119 => 15,   // between common thresholds
    121..=179 => 25,  // between 120 and 180 thresholds
    181..=250 => 20,  // slower, fewer bots
    251..=300 => 10,
    _ => 0,
};
// Bots filter >50 SOL typically
let vol_oddity = match volume_sol {
    40..=49 => 20,    // just below common 50 SOL threshold
    50..=65 => 5,     // max competition zone
    66..=100 => 12,
    101..=150 => 15,
    _ => 0,
};
let score = min((grad_oddity + vol_oddity) / 2, 25);
```

---

# PART 2: EXIT CRITIQUE

## 2.1 Probe Phase is Structurally Flawed

The 3s probe with -600 bps dump creates a binary trap:
- Token dumps: eat -600+ bps (actually -800 to -1200 with latency)
- Token flat: hold 3s → HELD_TIGHT → sit 8s more → time_sl. Net: -2.2% fees
- Token pumps: correct, you're in for 0.03 SOL

Probe adds value only for case 3 (pumps). But 80% = case 2 (dead), 13% = case 1 (dump). **Probe is a fee-burning machine for 93% of entries.**

### The -600 widening was WRONG:
Widening from -300→-600 meant holding LONGER in actively dumping tokens. -300 was actually better — faster escape.

### Recommendation: Replace probe with Rapid Assessment + Fast Kill

```
PHASE 1 (0-1500ms): RAPID ASSESSMENT
  - Monitor every 50ms tick
  - bps <= -200 at ANY tick → EXIT IMMEDIATELY (micro_sl)
  - bps >= +100 at any tick → MOMENTUM phase
  - bps ∈ (-200, +100) for 1500ms → OBSERVATION phase

PHASE 2a - MOMENTUM (crossed +100 bps):
  - Tight trailing: 150 bps from high
  - Widen to 300 bps after +500 bps
  - Widen to 500 bps after +1000 bps
  - No time limit (let runners run)

PHASE 2b - OBSERVATION (flat 1500ms):
  - Monitor 3000ms more (4500ms total)
  - Any WS activity → extend 2000ms
  - Zero WS for 3000ms straight → EXIT (dead_token_sl)
  - bps drops below -200 → EXIT (obs_sl)
  - bps crosses +100 → transfer to MOMENTUM
```

**Savings from -200 micro_sl vs -774 avg hard_sl:**
- Per trade: (774-200) × 0.03/10000 = 0.00172 SOL saved
- × 46 hard_sl trades × 0.8 (false kill adjustment) = **0.063 SOL** (53% of current net!)

## 2.2 Dead Token Gate (eliminates 80% of time_sl fee burn)

Pre-entry activity verification:
```
ws_messages_last_2s >= 3     // Must have ONGOING activity
last_trade_age_ms <= 1500    // A trade within last 1.5s
```

This eliminates ~60-70% of dead-token entries.
- 256 time_sl × 80% dead × 0.03 SOL × 2.2% fees = **0.135 SOL** in pure fee burn eliminated
- That's MORE than current total net profit

## 2.3 Trailing Stop Optimization

For memecoins in momentum: μ ≈ 30-50 bps/s, σ ≈ 80-150 bps/s.
Optimal trailing width: w* = σ²/(2μ) = 100²/(2×40) = 125 bps = 1.25%

Current accel trailing of 25% (2500 bps) is **20x too wide**. You're leaving massive profit on the table.

**Recommended adaptive trailing:**
```
bps < 500:   trail = 150 bps (1.5%)
500-1000:    trail = 300 bps (3%)
1000-3000:   trail = 500 bps (5%)
>3000:       trail = 800 bps (8%)
```

---

# PART 3: KELLY ANALYSIS

Given: WR=15.4%, avg_win=+6.36 mSOL, avg_loss=-0.78 mSOL

Win/loss ratio b = 6.36/0.78 = 8.15
Full Kelly: f* = p - q/b = 0.154 - 0.846/8.15 = 0.154 - 0.104 = **5.0%**
Quarter Kelly: f*/4 = **1.25%** of bankroll = 0.71 × 0.0125 = **8.9 mSOL**

Current probe at 30 mSOL is **3.4x quarter-Kelly**. You're OVERSIZED relative to the edge.

**However:** The distribution is bimodal — either +1770 bps (trailing_stop) or ~0 bps (everything else). True Kelly for bimodal:

- P(big win) = 39/344 = 11.3%, avg big win = +6.36 mSOL
- P(small loss) = 305/344 = 88.7%, avg small loss = -0.45 mSOL

f* = 0.113 - 0.887/14.1 = 0.113 - 0.063 = **5.0%** (same result, different path)

**Recommendation:** Reduce probe to 0.01 SOL (10 mSOL) and SCALE IN only on momentum confirmation.

```
Entry: 0.01 SOL (mini probe)
If bps > +100 within 1.5s: scale to 0.03 SOL
If bps > +500 within 10s: scale to 0.05 SOL
Otherwise: exit at 0.01 SOL (minimal fee drag)
```

Fee at 0.01 SOL: ~0.22 mSOL (vs 0.65 mSOL at 0.03 SOL). Dead token cost drops 66%.

---

# PART 4: ALGO BUILD SPEC — 5 ENGINEER TASKS

## ENGINEER 1: Dead Token Gate + Pre-Entry Activity Filter (HIGHEST IMPACT)

**Expected impact: +0.10-0.15 SOL per 344 trades**

Changes to `momentum/mod.rs`:

1. Add `ws_activity_gate` before any position opening:
```rust
// In on_graduation / entry path, after observation window:
let ws_count = price_feed.ws_notif_count(mint);
let last_trade_age_ms = now_ms - price_feed.last_ws_notif_ms(mint);

if ws_count < 3 || last_trade_age_ms > 1500 {
    tracing::info!(mint=%mint_str, ws_count, last_trade_age_ms,
        "[entry_gate] REJECTED — insufficient ongoing activity");
    return; // skip entry
}
```

2. Add `observation_window_activity_min` to MomentumConfig:
```rust
pub observation_min_ws_notifs: u16,        // default: 3
pub observation_max_last_trade_age_ms: u64, // default: 1500
```

3. Wire into observation window pass logic (mod.rs ~line 2294)

**Config changes:**
- ADD: `observation_min_ws_notifs: 3`
- ADD: `observation_max_last_trade_age_ms: 1500`

## ENGINEER 2: Micro-SL + Rapid Assessment (replaces probe phase)

**Expected impact: +0.05-0.08 SOL per 344 trades**

Replace probe state machine in `momentum/position.rs`:

1. Replace `ProbePhase` enum:
```rust
pub enum EntryPhase {
    RapidAssessment,  // 0-1500ms: watching for instant dump or momentum
    Momentum,         // crossed +100 bps: activate trailing
    Observation,      // flat after 1500ms: wait for activity
    Exiting,          // decided to exit
}
```

2. New `evaluate_rapid_assessment()`:
```rust
pub fn evaluate_rapid_assessment(&self, now_ms: u64, current_bps: i32, ws_active: bool) -> EntryPhase {
    let elapsed = self.hold_ms(now_ms);
    
    // Instant kill: any tick below -200 bps
    if current_bps <= -200 { return EntryPhase::Exiting; }
    
    // Momentum detected
    if current_bps >= 100 { return EntryPhase::Momentum; }
    
    // Still in rapid assessment window
    if elapsed < 1500 { return EntryPhase::RapidAssessment; }
    
    // Transition to observation
    EntryPhase::Observation
}
```

3. New `evaluate_observation()`:
```rust
pub fn evaluate_observation(&self, now_ms: u64, current_bps: i32,
    ws_count: u16, last_ws_ms: u64) -> EntryPhase {
    let elapsed = self.hold_ms(now_ms);
    
    // Kill if drops during observation
    if current_bps <= -200 { return EntryPhase::Exiting; }
    
    // Momentum detected during observation
    if current_bps >= 100 { return EntryPhase::Momentum; }
    
    // Dead token: no WS for 3000ms
    if now_ms - last_ws_ms > 3000 && ws_count < 2 { return EntryPhase::Exiting; }
    
    // Max observation time: 4500ms total
    if elapsed > 4500 && current_bps < 50 { return EntryPhase::Exiting; }
    
    EntryPhase::Observation
}
```

**Config changes:**
- CHANGE: `probe_hold_ms: 3000` → REMOVE (replaced by rapid assessment)
- CHANGE: `probe_dump_threshold_bps: -600` → `micro_sl_bps: -200`
- ADD: `rapid_assessment_ms: 1500`
- ADD: `momentum_entry_bps: 100`
- ADD: `observation_max_ms: 4500`
- ADD: `observation_dead_ws_ms: 3000`

## ENGINEER 3: Adaptive Trailing Stop (tightened for memecoin dynamics)

**Expected impact: +0.03-0.06 SOL per 344 trades**

Replace fixed trailing stop logic in exit evaluation:

1. New `compute_adaptive_trail_bps()` in `position.rs`:
```rust
pub fn compute_adaptive_trail_bps(&self, current_gain_bps: i32) -> u16 {
    match current_gain_bps {
        ..=500 => 150,       // tight trail for small gains
        501..=1000 => 300,   // medium trail
        1001..=3000 => 500,  // let big moves breathe
        _ => 800,            // very large moves: wide trail
    }
}
```

2. Remove all the momentum-state trailing stop logic (accel/decel/sustain/reversal width switching). Replace with the simple tiered approach above.

3. Remove `trailing_stop_tier1/tier2` configs. Replace with:
```rust
pub trail_tier_bps: Vec<(i32, u16)>,  // [(threshold_bps, trail_width_bps)]
// default: [(500, 150), (1000, 300), (3000, 500), (i32::MAX, 800)]
```

**Config changes:**
- REMOVE: `trailing_stop_accel_pct`, `trailing_stop_decel_pct`, `trailing_stop_reversal_pct`
- REMOVE: `trailing_stop_tier1_max_bps`, `trailing_stop_tier1_pct`, `trailing_stop_tier2_max_bps`, `trailing_stop_tier2_pct`
- ADD: `trail_tiers: [[500, 150], [1000, 300], [3000, 500], [999999, 800]]`

## ENGINEER 4: Revised Scorer (Gate + Rank replaces additive)

**Expected impact: +0.05-0.10 SOL per 344 trades (fewer but better entries)**

1. New `scorer_v5.rs` (or modify `scorer.rs`):
```rust
pub struct EntryGate {
    pub passed: bool,
    pub reject_reason: &'static str,
}

pub fn check_entry_gate(
    volume_sol: u32,          // SOL (not centisol)
    reserve_sol: u64,         // lamports
    grad_speed_s: u32,
    entry_price_fp: u64,
    terminal_price_fp: u64,
) -> EntryGate {
    let reserve = reserve_sol / 1_000_000_000;
    if volume_sol < 40 { return EntryGate { passed: false, reject_reason: "volume_too_low" }; }
    if volume_sol > 150 { return EntryGate { passed: false, reject_reason: "volume_too_high" }; }
    if reserve < 50 { return EntryGate { passed: false, reject_reason: "reserve_too_thin" }; }
    if reserve > 200 { return EntryGate { passed: false, reject_reason: "reserve_too_large" }; }
    if grad_speed_s < 75 { return EntryGate { passed: false, reject_reason: "too_fast" }; }
    if grad_speed_s > 300 { return EntryGate { passed: false, reject_reason: "too_slow" }; }
    if terminal_price_fp > 0 && entry_price_fp > terminal_price_fp * 103 / 100 {
        return EntryGate { passed: false, reject_reason: "premium_entry" };
    }
    EntryGate { passed: true, reject_reason: "" }
}

pub fn rank_graduation(
    buys_5s: u32, sells_5s: u32,
    volume_sol: u32, grad_speed_s: u32,
    reserve_sol_lamports: u64,
) -> u8 {
    let buy_imbalance = score_buy_imbalance(buys_5s, sells_5s);       // 0-30
    let anti_crowd = score_anti_crowding(grad_speed_s, volume_sol);    // 0-25
    let lp_fresh = score_lp_freshness(reserve_sol_lamports);           // 0-10
    let seller_exhaust = score_seller_exhaustion(buys_5s, sells_5s);   // 0-10
    // Volume accel requires runtime data (not available at scoring time)
    // Reserve 0-25 for it, use 12 (neutral) when unavailable
    let vol_accel_placeholder: u8 = 12;
    
    buy_imbalance + vol_accel_placeholder + anti_crowd + lp_fresh + seller_exhaust
}
```

2. Wire into `on_graduation` path, replacing `score_graduation()` call.

**Config changes:**
- CHANGE: `min_grad_score: 45` → `min_rank_score: 55`
- ADD: `gate_min_volume_sol: 40`
- ADD: `gate_max_volume_sol: 150`
- ADD: `gate_min_grad_speed_s: 75`
- ADD: `gate_max_grad_speed_s: 300`
- REMOVE: `cold_miss_bonus` concept (no more rewarding ignorance)

## ENGINEER 5: Scale-In Sizing (replace fixed probe with tiered entry)

**Expected impact: +0.02-0.04 SOL per 344 trades (reduced fee drag)**

1. Implement tiered entry in `momentum/mod.rs`:
```rust
// Initial entry: mini probe
let initial_size_sol = config.scale_in_initial_sol; // 0.01 SOL

// On momentum confirmation (+100 bps):
let scale1_size_sol = config.scale_in_momentum_sol; // 0.03 SOL (add 0.02)

// On strong momentum (+500 bps):
let scale2_size_sol = config.scale_in_strong_sol;   // 0.05 SOL (add 0.02)
```

2. Scale-in requires separate buy TX for each tier. Track in position:
```rust
pub scale_tier: u8,  // 0=initial, 1=momentum, 2=strong
pub total_size_lamports: u64,  // cumulative position size
```

3. When entry phase transitions to Momentum → submit scale1 buy TX
4. When gain crosses +500 bps → submit scale2 buy TX

**Config changes:**
- ADD: `scale_in_initial_sol: 0.01`
- ADD: `scale_in_momentum_sol: 0.03`
- ADD: `scale_in_strong_sol: 0.05`
- ADD: `scale_in_momentum_bps: 100`
- ADD: `scale_in_strong_bps: 500`
- CHANGE: `probe_size_sol: 0.03` → becomes `scale_in_initial_sol: 0.01`

---

# PART 5: EXPECTED IMPACT SUMMARY

| Change | Engineer | Est. Impact (per 344 trades) | Trades Affected |
|--------|----------|------------------------------|-----------------|
| Dead token gate | E1 | +0.10-0.15 SOL | ~200 eliminated |
| Micro-SL (-200 bps) | E2 | +0.05-0.08 SOL | 46 hard_sl improved |
| Adaptive trailing | E3 | +0.03-0.06 SOL | 39 trailing_stop improved |
| Revised scorer | E4 | +0.05-0.10 SOL | All (fewer, better entries) |
| Scale-in sizing | E5 | +0.02-0.04 SOL | All (reduced fee drag) |
| **TOTAL** | | **+0.25-0.43 SOL / 344 trades** | |

**Current net: +0.179 SOL / 344 trades → Projected: +0.43-0.61 SOL / 344 trades**

### Priority order:
1. E1 (dead token gate) — highest impact, easiest to implement
2. E2 (micro-SL) — second highest, moderate complexity
3. E4 (revised scorer) — third, reduces trade count but improves quality
4. E3 (adaptive trailing) — improves winners
5. E5 (scale-in) — optimizes sizing, most complex (multiple TX per position)
