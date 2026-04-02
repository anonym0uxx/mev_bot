//! Priority-aware token bucket rate limiter for RPC endpoints.
//!
//! Ensures `sendTransaction` always has headroom by throttling lower-priority
//! reads when approaching rate limits. Each endpoint gets its own limiter.
//!
//! # Design
//!
//! Token bucket with three priority tiers sharing a global budget per endpoint.
//! Uses `AtomicU64` for lock-free token tracking on the hot path.
//! Token refill is lazy — computed from `Instant` delta on every `acquire()`,
//! no background refill task needed.
//!
//! # Priority Tiers
//!
//! - **Critical** (`sendTransaction`): never blocked, can overdraft the bucket.
//! - **Normal** (price feeds): waits up to a configurable timeout for tokens.
//! - **Background** (pool resolution): immediately shed if no tokens available.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

// ── Priority ─────────────────────────────────────────────────────────────────

/// Priority tier for RPC calls. Determines acquire behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    /// `sendTransaction`, `getSignatureStatuses` — never throttled, can overdraft.
    Critical,
    /// Price feed `getAccountInfo`, `getLatestBlockhash` — waits for tokens.
    Normal,
    /// Pool resolution reads — shed immediately if no tokens available.
    Background,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::Critical => write!(f, "critical"),
            Priority::Normal => write!(f, "normal"),
            Priority::Background => write!(f, "background"),
        }
    }
}

// ── Acquire Result ───────────────────────────────────────────────────────────

/// Result of attempting to acquire a rate limiter token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// Normal-priority call timed out waiting for a token.
    RateLimited,
    /// Background-priority call shed — no tokens available.
    Shed,
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::RateLimited => write!(f, "rate limited (timeout)"),
            AcquireError::Shed => write!(f, "shed (no tokens)"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// Convenience alias.
pub type AcquireResult = Result<(), AcquireError>;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a single rate limiter instance.
///
/// Designed to be loaded from `canary.json` (or constructed in code).
///
/// ```json
/// {
///   "tokens_per_sec": 20.0,
///   "burst_capacity": 25,
///   "normal_wait_timeout_ms": 2000
/// }
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimiterConfig {
    /// Sustained token refill rate (tokens per second).
    #[serde(default = "default_tokens_per_sec")]
    pub tokens_per_sec: f64,

    /// Maximum tokens the bucket can hold (burst capacity).
    #[serde(default = "default_burst_capacity")]
    pub burst_capacity: u64,

    /// Maximum time (ms) a Normal-priority caller will wait for a token.
    /// After this, returns `Err(RateLimited)`.
    #[serde(default = "default_normal_wait_timeout_ms")]
    pub normal_wait_timeout_ms: u64,
}

fn default_tokens_per_sec() -> f64 {
    20.0
}
fn default_burst_capacity() -> u64 {
    25
}
fn default_normal_wait_timeout_ms() -> u64 {
    2000
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            tokens_per_sec: default_tokens_per_sec(),
            burst_capacity: default_burst_capacity(),
            normal_wait_timeout_ms: default_normal_wait_timeout_ms(),
        }
    }
}

// ── Internal: fixed-point scaling ────────────────────────────────────────────

/// We store tokens × SCALE for sub-token precision without floating point.
/// 1 token = 1_000_000 internal units. This gives microsecond-level refill
/// granularity while staying within u64 range (max ~18.4 × 10^12 tokens).
const SCALE: u64 = 1_000_000;

/// Convert a whole-token count to scaled internal representation.
#[inline(always)]
const fn to_scaled(tokens: u64) -> u64 {
    tokens * SCALE
}

/// Convert scaled internal representation back to whole-token count (truncated).
#[inline(always)]
const fn from_scaled(scaled: u64) -> u64 {
    scaled / SCALE
}

// ── Epoch (for Instant → AtomicU64 storage) ─────────────────────────────────

