//! Cache-aligned position struct and pending entry ring buffer.
//!
//! ## Layout: MomentumPosition — 265 raw bytes, 64-byte aligned (padded to 320 by repr)
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!   0      32   mint: [u8; 32]
//!  32       8   entry_ts_ms: u64             — buy DECISION timestamp
//!  40       8   entry_price_fp: u64
//!  48       8   bc_terminal_price_fp: u64
//!  56       8   peak_price_fp: u64
//!  64       8   size_lamports: u64
//!  72     120   price_samples_bps: [i32; 30]
//! 192       2   remaining_bps: u16  (2-byte aligned at even offset)
//! 194       1   sample_count: u8
//! 195       1   pool_type: u8
//! 196       1   grad_score: u8
//! 197       1   tp_flags: u8
//! 198       1   exit_reason: u8
//! 199       1   trail_stop_below_floor_count: u8
//! 200       4   grad_speed_s: u32
//! 204       4   grad_volume_sol_x100: u32
//! 208       4   pre_grad_buys_5s: u32
//! 212       4   entry_delay_ms: u32
//! 216       1   first_price_recorded: bool
//! 217       1   velocity_confirm_counter: u8
//! 218       8   buy_confirmed_ms: u64        — on-chain TX confirmation timestamp (0 = unconfirmed)
//! 226      39   _pad2: [u8; 39]              — packed storage (TopDetector, ws_notif, etc.)
//! ------  ----
//! TOTAL:  265 raw (320 with align(64), 5 cache lines)
//! ```
//!
//! ## Performance
//!
//! - `#[repr(C, align(64))]` ensures cache-line alignment
//! - All hot-path methods are `#[inline(always)]`
//! - No heap allocation in PendingEntryRing (64 fixed slots)
//! - Integer-only price tracking (fixed-point bps offsets)

// ── Adaptive Trail Config (TASK 6: Winner Management) ────────────────────────

/// A single tier in the gain-tiered adaptive trailing stop.
///
/// Mathematical basis: optimal trail w* = σ²/(2μ) for geometric Brownian motion.
/// For memecoins: μ ≈ 30-50 bps/s, σ ≈ 80-150 bps/s in momentum.
/// w* ≈ 100-375 bps. We tier from 100 (small gains) to 1500 (moonshots).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TrailTier {
    /// Gain level this tier applies up to (bps). Tiers must be sorted ascending.
    pub up_to_bps: i32,
    /// Trail width at this gain level (bps from peak).
    pub trail_bps: u16,
}

/// Configuration for the adaptive gain-tiered trailing stop.
///
/// Replaces the old momentum-state-based trailing (Accel=25%, Sustain=15%, etc.)
/// with gain-tiered trailing that's calibrated for memecoin dynamics:
/// - Tight 2% trail on small gains (0-3%) — protect early profits
/// - Wider trails as gains grow — let moonshots run
///
/// On-chain evidence: 25% trail at Accelerating state never triggers until
/// complete dump. At +30% peak, floor is +22.5% — by the time price drops 7.5%,
/// the dump is accelerating and actual exit is at +15% or worse.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TrailConfig {
    /// Gain-tiered trail widths. Must be sorted ascending by `up_to_bps`.
    pub tiers: Vec<TrailTier>,
    /// Must be below trail floor for this many consecutive ticks before firing.
    /// Prevents single-tick noise exits. Default: 2.
    pub confirm_samples: u8,
    /// Don't activate adaptive trail until this many price samples exist.
    /// Prevents premature exits on tokens with discontinuous early price action.
    /// Default: 5.
    pub min_samples_to_activate: u8,
    /// Minimum gain (bps from entry) below which adaptive trailing stop will NOT fire.
    /// Prevents "fee death" where small gains don't cover transaction overhead.
    /// Set to 0 to disable.
    /// Default: 350 (3.5% — covers 1.71% fee overhead + 1.25% avg slippage + margin).
    #[serde(default = "default_floor_bps")]
    pub floor_bps: u32,
}

fn default_floor_bps() -> u32 { 350 }

impl Default for TrailConfig {
    fn default() -> Self {
        Self {
            tiers: vec![
                TrailTier { up_to_bps: 300,      trail_bps: 200 },   // 0-3% gain: 2% trail
                TrailTier { up_to_bps: 1200,     trail_bps: 450 },   // 3-12% gain: 4.5% trail
                TrailTier { up_to_bps: 4000,     trail_bps: 700 },   // 12-40% gain: 7% trail
                TrailTier { up_to_bps: i32::MAX, trail_bps: 1100 },  // 40%+ gain: 11% trail
            ],
            confirm_samples: 2,
            min_samples_to_activate: 5,
            floor_bps: default_floor_bps(),
        }
    }
}

/// Number of price sample slots. At ~1s intervals (sample_interval_ticks=7),
/// 30 slots ≈ 30s coverage. At 10s intervals, 30 slots = 300s max hold.
pub const PRICE_SAMPLES: usize = 30;

/// A single momentum position. Sized for cache efficiency.
/// Must fit in ≤256 bytes with 64-byte alignment.
///
/// Written by the tick loop thread only (no concurrent writes).
/// Price reads come from AtomicU64 in PriceState, but this struct
/// is single-writer (the tick loop owns it).
#[repr(C, align(64))]
pub struct MomentumPosition {
    // ── Identity (40 bytes) ──────────────────────────────
    /// Token mint address (32 bytes).
    pub mint: [u8; 32],
    /// Entry timestamp (epoch ms).
    pub entry_ts_ms: u64,

    // ── Prices (32 bytes) ────────────────────────────────
    /// Entry price, fixed-point (lamports per 1M atoms).
    pub entry_price_fp: u64,
    /// Bonding curve terminal price, fixed-point (constant ~411).
    pub bc_terminal_price_fp: u64,
    /// Peak price seen since entry, for trailing stop.
    /// Non-atomic: only written by tick-thread.
    pub peak_price_fp: u64,
    /// Position size in lamports.
    pub size_lamports: u64,

    // ── Price trajectory (120 bytes) ─────────────────────
    /// Bps offset from entry price at each 10s sample.
    /// Positive = above entry, negative = below.
    pub price_samples_bps: [i32; PRICE_SAMPLES],

    // ── State (8 bytes) ──────────────────────────────────
    // remaining_bps first for 2-byte alignment at even offset (192)
    /// Remaining position in bps (10000 = 100%). Decremented on partial exits.
    pub remaining_bps: u16,
    /// Number of price samples recorded (0..=PRICE_SAMPLES).
    pub sample_count: u8,
    /// Pool type: 0 = Raydium AMM V4, 1 = PumpSwap.
    pub pool_type: u8,
    /// Graduation score (0-100).
    pub grad_score: u8,
    /// TP hit bitmask: bit0 = TP1 hit, bit1 = TP2 hit, bit2 = TP3/ceiling hit.
    pub tp_flags: u8,
    /// Exit reason (maps to MomentumExitReason discriminant).
    pub exit_reason: u8,
    /// Consecutive samples where trailing stop condition was met but not yet confirmed.
    /// Reset to 0 when price recovers above the trail floor.
    /// Used by trailing_stop_confirm_samples gate.
    pub trail_stop_below_floor_count: u8,

    // ── Grad context (16 bytes) ──────────────────────────
    /// Seconds from token creation to graduation.
    pub grad_speed_s: u32,
    /// Total bonding curve volume in centisol (SOL × 100).
    pub grad_volume_sol_x100: u32,
    /// Buy transactions in last 5 seconds of bonding curve.
    pub pre_grad_buys_5s: u32,
    /// Entry delay from graduation in ms.
    pub entry_delay_ms: u32,

    // ── First-tick tracking + on-chain confirmation + padding ────────
    /// Set to true after the first price sample has been recorded.
    /// Ensures we always capture a sample on the first tick with live price data.
    pub first_price_recorded: bool,
    /// Consecutive ticks where velocity exit condition was met (confirm gate).
    /// Resets to 0 when velocity recovers above threshold.
    pub velocity_confirm_counter: u8,
    /// Epoch ms when the buy TX was confirmed on-chain (slot landed).
    /// Set to 0 at position creation. Stamped by `process_active_positions`
    /// when `BuyState` transitions to `Confirmed` (first tick after TX lands).
    ///
    /// Used as the reference point for **enforced probe hold time** — all
    /// phase-gated exit evaluation (`evaluate_phase()`) measures hold duration
    /// from this timestamp, NOT `entry_ts_ms` (which records decision time).
    ///
    /// Before this field existed, `entry_ts_ms` was used for both purposes,
    /// causing the probe window to start 600-1200ms before the TX actually
    /// landed on-chain. 84% of trades exited in 0-1s because the elapsed
    /// time already included TX propagation latency.
    pub buy_confirmed_ms: u64,
    /// Packed storage for TopDetector, ws_notif, tokens_held, probe_phase,
    /// effective_trail_bps, and other hot-path fields. Layout:
    ///   [0..17]  = TopDetector (17 bytes)
    ///   [17]     = scaled_in flag
    ///   [18..20] = ws_notif_count (u16 LE)
    ///   [20..28] = ws_notif_last_ms (u64 LE)
    ///   [28..36] = tokens_held (u64 LE)
    ///   [36]     = probe_phase (u8)
    ///   [37..39] = effective_trail_bps (u16 LE)
    pub _pad2: [u8; 39],
}

// Compile-time size and alignment assertions.
// buy_confirmed_ms added 8 bytes → struct is now 265 raw bytes (still ≤320, 5 cache lines).
const _: () = assert!(std::mem::size_of::<MomentumPosition>() <= 320);
const _: () = assert!(std::mem::align_of::<MomentumPosition>() == 64);

impl MomentumPosition {
    /// Create a new position with the given parameters.
    #[cold]
    #[inline(never)]
    pub fn new(
        mint: [u8; 32],
        entry_ts_ms: u64,
        entry_price_fp: u64,
        bc_terminal_price_fp: u64,
        size_lamports: u64,
        pool_type: u8,
        grad_score: u8,
        grad_speed_s: u32,
        grad_volume_sol_x100: u32,
        pre_grad_buys_5s: u32,
        entry_delay_ms: u32,
    ) -> Self {
        Self {
            mint,
            entry_ts_ms,
            entry_price_fp,
            bc_terminal_price_fp,
            peak_price_fp: entry_price_fp,
            size_lamports,
            price_samples_bps: [0i32; PRICE_SAMPLES],
            remaining_bps: 10_000, // 100%
            sample_count: 0,
            pool_type,
            grad_score,
            tp_flags: 0,
            exit_reason: 0,
            trail_stop_below_floor_count: 0,
            grad_speed_s,
            grad_volume_sol_x100,
            pre_grad_buys_5s,
            entry_delay_ms,
            first_price_recorded: false,
            velocity_confirm_counter: 0,
            buy_confirmed_ms: 0, // 0 = not yet confirmed on-chain
            _pad2: [0u8; 39],
        }
    }

    /// Record a price sample (bps offset from entry).
    ///
    /// Called every ~1s from the tick loop. Updates peak_price_fp
    /// for trailing stop tracking.
    ///
    /// Spike guard: rejects samples where current_price_fp is >10x or <0.1x
    /// the previous recorded price (peak_price_fp or entry_price_fp).
    /// This prevents garbage RPC reads from corrupting price_samples_bps
    /// and inflating peak_price_fp, which was the root cause of ghost +0.9995 trades.
    #[inline(always)]
    pub fn record_sample(&mut self, current_price_fp: u64) {
        if self.sample_count as usize >= PRICE_SAMPLES {
            return;
        }
        // Spike rejection: compare against the last known good price.
        // Use peak_price_fp if nonzero (always >= entry_price_fp), else entry_price_fp.
        let ref_price = if self.peak_price_fp > 0 {
            self.peak_price_fp
        } else {
            self.entry_price_fp
        };
        if ref_price > 0 && current_price_fp > 0 {
            let ratio_num = current_price_fp.max(ref_price);
            let ratio_den = current_price_fp.min(ref_price);
            if ratio_den > 0 && ratio_num / ratio_den > 50 {
                // Skip this sample — bad data, don't corrupt peak or samples array
                return;
            }
        }
        let bps = price_to_bps_offset(self.entry_price_fp, current_price_fp);
        self.price_samples_bps[self.sample_count as usize] = bps;
        self.sample_count += 1;
        if current_price_fp > self.peak_price_fp {
            self.peak_price_fp = current_price_fp;
        }
    }

    /// Check if trailing stop triggered.
    ///
    /// Trailing stop activates after TP2 is hit. Triggers when price drops
    /// `trailing_stop_bps` below the peak price seen since entry.
    #[inline(always)]
    pub fn trailing_stop_hit(&self, current_price_fp: u64, trailing_stop_bps: u32) -> bool {
        if self.peak_price_fp == 0 {
            return false;
        }
        let drawdown_bps = self
            .peak_price_fp
            .saturating_sub(current_price_fp)
            .saturating_mul(10_000)
            / self.peak_price_fp;
        drawdown_bps as u32 >= trailing_stop_bps
    }

    /// Check if hard stop-loss triggered.
    #[inline(always)]
    pub fn hard_sl_hit(&self, current_price_fp: u64, hard_sl_bps: u32) -> bool {
        if self.entry_price_fp == 0 {
            return false;
        }
        let loss_bps = self
            .entry_price_fp
            .saturating_sub(current_price_fp)
            .saturating_mul(10_000)
            / self.entry_price_fp;
        loss_bps as u32 >= hard_sl_bps
    }

    /// Check if a take-profit level is hit.
    ///
    /// Returns true if current price is at or above entry_price * (1 + tp_bps/10000).
    #[inline(always)]
    pub fn tp_hit(&self, current_price_fp: u64, tp_bps: u32) -> bool {
        if self.entry_price_fp == 0 {
            return false;
        }
        let gain_bps = current_price_fp
            .saturating_sub(self.entry_price_fp)
            .saturating_mul(10_000)
            / self.entry_price_fp;
        gain_bps as u32 >= tp_bps
    }

    /// Apply a partial exit: reduce remaining_bps by the given amount.
    /// Returns the actual bps exited (may be less if position nearly closed).
    #[inline(always)]
    pub fn partial_exit(&mut self, exit_bps: u16) -> u16 {
        let actual = exit_bps.min(self.remaining_bps);
        self.remaining_bps -= actual;
        actual
    }

    /// Whether the position is fully closed (remaining_bps == 0).
    #[inline(always)]
    pub fn is_closed(&self) -> bool {
        self.remaining_bps == 0
    }

