# Architecture: BayesianSignal Module

> **File:** `rust/pump-quant-core/src/engine/bayesian_signal.rs`  
> **Replaces:** `signal_engine.rs` (665 lines → ~250 lines)  
> **Budget:** <15ns per update, <10ns for score, zero heap, integer-only  

---

## 1. Purpose

Replace the 12-feature weighted composite score (0–1000, dimensionless) with a
**Bayesian posterior half-Kelly fraction** `f̂*(t)` in permille. Thresholds are
derived from the entry conviction's `f*_entry`, not magic numbers.

The system now speaks one language everywhere: Kelly fractions.

---

## 2. Struct Layout (12 bytes)

```rust
/// Bayesian posterior tracker for pump-alive conviction.
/// Updated on every buy/sell event and every 500ms decay tick.
/// 12 bytes total — inlined into RideState v3 (no separate allocation).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct BayesianSignal {
    /// Beta distribution α × 16 (4-bit fractional precision).
    /// Range: [16, 65535]. Minimum = 1.0 (16 >> 4).
    pub alpha_x16: u16,        // offset +0

    /// Beta distribution β × 16.
    /// Range: [16, 65535]. Minimum = 1.0.
    pub beta_x16: u16,         // offset +2

    /// Current reward ratio estimate × 100.
    /// Initialized from EntryConviction.r_x100. Updated upward only.
    pub r_est_x100: u16,       // offset +4

    /// Peak MFE (maximum favorable excursion) in basis points from entry.
    /// Used for R̂(t) upward-only update.
    pub peak_mfe_bp: i16,      // offset +6

    /// Kelly f* at entry in permille (copied from EntryConviction).
    /// Immutable after init. Used as denominator for signal thresholds.
    pub entry_f_permille: u16, // offset +8

    /// Entry win probability in permille (copied from EntryConviction).
    /// Used to calibrate the initial Beta prior.
    pub entry_p_permille: u16, // offset +10
}
```

**Static assertion:**
```rust
const _: () = assert!(core::mem::size_of::<BayesianSignal>() == 12);
```

---

## 3. FeedSource Enum

```rust
/// Data feed that produced this evidence event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FeedSource {
    PumpPortal  = 0,
    Helius      = 1,
    CoreCast    = 2,
    ShredStream = 3,
}
```

---

## 4. Evidence Weight Constants

```rust
/// Evidence weight multipliers indexed by [is_buy as usize][FeedSource as usize].
/// Units: raw weight before sol-amount scaling.
///
///               PumpPortal  Helius  CoreCast  ShredStream
///  buy  row:   [10,        10,     10,       12        ]
///  sell row:   [10,        10,     25,       15        ]
///
/// CoreCast sells get 2.5× weight because CoreCast can verify creator identity.
/// ShredStream gets a slight boost (pre-confirmation speed advantage).
pub const EVIDENCE_WEIGHTS: [[u8; 4]; 2] = [
    [10, 10, 10, 12], // is_buy = true  (index 0 = !true)
    [10, 10, 25, 15], // is_buy = false (index 1 = !false)
];
// NOTE: Indexed as EVIDENCE_WEIGHTS[(!is_buy) as usize][source as usize]
//   When is_buy=true:  !true = false = 0 → row 0 (buy weights)
//   When is_buy=false: !false = true = 1 → row 1 (sell weights)
// Wait — that's inverted. Correction:
// EVIDENCE_WEIGHTS[0] = buy weights, EVIDENCE_WEIGHTS[1] = sell weights.
// Index: EVIDENCE_WEIGHTS[is_sell as usize][source as usize]
//   is_sell = !is_buy.
```

**Corrected constant (canonical form):**
```rust
/// Index: EVIDENCE_WEIGHTS[event_type][source]
///   event_type 0 = buy, 1 = sell
pub const EVIDENCE_WEIGHTS: [[u8; 4]; 2] = [
    //  PumpPortal  Helius  CoreCast  ShredStream
    [10,        10,     10,       12],   // buy
    [10,        10,     25,       15],   // sell
];
```

