# Architecture: RideState v3

> **File:** `rust/pump-quant-core/src/engine/ride_state.rs` (rewrite in-place)  
> **Size:** 128 bytes exactly (2 cache lines), same budget as v2  
> **Invariant:** `const _: () = assert!(core::mem::size_of::<RideState>() == 128);`  

---

## 1. Design Goals

- **Replace composite score** with inline BayesianSignal fields (α, β, R̂, peak_mfe)
- **Remove 5 fields** that are superseded by Bayesian posterior (composite_score, kelly_trail_mult, phase_trail_mult, vol_accel_bp, price_velocity)
- **Add 4 fields** from BayesianSignal (alpha_x16, beta_x16, r_est_x100, peak_mfe_bp)
- **Keep all ring buffers**, timing, emergency flags, peak tracking, bloom filter
- **Trail computation** now uses `f̂*/f_entry` ratio instead of kelly_mult × phase_mult
- **Grace period** preserved: no Bayesian exit until `buys_after_entry >= 1`

---

## 2. Field-by-Field Byte Map

### Cache Line 0: HOT (bytes 0–63) — accessed every event

| Offset | Size | Type   | Field              | Status    | Notes                                    |
|--------|------|--------|--------------------|-----------|------------------------------------------|
| 0      | 4    | u32    | `peak_mvsol`       | **KEPT**  | Highest vSOL seen                        |
| 4      | 4    | u32    | `trail_stop_mvsol` | **KEPT**  | Current trail stop (ratchets up only)    |
| 8      | 4    | u32    | `entry_mvsol`      | **KEPT**  | vSOL at entry                            |
| 12     | 2    | u16    | `current_trail_bp` | **KEPT**  | Active trail distance in vSOL bp         |
| 14     | 1    | u8     | `state`            | **KEPT**  | SignalState (StrongPump/Sustained/Weakening/Exit) |
| 15     | 1    | u8     | `flags`            | **KEPT**  | Bitflags (creator_sell, emergency, etc.) |
| 16     | 8    | u64    | `ride_start_ms`    | **KEPT**  | Entry timestamp                          |
| 24     | 8    | u64    | `last_buy_ms`      | **KEPT**  | Last buy event timestamp                 |
| 32     | 2    | u16    | `buys_after_entry` | **KEPT**  | Buy counter                              |
| 34     | 2    | u16    | `sells_after_entry`| **KEPT**  | Sell counter                             |
| 36     | 1    | u8     | `unique_wallets`   | **KEPT**  | Approx via bloom filter                  |
| 37     | 3    | [u8;3] | `_pad0`            | **KEPT**  | Alignment padding                        |
| 40     | 4    | u32    | `confirming_vol_msol` | **KEPT** | Cumulative buy volume in milli-SOL     |
| 44     | 2    | i16    | `peak_pnl_bp`      | **KEPT**  | Best unrealized PnL in basis points      |
| 46     | 2    | u16    | `peak_pnl_ms_rel`  | **KEPT**  | When peak occurred (relative ms)         |
| **48** | **2**| **u16**| **`alpha_x16`**    | **NEW**   | Beta dist α × 16                        |
| **50** | **2**| **u16**| **`beta_x16`**     | **NEW**   | Beta dist β × 16                        |
| **52** | **2**| **u16**| **`r_est_x100`**   | **NEW**   | Current R estimate × 100                 |
| **54** | **2**| **i16**| **`peak_mfe_bp`**  | **NEW**   | Peak MFE in basis points                 |
| 56     | 2    | u16    | `entry_f_permille` | **KEPT**  | Kelly f* at entry (was at offset 62)     |
| 58     | 2    | u16    | `entry_p_permille` | **KEPT**  | Entry win prob (moved from EntryConviction read; saves a pointer chase) |
| 60     | 2    | u16    | `peak_composite`   | **KEPT→RENAMED** | Renamed: `peak_f_permille` — peak f̂* seen during ride |
| 62     | 2    | u16    | `avg_loss_bp`      | **NEW**   | Configured avg loss bp (for R̂ update)   |

**Cache line 0 total: 64 bytes ✓**

### Cache Line 1: WARM (bytes 64–127) — ring buffers + bloom