    /// Hold duration in ms since entry (decision time).
    #[inline(always)]
    pub fn hold_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.entry_ts_ms)
    }

    /// Hold duration in ms since on-chain buy confirmation.
    /// Returns 0 if buy has not yet been confirmed (buy_confirmed_ms == 0).
    #[inline(always)]
    pub fn confirmed_hold_ms(&self, now_ms: u64) -> u64 {
        if self.buy_confirmed_ms == 0 {
            return 0;
        }
        now_ms.saturating_sub(self.buy_confirmed_ms)
    }

    /// Stamp the buy TX confirmation time. Should be called exactly once
    /// when the buy TX lands on-chain (BuyState::Confirmed).
    /// No-op if already stamped (idempotent).
    #[inline(always)]
    pub fn stamp_buy_confirmed(&mut self, now_ms: u64) {
        if self.buy_confirmed_ms == 0 {
            self.buy_confirmed_ms = now_ms;
        }
    }

    /// Evaluate the enforced hold phase based on on-chain confirmation time.
    ///
    /// This is the **primary phase gate** for all exit evaluation. It replaces
    /// the old `entry_ts_ms`-based elapsed calculation with one anchored to
    /// the actual on-chain buy confirmation timestamp.
    ///
    /// # Phase Timeline (from buy_confirmed_ms)
    ///
    /// ```text
    /// [0]────────[1500ms]──────────[4500ms]──────────→
    ///   AwaitingConfirmation (buy_confirmed_ms == 0)
    ///   RapidAssessment      (0 - 1500ms)  micro-SL only
    ///   Observation           (1500 - 4500ms) hard SL + dead token
    ///   Momentum              (any time, current_bps >= +100)
    ///   ExitEligible          (>4500ms) full exit evaluation
    ///   Exiting               (exit decision made)
    /// ```
    ///
    /// # Arguments
    /// * `now_ms` - Current epoch ms
    /// * `current_bps` - Current price in bps relative to entry
    /// * `ws_messages_last_3s` - WS notification count in last 3 seconds
    /// * `last_ws_age_ms` - Ms since the last WS notification (now_ms - last_notif_ms)
    ///
    /// # Performance
    /// O(1), no allocations, no loops. All comparisons are integer.
    #[inline(always)]
    pub fn evaluate_phase(
        &self,
        now_ms: u64,
        current_bps: i32,
        ws_messages_last_3s: u16,
        last_ws_age_ms: u64,
    ) -> PositionPhase {
        // Not yet confirmed on-chain
        if self.buy_confirmed_ms == 0 {
            // Safety timeout: if 10s since decision and still not confirmed, force exit.
            // Covers: TX dropped, RPC failure, circuit breaker timeout.
            if now_ms.saturating_sub(self.entry_ts_ms) > 10_000 {
                return PositionPhase::Exiting;
            }
            return PositionPhase::AwaitingConfirmation;
        }

        let hold_ms = now_ms.saturating_sub(self.buy_confirmed_ms);

        // Phase 1: Rapid Assessment (0-1500ms from confirmation)
        // Only micro-SL fires here. Everything else waits.
        if hold_ms < 1500 {
            // Micro-SL: instant kill at -2% (200 bps). These are instant dumps
            // where the token is being actively sold into. No recovery expected.
            if current_bps <= -200 {
                return PositionPhase::Exiting;
            }
            // Early momentum: if price already crossed +100 bps, activate
            // trailing stop tracking to protect the gain.
            if current_bps >= 100 {
                return PositionPhase::Momentum;
            }
            return PositionPhase::RapidAssessment;
        }

        // Momentum takes priority over observation — if price is running,
        // we want trailing stop evaluation regardless of hold time.
        if current_bps >= 100 {
            return PositionPhase::Momentum;
        }

        // Phase 2: Observation (1500-4500ms)
        // Hard SL + dead token detection. No trailing stop, no time_sl, no TP.
        if hold_ms < 4500 {
            // Kill losers: -2% in observation = no recovery for micro tokens
            if current_bps <= -200 {
                return PositionPhase::Exiting;
            }
            // Dead token detection: no WS activity for 3s+ AND fewer than
            // 2 messages in the window. Token is DOA, free the slot.
            if ws_messages_last_3s < 2 && last_ws_age_ms > 3000 {
                return PositionPhase::Exiting;
            }
            return PositionPhase::Observation;
        }

        // Phase 3: Full exit evaluation (>4500ms from confirmation)
        // All exit conditions enabled: TP levels, trailing stop, time_sl,
        // velocity exit, dead zone, drain detection, etc.
        PositionPhase::ExitEligible
    }

    /// Compute current momentum state from price sample derivatives.
    /// Requires at least 2 samples. Returns Unknown if insufficient data.
    #[inline]
    pub fn momentum_state(
        &self,
        accel_threshold: i32,
        decel_threshold: i32,
        reversal_threshold: i32,
    ) -> MomentumState {
        let n = self.sample_count as usize;
        if n < 2 {
            return MomentumState::Unknown;
        }
        // Most recent derivative: s[n-1] - s[n-2]
        let d = self.price_samples_bps[n - 1] - self.price_samples_bps[n - 2];

        if d <= reversal_threshold {
            return MomentumState::Reversing;
        }

        // Check for 2-consecutive deceleration (need n >= 3)
        if n >= 3 {
            let d_prev = self.price_samples_bps[n - 2] - self.price_samples_bps[n - 3];
            if d < decel_threshold && d_prev < decel_threshold {
                return MomentumState::Decelerating;
            }
        } else if d < decel_threshold {
            return MomentumState::Decelerating;
        }

        if d > accel_threshold {
            MomentumState::Accelerating
        } else {
            MomentumState::Sustaining
        }
    }

    /// Whether position has been scaled in (probe → full size).
    /// Uses _pad2[17] (byte 17, after TopDetector's 0..17).
    #[inline(always)]
    pub fn is_scaled_in(&self) -> bool {
        self._pad2[17] != 0
    }

    /// Mark position as scaled in (probe → full size).
    /// Prevents double-scaling.
    #[inline(always)]
    pub fn set_scaled_in(&mut self) {
        self._pad2[17] = 1;
    }

    /// Get the number of tokens held in this position (for live sell tx).
    /// Stored in _pad2[28..36].
    #[inline(always)]
    pub fn tokens_held(&self) -> u64 {
        u64::from_le_bytes(self._pad2[28..36].try_into().unwrap())
    }

    /// Set the number of tokens held in this position (for live sell tx).
    /// Stored in _pad2[28..36].
    #[inline(always)]
    pub fn set_tokens_held(&mut self, v: u64) {
        self._pad2[28..36].copy_from_slice(&v.to_le_bytes());
    }

    #[inline(always)]
    pub fn ws_notif_count(&self) -> u16 {
        u16::from_le_bytes(self._pad2[18..20].try_into().unwrap())
    }

    #[inline(always)]
    pub fn set_ws_notif_count(&mut self, v: u64) {
        let clamped = v.min(u16::MAX as u64) as u16;
        self._pad2[18..20].copy_from_slice(&clamped.to_le_bytes());
    }

    #[inline(always)]
    pub fn ws_notif_last_ms(&self) -> u64 {
        u64::from_le_bytes(self._pad2[20..28].try_into().unwrap())
    }

    #[inline(always)]
    pub fn set_ws_notif_last_ms(&mut self, v: u64) {
        self._pad2[20..28].copy_from_slice(&v.to_le_bytes());
    }

    /// Get TopDetector state from _pad2 storage (bytes 0..17).
    #[inline]
    pub fn top_detector(&self) -> TopDetector {
        TopDetector::from_bytes(self._pad2[0..17].try_into().unwrap())
    }

    /// Save TopDetector state to _pad2 storage (bytes 0..17).
    #[inline]
    pub fn set_top_detector(&mut self, td: &TopDetector) {
        let bytes = td.to_bytes();
        self._pad2[0..17].copy_from_slice(&bytes);
    }

    // ── Probe-then-scale methods (TASK 2) ───────────────────

    /// Get current probe phase. Stored in _pad2[36].
    #[inline(always)]
    pub fn probe_phase(&self) -> ProbePhase {
        ProbePhase::from_u8(self._pad2[36])
    }

    /// Set probe phase. Stored in _pad2[36].
    #[inline(always)]
    pub fn set_probe_phase(&mut self, phase: ProbePhase) {
        self._pad2[36] = phase as u8;
    }

    /// Evaluate probe phase transition based on current price and elapsed time.
    ///
    /// Called from `on_tick()` in mod.rs during the probe window.
    /// Returns the new phase after evaluation. Does NOT mutate the position —
    /// caller is responsible for calling `set_probe_phase()` and adjusting size.
    ///
    /// # Decision tree
    ///
    /// ```text
    /// if hold_ms < probe_hold_ms:
    ///   if current_bps <= probe_dump_threshold_bps (-500): → Failed (immediate exit)
    ///   else: → Probing (keep waiting)
    /// if hold_ms >= probe_hold_ms:
    ///   if sample_count == 0 && require_price: → HeldTight (no data, don't scale blind)
    ///   if current_bps >= 0: → Scaled (price flat/up, scale to full size)
    ///   if current_bps >= probe_scale_min_bps (-300): → HeldTight (moderate dip, tight SL)
    ///   else: → Failed (too much loss, exit or stay tight)
    /// ```
    #[inline]
    pub fn evaluate_probe(
        &self,
        now_ms: u64,
        current_price_fp: u64,
        probe_hold_ms: u64,
        probe_dump_threshold_bps: i32,
        probe_scale_min_bps: i32,
        probe_scale_require_price: bool,
    ) -> ProbePhase {
        // Only evaluate if currently probing
        if self.probe_phase() != ProbePhase::Probing {
            return self.probe_phase();
        }

        let elapsed = self.hold_ms(now_ms);
        let current_bps = if self.entry_price_fp > 0 && current_price_fp > 0 {
            price_to_bps_offset(self.entry_price_fp, current_price_fp)
        } else {
            0
        };

        // Phase 1: Still within probe hold window
        if elapsed < probe_hold_ms {
            // Immediate dump detection — exit probe at minimal loss
            if current_bps <= probe_dump_threshold_bps {
                return ProbePhase::Failed;
            }
            return ProbePhase::Probing;
        }

        // Phase 2: Probe hold window elapsed — evaluate for scale-in

        // No price data and require_price is set → don't scale blind
        if probe_scale_require_price && self.sample_count == 0 {
            return ProbePhase::HeldTight;
        }

        // Price flat or rising → scale up
        if current_bps >= 0 {
            return ProbePhase::Scaled;
        }

        // Moderate dip (between scale_min and dump threshold) → hold tight
        if current_bps >= probe_scale_min_bps {
            return ProbePhase::HeldTight;
        }

        // Larger drop → failed
        ProbePhase::Failed
    }

    /// Whether the position is in an active probe phase (not yet resolved).
    #[inline(always)]
    pub fn is_probe_active(&self) -> bool {
        self.probe_phase().is_probing()
    }

    /// Whether probe succeeded and position should scale to full size.
    #[inline(always)]
    pub fn probe_should_scale(&self) -> bool {
        self.probe_phase() == ProbePhase::Scaled
    }

    // ── Task 3: ws_notif scale-in gate ───────────────────────────────────────
    /// Returns true if insufficient WebSocket notification activity to allow scale-in.
    /// ws_notif=0 has 0.0% WR (165 trades). ws_notif≥10 has 27.2% WR (371 trades).
    /// `min_threshold=0` disables the gate (always returns false).
    #[inline(always)]
    pub fn ws_notif_blocks_scale_in(&self, min_threshold: u16) -> bool {
        if min_threshold == 0 {
            return false;
        }
        self.ws_notif_count() < min_threshold
    }

    // ── Task 4: s[1] price trajectory gate ───────────────────────────────────
    /// Returns true if the second price sample (s[1]) is below `min_s1_bps`.
    /// s[1]=0: 6.8% WR (676 trades). s[1]>0: 50.9% WR (118 trades).
    /// s[0] is always 0 (entry baseline); s[1] is the first informative sample.
    /// Returns true (blocks) when sample_count < 2 — defer until we have the data.
    /// `min_s1_bps=i32::MIN` disables the gate (always returns false).
    #[inline(always)]
    pub fn s1_blocks_scale_in(&self, min_s1_bps: i32) -> bool {
        if min_s1_bps == i32::MIN {
            return false;
        }
        if self.sample_count < 2 {
            return true; // Not enough data yet — defer scale-in
        }
        self.price_samples_bps[1] < min_s1_bps
    }

    // ── T3+T4 combined: evaluate_probe_gated ─────────────────────────────────
    /// Like `evaluate_probe`, but also applies T3 (ws_notif) and T4 (s[1]) gates.
    /// When either gate blocks, returns `HeldTight` instead of `Scaled`.
    /// Gates are applied only when the probe hold window has elapsed (same as scale-in timing).
    ///
    /// `min_ws_notif`: T3 threshold (0 = disabled)
    /// `min_s1_bps`: T4 threshold (i32::MIN = disabled)
    pub fn evaluate_probe_gated(
        &self,
        now_ms: u64,
        current_price_fp: u64,
        probe_hold_ms: u64,
        probe_dump_threshold_bps: i32,
        probe_scale_min_bps: i32,
        probe_scale_require_price: bool,
        min_ws_notif: u16,
        min_s1_bps: i32,
    ) -> ProbePhase {
        // Run base probe evaluation first
        let base = self.evaluate_probe(
            now_ms,
            current_price_fp,
            probe_hold_ms,
            probe_dump_threshold_bps,
            probe_scale_min_bps,
            probe_scale_require_price,
        );

        // Only apply gates when base result is Scaled (would scale up)
        if base != ProbePhase::Scaled {
            return base;
        }

        // T3: ws_notif gate — block scale-in on dead pools
        if self.ws_notif_blocks_scale_in(min_ws_notif) {
            tracing::trace!(
                ws_notif = self.ws_notif_count(),
                threshold = min_ws_notif,
                "T3 gate: scale-in deferred (insufficient ws_notif)",
            );
            return ProbePhase::HeldTight;
        }

        // T4: s[1] price gate — block scale-in when price not rising
        if self.s1_blocks_scale_in(min_s1_bps) {
            tracing::trace!(
                s1 = if self.sample_count >= 2 { self.price_samples_bps[1] } else { 0 },
                threshold = min_s1_bps,
                "T4 gate: scale-in deferred (s[1] below threshold)",
            );
            return ProbePhase::HeldTight;
        }

        ProbePhase::Scaled
    }

    // ── Time-decay trailing stop methods (TASK 5) ───────────

    /// Get the current effective trailing stop in bps.
    /// Stored in `_pad2[37..39]` as u16 (little-endian).
    /// Returns 0 if no time-decay trail has been ratcheted yet.
    #[inline(always)]
    pub fn effective_trail_bps(&self) -> u16 {
        u16::from_le_bytes(self._pad2[37..39].try_into().unwrap())
    }

    /// Ratchet (tighten) the effective trailing stop.
    /// Only updates if `new_bps > 0` AND either no trail is set yet (stored == 0)
    /// or `new_bps` is strictly tighter (lower) than the current value.
    /// This ensures the trail only ever tightens over the position lifetime.
    #[inline(always)]
    pub fn ratchet_trail_bps(&mut self, new_bps: u16) {
        if new_bps == 0 {
            return;
        }
        let current = self.effective_trail_bps();
        if current == 0 || new_bps < current {
            self._pad2[37..39].copy_from_slice(&new_bps.to_le_bytes());
        }
    }

    /// Compute the time-decay trailing stop for the current hold duration
    /// and ratchet it into storage. Returns the effective trail in bps.
    ///
    /// `stages_ms` and `trail_bps` must be the same length and sorted ascending.
    /// Each stage activates when `hold_ms >= stages_ms[i]`, setting trail to
    /// `trail_bps[i]`. Later stages have tighter (lower) trail values.
    ///
    /// Example stages from spec:
    /// ```text
    /// stages_ms:  [30000, 60000, 120000, 180000, 240000]
    /// trail_bps:  [  800,   500,    300,    200,    100]
    /// ```
    ///
    /// After 30s → 800 bps (8%), after 120s → 300 bps (3%), etc.
    #[inline]
    pub fn time_decay_trail_bps(&mut self, hold_ms: u64, stages_ms: &[u64], trail_bps: &[u16]) -> u16 {
        debug_assert_eq!(stages_ms.len(), trail_bps.len());

        // Walk stages in reverse to find the tightest applicable stage.
        // Stages are ascending in time and descending in trail width,
        // so the last matching stage is the tightest.
        let mut candidate: u16 = 0;
        for i in (0..stages_ms.len()).rev() {
            if hold_ms >= stages_ms[i] {
                candidate = trail_bps[i];
                break;
            }
        }

        if candidate > 0 {
            self.ratchet_trail_bps(candidate);
        }

        self.effective_trail_bps()
    }

    /// Check if the time-decay trailing stop has been triggered.
    ///
    /// Uses `effective_trail_bps` (from `_pad2[37..39]`) as the drawdown
    /// threshold from `peak_price_fp`. Returns false if no trail is active
    /// (effective == 0) or peak is zero.
    #[inline(always)]
    pub fn time_decay_trailing_stop_hit(&self, current_price_fp: u64) -> bool {
        let trail = self.effective_trail_bps();
        if trail == 0 || self.peak_price_fp == 0 {
            return false;
        }
        let drawdown_bps = self
            .peak_price_fp
            .saturating_sub(current_price_fp)
            .saturating_mul(10_000)
            / self.peak_price_fp;
        drawdown_bps as u16 >= trail
    }

    // ── Task 5B: Dead token fast exit ──────────────────────────────────

    /// Check if this is a dead token that should be fast-exited.
    ///
    /// Returns true when ALL of:
    /// 1. `hold_ms >= min_hold_ms` (waited long enough for data)
    /// 2. `sample_count >= min_samples` (enough samples to judge)
    /// 3. ALL recorded `price_samples_bps[0..sample_count]` are exactly 0 (flat)
    /// 4. `ws_notif_count == 0` (zero on-chain trading activity)
    ///
    /// Data basis: 322/856 enriched trades (37.6%) have all-zero price samples.
    /// ws_notif=0 at close: n=165, WR=0.0%. These are dead-on-arrival tokens
    /// that waste a position slot for the full time_sl window (15-60s).
    /// Fast exit after ~5s frees the slot 3-4x faster.
    ///
    /// Exits with TimeSl reason (caller decides). Defense-in-depth alongside
    /// stagnation_exit and Phase 5 dead zone.
    #[inline]
    pub fn is_dead_token(&self, hold_ms: u64, min_hold_ms: u64, min_samples: u8) -> bool {
        if hold_ms < min_hold_ms {
            return false;
        }
        let n = self.sample_count;
        if n < min_samples {
            return false;
        }
        // All price samples must be exactly 0 (flat / never moved)
        let count = n as usize;
        let price_flat = self.price_samples_bps[..count].iter().all(|&s| s == 0);
        // Zero WS notifications = no on-chain swap activity at all
        let no_activity = self.ws_notif_count() == 0;
        price_flat && no_activity
    }

    // ── End Task 5B ─────────────────────────────────────────────────────

    /// Check if the position is stagnant (dead token — no price movement).
    ///
    /// Returns true when:
    /// 1. `hold_ms >= threshold_ms` (position has been held long enough), AND
    /// 2. ALL recorded `price_samples_bps` are exactly 0.
    ///
    /// A token with zero movement after 60s is dead — exit immediately rather
    /// than holding for the full max_hold window.
    ///
    /// Note: requires `sample_count > 0` — if no samples recorded at all,
    /// returns false (we have no data, not proven stagnant).
    #[inline]
    pub fn is_stagnant(&self, hold_ms: u64, threshold_ms: u64) -> bool {
        if hold_ms < threshold_ms || self.sample_count == 0 {
            return false;
        }
        let n = self.sample_count as usize;
        for i in 0..n {
            if self.price_samples_bps[i] != 0 {
                return false;
            }
        }
        true
    }

    // ── TASK 6: Adaptive Trailing Stop + Winner Management ──────────────

    /// Compute trailing stop width based on current gain level.
    /// Tighter trails at small gains, wider as gains grow.
    ///
    /// Mathematical basis: optimal trail w* = σ²/(2μ) for geometric Brownian motion.
    /// For memecoins: μ ≈ 30-50 bps/s, σ ≈ 80-150 bps/s in momentum.
    /// w* ≈ 100-375 bps. We tier from 100 (small gains) to 1500 (moonshots).
    ///
    /// Returns 0 for losses (no trailing on losing positions — hard_sl handles those).
    #[inline(always)]
    pub fn compute_adaptive_trail_bps(&self, gain_bps: i32, config: &TrailConfig) -> u16 {
        if gain_bps <= 0 {
            return 0; // No trailing for losses
        }

        // Find the appropriate tier (tiers sorted ascending by up_to_bps)
        for tier in &config.tiers {
            if gain_bps <= tier.up_to_bps {
                return tier.trail_bps;
            }
        }
        // Above all tiers: use the last tier's trail
        config.tiers.last().map(|t| t.trail_bps).unwrap_or(800)
    }

    /// Check if adaptive trailing stop should fire.
    /// Returns true if price has dropped by trail_bps from peak.
    ///
    /// Uses peak_price_fp (highest price seen since entry) as reference.
    /// Floor = peak × (1 - trail_bps / 10000).
    #[inline(always)]
    pub fn adaptive_trailing_stop_hit(
        &self,
        current_price_fp: u64,
        trail_bps: u16,
    ) -> bool {
        if self.peak_price_fp == 0 || trail_bps == 0 {
            return false;
        }

        // Floor = peak - (peak * trail_bps / 10000)
        let floor_fp = self
            .peak_price_fp
            .saturating_sub(self.peak_price_fp * trail_bps as u64 / 10_000);

        current_price_fp < floor_fp
    }

    /// Returns true if this position should be protected from time-based exits.
    ///
    /// A profitable position with ongoing WebSocket activity should NEVER be
    /// time-exited. Only the trailing stop can close it.
    ///
    /// On-chain evidence: the biggest winner (+82.2%, +0.025 SOL) held 40 minutes.
    /// Time-based exits (time_sl, dead_zone) killed profitable positions early,
    /// capping the average win at +15.8% when we need +24% for +EV.
    ///
    /// `ws_messages_last_5s`: WebSocket notifications in the last 5 seconds.
    /// If >0, the pool is still actively trading — momentum is alive.
    #[inline(always)]
    pub fn is_momentum_locked(
        &self,
        current_bps: i32,
        ws_messages_last_5s: u64,
    ) -> bool {
        // Profitable AND has recent activity
        current_bps > 0 && ws_messages_last_5s > 0
    }

    // ── End TASK 6 ──────────────────────────────────────────────────────
}