**Accessed as:** `EVIDENCE_WEIGHTS[(!is_buy) as usize][source as usize]`  
(Rust: `false as usize = 0`, `true as usize = 1`, so `!is_buy` for sell = `true` = index 1.)

**Special override weights (passed as `weight_mult` parameter):**
```rust
/// Creator sell — worth 5× a normal sell (insider information).
pub const CREATOR_SELL_WEIGHT: u8 = 50;

/// Whale sell (>2 SOL) — worth 3× a normal sell.
pub const WHALE_SELL_WEIGHT: u8 = 30;

/// Bonus α increment for a unique new buyer wallet.
/// Applied as additional alpha_x16 increment after normal update.
pub const UNIQUE_BUYER_BONUS: u8 = 5;
```

**Normal trades use `weight_mult = 10` (1.0× baseline).** The `/10` in the
formula normalizes so that weight_mult=10 is neutral.

---

## 5. Prior Initialization from EntryConviction

```rust
/// Prior pseudo-observation count by conviction tier.
/// LOW (tier=0):  6 total → weak prior, easily swayed by 3-4 events
/// MED (tier=1):  9 total → moderate prior, needs ~5-6 events to shift
/// HIGH (tier=2): 13 total → strong prior, needs ~8-10 events to override
const PRIOR_STRENGTH: [u16; 3] = [6, 9, 13];

impl BayesianSignal {
    /// Initialize from EntryConviction at position open.
    ///
    /// Sets Beta(α₀, β₀) such that α₀/(α₀+β₀) ≈ p_entry
    /// and α₀ + β₀ = PRIOR_STRENGTH[tier].
    ///
    /// All values stored ×16 for 4-bit fractional precision.
    #[inline(always)]
    pub fn from_conviction(
        p_permille: u16,
        r_x100: u16,
        f_permille: u16,
        conviction_tier: u8,
    ) -> Self {
        let tier = (conviction_tier as usize).min(2);
        let total = PRIOR_STRENGTH[tier];

        // α₀ = round(p × total / 1000), clamped to [1, total-1]
        let alpha_raw = ((p_permille as u32 * total as u32 + 500) / 1000)
            .max(1)
            .min(total as u32 - 1) as u16;
        let beta_raw = total - alpha_raw;

        Self {
            alpha_x16: alpha_raw << 4,   // ×16
            beta_x16:  beta_raw << 4,    // ×16
            r_est_x100: r_x100,
            peak_mfe_bp: 0,
            entry_f_permille: f_permille,
            entry_p_permille: p_permille,
        }
    }
}
```

### Initialization Examples

| Tier | p_permille | total | α₀_raw | β₀_raw | α_x16 | β_x16 | p̂=α/(α+β) |
|------|-----------|-------|--------|--------|-------|-------|-----------|
| LOW  | 560       | 6     | 3      | 3      | 48    | 48    | 0.500     |
| MED  | 560       | 9     | 5      | 4      | 80    | 64    | 0.556     |
| HIGH | 560       | 13    | 7      | 6      | 112   | 96    | 0.538     |

---

## 6. Core Functions

### 6.1 `update_evidence` — <10ns

