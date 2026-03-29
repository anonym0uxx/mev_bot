//! Risk management layer for pump-quant.
//!
//! All checks on the hot path (`allows_entry`) are pure integer comparisons —
//! no allocations, no syscalls, no locks. Target: <5 ns per call.

// ── Constants ────────────────────────────────────────────────────────────────

const MS_PER_DAY: u64 = 86_400_000;

// ── RiskConfig ───────────────────────────────────────────────────────────────

/// Configuration loaded from `canary.json`.  Immutable after construction.
#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub daily_loss_limit_lamports: i64,
    pub consecutive_loss_limit: u8,
    pub pause_duration_ms: u64,
    pub daily_trade_limit: u32,
    pub loss_cooldown_ms: u64,
    pub max_concurrent_scalp: u8,
    pub max_concurrent_ride: u8,
    pub max_concurrent_total: u8,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            daily_loss_limit_lamports: -1_500_000_000, // -1.5 SOL
            consecutive_loss_limit: 5,
            pause_duration_ms: 300_000,  // 5 min
            daily_trade_limit: 60,
            loss_cooldown_ms: 5_000,
            max_concurrent_scalp: 5,
            max_concurrent_ride: 3,
            max_concurrent_total: 8,
        }
    }
}

// ── RiskManager ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct RiskManager {
    // ── daily P&L ────────────────────────────────────────────────────────
    pub daily_pnl_lamports: i64,
    pub daily_loss_limit: i64,

    // ── consecutive-loss circuit breaker ──────────────────────────────────
    pub consecutive_losses: u8,
    pub consecutive_loss_limit: u8,

    // ── pause state ──────────────────────────────────────────────────────
    pub paused_until_ms: u64,
    pub pause_duration_ms: u64,

    // ── daily trade cap ──────────────────────────────────────────────────
    pub daily_trade_count: u32,
    pub daily_trade_limit: u32,

    // ── loss cooldown ────────────────────────────────────────────────────
    pub last_loss_ms: u64,
    pub loss_cooldown_ms: u64,

    // ── concurrent position limits ───────────────────────────────────────
    pub scalp_count: u8,
    pub ride_count: u8,
    pub max_scalp: u8,
    pub max_ride: u8,
    pub max_total: u8,

    // ── daily reset tracking ─────────────────────────────────────────────
    pub last_reset_day: u32,
}

impl RiskManager {
    // ── Constructor ──────────────────────────────────────────────────────

    pub fn new(config: &RiskConfig) -> Self {
        Self {
            daily_pnl_lamports: 0,
            daily_loss_limit: config.daily_loss_limit_lamports,
            consecutive_losses: 0,
            consecutive_loss_limit: config.consecutive_loss_limit,
            paused_until_ms: 0,
            pause_duration_ms: config.pause_duration_ms,
            daily_trade_count: 0,
            daily_trade_limit: config.daily_trade_limit,
            last_loss_ms: 0,
            loss_cooldown_ms: config.loss_cooldown_ms,
            scalp_count: 0,
            ride_count: 0,
            max_scalp: config.max_concurrent_scalp,
            max_ride: config.max_concurrent_ride,
            max_total: config.max_concurrent_total,
            last_reset_day: 0,
        }
    }

    // ── Hot-path gate (< 5 ns) ──────────────────────────────────────────

    /// Returns `true` if the engine is allowed to open a new position **right
    /// now**.  This is the single gate checked before the entry engine runs.
    ///
    /// All branches are simple integer comparisons — no heap work, no I/O.
    #[inline(always)]
    pub fn allows_entry(&self, now_ms: u64, is_ride: bool) -> bool {
        // 1. Paused (consecutive-loss breaker or manual pause)
        if self.paused_until_ms > now_ms {
            return false;
        }

        // 2. Daily loss limit breached
        if self.daily_pnl_lamports <= self.daily_loss_limit {
            return false;
        }

        // 3. Daily trade cap
        if self.daily_trade_count >= self.daily_trade_limit {
            return false;
        }

        // 4. Per-strategy concurrent limit
        if is_ride {
            if self.ride_count >= self.max_ride {
                return false;
            }
        } else if self.scalp_count >= self.max_scalp {
            return false;
        }

        // 5. Total concurrent limit
        if (self.scalp_count as u16 + self.ride_count as u16) >= self.max_total as u16 {
            return false;
        }

        // 6. Loss cooldown — suppress rapid re-entry after a loss
        if self.last_loss_ms > 0 && now_ms.saturating_sub(self.last_loss_ms) < self.loss_cooldown_ms
        {
            return false;
        }

        true
    }

    // ── Lifecycle hooks ─────────────────────────────────────────────────