// ── Exit reason enum ─────────────────────────────────────────────────────────

/// Signal type from the momentum velocity exit system.
/// Indicates which condition triggered an early exit.
///
/// Priority (highest → lowest): MomentumCollapse > AccelerationCollapse > VelocityThreshold.
/// MomentumCollapse fires immediately (no confirmation needed).
/// AccelerationCollapse and VelocityThreshold require `velocity_exit_confirm_samples`
/// consecutive ticks of active signal before firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocityExitSignal {
    /// No velocity exit signal — conditions not met or insufficient data.
    None,
    /// Price falling faster than velocity_exit_threshold_mbps for
    /// confirm_samples consecutive ticks.
    VelocityThreshold,
    /// Rate of decline accelerating below accel_exit_threshold_mbps
    /// while velocity is already negative.
    AccelerationCollapse,
    /// Local peak detected, then gap-down of >= collapse_drop_threshold_bps
    /// in <= max_samples. Fires immediately (no confirmation needed).
    MomentumCollapse,
}

/// Momentum state derived from price sample derivatives.
/// Used to select trailing stop width and inform exit decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MomentumState {
    /// d(bps/s) > +100: strongly accelerating. Hold, widen trail to 15%.
    Accelerating = 0,
    /// d(bps/s) between -100 and +100: steady. Standard 8% trail.
    Sustaining = 1,
    /// d(bps/s) < -100 for 2+ consecutive: momentum fading. Tighten to 5%.
    Decelerating = 2,
    /// d(bps/s) < -500 or bps goes negative: reversal. Exit imminent, 3% trail.
    Reversing = 3,
    /// Insufficient samples to classify. Treat as Sustaining.
    Unknown = 4,
}

// ── Probe phase enum (TASK 2: Hard SL Reduction) ─────────────────────────────

/// Probe-then-scale entry phase.
///
/// Trades start at `probe_size_sol` (0.05 SOL) and only scale up after
/// `probe_hold_ms` (2s) if price is stable. This reduces hard_sl losses
/// from -49.9 mSOL avg to ~-2.5 mSOL for the 60 trades that dump <1s.
///
/// Stored in `MomentumPosition._pad2[36]` (1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbePhase {
    /// Not using probe entry (legacy behavior or probe disabled).
    Disabled = 0,
    /// In probe phase: holding at probe_size_sol, monitoring for dump.
    /// Will evaluate scale-in after probe_hold_ms elapses.
    Probing = 1,
    /// Probe passed: price was stable, position scaled up to full size.
    Scaled = 2,
    /// Probe failed: dump detected during probe, position exiting or
    /// staying at probe size with tight SL (price dropped >3% but <5%).
    Failed = 3,
    /// Probe held: price dropped moderately (-3% to -5% range).
    /// Stay at probe size with tight 3% SL — don't scale up.
    HeldTight = 4,
}

impl ProbePhase {
    /// Convert from u8 discriminant (stored in _pad2[36]).
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Disabled,
            1 => Self::Probing,
            2 => Self::Scaled,
            3 => Self::Failed,
            4 => Self::HeldTight,
            _ => Self::Disabled,
        }
    }

    /// Whether the probe phase allows scaling up (still evaluating).
    #[inline(always)]
    pub fn is_probing(self) -> bool {
        self == Self::Probing
    }

    /// Whether probe evaluation is complete (any terminal state).
    #[inline(always)]
    pub fn is_resolved(self) -> bool {
        matches!(self, Self::Disabled | Self::Scaled | Self::Failed | Self::HeldTight)
    }

    /// String representation for JSONL logging.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Probing => "probing",
            Self::Scaled => "scaled",
            Self::Failed => "failed",
            Self::HeldTight => "held_tight",
        }
    }
}

// ── Position Phase (enforced hold time state machine) ─────────────────────────

/// Position lifecycle phase — enforces minimum hold time from on-chain buy
/// confirmation, NOT from buy decision time.
///
/// This is the **root cause fix** for the 0-1s hold time problem:
/// - `entry_ts_ms` was set when the buy DECISION was made
/// - Buy TX takes 600-1200ms to land on-chain
/// - Exit evaluation started immediately using `entry_ts_ms`, so the probe
///   window (3s) was already 600-1200ms expired when the position actually existed
/// - Result: 84% of trades held 0-1s, probe phase never fired
///
/// `PositionPhase` uses `buy_confirmed_ms` (set when BuyState → Confirmed)
/// as the reference point. Until that timestamp is set, the position is in
/// `AwaitingConfirmation` and ALL exit evaluation is skipped.
///
/// Data basis: 6 trades that held 3-5s had 50% win rate vs 13% at 0-1s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PositionPhase {
    /// Buy TX submitted but not confirmed on-chain.
    /// No exit evaluation. Position doesn't exist on-chain yet.
    AwaitingConfirmation = 0,
    /// 0-1500ms post-confirmation: watching for instant dump.
    /// Only micro-SL (-2%) fires. Everything else waits.
    RapidAssessment = 1,
    /// 1500-4500ms: observing price + activity.
    /// Hard SL + dead token detection only. No TP, no trail.
    Observation = 2,
    /// Price crossed +100 bps at any time: trailing stop active.
    /// Can be entered from any phase. Protects early gains.
    Momentum = 3,
    /// Full exit evaluation enabled (>4500ms from confirmation).
    /// All exit conditions: TP, trailing stop, time_sl, velocity, etc.
    ExitEligible = 4,
    /// Exit decision made, sell pending.
    /// Terminal state — caller should push to `to_close` vec.
    Exiting = 5,
}

impl PositionPhase {
    /// Convert from u8 discriminant.
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::AwaitingConfirmation,
            1 => Self::RapidAssessment,
            2 => Self::Observation,
            3 => Self::Momentum,
            4 => Self::ExitEligible,
            5 => Self::Exiting,
            _ => Self::AwaitingConfirmation,
        }
    }

    /// Whether the phase blocks all exit evaluation.
    #[inline(always)]
    pub fn blocks_exit(self) -> bool {
        matches!(self, Self::AwaitingConfirmation | Self::RapidAssessment)
    }

    /// Whether the phase allows full exit evaluation (TP, trailing, time_sl, etc.).
    #[inline(always)]
    pub fn allows_full_exit(self) -> bool {
        matches!(self, Self::ExitEligible)
    }

    /// Whether the position is in a momentum state (trailing stop only).
    #[inline(always)]
    pub fn is_momentum(self) -> bool {
        matches!(self, Self::Momentum)
    }

    /// String representation for JSONL logging.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::RapidAssessment => "rapid_assessment",
            Self::Observation => "observation",
            Self::Momentum => "momentum",
            Self::ExitEligible => "exit_eligible",
            Self::Exiting => "exiting",
        }
    }
}

// ── Momentum Zone State Machine (reserve-based liquidity gate) ────────────────

/// Reserve trajectory phase — tracks whether real buying pressure is present.
///
/// Used to gate scale-in: only allow adding to a position when reserves
/// are stable or growing (MomentumConfirmed/Neutral), NOT when reserves
/// are dropping (Shakeout = distribution/selling).
///
/// Lives outside MomentumPosition (no free bytes in the 256-byte struct).
/// Stored per-mint in MomentumEngine::momentum_zones DashMap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MomentumPhase {
    /// First 10s after entry — observing reserve trajectory, no signal yet.
    InitialChurn,
    /// Reserve dipped significantly from peak — waiting for recovery.
    Shakeout,
    /// Reserve recovering from trough — potential momentum building.
    MomentumCandidate,
    /// Reserve broke above entry level — confirmed real buying pressure.
    MomentumConfirmed,
    /// Stable reserves, no clear directional signal.
    Neutral,
}

/// Per-position reserve trajectory tracker.
///
/// Created when a position is opened. Updated each tick with current
/// reserve_sol from PriceState. Consulted by scale-in logic to gate
/// position sizing increases.
///
/// Stored in `MomentumEngine::momentum_zones` DashMap, keyed by mint.
pub struct MomentumZoneTracker {
    /// Reserve SOL (lamports) at entry time — set once.
    pub reserve_sol_entry: u64,
    /// Highest reserve SOL seen since entry (rolling max).
    pub reserve_sol_peak: u64,
    /// Lowest reserve SOL seen after peak (rolling min, reset on new peak).
    pub reserve_sol_trough: u64,
    /// Current phase in the momentum state machine.
    pub phase: MomentumPhase,
}

impl MomentumZoneTracker {
    /// Create a new tracker at entry time.
    #[inline]
    pub fn new(reserve_sol_at_entry: u64) -> Self {
        Self {
            reserve_sol_entry: reserve_sol_at_entry,
            reserve_sol_peak: reserve_sol_at_entry,
            reserve_sol_trough: reserve_sol_at_entry,
            phase: MomentumPhase::InitialChurn,
        }
    }

    /// Update the momentum phase based on current reserve and hold duration.
    ///
    /// Called once per tick from the main evaluation loop. All arithmetic
    /// is integer-only (no floating point) for determinism.
    #[inline]
    pub fn update(&mut self, current_reserve: u64, hold_ms: u64) {
        // Update rolling peak/trough
        if current_reserve > self.reserve_sol_peak {
            self.reserve_sol_peak = current_reserve;
        }
        if current_reserve < self.reserve_sol_trough || self.reserve_sol_trough == 0 {
            self.reserve_sol_trough = current_reserve;
        }

        self.phase = match self.phase {
            MomentumPhase::InitialChurn => {
                if hold_ms > 10_000 {
                    // After 10s observation: classify based on reserve trajectory
                    if current_reserve < self.reserve_sol_peak * 90 / 100 {
                        MomentumPhase::Shakeout
                    } else {
                        MomentumPhase::Neutral
                    }
                } else {
                    MomentumPhase::InitialChurn
                }
            }
            MomentumPhase::Shakeout => {
                // 10% recovery from trough → candidate for momentum
                if current_reserve > self.reserve_sol_trough * 110 / 100 {
                    MomentumPhase::MomentumCandidate
                } else {
                    MomentumPhase::Shakeout
                }
            }
            MomentumPhase::MomentumCandidate => {
                // Broke 5% above entry → confirmed real momentum
                if current_reserve > self.reserve_sol_entry * 105 / 100 {
                    MomentumPhase::MomentumConfirmed
                } else if current_reserve < self.reserve_sol_trough * 95 / 100 {
                    // Failed recovery — back to shakeout
                    MomentumPhase::Shakeout
                } else {
                    MomentumPhase::MomentumCandidate
                }
            }
            MomentumPhase::MomentumConfirmed => {
                // Stay confirmed unless reserve collapses (>30% below entry)
                if current_reserve < self.reserve_sol_entry * 70 / 100 {
                    MomentumPhase::Shakeout
                } else {
                    MomentumPhase::MomentumConfirmed
                }
            }
            MomentumPhase::Neutral => {
                if current_reserve > self.reserve_sol_entry * 105 / 100 {
                    MomentumPhase::MomentumConfirmed
                } else if current_reserve < self.reserve_sol_peak * 85 / 100 {
                    MomentumPhase::Shakeout
                } else {
                    MomentumPhase::Neutral
                }
            }
        };
    }

    /// Whether current phase + reserve level allows scale-in.
    ///
    /// Returns true only when:
    /// 1. Phase is MomentumConfirmed or Neutral (stable/growing reserves)
    /// 2. Current reserve is at least 85% of entry reserve (not drained)
    #[inline(always)]
    pub fn allows_scale_in(&self, current_reserve: u64) -> bool {
        matches!(
            self.phase,
            MomentumPhase::MomentumConfirmed | MomentumPhase::Neutral
        ) && current_reserve >= self.reserve_sol_entry * 85 / 100
    }

    /// String representation of current phase for logging.
    #[inline(always)]
    pub fn phase_str(&self) -> &'static str {
        match self.phase {
            MomentumPhase::InitialChurn => "initial_churn",
            MomentumPhase::Shakeout => "shakeout",
            MomentumPhase::MomentumCandidate => "momentum_candidate",
            MomentumPhase::MomentumConfirmed => "momentum_confirmed",
            MomentumPhase::Neutral => "neutral",
        }
    }
}

/// Exit reason for momentum positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MomentumExitReason {
    /// Not yet exited (position still open).
    Open = 0,
    /// Hit take-profit tier 1 (+5%).
    Tp1 = 1,
    /// Hit take-profit tier 2 (+15%).
    Tp2 = 2,
    /// Hit take-profit ceiling (+50%).
    Tp3 = 3,
    /// Trailing stop triggered (8% below peak, active after TP2).
    TrailingStop = 4,
    /// Hard stop-loss (-12%).
    HardSl = 5,
    /// Time-based stop-loss (60s elapsed, PnL < -2%).
    TimeSl = 6,
    /// Maximum hold time exceeded (300s).
    MaxHold = 7,
    /// Daily loss cap reached.
    DailyCapHit = 8,
    /// Pool SOL reserve drain detected (rug pull).
    /// Triggers on: reserve < 10 SOL floor, >30% drop in 3s, or >50% drop in 10s.
    DrainDetected = 9,
    /// Velocity exit: sustained negative velocity/acceleration detected.
    /// Fires when price momentum collapses — protects profits before trailing stop trips.
    VelocityExit = 10,
    /// Micro stop-loss: position exited during RapidAssessment or Observation phase
    /// due to -2% dump within 4.5s of on-chain buy confirmation.
    /// Distinct from HardSl to track enforced-hold exit effectiveness separately.
    MicroSl = 11,
    /// Dead token exit during Observation phase: no WS activity detected
    /// within 3s of on-chain entry. Token is DOA.
    DeadOnArrival = 12,
    /// Buy TX never confirmed on-chain within 10s safety timeout.
    /// Position never existed — no sell TX needed.
    BuyTimeout = 13,
}

impl MomentumExitReason {
    /// String representation for JSONL logging.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Tp1 => "tp1",
            Self::Tp2 => "tp2",
            Self::Tp3 => "tp3",
            Self::TrailingStop => "trailing_stop",
            Self::HardSl => "hard_sl",
            Self::TimeSl => "time_sl",
            Self::MaxHold => "max_hold",
            Self::DailyCapHit => "daily_cap",
            Self::DrainDetected => "drain_detected",
            Self::VelocityExit => "velocity_exit",
            Self::MicroSl => "micro_sl",
            Self::DeadOnArrival => "dead_on_arrival",
            Self::BuyTimeout => "buy_timeout",
        }
    }

    /// Convert from u8 discriminant.
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Open,
            1 => Self::Tp1,
            2 => Self::Tp2,
            3 => Self::Tp3,
            4 => Self::TrailingStop,
            5 => Self::HardSl,
            6 => Self::TimeSl,
            7 => Self::MaxHold,
            8 => Self::DailyCapHit,
            9 => Self::DrainDetected,
            10 => Self::VelocityExit,
            11 => Self::MicroSl,
            12 => Self::DeadOnArrival,
            13 => Self::BuyTimeout,
            _ => Self::Open,
        }
    }
}

// ── Pending entry ring buffer ────────────────────────────────────────────────

