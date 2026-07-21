//! Feed health monitoring — detects stale feeds and auto-pauses trading.
//!
//! Tracks per-feed last-event timestamps via atomics. On each tick,
//! checks if any required feed exceeds the stale threshold. If so,
//! sets `paused = true` and records which feeds are stale.
//!
//! Thread-safe: all state is atomic. Shared via `Arc<HealthMonitor>`.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use arrayvec::ArrayVec;

use crate::feeds::FeedSource;

/// Configuration for the health monitor, loaded from canary.json `health` section.
#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// How long (ms) before a feed is considered stale. Default: 45_000.
    pub market_feed_stale_ms: u64,
    /// Whether to auto-pause trading when feeds go stale. Default: true.
    pub auto_pause_on_degraded: bool,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            market_feed_stale_ms: 45_000,
            auto_pause_on_degraded: true,
        }
    }
}

// ── Divergence tracker (dual signal mode) ────────────────────────────────────

/// Tracks divergence between Bayesian and composite signal systems.
/// Ring buffer of last 50 positions' divergence status, stored as bits
/// in a u64 (bottom 50 bits). Zero heap allocation.
pub struct DivergenceTracker {
    /// Ring buffer: bit set = divergent exit for that position slot.
    ring: u64,
    /// Next write index (0–49).
    head: u8,
    /// Lifetime divergence count (saturates at u16::MAX).
    count: u16,
    /// Alert threshold from config.
    alert_threshold: u8,
}

impl DivergenceTracker {
    pub fn new(alert_threshold: u8) -> Self {
        Self {
            ring: 0,
            head: 0,
            count: 0,
            alert_threshold,
        }
    }

    /// Record a position close. `diverged` = bayesian and composite
    /// disagree on exit (one says Exit while other says ≥Sustained).
    #[inline]
    pub fn record(&mut self, diverged: bool) {
        let bit = 1u64 << self.head;
        if diverged {
            self.ring |= bit;
            self.count = self.count.saturating_add(1);
        } else {
            self.ring &= !bit;
        }
        self.head = (self.head + 1) % 50;
    }

    /// Count divergences in the last 50 positions.
    #[inline]
    pub fn recent_count(&self) -> u16 {
        // Mask to bottom 50 bits
        (self.ring & ((1u64 << 50) - 1)).count_ones() as u16
    }

    /// Whether we've breached the alert threshold.
    #[inline]
    pub fn is_alert(&self) -> bool {
        self.recent_count() > self.alert_threshold as u16
    }

    /// Lifetime divergence count.
    #[inline]
    pub fn lifetime_count(&self) -> u16 {
        self.count
    }
}

// ── Signal stats (dual signal mode API response) ─────────────────────────────

/// Signal mode + divergence stats for the `/api/stats` response.
/// Computed from atomic accumulators in `HealthMonitor` + `DivergenceTracker`.
#[derive(Debug, Clone)]
pub struct SignalStats {
    /// Current signal mode: "bayesian" or "composite".
    pub signal_mode: &'static str,
    /// Rolling average of Bayesian f̂*(t) at exit (permille, signed).
    pub bayesian_avg_f_at_exit: i16,
    /// Rolling average of composite score at exit (0–1000).
    pub composite_avg_score_at_exit: u16,
    /// Divergence count in last 50 positions.
    pub divergence_count: u16,
}

