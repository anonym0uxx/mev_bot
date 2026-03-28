//! Lock-free latency histogram for the hot-path decision loop.
//!
//! Zero allocation on record(). Uses `AtomicU64` with `Relaxed` ordering
//! for maximum throughput — exact consistency not required for monitoring.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bucket thresholds in nanoseconds.
const BUCKET_THRESHOLDS_NS: [u64; 7] = [
    1_000,       // < 1µs
    5_000,       // < 5µs
    10_000,      // < 10µs
    50_000,      // < 50µs
    100_000,     // < 100µs
    200_000,     // < 200µs
    500_000,     // < 500µs
];

/// Human-readable labels for each bucket.
const BUCKET_LABELS: [&str; 8] = [
    "<1µs",
    "<5µs",
    "<10µs",
    "<50µs",
    "<100µs",
    "<200µs",
    "<500µs",
    ">=500µs",
];

/// Tracks latency percentiles for the hot-path decision loop.
///
/// Buckets: <1µs, <5µs, <10µs, <50µs, <100µs, <200µs, <500µs, >=500µs
///
/// All operations are lock-free and zero-allocation.
pub struct LatencyTracker {
    buckets: [AtomicU64; 8],
    total: AtomicU64,
}

impl LatencyTracker {
    /// Create a new zeroed tracker. Usable in `static` context.
    pub const fn new() -> Self {
        Self {
            buckets: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            total: AtomicU64::new(0),
        }
    }

    /// Record a latency observation in nanoseconds.
    ///
    /// Call with elapsed nanos from `quanta` or `std::time::Instant`.
    /// Zero allocation, single atomic increment.
    #[inline]
    pub fn record(&self, nanos: u64) {
        let bucket_idx = match nanos {
            n if n < BUCKET_THRESHOLDS_NS[0] => 0,
            n if n < BUCKET_THRESHOLDS_NS[1] => 1,
            n if n < BUCKET_THRESHOLDS_NS[2] => 2,
            n if n < BUCKET_THRESHOLDS_NS[3] => 3,
            n if n < BUCKET_THRESHOLDS_NS[4] => 4,
            n if n < BUCKET_THRESHOLDS_NS[5] => 5,
            n if n < BUCKET_THRESHOLDS_NS[6] => 6,
            _ => 7,
        };
        self.buckets[bucket_idx].fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// Return the bucket label containing the p99 latency.
    ///
    /// Walks buckets from lowest to highest, accumulating counts until
    /// we reach 99% of total. Returns the label of that bucket.
    pub fn p99_bucket(&self) -> &'static str {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return BUCKET_LABELS[0];
        }

        let threshold = (total as f64 * 0.99).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= threshold {
                return BUCKET_LABELS[i];
            }
        }

        BUCKET_LABELS[7]
    }

    /// Human-readable summary of all buckets.
    pub fn summary(&self) -> String {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return "LatencyTracker: 0 samples".to_string();
        }

        let mut parts = Vec::with_capacity(9);
        parts.push(format!("total={}", total));

        for (i, bucket) in self.buckets.iter().enumerate() {
            let count = bucket.load(Ordering::Relaxed);
            if count > 0 {
                let pct = count as f64 / total as f64 * 100.0;
                parts.push(format!("{}: {} ({:.1}%)", BUCKET_LABELS[i], count, pct));
            }
        }

        parts.push(format!("p99={}", self.p99_bucket()));

        parts.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tracker() {
        let t = LatencyTracker::new();
        assert_eq!(t.p99_bucket(), "<1µs");
        assert!(t.summary().contains("0 samples"));
    }

    #[test]
    fn single_record() {
        let t = LatencyTracker::new();
        t.record(500); // 500ns → <1µs bucket
        assert_eq!(t.p99_bucket(), "<1µs");
        assert_eq!(t.total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bucket_boundaries() {
        let t = LatencyTracker::new();

        t.record(999);     // <1µs
        t.record(1_000);   // <5µs (exactly at boundary → next bucket)
        t.record(4_999);   // <5µs
        t.record(5_000);   // <10µs
        t.record(50_000);  // <100µs
        t.record(500_000); // >=500µs

        assert_eq!(t.buckets[0].load(Ordering::Relaxed), 1); // <1µs
        assert_eq!(t.buckets[1].load(Ordering::Relaxed), 2); // <5µs
        assert_eq!(t.buckets[2].load(Ordering::Relaxed), 1); // <10µs
        assert_eq!(t.buckets[4].load(Ordering::Relaxed), 1); // <100µs
        assert_eq!(t.buckets[7].load(Ordering::Relaxed), 1); // >=500µs
        assert_eq!(t.total.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn p99_with_distribution() {
        let t = LatencyTracker::new();

        // 99 samples in <1µs, 1 sample in <200µs
        for _ in 0..99 {
            t.record(500);
        }
        t.record(150_000); // <200µs

        // p99 should be <1µs (99% of 100 = 99 samples, all in first bucket)
        assert_eq!(t.p99_bucket(), "<1µs");
    }

    #[test]
    fn p99_when_spread() {
        let t = LatencyTracker::new();

        // 50 in <1µs, 49 in <5µs, 1 in >=500µs
        for _ in 0..50 {
            t.record(500);
        }
        for _ in 0..49 {
            t.record(3_000);
        }
        t.record(1_000_000);

        // p99 threshold = ceil(100 * 0.99) = 99
        // cumulative after bucket 0: 50
        // cumulative after bucket 1: 99 → p99 is <5µs
        assert_eq!(t.p99_bucket(), "<5µs");
    }

    #[test]
    fn summary_format() {
        let t = LatencyTracker::new();
        t.record(500);
        t.record(3_000);
        let s = t.summary();
        assert!(s.contains("total=2"));
        assert!(s.contains("<1µs"));
        assert!(s.contains("p99="));
    }

    #[test]
    fn const_new() {
        // Verify LatencyTracker can be used in static context
        static TRACKER: LatencyTracker = LatencyTracker::new();
        TRACKER.record(100);
        assert_eq!(TRACKER.total.load(Ordering::Relaxed), 1);
    }
}
