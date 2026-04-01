//! Cache-aligned position struct and pending entry ring buffer.
//!
//! ## Layout: MomentumPosition — exactly 256 bytes, 64-byte aligned
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!   0      32   mint: [u8; 32]
//!  32       8   entry_ts_ms: u64
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
//! 199       1   _pad: u8
//! 200       4   grad_speed_s: u32
//! 204       4   grad_volume_sol_x100: u32
//! 208       4   pre_grad_buys_5s: u32
//! 212       4   entry_delay_ms: u32
//! 216       1   first_price_recorded: bool
//! 217      39   _pad2: [u8; 39]     — pad to 256
//! ------  ----
//! TOTAL:  256
//! ```
//!
//! ## Performance
//!
//! - `#[repr(C, align(64))]` ensures cache-line alignment
//! - All hot-path methods are `#[inline(always)]`
//! - No heap allocation in PendingEntryRing (64 fixed slots)
//! - Integer-only price tracking (fixed-point bps offsets)

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
    /// Padding for alignment to next u32 field.
    pub _pad: u8,

    // ── Grad context (16 bytes) ──────────────────────────
    /// Seconds from token creation to graduation.
    pub grad_speed_s: u32,
    /// Total bonding curve volume in centisol (SOL × 100).
    pub grad_volume_sol_x100: u32,
    /// Buy transactions in last 5 seconds of bonding curve.
    pub pre_grad_buys_5s: u32,
    /// Entry delay from graduation in ms.
    pub entry_delay_ms: u32,

    // ── First-tick tracking + padding to 256 bytes ────────
    /// Set to true after the first price sample has been recorded.
    /// Ensures we always capture a sample on the first tick with live price data.
    pub first_price_recorded: bool,
    pub _pad2: [u8; 39],
}

// Compile-time size and alignment assertions.
const _: () = assert!(std::mem::size_of::<MomentumPosition>() <= 256);
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
            _pad: 0,
            grad_speed_s,
            grad_volume_sol_x100,
            pre_grad_buys_5s,
            entry_delay_ms,
            first_price_recorded: false,
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

    /// Hold duration in ms since entry.
    #[inline(always)]
    pub fn hold_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.entry_ts_ms)
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
}

// ── Exit reason enum ─────────────────────────────────────────────────────────

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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_size_256_bytes() {
        let size = std::mem::size_of::<MomentumPosition>();
        assert!(
            size <= 256,
            "MomentumPosition is {} bytes, must be <= 256",
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
        for i in 0..=8u8 {
            let reason = MomentumExitReason::from_u8(i);
            assert_eq!(reason as u8, i);
        }
        // Unknown values default to Open
        assert_eq!(MomentumExitReason::from_u8(255), MomentumExitReason::Open);
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
}