| Offset | Size | Type     | Field              | Status   | Notes                                   |
|--------|------|----------|--------------------|----------|-----------------------------------------|
| 64     | 16   | [u16;8]  | `buy_ts_ring`      | **KEPT** | Buy timestamps (relative ms)            |
| 80     | 16   | [u16;8]  | `buy_sol_ring`     | **KEPT** | Buy amounts (milli-SOL)                 |
| 96     | 8    | [u16;4]  | `sell_ts_ring`     | **KEPT** | Sell timestamps (relative ms)           |
| 104    | 8    | [u16;4]  | `sell_sol_ring`    | **KEPT** | Sell amounts (milli-SOL)                |
| 112    | 1    | u8       | `buy_ring_idx`     | **KEPT** | Next write position in buy ring         |
| 113    | 1    | u8       | `sell_ring_idx`    | **KEPT** | Next write position in sell ring        |
| 114    | 8    | [u8;8]   | `bloom_filter`     | **KEPT** | 64-bit bloom for unique wallet tracking |
| 122    | 2    | u16      | `vol_recent_msol`  | **KEPT** | Buy vol in [now-2s, now]                |
| 124    | 2    | u16      | `vol_prior_msol`   | **KEPT** | Buy vol in [now-4s, now-2s]             |
| 126    | 1    | u8       | `phase`            | **KEPT** | Legacy RidePhase for logging compat     |
| 127    | 1    | u8       | `_pad2`            | **KEPT** | Alignment padding                       |

**Cache line 1 total: 64 bytes ✓**

**Grand total: 128 bytes ✓**

---

## 3. Fields Removed (from v2)

| Field               | Size | Reason                                                    |
|---------------------|------|-----------------------------------------------------------|
| `composite_score`   | u16  | Replaced by `current_f_permille()` computed on-the-fly    |
| `kelly_trail_mult`  | u16  | Trail now computed from f̂*/f_entry ratio                  |
| `phase_trail_mult`  | u16  | Lifecycle phases embedded in Beta posterior                |
| `vol_accel_bp`      | i16  | Buy/sell rates captured in α/β updates                    |
| `price_velocity`    | i32  | Price trajectory captured in R̂ update                     |

**Removed: 2+2+2+2+4 = 12 bytes**

## 4. Fields Added (v3)

| Field              | Size | Source                                                    |
|--------------------|------|-----------------------------------------------------------|
| `alpha_x16`        | u16  | Beta distribution α × 16                                  |
| `beta_x16`         | u16  | Beta distribution β × 16                                  |
| `r_est_x100`       | u16  | Reward ratio estimate × 100 (from EntryConviction)        |
| `peak_mfe_bp`      | i16  | Peak MFE for R̂ upward-only update                         |
| `entry_p_permille` | u16  | Entry p (avoids pointer chase to EntryConviction)          |
| `avg_loss_bp`      | u16  | Configured average loss (for R̂ calc)                      |

**Added: 2+2+2+2+2+2 = 12 bytes**

**Net: +12 - 12 = 0 bytes. Budget unchanged at 128 bytes. ✓**

---

## 5. Struct Definition

