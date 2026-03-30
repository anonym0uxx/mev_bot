# ARCH: Dual Signal Mode — Feature-Flagged Bayesian + Composite Shadow System

**Status:** Design  
**Depends on:** QUANT_SIGNAL_KELLY_COHERENCE.md (Bayesian spec), RideState v2 (current)  
**Produces:** RideState v3 (Bayesian-native), shadow composite scoring, dv8 JSONL, API extensions  

---

## 1. Problem

We need to deploy the Bayesian signal engine (QUANT_SIGNAL_KELLY_COHERENCE.md) without losing the ability to:
1. Compare its performance against the current composite scoring system
2. Instantly revert if Bayesian underperforms
3. Collect dual-signal data for offline analysis

The 128-byte RideState cache-line budget cannot fit both signal systems simultaneously. We need an architecture that runs both in parallel without violating the size constraint.

## 2. Solution Overview

```
                        ┌──────────────────────────┐
                        │      EngineConfig         │
                        │  use_bayesian_signal: bool│
                        │  shadow_composite: bool   │
                        └──────────┬───────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              ▼                    ▼                     ▼
     ┌─────────────────┐  ┌──────────────┐    ┌──────────────────┐
     │  RideState v3   │  │ OpenPosition │    │  SignalEngine     │
     │  (128 bytes)    │  │  (no size    │    │  (pure functions) │
     │  Bayesian ONLY  │  │   limit)     │    │  composite_score  │
     │  - alpha_x16    │  │              │    └──────────────────┘
     │  - beta_x16     │  │ + shadow_    │              │
     │  - r_est_x100   │  │   composite  │◄─────────────┘
     │  - f_hat_permil │  │   _score     │   shadow computation
     └─────────────────┘  │ + shadow_    │   on every tick
              │           │   peak_comp  │
              │           │ + shadow_    │
              │           │   signal_st  │
              │           └──────────────┘
              │                    │
              ▼                    ▼
        ┌─────────────────────────────────┐
        │  Exit Decision Mux              │
        │  if use_bayesian → f̂*(t) drives │
        │  else → shadow_composite drives │
        └─────────────────────────────────┘
              │
              ▼
        ┌──────────────┐
        │  JSONL dv8   │
        │  (both sets) │
        └──────────────┘
```

**Key invariant:** RideState v3 is 128 bytes, Bayesian-only. The old composite score is a shadow computation stored in OpenPosition (heap-allocated, no size constraint). Both run on every tick. The feature flag selects which one drives the exit decision.

---

## 3. Feature Flag

### 3.1 Config Field

```rust
// In EngineConfig (config.rs):
/// When true, Bayesian f̂*(t) drives exit decisions for NEW positions.
/// Existing open positions keep the mode they were opened with.
/// Default: false (composite score drives exits).
pub use_bayesian_signal: bool,

/// When true, compute composite score as a shadow signal alongside
/// the primary (for logging/comparison). Default: true in paper, false in live.
pub shadow_composite_enabled: bool,
```

### 3.2 JSON Config (canary.json)

```json
{
  "mev": {
    "signal": {
      "useBayesianSignal": false,
      "shadowCompositeEnabled": true,
      "bayesianDecayRate": 240,
      "bayesianPriorStrength": [6, 9, 13],
      "divergenceAlertThreshold": 10
    }
  }
}
```

### 3.3 Runtime Toggle Semantics

When `useBayesianSignal` changes via config hot-reload:
- **Does NOT affect open positions.** Each `OpenPosition` records `signal_mode_at_open: SignalMode` at open time.
- **Takes effect on the NEXT `position_manager.open_position()` call.**
- The change is logged to `config_changes.jsonl`:
  ```json
  {"ts_ms": 1711756800000, "field": "useBayesianSignal", "old": false, "new": true}
  ```

### 3.4 SignalMode Enum

```rust
/// Which signal system drives exit decisions for a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalMode {
    Composite = 0,
    Bayesian  = 1,
}

impl SignalMode {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Composite => "composite",
            Self::Bayesian  => "bayesian",
        }
    }
}
```

---

## 4. RideState v3 — Bayesian-Native (128 bytes)

### 4.1 Layout

RideState v3 replaces the composite scoring fields with Bayesian posterior tracking. Net change: 0 bytes — the fields swap 1:1.