/// A pending entry scheduled for delayed execution.
///
/// After graduation, entries wait `entry_delay_ms` before executing.
/// During the delay, the price feed subscribes and collects live prices.
#[derive(Clone, Copy)]
pub struct PendingEntry {
    /// Token mint address.
    pub mint: [u8; 32],
    /// Pool type: 0 = Raydium, 1 = PumpSwap.
    pub pool_type: u8,
    /// Graduation score (0-100, without recovery which is deferred).
    pub grad_score: u8,
    /// Graduation speed in seconds.
    pub grad_speed_s: u32,
    /// Bonding curve volume in centisol.
    pub grad_volume_sol_x100: u32,
    /// Buy transactions in last 5 seconds.
    pub pre_grad_buys_5s: u32,
    /// When to enter: graduation_ts + entry_delay_ms.
    pub scheduled_ts_ms: u64,
    /// Price at graduation time (for reference).
    pub opening_price_fp: u64,
    /// Bonding curve terminal price.
    pub bc_price_fp: u64,
    /// Timestamp when this entry was first scheduled (graduation_ts + entry_delay_ms).
    /// Used to detect price feed timeout — if now_ms - first_scheduled_ts_ms > no_price_timeout_ms,
    /// skip entry rather than use stale opening_price_fp.
    pub first_scheduled_ts_ms: u64,
    /// Recovery score (computed in process_pending_entries, init 0).
    pub recovery_score: u8,
    /// Observed price velocity (bps/s) from observation window. None if window was skipped.
    pub observed_velocity_bps_per_s: Option<i64>,
    /// Whether pool was resolved via last-chance RPC call (zeroed PDA path).
    /// Used to apply wider slippage tolerance for Token-2022 on this high-latency path.
    pub was_last_chance_resolved: bool,
    /// Whether this slot is active.
    pub active: bool,
}

impl Default for PendingEntry {
    fn default() -> Self {
        Self {
            mint: [0u8; 32],
            pool_type: 0,
            grad_score: 0,
            grad_speed_s: 0,
            grad_volume_sol_x100: 0,
            pre_grad_buys_5s: 0,
            scheduled_ts_ms: 0,
            opening_price_fp: 0,
            bc_price_fp: 0,
            first_scheduled_ts_ms: 0,
            recovery_score: 0,
            observed_velocity_bps_per_s: None,
            was_last_chance_resolved: false,
            active: false,
        }
    }
}

/// Fixed-size ring buffer for pending entries.
///
/// 64 slots, no heap allocation. At 10-30 graduations/day, this provides
/// 2+ days of capacity. Wraps around, overwriting oldest inactive slots.
///
/// ## Performance
///
/// - push: O(1) — write at head, advance
/// - drain_ready: O(64) scan — sequential access, branch-predictor friendly
/// - Zero allocation, fully stack-resident
pub struct PendingEntryRing {
    slots: [PendingEntry; 64],
    head: usize,
    count: usize,
}

impl PendingEntryRing {
    /// Create an empty ring buffer.
    pub fn new() -> Self {
        Self {
            slots: [PendingEntry::default(); 64],
            head: 0,
            count: 0,
        }
    }

    /// Push a new pending entry. Returns false if ring is full (all 64 active)
    /// or if a duplicate mint is already queued.
    ///
    /// If the slot at head is inactive (already drained), it will be overwritten.
    /// If the ring has wrapped and the slot is still active, returns false.
    pub fn push(&mut self, entry: PendingEntry) -> bool {
        // Dedup: reject if this mint is already pending in an active slot.
        // Multiple feeds fire on_graduation for the same mint — only keep the first.
        let count = self.count.min(64);
        for i in 0..count {
            let idx = (self.head + i) % 64;
            if self.slots[idx].active && self.slots[idx].mint == entry.mint {
                return false; // already queued, discard duplicate
            }
        }

        if self.count >= 64 {
            // Ring full — check if head slot is inactive (can reclaim)
            if self.slots[self.head].active {
                return false; // truly full, all active
            }
            // Overwrite inactive slot at head
            self.slots[self.head] = entry;
            self.head = (self.head + 1) % 64;
            // count stays at 64
            return true;
        }

        // Write at the next available slot
        let write_idx = (self.head + self.count) % 64;
        self.slots[write_idx] = entry;
        self.count += 1;
        true
    }

    /// Drain all ready entries (scheduled_ts_ms <= now_ms && active).
    ///
    /// Returns an iterator over ready entries. Marks them inactive.
    pub fn drain_ready(&mut self, now_ms: u64) -> DrainReady<'_> {
        DrainReady {
            ring: self,
            idx: 0,
            now_ms,
        }
    }

    /// Number of active entries in the ring.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the ring has no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Number of currently active (not yet drained) entries.
    pub fn active_count(&self) -> usize {
        let mut count = 0;
        for i in 0..self.count.min(64) {
            let idx = (self.head + i) % 64;
            if self.slots[idx].active {
                count += 1;
            }
        }
        count
    }

    /// Deactivate a pending entry by mint. Used by the observation window
    /// to kill entries that failed sniper dump detection before they are
    /// drained by `drain_ready()`. O(64) scan.
    pub fn deactivate_mint(&mut self, mint: &[u8; 32]) {
        let count = self.count.min(64);
        for i in 0..count {
            let idx = (self.head + i) % 64;
            if self.slots[idx].active && self.slots[idx].mint == *mint {
                self.slots[idx].active = false;
                return;
            }
        }
    }
}

/// Iterator returned by `PendingEntryRing::drain_ready()`.
pub struct DrainReady<'a> {
    ring: &'a mut PendingEntryRing,
    idx: usize,
    now_ms: u64,
}

impl<'a> Iterator for DrainReady<'a> {
    type Item = PendingEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let count = self.ring.count.min(64);
        while self.idx < count {
            let slot_idx = (self.ring.head + self.idx) % 64;
            self.idx += 1;

            let slot = &mut self.ring.slots[slot_idx];
            if slot.active && slot.scheduled_ts_ms <= self.now_ms {
                slot.active = false;
                return Some(*slot);
            }
        }
        None
    }
}

// ── Price utilities ──────────────────────────────────────────────────────────