```rust
/// Signal-driven RIDE exit state v3. 128 bytes exactly.
///
/// Cache line 0 (bytes 0-63): HOT — accessed every event.
///   Bayesian signal fields (α, β, R̂, MFE) live here for <10ns access.
/// Cache line 1 (bytes 64-127): WARM — ring buffers + bloom.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct RideState {
    // ── Cache line 0: trail + timing + counters + Bayesian ───────

    // Trail state (16 bytes)
    pub peak_mvsol: u32,             // 0-3
    pub trail_stop_mvsol: u32,       // 4-7
    pub entry_mvsol: u32,            // 8-11
    pub current_trail_bp: u16,       // 12-13
    pub state: SignalState,          // 14     (u8)
    pub flags: u8,                   // 15

    // Timing (16 bytes)
    pub ride_start_ms: u64,          // 16-23
    pub last_buy_ms: u64,            // 24-31

    // Counters (16 bytes)
    pub buys_after_entry: u16,       // 32-33
    pub sells_after_entry: u16,      // 34-35
    pub unique_wallets: u8,          // 36
    _pad0: [u8; 3],                  // 37-39
    pub confirming_vol_msol: u32,    // 40-43
    pub peak_pnl_bp: i16,           // 44-45
    pub peak_pnl_ms_rel: u16,       // 46-47

    // Bayesian signal (16 bytes) — replaces composite_score block
    pub alpha_x16: u16,             // 48-49
    pub beta_x16: u16,              // 50-51
    pub r_est_x100: u16,            // 52-53
    pub peak_mfe_bp: i16,           // 54-55
    pub entry_f_permille: u16,      // 56-57
    pub entry_p_permille: u16,      // 58-59
    pub peak_f_permille: u16,       // 60-61  (was peak_composite)
    pub avg_loss_bp: u16,           // 62-63

    // ── Cache line 1: ring buffers + bloom ────────────────────────

    pub buy_ts_ring: [u16; BUY_RING_LEN],    // 64-79
    pub buy_sol_ring: [u16; BUY_RING_LEN],   // 80-95
    pub sell_ts_ring: [u16; SELL_RING_LEN],   // 96-103
    pub sell_sol_ring: [u16; SELL_RING_LEN],  // 104-111
    pub buy_ring_idx: u8,                     // 112
    pub sell_ring_idx: u8,                    // 113
    pub bloom_filter: [u8; 8],                // 114-121
    pub vol_recent_msol: u16,                 // 122-123
    pub vol_prior_msol: u16,                  // 124-125
    pub phase: RidePhase,                     // 126  (u8)
    pub _pad2: u8,                            // 127
}

const _: () = assert!(core::mem::size_of::<RideState>() == 128);
```

---

## 6. Constructor

```rust
impl RideState {
    /// Create a new RideState v3 for a freshly opened position.
    ///
    /// `entry_f_permille`: Kelly f* from EntryConviction (conviction prior for exit)
    /// `entry_p_permille`: Win probability from EntryConviction
    /// `entry_r_x100`:     Reward ratio from EntryConviction
    /// `conviction_tier`:  0=LOW, 1=MED, 2=HIGH
    /// `avg_loss_bp`:      Configured average loss in bp (from historical data)
    #[inline(always)]
    pub fn new(
        entry_mvsol: u32,
        now_ms: u64,
        entry_f_permille: u16,
        entry_p_permille: u16,
        entry_r_x100: u16,
        conviction_tier: u8,
        avg_loss_bp: u16,
        config: &RideConfig,
    ) -> Self {
        let initial_trail = config.trail_strong_pump_bp;
        let trail_stop = compute_trail_stop(entry_mvsol, initial_trail);

        // Initialize Bayesian prior from conviction tier
        let bayes = BayesianSignal::from_conviction(
            entry_p_permille, entry_r_x100, entry_f_permille, conviction_tier,
        );

        RideState {
            // Trail
            peak_mvsol: entry_mvsol,
            trail_stop_mvsol: trail_stop,
            entry_mvsol,
            current_trail_bp: initial_trail,
            state: SignalState::StrongPump,
            flags: 0,

            // Timing
            ride_start_ms: now_ms,
            last_buy_ms: now_ms,

            // Counters
            buys_after_entry: 0,
            sells_after_entry: 0,
            unique_wallets: 0,
            _pad0: [0; 3],
            confirming_vol_msol: 0,
            peak_pnl_bp: 0,
            peak_pnl_ms_rel: 0,

            // Bayesian signal (from prior)
            alpha_x16: bayes.alpha_x16,
            beta_x16: bayes.beta_x16,
            r_est_x100: bayes.r_est_x100,
            peak_mfe_bp: 0,
            entry_f_permille,
            entry_p_permille,
            peak_f_permille: entry_f_permille, // start at entry conviction
            avg_loss_bp,

            // Ring buffers — sentinel u16::MAX so they don't falsely count as "in window"
            buy_ts_ring: [u16::MAX; BUY_RING_LEN],
            buy_sol_ring: [0; BUY_RING_LEN],
            sell_ts_ring: [u16::MAX; SELL_RING_LEN],
            sell_sol_ring: [0; SELL_RING_LEN],
            buy_ring_idx: 0,
            sell_ring_idx: 0,
            bloom_filter: [0; 8],
            vol_recent_msol: 0,
            vol_prior_msol: 0,

            phase: RidePhase::Early,
            _pad2: 0,
        }
    }
}
```