```rust
/// Signal-driven RIDE exit state v3. 128 bytes exactly.
///
/// Cache line 0 (bytes 0–63): HOT — accessed every event.
/// Cache line 1 (bytes 64–127): WARM — ring buffers + bloom.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct RideState {
    // ── Cache line 0: trail + timing + counters + Bayesian ────────

    // Trail state (16 bytes)
    pub peak_mvsol: u32,           // highest vSOL seen
    pub trail_stop_mvsol: u32,     // current trail stop (ratchets up)
    pub entry_mvsol: u32,          // vSOL at entry
    pub current_trail_bp: u16,     // active trail distance in vSOL bp
    pub state: SignalState,        // u8: signal-driven state
    pub flags: u8,                 // bitflags

    // Timing (16 bytes)
    pub ride_start_ms: u64,        // entry timestamp
    pub last_buy_ms: u64,          // last buy event timestamp

    // Counters (16 bytes)
    pub buys_after_entry: u16,
    pub sells_after_entry: u16,
    pub unique_wallets: u8,        // approx via bloom filter
    _pad0: [u8; 3],
    pub confirming_vol_msol: u32,  // cumulative buy volume in milli-SOL
    pub peak_pnl_bp: i16,         // best unrealized PnL in basis points
    pub peak_pnl_ms_rel: u16,     // when peak occurred (relative to entry, ms)

    // ── Bayesian signal (16 bytes) — REPLACES old composite fields ──
    pub alpha_x16: u16,           // Beta distribution α × 16 (4-bit fractional)
    pub beta_x16: u16,            // Beta distribution β × 16
    pub r_est_x100: u16,          // Current R̂(t) estimate × 100
    pub f_hat_permille: i16,      // Current f̂*(t) in permille (signed: can go negative)
    pub entry_f_permille: u16,    // Kelly f* at entry (conviction prior)
    pub vol_accel_bp: i16,        // volume acceleration (kept for shadow composite)
    pub price_velocity: i32,      // EMA-smoothed vSOL delta/s (kept for shadow composite)

    // ── Cache line 1: ring buffers + bloom ────────────────────────

    // Buy ring: 8 entries × (u16 timestamp_rel + u16 amount_msol) = 32 bytes
    pub buy_ts_ring: [u16; 8],
    pub buy_sol_ring: [u16; 8],

    // Sell ring: 4 entries × (u16 timestamp_rel + u16 amount_msol) = 16 bytes
    pub sell_ts_ring: [u16; 4],
    pub sell_sol_ring: [u16; 4],

    // Ring indices + bloom + metadata (16 bytes)
    pub buy_ring_idx: u8,
    pub sell_ring_idx: u8,
    pub bloom_filter: [u8; 8],
    pub vol_recent_msol: u16,     // buy vol in [now-2s, now] for accel
    pub vol_prior_msol: u16,      // buy vol in [now-4s, now-2s] for accel

    // Legacy compat (2 bytes)
    pub phase: RidePhase,         // maps signal state for logging
    pub _pad2: u8,
}
```

### 4.2 Field Replacement Map (v2 → v3)

| v2 Field (removed)        | Size | v3 Replacement          | Size | Notes |
|--------------------------|------|------------------------|------|-------|
| `composite_score: u16`   | 2    | `alpha_x16: u16`       | 2    | Beta posterior α |
| `kelly_trail_mult: u16`  | 2    | `beta_x16: u16`        | 2    | Beta posterior β |
| `phase_trail_mult: u16`  | 2    | `r_est_x100: u16`      | 2    | Reward ratio estimate |
| `vol_accel_bp: i16`      | 2    | `vol_accel_bp: i16`    | 2    | **Kept** (used by shadow + accel) |
| `price_velocity: i32`    | 4    | `price_velocity: i32`  | 4    | **Kept** (used by shadow + R̂ update) |
| `peak_composite: u16`    | 2    | `f_hat_permille: i16`  | 2    | Current Kelly fraction (signed) |
| `entry_f_permille: u16`  | 2    | `entry_f_permille: u16`| 2    | **Kept** (entry conviction prior) |

**Net change: 0 bytes. Total: 128 bytes. 2 cache lines.**

### 4.3 Compile-Time Size Assertion

```rust
const _: () = assert!(core::mem::size_of::<RideState>() == 128);
```

### 4.4 Bayesian Core Methods