```rust
/// Update Beta posterior with a trade event.
///
/// `is_buy`:      true → α evidence, false → β evidence
/// `sol_msol`:    trade size in milli-SOL (1 SOL = 1000)
/// `source`:      which feed reported this event (for weight lookup)
/// `weight_mult`: caller-supplied multiplier:
///                  10 = normal trade (1.0×)
///                  CREATOR_SELL_WEIGHT (50) = creator dumping (5.0×)
///                  WHALE_SELL_WEIGHT (30) = whale sell (3.0×)
///
/// Weight formula:
///   base = EVIDENCE_WEIGHTS[is_sell][source]
///   size_factor = clamp(1 + sol_msol / 500, 1, 16)
///   w = base × size_factor × weight_mult / 10
///
/// The w value is added directly to alpha_x16 or beta_x16 (already in x16 scale
/// since base weights ~10 produce w ~20 which is ~1.25 in natural units).
///
/// Performance: 4 integer ops + 1 saturating_add + 1 branch (is_buy). <10ns.
#[inline(always)]
pub fn update_evidence(
    &mut self,
    is_buy: bool,
    sol_msol: u16,
    source: FeedSource,
    weight_mult: u8,
) {
    // Look up base weight: buy=row0, sell=row1
    let base = EVIDENCE_WEIGHTS[(!is_buy) as usize][source as usize] as u32;

    // Size scaling: 1 + sol_msol/500, capped at 16.
    //   0.1 SOL (100 msol) → 1
    //   0.5 SOL (500 msol) → 2
    //   1.0 SOL (1000 msol) → 3
    //   5.0 SOL (5000 msol) → 11
    let size_factor = (1u32 + sol_msol as u32 / 500).min(16);

    // Total weight (in x16 units approximately):
    //   Normal buy 0.5 SOL PumpPortal: 10 × 2 × 10 / 10 = 20 (~1.25 in natural)
    //   Creator sell 2 SOL CoreCast:   25 × 5 × 50 / 10 = 625 (~39 in natural)
    let w = (base * size_factor * weight_mult as u32 / 10).min(4080) as u16;

    if is_buy {
        self.alpha_x16 = self.alpha_x16.saturating_add(w);
    } else {
        self.beta_x16 = self.beta_x16.saturating_add(w);
    }
}
```

### 6.2 `decay_tick` — <5ns

```rust
/// Exponential forgetting. Called every 500ms tick.
///
/// Multiplies both α and β by 240/256 ≈ 0.9375 per tick.
/// Half-life: ln(2) / ln(256/240) ≈ 10.4 ticks × 500ms ≈ 5.2 seconds.
///
/// After 5s:  ~50% of accumulated evidence forgotten
/// After 10s: ~75% forgotten
/// After 15s: ~87.5% forgotten
///
/// Clamps to MIN_AB_X16 (16 = 1.0 in natural units) to prevent
/// division-by-zero in current_f_permille.
///
/// Performance: 2 multiplies + 2 right-shifts + 2 max. No branches. <5ns.
const DECAY_NUMER: u32 = 240;
const DECAY_DENOM_SHIFT: u32 = 8; // divide by 256
const MIN_AB_X16: u16 = 16;       // 1.0 in x16 representation

#[inline(always)]
pub fn decay_tick(&mut self) {
    self.alpha_x16 = ((self.alpha_x16 as u32 * DECAY_NUMER) >> DECAY_DENOM_SHIFT)
        .max(MIN_AB_X16 as u32) as u16;
    self.beta_x16 = ((self.beta_x16 as u32 * DECAY_NUMER) >> DECAY_DENOM_SHIFT)
        .max(MIN_AB_X16 as u32) as u16;
}
```

### 6.3 `current_f_permille` — <10ns

**Derivation:**

Let `p = p_x1000/1000`, `R = r_x100/100`.

```
f* = (p(R+1) - 1) / R
f_half = f*/2

f_half_permille = f_half × 1000
  = [(p_x1000/1000) × ((r_x100+100)/100) - 1] × 1000 / (r_x100/100) / 2
  = [p_x1000 × (r_x100+100) / 100000 - 1] × 100000 / r_x100 / 2
  = [p_x1000 × (r_x100+100) - 100000] / (2 × r_x100)
```

**Implementation:**

```rust
/// Compute current half-Kelly fraction in permille from Bayesian posterior.
///
/// Returns: signed i16. Positive = edge exists. Zero/negative = no edge → exit.
///
/// Integer formula:
///   p_x1000 = alpha_x16 × 1000 / (alpha_x16 + beta_x16)
///   numerator = p_x1000 × (r_est_x100 + 100) - 100_000
///   f_half_permille = numerator / (2 × r_est_x100)
///
/// Operation count: 3 multiplies + 1 divide + 1 subtract. <10ns.
#[inline(always)]
pub fn current_f_permille(&self) -> i16 {
    let a = self.alpha_x16 as u32;
    let b = self.beta_x16 as u32;
    let ab = a + b; // guaranteed ≥ 32 (2 × MIN_AB_X16)

    // p̂ × 1000
    let p_x1000 = (a * 1000) / ab;

    let r = self.r_est_x100.max(1) as u32;
    let r_plus_1_x100 = r + 100;

    // p_x1000 × r_plus_1_x100 max: 1000 × 65635 = 65_635_000, fits u32
    let numerator = (p_x1000 * r_plus_1_x100) as i32 - 100_000;

    // half-Kelly: / (2 × r)
    let f = numerator / (2 * r as i32);

    f.clamp(-1000, 1000) as i16
}
```