---

## 7. Updated Event Handlers

### 7.1 `on_buy_event` — adds `source` and `weight_mult`

```rust
/// Process a buy event. Updates ring buffers, bloom, counters, and Bayesian posterior.
///
/// `source`:      FeedSource (PumpPortal, Helius, CoreCast, ShredStream)
/// `weight_mult`: Evidence multiplier (10=normal, higher for special events)
///
/// v2 signature: on_buy_event(sol_amount_mvsol, now_ms, wallet_hash)
/// v3 signature: on_buy_event(sol_amount_mvsol, now_ms, wallet_hash, source, weight_mult)
#[inline(always)]
pub fn on_buy_event(
    &mut self,
    sol_amount_mvsol: u32,
    now_ms: u64,
    wallet_hash: u64,
    source: FeedSource,
    weight_mult: u8,
) {
    self.buys_after_entry = self.buys_after_entry.saturating_add(1);
    self.last_buy_ms = now_ms;

    let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
    self.confirming_vol_msol = self.confirming_vol_msol.saturating_add(sol_amount_mvsol);

    // Volume window accumulation
    self.vol_recent_msol = self.vol_recent_msol.saturating_add(amount_msol);

    // Buy ring buffer
    let now_rel = self.rel_ms(now_ms);
    let idx = (self.buy_ring_idx as usize) % BUY_RING_LEN;
    self.buy_ts_ring[idx] = now_rel;
    self.buy_sol_ring[idx] = amount_msol;
    self.buy_ring_idx = self.buy_ring_idx.wrapping_add(1);

    // Bloom filter for unique wallets
    bloom_insert(&mut self.bloom_filter, wallet_hash);
    let old_wallets = self.unique_wallets;
    self.unique_wallets = bloom_count(&self.bloom_filter);

    // ── Bayesian update ──
    // Construct inline BayesianSignal view and call update_evidence
    self.bayesian_update_evidence(true, amount_msol, source, weight_mult);

    // Unique wallet bonus: new wallet = extra α
    if self.unique_wallets > old_wallets {
        self.alpha_x16 = self.alpha_x16.saturating_add(UNIQUE_BUYER_BONUS as u16);
    }
}
```

### 7.2 `on_sell_event` — adds `source` and `weight_mult`

```rust
/// Process a sell event. Returns Some(reason) for emergency immediate exit.
///
/// v2 signature: on_sell_event(sol_amount_mvsol, now_ms, config)
/// v3 signature: on_sell_event(sol_amount_mvsol, now_ms, config, source, weight_mult)
#[inline(always)]
pub fn on_sell_event(
    &mut self,
    sol_amount_mvsol: u32,
    now_ms: u64,
    config: &RideConfig,
    source: FeedSource,
    weight_mult: u8,
) -> Option<RideExitReason> {
    self.sells_after_entry = self.sells_after_entry.saturating_add(1);

    let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
    let now_rel = self.rel_ms(now_ms);

    // Sell ring buffer
    let idx = (self.sell_ring_idx as usize) % SELL_RING_LEN;
    self.sell_ts_ring[idx] = now_rel;
    self.sell_sol_ring[idx] = amount_msol;
    self.sell_ring_idx = self.sell_ring_idx.wrapping_add(1);

    // ── Emergency checks (override everything, UNCHANGED from v2) ──

    // Creator sell flag (set externally via mark_creator_sell)
    if self.flags & ride_flags::CREATOR_SELL != 0 {
        return Some(RideExitReason::CreatorSell);
    }

    // Whale exit: single sell > threshold
    let whale_threshold_msol = (config.whale_exit_lamports / 1_000_000) as u32;
    if sol_amount_mvsol > whale_threshold_msol {
        self.flags |= ride_flags::WHALE_EXIT_SEEN;
        return Some(RideExitReason::WhaleExit);
    }

    // Sell cascade: N sells in window
    let cascade_count = count_in_window(
        &self.sell_ts_ring, self.sell_ring_idx,
        SELL_RING_LEN as u8, now_rel, SELL_CASCADE_WINDOW_MS,
    );
    if cascade_count >= SELL_CASCADE_COUNT {
        self.flags |= ride_flags::SELL_CASCADE_SEEN;
        return Some(RideExitReason::SellCascade);
    }

    // ── Bayesian update ──
    self.bayesian_update_evidence(false, amount_msol, source, weight_mult);

    None
}
```