```rust
impl RideState {
    /// Initialize Bayesian prior from EntryConviction.
    /// Called once at position open.
    #[inline(always)]
    pub fn init_bayesian(
        &mut self,
        p_permille: u16,      // entry win probability × 1000
        r_x100: u16,          // entry reward ratio × 100
        conviction_tier: u8,  // 0=LOW, 1=MED, 2=HIGH
        prior_strengths: &[u16; 3], // [6, 9, 13] from config
    ) {
        let total = prior_strengths[conviction_tier.min(2) as usize] as u32;
        // α₀ = p × total / 1000, β₀ = total - α₀ (scaled ×16)
        let alpha_0 = ((p_permille as u32 * total * 16) / 1000) as u16;
        let beta_0 = (total * 16).saturating_sub(alpha_0 as u32) as u16;
        self.alpha_x16 = alpha_0.max(16);  // minimum 1.0 (×16)
        self.beta_x16 = beta_0.max(16);
        self.r_est_x100 = r_x100;
        self.entry_f_permille = self.compute_f_hat();
    }

    /// Update posterior on buy event evidence.
    /// Weight = min(sol_msol / 100, 16) — capped at 16 increments per event.
    #[inline(always)]
    pub fn bayesian_on_buy(&mut self, sol_msol: u16) {
        let weight = (sol_msol / 100).min(16);
        self.alpha_x16 = self.alpha_x16.saturating_add(weight);
    }

    /// Update posterior on sell event evidence.
    /// Creator sells get CREATOR_SELL_WEIGHT (50 ×16 = 800 beta increments).
    #[inline(always)]
    pub fn bayesian_on_sell(&mut self, sol_msol: u16, is_creator: bool) {
        let base_weight = (sol_msol / 100).min(16);
        let weight = if is_creator { 50 } else { base_weight };
        self.beta_x16 = self.beta_x16.saturating_add(weight);
    }

    /// Apply time decay (forgetting factor). Called every ~500ms.
    /// Shrinks both α and β toward prior, half-life ≈ 5s at DECAY_RATE=240.
    #[inline(always)]
    pub fn bayesian_decay(&mut self, decay_rate: u16) {
        self.alpha_x16 = ((self.alpha_x16 as u32 * decay_rate as u32) / 256)
            .max(16) as u16; // floor at 1.0 ×16
        self.beta_x16 = ((self.beta_x16 as u32 * decay_rate as u32) / 256)
            .max(16) as u16;
    }

    /// Compute f̂*(t) = half-Kelly fraction in permille.
    /// f* = (p̂(R+1) - 1) / R, then halved.
    /// Returns signed: negative means breakeven violated → Exit.
    ///
    /// Budget: 3 multiplies + 1 divide ≈ 8ns.
    #[inline(always)]
    pub fn compute_f_hat(&self) -> i16 {
        let alpha = self.alpha_x16 as u32;
        let beta = self.beta_x16 as u32;
        let total = alpha + beta;
        if total == 0 { return 0; }

        // p̂ × 1000 = α × 1000 / (α + β)
        let p_x1000 = (alpha * 1000) / total;
        // f* = (p̂(R+1) - 1) / R  → multiply through by 1000:
        // f*_x1000 = (p_x1000 × (r_x100 + 100) / 1000 - 100) × 1000 / r_x100
        let r_plus_1_x100 = self.r_est_x100 as i32 + 100;
        let numerator = p_x1000 as i32 * r_plus_1_x100 / 1000 - 100;
        let r = self.r_est_x100.max(1) as i32;
        let f_full = (numerator * 1000) / r;
        // Half-Kelly
        (f_full / 2).clamp(-1000, 1000) as i16
    }

    /// Map f̂*(t) to SignalState using Kelly-derived thresholds.
    /// Thresholds are fractions of entry_f_permille.
    ///
    /// f̂*(t) > 0.70 × f*_entry → StrongPump
    /// f̂*(t) > 0.35 × f*_entry → Sustained
    /// f̂*(t) > 0               → Weakening
    /// f̂*(t) ≤ 0               → Exit
    #[inline(always)]
    pub fn bayesian_signal_state(&self) -> SignalState {
        let f = self.f_hat_permille as i32;
        let f_entry = self.entry_f_permille as i32;

        // Thresholds as fractions of entry Kelly (integer: ×1000 → ×700, ×350)
        let strong_thresh = (f_entry * 700) / 1000;  // 0.70 × f*_entry
        let sustain_thresh = (f_entry * 350) / 1000;  // 0.35 × f*_entry

        if f > strong_thresh {
            SignalState::StrongPump
        } else if f > sustain_thresh {
            SignalState::Sustained
        } else if f > 0 {
            SignalState::Weakening
        } else {
            SignalState::Exit
        }
    }

    /// Compute trail width directly from f̂*(t) ratio.
    /// trail_bp = base_trail × (f̂*(t) / f*_entry) clamped to [min, max].
    /// When f̂*(t) ≤ 0, trail = 0 → triggers exit.
    #[inline(always)]
    pub fn bayesian_trail_bp(&self, base_trail_bp: u16, min_bp: u16, max_bp: u16) -> u16 {
        if self.f_hat_permille <= 0 || self.entry_f_permille == 0 {
            return 0;
        }
        let ratio = (self.f_hat_permille as u32 * 256) / self.entry_f_permille as u32;
        let trail = (base_trail_bp as u32 * ratio) / 256;
        trail.clamp(min_bp as u32, max_bp as u32) as u16
    }

    /// Full Bayesian tick: update f̂*(t), state, trail.
    /// Called from recompute_signals() when Bayesian mode is active.
    ///
    /// Budget: ~15ns (3 muls + 1 div + 2 comparisons).
    #[inline(always)]
    pub fn bayesian_recompute(
        &mut self,
        current_mvsol: u32,
        now_ms: u64,
        config: &BayesianSignalConfig,
        ride_config: &RideConfig,
    ) {
        // 1. Time decay (every tick — caller is responsible for 500ms gating)
        self.bayesian_decay(config.decay_rate);

        // 2. Update R̂(t) from realized MFE trajectory (upward only)
        if current_mvsol > self.entry_mvsol && self.entry_mvsol > 0 {
            let mfe_bp = ((current_mvsol as u64 - self.entry_mvsol as u64) * 10000
                / self.entry_mvsol as u64) as u16;
            // Implied R from MFE (simplified: R ≈ MFE_bp / 100)
            let implied_r = mfe_bp.max(1);
            if implied_r > self.r_est_x100 {
                // EMA-8 upward update: R̂ = (R̂ × 7 + implied_r) / 8
                self.r_est_x100 = ((self.r_est_x100 as u32 * 7 + implied_r as u32) / 8) as u16;
            }
        }

        // 3. Compute f̂*(t)
        self.f_hat_permille = self.compute_f_hat();

        // 4. State transition
        self.state = self.bayesian_signal_state();

        // 5. Trail width (proportional to f̂*(t) / f*_entry)
        let base_trail = match self.state {
            SignalState::StrongPump => ride_config.trail_strong_pump_bp,
            SignalState::Sustained  => ride_config.trail_sustained_bp,
            SignalState::Weakening  => ride_config.trail_weakening_bp,
            SignalState::Exit       => 0,
        };
        self.current_trail_bp = self.bayesian_trail_bp(
            base_trail,
            ride_config.kelly_min_trail_bp,
            ride_config.kelly_max_trail_bp,
        );

        // 6. Legacy phase mapping
        self.phase = match self.state {
            SignalState::StrongPump => RidePhase::Early,
            SignalState::Sustained  => RidePhase::Momentum,
            SignalState::Weakening | SignalState::Exit => RidePhase::Tighten,
        };
    }
}
```

---

## 5. OpenPosition Shadow Fields

OpenPosition is heap-allocated with no size constraint. Add shadow composite scoring fields:

```rust
// Added to OpenPosition:

/// Signal mode at open — determines which signal drives exit for THIS position.
pub signal_mode: SignalMode,

/// Latest shadow composite score (0–1000). Computed on every tick/event
/// by calling signal_engine::compute_composite_score() outside RideState.
pub shadow_composite_score: u16,

/// Peak shadow composite score seen during hold.
pub shadow_peak_composite: u16,

/// Shadow composite-derived signal state (0=Strong, 1=Sustained, 2=Weakening, 3=Exit).
pub shadow_signal_state: u8,

/// Shadow Kelly trail multiplier (8.8 fixed-point, for logging).
pub shadow_kelly_trail_mult: u16,
```