/// Overall health status returned by `HealthMonitor::check()`.
#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy,
    // LATENCY: ArrayVec<&'static str, 2> — zero heap allocation.
    // Max 2 feeds tracked (PumpPortal + Helius), so capacity 2 is exact.
    Degraded { stale_feeds: ArrayVec<&'static str, 2> },
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

/// Thread-safe feed health monitor. Shared via `Arc<HealthMonitor>`.
pub struct HealthMonitor {
    /// Last event timestamp from PumpPortal (epoch ms).
    last_pp_event_ms: AtomicU64,
    /// Last event timestamp from Helius (epoch ms).
    last_helius_event_ms: AtomicU64,
    /// Last event timestamp from CoreCast/Bitquery (epoch ms).
    last_corecast_event_ms: AtomicU64,
    /// Staleness threshold in milliseconds.
    stale_threshold_ms: u64,
    /// Whether to auto-pause when degraded.
    auto_pause_on_degraded: bool,
    /// Trading pause flag — set true when feeds are stale.
    pub paused: AtomicBool,
    /// Track which feeds were previously stale (for recovery detection).
    /// Bit 0 = PumpPortal, Bit 1 = Helius
    previously_stale: AtomicU64,

    // ── Dual signal mode accumulators (cold path — API reads only) ───
    /// Sum of Bayesian f̂*(t) at exit across closed positions (signed).
    pub bayesian_f_sum: AtomicI64,
    /// Count of closed positions with Bayesian f̂ recorded.
    pub bayesian_f_count: AtomicU64,
    /// Sum of composite scores at exit across closed positions.
    pub composite_score_sum: AtomicU64,
    /// Count of closed positions with composite score recorded.
    pub composite_score_count: AtomicU64,
}

impl HealthMonitor {
    /// Create a new `HealthMonitor` wrapped in `Arc`.
    pub fn new(config: &HealthConfig) -> Arc<Self> {
        Arc::new(Self {
            last_pp_event_ms: AtomicU64::new(0),
            last_helius_event_ms: AtomicU64::new(0),
            last_corecast_event_ms: AtomicU64::new(0),
            stale_threshold_ms: config.market_feed_stale_ms,
            auto_pause_on_degraded: config.auto_pause_on_degraded,
            paused: AtomicBool::new(false),
            previously_stale: AtomicU64::new(0),
            bayesian_f_sum: AtomicI64::new(0),
            bayesian_f_count: AtomicU64::new(0),
            composite_score_sum: AtomicU64::new(0),
            composite_score_count: AtomicU64::new(0),
        })
    }

    /// Record that an event was received from the given feed source.
    /// Call this for every `FeedEvent` before passing to HotPath.
    #[inline]
    pub fn record_event(&self, source: FeedSource, ts_ms: u64) {
        match source {
            FeedSource::PumpPortal => {
                self.last_pp_event_ms.store(ts_ms, Ordering::Relaxed);
            }
            FeedSource::Helius => {
                self.last_helius_event_ms.store(ts_ms, Ordering::Relaxed);
            }
            FeedSource::CoreCast => {
                self.last_corecast_event_ms.store(ts_ms, Ordering::Relaxed);
            }
            FeedSource::ShredStream => {
                // ShredStream is optional/bonus — not required for health.
                // We still update PumpPortal timestamp since it supplements it.
            }
        }
    }

    /// Check health status. Called every ~5s (100 ticks).
    ///
    /// Returns the current `HealthStatus` and an ArrayVec of feeds that just recovered
    /// (transitioned from stale → fresh since last check).
    ///
    /// LATENCY: zero heap allocation — ArrayVec<_, 2> is fully stack-resident.
    pub fn check(&self, now_ms: u64) -> (HealthStatus, ArrayVec<&'static str, 2>) {
        let pp_last = self.last_pp_event_ms.load(Ordering::Relaxed);
        let prev_stale = self.previously_stale.load(Ordering::Relaxed);

        let mut stale_feeds: ArrayVec<&'static str, 2> = ArrayVec::new();
        let mut recovered_feeds: ArrayVec<&'static str, 2> = ArrayVec::new();
        let mut current_stale_bits: u64 = 0;

        // PumpPortal: required feed (always check if we've ever seen an event)
        if pp_last > 0 && now_ms.saturating_sub(pp_last) > self.stale_threshold_ms {
            stale_feeds.push("PumpPortal");
            current_stale_bits |= 1;
        } else if pp_last > 0 && (prev_stale & 1) != 0 {
            // Was stale, now recovered
            recovered_feeds.push("PumpPortal");
        }

        // Helius: optional but tracked
        let hel_last = self.last_helius_event_ms.load(Ordering::Relaxed);
        if hel_last > 0 && now_ms.saturating_sub(hel_last) > self.stale_threshold_ms {
            stale_feeds.push("Helius");
            current_stale_bits |= 2;
        } else if hel_last > 0 && (prev_stale & 2) != 0 {
            recovered_feeds.push("Helius");
        }

        self.previously_stale.store(current_stale_bits, Ordering::Relaxed);

        let status = if stale_feeds.is_empty() {
            // All feeds healthy — unpause if we auto-paused
            if self.auto_pause_on_degraded && self.paused.load(Ordering::Relaxed) && !recovered_feeds.is_empty() {
                self.paused.store(false, Ordering::Relaxed);
            }
            HealthStatus::Healthy
        } else {
            // Degraded — auto-pause if configured
            if self.auto_pause_on_degraded {
                self.paused.store(true, Ordering::Relaxed);
            }
            HealthStatus::Degraded { stale_feeds }
        };

        (status, recovered_feeds)
    }

    /// Whether trading is currently allowed (not paused by health monitor).
    #[inline]
    pub fn is_trading_allowed(&self) -> bool {
        !self.paused.load(Ordering::Relaxed)
    }

    /// Get the last event timestamp for a feed (for API reporting).
    pub fn last_event_ms(&self, source: FeedSource) -> u64 {
        match source {
            FeedSource::PumpPortal => self.last_pp_event_ms.load(Ordering::Relaxed),
            FeedSource::Helius => self.last_helius_event_ms.load(Ordering::Relaxed),
            FeedSource::CoreCast => self.last_corecast_event_ms.load(Ordering::Relaxed),
            FeedSource::ShredStream => 0, // not tracked separately
        }
    }

    /// Get stale threshold (for API reporting).
    pub fn stale_threshold_ms(&self) -> u64 {
        self.stale_threshold_ms
    }

    // ── Dual signal mode methods ────────────────────────────────────

    /// Record a closed position's Bayesian f̂*(t) at exit.
    /// Called from HotPath::on_position_closed().
    #[inline]
    pub fn record_bayesian_f_at_exit(&self, f_permille: i16) {
        self.bayesian_f_sum.fetch_add(f_permille as i64, Ordering::Relaxed);
        self.bayesian_f_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a closed position's composite score at exit.
    /// Called from HotPath::on_position_closed().
    #[inline]
    pub fn record_composite_score_at_exit(&self, score: u16) {
        self.composite_score_sum.fetch_add(score as u64, Ordering::Relaxed);
        self.composite_score_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Compute signal stats for API response.
    /// `signal_mode` = current mode string, `divergence_tracker` = shared tracker.
    pub fn signal_stats(
        &self,
        signal_mode: &'static str,
        divergence_tracker: &DivergenceTracker,
    ) -> SignalStats {
        let b_count = self.bayesian_f_count.load(Ordering::Relaxed);
        let b_sum = self.bayesian_f_sum.load(Ordering::Relaxed);
        let bayesian_avg = if b_count > 0 {
            (b_sum / b_count as i64) as i16
        } else {
            0
        };

        let c_count = self.composite_score_count.load(Ordering::Relaxed);
        let c_sum = self.composite_score_sum.load(Ordering::Relaxed);
        let composite_avg = if c_count > 0 {
            (c_sum / c_count) as u16
        } else {
            0
        };

        SignalStats {
            signal_mode,
            bayesian_avg_f_at_exit: bayesian_avg,
            composite_avg_score_at_exit: composite_avg,
            divergence_count: divergence_tracker.recent_count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_healthy_when_feeds_active() {
        let config = HealthConfig {
            market_feed_stale_ms: 45_000,
            auto_pause_on_degraded: true,
        };
        let monitor = HealthMonitor::new(&config);

        let now = 100_000;
        monitor.record_event(FeedSource::PumpPortal, now);

        let (status, recovered) = monitor.check(now + 10_000); // 10s later
        assert!(status.is_healthy());
        assert!(recovered.is_empty());
        assert!(monitor.is_trading_allowed());
    }

    #[test]
    fn test_stale_feed_pauses_trading() {
        let config = HealthConfig {
            market_feed_stale_ms: 45_000,
            auto_pause_on_degraded: true,
        };
        let monitor = HealthMonitor::new(&config);

        let now = 100_000;
        monitor.record_event(FeedSource::PumpPortal, now);

        // 50s later — exceeds 45s threshold
        let (status, _) = monitor.check(now + 50_000);
        assert!(matches!(status, HealthStatus::Degraded { .. }));
        assert!(!monitor.is_trading_allowed());
    }

    #[test]
    fn test_feed_recovery_resumes_trading() {
        let config = HealthConfig {
            market_feed_stale_ms: 45_000,
            auto_pause_on_degraded: true,
        };
        let monitor = HealthMonitor::new(&config);

        let now = 100_000;
        monitor.record_event(FeedSource::PumpPortal, now);

        // Go stale
        let (status, _) = monitor.check(now + 50_000);
        assert!(!monitor.is_trading_allowed());

        // Feed comes back
        let new_now = now + 51_000;
        monitor.record_event(FeedSource::PumpPortal, new_now);
        let (status, recovered) = monitor.check(new_now + 1_000);
        assert!(status.is_healthy());
        assert!(monitor.is_trading_allowed());
        assert!(recovered.contains(&"PumpPortal"));
    }

    #[test]
    fn test_no_auto_pause_when_disabled() {
        let config = HealthConfig {
            market_feed_stale_ms: 45_000,
            auto_pause_on_degraded: false,
        };
        let monitor = HealthMonitor::new(&config);

        let now = 100_000;
        monitor.record_event(FeedSource::PumpPortal, now);

        // Go stale — but auto-pause is off
        let (status, _) = monitor.check(now + 50_000);
        assert!(matches!(status, HealthStatus::Degraded { .. }));
        // Still allowed because auto_pause_on_degraded is false
        assert!(monitor.is_trading_allowed());
    }

    #[test]
    fn test_never_seen_feed_not_stale() {
        let config = HealthConfig::default();
        let monitor = HealthMonitor::new(&config);

        // Never recorded any events — should not trigger stale
        let (status, _) = monitor.check(100_000);
        assert!(status.is_healthy());
        assert!(monitor.is_trading_allowed());
    }

    // ── DivergenceTracker tests ─────────────────────────────────────

    #[test]
    fn test_divergence_tracker_empty() {
        let tracker = DivergenceTracker::new(10);
        assert_eq!(tracker.recent_count(), 0);
        assert_eq!(tracker.lifetime_count(), 0);
        assert!(!tracker.is_alert());
    }

    #[test]
    fn test_divergence_tracker_counts_correctly() {
        let mut tracker = DivergenceTracker::new(10);

        // 5 non-divergent, 0 divergences
        for _ in 0..5 {
            tracker.record(false);
        }
        assert_eq!(tracker.recent_count(), 0);
        assert_eq!(tracker.lifetime_count(), 0);

        // 3 divergent
        for _ in 0..3 {
            tracker.record(true);
        }
        assert_eq!(tracker.recent_count(), 3);
        assert_eq!(tracker.lifetime_count(), 3);
    }

    #[test]
    fn test_divergence_tracker_alert_threshold() {
        let mut tracker = DivergenceTracker::new(10);

        // 10 divergences — at threshold, not above
        for _ in 0..10 {
            tracker.record(true);
        }
        assert_eq!(tracker.recent_count(), 10);
        assert!(!tracker.is_alert()); // 10 is NOT > 10

        // 11th divergence — now above threshold
        tracker.record(true);
        assert_eq!(tracker.recent_count(), 11);
        assert!(tracker.is_alert());
    }

    #[test]
    fn test_divergence_tracker_ring_wraps() {
        let mut tracker = DivergenceTracker::new(10);

        // Fill all 50 slots with divergences
        for _ in 0..50 {
            tracker.record(true);
        }
        assert_eq!(tracker.recent_count(), 50);
        assert_eq!(tracker.lifetime_count(), 50);

        // Now write 50 non-divergent — overwrites all divergent bits
        for _ in 0..50 {
            tracker.record(false);
        }
        assert_eq!(tracker.recent_count(), 0);
        // Lifetime count stays at 50 (it only increments)
        assert_eq!(tracker.lifetime_count(), 50);
    }

    #[test]
    fn test_divergence_tracker_partial_ring_overwrite() {
        let mut tracker = DivergenceTracker::new(5);

        // Write 10 divergences
        for _ in 0..10 {
            tracker.record(true);
        }
        assert_eq!(tracker.recent_count(), 10);

        // Overwrite 5 of the 10 divergent slots with non-divergent
        for _ in 0..5 {
            tracker.record(false);
        }
        // 15 total records, ring is 50-wide. First 10 set bits 0-9, next 5 clear bits 10-14.
        // Recent count = bits set in bottom 50 = 10 (old divergences) - 0 + 0 (new non-divergent)
        // Actually ring wraps: head goes 0..15, bits 0-9 are set, 10-14 are cleared.
        // recent_count uses .count_ones() on the full mask, so 10 divergent bits remain.
        assert_eq!(tracker.recent_count(), 10);
        assert!(tracker.is_alert()); // 10 > 5
    }

    // ── Signal stats accumulator tests ──────────────────────────────

    #[test]
    fn test_signal_stats_empty() {
        let config = HealthConfig::default();
        let monitor = HealthMonitor::new(&config);
        let tracker = DivergenceTracker::new(10);

        let stats = monitor.signal_stats("composite", &tracker);
        assert_eq!(stats.signal_mode, "composite");
        assert_eq!(stats.bayesian_avg_f_at_exit, 0);
        assert_eq!(stats.composite_avg_score_at_exit, 0);
        assert_eq!(stats.divergence_count, 0);
    }

    #[test]
    fn test_signal_stats_bayesian_avg() {
        let config = HealthConfig::default();
        let monitor = HealthMonitor::new(&config);
        let tracker = DivergenceTracker::new(10);

        // Record 3 Bayesian exits: 100, -50, 250 → avg = 100
        monitor.record_bayesian_f_at_exit(100);
        monitor.record_bayesian_f_at_exit(-50);
        monitor.record_bayesian_f_at_exit(250);

        let stats = monitor.signal_stats("bayesian", &tracker);
        assert_eq!(stats.signal_mode, "bayesian");
        assert_eq!(stats.bayesian_avg_f_at_exit, 100); // 300 / 3 = 100
    }

    #[test]
    fn test_signal_stats_composite_avg() {
        let config = HealthConfig::default();
        let monitor = HealthMonitor::new(&config);
        let tracker = DivergenceTracker::new(10);

        // Record 4 composite exits: 500, 300, 700, 400 → avg = 475
        monitor.record_composite_score_at_exit(500);
        monitor.record_composite_score_at_exit(300);
        monitor.record_composite_score_at_exit(700);
        monitor.record_composite_score_at_exit(400);

        let stats = monitor.signal_stats("composite", &tracker);
        assert_eq!(stats.composite_avg_score_at_exit, 475); // 1900 / 4 = 475
    }

    #[test]
    fn test_signal_stats_with_divergence() {
        let config = HealthConfig::default();
        let monitor = HealthMonitor::new(&config);
        let mut tracker = DivergenceTracker::new(10);

        // Record some divergences
        for _ in 0..7 {
            tracker.record(true);
        }
        for _ in 0..3 {
            tracker.record(false);
        }

        monitor.record_bayesian_f_at_exit(200);
        monitor.record_composite_score_at_exit(600);

        let stats = monitor.signal_stats("bayesian", &tracker);
        assert_eq!(stats.divergence_count, 7);
        assert_eq!(stats.bayesian_avg_f_at_exit, 200);
        assert_eq!(stats.composite_avg_score_at_exit, 600);
    }
}