### 7.3 `on_tick` — decay + f̂ recompute + trail

```rust
/// Main tick: emergency checks → decay → Bayesian score → trail → trailing stop.
///
/// Called after on_buy_event or on_sell_event, and periodically (every 500ms).
///
/// Changes from v2:
///   - Calls decay_tick() instead of recompute_signals()
///   - Computes f̂*(t) via current_f_permille()
///   - Trail from f̂*/f_entry ratio, not kelly_mult × phase_mult
///   - Grace period: no Bayesian exit until buys_after_entry >= 1
#[inline(always)]
pub fn on_tick(
    &mut self,
    current_mvsol: u32,
    now_ms: u64,
    config: &RideConfig,
) -> RideDecision {
    // ── Emergency overrides (highest priority, UNCHANGED) ──

    // Creator sell
    if self.flags & ride_flags::CREATOR_SELL != 0 {
        return RideDecision::Exit(RideExitReason::CreatorSell);
    }

    // Hard floor: price below entry
    if HARD_FLOOR_ENABLED && current_mvsol < self.entry_mvsol {
        return RideDecision::Exit(RideExitReason::HardFloor);
    }

    // Max hold safety backstop
    if now_ms.saturating_sub(self.ride_start_ms) >= config.max_hold_ms.max(MAX_HOLD_RIDE_MS) {
        return RideDecision::Exit(RideExitReason::MaxHold);
    }

    // Buy gap timeout
    let gap = self.buy_gap_ms(now_ms);
    if gap >= BUY_GAP_EXIT_MS {
        return RideDecision::Exit(RideExitReason::BuyGapTimeout);
    }

    // ── Bayesian decay ──
    self.bayesian_decay_tick();

    // ── Compute current f̂* and update R̂ ──
    let pnl_bp = self.unrealized_pnl_bp(current_mvsol);
    self.bayesian_update_r_estimate(pnl_bp);

    let f_hat = self.bayesian_current_f_permille();

    // Track peak f̂*
    if f_hat > 0 && f_hat as u16 > self.peak_f_permille {
        self.peak_f_permille = f_hat as u16;
    }

    // ── Signal state from f̂* ──
    let new_state = self.bayesian_signal_state(f_hat);
    self.state = new_state;

    // Legacy phase mapping
    self.phase = match new_state {
        SignalState::StrongPump => RidePhase::Early,
        SignalState::Sustained  => RidePhase::Momentum,
        SignalState::Weakening | SignalState::Exit => RidePhase::Tighten,
    };

    // ── Grace period: no Bayesian exit until buys_after_entry >= 1 ──
    // Fix for empty-ringbuffer bug: on entry, Beta prior may produce f̂ < threshold
    // if entry_p is borderline. We must see at least 1 confirming buy first.
    if new_state == SignalState::Exit && self.buys_after_entry >= 1 {
        return RideDecision::Exit(RideExitReason::SignalExit);
    }

    // ── Dynamic trail computation ──
    let base_trail = match self.state {
        SignalState::StrongPump => config.trail_strong_pump_bp,
        SignalState::Sustained  => config.trail_sustained_bp,
        SignalState::Weakening  => config.trail_weakening_bp,
        SignalState::Exit       => 0, // would have returned above if buys >= 1
    };

    // Trail modulation by f̂*/f_entry ratio:
    //   trail_bp = base_bp × clamp(f_hat × 256 / f_entry, 64, 400) >> 8
    //
    // When f̂ = f_entry: scale = 256 (1.0×) → trail = base
    // When f̂ = 1.5 × f_entry: scale = 384 (1.5×) → wider trail
    // When f̂ = 0.25 × f_entry: scale = 64 (0.25×) → tighter trail
    // Clamped: [64, 400] = [0.25×, 1.5625×]
    let f_entry = self.entry_f_permille.max(1) as u32;
    let f_now = (f_hat.max(0) as u32).min(f_entry * 2); // cap at 2× entry
    let scale = (f_now * 256 / f_entry).clamp(64, 400);
    let trail = (base_trail as u32 * scale) >> 8;
    let trail = trail.clamp(config.kelly_min_trail_bp as u32, config.kelly_max_trail_bp as u32);
    self.current_trail_bp = trail as u16;

    // Update peak PnL tracking
    let now_rel = self.rel_ms(now_ms);
    if pnl_bp > self.peak_pnl_bp {
        self.peak_pnl_bp = pnl_bp;
        self.peak_pnl_ms_rel = now_rel;
    }

    // ── Update peak and trail stop ──
    if current_mvsol > self.peak_mvsol {
        self.peak_mvsol = current_mvsol;
    }

    let new_stop = compute_trail_stop(self.peak_mvsol, self.current_trail_bp);
    if new_stop > self.trail_stop_mvsol {
        self.trail_stop_mvsol = new_stop;
    }

    // ── Check trailing stop ──
    if current_mvsol <= self.trail_stop_mvsol {
        return RideDecision::Exit(RideExitReason::TrailingStop);
    }

    RideDecision::Hold
}
```