/// A shared reference epoch so we can store `Instant` deltas as `u64` nanos
/// in atomics. `Instant` itself isn't `Copy`-into-atomic-friendly.
///
/// Thread-safe: initialized once via `std::sync::OnceLock`.
fn epoch() -> Instant {
    use std::sync::OnceLock;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Convert `Instant` to nanoseconds since epoch (for atomic storage).
#[inline(always)]
fn instant_to_nanos(t: Instant) -> u64 {
    t.duration_since(epoch()).as_nanos() as u64
}

/// Get current time as nanos since epoch.
#[inline(always)]
fn now_nanos() -> u64 {
    instant_to_nanos(Instant::now())
}

// ── Rate Limiter ─────────────────────────────────────────────────────────────

/// Per-endpoint token bucket rate limiter with priority-aware acquisition.
///
/// Thread-safe (`Send + Sync`). The hot path uses only atomic operations —
/// no mutexes, no async locks.
///
/// # Token Refill
///
/// Tokens are refilled lazily on each `acquire()` call by computing the
/// elapsed time since the last refill and adding `elapsed × tokens_per_sec`.
/// This avoids the need for a background refill task.
///
/// # Overdraft (Critical)
///
/// Critical-priority callers can push the token count below zero (tracked as
/// `overdraft_tokens`). The bucket will naturally recover as tokens refill,
/// but the overdraft counter lets operators observe when the system is
/// exceeding its rate budget for critical work.
pub struct RateLimiter {
    /// Available tokens × SCALE. Atomically decremented on acquire, incremented on refill.
    tokens: AtomicU64,
    /// Maximum tokens × SCALE (burst capacity).
    capacity: u64,
    /// Tokens added per nanosecond × SCALE. Pre-computed from `tokens_per_sec`.
    refill_per_nano: f64,
    /// Last refill timestamp (nanos since epoch). CAS'd to prevent double-refill.
    last_refill_nanos: AtomicU64,
    /// Maximum wait for Normal-priority callers.
    normal_wait_timeout: Duration,
    /// Stats: total tokens acquired (all priorities).
    stats_acquired: AtomicU64,
    /// Stats: total Normal calls that had to wait.
    stats_waited: AtomicU64,
    /// Stats: total Background calls shed.
    stats_shed: AtomicU64,
    /// Stats: total Critical overdraft events.
    stats_overdraft: AtomicU64,
}

// SAFETY: All fields are either atomic or immutable after construction.
unsafe impl Send for RateLimiter {}
unsafe impl Sync for RateLimiter {}

impl RateLimiter {
    /// Create a new rate limiter from config.
    ///
    /// Starts with a full bucket (burst capacity tokens available).
    pub fn new(config: &RateLimiterConfig) -> Self {
        assert!(config.tokens_per_sec > 0.0, "tokens_per_sec must be > 0");
        assert!(config.burst_capacity > 0, "burst_capacity must be > 0");

        let capacity = to_scaled(config.burst_capacity);
        let refill_per_nano = (config.tokens_per_sec * SCALE as f64) / 1_000_000_000.0;

        Self {
            tokens: AtomicU64::new(capacity),
            capacity,
            refill_per_nano,
            last_refill_nanos: AtomicU64::new(now_nanos()),
            normal_wait_timeout: Duration::from_millis(config.normal_wait_timeout_ms),
            stats_acquired: AtomicU64::new(0),
            stats_waited: AtomicU64::new(0),
            stats_shed: AtomicU64::new(0),
            stats_overdraft: AtomicU64::new(0),
        }
    }

    /// Create a rate limiter for testing with a specific initial token count.
    #[cfg(test)]
    fn with_tokens(config: &RateLimiterConfig, initial_tokens: u64) -> Self {
        let mut limiter = Self::new(config);
        limiter.tokens = AtomicU64::new(to_scaled(initial_tokens));
        limiter
    }

    // ── Refill ───────────────────────────────────────────────────────────

    /// Refill tokens based on elapsed time since last refill.
    ///
    /// Lock-free CAS loop. Only one thread wins the refill race per interval;
    /// losers observe the updated token count on their next load.
    fn refill(&self) {
        let now = now_nanos();
        let prev = self.last_refill_nanos.load(Ordering::Acquire);
        let elapsed_nanos = now.saturating_sub(prev);

        if elapsed_nanos == 0 {
            return;
        }

        // CAS on last_refill_nanos: exactly one thread wins the refill.
        if self
            .last_refill_nanos
            .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            // Another thread won — they'll add the tokens.
            return;
        }

        let add = (elapsed_nanos as f64 * self.refill_per_nano) as u64;
        if add == 0 {
            return;
        }

        // CAS loop to add tokens, capping at capacity.
        let mut current = self.tokens.load(Ordering::Acquire);
        loop {
            let new_val = (current.saturating_add(add)).min(self.capacity);
            match self.tokens.compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    // ── Try Consume ──────────────────────────────────────────────────────

    /// Try to consume one token. Returns `true` if successful.
    ///
    /// Lock-free CAS loop. Returns `false` if fewer than 1 token available.
    fn try_consume(&self) -> bool {
        let cost = SCALE; // 1 token
        let mut current = self.tokens.load(Ordering::Acquire);
        loop {
            if current < cost {
                return false;
            }
            match self.tokens.compare_exchange_weak(
                current,
                current - cost,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Force-consume one token, allowing overdraft (for Critical priority).
    ///
    /// If there aren't enough tokens, saturates at 0 rather than wrapping.
    fn force_consume(&self) {
        let cost = SCALE;
        let mut current = self.tokens.load(Ordering::Acquire);
        loop {
            let new_val = current.saturating_sub(cost);
            match self.tokens.compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if current < cost {
                        // We overdrafted.
                        self.stats_overdraft.fetch_add(1, Ordering::Relaxed);
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    // ── Public API ───────────────────────────────────────────────────────

    /// Acquire a rate limiter token with priority-aware behavior.
    ///
    /// - **Critical**: always returns `Ok(())` immediately. If the bucket is
    ///   empty, force-consumes (overdraft). `sendTransaction` must never block.
    ///
    /// - **Normal**: if a token is available, consumes it and returns `Ok(())`.
    ///   If not, waits up to `normal_wait_timeout_ms` for refill. Returns
    ///   `Err(RateLimited)` if timeout expires without a token.
    ///
    /// - **Background**: if a token is available, consumes it and returns
    ///   `Ok(())`. If not, returns `Err(Shed)` immediately (non-blocking).
    ///   Pool resolution calls are expendable.
    pub async fn acquire(&self, priority: Priority) -> AcquireResult {
        self.refill();

        match priority {
            Priority::Critical => {
                if !self.try_consume() {
                    // Overdraft: force-consume so we track it, but never block.
                    self.force_consume();
                    tracing::debug!("[rate_limiter] critical overdraft — bucket empty");
                }
                self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }

            Priority::Normal => {
                // Fast path: try immediate consume.
                if self.try_consume() {
                    self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }

                // Slow path: wait for refill up to timeout.
                self.stats_waited.fetch_add(1, Ordering::Relaxed);

                let deadline = Instant::now() + self.normal_wait_timeout;
                loop {
                    // How long until 1 token refills?
                    // 1 token = SCALE units. Time = SCALE / refill_per_nano nanos.
                    let wait_nanos =
                        (SCALE as f64 / self.refill_per_nano).min(50_000_000.0) as u64; // cap at 50ms per tick
                    let wait = Duration::from_nanos(wait_nanos.max(1_000_000)); // min 1ms

                    if Instant::now() + wait > deadline {
                        // Would exceed timeout — give up.
                        tracing::debug!(
                            timeout_ms = self.normal_wait_timeout.as_millis() as u64,
                            "[rate_limiter] normal priority timed out"
                        );
                        return Err(AcquireError::RateLimited);
                    }

                    tokio::time::sleep(wait).await;
                    self.refill();

                    if self.try_consume() {
                        self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                }
            }

            Priority::Background => {
                // Non-blocking: either get a token now or shed.
                if self.try_consume() {
                    self.stats_acquired.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                } else {
                    self.stats_shed.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("[rate_limiter] background shed — no tokens");
                    Err(AcquireError::Shed)
                }
            }
        }
    }

    // ── Observability ────────────────────────────────────────────────────

    /// Current number of available tokens (whole tokens, not scaled).
    pub fn available_tokens(&self) -> u64 {
        self.refill();
        from_scaled(self.tokens.load(Ordering::Relaxed))
    }

    /// Current fill level as a fraction in `[0.0, 1.0]`.
    pub fn fill_level(&self) -> f64 {
        self.refill();
        let current = self.tokens.load(Ordering::Relaxed);
        current as f64 / self.capacity as f64
    }

    /// Snapshot of rate limiter statistics.
    pub fn stats(&self) -> RateLimiterStats {
        self.refill();
        RateLimiterStats {
            tokens_available: from_scaled(self.tokens.load(Ordering::Relaxed)),
            capacity: from_scaled(self.capacity),
            fill_pct: self.fill_level() * 100.0,
            total_acquired: self.stats_acquired.load(Ordering::Relaxed),
            total_waited: self.stats_waited.load(Ordering::Relaxed),
            total_shed: self.stats_shed.load(Ordering::Relaxed),
            total_overdraft: self.stats_overdraft.load(Ordering::Relaxed),
        }
    }
}

// ── Stats ────────────────────────────────────────────────────────────────────

/// Point-in-time snapshot of rate limiter statistics.
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    /// Currently available tokens.
    pub tokens_available: u64,
    /// Maximum capacity (burst).
    pub capacity: u64,
    /// Fill percentage (0–100).
    pub fill_pct: f64,
    /// Total tokens successfully acquired.
    pub total_acquired: u64,
    /// Total Normal-priority calls that had to wait.
    pub total_waited: u64,
    /// Total Background-priority calls shed.
    pub total_shed: u64,
    /// Total Critical-priority overdraft events.
    pub total_overdraft: u64,
}

impl std::fmt::Display for RateLimiterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tokens={}/{} ({:.0}%) acquired={} waited={} shed={} overdraft={}",
            self.tokens_available,
            self.capacity,
            self.fill_pct,
            self.total_acquired,
            self.total_waited,
            self.total_shed,
            self.total_overdraft,
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: config with fast refill for testing.
    fn test_config(tokens_per_sec: f64, burst: u64) -> RateLimiterConfig {
        RateLimiterConfig {
            tokens_per_sec,
            burst_capacity: burst,
            normal_wait_timeout_ms: 500,
        }
    }

    // ── 1. Basic token consumption ───────────────────────────────────────

    #[tokio::test]
    async fn test_basic_consumption() {
        let cfg = test_config(100.0, 5);
        let rl = RateLimiter::new(&cfg);

        // Should be able to consume burst_capacity tokens immediately.
        for _ in 0..5 {
            assert!(rl.try_consume(), "should have tokens available");
        }
        // 6th should fail — bucket empty.
        assert!(!rl.try_consume(), "bucket should be empty");
    }

    // ── 2. Token refill over time ────────────────────────────────────────

    #[tokio::test]
    async fn test_refill_over_time() {
        let cfg = test_config(100.0, 10); // 100 tokens/sec = 1 token per 10ms
        let rl = RateLimiter::with_tokens(&cfg, 0);

        // Empty bucket — no tokens.
        assert!(!rl.try_consume());

        // Wait 60ms → should refill ~6 tokens at 100/sec.
        tokio::time::sleep(Duration::from_millis(60)).await;
        rl.refill();

        let available = rl.available_tokens();
        // Allow some tolerance for timing jitter.
        assert!(
            available >= 4 && available <= 8,
            "expected ~6 tokens after 60ms at 100/sec, got {}",
            available,
        );
    }

    // ── 3. Critical priority never blocks and can overdraft ──────────────

    #[tokio::test]
    async fn test_critical_never_blocked() {
        let cfg = test_config(1.0, 2); // Very slow refill.
        let rl = RateLimiter::with_tokens(&cfg, 0); // Start empty.

        // Critical should succeed even with 0 tokens.
        let result = rl.acquire(Priority::Critical).await;
        assert_eq!(result, Ok(()));

        // And again.
        let result = rl.acquire(Priority::Critical).await;
        assert_eq!(result, Ok(()));

        // Stats should show overdraft.
        let stats = rl.stats();
        assert!(stats.total_overdraft >= 2, "expected overdraft, got {}", stats.total_overdraft);
        assert_eq!(stats.total_acquired, 2);
    }

    // ── 4. Background priority sheds when empty ──────────────────────────

    #[tokio::test]
    async fn test_background_shedding() {
        let cfg = test_config(1.0, 2);
        let rl = RateLimiter::with_tokens(&cfg, 1);

        // First Background call: should succeed (1 token available).
        let result = rl.acquire(Priority::Background).await;
        assert_eq!(result, Ok(()));

        // Second Background call: should be shed (0 tokens).
        let result = rl.acquire(Priority::Background).await;
        assert_eq!(result, Err(AcquireError::Shed));

        let stats = rl.stats();
        assert_eq!(stats.total_shed, 1);
        assert_eq!(stats.total_acquired, 1);
    }

    // ── 5. Normal priority waits then succeeds ───────────────────────────

    #[tokio::test]
    async fn test_normal_waits_for_token() {
        // 50 tokens/sec → 1 token per 20ms. Timeout = 500ms.
        let cfg = test_config(50.0, 5);
        let rl = RateLimiter::with_tokens(&cfg, 0); // Start empty.

        let start = Instant::now();
        let result = rl.acquire(Priority::Normal).await;
        let elapsed = start.elapsed();

        assert_eq!(result, Ok(()));
        // Should have waited some time for refill (at least a few ms).
        assert!(
            elapsed.as_millis() >= 5,
            "expected some wait, got {:?}",
            elapsed,
        );
        assert!(
            elapsed.as_millis() < 400,
            "waited too long: {:?}",
            elapsed,
        );

        let stats = rl.stats();
        assert_eq!(stats.total_waited, 1);
    }

    // ── 6. Normal priority times out ─────────────────────────────────────

    #[tokio::test]
    async fn test_normal_timeout() {
        // 0.5 tokens/sec with 200ms timeout = ~0.1 tokens in the window.
        // Bucket starts empty → should timeout.
        let cfg = RateLimiterConfig {
            tokens_per_sec: 0.5,
            burst_capacity: 5,
            normal_wait_timeout_ms: 200,
        };
        let rl = RateLimiter::with_tokens(&cfg, 0);

        let start = Instant::now();
        let result = rl.acquire(Priority::Normal).await;
        let elapsed = start.elapsed();

        assert_eq!(result, Err(AcquireError::RateLimited));
        // Should have waited close to the timeout.
        assert!(
            elapsed.as_millis() >= 150,
            "should have waited near timeout, got {:?}",
            elapsed,
        );
    }

    // ── 7. Burst behavior: can consume up to capacity immediately ────────

    #[tokio::test]
    async fn test_burst_capacity() {
        let cfg = test_config(10.0, 20);
        let rl = RateLimiter::new(&cfg);

        // Consume all 20 tokens in rapid succession.
        let mut consumed = 0u64;
        for _ in 0..25 {
            if rl.try_consume() {
                consumed += 1;
            }
        }
        assert_eq!(consumed, 20, "should consume exactly burst_capacity tokens");
    }

    // ── 8. Concurrent access safety ──────────────────────────────────────

    #[tokio::test]
    async fn test_concurrent_access() {
        use std::sync::Arc;

        let cfg = test_config(1000.0, 50);
        let rl = Arc::new(RateLimiter::new(&cfg));

        // Spawn 20 tasks, each trying to acquire 5 tokens.
        let mut handles = Vec::new();
        for _ in 0..20 {
            let rl = Arc::clone(&rl);
            handles.push(tokio::spawn(async move {
                let mut ok_count = 0u64;
                for _ in 0..5 {
                    if rl.acquire(Priority::Background).await.is_ok() {
                        ok_count += 1;
                    }
                }
                ok_count
            }));
        }

        let mut total_ok = 0u64;
        for h in handles {
            total_ok += h.await.unwrap();
        }

        let stats = rl.stats();

        // Total acquired + shed should equal total attempts (100).
        let total_attempts = stats.total_acquired + stats.total_shed;
        assert_eq!(
            total_attempts, 100,
            "acquired({}) + shed({}) should equal 100 attempts",
            stats.total_acquired, stats.total_shed,
        );
        assert_eq!(total_ok, stats.total_acquired);

        // With 50 burst + 1000/sec refill, most should succeed, but
        // the exact count depends on scheduling. Just verify no panics
        // and accounting is consistent.
        assert!(
            stats.total_acquired >= 50,
            "should acquire at least burst_capacity tokens, got {}",
            stats.total_acquired,
        );
    }

    // ── 9. Refill does not exceed capacity ───────────────────────────────

    #[tokio::test]
    async fn test_refill_capped_at_capacity() {
        let cfg = test_config(10000.0, 10); // Very fast refill.
        let rl = RateLimiter::with_tokens(&cfg, 5);

        // Wait for refill to accumulate.
        tokio::time::sleep(Duration::from_millis(50)).await;
        rl.refill();

        let available = rl.available_tokens();
        assert!(
            available <= 10,
            "tokens should not exceed capacity (10), got {}",
            available,
        );
    }

    // ── 10. Priority ordering: Critical succeeds when Background sheds ──

    #[tokio::test]
    async fn test_priority_ordering() {
        let cfg = test_config(1.0, 1); // 1 token capacity, slow refill.
        let rl = RateLimiter::with_tokens(&cfg, 1);

        // Background takes the last token.
        assert_eq!(rl.acquire(Priority::Background).await, Ok(()));

        // Background is shed (empty).
        assert_eq!(
            rl.acquire(Priority::Background).await,
            Err(AcquireError::Shed),
        );

        // Critical still succeeds (overdraft).
        assert_eq!(rl.acquire(Priority::Critical).await, Ok(()));

        let stats = rl.stats();
        assert_eq!(stats.total_acquired, 2); // 1 Background + 1 Critical
        assert_eq!(stats.total_shed, 1);
        assert!(stats.total_overdraft >= 1);
    }

    // ── 11. Stats display formatting ─────────────────────────────────────

    #[tokio::test]
    async fn test_stats_display() {
        let cfg = test_config(10.0, 10);
        let rl = RateLimiter::new(&cfg);

        let _ = rl.acquire(Priority::Critical).await;
        let _ = rl.acquire(Priority::Normal).await;

        let stats = rl.stats();
        let display = format!("{}", stats);
        assert!(display.contains("acquired=2"), "display: {}", display);
        assert!(display.contains("tokens="), "display: {}", display);
    }

    // ── 12. Config deserialization ───────────────────────────────────────

    #[test]
    fn test_config_deserialize() {
        let json = r#"{
            "tokens_per_sec": 15.0,
            "burst_capacity": 30,
            "normal_wait_timeout_ms": 1000
        }"#;
        let cfg: RateLimiterConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.tokens_per_sec, 15.0);
        assert_eq!(cfg.burst_capacity, 30);
        assert_eq!(cfg.normal_wait_timeout_ms, 1000);
    }

    #[test]
    fn test_config_defaults() {
        let json = r#"{}"#;
        let cfg: RateLimiterConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.tokens_per_sec, 20.0);
        assert_eq!(cfg.burst_capacity, 25);
        assert_eq!(cfg.normal_wait_timeout_ms, 2000);
    }
}