### 6.4 `signal_state` — <3ns

```rust
/// Map current f̂*(t) to a SignalState using Kelly-derived thresholds.
///
/// Thresholds are fractions of entry_f_permille:
///   StrongPump:  f̂ > 0.70 × f_entry   (conviction still strong)
///   Sustained:   f̂ > 0.35 × f_entry   (positive but decaying)
///   Weakening:   f̂ > 0                 (any positive Kelly edge)
///   Exit:        f̂ ≤ 0                 (no edge → close position)
///
/// Integer approximations:
///   0.70 ≈ 179/256 (error: -0.1%)
///   0.35 ≈ 90/256  (error: +0.5%)
///
/// Performance: 2 multiplies + 2 shifts + 3 comparisons. <3ns.
#[inline(always)]
pub fn signal_state(&self) -> SignalState {
    let f_hat = self.current_f_permille() as i32;
    let f_entry = self.entry_f_permille as i32;

    if f_entry == 0 {
        return SignalState::Exit;
    }

    // 0.70 × f_entry ≈ f_entry × 179 >> 8
    let strong_thresh = (f_entry * 179) >> 8;
    // 0.35 × f_entry ≈ f_entry × 90 >> 8
    let sustain_thresh = (f_entry * 90) >> 8;

    if f_hat > strong_thresh {
        SignalState::StrongPump
    } else if f_hat > sustain_thresh {
        SignalState::Sustained
    } else if f_hat > 0 {
        SignalState::Weakening
    } else {
        SignalState::Exit
    }
}
```

### 6.5 `update_r_estimate` — <5ns (warm path)

```rust
/// Update R̂(t) from realized PnL trajectory. Upward-only.
///
/// Called when price updates in on_tick. Only revises R̂ upward because
/// observing a higher MFE is evidence of larger available reward.
///
/// EMA-8 smoothing: R̂ = (R̂ × 7 + implied_R) / 8
///
/// `current_pnl_bp`: unrealized PnL in basis points from entry
/// `avg_loss_bp`:    configured average loss (e.g. 300bp from historical data)
#[inline(always)]
pub fn update_r_estimate(&mut self, current_pnl_bp: i16, avg_loss_bp: u16) {
    if current_pnl_bp > self.peak_mfe_bp {
        self.peak_mfe_bp = current_pnl_bp;
    }

    let avg = avg_loss_bp.max(1) as u32;
    let implied_r_x100 = (self.peak_mfe_bp.max(0) as u32 * 100) / avg;

    if implied_r_x100 > self.r_est_x100 as u32 {
        self.r_est_x100 = (((self.r_est_x100 as u32) * 7 + implied_r_x100) >> 3) as u16;
    }
}
```

---

## 7. SignalState Enum

```rust
/// Signal-driven state machine states.
/// Defined in bayesian_signal.rs, re-exported by ride_state.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalState {
    StrongPump = 0,
    Sustained  = 1,
    Weakening  = 2,
    Exit       = 3,
}
```

---

## 8. What This Replaces