### 5.1 Shadow Computation Call Site (positions.rs)

On every tick/event for an open position, `positions.rs` calls **both** signal systems:

```rust
// In PositionManager::on_subsequent_trade() and on_tick():

// 1. ALWAYS: Bayesian update in RideState v3
ride_state.bayesian_on_buy(sol_msol);  // or bayesian_on_sell()
ride_state.bayesian_recompute(current_mvsol, now_ms, &bayesian_cfg, &ride_cfg);

// 2. SHADOW (if enabled): Composite score outside RideState
if engine_config.shadow_composite_enabled {
    // Extract features from ring buffers (same data RideState already has)
    let buy_rate_1s = signal_engine::count_in_window(...);
    let buy_rate_5s = signal_engine::count_in_window(...);
    let sell_rate_5s = signal_engine::count_in_window(...);
    // ... other features ...

    let shadow_score = signal_engine::compute_composite_score(
        buy_rate_1s, buy_rate_5s, sell_rate_5s,
        ride_state.vol_accel_bp, buy_gap,
        sell_pressure, pnl_bp, time_since_peak,
        ride_state.unique_wallets, ride_state.confirming_vol_msol,
        &ride_cfg.signal_weights(),
    );

    open_pos.shadow_composite_score = shadow_score;
    if shadow_score > open_pos.shadow_peak_composite {
        open_pos.shadow_peak_composite = shadow_score;
    }
    open_pos.shadow_signal_state = composite_to_signal_state(
        shadow_score, &ride_cfg
    );
    open_pos.shadow_kelly_trail_mult = signal_engine::compute_kelly_multiplier(
        ride_state.buys_after_entry, ride_state.confirming_vol_msol,
        ride_state.sells_after_entry, &ride_cfg.kelly_config(),
    );
}

// 3. EXIT DECISION: mux based on position's signal_mode
let exit_signal_state = match open_pos.signal_mode {
    SignalMode::Bayesian  => ride_state.state,                         // from RideState v3
    SignalMode::Composite => SignalState::from_u8(open_pos.shadow_signal_state), // from shadow
};
```

### 5.2 Divergence Tracking

```rust
// After computing both signals, check for divergence:
let bayesian_says_exit = ride_state.state == SignalState::Exit;
let composite_says_exit = open_pos.shadow_signal_state == SignalState::Exit as u8;
let bayesian_says_hold = ride_state.state != SignalState::Exit;
let composite_says_hold = open_pos.shadow_signal_state != SignalState::Exit as u8;

// Divergence = one says Exit while the other says hold
if (bayesian_says_exit && composite_says_hold) ||
   (composite_says_exit && bayesian_says_hold) {
    divergence_tracker.increment();
}
```

---

## 6. ClosedPosition Extensions

```rust
// Added to ClosedPosition:

/// Bayesian f̂*(t) at exit, in permille. Signed.
pub bayesian_f_at_exit: i16,

/// Bayesian-derived signal state at exit (0–3).
pub bayesian_state_at_exit: u8,

/// Raw posterior α ×16 at exit.
pub alpha_at_exit: u16,

/// Raw posterior β ×16 at exit.
pub beta_at_exit: u16,

/// Which signal mode drove this position's exit decision.
pub signal_mode: SignalMode,

/// Shadow composite score at exit (from OpenPosition.shadow_composite_score).
/// Populated regardless of which mode was primary.
pub shadow_composite_at_exit: u16,

/// Shadow peak composite (from OpenPosition.shadow_peak_composite).
pub shadow_peak_composite: u16,
```

---

## 7. JSONL dv8 Schema

Data version bumps from 7 → 8. All existing dv7 fields are preserved. New fields:

```json
{
    "signalScoreAtExit": 450,
    "signalStateAtExit": 1,
    "peakSignalScore": 780,

    "bayesianFAtExit": 142,
    "bayesianStateAtExit": 0,
    "alphaAtExit": 96,
    "betaAtExit": 48,

    "signalMode": "bayesian",
    "dataVersion": 8
}
```

### 7.1 Field Definitions

| Field | Type | Source | Description |
|-------|------|--------|-------------|
| `signalScoreAtExit` | u16 | `shadow_composite_at_exit` | Old composite score (shadow), always populated |
| `signalStateAtExit` | u8 | `shadow_signal_state` (mapped) | Composite-derived state (0–3) |
| `peakSignalScore` | u16 | `shadow_peak_composite` | Peak composite score during hold |
| `bayesianFAtExit` | i16 | `bayesian_f_at_exit` | f̂*(t) at exit, permille (signed) |
| `bayesianStateAtExit` | u8 | `bayesian_state_at_exit` | Kelly-derived state (0–3) |
| `alphaAtExit` | u16 | `alpha_at_exit` | Raw posterior α ×16 |
| `betaAtExit` | u16 | `beta_at_exit` | Raw posterior β ×16 |
| `signalMode` | string | `signal_mode.as_str()` | `"composite"` or `"bayesian"` |
| `dataVersion` | u8 | constant | `8` |

### 7.2 paper_logger.rs Changes

