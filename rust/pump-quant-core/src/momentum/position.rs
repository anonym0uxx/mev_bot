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
    /// Called every 10 seconds from the tick loop. Updates peak_price_fp
    /// for trailing stop tracking.
    #[inline(always)]
    pub fn record_sample(&mut self, current_price_fp: u64) {
        if self.sample_count as usize >= PRICE_SAMPLES {
            return;
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
}

// ── Exit reason enum ─────────────────────────────────────────────────────────

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

    /// Push a new pending entry. Returns false if ring is full (all 64 active).
    ///
    /// If the slot at head is inactive (already drained), it will be overwritten.
    /// If the ring has wrapped and the slot is still active, returns false.
    pub fn push(&mut self, entry: PendingEntry) -> bool {
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
}