| Old (signal_engine.rs)                    | New (bayesian_signal.rs)                     |
|-------------------------------------------|----------------------------------------------|
| `compute_composite_score()` — 12 features | `current_f_permille()` — 3 muls + 1 div     |
| Score 0–1000 (dimensionless)              | `f̂*` in permille (meaningful unit)           |
| Thresholds: 200/400/700 (magic numbers)   | Thresholds: 0 / 0.35f* / 0.7f* (derived)    |
| `compute_kelly_multiplier()` — sqrt LUT   | Trail ∝ f̂*/f_entry (direct proportion)       |
| `compute_lifecycle_multiplier()` — phases | Removed — lifecycle baked into Beta posterior |
| `SignalWeights` (10 tunable params)       | `EVIDENCE_WEIGHTS` (8 constants + 3 special) |
| `KellyConfig`, `LifecycleConfig` structs  | Removed — all derived from entry conviction   |
| `KELLY_SQRT_LUT` (17 entries)             | Removed — no LUT needed                       |
| `volume_acceleration_bp()`                | Removed — buy/sell rate captured in α/β       |
| `update_price_velocity_ema()`             | Removed — price info via R̂ update only        |
| `sell_pressure_ratio()`                   | Removed — sell evidence updates β directly     |

### Functions KEPT (moved to bayesian_signal.rs or kept in ride_state.rs)

| Function             | Location after refactor     | Reason                               |
|----------------------|-----------------------------|--------------------------------------|
| `count_in_window()`  | `ride_state.rs` (private)   | Sell cascade detection               |
| `bloom_insert()`     | `bayesian_signal.rs` (pub)  | Unique wallet tracking               |
| `bloom_count()`      | `bayesian_signal.rs` (pub)  | Unique wallet tracking               |

---

## 9. Test Cases with Exact Expected Values

All tests use: `p_permille=560, r_x100=1100, f_permille=248, tier=MED(1)`

### Test 1: Fresh initialization — StrongPump

```
from_conviction(560, 1100, 248, 1):
  total = PRIOR_STRENGTH[1] = 9
  alpha_raw = (560 × 9 + 500) / 1000 = 5540/1000 = 5
  beta_raw  = 9 - 5 = 4
  alpha_x16 = 80, beta_x16 = 64

current_f_permille():
  a=80, b=64, ab=144
  p_x1000 = 80000 / 144 = 555
  r=1100, r_plus_1_x100 = 1200
  numerator = 555 × 1200 - 100000 = 666000 - 100000 = 566000
  f_half = 566000 / 2200 = 257

signal_state(entry_f=248):
  strong_thresh = 248 × 179 >> 8 = 44392 >> 8 = 173
  sustain_thresh = 248 × 90 >> 8 = 22320 >> 8 = 87
  257 > 173 → StrongPump

EXPECTED: f_hat = 257, state = StrongPump ✓
```

### Test 2: Five sells drive to Sustained

```
Start: alpha_x16=80, beta_x16=64
5 sells × 1.0 SOL (1000 msol), PumpPortal, weight_mult=10:
  base = EVIDENCE_WEIGHTS[1][0] = 10
  size_factor = 1 + 1000/500 = 3
  w = 10 × 3 × 10 / 10 = 30 per sell
  total β added = 5 × 30 = 150
After: alpha_x16=80, beta_x16=64+150=214

current_f_permille():
  a=80, b=214, ab=294
  p_x1000 = 80000/294 = 272
  numerator = 272 × 1200 - 100000 = 326400 - 100000 = 226400
  f_half = 226400 / 2200 = 102

signal_state(entry_f=248):
  173 > 102 > 87 → Sustained

EXPECTED: f_hat = 102, state = Sustained ✓
```

### Test 3: Ten more sells drive to Exit