---

## 8. Bayesian Helper Methods (on RideState)

These methods operate on the inline Bayesian fields without constructing a
separate `BayesianSignal` struct. This avoids copying 12 bytes and keeps
everything in registers.

```rust
impl RideState {
    /// Bayesian evidence update (inlined on RideState fields).
    #[inline(always)]
    fn bayesian_update_evidence(
        &mut self,
        is_buy: bool,
        sol_msol: u16,
        source: FeedSource,
        weight_mult: u8,
    ) {
        let base = EVIDENCE_WEIGHTS[(!is_buy) as usize][source as usize] as u32;
        let size_factor = (1u32 + sol_msol as u32 / 500).min(16);
        let w = (base * size_factor * weight_mult as u32 / 10).min(4080) as u16;

        if is_buy {
            self.alpha_x16 = self.alpha_x16.saturating_add(w);
        } else {
            self.beta_x16 = self.beta_x16.saturating_add(w);
        }
    }

    /// Bayesian decay (inlined).
    #[inline(always)]
    fn bayesian_decay_tick(&mut self) {
        self.alpha_x16 = ((self.alpha_x16 as u32 * 240) >> 8).max(16) as u16;
        self.beta_x16 = ((self.beta_x16 as u32 * 240) >> 8).max(16) as u16;
    }

    /// Compute current half-Kelly f̂* in permille (inlined).
    #[inline(always)]
    fn bayesian_current_f_permille(&self) -> i16 {
        let a = self.alpha_x16 as u32;
        let b = self.beta_x16 as u32;
        let ab = a + b;
        let p_x1000 = (a * 1000) / ab;
        let r = self.r_est_x100.max(1) as u32;
        let numerator = (p_x1000 * (r + 100)) as i32 - 100_000;
        (numerator / (2 * r as i32)).clamp(-1000, 1000) as i16
    }

    /// Map f̂* to SignalState (inlined).
    #[inline(always)]
    fn bayesian_signal_state(&self, f_hat: i16) -> SignalState {
        let f_entry = self.entry_f_permille as i32;
        if f_entry == 0 { return SignalState::Exit; }
        let strong = (f_entry * 179) >> 8;
        let sustain = (f_entry * 90) >> 8;
        if f_hat as i32 > strong {
            SignalState::StrongPump
        } else if f_hat as i32 > sustain {
            SignalState::Sustained
        } else if f_hat > 0 {
            SignalState::Weakening
        } else {
            SignalState::Exit
        }
    }

    /// Update R̂ from current PnL (inlined, upward-only).
    #[inline(always)]
    fn bayesian_update_r_estimate(&mut self, current_pnl_bp: i16) {
        if current_pnl_bp > self.peak_mfe_bp {
            self.peak_mfe_bp = current_pnl_bp;
        }
        let avg = self.avg_loss_bp.max(1) as u32;
        let implied = (self.peak_mfe_bp.max(0) as u32 * 100) / avg;
        if implied > self.r_est_x100 as u32 {
            self.r_est_x100 = ((self.r_est_x100 as u32 * 7 + implied) >> 3) as u16;
        }
    }
}
```