    /// Called immediately after a position is opened.
    #[inline]
    pub fn on_trade_opened(&mut self, is_ride: bool) {
        if is_ride {
            self.ride_count = self.ride_count.saturating_add(1);
        } else {
            self.scalp_count = self.scalp_count.saturating_add(1);
        }
        self.daily_trade_count = self.daily_trade_count.saturating_add(1);
    }

    /// Called when a position is closed (win or loss).
    #[inline]
    pub fn on_trade_closed(&mut self, pnl_lamports: i64, is_ride: bool, now_ms: u64) {
        // Update position counts
        if is_ride {
            self.ride_count = self.ride_count.saturating_sub(1);
        } else {
            self.scalp_count = self.scalp_count.saturating_sub(1);
        }

        // Accumulate daily P&L
        self.daily_pnl_lamports = self.daily_pnl_lamports.saturating_add(pnl_lamports);

        // Consecutive loss tracking
        if pnl_lamports < 0 {
            self.consecutive_losses = self.consecutive_losses.saturating_add(1);
            self.last_loss_ms = now_ms;

            // Circuit breaker
            if self.consecutive_losses >= self.consecutive_loss_limit {
                self.paused_until_ms = now_ms.saturating_add(self.pause_duration_ms);
            }
        } else {
            // Any non-negative close resets the streak
            self.consecutive_losses = 0;
        }
    }

    // ── Daily reset ─────────────────────────────────────────────────────

    /// Reset daily counters when the UTC day rolls over.
    /// Should be called at the top of each tick / entry check.
    #[inline]
    pub fn check_daily_reset(&mut self, now_ms: u64) {
        let current_day = (now_ms / MS_PER_DAY) as u32;
        if current_day != self.last_reset_day {
            self.daily_pnl_lamports = 0;
            self.daily_trade_count = 0;
            self.consecutive_losses = 0;
            self.last_loss_ms = 0;
            self.last_reset_day = current_day;
            // NOTE: paused_until_ms is intentionally NOT reset — a pause
            // carries across midnight if the duration hasn't elapsed.
        }
    }

    // ── Pause helpers ───────────────────────────────────────────────────