```rust
// In PaperTradeLogger::log(), replace the dv7 signal fields with dv8:

// Remove these hardcoded placeholders:
// "entryPPermille": 0u16,  →  wire from closed.entry_p_permille
// "entryRx100": 0u16,      →  wire from closed.entry_r_x100
// "entryFPermille": 0u16,  →  wire from closed.entry_f_permille
// "convictionTier": 0u8,   →  wire from closed.conviction_tier

// Add new dv8 fields:
"bayesianFAtExit": pos.bayesian_f_at_exit,
"bayesianStateAtExit": pos.bayesian_state_at_exit,
"alphaAtExit": pos.alpha_at_exit,
"betaAtExit": pos.beta_at_exit,
"signalMode": pos.signal_mode.as_str(),
"dataVersion": 8,

// ALSO wire the Kelly conviction fields that were TODO in dv7:
"entryPPermille": pos.entry_p_permille,
"entryRx100": pos.entry_r_x100,
"entryFPermille": pos.entry_f_permille,
"convictionTier": pos.conviction_tier,
```

---

## 8. Config Integration

### 8.1 New JSON Config Structs

```rust
// In config.rs — new JSON deserialization struct:

#[derive(Debug, Clone, Deserialize)]
pub struct SignalConfigJson {
    /// Use Bayesian signal for exit decisions. Default: false.
    #[serde(default, rename = "useBayesianSignal")]
    pub use_bayesian_signal: bool,

    /// Compute composite score as shadow (for comparison logging).
    /// Default: true. Disable in live mode for ~30ns/tick savings.
    #[serde(default = "default_shadow_composite_enabled", rename = "shadowCompositeEnabled")]
    pub shadow_composite_enabled: bool,

    /// Bayesian time-decay rate (0–255). 240 = half-life ≈ 5s.
    /// Higher = slower decay. Lower = faster forgetting.
    #[serde(default = "default_bayesian_decay_rate", rename = "bayesianDecayRate")]
    pub bayesian_decay_rate: u16,

    /// Beta prior strengths for [LOW, MED, HIGH] conviction tiers.
    /// Sum of α₀+β₀ (in units, before ×16 scaling).
    #[serde(default = "default_bayesian_prior_strength", rename = "bayesianPriorStrength")]
    pub bayesian_prior_strength: [u16; 3],

    /// Alert threshold: log warning if divergence_count exceeds this
    /// in the last 50 positions. Default: 10.
    #[serde(default = "default_divergence_alert_threshold", rename = "divergenceAlertThreshold")]
    pub divergence_alert_threshold: u16,
}

fn default_shadow_composite_enabled() -> bool { true }
fn default_bayesian_decay_rate() -> u16 { 240 }
fn default_bayesian_prior_strength() -> [u16; 3] { [6, 9, 13] }
fn default_divergence_alert_threshold() -> u16 { 10 }

impl Default for SignalConfigJson {
    fn default() -> Self {
        Self {
            use_bayesian_signal: false,
            shadow_composite_enabled: true,
            bayesian_decay_rate: 240,
            bayesian_prior_strength: [6, 9, 13],
            divergence_alert_threshold: 10,
        }
    }
}
```

### 8.2 Runtime Config Struct

```rust
/// Runtime Bayesian signal config (built from JSON, passed by reference on hot path).
#[derive(Debug, Clone, Copy)]
pub struct BayesianSignalConfig {
    /// Time-decay rate: α,β *= decay_rate/256 per tick. 240 → half-life ≈ 5s.
    pub decay_rate: u16,
    /// Prior strengths for [LOW, MED, HIGH] conviction tiers.
    pub prior_strength: [u16; 3],
    /// Divergence alert threshold (last 50 positions).
    pub divergence_alert_threshold: u16,
}

impl Default for BayesianSignalConfig {
    fn default() -> Self {
        Self {
            decay_rate: 240,
            prior_strength: [6, 9, 13],
            divergence_alert_threshold: 10,
        }
    }
}
```

### 8.3 EngineConfig Extensions

```rust
// Added to EngineConfig:

/// Bayesian signal config (runtime, built from signal JSON section).
pub bayesian_signal_config: BayesianSignalConfig,

/// Whether Bayesian mode is active for new positions.
pub use_bayesian_signal: bool,

/// Whether shadow composite scoring is enabled.
pub shadow_composite_enabled: bool,
```

### 8.4 MevJsonConfig Extension

```rust
// Added to MevJsonConfig:

/// Signal engine configuration (Bayesian + shadow).
pub signal: Option<SignalConfigJson>,
```

### 8.5 Builder (in load_config)

```rust
// In load_config(), after building other configs:
let signal_json = mev.signal.unwrap_or_default();

// Add to EngineConfig construction:
bayesian_signal_config: BayesianSignalConfig {
    decay_rate: signal_json.bayesian_decay_rate,
    prior_strength: signal_json.bayesian_prior_strength,
    divergence_alert_threshold: signal_json.divergence_alert_threshold,
},
use_bayesian_signal: signal_json.use_bayesian_signal,
shadow_composite_enabled: signal_json.shadow_composite_enabled,
```

---

## 9. Status API Changes

### 9.1 New Fields in `/api/stats`

```json
{
    "signal_mode": "bayesian",
    "bayesian_avg_f_at_exit": 85,
    "composite_avg_score_at_exit": 320,
    "signal_divergence_count": 3,
    "signal_divergence_alert": false
}
```

### 9.2 DivergenceTracker (new struct in health.rs)