---

## 9. Trail Computation Detail

### Formula

```
trail_bp = base_bp × clamp(f_hat × 256 / f_entry, 64, 400) >> 8
```

### Worked Examples

| Scenario              | f̂*  | f_entry | scale = f̂×256/f | clamped | base | trail_bp          |
|-----------------------|------|---------|------------------|---------|------|-------------------|
| Fresh (strong)        | 257  | 248     | 265              | 265     | 500  | 500×265>>8 = 517  |
| Degrading (sustained) | 102  | 248     | 105              | 105     | 350  | 350×105>>8 = 143  |
| Barely alive          | 20   | 248     | 20               | 64      | 200  | 200×64>>8 = 50    |
| Recovering            | 371  | 248     | 383              | 383     | 500  | 500×383>>8 = 748  |
| Conviction exceeded   | 500  | 248     | 516              | 400     | 500  | 500×400>>8 = 781  |

Trail widens/tightens **continuously** with conviction, not in discrete steps.

---

## 10. Emergency Exits — UNCHANGED from v2

All emergency exits are preserved exactly:

| Exit Type       | Condition                                              | Attribute          |
|-----------------|--------------------------------------------------------|--------------------|
| Creator sell    | `flags & CREATOR_SELL != 0`                            | `#[cold]`          |
| Hard floor      | `current_mvsol < entry_mvsol`                          | `#[cold]`          |
| Whale exit      | `sol_amount_mvsol > whale_threshold`                   | `#[cold]`          |
| Sell cascade    | `count_in_window(sell_ring, ...) >= SELL_CASCADE_COUNT` | `#[cold]`          |
| Max hold        | `now - start >= MAX_HOLD_RIDE_MS`                      | `#[cold]`          |
| Buy gap         | `buy_gap_ms >= BUY_GAP_EXIT_MS`                        | `#[cold]`          |

Emergency functions should be annotated `#[cold]` to keep them out of the branch
predictor's hot path.

---

## 11. Grace Period: Empty-Ringbuffer Bug Fix

**Problem (inherited from v2, now explicit):** When a position is opened, the
ring buffers are empty (all sentinels = `u16::MAX`). The Bayesian posterior
starts at the prior, which may be below the StrongPump threshold for borderline
entries. Without a guard, the very first `on_tick()` would signal Exit.

**Solution:**

```rust
// In on_tick():
if new_state == SignalState::Exit && self.buys_after_entry >= 1 {
    return RideDecision::Exit(RideExitReason::SignalExit);
}
```

**The guard `buys_after_entry >= 1` ensures:**
- No Bayesian exit fires before at least 1 confirming buy is seen
- Ring buffers have at least 1 real entry
- The posterior has received at least 1 observation beyond the prior
- Emergency exits (creator_sell, hard_floor, whale, etc.) still fire immediately
  regardless of this guard — they are checked first

---

## 12. Ring Buffer Initialization — Preserved

```rust
// Sentinels prevent count_in_window from falsely counting empty slots
buy_ts_ring: [u16::MAX; BUY_RING_LEN],   // 8 entries
buy_sol_ring: [0; BUY_RING_LEN],
sell_ts_ring: [u16::MAX; SELL_RING_LEN],  // 4 entries
sell_sol_ring: [0; SELL_RING_LEN],
```

`u16::MAX = 65535` as a timestamp means "65.5 seconds from entry" which is well
past any realistic window check. The `count_in_window` function checks
`ts >= threshold && ts <= now_rel`, so sentinel values (65535) will only match if
`now_rel` is also >= 65535, which only happens after 65+ seconds.

---

## 13. Annotations

```rust
// Hot-path functions (every event):
#[inline(always)] fn on_buy_event(...)
#[inline(always)] fn on_sell_event(...)
#[inline(always)] fn on_tick(...)
#[inline(always)] fn bayesian_update_evidence(...)
#[inline(always)] fn bayesian_decay_tick(...)
#[inline(always)] fn bayesian_current_f_permille(...)
#[inline(always)] fn bayesian_signal_state(...)
#[inline(always)] fn bayesian_update_r_estimate(...)

// Cold-path functions (emergency, rare):
#[cold] fn mark_creator_sell(...)
// Emergency branches inside on_tick/on_sell_event use #[cold] hint
// via unlikely() pattern or manual branch weight hints.
```

