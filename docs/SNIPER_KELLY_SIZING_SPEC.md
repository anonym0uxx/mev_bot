# SniperEngine — Kelly Position Sizing Spec

**Version:** 1.0  
**Date:** 2026-04-04  
**Author:** Apollo (quant sizing subagent)  
**Status:** Ready for implementation  
**Depends on:** `SNIPER_SIGNAL_SPEC.md` v1.4 (entry signal system — do not modify)  
**Purpose:** Replace the Kelly sizing stub in the decision flowchart with a zone-aware, empirically-calibrated position sizing system.

---

## Table of Contents

1. [Design Rationale](#1-design-rationale)
2. [Kelly Formula Adaptation for Bonding Curve Sniping](#2-kelly-formula-adaptation-for-bonding-curve-sniping)
3. [Score-to-Probability Calibration](#3-score-to-probability-calibration)
4. [Zone-Aware Position Sizing](#4-zone-aware-position-sizing)
5. [Score-to-Size Mapping](#5-score-to-size-mapping)
6. [Wallet Sizing Constraints & Drawdown Protection](#6-wallet-sizing-constraints--drawdown-protection)
7. [Rust Implementation Spec](#7-rust-implementation-spec)
8. [Config Block](#8-config-block)
9. [Worked Examples](#9-worked-examples)
10. [Calibration Plan](#10-calibration-plan)

---

## 1. Design Rationale

### Why Kelly for Atomic Bundle Sniping?

Standard Kelly criterion optimizes long-run growth rate for repeated bets with known win probability `p` and payoff odds `b`. Our sniper satisfies the core assumptions:

- **Repeated, independent bets.** Each snipe is a distinct token on an independent curve.
- **Known payoff structure.** Jito atomic bundle: win = +12% gross, lose = tip + fees only (~0.000005 SOL).
- **Edge estimation possible.** `final_score` correlates with win probability — estimable from score and refinable from data.

### What's Wrong with the Current Stub

| Problem | Solution |
|---------|----------|
| Hardcoded `score_to_probability` — no empirical basis | Bayesian prior → running win rate by score bucket (§3) |
| No G4 zone information in sizing | Explicit zone multiplier post-allocation (§4) |
| Fixed `b = 0.12` ignores curve-dependent slippage | Zone-adjusted `b_eff` (§2) |
| No bootstrap period | First 50 trades: flat 0.02 SOL (§3) |
| No optimal vs conditional zone distinction | Zone multiplier table (§4) |
| Kelly inputs not updated from live results | Exponentially-weighted buckets with 100-trade effective lookback (§3) |

### Key Design Decision: Layered Sizing with Kelly Modulator

Pure Kelly degenerates for atomic bundles: the real loss is ~0, so Kelly says "always max bet." A conservative model (full position at risk) requires >90% win rate for positive edge at 9% reward — too restrictive.

**Solution: Score-based allocation modulated by empirical Kelly signal.**

Position size is computed in **five layers**, applied multiplicatively:

```
position = effective_max_position
         × score_allocation          // piecewise linear from final_score
         × kelly_modulator           // empirical performance vs prior expectation
         × kelly_fraction            // half-Kelly (0.50)
         × zone_multiplier           // curve depth risk adjustment
         × drawdown_multiplier       // portfolio protection
         → clamp to [MIN_POSITION, effective_max]
```

Each layer is explicit, logged, and independently tunable.

---

## 2. Kelly Formula Adaptation for Bonding Curve Sniping

### The Asymmetry Problem

Jito atomic bundle payoff:
- **Win:** +12% gross, ~9.5% net after 2.5% round-trip bonding curve fees.
- **Loss:** Bundle doesn't land or is rejected → 0. Only cost = Jito tip ≈ 0.000005 SOL.

Traditional Kelly with real payoffs:
```
b_real = (0.095 × position) / 0.000005 ≈ 19000 × position_sol
kelly_f = p - (1-p)/b_real ≈ p  → "bet everything" for any p > 1%
```

This is mathematically correct but operationally useless. When bets are effectively free, Kelly provides no useful sizing guidance.

### The Conservative Model Problem (and why we don't use it)

If we pretend the full position is at risk:
```
b_eff = 0.095  (9.5% net reward)
kelly_f > 0  requires  p > 1/(1 + b_eff) = 1/1.095 = 0.913
```

A 91.3% win rate threshold is unreachable. The conservative model never produces positive Kelly for sub-10% reward bets.

### Resolution: Score-Based Allocation + Kelly Modulator

Since pure Kelly degenerates both ways, we use a **hybrid:**

1. **Score-based allocation** provides the base size (piecewise linear, works without data).
2. **Kelly modulator** adjusts base allocation up/down based on how each score-zone bucket is actually performing vs its prior expectation.

The modulator preserves Kelly's core insight (size proportional to edge) without requiring a coherent loss model:

```
modulator = blended_win_rate / prior_win_rate
          → clamped to [0.50, 2.00]

// Bucket outperforming prior → size up (max 2×)
// Bucket underperforming    → size down (min 0.5×)
// Bucket catastrophically bad (win_rate < 10%, 30+ trades) → kill (0×)
```

### 2.1 Zone-Adjusted Effective Reward (`b_eff`)

Target: +12% gross. After 1.25% fee each way = 2.5% round trip → ~9.5% net. But slippage depends on curve depth:

```
position_impact = position_sol / real_sol
// real_sol=3, 0.10 SOL → 3.3% of curve depth → material sell impact
// real_sol=12, 0.10 SOL → 0.83% → negligible
```

**Zone-adjusted b_eff (integer bps):**

| Zone | real_sol | b_eff_bps | b_eff | Slippage | Rationale |
|------|----------|-----------|-------|----------|-----------|
| Early Optimal | 2–5 | 650 | 6.5% | 1–3% extra | Steep curve, thin depth |
| Peak Optimal | 5–15 | 900 | 9.0% | 0–1% extra | Good depth, clean execution |
| Conditional | 15–20 | 850 | 8.5% | <0.5% extra | Deep curve but elevated opportunity risk |

**`b_eff` is used in the Kelly modulator's prior computation, not directly in sizing.** It influences how the prior sigmoid translates score → expected win rate, which in turn affects when the modulator triggers bucket kills.

**⚠️ CALIBRATE AFTER 200 PAPER TRADES:** Compute actual `avg_net_pnl_pct` per zone. Update b_eff if realized differs by >2%.

---

## 3. Score-to-Probability Calibration

### 3.1 Bootstrap Period (First 50 Trades)

```
if total_trades < BOOTSTRAP_TRADE_COUNT:
    return BOOTSTRAP_POSITION_SOL  // 0.02 SOL flat, no Kelly
```

**Why 0.02 SOL:** Large enough for meaningful P&L signal, small enough to limit bootstrap cost. Maximum bootstrap exposure = 50 × 0.02 = 1.0 SOL (but atomic bundles mean actual exposure is one trade at a time).

During bootstrap, record `(final_score, zone, won)` tuples for adaptive estimator seeding.

### 3.2 Prior Probability Model (Sigmoid)

Before empirical data dominates, a logistic sigmoid maps score → expected win rate:

```rust
fn prior_probability(final_score: u8, config: &SniperSizingConfig) -> f64 {
    let p_min = config.prior_p_min_bps as f64 / 10000.0;  // 0.15
    let p_max = config.prior_p_max_bps as f64 / 10000.0;  // 0.75
    let mid = config.prior_midpoint as f64;                 // 65.0
    let k = config.prior_steepness_bps as f64 / 10000.0;   // 0.08
    let x = final_score as f64;
    p_min + (p_max - p_min) / (1.0 + (-k * (x - mid)).exp())
}

// Output:
//   score=40 → 0.26    score=65 → 0.45    score=90 → 0.63
//   score=50 → 0.32    score=70 → 0.49    score=100 → 0.68
//   score=60 → 0.40    score=80 → 0.57
```

**Anchor reasoning:**
- p=0.26 at score=40: marginal tokens win ~1 in 4 — conservative.
- p=0.68 at score=100: even perfect signals aren't guaranteed (bundle inclusion, timing, intervening trades).
- Inflection at 65: most score-to-performance sensitivity is in the 50-80 range.

**Zone-adjusted prior:** Conditional zone and early optimal get penalized priors:

```
zone_prior_adjustment:
    EarlyOptimal:  × 0.90  (thin data early in token life)
    PeakOptimal:   × 1.00  (baseline)
    Conditional:   × 0.75  (high follow-on requirement, profit-taking headwind)
```

### 3.3 Adaptive Kelly Buckets (Post-Bootstrap)

**Bucket structure:** 5 score ranges × 3 zones = 15 buckets.

```
Score: [40-49]=0  [50-59]=1  [60-69]=2  [70-79]=3  [80-100]=4
Zone:  EarlyOptimal=0  PeakOptimal=1  Conditional=2
```

**Exponentially-weighted tracking per bucket:**

```rust
struct KellyBucket {
    wins: u32,       // lifetime
    total: u32,      // lifetime
    ew_wins: f64,    // exponentially weighted
    ew_total: f64,   // exponentially weighted
}

const DECAY: f64 = 0.97;
// Half-life ≈ 23 trades. After 100 trades, oldest has 5% weight.
// Consistent with 100-trade lookback requirement.

fn update(&mut self, won: bool) {
    self.ew_wins = self.ew_wins * DECAY + if won { 1.0 } else { 0.0 };
    self.ew_total = self.ew_total * DECAY + 1.0;
    self.wins += won as u32;
    self.total += 1;
}

fn win_rate(&self) -> Option<f64> {
    if self.ew_total < 3.0 { return None; }
    Some(self.ew_wins / self.ew_total)
}
```

### 3.4 Kelly Modulator Computation

The modulator adjusts score-based allocation using empirical bucket performance:

```rust
fn compute_kelly_modulator(
    final_score: u8,
    zone: SizingZone,
    state: &SniperSizerState,
    config: &SniperSizingConfig,
) -> (u16, SizingDataSource) {
    // modulator in bps: 10000 = 1.0×, 5000 = 0.5×, 20000 = 2.0×
    let bucket = state.kelly_buckets.get(final_score, zone);

    match bucket.win_rate() {
        None => (10000, SizingDataSource::Prior),  // no data, neutral
        Some(empirical_wr) => {
            // Blend with prior
            let raw_prior = prior_probability(final_score, config);
            let zone_adj = zone.prior_adjustment_bps() as f64 / 10000.0;
            let prior_p = raw_prior * zone_adj;
            let strength = config.prior_strength_bps as f64 / 10000.0;
            let n = bucket.ew_total;
            let w = strength / (strength + n);
            let blended_wr = w * prior_p + (1.0 - w) * empirical_wr;

            // Kill check: enough data + terrible win rate
            if bucket.total >= config.bucket_kill_min_trades && blended_wr < 0.10 {
                return (0, SizingDataSource::BucketKilled);
            }

            // Modulator = blended / prior
            if prior_p < 0.01 { return (10000, SizingDataSource::Blended); }
            let modulator = (blended_wr / prior_p).clamp(0.50, 2.00);
            let mod_bps = (modulator * 10000.0) as u16;

            let src = if w < 0.10 { SizingDataSource::Empirical }
                      else { SizingDataSource::Blended };
            (mod_bps, src)
        }
    }
}
```

**Lifecycle:**
1. **Bootstrap (0-50 trades):** Flat 0.02 SOL. Modulator irrelevant.
2. **Early (50-150 trades):** Most buckets have < 3 ew_total → modulator = 1.0 (prior only). Score allocation dominates.
3. **Maturing (150-300 trades):** Buckets fill. Blended win rates nudge modulator. Bad buckets start getting penalized.
4. **Mature (300+ trades):** Empirical dominates. Good buckets → modulator > 1.0 → bigger positions. Bad buckets → killed.

---

## 4. Zone-Aware Position Sizing

### Zone Multiplier Table

Applied **after** allocation and modulator. Explicit, auditable, never buried in score logic.

| Zone | real_sol | mult_bps | Multiplier | Rationale |
|------|----------|----------|------------|-----------|
| Early Optimal | 2.0–5.0 | 6000 | 0.60× | Thin exit liquidity. Position = 2-5% of curve. |
| Peak Optimal | 5.0–15.0 | 10000 | 1.00× | Best risk/reward. Full sizing. |
| Conditional | 15.0–20.0 | 5000 | 0.50× | >0.84 SOL follow-on. Earlier entrants profit-taking. |

### SizingZone Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SizingZone {
    EarlyOptimal,   // real_sol 2.0–5.0
    PeakOptimal,    // real_sol 5.0–15.0
    Conditional,    // real_sol 15.0–20.0
}

impl SizingZone {
    pub fn from_real_sol(real_sol: f64) -> Option<Self> {
        if real_sol < 2.0 { None }
        else if real_sol < 5.0 { Some(Self::EarlyOptimal) }
        else if real_sol < 15.0 { Some(Self::PeakOptimal) }
        else if real_sol <= 20.0 { Some(Self::Conditional) }
        else { None }
    }

    pub fn b_eff_bps(self) -> u16 {
        match self {
            Self::EarlyOptimal => 650,
            Self::PeakOptimal  => 900,
            Self::Conditional  => 850,
        }
    }

    pub fn zone_mult_bps(self) -> u16 {
        match self {
            Self::EarlyOptimal => 6000,
            Self::PeakOptimal  => 10000,
            Self::Conditional  => 5000,
        }
    }

    pub fn prior_adjustment_bps(self) -> u16 {
        match self {
            Self::EarlyOptimal => 9000,
            Self::PeakOptimal  => 10000,
            Self::Conditional  => 7500,
        }
    }

    pub fn bucket_index(self) -> usize {
        match self {
            Self::EarlyOptimal => 0,
            Self::PeakOptimal  => 1,
            Self::Conditional  => 2,
        }
    }
}
```

### Zone × Score Interaction

Zone multiplier is independent of score allocation. A score-80 token gets:
- Peak optimal: `0.80 alloc × 1.0 zone = 0.80 effective`
- Early optimal: `0.80 alloc × 0.6 zone = 0.48 effective`
- Conditional: `0.80 alloc × 0.5 zone = 0.40 effective`

Same signal quality, different risk profiles → different position sizes. This is the correct behavior.

---

## 5. Score-to-Size Mapping

### Continuous Allocation Function

Piecewise linear producing allocation fraction (bps) of effective max position:

```rust
fn score_to_allocation_bps(final_score: u8) -> u16 {
    match final_score {
        0..=39   => 0,
        40..=49  => 1000 + (final_score as u16 - 40) * 100,    // 1000–1900 bps
        50..=64  => 2000 + (final_score as u16 - 50) * 200,    // 2000–4800 bps
        65..=79  => 5000 + (final_score as u16 - 65) * 200,    // 5000–7800 bps
        80..=100 => 8000 + (final_score as u16 - 80) * 100,    // 8000–10000 bps
        _        => 0,
    }
}

// Effective position for peak optimal zone, max wallet, no modulator:
// pos = max(0.10) × alloc × 0.50(half-kelly) × 1.0(zone) × 1.0(dd)
//
// score=40 → 1000bps → 0.10 × 0.10 × 0.50 = 0.005 → clamped to 0.01 SOL
// score=50 → 2000bps → 0.10 × 0.20 × 0.50 = 0.010 SOL
// score=65 → 5000bps → 0.10 × 0.50 × 0.50 = 0.025 SOL
// score=80 → 8000bps → 0.10 × 0.80 × 0.50 = 0.040 SOL
// score=100 → 10000bps → 0.10 × 1.00 × 0.50 = 0.050 SOL
```

### Named Tiers (Logging Only)

Tiers don't affect sizing — purely for post-hoc analysis and monitoring:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SizingTier {
    Probe,       // 40-49: minimum viable, data collection
    Standard,    // 50-64: bread and butter
    Strong,      // 65-79: above-average signal
    Conviction,  // 80-100: exceptional signal, full size
}

impl SizingTier {
    pub fn from_score(score: u8) -> Option<Self> {
        match score {
            40..=49  => Some(Self::Probe),
            50..=64  => Some(Self::Standard),
            65..=79  => Some(Self::Strong),
            80..=100 => Some(Self::Conviction),
            _        => None,
        }
    }
}
```

### Minimum Position Enforcement

Any trade that passes all gates and thresholds always gets at least `MIN_POSITION_SOL` (0.01 SOL). We've already decided to trade — the minimum ensures economically meaningful data collection.

---

## 6. Wallet Sizing Constraints & Drawdown Protection

### Hard Constraints

| Constant | Value | Lamports | Purpose |
|----------|-------|----------|---------|
| `MIN_POSITION_SOL` | 0.01 | 10,000,000 | Floor per trade |
| `MAX_POSITION_SOL` | 0.10 | 100,000,000 | Ceiling per trade |
| `KELLY_FRACTION` | 0.50 | — | Half-Kelly |
| `MAX_SINGLE_RISK_PCT` | 20% | — | Max wallet fraction per trade |
| `MIN_WALLET_TO_TRADE` | 0.013 | 13,000,000 | Below this: halt trading |
| `BOOTSTRAP_TRADES` | 50 | — | Flat 0.02 SOL period |
| `BOOTSTRAP_SIZE` | 0.02 | 20,000,000 | Bootstrap position |
| `LOOKBACK` | 100 | — | Outcome history window |

### Wallet-Proportional Cap

```
wallet_cap = wallet_sol × MAX_SINGLE_RISK_PCT
effective_max = min(MAX_POSITION_SOL, wallet_cap)

// 0.30 SOL wallet → cap 0.06 → max 0.06 SOL
// 1.00 SOL wallet → cap 0.20 → max 0.10 SOL (config ceiling)
// 0.03 SOL wallet → cap 0.006 → below MIN → don't trade
```

### Drawdown Protection

Track high-water mark (HWM) of wallet balance:

```
dd_pct = (wallet_hwm - wallet_sol) / wallet_hwm

drawdown_mult_bps:
    dd_pct < 0.10    → 10000 (1.00×) — normal operation
    dd_pct 0.10–0.25 →  7500 (0.75×) — moderate drawdown
    dd_pct 0.25–0.40 →  5000 (0.50×) — significant drawdown
    dd_pct > 0.40    →  2500 (0.25×) — severe drawdown, survival mode
```

**HWM rules:**
- Updated only on trade outcomes (wallet balance check post-settlement)
- Never automatically decreases
- Manual reset available for deposits (`reset_hwm` function)
- Initial HWM set to wallet balance at first trade

### Halt Condition

```
if wallet_sol < MIN_WALLET_TO_TRADE (0.013 SOL):
    log warning, refuse to trade, preserve wallet for rent/fees
```

---

## 7. Rust Implementation Spec

### 7.1 Core Structs

```rust
use serde::{Deserialize, Serialize};

// ── Kelly Bucket ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyBucket {
    pub wins: u32,
    pub total: u32,
    pub ew_wins: f64,
    pub ew_total: f64,
}

impl Default for KellyBucket {
    fn default() -> Self {
        Self { wins: 0, total: 0, ew_wins: 0.0, ew_total: 0.0 }
    }
}

impl KellyBucket {
    pub fn update(&mut self, won: bool, decay: f64) {
        self.ew_wins = self.ew_wins * decay + if won { 1.0 } else { 0.0 };
        self.ew_total = self.ew_total * decay + 1.0;
        self.wins += won as u32;
        self.total += 1;
    }

    pub fn win_rate(&self) -> Option<f64> {
        if self.ew_total < 3.0 { return None; }
        Some(self.ew_wins / self.ew_total)
    }
}

// ── Bucket Grid (5 score × 3 zone = 15 buckets) ─────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyBuckets {
    /// [score_bucket_idx][zone_idx]
    pub buckets: [[KellyBucket; 3]; 5],
}

impl Default for KellyBuckets {
    fn default() -> Self {
        Self { buckets: Default::default() }
    }
}

impl KellyBuckets {
    pub fn get(&self, score: u8, zone: SizingZone) -> &KellyBucket {
        &self.buckets[score_bucket_index(score)][zone.bucket_index()]
    }
    pub fn get_mut(&mut self, score: u8, zone: SizingZone) -> &mut KellyBucket {
        &mut self.buckets[score_bucket_index(score)][zone.bucket_index()]
    }
}

fn score_bucket_index(score: u8) -> usize {
    match score {
        0..=49   => 0,
        50..=59  => 1,
        60..=69  => 2,
        70..=79  => 3,
        _        => 4,  // 80-100+
    }
}

// ── Trade Outcome Record ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeOutcomeRecord {
    pub final_score: u8,
    pub zone: SizingZone,
    pub won: bool,
    pub pnl_lamports: i64,
    pub position_lamports: u64,
    pub ts_ms: u64,
}

// ── Persistent Sizer State ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperSizerState {
    pub kelly_buckets: KellyBuckets,
    pub total_trades: u32,
    pub total_wins: u32,
    pub wallet_hwm_lamports: u64,
    pub recent_outcomes: Vec<TradeOutcomeRecord>,
}

impl Default for SniperSizerState {
    fn default() -> Self {
        Self {
            kelly_buckets: KellyBuckets::default(),
            total_trades: 0,
            total_wins: 0,
            wallet_hwm_lamports: 0,
            recent_outcomes: Vec::with_capacity(128),
        }
    }
}

impl SniperSizerState {
    pub fn save(&self, path: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    pub fn load_or_default(path: &str) -> Self {
        std::fs::read_to_string(path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}
```

### 7.2 Sizing Result

```rust
#[derive(Debug, Clone)]
pub struct SizingResult {
    pub position_lamports: u64,
    pub position_sol: f64,
    pub sizing_tier: SizingTier,
    pub zone: SizingZone,
    pub estimated_p: f64,
    pub b_eff_bps: u16,
    pub allocation_bps: u16,
    pub modulator_bps: u16,
    pub zone_mult_bps: u16,
    pub dd_mult_bps: u16,
    pub effective_max_lamports: u64,
    pub is_bootstrap: bool,
    pub bucket_total_trades: u32,
    pub data_source: SizingDataSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SizingDataSource {
    Bootstrap,            // flat 0.02 SOL
    Prior,                // sigmoid prior only
    Blended,              // prior + empirical mix
    Empirical,            // empirical dominant (prior < 10%)
    BucketKilled,         // negative edge, refuse to trade
    InsufficientWallet,   // wallet too low
}
```

### 7.3 Core Sizing Function

```rust
pub fn compute_position_size(
    final_score: u8,
    real_sol: f64,
    wallet_lamports: u64,
    state: &SniperSizerState,
    config: &SniperSizingConfig,
) -> Option<SizingResult> {
    // ── 1. Wallet minimum ────────────────────────────────────────
    if wallet_lamports < config.min_wallet_to_trade_lamports {
        return None;
    }

    // ── 2. Zone (fails if G4 shouldn't have passed) ─────────────
    let zone = SizingZone::from_real_sol(real_sol)?;

    // ── 3. Effective max (wallet-proportional cap) ──────────────
    let wallet_cap = (wallet_lamports as u128
        * config.max_single_risk_bps as u128 / 10000) as u64;
    let eff_max = config.max_position_lamports.min(wallet_cap);
    if eff_max < config.min_position_lamports {
        return None;
    }

    // ── 4. Bootstrap ─────────────────────────────────────────────
    if state.total_trades < config.bootstrap_trade_count {
        let pos = config.bootstrap_position_lamports
            .min(eff_max).max(config.min_position_lamports);
        return Some(SizingResult {
            position_lamports: pos,
            position_sol: pos as f64 / 1e9,
            sizing_tier: SizingTier::from_score(final_score)
                .unwrap_or(SizingTier::Probe),
            zone,
            estimated_p: 0.0,
            b_eff_bps: zone.b_eff_bps(),
            allocation_bps: 10000,
            modulator_bps: 10000,
            zone_mult_bps: 10000,
            dd_mult_bps: 10000,
            effective_max_lamports: eff_max,
            is_bootstrap: true,
            bucket_total_trades: 0,
            data_source: SizingDataSource::Bootstrap,
        });
    }

    // ── 5. Score-based allocation ────────────────────────────────
    let alloc_bps = score_to_allocation_bps(final_score);
    if alloc_bps == 0 { return None; }

    // ── 6. Kelly modulator ───────────────────────────────────────
    let (mod_bps, data_source) =
        compute_kelly_modulator(final_