```rust
/// Tracks divergence between Bayesian and composite signal systems.
/// Ring buffer of last 50 positions' divergence status.
pub struct DivergenceTracker {
    /// Ring buffer: bit set = divergent exit for that position.
    ring: u64,           // 64 bits, use bottom 50
    head: u8,            // next write index (0–49)
    count: u16,          // total divergences seen (lifetime)
    alert_threshold: u16, // from config
}

impl DivergenceTracker {
    pub fn new(alert_threshold: u16) -> Self {
        Self {
            ring: 0,
            head: 0,
            count: 0,
            alert_threshold,
        }
    }

    /// Record a position close. `diverged` = bayesian and composite disagree on exit.
    #[inline]
    pub fn record(&mut self, diverged: bool) {
        let bit = 1u64 << self.head;
        if diverged {
            self.ring |= bit;
            self.count += 1;
        } else {
            self.ring &= !bit;
        }
        self.head = (self.head + 1) % 50;
    }

    /// Count divergences in the last 50 positions.
    #[inline]
    pub fn recent_count(&self) -> u16 {
        (self.ring & ((1u64 << 50) - 1)).count_ones() as u16
    }

    /// Whether we've breached the alert threshold.
    #[inline]
    pub fn is_alert(&self) -> bool {
        self.recent_count() > self.alert_threshold
    }

    /// Lifetime divergence count.
    pub fn lifetime_count(&self) -> u16 {
        self.count
    }
}
```

### 9.3 HealthMonitor Extensions

```rust
// Added to HealthMonitor (or a separate SignalHealth struct):

/// Accumulator for Bayesian f̂*(t) at exit (for avg computation).
pub bayesian_f_sum: AtomicI64,
pub bayesian_f_count: AtomicU64,

/// Accumulator for composite score at exit.
pub composite_score_sum: AtomicU64,
pub composite_score_count: AtomicU64,

/// Divergence tracker (needs Mutex since it's mutable; cold path only).
pub divergence: Mutex<DivergenceTracker>,
```

The health API endpoint reads these atomics to compute averages and divergence stats.

---

## 10. Hot Path Integration

### 10.1 `hot_path.rs` Changes

Minimal changes — HotPath reads the flag and passes it through:

```rust
// In HotPath::on_trade(), when opening a position:
let signal_mode = if engine_config.use_bayesian_signal {
    SignalMode::Bayesian
} else {
    SignalMode::Composite
};

// Pass to open_position:
self.position_manager.open_position(
    trade, decision.score, now,
    decision.magnitude,
    decision.conviction.size_lamports,
    decision.conviction,
    signal_mode,  // NEW parameter
);
```

```rust
// In HotPath::on_position_closed():
// Record to divergence tracker + signal health accumulators
if let Some(ref health) = self.health_monitor {
    // ... update bayesian_f_sum, composite_score_sum, divergence tracker
}
```

### 10.2 `positions.rs` Changes

The `PositionManager` is where both signal systems converge:

```rust
// In PositionManager::open_position():
let mut open_pos = OpenPosition { ... };
open_pos.signal_mode = signal_mode;

// Initialize Bayesian prior in RideState v3:
ride_state.init_bayesian(
    conviction.p_permille,
    conviction.r_x100,
    conviction.tier as u8,
    &bayesian_config.prior_strength,
);
```

```rust
// In PositionManager::on_subsequent_trade() — the DUAL COMPUTATION:
fn on_subsequent_trade(&mut self, trade: &TradeEvent, now_ms: u64) {
    let pos = /* get mutable open position */;
    let ride = /* get mutable ride state */;

    // 1. Feed events into RideState v3 (Bayesian)
    if trade.is_buy {
        ride.on_buy_event(sol_mvsol, now_ms, wallet_hash);
        ride.bayesian_on_buy(sol_msol);
    } else {
        ride.on_sell_event(sol_mvsol, now_ms, &ride_cfg);
        ride.bayesian_on_sell(sol_msol, is_creator);
    }

    // 2. Bayesian recompute (always)
    ride.bayesian_recompute(current_mvsol, now_ms, &bayesian_cfg, &ride_cfg);

    // 3. Shadow composite (if enabled)
    if shadow_composite_enabled {
        // Feature extraction from ride_state ring buffers
        let shadow_score = signal_engine::compute_composite_score(/* ... */);
        pos.shadow_composite_score = shadow_score;
        if shadow_score > pos.shadow_peak_composite {
            pos.shadow_peak_composite = shadow_score;
        }
        pos.shadow_signal_state = match shadow_score {
            s if s >= ride_cfg.signal_strong_threshold => 0,
            s if s >= ride_cfg.signal_sustained_threshold => 1,
            s if s >= ride_cfg.signal_weakening_threshold => 2,
            _ => 3,
        };
    }

    // 4. Exit decision mux
    let effective_state = match pos.signal_mode {
        SignalMode::Bayesian  => ride.state,
        SignalMode::Composite => SignalState::from_u8(pos.shadow_signal_state),
    };

    // 5. Apply effective state to exit logic
    if effective_state == SignalState::Exit && ride.buys_after_entry >= 1 {
        // Trigger exit
    }
}
```

---

## 11. Rollback Safety

### 11.1 Auto-Revert Logic

Implemented in `HotPath::on_position_closed()`:

```rust
/// Bayesian auto-revert state. Lives in HotPath (session-scoped, not persisted).
struct BayesianRevertTracker {
    bayesian_wins: u16,
    bayesian_total: u16,
    reverted: bool,
}

impl BayesianRevertTracker {
    fn record(&mut self, win: bool) {
        self.bayesian_total += 1;
        if win { self.bayesian_wins += 1; }
    }

    /// Check if auto-revert should fire.
    /// Condition: WR < 20% on > 20 trades → revert.
    fn should_revert(&self) -> bool {
        if self.reverted { return false; }
        if self.bayesian_total <= 20 { return false; }
        let wr_pct = (self.bayesian_wins as u32 * 100) / self.bayesian_total as u32;
        wr_pct < 20
    }

    fn mark_reverted(&mut self) {
        self.reverted = true;
    }
}
```

