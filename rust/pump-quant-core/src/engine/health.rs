//! Feed health monitoring — detects stale feeds and auto-pauses trading.
//!
//! Tracks per-feed last-event timestamps via atomics. On each tick,
//! checks if any required feed exceeds the stale threshold. If so,
//! sets `paused = true` and records which feeds are stale.
//!
//! Thread-safe: all state is atomic. Shared via `Arc<HealthMonitor>`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

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

/// Overall health status returned by `HealthMonitor::check()`.
#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy,
    Degraded { stale_feeds: Vec<String> },
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
    /// Staleness threshold in milliseconds.
    stale_threshold_ms: u64,
    /// Whether to auto-pause when degraded.
    auto_pause_on_degraded: bool,
    /// Trading pause flag — set true when feeds are stale.
    pub paused: AtomicBool,
    /// Track which feeds were previously stale (for recovery detection).
    /// Bit 0 = PumpPortal, Bit 1 = Helius
    previously_stale: AtomicU64,
}

impl HealthMonitor {
    /// Create a new `HealthMonitor` wrapped in `Arc`.
    pub fn new(config: &HealthConfig) -> Arc<Self> {
        Arc::new(Self {
            last_pp_event_ms: AtomicU64::new(0),
            last_helius_event_ms: AtomicU64::new(0),
            stale_threshold_ms: config.market_feed_stale_ms,
            auto_pause_on_degraded: config.auto_pause_on_degraded,
            paused: AtomicBool::new(false),
            previously_stale: AtomicU64::new(0),
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
            FeedSource::ShredStream => {
                // ShredStream is optional/bonus — not required for health.
                // We still update PumpPortal timestamp since it supplements it.
            }
        }
    }

    /// Check health status. Call on every tick (every ~50ms).
    ///
    /// Returns the current `HealthStatus` and a vec of feeds that just recovered
    /// (transitioned from stale → fresh since last check).
    pub fn check(&self, now_ms: u64) -> (HealthStatus, Vec<String>) {
        let pp_last = self.last_pp_event_ms.load(Ordering::Relaxed);
        let prev_stale = self.previously_stale.load(Ordering::Relaxed);

        let mut stale_feeds = Vec::new();
        let mut recovered_feeds = Vec::new();
        let mut current_stale_bits: u64 = 0;

        // PumpPortal: required feed (always check if we've ever seen an event)
        if pp_last > 0 && now_ms.saturating_sub(pp_last) > self.stale_threshold_ms {
            stale_feeds.push("PumpPortal".to_string());
            current_stale_bits |= 1;
        } else if pp_last > 0 && (prev_stale & 1) != 0 {
            // Was stale, now recovered
            recovered_feeds.push("PumpPortal".to_string());
        }

        // Helius: optional but tracked
        let hel_last = self.last_helius_event_ms.load(Ordering::Relaxed);
        if hel_last > 0 && now_ms.saturating_sub(hel_last) > self.stale_threshold_ms {
            stale_feeds.push("Helius".to_string());
            current_stale_bits |= 2;
        } else if hel_last > 0 && (prev_stale & 2) != 0 {
            recovered_feeds.push("Helius".to_string());
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
            FeedSource::ShredStream => 0, // not tracked separately
        }
    }

    /// Get stale threshold (for API reporting).
    pub fn stale_threshold_ms(&self) -> u64 {
        self.stale_threshold_ms
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
        assert!(recovered.contains(&"PumpPortal".to_string()));
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
}
