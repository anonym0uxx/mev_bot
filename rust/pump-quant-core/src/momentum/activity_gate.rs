//! Pre-entry activity verification gate.
//!
//! Prevents entries on dead tokens by requiring minimum WebSocket activity
//! before allowing a buy. On-chain data shows 61/167 trades (36.5%) were
//! dead tokens with zero price movement, each losing -5.3% in AMM fees.
//!
//! ## Design
//!
//! The `ActivityTracker` maintains per-mint activity metrics using atomics
//! (no Mutex) for lock-free reads on the hot path. The `check_entry()`
//! function runs four O(1) gates:
//!
//! 1. **Minimum WS notifications** — token must have observable trading activity
//! 2. **Recency** — last trade must be within `max_last_trade_age_ms`
//! 3. **Buy pressure** — at least `min_buys_3s` buy-side transactions
//! 4. **Price range** — price must have moved ≥ `min_price_range_bps`
//!
//! ## Mathematical justification
//!
//! 61 dead trades × 0.000823 avg loss = 0.050 SOL wasted on round-trip fees.
//! Filter blocks ~90% of dead tokens (55 trades): +0.045 SOL saved.
//! Filter blocks ~5% of winners (1 trade): -0.003 SOL lost.
//! Net improvement: **+0.042 SOL per 167 trades**.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

// ── Configuration ────────────────────────────────────────────────────────────

fn default_activity_gate_enabled() -> bool {
    true
}
fn default_min_ws_notifs() -> u64 {
    5
}
fn default_max_last_trade_age_ms() -> u64 {
    2000
}
fn default_min_buys_3s() -> u64 {
    1
}
fn default_min_price_range_bps() -> u64 {
    50
}
fn default_cleanup_stale_ms() -> u64 {
    60_000
}

/// Configuration for the pre-entry activity gate.
///
/// All fields have serde defaults so the section can be omitted entirely
/// from canary.json (gate defaults to enabled with conservative thresholds).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ActivityGateConfig {
    /// Master toggle. When false, `check_entry` always returns `Proceed`.
    #[serde(default = "default_activity_gate_enabled")]
    pub enabled: bool,

    /// Minimum total WS accountSubscribe notifications required before entry.
    /// Each notification ≈ one swap on the Raydium/PumpSwap pool.
    /// Dead tokens typically have 0-2 notifications. Default: 5.
    #[serde(default = "default_min_ws_notifs")]
    pub min_ws_notifs: u64,

    /// Maximum age (ms) of the most recent WS notification.
    /// Rejects tokens where trading has gone stale. Default: 2000ms.
    #[serde(default = "default_max_last_trade_age_ms")]
    pub max_last_trade_age_ms: u64,

    /// Minimum buy-side transactions observed in the recent window.
    /// Ensures there's active demand, not just sells dumping. Default: 1.
    #[serde(default = "default_min_buys_3s")]
    pub min_buys_3s: u64,

    /// Minimum price range in basis points (max - min) / min × 10000.
    /// Flat-price tokens (range < 50 bps) are dead. Default: 50 (0.5%).
    #[serde(default = "default_min_price_range_bps")]
    pub min_price_range_bps: u64,

    /// How long (ms) before a mint's activity record is considered stale
    /// and eligible for cleanup. Default: 60_000ms (60s).
    #[serde(default = "default_cleanup_stale_ms")]
    pub cleanup_stale_ms: u64,
}

impl Default for ActivityGateConfig {
    fn default() -> Self {
        Self {
            enabled: default_activity_gate_enabled(),
            min_ws_notifs: default_min_ws_notifs(),
            max_last_trade_age_ms: default_max_last_trade_age_ms(),
            min_buys_3s: default_min_buys_3s(),
            min_price_range_bps: default_min_price_range_bps(),
            cleanup_stale_ms: default_cleanup_stale_ms(),
        }
    }
}

// ── Decision types ───────────────────────────────────────────────────────────

/// Result of the pre-entry activity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityDecision {
    /// All gates passed — proceed with entry.
    Proceed,
    /// One or more gates failed — reject entry.
    Reject(ActivityRejectReason),
}