### 11.2 Revert Behavior

When `should_revert()` fires:
1. Set `engine_config.use_bayesian_signal = false` (runtime only — not written to canary.json)
2. Log to `config_changes.jsonl`:
   ```json
   {"ts_ms": 1711756800000, "event": "bayesian_auto_revert", "bayesian_wr_pct": 18, "bayesian_trades": 25}
   ```
3. Send Telegram alert:
   ```
   ⚠️ Bayesian signal auto-reverted: WR 18% on 25 trades (threshold: 20% on 20+)
   ```
4. **Existing Bayesian positions keep their mode** — they'll exit naturally. Only NEW positions use composite.
5. Revert is per-daemon-session: restarting the daemon re-reads canary.json (which still has the original `useBayesianSignal` value). The operator must manually update canary.json if they want a permanent revert.

---

## 12. Monitoring & Comparison

### 12.1 Shadow Overhead Budget

| Operation | Cost | When | Notes |
|-----------|------|------|-------|
| `bayesian_recompute()` | ~15ns | Every tick | Always runs (RideState v3 native) |
| `compute_composite_score()` | ~30ns | Every tick (shadow) | Only when `shadow_composite_enabled` |
| Divergence check | ~3ns | Every tick | 2 comparisons + conditional increment |
| **Total hot-path overhead** | **~48ns** | Every tick | Well within 80ns budget |

### 12.2 Shadow Disable in Live Mode

When `shadow_composite_enabled: false` (default in live mode):
- `compute_composite_score()` is NOT called
- Shadow fields in OpenPosition stay at 0
- JSONL dv8 `signalScoreAtExit` / `peakSignalScore` are 0 (sentinel for "shadow disabled")
- Hot path cost: ~15ns (Bayesian only)

### 12.3 Offline Comparison Metrics

The dv8 JSONL enables these comparisons via `pump-quant-analyzer`:

| Metric | Computation | What It Shows |
|--------|-------------|---------------|
| WR by signal mode | Group by `signalMode`, count `netPnlSol > 0` | Which mode picks better exits |
| Avg hold time by mode | Group by `signalMode`, avg `holdMs` | Whether Bayesian exits faster/slower |
| Fee drag by mode | Group by `signalMode`, avg `feesSol / sizeSol` | If shorter holds = less fee burden |
| f̂* vs composite correlation | Pearson(bayesianFAtExit, signalScoreAtExit) | How correlated the two systems are |
| Divergence rate | Count where bayesian and composite disagreed / total | How often they see different things |
| Exit quality (MFE capture) | `(exitVSol - entryVSol) / (peakVSol - entryVSol)` by mode | % of MFE captured at exit |

---

## 13. Integration Points Summary

### 13.1 File Change Map

| File | Changes | Complexity |
|------|---------|------------|
| **`config.rs`** | Add `SignalConfigJson`, `BayesianSignalConfig` structs. Add `signal: Option<SignalConfigJson>` to `MevJsonConfig`. Add fields to `EngineConfig`. Wire in `load_config()`. | Medium |
| **`ride_state.rs`** | Replace composite fields with Bayesian fields (v2→v3). Add `init_bayesian`, `bayesian_on_buy`, `bayesian_on_sell`, `bayesian_decay`, `compute_f_hat`, `bayesian_signal_state`, `bayesian_trail_bp`, `bayesian_recompute`. Keep `recompute_signals` for shadow path (calls `compute_composite_score` into external buffer). | High |
| **`positions.rs`** | Add `SignalMode` enum. Add shadow fields to `OpenPosition`. Add Bayesian + shadow fields to `ClosedPosition`. Dual computation in `on_subsequent_trade` and `on_tick`. Exit decision mux. | High |
| **`hot_path.rs`** | Read `use_bayesian_signal` flag, pass `SignalMode` to `open_position`. Add `BayesianRevertTracker`. Auto-revert logic in `on_position_closed`. | Medium |
| **`paper_logger.rs`** | Add dv8 fields. Wire `bayesian_f_at_exit`, `alpha_at_exit`, `beta_at_exit`, `signal_mode`. Wire Kelly conviction fields (was TODO). Bump `dataVersion` to 8. | Low |
| **`health.rs`** | Add `DivergenceTracker`. Add signal mode + divergence stats to API response. | Low |
| **`signal_engine.rs`** | **No changes.** Pure functions remain as-is. Called from positions.rs for shadow scoring. | None |

### 13.2 New Files

| File | Purpose |
|------|---------|
| None | All new code lives in existing modules. No new files needed. |

---

## 14. Test Cases

### Test 1: Dual Mode — Both Scores Computed, Correct One Drives Exit

```rust
#[test]
fn test_dual_mode_bayesian_drives_exit() {
    let mut pos = open_test_position(SignalMode::Bayesian);
    let mut ride = RideState::new_v3(/* ... */);

    // Simulate sell cascade → Bayesian f̂*(t) goes negative
    for _ in 0..10 {
        ride.bayesian_on_sell(500, false);
    }
    ride.bayesian_recompute(/* ... */);

    // Shadow composite still shows Sustained (different scoring)
    pos.shadow_composite_score = 500; // artificially high
    pos.shadow_signal_state = 1;       // Sustained

    // Exit decision should use Bayesian (pos.signal_mode = Bayesian)
    let effective_state = match pos.signal_mode {
        SignalMode::Bayesian => ride.state,
        SignalMode::Composite => SignalState::from_u8(pos.shadow_signal_state),
    };
    assert_eq!(effective_state, SignalState::Exit);
    // Verify shadow was still computed
    assert_eq!(pos.shadow_composite_score, 500);
}
```