```
Start: alpha_x16=80, beta_x16=214 (continuing from Test 2)
10 more sells × 0.5 SOL (500 msol), PumpPortal, weight_mult=10:
  base=10, size_factor=1+500/500=2, w=10×2×10/10=20 per sell
  total β added = 10 × 20 = 200
After: alpha_x16=80, beta_x16=214+200=414

current_f_permille():
  a=80, b=414, ab=494
  p_x1000 = 80000/494 = 161
  numerator = 161 × 1200 - 100000 = 193200 - 100000 = 93200
  f_half = 93200 / 2200 = 42

signal_state(entry_f=248):
  87 > 42 > 0 → Weakening

Still not Exit! 15 sells weren't enough. Let's continue:

5 more sells × 1.0 SOL, w=30 each → β += 150
After: alpha_x16=80, beta_x16=414+150=564

current_f_permille():
  a=80, b=564, ab=644
  p_x1000 = 80000/644 = 124
  numerator = 124 × 1200 - 100000 = 148800 - 100000 = 48800
  f_half = 48800 / 2200 = 22

Still Weakening (22 > 0). For Exit we need f ≤ 0:
  Need p_x1000 × 1200 ≤ 100000 → p_x1000 ≤ 83 → a/(a+b) ≤ 0.083
  80/(80+b) ≤ 0.083 → b ≥ 80/0.083 - 80 = 883

Let's jump: 20 more sells × 2 SOL (2000 msol), w = 10×5×10/10 = 50
After: beta_x16 = 564 + 20×50 = 1564

current_f_permille():
  a=80, b=1564, ab=1644
  p_x1000 = 80000/1644 = 48
  numerator = 48 × 1200 - 100000 = 57600 - 100000 = -42400
  f_half = -42400 / 2200 = -19

EXPECTED: f_hat = -19, state = Exit ✓
(Takes heavy selling to overcome MED prior — this is intentional.)
```

### Test 4: Creator sell — massive β spike → Weakening

```
Start: alpha_x16=80, beta_x16=64 (fresh MED)
1 creator sell, 2.0 SOL (2000 msol), CoreCast, weight_mult=CREATOR_SELL_WEIGHT(50):
  base = EVIDENCE_WEIGHTS[1][2] = 25
  size_factor = 1 + 2000/500 = 5
  w = 25 × 5 × 50 / 10 = 625
After: alpha_x16=80, beta_x16=64+625=689

current_f_permille():
  a=80, b=689, ab=769
  p_x1000 = 80000/769 = 104
  numerator = 104 × 1200 - 100000 = 124800 - 100000 = 24800
  f_half = 24800 / 2200 = 11

signal_state(entry_f=248):
  87 > 11 > 0 → Weakening

EXPECTED: f_hat = 11, state = Weakening ✓
(Bayesian alone doesn't trigger Exit from one creator sell because prior fights back.
 RideState emergency flag CREATOR_SELL handles instant exit independently.)
```

### Test 5: Healthy pump — 8 buys, 1 sell → StrongPump

```
Start: alpha_x16=80, beta_x16=64 (fresh MED)

8 buys × 0.5 SOL (500 msol), PumpPortal, weight_mult=10:
  base=10, size_factor=1+500/500=2, w=10×2×10/10=20 per buy
  α added = 8 × 20 = 160
After buys: alpha_x16=80+160=240, beta_x16=64

1 sell × 0.3 SOL (300 msol), PumpPortal, weight_mult=10:
  base=10, size_factor=1+300/500=1 (integer), w=10×1×10/10=10
After sell: alpha_x16=240, beta_x16=64+10=74

current_f_permille():
  a=240, b=74, ab=314
  p_x1000 = 240000/314 = 764
  numerator = 764 × 1200 - 100000 = 916800 - 100000 = 816800
  f_half = 816800 / 2200 = 371

signal_state(entry_f=248):
  strong_thresh = 173
  371 > 173 → StrongPump

EXPECTED: f_hat = 371, state = StrongPump ✓
(Healthy pump with 8:1 buy:sell ratio → conviction exceeds entry.)
```

### Test 6: Decay preserves ratio (10 ticks = 5 seconds)

```
Start: alpha_x16=80, beta_x16=64

After 10 decay ticks (each: x = x × 240 >> 8):
  Tick  1: α=75, β=60    (80×240/256=75.0 → 75)
  Tick  2: α=70, β=56
  Tick  3: α=65, β=52
  Tick  4: α=60, β=48
  Tick  5: α=56, β=45
  Tick  6: α=52, β=42
  Tick  7: α=48, β=39
  Tick  8: α=45, β=36
  Tick  9: α=42, β=33
  Tick 10: α=39, β=30

p̂ before: 80/144 = 0.556
p̂ after:  39/69  = 0.565 (ratio approximately preserved, small drift from rounding)

current_f_permille():
  a=39, b=30, ab=69
  p_x1000 = 39000/69 = 565
  numerator = 565 × 1200 - 100000 = 678000 - 100000 = 578000
  f_half = 578000 / 2200 = 262

EXPECTED: f_hat ≈ 262, state = StrongPump
(Decay reduces confidence/sample-size but preserves the α/β ratio.
 The posterior p̂ barely moves — decay makes future evidence more impactful,
 it doesn't assume the pump is dying. This is correct Bayesian forgetting.)
```