    /// Check whether the manager is currently paused (does NOT use wall-clock;
    /// caller must pass `now_ms` to `allows_entry` for a live check).
    /// This is a snapshot check useful for dashboards / status endpoints.
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.paused_until_ms > 0
    }

    /// Manually pause the risk manager (e.g., triggered from an API call).
    #[inline]
    pub fn force_pause(&mut self, duration_ms: u64, now_ms: u64) {
        self.paused_until_ms = now_ms.saturating_add(duration_ms);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: a config with tight limits for fast tests.
    fn test_config() -> RiskConfig {
        RiskConfig {
            daily_loss_limit_lamports: -1_000_000_000, // -1 SOL
            consecutive_loss_limit: 3,
            pause_duration_ms: 60_000, // 1 min
            daily_trade_limit: 10,
            loss_cooldown_ms: 5_000,
            max_concurrent_scalp: 2,
            max_concurrent_ride: 1,
            max_concurrent_total: 3,
        }
    }

    fn day_start_ms(day: u32) -> u64 {
        day as u64 * MS_PER_DAY
    }

    // ── new() ────────────────────────────────────────────────────────────

    #[test]
    fn new_initializes_cleanly() {
        let rm = RiskManager::new(&test_config());
        assert_eq!(rm.daily_pnl_lamports, 0);
        assert_eq!(rm.consecutive_losses, 0);
        assert_eq!(rm.scalp_count, 0);
        assert_eq!(rm.ride_count, 0);
        assert_eq!(rm.daily_trade_count, 0);
        assert_eq!(rm.paused_until_ms, 0);
        assert!(rm.allows_entry(1_000, false));
    }

    // ── Daily reset ──────────────────────────────────────────────────────

    #[test]
    fn daily_reset_clears_counters() {
        let mut rm = RiskManager::new(&test_config());
        let day0 = day_start_ms(100) + 1_000;
        rm.check_daily_reset(day0);

        // Accumulate some state during day 100
        rm.on_trade_opened(false);
        rm.on_trade_closed(-500_000_000, false, day0 + 1_000);
        assert_eq!(rm.daily_pnl_lamports, -500_000_000);
        assert_eq!(rm.daily_trade_count, 1);
        assert_eq!(rm.consecutive_losses, 1);
        assert!(rm.last_loss_ms > 0);

        // Roll to day 101
        let day1 = day_start_ms(101) + 1_000;
        rm.check_daily_reset(day1);
        assert_eq!(rm.daily_pnl_lamports, 0);
        assert_eq!(rm.daily_trade_count, 0);
        assert_eq!(rm.consecutive_losses, 0);
        assert_eq!(rm.last_loss_ms, 0);
        assert_eq!(rm.last_reset_day, 101);
    }

    #[test]
    fn daily_reset_preserves_pause_across_midnight() {
        let mut rm = RiskManager::new(&test_config());
        let day0 = day_start_ms(100) + 80_000_000; // late in the day
        rm.check_daily_reset(day0);

        // Force a long pause that bleeds into next day
        rm.force_pause(20_000_000, day0); // ~5.5 hours
        let expected_pause = day0 + 20_000_000;

        // Roll to day 101
        let day1 = day_start_ms(101) + 1_000;
        rm.check_daily_reset(day1);
        assert_eq!(rm.paused_until_ms, expected_pause, "pause must survive midnight");
        assert!(!rm.allows_entry(day1, false), "still paused after midnight");
    }

    // ── Consecutive loss pause ───────────────────────────────────────────

    #[test]
    fn consecutive_losses_trigger_pause() {
        let cfg = test_config(); // limit = 3
        let mut rm = RiskManager::new(&cfg);
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t);
        assert_eq!(rm.consecutive_losses, 1);

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t + 10_000);
        assert_eq!(rm.consecutive_losses, 2);

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t + 20_000);
        assert_eq!(rm.consecutive_losses, 3);

        // Should now be paused
        assert!(rm.is_paused());
        assert_eq!(rm.paused_until_ms, t + 20_000 + cfg.pause_duration_ms);
        assert!(!rm.allows_entry(t + 20_001, false));
        // After pause elapses
        assert!(rm.allows_entry(t + 20_000 + cfg.pause_duration_ms + 1, false));
    }

    #[test]
    fn win_resets_consecutive_losses() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t);
        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t + 10_000);
        assert_eq!(rm.consecutive_losses, 2);

        // A win resets
        rm.on_trade_opened(false);
        rm.on_trade_closed(500, false, t + 20_000);
        assert_eq!(rm.consecutive_losses, 0);
    }

    #[test]
    fn breakeven_resets_consecutive_losses() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t);
        assert_eq!(rm.consecutive_losses, 1);

        // pnl == 0 is non-negative → resets streak
        rm.on_trade_opened(false);
        rm.on_trade_closed(0, false, t + 10_000);
        assert_eq!(rm.consecutive_losses, 0);
    }

    // ── Concurrent position limits ───────────────────────────────────────

    #[test]
    fn scalp_concurrent_limit() {
        let mut rm = RiskManager::new(&test_config()); // max_scalp=2
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_opened(false);
        assert_eq!(rm.scalp_count, 2);
        assert!(
            !rm.allows_entry(t, false),
            "should block 3rd scalp"
        );
        // ride still OK (if total allows)
        assert!(rm.allows_entry(t, true), "ride should still be allowed");
    }

    #[test]
    fn ride_concurrent_limit() {
        let mut rm = RiskManager::new(&test_config()); // max_ride=1
        let t = 1_000_000u64;

        rm.on_trade_opened(true);
        assert_eq!(rm.ride_count, 1);
        assert!(
            !rm.allows_entry(t, true),
            "should block 2nd ride"
        );
        // scalp still OK
        assert!(rm.allows_entry(t, false));
    }

    #[test]
    fn total_concurrent_limit() {
        let mut rm = RiskManager::new(&test_config()); // max_total=3
        let t = 1_000_000u64;

        rm.on_trade_opened(false); // scalp 1
        rm.on_trade_opened(false); // scalp 2
        rm.on_trade_opened(true);  // ride 1
        assert_eq!(rm.scalp_count, 2);
        assert_eq!(rm.ride_count, 1);

        // Total = 3 = max_total → blocked for both
        assert!(!rm.allows_entry(t, false));
        assert!(!rm.allows_entry(t, true));
    }

    #[test]
    fn closing_trade_frees_slot() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_opened(false);
        assert!(!rm.allows_entry(t, false));

        // Close one (win, so no cooldown concern)
        rm.on_trade_closed(100, false, t);
        assert_eq!(rm.scalp_count, 1);
        assert!(rm.allows_entry(t + 1, false));
    }

    // ── Daily trade limit ────────────────────────────────────────────────

    #[test]
    fn daily_trade_limit_blocks_entry() {
        let cfg = RiskConfig {
            daily_trade_limit: 3,
            ..test_config()
        };
        let mut rm = RiskManager::new(&cfg);
        let t = 1_000_000u64;

        for i in 0..3 {
            rm.on_trade_opened(false);
            rm.on_trade_closed(100, false, t + i * 1_000);
        }
        assert_eq!(rm.daily_trade_count, 3);
        assert!(
            !rm.allows_entry(t + 10_000, false),
            "should block after daily limit"
        );
    }

    // ── Daily loss limit ─────────────────────────────────────────────────

    #[test]
    fn daily_loss_limit_blocks_entry() {
        let mut rm = RiskManager::new(&test_config()); // limit = -1 SOL
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(-1_000_000_001, false, t); // just past the limit
        assert!(rm.daily_pnl_lamports <= rm.daily_loss_limit);
        // Need to be past cooldown
        let after_cooldown = t + rm.loss_cooldown_ms + 1;
        // Still blocked by daily loss even after cooldown + pause (if any)
        // Pause might be set from consecutive losses, but daily P&L gate is independent
        // Here consecutive_losses=1 < limit=3, so no pause, but daily loss blocks
        assert!(
            !rm.allows_entry(after_cooldown + rm.pause_duration_ms + 1, false),
            "daily loss limit should block"
        );
    }

    // ── Loss cooldown ────────────────────────────────────────────────────

    #[test]
    fn loss_cooldown_blocks_immediate_reentry() {
        let mut rm = RiskManager::new(&test_config()); // cooldown = 5_000 ms
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(-100, false, t);

        // Immediately after loss
        assert!(
            !rm.allows_entry(t + 1, false),
            "should be in cooldown"
        );
        // Still in cooldown
        assert!(
            !rm.allows_entry(t + 4_999, false),
            "still in cooldown"
        );
        // Cooldown elapsed
        assert!(
            rm.allows_entry(t + 5_001, false),
            "cooldown should be over"
        );
    }

    #[test]
    fn win_does_not_trigger_cooldown() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;

        rm.on_trade_opened(false);
        rm.on_trade_closed(500, false, t);
        assert_eq!(rm.last_loss_ms, 0, "win should not set last_loss_ms");
        assert!(rm.allows_entry(t + 1, false));
    }

    // ── Force pause ──────────────────────────────────────────────────────

    #[test]
    fn force_pause_blocks_and_expires() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;

        rm.force_pause(10_000, t);
        assert!(rm.is_paused());
        assert!(!rm.allows_entry(t + 1, false));
        assert!(!rm.allows_entry(t + 9_999, true));
        assert!(rm.allows_entry(t + 10_001, false));
    }

    // ── is_paused snapshot ───────────────────────────────────────────────

    #[test]
    fn is_paused_returns_true_when_set() {
        let mut rm = RiskManager::new(&test_config());
        assert!(!rm.is_paused());
        rm.paused_until_ms = 1;
        assert!(rm.is_paused());
    }

    // ── Default config ───────────────────────────────────────────────────

    #[test]
    fn default_config_sane() {
        let cfg = RiskConfig::default();
        assert_eq!(cfg.daily_loss_limit_lamports, -1_500_000_000);
        assert_eq!(cfg.consecutive_loss_limit, 5);
        assert_eq!(cfg.pause_duration_ms, 300_000);
        assert_eq!(cfg.daily_trade_limit, 60);
        assert_eq!(cfg.loss_cooldown_ms, 5_000);
        assert_eq!(cfg.max_concurrent_scalp, 5);
        assert_eq!(cfg.max_concurrent_ride, 3);
        assert_eq!(cfg.max_concurrent_total, 8);
    }

    // ── Edge: saturating arithmetic ──────────────────────────────────────

    #[test]
    fn closing_more_than_opened_saturates_to_zero() {
        let mut rm = RiskManager::new(&test_config());
        let t = 1_000_000u64;
        // Close without opening — should not underflow
        rm.on_trade_closed(100, false, t);
        assert_eq!(rm.scalp_count, 0);
        rm.on_trade_closed(100, true, t);
        assert_eq!(rm.ride_count, 0);
    }

    // ── Integration: multi-day scenario ──────────────────────────────────

    #[test]
    fn multi_day_lifecycle() {
        let mut rm = RiskManager::new(&test_config());

        // Day 200
        let d200 = day_start_ms(200) + 1_000;
        rm.check_daily_reset(d200);

        // Open 2 scalps, close one at a loss
        rm.on_trade_opened(false);
        rm.on_trade_opened(false);
        rm.on_trade_closed(-500_000_000, false, d200 + 2_000);
        assert_eq!(rm.daily_pnl_lamports, -500_000_000);
        assert_eq!(rm.scalp_count, 1);

        // Close second at a loss (but not breaching daily limit)
        rm.on_trade_closed(-400_000_000, false, d200 + 10_000);
        assert_eq!(rm.daily_pnl_lamports, -900_000_000);
        assert_eq!(rm.consecutive_losses, 2);

        // Day 201 — everything resets except pause
        let d201 = day_start_ms(201) + 1_000;
        rm.check_daily_reset(d201);
        assert_eq!(rm.daily_pnl_lamports, 0);
        assert_eq!(rm.consecutive_losses, 0);
        assert_eq!(rm.daily_trade_count, 0);
        assert!(rm.allows_entry(d201, false));
    }
}