### Test 2: Flag Toggle — New Positions Use New Mode, Existing Keep Old

```rust
#[test]
fn test_flag_toggle_position_isolation() {
    let mut pm = PositionManager::new(/* ... */);

    // Open position A with composite mode
    pm.open_position(/* ... */, SignalMode::Composite);
    assert_eq!(pm.get_position("A").signal_mode, SignalMode::Composite);

    // Toggle flag
    // (engine_config.use_bayesian_signal = true)

    // Open position B with bayesian mode
    pm.open_position(/* ... */, SignalMode::Bayesian);
    assert_eq!(pm.get_position("B").signal_mode, SignalMode::Bayesian);

    // Position A is STILL composite
    assert_eq!(pm.get_position("A").signal_mode, SignalMode::Composite);
}
```

### Test 3: Divergence Detection — Bayesian=Exit, Composite=Sustained

```rust
#[test]
fn test_divergence_detection() {
    let mut tracker = DivergenceTracker::new(10);

    // 5 agreeing closes, 0 divergences
    for _ in 0..5 {
        tracker.record(false);
    }
    assert_eq!(tracker.recent_count(), 0);

    // 11 divergent closes → should trigger alert
    for _ in 0..11 {
        tracker.record(true);
    }
    assert_eq!(tracker.recent_count(), 11);
    assert!(tracker.is_alert()); // threshold = 10
}
```

### Test 4: Auto-Revert — WR < 20% on 20 Trades Switches Back

```rust
#[test]
fn test_auto_revert_on_low_wr() {
    let mut revert = BayesianRevertTracker::default();

    // 21 trades: 4 wins, 17 losses → WR = 19%
    for _ in 0..4 { revert.record(true); }
    for _ in 0..17 { revert.record(false); }

    assert_eq!(revert.bayesian_total, 21);
    assert!(revert.should_revert());

    revert.mark_reverted();
    assert!(!revert.should_revert()); // won't fire twice
}
```

### Test 5: Shadow Disable — Live Mode with Shadow Off → Composite Not Computed

```rust
#[test]
fn test_shadow_disabled() {
    let shadow_enabled = false;
    let mut pos = open_test_position(SignalMode::Bayesian);

    // Simulate tick processing
    if shadow_enabled {
        pos.shadow_composite_score = signal_engine::compute_composite_score(/* ... */);
    }
    // Shadow was NOT computed
    assert_eq!(pos.shadow_composite_score, 0); // default/sentinel
    assert_eq!(pos.shadow_peak_composite, 0);
}
```

### Test 6: RideState v3 Size Assertion

```rust
#[test]
fn test_ride_state_v3_size() {
    assert_eq!(core::mem::size_of::<RideState>(), 128);
}
```

### Test 7: Bayesian f̂*(t) Computation Correctness

```rust
#[test]
fn test_bayesian_f_hat_computation() {
    let mut ride = RideState::new_v3(/* ... */);
    // Initialize: p=542‰, R=43.00, prior=MED (total=9)
    ride.init_bayesian(542, 4300, 1, &[6, 9, 13]);

    let f = ride.compute_f_hat();
    // f* = (0.542 × 44 - 1) / 43 = (23.848 - 1) / 43 ≈ 0.531
    // Half-Kelly ≈ 266 permille
    // With ×16 scaling and integer rounding, expect ~248-270 range
    assert!(f > 200 && f < 300, "f̂ = {} (expected ~266)", f);

    // After 10 sells, f̂ should decrease
    for _ in 0..10 {
        ride.bayesian_on_sell(200, false);
    }
    let f2 = ride.compute_f_hat();
    assert!(f2 < f, "f̂ should decrease after sells: {} → {}", f, f2);
}
```

---

## 15. Migration Path

### Phase 1: Deploy with `useBayesianSignal: false` (default)
- RideState v3 computes Bayesian f̂*(t) on every tick (always).
- Shadow composite score also computed (both systems run).
- Composite score drives exit decisions (no behavior change).
- JSONL dv8 logs both score sets for offline comparison.
- **Risk: Zero.** Composite is still primary. Bayesian is compute-only.

### Phase 2: Enable Bayesian on canary (`useBayesianSignal: true`)
- New positions use Bayesian for exit decisions.
- Shadow composite continues running for comparison.
- Monitor divergence count, WR, hold times.
- Auto-revert fires if WR < 20% on 20+ trades.
- **Risk: Low.** Auto-revert provides safety net. Can manually toggle back anytime.

### Phase 3: Validate and disable shadow
- After 200+ trades with Bayesian primary, compare metrics.
- If Bayesian ≥ composite on WR and MFE capture → set `shadowCompositeEnabled: false`.
- Saves ~30ns/tick on hot path.
- **Risk: None.** Shadow is purely observational.

### Phase 4: Remove composite code (future — separate PR)
- Once confident, remove `compute_composite_score` from the exit path entirely.
- Remove shadow fields from OpenPosition/ClosedPosition.
- Keep `signal_engine.rs` functions (useful for other analysis).
- Bump to dv9 (drop shadow fields).

---

## 16. Performance Budget

| Phase | Hot-path cost per tick | Notes |
|-------|----------------------|-------|
| **Current (v2)** | ~45ns | composite_score(30ns) + trail(10ns) + state(5ns) |
| **Phase 1 (dual)** | ~48ns | bayesian(15ns) + shadow_composite(30ns) + divergence(3ns) |
| **Phase 2 (bayesian primary)** | ~48ns | Same — shadow still enabled for comparison |
| **Phase 3 (bayesian only)** | ~18ns | bayesian(15ns) + divergence(3ns), shadow off |

All phases stay under the 80ns hot-path budget.