---

## 14. Performance Budget

| Phase                        | v2 Cost  | v3 Cost  | Notes                                         |
|------------------------------|----------|----------|-----------------------------------------------|
| Emergency checks             | ~5ns     | ~5ns     | Unchanged (6 comparisons)                     |
| Signal recomputation         | ~35ns    | ~15ns    | 12-feature weighted sum → 3 muls + 1 div      |
| Trail computation            | ~10ns    | ~5ns     | 2 LUT lookups + 3 muls → 1 mul + 1 shift      |
| Trail stop / peak update     | ~5ns     | ~5ns     | Unchanged                                     |
| **Total per on_tick()**      | **~55ns**| **~30ns**| **45% faster**                                |
| Ring buffer + bloom update   | ~15ns    | ~15ns    | Unchanged (in on_buy/on_sell)                  |
| Bayesian update (in event)   | —        | ~10ns    | New cost, but absorbed into event handler      |
| **Total event + tick**       | **~70ns**| **~55ns**| **Well under 80ns budget**                    |

---

## 15. RideConfig Changes

Fields **removed** from RideConfig:
```rust
// No longer needed — Bayesian replaces weighted score + lifecycle phases
// signal_strong_threshold: u16,     // was 700
// signal_sustained_threshold: u16,  // was 400
// signal_weakening_threshold: u16,  // was 200
// w_buy_rate_1s: i8,
// w_buy_rate_5s: i8,
// w_sell_rate_5s: i8,
// w_vol_accel_shift: u8,
// w_buy_gap_divisor: u16,
// w_sell_pressure_shift: u8,
// w_pnl_shift: u8,
// w_time_since_peak_divisor: u16,
// w_unique_wallets: i8,
// w_confirm_vol_shift: u8,
// kelly_baseline_f_permille: u16,
// lifecycle_accel_min_buys: u16,
// lifecycle_accel_min_sol_msol: u32,
// lifecycle_momentum_min_buys: u16,
// lifecycle_momentum_min_sol_msol: u32,
```

Fields **kept** in RideConfig:
```rust
pub trail_strong_pump_bp: u16,   // base trail for StrongPump state
pub trail_sustained_bp: u16,     // base trail for Sustained state
pub trail_weakening_bp: u16,     // base trail for Weakening state
pub kelly_min_trail_bp: u16,     // floor on computed trail
pub kelly_max_trail_bp: u16,     // ceiling on computed trail
pub max_hold_ms: u64,            // safety backstop
pub whale_exit_lamports: u64,    // whale exit threshold
pub avg_loss_bp: u16,            // NEW: avg historical loss for R̂ update
```

The `signal_weights()`, `kelly_config()`, and `lifecycle_config()` helper methods
on RideConfig are **removed** — no longer needed.

---

## 16. Migration Checklist

- [ ] Add `bayesian_signal.rs` with `BayesianSignal`, `FeedSource`, `SignalState`, bloom functions
- [ ] Rewrite `RideState` struct to v3 layout (this spec)
- [ ] Update `RideState::new()` to accept conviction params + call `from_conviction`
- [ ] Update `on_buy_event()` signature: add `source: FeedSource, weight_mult: u8`
- [ ] Update `on_sell_event()` signature: add `source: FeedSource, weight_mult: u8`
- [ ] Rewrite `on_tick()` per section 7.3
- [ ] Remove `recompute_signals()` (replaced by inline Bayesian methods)
- [ ] Remove RideConfig signal weight fields
- [ ] Remove `signal_engine.rs` import from `ride_state.rs`
- [ ] Update all call sites that create RideState (positions.rs, etc.)
- [ ] Update all call sites that call on_buy_event / on_sell_event (feed handlers)
- [ ] Add FeedSource mapping in each feed handler (PumpPortal → FeedSource::PumpPortal, etc.)
- [ ] Verify: `const _: () = assert!(size_of::<RideState>() == 128);` compiles
- [ ] Run existing tests, update expected values
- [ ] Add new tests for Bayesian signal behavior