/// Specific reason why entry was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityRejectReason {
    /// Too few WS notifications observed for this mint.
    InsufficientActivity { notifs: u64, required: u64 },
    /// Most recent trade is too old.
    StaleTrade { age_ms: u64, max_age_ms: u64 },
    /// No buy-side pressure in the recent window.
    NoBuyPressure { buys_3s: u64 },
    /// Price hasn't moved enough (dead/flat token).
    FlatPrice { range_bps: u64, min_range_bps: u64 },
}

impl std::fmt::Display for ActivityRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientActivity { notifs, required } => {
                write!(f, "insufficient_activity(notifs={notifs}, required={required})")
            }
            Self::StaleTrade { age_ms, max_age_ms } => {
                write!(f, "stale_trade(age={age_ms}ms, max={max_age_ms}ms)")
            }
            Self::NoBuyPressure { buys_3s } => {
                write!(f, "no_buy_pressure(buys_3s={buys_3s})")
            }
            Self::FlatPrice { range_bps, min_range_bps } => {
                write!(f, "flat_price(range={range_bps}bps, min={min_range_bps}bps)")
            }
        }
    }
}

// ── Per-mint activity state ──────────────────────────────────────────────────

/// Rolling activity metrics for a single mint.
///
/// All fields are atomic for lock-free concurrent access from the WS
/// notification path (writer) and the entry gate path (reader).
pub struct MintActivity {
    /// Total WS notifications received for this mint's pool vaults.
    pub total_notifs: AtomicU64,
    /// Timestamp (epoch ms) of the first notification.
    pub first_notif_ms: AtomicU64,
    /// Timestamp (epoch ms) of the most recent notification.
    pub last_notif_ms: AtomicU64,
    /// Number of buy-side transactions observed in the recent window.
    /// Note: this is a cumulative counter (not truly windowed) for O(1).
    /// For dead tokens, buys_3s == 0 is the dominant signal.
    pub buys_3s: AtomicU64,
    /// Number of sell-side transactions observed.
    pub sells_3s: AtomicU64,
    /// Price (fixed-point) at first observation.
    pub first_price_fp: AtomicU64,
    /// Minimum price seen across all notifications.
    pub min_price_fp: AtomicU64,
    /// Maximum price seen across all notifications.
    pub max_price_fp: AtomicU64,
}

impl MintActivity {
    /// Create a new activity record seeded with initial observation.
    fn new(now_ms: u64, price_fp: u64) -> Self {
        Self {
            total_notifs: AtomicU64::new(0),
            first_notif_ms: AtomicU64::new(now_ms),
            last_notif_ms: AtomicU64::new(now_ms),
            buys_3s: AtomicU64::new(0),
            sells_3s: AtomicU64::new(0),
            first_price_fp: AtomicU64::new(price_fp),
            min_price_fp: AtomicU64::new(price_fp),
            max_price_fp: AtomicU64::new(price_fp),
        }
    }
}

// ── Activity Tracker ─────────────────────────────────────────────────────────

/// Tracks recent trading activity per mint for pre-entry gating.
///
/// Populated by the WS notification path in `PriceFeedManager`. Queried
/// by `MomentumEngine::on_graduation()` before opening any position.
///
/// Memory is bounded by periodic `cleanup()` calls (every ~10s) which
/// remove mints with no activity in the last 60s.
pub struct ActivityTracker {
    /// Per-mint activity windows. Key = mint pubkey bytes.
    states: DashMap<[u8; 32], MintActivity>,
}