### Test 7: LOW tier initialization — weaker prior, faster state transitions

```
from_conviction(560, 1100, 248, 0):  // tier=LOW
  total = PRIOR_STRENGTH[0] = 6
  alpha_raw = (560 × 6 + 500) / 1000 = 3860/1000 = 3
  beta_raw = 6 - 3 = 3
  alpha_x16 = 48, beta_x16 = 48

current_f_permille():
  a=48, b=48, ab=96
  p_x1000 = 48000/96 = 500
  numerator = 500 × 1200 - 100000 = 600000 - 100000 = 500000
  f_half = 500000 / 2200 = 227

signal_state(entry_f=248): strong_thresh=173, 227>173 → StrongPump

Now 3 sells × 1 SOL, PumpPortal, weight_mult=10 (w=30 each):
After: alpha_x16=48, beta_x16=48+90=138

current_f_permille():
  a=48, b=138, ab=186
  p_x1000 = 48000/186 = 258
  numerator = 258 × 1200 - 100000 = 309600 - 100000 = 209600
  f_half = 209600 / 2200 = 95

signal_state: 173>95>87 → Sustained

EXPECTED: LOW tier drops to Sustained after just 3 sells (vs 5 for MED).
Weaker prior = faster state transitions. ✓
```

---

## 10. Performance Budget

| Function            | Operations                  | Measured Target | Notes                              |
|---------------------|-----------------------------|-----------------|------------------------------------|
| `update_evidence()` | 4 mul + 1 sat_add + 1 branch | <10ns          | Single cache line touch            |
| `decay_tick()`      | 2 mul + 2 shift + 2 max     | <5ns           | No branches                        |
| `current_f_permille()` | 3 mul + 1 div + 1 sub     | <10ns          | div by u32 → compiler reciprocal   |
| `signal_state()`    | 2 mul + 2 shift + 3 cmp     | <3ns           | Inlined into on_tick               |
| `update_r_estimate()` | 1 cmp + (rarely) 1 mul + 1 shift | <5ns    | Only fires on new MFE peak         |

**Total hot-path cycle: `update_evidence + decay_tick + current_f_permille + signal_state` ≈ 25ns.**

All functions are `#[inline(always)]`. No heap allocation, no f64, no String/Vec/Box.

---

## 11. Module Structure

```
engine/
├── bayesian_signal.rs     ← NEW (this spec)
│   ├── BayesianSignal struct (12 bytes)
│   ├── FeedSource enum
│   ├── SignalState enum (moved from ride_state.rs)
│   ├── EVIDENCE_WEIGHTS, CREATOR_SELL_WEIGHT, etc.
│   ├── bloom_insert(), bloom_count()  (moved from signal_engine.rs)
│   └── from_conviction(), update_evidence(), decay_tick(),
│       current_f_permille(), signal_state(), update_r_estimate()
│
├── ride_state.rs          ← MODIFIED (see ARCH_RIDESTATE_V3.md)
│   ├── RideState v3 struct (128 bytes)
│   ├── count_in_window()  (kept private, sell cascade only)
│   └── RideDecision, RideExitReason, ride_flags
│
├── kelly_sizing.rs        ← UNCHANGED (entry-time only, not hot path)
│
└── signal_engine.rs       ← DELETED (replaced by bayesian_signal.rs)
```

---

## 12. Migration Path

1. **Phase 1:** Add `bayesian_signal.rs` alongside `signal_engine.rs`. Feature-flag `cfg(feature = "bayesian")`.
2. **Phase 2:** Log both old composite score and new f̂* in JSONL for A/B comparison.
3. **Phase 3:** Once validated, remove `signal_engine.rs` and the feature flag.

RideState v3 (see `ARCH_RIDESTATE_V3.md`) is designed to be the Phase 3 layout.