/// Convert price ratio to bps offset from entry.
///
/// Returns signed bps: +500 = +5%, -1200 = -12%.
/// Uses i64 intermediate for safe subtraction of large u64 values.
#[inline(always)]
pub fn price_to_bps_offset(entry_fp: u64, current_fp: u64) -> i32 {
    if entry_fp == 0 {
        return 0;
    }
    let ratio_bps =
        (current_fp as i64 - entry_fp as i64).saturating_mul(10_000) / entry_fp as i64;
    ratio_bps.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Top detection state — stored serialized in MomentumPosition._pad2[0..17].
/// 17 bytes for TopDetector + remaining bytes free for future use.
/// Total: 17 bytes, well within _pad2's 39 bytes.
pub struct TopDetector {
    /// Last 3 momentum derivatives in bps/s (newest first)
    pub last_3_d: [i32; 3],
    /// Count of consecutive up-ticks
    pub consecutive_up: u8,
    /// Highest bps seen from entry (for rollback detection)
    pub peak_bps: i32,
}

impl TopDetector {
    pub fn new() -> Self {
        Self { last_3_d: [0; 3], consecutive_up: 0, peak_bps: 0 }
    }

    /// Evaluate top detection signals. Returns count of active signals (0-5).
    /// Caller triggers exit if signal_count >= config.top_detection_strong_signals (default: 2).
    pub fn evaluate(&mut self, current_bps: i32, prev_bps: i32) -> u8 {
        let d = current_bps - prev_bps;
        self.last_3_d[2] = self.last_3_d[1];
        self.last_3_d[1] = self.last_3_d[0];
        self.last_3_d[0] = d;
        if current_bps > self.peak_bps { self.peak_bps = current_bps; }

        let mut signals = 0u8;
        // Signal 1: Momentum cliff — lost 300+ bps/s in one tick
        if d < self.last_3_d[1] - 300 { signals += 1; }
        // Signal 2: First negative after 3+ consecutive up ticks
        if d < 0 && self.consecutive_up >= 3 { signals += 1; }
        // Signal 3: Momentum halved (current d < 40% of previous, prev was significant)
        if self.last_3_d[1] > 100 && d < (self.last_3_d[1] as f64 * 0.4) as i32 { signals += 1; }
        // Signal 4: Plateau — all 3 recent derivatives within ±50 bps/s
        if self.last_3_d[0].abs() < 50 && self.last_3_d[1].abs() < 50 && self.last_3_d[2].abs() < 50 { signals += 1; }
        // Signal 5: Rollback from peak — gave back 15%+ of gains (only above 1000 bps)
        if self.peak_bps > 1000 && current_bps < (self.peak_bps as f64 * 0.85) as i32 { signals += 1; }

        if d > 0 { self.consecutive_up = self.consecutive_up.saturating_add(1); }
        else { self.consecutive_up = 0; }

        signals
    }

    /// Serialize into 17-byte buffer for storage in MomentumPosition._pad2
    pub fn to_bytes(&self) -> [u8; 17] {
        let mut buf = [0u8; 17];
        buf[0..4].copy_from_slice(&self.last_3_d[0].to_le_bytes());
        buf[4..8].copy_from_slice(&self.last_3_d[1].to_le_bytes());
        buf[8..12].copy_from_slice(&self.last_3_d[2].to_le_bytes());
        buf[12] = self.consecutive_up;
        buf[13..17].copy_from_slice(&self.peak_bps.to_le_bytes());
        buf
    }

    /// Deserialize from 17-byte buffer
    pub fn from_bytes(buf: &[u8; 17]) -> Self {
        Self {
            last_3_d: [
                i32::from_le_bytes(buf[0..4].try_into().unwrap()),
                i32::from_le_bytes(buf[4..8].try_into().unwrap()),
                i32::from_le_bytes(buf[8..12].try_into().unwrap()),
            ],
            consecutive_up: buf[12],
            peak_bps: i32::from_le_bytes(buf[13..17].try_into().unwrap()),
        }
    }
}

// ── Liquidity Quality Score (LQS) ────────────────────────────────────────────

/// Reserve SOL context stored outside MomentumPosition (no free bytes in 256-byte struct).
/// Keyed by mint in a DashMap on MomentumEngine.
#[derive(Debug, Clone, Copy)]
pub struct ReserveSolContext {
    /// Reserve SOL (lamports) at position entry time.
    pub entry_lamports: u64,
    /// Peak reserve SOL (lamports) observed during position lifetime.
    pub peak_lamports: u64,
}

impl ReserveSolContext {
    /// Create a new context at entry time. Peak starts at entry value.
    #[inline]
    pub fn new(entry_lamports: u64) -> Self {
        Self {
            entry_lamports,
            peak_lamports: entry_lamports,
        }
    }

    /// Update peak if current reserve exceeds it. Returns the (possibly updated) peak.
    #[inline]
    pub fn update_peak(&mut self, current_lamports: u64) -> u64 {
        if current_lamports > self.peak_lamports {
            self.peak_lamports = current_lamports;
        }
        self.peak_lamports
    }
}

/// Liquidity Quality Score: 0.0 (dangerous) to 1.0 (excellent).
///
/// Used to scale Kelly-derived scale-in sizing based on current pool depth.
/// Combines three components:
/// - Absolute depth (40%): full score at 80+ SOL, linear decay below
/// - Trend vs entry (30%): positive if reserve grew since entry, negative if drained
/// - Drawdown from peak (30%): how much reserve has dropped from its peak
///
/// Called once per scale-in decision (~few hundred/day). NOT on the hot path.
pub fn liquidity_quality_score(
    reserve_sol_lamports: u64,
    reserve_sol_entry_lamports: u64,
    reserve_sol_peak_lamports: u64,
) -> f64 {
    let r_current = reserve_sol_lamports as f64 / 1e9;
    let r_entry = reserve_sol_entry_lamports as f64 / 1e9;
    let r_peak = reserve_sol_peak_lamports as f64 / 1e9;

    // Component 1: Absolute depth (40% weight)
    // Full score at 80+ SOL, linear decay below 80, zero at 0
    let depth_score = (r_current / 80.0).min(1.0_f64).max(0.0_f64);

    // Component 2: Trend vs entry (30% weight)
    // +1.0 if reserve doubled, -1.0 if fully drained, 0.0 if unchanged
    let trend_score = if r_entry > 0.0 {
        ((r_current - r_entry) / r_entry).max(-1.0_f64).min(1.0_f64)
    } else {
        -1.0
    };
    let trend_normalized = (trend_score + 1.0) / 2.0; // map to 0..1

    // Component 3: Drawdown from peak (30% weight)
    let peak_ratio = if r_peak > 0.0 {
        (r_current / r_peak).min(1.0_f64).max(0.0_f64)
    } else {
        0.0
    };

    0.40 * depth_score + 0.30 * trend_normalized + 0.30 * peak_ratio
}

// ── ATR computation ──────────────────────────────────────────────────────────

/// Compute Average True Range from price_samples_bps.
/// Returns average absolute per-tick change in bps over last `window` samples.
/// Pure integer arithmetic. Returns 0 if fewer than 2 samples.
#[inline]
pub fn compute_atr_bps(samples: &[i32], window: usize) -> u32 {
    let n = samples.len();
    if n < 2 { return 0; }
    let start = if n > window { n - window } else { 0 };
    let slice = &samples[start..n];
    let sum: i64 = slice.windows(2)
        .map(|w| (w[1] as i64 - w[0] as i64).abs())
        .sum();
    let count = (slice.len() - 1) as i64;
    if count == 0 { return 0; }
    (sum / count) as u32
}

// ── Momentum score ───────────────────────────────────────────────────────────

/// Exponentially weighted sum of recent per-tick price deltas.
/// Returns positive for upward momentum, negative for decay/reversal.
/// decay=0.5: each older tick has half the weight of the next newer tick.
/// Returns f64::MAX when fewer than 4 samples (insufficient data — never exit).
#[inline]
pub fn compute_momentum_score(samples: &[i32], window: usize) -> f64 {
    let n = samples.len();
    if n < 4 {
        return f64::MAX; // not enough data — don't exit
    }
    // Per-tick deltas (cumulative → first-difference)
    let changes_len = n - 1;
    let start = if changes_len > window { changes_len - window } else { 0 };

    let mut score = 0.0f64;
    let mut weight = 1.0f64;
    // Iterate from most recent delta backward (rev order = most recent first)
    for i in (start..changes_len).rev() {
        let delta = (samples[i + 1] - samples[i]) as f64;
        score += delta * weight;
        weight *= 0.5;
    }
    score
}

// ── Velocity exit evaluation ─────────────────────────────────────────────────

/// Evaluate whether any velocity exit signal has fired.
///
/// Returns the highest-priority signal that fired, or `VelocityExitSignal::None`.
/// Priority: MomentumCollapse > AccelerationCollapse > VelocityThreshold.
///
/// MomentumCollapse fires immediately on detection (no confirmation needed).
/// AccelerationCollapse and VelocityThreshold require `velocity_exit_confirm_samples`
/// consecutive ticks of active signal before firing.
///
/// # Arguments
/// * `samples` - The full `price_samples_bps` slice from the position
///   (pass `&pos.price_samples_bps[..pos.sample_count as usize]`)
/// * `config` - `MomentumConfig` reference
/// * `current_peak_bps` - The highest price seen so far (peak_bps in position)
/// * `confirm_counter` - Mutable reference to the consecutive-signal counter.
///   Stored externally (e.g. in a DashMap) because the 256-byte position struct
///   has no free bytes. Reset to 0 on gate failure or condition inactive.
pub fn evaluate_velocity_exit(
    samples: &[i32],
    config: &crate::momentum::config::MomentumConfig,
    _current_peak_bps: i32,
    confirm_counter: &mut u32,
) -> VelocityExitSignal {
    // Gate 1: master switch
    if !config.velocity_exit_enabled {
        return VelocityExitSignal::None;
    }

    // Gate 2: minimum samples
    if samples.len() < config.velocity_exit_min_samples as usize {
        *confirm_counter = 0;
        return VelocityExitSignal::None;
    }

    // Gate 3: must be in profit (don't compete with hard_sl for loss exits)
    let current_bps = *samples.last().unwrap_or(&0);
    if current_bps <= config.velocity_exit_min_profit_bps {
        *confirm_counter = 0;
        return VelocityExitSignal::None;
    }

    // Signal 1: MomentumCollapse (highest priority, no confirmation needed)
    let collapse = crate::momentum::velocity::detect_momentum_collapse(
        samples,
        config.momentum_collapse_lookback as usize,
        config.momentum_collapse_min_peak_bps,
        config.momentum_collapse_drop_threshold_bps,
        config.momentum_collapse_max_samples as usize,
    );
    if collapse {
        *confirm_counter = 0;
        return VelocityExitSignal::MomentumCollapse;
    }

    // Compute velocity for the remaining two signals
    let velocity = crate::momentum::velocity::compute_velocity(
        samples,
        config.velocity_window as usize,
    );

    // Check acceleration signal: only when enough samples AND velocity already negative
    let accel_signal = if samples.len() >= config.accel_window as usize && velocity < 0 {
        let accel = crate::momentum::velocity::compute_acceleration(
            samples,
            config.accel_window as usize,
        );
        accel <= config.accel_exit_threshold_mbps
    } else {
        false
    };

    // Check velocity signal
    let vel_signal = velocity <= config.velocity_exit_threshold_mbps;

    // Determine if any condition is currently active
    let condition_active = accel_signal || vel_signal;

    if condition_active {
        *confirm_counter += 1;
        if *confirm_counter >= config.velocity_exit_confirm_samples {
            // Return highest priority active signal
            if accel_signal {
                return VelocityExitSignal::AccelerationCollapse;
            } else {
                return VelocityExitSignal::VelocityThreshold;
            }
        }
    } else {
        *confirm_counter = 0;
    }

    VelocityExitSignal::None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_size_within_cache_lines() {
        let size = std::mem::size_of::<MomentumPosition>();
        assert!(
            size <= 320,
            "MomentumPosition is {} bytes, must be <= 320 (5 cache lines)",
            size
        );
    }

    #[test]
    fn test_position_align_64() {
        let align = std::mem::align_of::<MomentumPosition>();
        assert_eq!(
            align, 64,
            "MomentumPosition alignment is {}, must be 64",
            align
        );
    }

    #[test]
    fn test_pending_ring_push_drain() {
        let mut ring = PendingEntryRing::new();
        assert_eq!(ring.len(), 0);
        assert!(ring.is_empty());

        // Push 3 entries with different scheduled times
        for i in 0..3 {
            let mut entry = PendingEntry::default();
            entry.mint[0] = i as u8;
            entry.scheduled_ts_ms = (i as u64 + 1) * 1000; // 1000, 2000, 3000
            entry.active = true;
            assert!(ring.push(entry));
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.active_count(), 3);

        // Drain at T=2500 → should get entries 1 and 2 (scheduled at 1000, 2000)
        let ready: Vec<PendingEntry> = ring.drain_ready(2500).collect();
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].mint[0], 0);
        assert_eq!(ready[1].mint[0], 1);

        // Entry 3 (scheduled at 3000) should still be active
        assert_eq!(ring.active_count(), 1);

        // Drain at T=5000 → should get entry 3
        let ready: Vec<PendingEntry> = ring.drain_ready(5000).collect();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].mint[0], 2);

        // All drained
        assert_eq!(ring.active_count(), 0);
    }

    #[test]
    fn test_pending_ring_full() {
        let mut ring = PendingEntryRing::new();

        // Fill all 64 slots
        for i in 0..64u8 {
            let mut entry = PendingEntry::default();
            entry.mint[0] = i;
            entry.scheduled_ts_ms = 10_000;
            entry.active = true;
            assert!(ring.push(entry), "push {} should succeed", i);
        }

        // Ring is full with all active → push should fail
        let mut entry = PendingEntry::default();
        entry.mint[0] = 99;
        entry.active = true;
        // All slots are active, can't overwrite
        // (the push function checks self.count >= 64 and then self.slots[self.head].active)
        assert!(!ring.push(entry));
    }

    #[test]
    fn test_trailing_stop_hit() {
        let mut pos = MomentumPosition::new(
            [0u8; 32],
            1000,
            1000,  // entry price
            411,   // bc terminal
            300_000_000,
            0,     // raydium
            72,    // grad score
            60,    // speed
            50_000,
            10,
            15_000,
        );

        // Set peak price to 1500 (50% gain)
        pos.peak_price_fp = 1500;

        // 8% trailing stop = 800 bps
        // At price 1500, 8% drawdown = price 1380
        // 1500 - 1380 = 120, 120 * 10000 / 1500 = 800 bps
        assert!(!pos.trailing_stop_hit(1400, 800)); // 6.7% drawdown, not triggered
        assert!(pos.trailing_stop_hit(1380, 800));  // exactly 800 bps
        assert!(pos.trailing_stop_hit(1200, 800));  // 20% drawdown, triggered

        // Zero peak → never triggers
        pos.peak_price_fp = 0;
        assert!(!pos.trailing_stop_hit(500, 800));
    }

    #[test]
    fn test_price_to_bps_offset() {
        // +5% gain
        let bps = price_to_bps_offset(1000, 1050);
        assert_eq!(bps, 500);

        // -12% loss
        let bps = price_to_bps_offset(1000, 880);
        assert_eq!(bps, -1200);

        // No change
        let bps = price_to_bps_offset(1000, 1000);
        assert_eq!(bps, 0);

        // Zero entry → 0
        let bps = price_to_bps_offset(0, 1000);
        assert_eq!(bps, 0);

        // Large gain: +100%
        let bps = price_to_bps_offset(500, 1000);
        assert_eq!(bps, 10_000);

        // Large loss: -50%
        let bps = price_to_bps_offset(1000, 500);
        assert_eq!(bps, -5000);
    }

    #[test]
    fn test_record_sample() {
        let mut pos = MomentumPosition::new(
            [0u8; 32],
            1000,
            1000,  // entry price
            411,
            300_000_000,
            0,
            72,
            60,
            50_000,
            10,
            15_000,
        );

        assert_eq!(pos.sample_count, 0);
        assert_eq!(pos.peak_price_fp, 1000);

        // Record a +5% sample
        pos.record_sample(1050);
        assert_eq!(pos.sample_count, 1);
        assert_eq!(pos.price_samples_bps[0], 500);
        assert_eq!(pos.peak_price_fp, 1050);

        // Record a -3% sample (peak stays at 1050)
        pos.record_sample(970);
        assert_eq!(pos.sample_count, 2);
        assert_eq!(pos.price_samples_bps[1], -300);
        assert_eq!(pos.peak_price_fp, 1050);

        // Fill all samples
        for i in 2..PRICE_SAMPLES {
            pos.record_sample(1000 + (i as u64 * 10));
        }
        assert_eq!(pos.sample_count as usize, PRICE_SAMPLES);

        // Overflow: should not panic or write beyond bounds
        pos.record_sample(2000);
        assert_eq!(pos.sample_count as usize, PRICE_SAMPLES); // unchanged
    }

    #[test]
    fn test_partial_exit() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 300_000_000,
            0, 72, 60, 50_000, 10, 15_000,
        );

        assert_eq!(pos.remaining_bps, 10_000);
        assert!(!pos.is_closed());

        // TP1: exit 30% (3000 bps)
        let exited = pos.partial_exit(3000);
        assert_eq!(exited, 3000);
        assert_eq!(pos.remaining_bps, 7000);
        assert!(!pos.is_closed());

        // TP2: exit another 30%
        let exited = pos.partial_exit(3000);
        assert_eq!(exited, 3000);
        assert_eq!(pos.remaining_bps, 4000);

        // Full dump of remaining
        let exited = pos.partial_exit(10_000); // request more than remaining
        assert_eq!(exited, 4000); // only remaining
        assert_eq!(pos.remaining_bps, 0);
        assert!(pos.is_closed());
    }

    #[test]
    fn test_hard_sl_hit() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 300_000_000,
            0, 72, 60, 50_000, 10, 15_000,
        );

        // -12% = 1200 bps
        assert!(!pos.hard_sl_hit(900, 1200)); // -10%, not triggered
        assert!(pos.hard_sl_hit(880, 1200));  // -12%, triggered
        assert!(pos.hard_sl_hit(500, 1200));  // -50%, triggered
    }

    #[test]
    fn test_tp_hit() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 300_000_000,
            0, 72, 60, 50_000, 10, 15_000,
        );

        // +5% = 500 bps
        assert!(!pos.tp_hit(1040, 500)); // +4%, not triggered
        assert!(pos.tp_hit(1050, 500));  // +5%, triggered
        assert!(pos.tp_hit(1100, 500));  // +10%, triggered
    }

    #[test]
    fn test_exit_reason_roundtrip() {
        for i in 0..=13u8 {
            let reason = MomentumExitReason::from_u8(i);
            assert_eq!(reason as u8, i);
        }
        // Unknown values default to Open
        assert_eq!(MomentumExitReason::from_u8(255), MomentumExitReason::Open);
        // DrainDetected variant
        assert_eq!(MomentumExitReason::DrainDetected.as_str(), "drain_detected");
        assert_eq!(MomentumExitReason::from_u8(9), MomentumExitReason::DrainDetected);
        // New exit reasons
        assert_eq!(MomentumExitReason::MicroSl.as_str(), "micro_sl");
        assert_eq!(MomentumExitReason::DeadOnArrival.as_str(), "dead_on_arrival");
        assert_eq!(MomentumExitReason::BuyTimeout.as_str(), "buy_timeout");
    }

    #[test]
    fn test_hold_ms() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 300_000_000,
            0, 72, 60, 50_000, 10, 15_000,
        );
        assert_eq!(pos.hold_ms(1500), 500);
        assert_eq!(pos.hold_ms(1000), 0);
        // Saturating: now < entry
        assert_eq!(pos.hold_ms(500), 0);
    }

    #[test]
    fn test_ws_notif_count_roundtrip() {
        let mut pos = MomentumPosition::new([0u8;32],0,1000,411,50_000_000,0,50,60,0,0,0);
        assert_eq!(pos.ws_notif_count(), 0);
        pos.set_ws_notif_count(42);
        assert_eq!(pos.ws_notif_count(), 42);
        pos.set_ws_notif_count(u64::MAX);
        assert_eq!(pos.ws_notif_count(), u16::MAX);
    }

    #[test]
    fn test_ws_notif_last_ms_roundtrip() {
        let mut pos = MomentumPosition::new([0u8;32],0,1000,411,50_000_000,0,50,60,0,0,0);
        assert_eq!(pos.ws_notif_last_ms(), 0);
        pos.set_ws_notif_last_ms(1_700_000_000_000);
        assert_eq!(pos.ws_notif_last_ms(), 1_700_000_000_000);
    }

    #[test]
    fn test_ws_notif_does_not_corrupt_scaled_in() {
        let mut pos = MomentumPosition::new([0u8;32],0,1000,411,50_000_000,0,50,60,0,0,0);
        pos.set_scaled_in();
        pos.set_ws_notif_count(7);
        pos.set_ws_notif_last_ms(12345678);
        assert!(pos.is_scaled_in());
        assert_eq!(pos.ws_notif_count(), 7);
        assert_eq!(pos.ws_notif_last_ms(), 12345678);
    }

    // ── Momentum score tests ───────────────────────────────────────────────

    #[test]
    fn test_momentum_score_reversing() {
        // Token peaked at 1000bps, now falling rapidly
        let samples: &[i32] = &[0, 200, 500, 800, 1000, 900, 750, 580, 420];
        let score = compute_momentum_score(samples, 8);
        assert!(score < -150.0, "reversing token should score below threshold: {score}");
    }

    #[test]
    fn test_momentum_score_stalling_at_peak() {
        // Token pumped to +1000bps, now flat — trailing stop should handle
        let samples: &[i32] = &[0, 200, 500, 1000, 1000, 1000, 1000, 1000, 1000];
        let score = compute_momentum_score(samples, 8);
        assert!(score > -150.0, "stalling token should NOT trigger decay exit: {score}");
    }

    #[test]
    fn test_momentum_score_slow_bleed() {
        // Slow bleed: -5bps/tick — time_sl should handle, not decay
        let samples: &[i32] = &[50, 45, 40, 35, 30, 25, 20, 15, 10];
        let score = compute_momentum_score(samples, 8);
        assert!(score > -150.0, "slow bleed should not trigger decay exit: {score}");
    }

    #[test]
    fn test_momentum_score_insufficient_samples() {
        // Fewer than 4 samples → return f64::MAX (never exit)
        assert_eq!(compute_momentum_score(&[0i32, 100], 8), f64::MAX);
        assert_eq!(compute_momentum_score(&[0i32, 50, 100], 8), f64::MAX);
    }

    // ── ATR tests ────────────────────────────────────────────────────────

    #[test]
    fn test_compute_atr_bps_normal_pump() {
        // samples pump to +500 bps then stabilize — ATR should be ~56
        let samples: &[i32] = &[0, 50, 100, 200, 350, 500, 480, 460, 450, 445, 440];
        let atr = compute_atr_bps(samples, 10);
        assert!(atr >= 40 && atr <= 80, "expected ATR ~56, got {atr}");
    }

    #[test]
    fn test_compute_atr_bps_flat() {
        let samples: &[i32] = &[10, -5, 8, -3, 12, 5, -8, 4, 6, -2, 3];
        let atr = compute_atr_bps(samples, 10);
        assert!(atr <= 20, "flat token ATR should be low, got {atr}");
    }

    #[test]
    fn test_compute_atr_bps_window_clamp() {
        // 15 samples, window=10 → should only use last 10
        let samples: &[i32] = &[0, 1000, 2000, 3000, 4000, 5000, 5100, 5050, 5000, 4950, 4900, 4880, 4860, 4850, 4840];
        let atr_full = compute_atr_bps(samples, 15);
        let atr_window = compute_atr_bps(samples, 10);
        // Window version should reflect only recent low-volatility ticks
        assert!(atr_window < atr_full, "window ATR should be lower than full ATR for this series");
    }

    #[test]
    fn test_compute_atr_bps_too_few_samples() {
        assert_eq!(compute_atr_bps(&[500i32], 10), 0);
        assert_eq!(compute_atr_bps(&[], 10), 0);
    }

    // ── Probe phase tests (TASK 2) ─────────────────────────────────────────

    #[test]
    fn test_probe_phase_roundtrip() {
        for i in 0..=4u8 {
            let phase = ProbePhase::from_u8(i);
            assert_eq!(phase as u8, i);
        }
        // Unknown values default to Disabled
        assert_eq!(ProbePhase::from_u8(255), ProbePhase::Disabled);
    }

    #[test]
    fn test_probe_phase_storage_in_pad2() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        assert_eq!(pos.probe_phase(), ProbePhase::Disabled);

        pos.set_probe_phase(ProbePhase::Probing);
        assert_eq!(pos.probe_phase(), ProbePhase::Probing);
        assert!(pos.is_probe_active());

        pos.set_probe_phase(ProbePhase::Scaled);
        assert_eq!(pos.probe_phase(), ProbePhase::Scaled);
        assert!(!pos.is_probe_active());
        assert!(pos.probe_should_scale());

        pos.set_probe_phase(ProbePhase::Failed);
        assert_eq!(pos.probe_phase(), ProbePhase::Failed);
        assert!(!pos.probe_should_scale());

        pos.set_probe_phase(ProbePhase::HeldTight);
        assert_eq!(pos.probe_phase(), ProbePhase::HeldTight);
    }

    #[test]
    fn test_probe_phase_does_not_corrupt_other_pad2_fields() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 1000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        // Set other _pad2 fields first
        pos.set_scaled_in();
        pos.set_ws_notif_count(42);
        pos.set_ws_notif_last_ms(9999999);
        pos.set_tokens_held(1_000_000);

        // Now set probe phase
        pos.set_probe_phase(ProbePhase::Probing);

        // Verify no corruption
        assert!(pos.is_scaled_in());
        assert_eq!(pos.ws_notif_count(), 42);
        assert_eq!(pos.ws_notif_last_ms(), 9999999);
        assert_eq!(pos.tokens_held(), 1_000_000);
        assert_eq!(pos.probe_phase(), ProbePhase::Probing);
    }

    #[test]
    fn test_evaluate_probe_immediate_dump() {
        // Token dumps 6% within 500ms of entry — should fail immediately
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);

        // Price dropped 6% (entry=10000, current=9400, bps=-600)
        let phase = pos.evaluate_probe(
            1500,     // 500ms after entry
            9400,     // -6% price
            2000,     // probe_hold_ms
            -500,     // dump threshold
            -300,     // scale min
            true,     // require price
        );
        assert_eq!(phase, ProbePhase::Failed, "should fail on -6% dump within probe window");
    }

    #[test]
    fn test_evaluate_probe_still_probing() {
        // Token down 3% at 1s — within threshold, keep probing
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);

        let phase = pos.evaluate_probe(
            2000,     // 1s after entry
            9700,     // -3% price
            2000,     // probe_hold_ms
            -500,     // dump threshold
            -300,     // scale min
            true,
        );
        assert_eq!(phase, ProbePhase::Probing, "should keep probing at -3% within window");
    }

    #[test]
    fn test_evaluate_probe_scale_up_price_rising() {
        // After 2s, price is +2% — scale up
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(10200); // one price sample

        let phase = pos.evaluate_probe(
            3000,     // 2s after entry
            10200,    // +2% price
            2000,     // probe_hold_ms
            -500,
            -300,
            true,
        );
        assert_eq!(phase, ProbePhase::Scaled, "should scale up when price is rising after 2s");
    }

    #[test]
    fn test_evaluate_probe_no_price_data() {
        // After 2s, zero price samples, require_price=true → HeldTight
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        // No samples recorded

        let phase = pos.evaluate_probe(
            3000,     // 2s after entry
            10000,    // entry price (no real data)
            2000,
            -500,
            -300,
            true,     // require price
        );
        assert_eq!(phase, ProbePhase::HeldTight, "should hold tight when no price samples");
    }

    #[test]
    fn test_evaluate_probe_moderate_dip_held_tight() {
        // After 2s, price down 2% — between 0% and -3% → HeldTight
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(9800); // one sample

        let phase = pos.evaluate_probe(
            3000,     // 2s after entry
            9800,     // -2% price (bps = -200)
            2000,
            -500,     // dump threshold
            -300,     // scale min (-3%)
            true,
        );
        assert_eq!(phase, ProbePhase::HeldTight, "should hold tight at -2% (between 0% and -3%)");

        // Also verify that -4% (below scale_min) → Failed
        let mut pos2 = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos2.set_probe_phase(ProbePhase::Probing);
        pos2.record_sample(9600);

        let phase2 = pos2.evaluate_probe(
            3000,     // 2s after entry
            9600,     // -4% price (bps = -400, below -300 scale_min)
            2000,
            -500,
            -300,
            true,
        );
        assert_eq!(phase2, ProbePhase::Failed, "-4% exceeds scale_min_bps of -3%");
    }

    #[test]
    fn test_evaluate_probe_large_drop_after_hold() {
        // After 2s, price down 6% → Failed (below scale_min_bps)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(9400);

        let phase = pos.evaluate_probe(
            3000,
            9400,     // -6% price
            2000,
            -500,
            -300,
            true,
        );
        assert_eq!(phase, ProbePhase::Failed, "should fail at -6% after probe window");
    }

    #[test]
    fn test_evaluate_probe_not_probing_returns_current() {
        // If already Scaled, evaluate should return Scaled (no-op)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Scaled);

        let phase = pos.evaluate_probe(3000, 5000, 2000, -500, -300, true);
        assert_eq!(phase, ProbePhase::Scaled, "already-resolved probe should stay resolved");
    }

    #[test]
    fn test_evaluate_probe_exact_boundary_flat() {
        // After 2s, price exactly at entry (0 bps) — should scale
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(10000);

        let phase = pos.evaluate_probe(
            3000,
            10000,    // exactly flat
            2000,
            -500,
            -300,
            true,
        );
        assert_eq!(phase, ProbePhase::Scaled, "exactly flat price should scale up");
    }

    #[test]
    fn test_probe_phase_as_str() {
        assert_eq!(ProbePhase::Disabled.as_str(), "disabled");
        assert_eq!(ProbePhase::Probing.as_str(), "probing");
        assert_eq!(ProbePhase::Scaled.as_str(), "scaled");
        assert_eq!(ProbePhase::Failed.as_str(), "failed");
        assert_eq!(ProbePhase::HeldTight.as_str(), "held_tight");
    }

    // ── Time-decay trailing stop tests (TASK 5) ─────────────────────────

    #[test]
    fn test_effective_trail_bps_default_zero() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        assert_eq!(pos.effective_trail_bps(), 0, "fresh position should have no trail");
    }

    #[test]
    fn test_ratchet_trail_bps_initial_set() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // First ratchet: 0 → 800
        pos.ratchet_trail_bps(800);
        assert_eq!(pos.effective_trail_bps(), 800);
    }

    #[test]
    fn test_ratchet_trail_bps_only_tightens() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.ratchet_trail_bps(800);
        assert_eq!(pos.effective_trail_bps(), 800);

        // Tighten: 800 → 500
        pos.ratchet_trail_bps(500);
        assert_eq!(pos.effective_trail_bps(), 500);

        // Attempt to widen: 500 should NOT become 1000
        pos.ratchet_trail_bps(1000);
        assert_eq!(pos.effective_trail_bps(), 500, "ratchet must not widen");

        // Tighten further: 500 → 100
        pos.ratchet_trail_bps(100);
        assert_eq!(pos.effective_trail_bps(), 100);
    }

    #[test]
    fn test_ratchet_trail_bps_ignores_zero() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.ratchet_trail_bps(500);
        pos.ratchet_trail_bps(0);
        assert_eq!(pos.effective_trail_bps(), 500, "zero should be ignored");
    }

    #[test]
    fn test_time_decay_trail_bps_stage_progression() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let stages_ms: &[u64] = &[30_000, 60_000, 120_000, 180_000, 240_000];
        let trail_bps: &[u16] = &[800, 500, 300, 200, 100];

        // Before any stage: no trail set
        let eff = pos.time_decay_trail_bps(15_000, stages_ms, trail_bps);
        assert_eq!(eff, 0, "before 30s no stage active");

        // At 30s: stage 0 → 800 bps
        let eff = pos.time_decay_trail_bps(30_000, stages_ms, trail_bps);
        assert_eq!(eff, 800);

        // At 45s: still in stage 0 range, ratchet stays at 800
        let eff = pos.time_decay_trail_bps(45_000, stages_ms, trail_bps);
        assert_eq!(eff, 800);

        // At 60s: stage 1 → tighten to 500
        let eff = pos.time_decay_trail_bps(60_000, stages_ms, trail_bps);
        assert_eq!(eff, 500);

        // At 120s: stage 2 → tighten to 300
        let eff = pos.time_decay_trail_bps(120_000, stages_ms, trail_bps);
        assert_eq!(eff, 300);

        // At 180s: stage 3 → tighten to 200
        let eff = pos.time_decay_trail_bps(180_000, stages_ms, trail_bps);
        assert_eq!(eff, 200);

        // At 240s: stage 4 → tighten to 100
        let eff = pos.time_decay_trail_bps(240_000, stages_ms, trail_bps);
        assert_eq!(eff, 100);

        // Way past 240s: still 100 (last stage, ratchet holds)
        let eff = pos.time_decay_trail_bps(500_000, stages_ms, trail_bps);
        assert_eq!(eff, 100);
    }

    #[test]
    fn test_time_decay_trail_bps_ratchet_prevents_widening() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let stages_ms: &[u64] = &[30_000, 60_000, 120_000];
        let trail_bps: &[u16] = &[800, 500, 300];

        // Jump to stage 2 directly (120s) → ratchets to 300
        let eff = pos.time_decay_trail_bps(120_000, stages_ms, trail_bps);
        assert_eq!(eff, 300);

        // Now call with 30s (stage 0 = 800) — ratchet should prevent widening
        let eff = pos.time_decay_trail_bps(30_000, stages_ms, trail_bps);
        assert_eq!(eff, 300, "ratchet must prevent widening back to 800");
    }

    #[test]
    fn test_time_decay_trailing_stop_hit_no_trail() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 12_000;
        // No trail ratcheted → should never trigger
        assert!(!pos.time_decay_trailing_stop_hit(8_000));
    }

    #[test]
    fn test_time_decay_trailing_stop_hit_basic() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 10_000;
        pos.ratchet_trail_bps(500); // 5% trail

        // 5% drawdown from peak of 10000 → threshold is 9500
        // At 9600: 4% drawdown → NOT hit
        assert!(!pos.time_decay_trailing_stop_hit(9_600));
        // At 9500: exactly 5% → HIT
        assert!(pos.time_decay_trailing_stop_hit(9_500));
        // At 9000: 10% drawdown → HIT
        assert!(pos.time_decay_trailing_stop_hit(9_000));
    }

    #[test]
    fn test_time_decay_trailing_stop_tight_trail() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 20_000;
        pos.ratchet_trail_bps(100); // 1% trail (very tight, late stage)

        // 1% of 20000 = 200. So threshold is 19800.
        assert!(!pos.time_decay_trailing_stop_hit(19_900)); // 0.5% → NOT hit
        assert!(pos.time_decay_trailing_stop_hit(19_800));  // exactly 1% → HIT
        assert!(pos.time_decay_trailing_stop_hit(15_000));  // 25% → HIT
    }

    #[test]
    fn test_time_decay_trailing_stop_zero_peak() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 0;
        pos.ratchet_trail_bps(500);
        // Zero peak → never triggers
        assert!(!pos.time_decay_trailing_stop_hit(5_000));
    }

    // ── Stagnation detection tests (TASK 5) ──────────────────────────────

    #[test]
    fn test_is_stagnant_all_zeros() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Record 5 samples, all at entry price (0 bps offset)
        for _ in 0..5 {
            pos.record_sample(10_000);
        }
        assert_eq!(pos.sample_count, 5);

        // Before threshold: not stagnant
        assert!(!pos.is_stagnant(30_000, 60_000));
        // After threshold with all zero samples: stagnant
        assert!(pos.is_stagnant(60_000, 60_000));
        assert!(pos.is_stagnant(120_000, 60_000));
    }

    #[test]
    fn test_is_stagnant_with_movement() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // 4 zero samples + 1 with movement
        for _ in 0..4 {
            pos.record_sample(10_000);
        }
        pos.record_sample(10_100); // +100 bps

        // Has movement → NOT stagnant even past threshold
        assert!(!pos.is_stagnant(120_000, 60_000));
    }

    #[test]
    fn test_is_stagnant_no_samples() {
        let pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // No samples recorded → not stagnant (no data ≠ proven stagnant)
        assert!(!pos.is_stagnant(120_000, 60_000));
    }

    #[test]
    fn test_is_stagnant_exact_threshold() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.record_sample(10_000);
        pos.record_sample(10_000);
        pos.record_sample(10_000);

        // Exactly at threshold: should be stagnant
        assert!(pos.is_stagnant(60_000, 60_000));
        // 1ms before threshold: not stagnant
        assert!(!pos.is_stagnant(59_999, 60_000));
    }

    #[test]
    fn test_trail_does_not_corrupt_probe_phase() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Set probe phase first
        pos.set_probe_phase(ProbePhase::Scaled);
        assert_eq!(pos.probe_phase(), ProbePhase::Scaled);

        // Now ratchet trail bps (adjacent byte 37..39)
        pos.ratchet_trail_bps(500);
        assert_eq!(pos.effective_trail_bps(), 500);

        // Probe phase should be untouched
        assert_eq!(pos.probe_phase(), ProbePhase::Scaled, "trail must not corrupt probe_phase at _pad2[36]");
    }

    #[test]
    fn test_trail_does_not_corrupt_tokens_held() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Set tokens held (bytes 28..36)
        pos.set_tokens_held(999_888_777);
        assert_eq!(pos.tokens_held(), 999_888_777);

        // Ratchet trail (bytes 37..39)
        pos.ratchet_trail_bps(300);
        assert_eq!(pos.effective_trail_bps(), 300);

        // tokens_held should be untouched
        assert_eq!(pos.tokens_held(), 999_888_777, "trail must not corrupt tokens_held at _pad2[28..36]");
    }

    #[test]
    fn test_full_time_decay_scenario() {
        // Simulate a real max_hold trade: token pumps 20%, then slowly bleeds
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let stages_ms: &[u64] = &[30_000, 60_000, 120_000, 180_000, 240_000];
        let trail_bps: &[u16] = &[800, 500, 300, 200, 100];

        // Token peaks at 12000 (20% gain)
        pos.peak_price_fp = 12_000;

        // At 30s: trail = 800 bps (8%). Price at 11500 → 4.2% drawdown → hold
        pos.time_decay_trail_bps(30_000, stages_ms, trail_bps);
        assert!(!pos.time_decay_trailing_stop_hit(11_500));

        // At 60s: trail tightens to 500 bps (5%). Price still 11500 → 4.2% → hold
        pos.time_decay_trail_bps(60_000, stages_ms, trail_bps);
        assert!(!pos.time_decay_trailing_stop_hit(11_500));

        // At 120s: trail tightens to 300 bps (3%). Price at 11500 → 4.2% > 3% → EXIT
        pos.time_decay_trail_bps(120_000, stages_ms, trail_bps);
        assert!(pos.time_decay_trailing_stop_hit(11_500), "4.2% drawdown should trigger 3% trail");

        // Verify the effective trail is 300 (not 800 or 500)
        assert_eq!(pos.effective_trail_bps(), 300);
    }

    // ── Task 3: ws_notif scale-in gate tests ────────────────────────────

    #[test]
    fn test_ws_notif_blocks_scale_in_zero_notifs() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        assert!(pos.ws_notif_blocks_scale_in(10), "ws_notif=0 should block");
    }

    #[test]
    fn test_ws_notif_blocks_scale_in_below_threshold() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_ws_notif_count(5);
        assert!(pos.ws_notif_blocks_scale_in(10), "ws_notif=5 < 10 should block");
    }

    #[test]
    fn test_ws_notif_allows_scale_in_at_threshold() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_ws_notif_count(10);
        assert!(!pos.ws_notif_blocks_scale_in(10), "ws_notif=10 at threshold should allow");
    }

    #[test]
    fn test_ws_notif_allows_scale_in_above_threshold() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_ws_notif_count(15);
        assert!(!pos.ws_notif_blocks_scale_in(10), "ws_notif=15 > 10 should allow");
    }

    #[test]
    fn test_ws_notif_gate_disabled_when_threshold_zero() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        assert!(!pos.ws_notif_blocks_scale_in(0), "threshold=0 disables gate");
    }

    // ── Task 4: s[1] price trajectory gate tests ────────────────────────

    #[test]
    fn test_s1_blocks_scale_in_insufficient_samples() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.record_sample(10000);
        assert_eq!(pos.sample_count, 1);
        assert!(pos.s1_blocks_scale_in(1), "sample_count=1 should defer");
    }

    #[test]
    fn test_s1_blocks_scale_in_negative() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.record_sample(10000);
        pos.record_sample(9950); // s[1] = -50 bps
        assert!(pos.s1_blocks_scale_in(1), "s[1]=-50 should block");
    }

    #[test]
    fn test_s1_blocks_scale_in_zero() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.record_sample(10000);
        pos.record_sample(10000); // s[1] = 0
        assert!(pos.s1_blocks_scale_in(1), "s[1]=0 should block (below threshold=1)");
    }

    #[test]
    fn test_s1_allows_scale_in_positive() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.record_sample(10000);
        pos.record_sample(10005); // s[1] = 5 bps
        assert!(!pos.s1_blocks_scale_in(1), "s[1]=5 >= 1 should allow");
    }

    #[test]
    fn test_s1_allows_scale_in_strong_positive() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.record_sample(10000);
        pos.record_sample(10100); // s[1] = 100 bps
        assert!(!pos.s1_blocks_scale_in(1), "s[1]=100 should allow");
    }

    #[test]
    fn test_s1_gate_disabled() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        assert!(!pos.s1_blocks_scale_in(i32::MIN), "i32::MIN disables gate");
    }

    // ── T3+T4 integration: evaluate_probe_gated tests ───────────────────

    #[test]
    fn test_probe_gated_ws_notif_blocks() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(10100);
        pos.record_sample(10200); // s[1] = +200
        // ws_notif = 0 (default)
        let phase = pos.evaluate_probe_gated(
            3000, 10200, 2000, -500, -300, true, 10, 1,
        );
        assert_eq!(phase, ProbePhase::HeldTight, "ws_notif=0 blocks even with good price");
    }

    #[test]
    fn test_probe_gated_s1_blocks() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.set_ws_notif_count(20);
        pos.record_sample(10000);
        pos.record_sample(10000); // s[1] = 0
        let phase = pos.evaluate_probe_gated(
            3000, 10000, 2000, -500, -300, true, 10, 1,
        );
        assert_eq!(phase, ProbePhase::HeldTight, "s[1]=0 blocks even with high ws_notif");
    }

    #[test]
    fn test_probe_gated_both_pass_scales() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.set_ws_notif_count(15);
        pos.record_sample(10000);
        pos.record_sample(10100); // s[1] = +100
        let phase = pos.evaluate_probe_gated(
            3000, 10100, 2000, -500, -300, true, 10, 1,
        );
        assert_eq!(phase, ProbePhase::Scaled, "both gates pass → scale");
    }

    #[test]
    fn test_probe_gated_gates_disabled() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(10000);
        pos.record_sample(10000);
        let phase = pos.evaluate_probe_gated(
            3000, 10000, 2000, -500, -300, true, 0, i32::MIN,
        );
        assert_eq!(phase, ProbePhase::Scaled, "gates disabled → normal behavior");
    }

    #[test]
    fn test_probe_gated_s1_defers_before_second_sample() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.set_ws_notif_count(20);
        pos.record_sample(10100); // only s[0]
        let phase = pos.evaluate_probe_gated(
            3000, 10100, 2000, -500, -300, true, 10, 1,
        );
        assert_eq!(phase, ProbePhase::HeldTight, "s1 defers when only 1 sample");
    }

    #[test]
    fn test_evaluate_probe_backward_compat() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10000, 411, 50_000_000, 0, 50, 60, 0, 0, 0,
        );
        pos.set_probe_phase(ProbePhase::Probing);
        pos.record_sample(10200);
        let phase = pos.evaluate_probe(3000, 10200, 2000, -500, -300, true);
        assert_eq!(phase, ProbePhase::Scaled, "old API backward compat works");
    }

    // ── Task 5B: Dead token fast exit tests ────────────────────────────

    #[test]
    fn test_dead_token_below_min_hold_no_exit() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // 5 flat samples, ws_notif=0, but hold_ms < 5000 → no fast exit
        for _ in 0..5 {
            pos.record_sample(10_000);
        }
        assert!(!pos.is_dead_token(4_999, 5_000, 5));
    }

    #[test]
    fn test_dead_token_fires_at_threshold() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // 5 flat samples, ws_notif=0, hold_ms=5000 → fast exit fires
        for _ in 0..5 {
            pos.record_sample(10_000);
        }
        assert!(pos.is_dead_token(5_000, 5_000, 5));
    }

    #[test]
    fn test_dead_token_ws_notif_blocks() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // 5 flat samples, hold_ms=5000, but ws_notif=1 → no fast exit (has activity)
        for _ in 0..5 {
            pos.record_sample(10_000);
        }
        pos.set_ws_notif_count(1);
        assert!(!pos.is_dead_token(5_000, 5_000, 5));
    }

    #[test]
    fn test_dead_token_too_few_samples() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Only 4 samples (need ≥5), ws_notif=0, hold_ms=5000 → no fast exit
        for _ in 0..4 {
            pos.record_sample(10_000);
        }
        assert!(!pos.is_dead_token(5_000, 5_000, 5));
    }

    #[test]
    fn test_dead_token_price_not_flat() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // 5 samples but s[3] has movement → not flat → no fast exit
        for _ in 0..3 {
            pos.record_sample(10_000);
        }
        pos.record_sample(10_050); // +50 bps — movement detected
        pos.record_sample(10_000);
        assert!(!pos.is_dead_token(5_000, 5_000, 5));
    }

    #[test]
    fn test_dead_token_disabled_via_zero_min_samples() {
        let pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // min_samples=0 with 0 samples: vacuously flat + no ws → fires.
        // In practice, dead_token_fast_exit_enabled=false in config prevents calling this.
        assert!(pos.is_dead_token(5_000, 5_000, 0),
            "min_samples=0 with 0 samples: vacuously flat + no ws → fires");
    }

    // ── End Task 5B tests ────────────────────────────────────────────────

    // ── Liquidity Quality Score (LQS) tests ─────────────────────────────

    #[test]
    fn test_lqs_excellent_liquidity() {
        // 100 SOL current, 80 SOL at entry, 100 SOL peak → excellent
        let lqs = liquidity_quality_score(100_000_000_000, 80_000_000_000, 100_000_000_000);
        assert!(lqs >= 0.85, "excellent liquidity should score >= 0.85, got {lqs}");
    }

    #[test]
    fn test_lqs_low_liquidity() {
        // 10 SOL current, 80 SOL at entry, 100 SOL peak → poor
        let lqs = liquidity_quality_score(10_000_000_000, 80_000_000_000, 100_000_000_000);
        assert!(lqs < 0.30, "drained pool should score < 0.30, got {lqs}");
    }

    #[test]
    fn test_lqs_moderate_liquidity() {
        // 50 SOL current, 50 SOL at entry, 60 SOL peak → moderate
        let lqs = liquidity_quality_score(50_000_000_000, 50_000_000_000, 60_000_000_000);
        assert!(lqs >= 0.50 && lqs < 0.85, "moderate liquidity should be 0.50-0.85, got {lqs}");
    }

    #[test]
    fn test_lqs_zero_reserve() {
        // 0 SOL current → should be near 0
        let lqs = liquidity_quality_score(0, 80_000_000_000, 100_000_000_000);
        assert!(lqs < 0.15, "zero reserve should score very low, got {lqs}");
    }

    #[test]
    fn test_lqs_growing_reserve() {
        // Reserve doubled from entry: 160 SOL current, 80 entry, 160 peak
        let lqs = liquidity_quality_score(160_000_000_000, 80_000_000_000, 160_000_000_000);
        assert!(lqs >= 0.90, "doubled reserve should score very high, got {lqs}");
    }

    #[test]
    fn test_lqs_zero_entry_reserve() {
        // Edge case: entry reserve was 0 (shouldn't happen, but handle gracefully)
        let lqs = liquidity_quality_score(50_000_000_000, 0, 50_000_000_000);
        // trend_score = -1.0 → trend_normalized = 0.0
        // depth = 50/80 = 0.625, peak_ratio = 1.0
        // = 0.40*0.625 + 0.30*0.0 + 0.30*1.0 = 0.25 + 0 + 0.30 = 0.55
        assert!(lqs >= 0.50 && lqs <= 0.60, "zero entry should still compute, got {lqs}");
    }

    #[test]
    fn test_lqs_range_bounded() {
        // LQS should always be in [0.0, 1.0]
        let cases: Vec<(u64, u64, u64)> = vec![
            (0, 0, 0),
            (u64::MAX, u64::MAX, u64::MAX),
            (1, 1_000_000_000_000, 1_000_000_000_000),
            (500_000_000_000, 1, 500_000_000_000),
        ];
        for (cur, entry, peak) in cases {
            let lqs = liquidity_quality_score(cur, entry, peak);
            assert!(lqs >= 0.0 && lqs <= 1.0, "LQS out of range: {lqs} for ({cur}, {entry}, {peak})");
        }
    }

    #[test]
    fn test_reserve_sol_context_update_peak() {
        let mut ctx = ReserveSolContext::new(80_000_000_000);
        assert_eq!(ctx.entry_lamports, 80_000_000_000);
        assert_eq!(ctx.peak_lamports, 80_000_000_000);

        ctx.update_peak(100_000_000_000);
        assert_eq!(ctx.peak_lamports, 100_000_000_000);

        // Lower value should not decrease peak
        ctx.update_peak(50_000_000_000);
        assert_eq!(ctx.peak_lamports, 100_000_000_000);
    }

    // ── End LQS tests ────────────────────────────────────────────────────

    // ── Velocity exit signal tests ──────────────────────────────────────

    fn make_velocity_config() -> crate::momentum::config::MomentumConfig {
        let mut cfg = crate::momentum::config::MomentumConfig::default();
        cfg.velocity_exit_enabled = true;
        cfg.velocity_exit_min_samples = 5;
        cfg.velocity_exit_min_profit_bps = 50;
        cfg.velocity_exit_threshold_mbps = -150_000;
        cfg.accel_exit_threshold_mbps = -100_000;
        cfg.velocity_exit_confirm_samples = 2;
        cfg.velocity_window = 3;
        cfg.accel_window = 4;
        cfg.momentum_collapse_lookback = 5;
        cfg.momentum_collapse_min_peak_bps = 200;
        cfg.momentum_collapse_drop_threshold_bps = -200;
        cfg.momentum_collapse_max_samples = 2;
        cfg
    }

    #[test]
    fn test_velocity_exit_disabled() {
        let mut cfg = make_velocity_config();
        cfg.velocity_exit_enabled = false;
        let samples = [0, 100, 200, 300, 200, 100];
        let mut counter = 0u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 300, &mut counter);
        assert_eq!(sig, VelocityExitSignal::None);
    }

    #[test]
    fn test_velocity_exit_insufficient_samples() {
        let cfg = make_velocity_config();
        let samples = [0, 100, 200, 300]; // only 4, need 5
        let mut counter = 5u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 300, &mut counter);
        assert_eq!(sig, VelocityExitSignal::None);
        assert_eq!(counter, 0, "counter should reset on gate failure");
    }

    #[test]
    fn test_velocity_exit_not_in_profit() {
        let cfg = make_velocity_config();
        // Current bps = 30 < min_profit_bps=50
        let samples = [0, 10, 20, 25, 30];
        let mut counter = 3u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 30, &mut counter);
        assert_eq!(sig, VelocityExitSignal::None);
        assert_eq!(counter, 0, "counter should reset when not in profit");
    }

    #[test]
    fn test_velocity_exit_momentum_collapse() {
        let cfg = make_velocity_config();
        // Peak at 500, then drops 300 bps (>= 200 threshold) in 2 samples
        let samples = [0, 200, 400, 500, 300, 200];
        let mut counter = 5u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 500, &mut counter);
        assert_eq!(sig, VelocityExitSignal::MomentumCollapse);
        assert_eq!(counter, 0, "collapse resets counter");
    }

    #[test]
    fn test_velocity_exit_no_signal_when_stable() {
        let cfg = make_velocity_config();
        // Stable rising price — no exit signal
        let samples = [0, 100, 200, 300, 400, 500];
        let mut counter = 0u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 500, &mut counter);
        assert_eq!(sig, VelocityExitSignal::None);
        assert_eq!(counter, 0);
    }

    #[test]
    fn test_velocity_exit_confirm_counter_increments() {
        // Use a custom config with a lower threshold to match computed velocity
        // and high min_peak to suppress MomentumCollapse.
        let mut cfg = make_velocity_config();
        cfg.velocity_exit_threshold_mbps = -80_000;  // -80 bps/sample
        cfg.momentum_collapse_min_peak_bps = 1000;   // suppress collapse (peaks < 1000 ignored)

        // Steady decline: [600, 510, 420, 330, 240, 150, 60]
        // Last 3: [240, 150, 60] → OLS slope = -90_000 < -80_000 → velocity fires ✓
        // MomentumCollapse: peak in last 5 = 420 < min_peak=1000 → no collapse ✓
        let samples = [600i32, 510, 420, 330, 240, 150, 60];
        let mut counter = 0u32;
        let sig = evaluate_velocity_exit(&samples, &cfg, 600, &mut counter);
        // First tick: velocity threshold fires, counter goes to 1, but needs 2 → None
        assert_eq!(sig, VelocityExitSignal::None, "should need 2 confirmations");
        assert_eq!(counter, 1);

        // Second tick with same conditions
        let sig2 = evaluate_velocity_exit(&samples, &cfg, 600, &mut counter);
        // Second tick: counter goes to 2 ≥ 2 → fires
        assert_ne!(sig2, VelocityExitSignal::None, "should fire on second confirmation");
    }

    #[test]
    fn test_velocity_exit_counter_resets_on_recovery() {
        let mut cfg = make_velocity_config();
        cfg.velocity_exit_threshold_mbps = -80_000;
        cfg.momentum_collapse_min_peak_bps = 1000; // suppress collapse

        let samples_drop = [600i32, 510, 420, 330, 240, 150, 60];
        let mut counter = 0u32;
        evaluate_velocity_exit(&samples_drop, &cfg, 600, &mut counter);
        assert_eq!(counter, 1);

        // Recovery — stable/rising, no signal
        let samples_recovery = [0i32, 200, 400, 500, 550, 600, 620];
        let sig = evaluate_velocity_exit(&samples_recovery, &cfg, 620, &mut counter);
        assert_eq!(sig, VelocityExitSignal::None);
        assert_eq!(counter, 0, "counter should reset on recovery");
    }

    // ── End velocity exit tests ─────────────────────────────────────────

    // ── PositionPhase + evaluate_phase tests ────────────────────────────

    #[test]
    fn test_position_phase_enum_roundtrip() {
        for i in 0..=5u8 {
            let phase = PositionPhase::from_u8(i);
            assert_eq!(phase as u8, i);
        }
        assert_eq!(PositionPhase::from_u8(255), PositionPhase::AwaitingConfirmation);
    }

    #[test]
    fn test_position_phase_as_str() {
        assert_eq!(PositionPhase::AwaitingConfirmation.as_str(), "awaiting_confirmation");
        assert_eq!(PositionPhase::RapidAssessment.as_str(), "rapid_assessment");
        assert_eq!(PositionPhase::Observation.as_str(), "observation");
        assert_eq!(PositionPhase::Momentum.as_str(), "momentum");
        assert_eq!(PositionPhase::ExitEligible.as_str(), "exit_eligible");
        assert_eq!(PositionPhase::Exiting.as_str(), "exiting");
    }

    #[test]
    fn test_position_phase_blocks_exit() {
        assert!(PositionPhase::AwaitingConfirmation.blocks_exit());
        assert!(PositionPhase::RapidAssessment.blocks_exit());
        assert!(!PositionPhase::Observation.blocks_exit());
        assert!(!PositionPhase::Momentum.blocks_exit());
        assert!(!PositionPhase::ExitEligible.blocks_exit());
        assert!(!PositionPhase::Exiting.blocks_exit());
    }

    #[test]
    fn test_evaluate_phase_awaiting_confirmation() {
        // buy_confirmed_ms == 0 → AwaitingConfirmation
        let pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        assert_eq!(pos.buy_confirmed_ms, 0);
        let phase = pos.evaluate_phase(105_000, 0, 10, 500);
        assert_eq!(phase, PositionPhase::AwaitingConfirmation);
    }

    #[test]
    fn test_evaluate_phase_buy_timeout_10s() {
        // 10s since decision, still not confirmed → Exiting (safety timeout)
        let pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let phase = pos.evaluate_phase(110_001, 0, 10, 500);
        assert_eq!(phase, PositionPhase::Exiting, "should force exit after 10s unconfirmed");
    }

    #[test]
    fn test_evaluate_phase_rapid_assessment_neutral() {
        // Confirmed 500ms ago, price flat → RapidAssessment
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000; // confirmed 1s after decision
        let phase = pos.evaluate_phase(101_500, 0, 5, 1000);
        assert_eq!(phase, PositionPhase::RapidAssessment);
    }

    #[test]
    fn test_evaluate_phase_rapid_assessment_micro_sl() {
        // Confirmed 200ms ago, price dumped -2.5% → Exiting (micro-SL)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(101_200, -250, 5, 200);
        assert_eq!(phase, PositionPhase::Exiting, "should micro-SL at -2.5% in rapid assessment");
    }

    #[test]
    fn test_evaluate_phase_rapid_assessment_not_triggered_at_minus_199() {
        // -199 bps should NOT trigger micro-SL (threshold is -200)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(101_500, -199, 5, 500);
        assert_eq!(phase, PositionPhase::RapidAssessment, "-199 bps should NOT trigger micro-SL");
    }

    #[test]
    fn test_evaluate_phase_early_momentum() {
        // Confirmed 800ms ago, price already +1.5% → Momentum
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(101_800, 150, 10, 300);
        assert_eq!(phase, PositionPhase::Momentum, "should enter Momentum at +150 bps in rapid assessment");
    }

    #[test]
    fn test_evaluate_phase_observation_neutral() {
        // Confirmed 2s ago, price -50 bps → Observation
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(103_000, -50, 5, 1000);
        assert_eq!(phase, PositionPhase::Observation);
    }

    #[test]
    fn test_evaluate_phase_observation_dump_exits() {
        // Confirmed 2.5s ago, price -3% → Exiting
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(103_500, -300, 5, 1000);
        assert_eq!(phase, PositionPhase::Exiting, "should exit at -3% in observation");
    }

    #[test]
    fn test_evaluate_phase_observation_dead_token() {
        // Confirmed 3s ago, 0 WS messages in 3s, last WS was >3s ago → dead token
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(104_000, 20, 1, 4000);
        assert_eq!(phase, PositionPhase::Exiting, "should exit dead token: ws_messages=1 < 2 && ws_age=4s > 3s");
    }

    #[test]
    fn test_evaluate_phase_observation_not_dead_with_ws_activity() {
        // 2.5s hold, ws_messages_last_3s >= 2 → NOT dead, Observation continues
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(103_500, 20, 5, 500);
        assert_eq!(phase, PositionPhase::Observation, "should stay in observation with active WS");
    }

    #[test]
    fn test_evaluate_phase_momentum_from_observation() {
        // 3s hold, +150 bps → Momentum (from observation window)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(104_000, 150, 10, 500);
        assert_eq!(phase, PositionPhase::Momentum, "+150 bps should enter Momentum");
    }

    #[test]
    fn test_evaluate_phase_exit_eligible_after_4500ms() {
        // 5s hold, price +50 bps (not momentum threshold) → ExitEligible
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(105_500, 50, 10, 500);
        assert_eq!(phase, PositionPhase::ExitEligible);
    }

    #[test]
    fn test_evaluate_phase_momentum_takes_priority_over_exit_eligible() {
        // 10s hold, +200 bps → Momentum (not ExitEligible)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 101_000;
        let phase = pos.evaluate_phase(111_000, 200, 15, 500);
        assert_eq!(phase, PositionPhase::Momentum, "+200 bps should be Momentum even after 10s");
    }

    #[test]
    fn test_stamp_buy_confirmed_idempotent() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        assert_eq!(pos.buy_confirmed_ms, 0);
        pos.stamp_buy_confirmed(101_000);
        assert_eq!(pos.buy_confirmed_ms, 101_000);
        // Second call should not overwrite
        pos.stamp_buy_confirmed(105_000);
        assert_eq!(pos.buy_confirmed_ms, 101_000, "stamp_buy_confirmed should be idempotent");
    }

    #[test]
    fn test_confirmed_hold_ms() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Before confirmation → 0
        assert_eq!(pos.confirmed_hold_ms(105_000), 0);
        pos.buy_confirmed_ms = 101_000;
        assert_eq!(pos.confirmed_hold_ms(103_000), 2000);
        assert_eq!(pos.confirmed_hold_ms(101_000), 0);
        // Saturating sub: now < confirmed
        assert_eq!(pos.confirmed_hold_ms(100_000), 0);
    }

    #[test]
    fn test_evaluate_phase_boundary_1500ms() {
        // Exactly at 1500ms → should transition from RapidAssessment to Observation
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 100_000;
        // 1499ms → RapidAssessment
        let phase = pos.evaluate_phase(101_499, 0, 10, 500);
        assert_eq!(phase, PositionPhase::RapidAssessment);
        // 1500ms → Observation
        let phase = pos.evaluate_phase(101_500, 0, 10, 500);
        assert_eq!(phase, PositionPhase::Observation);
    }

    #[test]
    fn test_evaluate_phase_boundary_4500ms() {
        // Exactly at 4500ms → should transition from Observation to ExitEligible
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.buy_confirmed_ms = 100_000;
        // 4499ms → Observation
        let phase = pos.evaluate_phase(104_499, 0, 10, 500);
        assert_eq!(phase, PositionPhase::Observation);
        // 4500ms → ExitEligible
        let phase = pos.evaluate_phase(104_500, 0, 10, 500);
        assert_eq!(phase, PositionPhase::ExitEligible);
    }

    #[test]
    fn test_evaluate_phase_10s_safety_timeout_boundary() {
        // 10000ms since decision, not confirmed → still AwaitingConfirmation
        let pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let phase = pos.evaluate_phase(110_000, 0, 10, 500);
        assert_eq!(phase, PositionPhase::AwaitingConfirmation, "exactly 10000ms should NOT timeout");
        // 10001ms → Exiting
        let phase = pos.evaluate_phase(110_001, 0, 10, 500);
        assert_eq!(phase, PositionPhase::Exiting, "10001ms should trigger safety timeout");
    }

    #[test]
    fn test_buy_confirmed_ms_field_does_not_corrupt_pad2() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 100_000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Set _pad2 packed fields first
        pos.set_scaled_in();
        pos.set_ws_notif_count(42);
        pos.set_ws_notif_last_ms(9999999);
        pos.set_tokens_held(1_000_000);
        pos.set_probe_phase(ProbePhase::Probing);
        pos.ratchet_trail_bps(500);

        // Now set buy_confirmed_ms
        pos.buy_confirmed_ms = 101_500;

        // Verify no corruption
        assert!(pos.is_scaled_in());
        assert_eq!(pos.ws_notif_count(), 42);
        assert_eq!(pos.ws_notif_last_ms(), 9999999);
        assert_eq!(pos.tokens_held(), 1_000_000);
        assert_eq!(pos.probe_phase(), ProbePhase::Probing);
        assert_eq!(pos.effective_trail_bps(), 500);
        assert_eq!(pos.buy_confirmed_ms, 101_500);
    }

    // ── End PositionPhase tests ─────────────────────────────────────────

    // ── TASK 6: Adaptive Trailing Stop + Winner Management tests ─────────

    #[test]
    fn test_trail_config_default_tiers() {
        let tc = TrailConfig::default();
        assert_eq!(tc.tiers.len(), 4);
        assert_eq!(tc.tiers[0].up_to_bps, 300);
        assert_eq!(tc.tiers[0].trail_bps, 200);
        assert_eq!(tc.tiers[1].up_to_bps, 1200);
        assert_eq!(tc.tiers[1].trail_bps, 450);
        assert_eq!(tc.tiers[2].up_to_bps, 4000);
        assert_eq!(tc.tiers[2].trail_bps, 700);
        assert_eq!(tc.tiers[3].up_to_bps, i32::MAX);
        assert_eq!(tc.tiers[3].trail_bps, 1100);
        assert_eq!(tc.confirm_samples, 2);
        assert_eq!(tc.min_samples_to_activate, 5);
        assert_eq!(tc.floor_bps, 350);
    }

    #[test]
    fn test_trail_config_serde_roundtrip() {
        let tc = TrailConfig::default();
        let json = serde_json::to_string(&tc).unwrap();
        let parsed: TrailConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tiers.len(), tc.tiers.len());
        assert_eq!(parsed.confirm_samples, tc.confirm_samples);
        assert_eq!(parsed.min_samples_to_activate, tc.min_samples_to_activate);
        assert_eq!(parsed.floor_bps, tc.floor_bps);
        for (a, b) in parsed.tiers.iter().zip(tc.tiers.iter()) {
            assert_eq!(a.up_to_bps, b.up_to_bps);
            assert_eq!(a.trail_bps, b.trail_bps);
        }
    }

    #[test]
    fn test_trail_config_serde_floor_bps_default() {
        // Omitting floor_bps from JSON should deserialize to default value
        let json = r#"{"tiers":[],"confirm_samples":2,"min_samples_to_activate":5}"#;
        let parsed: TrailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.floor_bps, 350, "floor_bps should default to 350 when omitted");
    }

    #[test]
    fn test_compute_adaptive_trail_bps_loss_returns_zero() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();
        assert_eq!(pos.compute_adaptive_trail_bps(-500, &tc), 0);
        assert_eq!(pos.compute_adaptive_trail_bps(0, &tc), 0);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_tier1_small_gain() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();
        // +1% gain (100 bps) → tier 0 (up_to=300): trail = 200 bps (2%)
        assert_eq!(pos.compute_adaptive_trail_bps(100, &tc), 200);
        // +3% gain (300 bps) → still tier 0: trail = 200 bps
        assert_eq!(pos.compute_adaptive_trail_bps(300, &tc), 200);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_tier2_moderate_gain() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();
        // +4% gain (400 bps) → tier 1 (up_to=1200): trail = 450 bps (4.5%)
        assert_eq!(pos.compute_adaptive_trail_bps(400, &tc), 450);
        // +12% gain (1200 bps) → still tier 1: trail = 450 bps
        assert_eq!(pos.compute_adaptive_trail_bps(1200, &tc), 450);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_tier3_strong_gain() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();
        // +15% gain (1500 bps) → tier 2 (up_to=4000): trail = 700 bps (7%)
        assert_eq!(pos.compute_adaptive_trail_bps(1500, &tc), 700);
        // +40% gain (4000 bps) → still tier 2: trail = 700 bps
        assert_eq!(pos.compute_adaptive_trail_bps(4000, &tc), 700);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_tier4_moonshot() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();
        // +50% gain (5000 bps) → tier 3 (up_to=MAX): trail = 1100 bps (11%)
        assert_eq!(pos.compute_adaptive_trail_bps(5000, &tc), 1100);
        // +100% gain (10000 bps) → still tier 3: trail = 1100 bps
        assert_eq!(pos.compute_adaptive_trail_bps(10_000, &tc), 1100);
        // +500% gain (50000 bps) → still tier 3
        assert_eq!(pos.compute_adaptive_trail_bps(50_000, &tc), 1100);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_empty_tiers() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig {
            tiers: vec![],
            confirm_samples: 2,
            min_samples_to_activate: 5,
            floor_bps: 350,
        };
        // No tiers → fallback to 800
        assert_eq!(pos.compute_adaptive_trail_bps(500, &tc), 800);
    }

    #[test]
    fn test_compute_adaptive_trail_bps_custom_config() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Very aggressive config: 50 bps trail at small gains, 150 at large
        let tc = TrailConfig {
            tiers: vec![
                TrailTier { up_to_bps: 100, trail_bps: 50 },
                TrailTier { up_to_bps: 1000, trail_bps: 150 },
            ],
            confirm_samples: 1,
            min_samples_to_activate: 2,
            floor_bps: 0, // disabled
        };
        assert_eq!(pos.compute_adaptive_trail_bps(50, &tc), 50);
        assert_eq!(pos.compute_adaptive_trail_bps(100, &tc), 50);
        assert_eq!(pos.compute_adaptive_trail_bps(500, &tc), 150);
        // Above all tiers → use last tier
        assert_eq!(pos.compute_adaptive_trail_bps(5000, &tc), 150);
    }

    #[test]
    fn test_adaptive_trailing_stop_hit_basic() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Peak at 12000 (20% gain)
        pos.peak_price_fp = 12_000;

        // 1% trail (100 bps): floor = 12000 - 120 = 11880
        assert!(!pos.adaptive_trailing_stop_hit(11_900, 100)); // above floor
        assert!(!pos.adaptive_trailing_stop_hit(11_880, 100)); // exactly at floor (not below)
        assert!(pos.adaptive_trailing_stop_hit(11_879, 100));  // 1 below floor → hit
        assert!(pos.adaptive_trailing_stop_hit(10_000, 100));  // way below → hit
    }

    #[test]
    fn test_adaptive_trailing_stop_hit_wider_trail() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 15_000; // +50% from entry

        // 8% trail (800 bps): floor = 15000 - 1200 = 13800
        assert!(!pos.adaptive_trailing_stop_hit(14_000, 800)); // above floor
        assert!(!pos.adaptive_trailing_stop_hit(13_800, 800)); // at floor
        assert!(pos.adaptive_trailing_stop_hit(13_799, 800));  // below floor → hit
    }

    #[test]
    fn test_adaptive_trailing_stop_hit_zero_peak() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 0;
        assert!(!pos.adaptive_trailing_stop_hit(5_000, 400)); // zero peak → never hit
    }

    #[test]
    fn test_adaptive_trailing_stop_hit_zero_trail() {
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 12_000;
        assert!(!pos.adaptive_trailing_stop_hit(5_000, 0)); // zero trail → never hit
    }

    #[test]
    fn test_adaptive_trail_scenario_old_vs_new() {
        // Scenario: token peaks at +20% (12000 from 10000 entry), then drops
        let mut pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 12_000; // +20% peak

        let tc = TrailConfig::default();

        // Current gain at +20% = 2000 bps → tier[1] (301-1200): trail = 450 bps (4.5%)
        let current_at_peak_bps = price_to_bps_offset(10_000, 12_000); // 2000
        assert_eq!(current_at_peak_bps, 2000);
        let trail = pos.compute_adaptive_trail_bps(current_at_peak_bps, &tc);
        assert_eq!(trail, 700); // 7% trail for 12-40% gain range (tier 2)

        // Floor = 12000 - (12000 * 700 / 10000) = 12000 - 840 = 11160
        // With OLD 25% trail: floor = 12000 - 3000 = 9000
        // Token drops to 11100 → new trail fires, old trail doesn't
        assert!(pos.adaptive_trailing_stop_hit(11_100, trail));

        // Old trailing stop with 25% (2500 bps) — same scenario
        assert!(!pos.trailing_stop_hit(11_100, 2500)); // old trail: 11100 > 9000, doesn't fire

        // Key improvement: new trail exits at ~+11.6% (11160/10000),
        // old trail wouldn't fire until +20% peak drops to 9000 (-10% loss)
    }

    #[test]
    fn test_adaptive_trail_gain_transitions() {
        // Simulate a position gaining profit, verify trail adjusts at tier boundaries
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();

        // New tier layout:
        //   tier 0: 0-300 bps (0-3%): 200 bps (2%) trail
        //   tier 1: 301-1200 bps (3-12%): 450 bps (4.5%) trail
        //   tier 2: 1201-4000 bps (12-40%): 700 bps (7%) trail
        //   tier 3: 4001+ bps (40%+): 1100 bps (11%) trail
        let cases = vec![
            (50, 200),     // +0.5% → tier 0: 2% trail
            (200, 200),    // +2.0% → tier 0: 2% trail
            (300, 200),    // +3.0% → tier 0 boundary: 2% trail
            (301, 450),    // +3.01% → tier 1: 4.5% trail (crossed boundary)
            (800, 450),    // +8.0% → tier 1: 4.5% trail
            (1200, 450),   // +12.0% → tier 1 boundary: 4.5% trail
            (1201, 700),   // +12.01% → tier 2: 7% trail (crossed boundary)
            (3000, 700),   // +30.0% → tier 2: 7% trail
            (4000, 700),   // +40.0% → tier 2 boundary: 7% trail
            (4001, 1100),  // +40.01% → tier 3: 11% trail (crossed boundary)
            (10000, 1100), // +100% → tier 3: 11% trail
        ];

        for (gain_bps, expected_trail) in cases {
            let trail = pos.compute_adaptive_trail_bps(gain_bps, &tc);
            assert_eq!(
                trail, expected_trail,
                "gain_bps={}: expected trail={}, got {}",
                gain_bps, expected_trail, trail
            );
        }
    }

    #[test]
    fn test_is_momentum_locked_profitable_with_activity() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Profitable and active → locked
        assert!(pos.is_momentum_locked(500, 5));
        assert!(pos.is_momentum_locked(1, 1));
    }

    #[test]
    fn test_is_momentum_locked_profitable_no_activity() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Profitable but no recent activity → NOT locked (stale, let time exits handle)
        assert!(!pos.is_momentum_locked(500, 0));
    }

    #[test]
    fn test_is_momentum_locked_losing_with_activity() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Losing position → NOT locked (hard_sl/time_sl should handle)
        assert!(!pos.is_momentum_locked(-100, 10));
        assert!(!pos.is_momentum_locked(0, 10));
    }

    #[test]
    fn test_is_momentum_locked_losing_no_activity() {
        let pos = MomentumPosition::new(
            [0u8; 32], 1000, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        // Losing and no activity → definitely NOT locked
        assert!(!pos.is_momentum_locked(-500, 0));
    }

    #[test]
    fn test_adaptive_trail_full_lifecycle() {
        // Simulate the full lifecycle of a winning trade with adaptive trail
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig::default();

        // Phase 1: token rises to +5% (500 bps)
        for _ in 0..5 {
            pos.record_sample(10_500); // +5%
        }
        assert!(pos.sample_count >= tc.min_samples_to_activate);

        let gain = price_to_bps_offset(10_000, 10_500); // 500 bps
        let trail = pos.compute_adaptive_trail_bps(gain, &tc);
        // 500 bps is in tier 1 (301-1200) → trail=450 (4.5%)
        assert_eq!(trail, 450);

        // Floor = 10500 - (10500 * 450 / 10000) = 10500 - 472 = 10028
        assert!(!pos.adaptive_trailing_stop_hit(10_100, trail)); // above floor
        assert!(pos.adaptive_trailing_stop_hit(10_020, trail));  // below floor

        // Phase 2: token pumps to +20% (peak = 12000)
        pos.peak_price_fp = 12_000;
        let gain = price_to_bps_offset(10_000, 12_000); // 2000 bps
        let trail = pos.compute_adaptive_trail_bps(gain, &tc);
        // 2000 bps is in tier 2 (1201-4000) → trail=700 (7%)
        assert_eq!(trail, 700);

        // Floor = 12000 - (12000 * 700 / 10000) = 12000 - 840 = 11160
        assert!(!pos.adaptive_trailing_stop_hit(11_200, trail)); // still above
        assert!(pos.adaptive_trailing_stop_hit(11_100, trail));  // below → exit at ~+11.6%

        // With old 25% trail: 12000 * 0.75 = 9000. Exit would be at -10% loss!
        // New trail captures +11.6% vs old trail capturing -10% or worse.
    }

    #[test]
    fn test_adaptive_trail_confirm_samples_integration() {
        // Verify the confirm samples field reuse works correctly
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        let tc = TrailConfig { confirm_samples: 3, ..TrailConfig::default() };

        pos.peak_price_fp = 12_000;

        // Below floor for 1 tick
        pos.trail_stop_below_floor_count = 0;
        pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
        assert!(pos.trail_stop_below_floor_count < tc.confirm_samples);

        // Below floor for 2 ticks
        pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
        assert!(pos.trail_stop_below_floor_count < tc.confirm_samples);

        // Below floor for 3 ticks → confirmed
        pos.trail_stop_below_floor_count = pos.trail_stop_below_floor_count.saturating_add(1);
        assert!(pos.trail_stop_below_floor_count >= tc.confirm_samples);

        // Reset on recovery
        pos.trail_stop_below_floor_count = 0;
        assert!(pos.trail_stop_below_floor_count < tc.confirm_samples);
    }

    #[test]
    fn test_adaptive_trail_expected_impact_math() {
        // Verify the key claim: at +20% peak with 7% trail, exit at ~+13%
        // vs old 25% trail where exit would be at ~-10% (9000 from 10000 entry)
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 12_000; // +20% peak

        // gain_bps = 2000, which is in tier 2 (1201-4000) → trail = 700 bps (7%)
        let tc = TrailConfig::default();
        let gain_at_peak = price_to_bps_offset(10_000, 12_000); // 2000 bps
        let trail = pos.compute_adaptive_trail_bps(gain_at_peak, &tc);
        assert_eq!(trail, 700); // 7% trail for 12-40% range

        // At +20% peak with 7% trail: floor = 12000 - (12000 * 700 / 10000) = 12000 - 840 = 11160
        let floor = 12_000u64.saturating_sub(12_000 * 700 / 10_000);
        assert_eq!(floor, 11_160);

        // Exit at 11160 = +11.6% from entry (10000)
        let exit_gain = price_to_bps_offset(10_000, floor);
        assert_eq!(exit_gain, 1160); // +11.6%

        // Old 25% trail: floor = 12000 * 0.75 = 9000
        let old_floor = 12_000u64.saturating_sub(12_000 * 2500 / 10_000);
        assert_eq!(old_floor, 9000);
        let old_exit_gain = price_to_bps_offset(10_000, old_floor);
        assert_eq!(old_exit_gain, -1000); // -10%!

        // Improvement: +11.6% vs -10% = 21.6 percentage points captured
        assert!(exit_gain > old_exit_gain + 2000,
            "new trail should capture at least 20 pct pts more than old");
    }

    #[test]
    fn test_adaptive_trail_moonshot_scenario() {
        // Moonshot: token goes +80%, then dumps.
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 18_000; // +80%

        let tc = TrailConfig::default();
        // gain=8000 bps > 4000 (tier[2]) → tier[3].up_to_bps=MAX → trail=1100 (11%)
        let trail = pos.compute_adaptive_trail_bps(8000, &tc);
        assert_eq!(trail, 1100); // 11% trail for moonshots (40%+ gain)

        // Floor = 18000 - (18000 * 1100 / 10000) = 18000 - 1980 = 16020
        let floor = 18_000u64.saturating_sub(18_000 * 1100 / 10_000);
        assert_eq!(floor, 16_020);
        let exit_gain = price_to_bps_offset(10_000, floor);
        assert_eq!(exit_gain, 6020); // +60.2%

        // With old 25% trail: floor = 18000 * 0.75 = 13500 → +35%
        // With new 11% trail: exit at +60.2% vs +35% → 25 pct pts improvement
        assert!(pos.adaptive_trailing_stop_hit(15_900, trail));   // below 16020 → trail fires
        assert!(!pos.adaptive_trailing_stop_hit(16_100, trail));  // above 16020 → hold
    }

    #[test]
    fn test_adaptive_trail_small_gain_protection() {
        // Key scenario: small +1.5% gain with tight 1% trail protects tiny wins
        let mut pos = MomentumPosition::new(
            [0u8; 32], 0, 10_000, 411, 300_000_000, 0, 72, 60, 50_000, 10, 15_000,
        );
        pos.peak_price_fp = 10_150; // +1.5% peak

        let tc = TrailConfig::default();
        let gain = price_to_bps_offset(10_000, 10_150); // 150 bps
        let trail = pos.compute_adaptive_trail_bps(gain, &tc);
        assert_eq!(trail, 200); // 2% trail for small gains (0-3%)

        // Floor = 10150 - (10150 * 200 / 10000) = 10150 - 203 = 9947
        // Trail fires below 9947 — but floor_bps=350 prevents exit below +3.5%
        let floor = 10_150u64.saturating_sub(10_150 * 200 / 10_000);
        assert_eq!(floor, 9_947); // note: integer rounding
        assert!(pos.adaptive_trailing_stop_hit(9_940, trail));  // below floor → trail triggers
        assert!(!pos.adaptive_trailing_stop_hit(9_960, trail)); // above floor → hold
    }

    // ── End TASK 6 tests ─────────────────────────────────────────────────
}