impl ActivityTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
        }
    }

    /// Check whether a mint has sufficient activity to allow entry.
    ///
    /// All operations are O(1) — single DashMap lookup + atomic loads.
    /// Called from the cold path (`on_graduation`), not the hot tick loop.
    pub fn check_entry(
        &self,
        mint: &[u8; 32],
        now_ms: u64,
        config: &ActivityGateConfig,
    ) -> ActivityDecision {
        // Fast path: gate disabled
        if !config.enabled {
            return ActivityDecision::Proceed;
        }

        let state = match self.states.get(mint) {
            Some(s) => s,
            None => {
                return ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity {
                    notifs: 0,
                    required: config.min_ws_notifs,
                });
            }
        };

        let notifs = state.total_notifs.load(Ordering::Relaxed);
        let last_ms = state.last_notif_ms.load(Ordering::Relaxed);
        let buys = state.buys_3s.load(Ordering::Relaxed);

        // Gate 1: Minimum total notifications
        // Dead tokens have 0-2 notifs. Active tokens have 5+ within seconds.
        if notifs < config.min_ws_notifs {
            return ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity {
                notifs,
                required: config.min_ws_notifs,
            });
        }

        // Gate 2: Last trade must be recent
        // Stale trades indicate the token had brief activity but went dead.
        let age_ms = now_ms.saturating_sub(last_ms);
        if age_ms > config.max_last_trade_age_ms {
            return ActivityDecision::Reject(ActivityRejectReason::StaleTrade {
                age_ms,
                max_age_ms: config.max_last_trade_age_ms,
            });
        }

        // Gate 3: Must have buy-side activity
        // Sell-only activity = sniper dump, not organic demand.
        if buys < config.min_buys_3s {
            return ActivityDecision::Reject(ActivityRejectReason::NoBuyPressure {
                buys_3s: buys,
            });
        }

        // Gate 4: Price must have moved (not flat)
        // Dead tokens show range_bps == 0 because no trades changed the price.
        let min_p = state.min_price_fp.load(Ordering::Relaxed);
        let max_p = state.max_price_fp.load(Ordering::Relaxed);
        let range_bps = if min_p > 0 {
            ((max_p.saturating_sub(min_p)) * 10_000) / min_p
        } else {
            0
        };
        if range_bps < config.min_price_range_bps {
            return ActivityDecision::Reject(ActivityRejectReason::FlatPrice {
                range_bps,
                min_range_bps: config.min_price_range_bps,
            });
        }

        ActivityDecision::Proceed
    }

    /// Called by the price feed on every WS accountSubscribe notification.
    ///
    /// This is the hot path — must be fast. All operations are atomic
    /// (no allocation after initial `or_insert_with`).
    pub fn on_ws_notification(
        &self,
        mint: &[u8; 32],
        now_ms: u64,
        price_fp: u64,
        is_buy: bool,
    ) {
        let entry = self
            .states
            .entry(*mint)
            .or_insert_with(|| MintActivity::new(now_ms, price_fp));

        entry.total_notifs.fetch_add(1, Ordering::Relaxed);
        entry.last_notif_ms.store(now_ms, Ordering::Relaxed);

        // Update min price (CAS-free: relaxed store is fine for approximate tracking)
        let cur_min = entry.min_price_fp.load(Ordering::Relaxed);
        if price_fp < cur_min || cur_min == 0 {
            entry.min_price_fp.store(price_fp, Ordering::Relaxed);
        }

        // Update max price
        let cur_max = entry.max_price_fp.load(Ordering::Relaxed);
        if price_fp > cur_max {
            entry.max_price_fp.store(price_fp, Ordering::Relaxed);
        }

        if is_buy {
            entry.buys_3s.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.sells_3s.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Periodic cleanup: remove mints with no activity in the last `stale_ms`.
    ///
    /// Called every ~10s from the tick loop to bound memory. With ~10
    /// graduations/day, this keeps the map under ~100 entries.
    pub fn cleanup(&self, now_ms: u64, stale_ms: u64) {
        self.states.retain(|_, v| {
            now_ms.saturating_sub(v.last_notif_ms.load(Ordering::Relaxed)) < stale_ms
        });
    }

    /// Returns the number of mints currently tracked (for monitoring).
    #[inline]
    pub fn tracked_count(&self) -> usize {
        self.states.len()
    }

    /// Returns a snapshot of activity metrics for a mint (for logging/debugging).
    /// Returns `None` if the mint is not tracked.
    pub fn snapshot(&self, mint: &[u8; 32]) -> Option<ActivitySnapshot> {
        self.states.get(mint).map(|s| ActivitySnapshot {
            total_notifs: s.total_notifs.load(Ordering::Relaxed),
            first_notif_ms: s.first_notif_ms.load(Ordering::Relaxed),
            last_notif_ms: s.last_notif_ms.load(Ordering::Relaxed),
            buys_3s: s.buys_3s.load(Ordering::Relaxed),
            sells_3s: s.sells_3s.load(Ordering::Relaxed),
            first_price_fp: s.first_price_fp.load(Ordering::Relaxed),
            min_price_fp: s.min_price_fp.load(Ordering::Relaxed),
            max_price_fp: s.max_price_fp.load(Ordering::Relaxed),
        })
    }
}

/// Point-in-time snapshot of a mint's activity (all atomics read once).
#[derive(Debug, Clone)]
pub struct ActivitySnapshot {
    pub total_notifs: u64,
    pub first_notif_ms: u64,
    pub last_notif_ms: u64,
    pub buys_3s: u64,
    pub sells_3s: u64,
    pub first_price_fp: u64,
    pub min_price_fp: u64,
    pub max_price_fp: u64,
}

impl ActivitySnapshot {
    /// Price range in basis points: (max - min) / min × 10000.
    pub fn price_range_bps(&self) -> u64 {
        if self.min_price_fp > 0 {
            ((self.max_price_fp.saturating_sub(self.min_price_fp)) * 10_000) / self.min_price_fp
        } else {
            0
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mint(id: u8) -> [u8; 32] {
        let mut m = [0u8; 32];
        m[0] = id;
        m
    }

    fn default_config() -> ActivityGateConfig {
        ActivityGateConfig::default()
    }

    // ── Constructor / basic ──────────────────────────────────────────────

    #[test]
    fn test_new_tracker_is_empty() {
        let tracker = ActivityTracker::new();
        assert_eq!(tracker.tracked_count(), 0);
    }

    #[test]
    fn test_config_defaults() {
        let cfg = ActivityGateConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.min_ws_notifs, 5);
        assert_eq!(cfg.max_last_trade_age_ms, 2000);
        assert_eq!(cfg.min_buys_3s, 1);
        assert_eq!(cfg.min_price_range_bps, 50);
        assert_eq!(cfg.cleanup_stale_ms, 60_000);
    }

    // ── Gate disabled ────────────────────────────────────────────────────

    #[test]
    fn test_disabled_gate_always_proceeds() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(1);
        let mut cfg = default_config();
        cfg.enabled = false;

        // No activity at all — should still proceed when disabled
        let decision = tracker.check_entry(&mint, 1000, &cfg);
        assert_eq!(decision, ActivityDecision::Proceed);
    }

    // ── Gate 1: InsufficientActivity ─────────────────────────────────────

    #[test]
    fn test_no_activity_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(1);
        let cfg = default_config();

        let decision = tracker.check_entry(&mint, 5000, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity {
                notifs: 0,
                required: 5
            })
        ));
    }

    #[test]
    fn test_insufficient_notifs_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(1);
        let cfg = default_config();

        // Send 3 notifications (below threshold of 5)
        for i in 0..3 {
            tracker.on_ws_notification(&mint, 1000 + i * 100, 1_000_000 + i * 10_000, true);
        }

        let decision = tracker.check_entry(&mint, 1300, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity {
                notifs: 3,
                required: 5
            })
        ));
    }

    // ── Gate 2: StaleTrade ───────────────────────────────────────────────

    #[test]
    fn test_stale_trade_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(2);
        let cfg = default_config();

        // 10 notifs with price movement and buys, but all at t=1000
        for i in 0..10 {
            tracker.on_ws_notification(&mint, 1000, 1_000_000 + i * 20_000, true);
        }

        // Check at t=5000 (age = 4000ms > max 2000ms)
        let decision = tracker.check_entry(&mint, 5000, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::StaleTrade { age_ms: 4000, max_age_ms: 2000 })
        ));
    }

    // ── Gate 3: NoBuyPressure ────────────────────────────────────────────

    #[test]
    fn test_no_buy_pressure_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(3);
        let cfg = default_config();

        // 10 sell-side notifs, recent, with price movement — but no buys
        for i in 0..10 {
            tracker.on_ws_notification(&mint, 1000 + i * 100, 1_000_000 + i * 20_000, false);
        }

        let decision = tracker.check_entry(&mint, 1900, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::NoBuyPressure { buys_3s: 0 })
        ));
    }

    // ── Gate 4: FlatPrice ────────────────────────────────────────────────

    #[test]
    fn test_flat_price_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(4);
        let cfg = default_config();

        // 10 notifs, recent, with buys — but all at the SAME price
        let flat_price = 1_000_000u64;
        for i in 0..10 {
            tracker.on_ws_notification(&mint, 1000 + i * 100, flat_price, true);
        }

        let decision = tracker.check_entry(&mint, 1900, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::FlatPrice {
                range_bps: 0,
                min_range_bps: 50
            })
        ));
    }

    #[test]
    fn test_barely_below_price_range_rejects() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(5);
        let cfg = default_config(); // min_price_range_bps = 50

        // Price range: 1_000_000 to 1_004_000 = 40 bps (below 50)
        for i in 0..5 {
            tracker.on_ws_notification(&mint, 1000 + i * 100, 1_000_000, true);
        }
        for i in 5..10 {
            tracker.on_ws_notification(&mint, 1000 + i * 100, 1_004_000, true);
        }

        let decision = tracker.check_entry(&mint, 1900, &cfg);
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::FlatPrice { .. })
        ));
    }

    // ── All gates pass ───────────────────────────────────────────────────

    #[test]
    fn test_active_token_proceeds() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(10);
        let cfg = default_config();

        // Simulate active token: 10 notifs, recent, with buys, price moved 5%
        let base_price = 1_000_000u64;
        for i in 0..10 {
            let price = base_price + i * 5_500; // ~5% range over 10 notifs
            let is_buy = i % 3 != 0; // 7 buys, 3 sells
            tracker.on_ws_notification(&mint, 2000 + i * 200, price, is_buy);
        }

        let decision = tracker.check_entry(&mint, 3800, &cfg);
        assert_eq!(decision, ActivityDecision::Proceed);
    }

    #[test]
    fn test_borderline_pass() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(11);
        let cfg = default_config();

        // Exactly 5 notifs (minimum), 1 buy, price range exactly 50 bps
        // Price: 1_000_000 to 1_005_000 = 50 bps
        tracker.on_ws_notification(&mint, 1000, 1_000_000, true);
        tracker.on_ws_notification(&mint, 1100, 1_001_000, false);
        tracker.on_ws_notification(&mint, 1200, 1_002_000, false);
        tracker.on_ws_notification(&mint, 1300, 1_003_000, false);
        tracker.on_ws_notification(&mint, 1400, 1_005_000, false);

        let decision = tracker.check_entry(&mint, 1500, &cfg);
        assert_eq!(decision, ActivityDecision::Proceed);
    }

    // ── on_ws_notification accumulation ──────────────────────────────────

    #[test]
    fn test_notification_accumulates_correctly() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(20);

        tracker.on_ws_notification(&mint, 1000, 500_000, true);
        tracker.on_ws_notification(&mint, 1100, 600_000, false);
        tracker.on_ws_notification(&mint, 1200, 400_000, true);

        let snap = tracker.snapshot(&mint).unwrap();
        assert_eq!(snap.total_notifs, 3);
        assert_eq!(snap.first_notif_ms, 1000);
        assert_eq!(snap.last_notif_ms, 1200);
        assert_eq!(snap.buys_3s, 2);
        assert_eq!(snap.sells_3s, 1);
        assert_eq!(snap.first_price_fp, 500_000);
        assert_eq!(snap.min_price_fp, 400_000);
        assert_eq!(snap.max_price_fp, 600_000);
    }

    #[test]
    fn test_price_range_bps_calculation() {
        let snap = ActivitySnapshot {
            total_notifs: 10,
            first_notif_ms: 0,
            last_notif_ms: 0,
            buys_3s: 5,
            sells_3s: 5,
            first_price_fp: 1_000_000,
            min_price_fp: 1_000_000,
            max_price_fp: 1_050_000, // 5% above min
        };
        assert_eq!(snap.price_range_bps(), 500); // 5% = 500 bps
    }

    #[test]
    fn test_price_range_bps_zero_min() {
        let snap = ActivitySnapshot {
            total_notifs: 0,
            first_notif_ms: 0,
            last_notif_ms: 0,
            buys_3s: 0,
            sells_3s: 0,
            first_price_fp: 0,
            min_price_fp: 0,
            max_price_fp: 100,
        };
        assert_eq!(snap.price_range_bps(), 0); // avoid div-by-zero
    }

    // ── Cleanup ──────────────────────────────────────────────────────────

    #[test]
    fn test_cleanup_removes_stale() {
        let tracker = ActivityTracker::new();
        let mint_old = test_mint(30);
        let mint_new = test_mint(31);

        tracker.on_ws_notification(&mint_old, 1000, 1_000_000, true);
        tracker.on_ws_notification(&mint_new, 50_000, 1_000_000, true);

        assert_eq!(tracker.tracked_count(), 2);

        // Cleanup at t=62_000 with 60s stale threshold
        // mint_old (last_notif=1000) → age=61_000 ≥ 60_000 → removed
        // mint_new (last_notif=50_000) → age=12_000 < 60_000 → kept
        tracker.cleanup(62_000, 60_000);

        assert_eq!(tracker.tracked_count(), 1);
        assert!(tracker.snapshot(&mint_old).is_none());
        assert!(tracker.snapshot(&mint_new).is_some());
    }

    #[test]
    fn test_cleanup_keeps_all_when_fresh() {
        let tracker = ActivityTracker::new();
        let now = 100_000u64;

        for i in 0..5 {
            let mint = test_mint(40 + i);
            tracker.on_ws_notification(&mint, now - 1000, 1_000_000, true);
        }

        tracker.cleanup(now, 60_000);
        assert_eq!(tracker.tracked_count(), 5);
    }

    // ── Snapshot ─────────────────────────────────────────────────────────

    #[test]
    fn test_snapshot_returns_none_for_unknown() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(99);
        assert!(tracker.snapshot(&mint).is_none());
    }

    // ── Dead token simulation ────────────────────────────────────────────

    #[test]
    fn test_dead_token_scenario() {
        // Simulates a dead token: graduated from Pump.fun, got 1-2 WS notifs
        // from the graduation swap itself, then nothing. Engine would have
        // bought and lost -5.3% on AMM fees.
        let tracker = ActivityTracker::new();
        let mint = test_mint(50);
        let cfg = default_config();

        // Graduation swap generates ~2 notifications at the same price
        let grad_price = 850_000u64;
        tracker.on_ws_notification(&mint, 10_000, grad_price, true);
        tracker.on_ws_notification(&mint, 10_050, grad_price, false);

        // on_graduation fires at t=10_200 (200ms after grad)
        let decision = tracker.check_entry(&mint, 10_200, &cfg);

        // Should be rejected: only 2 notifs (need 5), flat price, only 1 buy
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity { notifs: 2, .. })
        ));
    }

    #[test]
    fn test_active_token_scenario() {
        // Simulates an active token: graduated and immediately started trading.
        // Multiple buys, sells, price moving up. This should PASS the gate.
        let tracker = ActivityTracker::new();
        let mint = test_mint(51);
        let cfg = default_config();

        let base_ms = 10_000u64;
        let base_price = 1_000_000u64;

        // Active trading: 8 buys, 4 sells, price rising from 1.0 to 1.08 (+8%)
        tracker.on_ws_notification(&mint, base_ms, base_price, true);
        tracker.on_ws_notification(&mint, base_ms + 100, base_price + 10_000, true);
        tracker.on_ws_notification(&mint, base_ms + 200, base_price + 15_000, false);
        tracker.on_ws_notification(&mint, base_ms + 300, base_price + 25_000, true);
        tracker.on_ws_notification(&mint, base_ms + 400, base_price + 30_000, true);
        tracker.on_ws_notification(&mint, base_ms + 500, base_price + 40_000, false);
        tracker.on_ws_notification(&mint, base_ms + 600, base_price + 50_000, true);
        tracker.on_ws_notification(&mint, base_ms + 700, base_price + 55_000, true);
        tracker.on_ws_notification(&mint, base_ms + 800, base_price + 65_000, false);
        tracker.on_ws_notification(&mint, base_ms + 900, base_price + 70_000, true);
        tracker.on_ws_notification(&mint, base_ms + 1000, base_price + 75_000, true);
        tracker.on_ws_notification(&mint, base_ms + 1100, base_price + 80_000, false);

        let decision = tracker.check_entry(&mint, base_ms + 1200, &cfg);
        assert_eq!(decision, ActivityDecision::Proceed);

        // Verify snapshot values
        let snap = tracker.snapshot(&mint).unwrap();
        assert_eq!(snap.total_notifs, 12);
        assert_eq!(snap.buys_3s, 8);
        assert_eq!(snap.sells_3s, 4);
        assert_eq!(snap.min_price_fp, base_price);
        assert_eq!(snap.max_price_fp, base_price + 80_000);
        assert_eq!(snap.price_range_bps(), 800); // 8% = 800 bps
    }

    // ── Concurrent access safety (compile-time check) ────────────────────

    #[test]
    fn test_tracker_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ActivityTracker>();
    }

    // ── Display impl ─────────────────────────────────────────────────────

    #[test]
    fn test_reject_reason_display() {
        let reason = ActivityRejectReason::InsufficientActivity {
            notifs: 2,
            required: 5,
        };
        assert_eq!(
            reason.to_string(),
            "insufficient_activity(notifs=2, required=5)"
        );

        let reason = ActivityRejectReason::FlatPrice {
            range_bps: 10,
            min_range_bps: 50,
        };
        assert_eq!(
            reason.to_string(),
            "flat_price(range=10bps, min=50bps)"
        );
    }

    // ── Custom config thresholds ─────────────────────────────────────────

    #[test]
    fn test_custom_config_thresholds() {
        let tracker = ActivityTracker::new();
        let mint = test_mint(60);

        // Relaxed config: only need 2 notifs, 5s stale window, no buy req, no price range req
        let cfg = ActivityGateConfig {
            enabled: true,
            min_ws_notifs: 2,
            max_last_trade_age_ms: 5000,
            min_buys_3s: 0,
            min_price_range_bps: 0,
            cleanup_stale_ms: 60_000,
        };

        // Just 2 notifs at the same price, no buys — passes with relaxed config
        tracker.on_ws_notification(&mint, 1000, 1_000_000, false);
        tracker.on_ws_notification(&mint, 1100, 1_000_000, false);

        let decision = tracker.check_entry(&mint, 1200, &cfg);
        assert_eq!(decision, ActivityDecision::Proceed);
    }

    // ── Gate evaluation order ────────────────────────────────────────────

    #[test]
    fn test_gates_evaluated_in_order() {
        // When multiple gates would fail, the FIRST gate in order should be reported.
        let tracker = ActivityTracker::new();
        let mint = test_mint(70);
        let cfg = default_config();

        // 1 notif (fails gate 1), stale (fails gate 2), no buys (fails gate 3), flat (fails gate 4)
        tracker.on_ws_notification(&mint, 1000, 1_000_000, false);

        let decision = tracker.check_entry(&mint, 50_000, &cfg);
        // Should report gate 1 (insufficient activity), not gate 2/3/4
        assert!(matches!(
            decision,
            ActivityDecision::Reject(ActivityRejectReason::InsufficientActivity { notifs: 1, .. })
        ));
    }

    // ── Serde round-trip ─────────────────────────────────────────────────

    #[test]
    fn test_config_serde_defaults() {
        let json = "{}";
        let cfg: ActivityGateConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.min_ws_notifs, 5);
        assert_eq!(cfg.max_last_trade_age_ms, 2000);
        assert_eq!(cfg.min_buys_3s, 1);
        assert_eq!(cfg.min_price_range_bps, 50);
    }

    #[test]
    fn test_config_serde_override() {
        let json = r#"{"enabled": false, "min_ws_notifs": 10, "min_price_range_bps": 100}"#;
        let cfg: ActivityGateConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_ws_notifs, 10);
        assert_eq!(cfg.max_last_trade_age_ms, 2000); // kept default
        assert_eq!(cfg.min_price_range_bps, 100);
    }
}