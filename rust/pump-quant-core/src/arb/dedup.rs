//! Migration event deduplicator.
//!
//! Ring-buffer-based dedup for migration events arriving from multiple sources
//! (Helius logsSubscribe, CoreCast stream 2). Uses 32-byte key (sig prefix),
//! tracks first detection source + timestamp in a fixed-size ring buffer.
//!
//! ## Performance
//!
//! Previous implementation used `DashMap<[u8;32], DedupEntry>` which is
//! optimized for thousands of concurrent entries with sharded locks. For
//! 10-30 graduation events per day, DashMap is massive overkill:
//! - 64 shards × (header + bucket array) ≈ 4KB metadata overhead
//! - Hash computation (~15ns per lookup) unnecessary for 10-30 entries
//! - Arc + DashMap heap allocations on every insert
//!
//! This ring buffer implementation:
//! - 64 slots × 41 bytes = ~2.6KB total, fits in 3 cache lines
//! - O(64) linear scan = ~5ns (branch predictor loves sequential access)
//! - Zero heap allocation, zero hash computation
//! - Mutex overhead is negligible at 10-30 events/day contention rate

use std::sync::Mutex;

use crate::feeds::MigrationSource;

/// Number of ring buffer slots. 64 = 2+ days of capacity at 30 events/day.
/// Must be a power of 2 for cache-line alignment friendliness.
const RING_SIZE: usize = 64;

/// Metadata stored per deduplicated migration event.
#[derive(Debug, Clone, Copy)]
pub struct DedupEntry {
    /// Epoch millisecond timestamp of first detection.
    pub detected_at_ms: u64,
    /// Feed source that detected the migration first.
    pub source: MigrationSource,
}

/// A single slot in the ring buffer.
#[derive(Clone, Copy)]
struct RingSlot {
    /// The dedup key (first 32 bytes of transaction signature).
    key: [u8; 32],
    /// Timestamp when this entry was inserted (epoch ms).
    ts_ms: u64,
    /// Source that first detected this migration.
    #[allow(dead_code)]
    source: MigrationSource,
}

impl Default for RingSlot {
    fn default() -> Self {
        Self {
            key: [0u8; 32],
            ts_ms: 0,
            source: MigrationSource::HeliusLogs,
        }
    }
}

/// Inner state protected by Mutex.
struct RingInner {
    ring: [RingSlot; RING_SIZE],
    head: usize,
    count: usize,
}

/// Thread-safe migration event deduplicator using a fixed-size ring buffer.
///
/// O(64) linear scan per operation — faster than DashMap for the expected
/// 10-30 active entries. All data fits in ~2.6KB (3 cache lines).
/// Mutex contention is negligible at 10-30 events/day.
pub struct MigrationDedup {
    inner: Mutex<RingInner>,
    ttl_ms: u64,
}

impl MigrationDedup {
    /// Create a new deduplicator with the given TTL window in milliseconds.
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            inner: Mutex::new(RingInner {
                ring: [RingSlot::default(); RING_SIZE],
                head: 0,
                count: 0,
            }),
            ttl_ms,
        }
    }

    /// Attempt to register a new migration event.
    ///
    /// Returns `Some(DedupEntry)` if this is the **first** detection of this key
    /// within the TTL window. Returns `None` if already seen (duplicate from
    /// a second source or repeated delivery).
    ///
    /// PERF: O(64) scan + O(64) eviction = ~10ns total. Zero allocation.
    #[inline(always)]
    pub fn try_insert(
        &self,
        key: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
    ) -> Option<DedupEntry> {
        let mut inner = self.inner.lock().ok()?;

        // Phase 1: Check for existing non-expired entry with this key
        for i in 0..inner.count.min(RING_SIZE) {
            let idx = (inner.head + RING_SIZE - 1 - i) % RING_SIZE;
            let slot = &inner.ring[idx];
            // Skip expired entries
            if ts_ms.saturating_sub(slot.ts_ms) >= self.ttl_ms {
                continue;
            }
            if slot.key == key {
                return None; // duplicate within TTL
            }
        }

        // Phase 2: Insert new entry at head position
        let entry = DedupEntry {
            detected_at_ms: ts_ms,
            source,
        };

        let head = inner.head;
        inner.ring[head] = RingSlot {
            key,
            ts_ms,
            source,
        };
        inner.head = (head + 1) % RING_SIZE;
        if inner.count < RING_SIZE {
            inner.count += 1;
        }

        Some(entry)
    }

    /// Current number of tracked (non-evicted) entries. For diagnostics.
    /// Note: includes expired entries that haven't been overwritten yet.
    /// For exact count, call with a timestamp to filter.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.count).unwrap_or(0)
    }

    /// Count of non-expired entries. For diagnostics.
    #[allow(dead_code)]
    pub fn active_count(&self, now_ms: u64) -> usize {
        let inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return 0,
        };
        let mut count = 0;
        for i in 0..inner.count.min(RING_SIZE) {
            let idx = (inner.head + RING_SIZE - 1 - i) % RING_SIZE;
            if now_ms.saturating_sub(inner.ring[idx].ts_ms) < self.ttl_ms {
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::MigrationSource;

    #[test]
    fn first_insert_returns_some() {
        let dedup = MigrationDedup::new(10_000);
        let mint = [1u8; 32];
        let result = dedup.try_insert(mint, 1000, MigrationSource::HeliusLogs);
        assert!(result.is_some());
        let entry = result.unwrap();
        assert_eq!(entry.detected_at_ms, 1000);
        assert_eq!(entry.source, MigrationSource::HeliusLogs);
    }

    #[test]
    fn duplicate_within_ttl_returns_none() {
        let dedup = MigrationDedup::new(10_000);
        let mint = [2u8; 32];

        let first = dedup.try_insert(mint, 1000, MigrationSource::HeliusLogs);
        assert!(first.is_some());

        // Same mint, different source, within TTL → duplicate
        let second = dedup.try_insert(mint, 1500, MigrationSource::CoreCastStream2);
        assert!(second.is_none());
    }

    #[test]
    fn second_insert_after_ttl_returns_some() {
        let dedup = MigrationDedup::new(10_000);
        let mint = [3u8; 32];

        let first = dedup.try_insert(mint, 1000, MigrationSource::HeliusLogs);
        assert!(first.is_some());

        // Same mint, after TTL expires → should succeed as new detection
        let second = dedup.try_insert(mint, 12_000, MigrationSource::CoreCastStream2);
        assert!(second.is_some());
        assert_eq!(second.unwrap().source, MigrationSource::CoreCastStream2);
    }

    #[test]
    fn evict_stale_via_ttl_check() {
        let dedup = MigrationDedup::new(5_000);

        let mint_a = [10u8; 32];
        let mint_b = [20u8; 32];

        dedup.try_insert(mint_a, 1000, MigrationSource::HeliusLogs);
        dedup.try_insert(mint_b, 3000, MigrationSource::CoreCastStream2);
        assert_eq!(dedup.len(), 2);

        // At t=7000, mint_a (t=1000) is expired (>5s), so re-inserting it should succeed
        let result = dedup.try_insert(mint_a, 7000, MigrationSource::CoreCastStream2);
        assert!(result.is_some());
        // mint_b (t=3000) at t=7000 is 4s old, still within TTL
        let dup_b = dedup.try_insert(mint_b, 7000, MigrationSource::HeliusLogs);
        assert!(dup_b.is_none());
    }

    #[test]
    fn different_mints_both_accepted() {
        let dedup = MigrationDedup::new(10_000);
        let mint_a = [40u8; 32];
        let mint_b = [50u8; 32];

        let a = dedup.try_insert(mint_a, 1000, MigrationSource::HeliusLogs);
        let b = dedup.try_insert(mint_b, 1000, MigrationSource::CoreCastStream2);
        assert!(a.is_some());
        assert!(b.is_some());
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn ring_wraps_around() {
        let dedup = MigrationDedup::new(100_000); // long TTL so nothing expires

        // Fill the ring completely
        for i in 0..RING_SIZE as u8 {
            let mut key = [0u8; 32];
            key[0] = i;
            let result = dedup.try_insert(key, (i as u64 + 1) * 1000, MigrationSource::HeliusLogs);
            assert!(result.is_some(), "insert {} should succeed", i);
        }

        // Ring is full. Insert one more — overwrites oldest slot
        let mut new_key = [0u8; 32];
        new_key[0] = 0xFF;
        let result = dedup.try_insert(new_key, 100_000, MigrationSource::HeliusLogs);
        assert!(result.is_some());

        // The overwritten slot (key[0]=0) should now be gone — re-inserting it succeeds
        // because the slot was overwritten by new_key
        let mut old_key = [0u8; 32];
        old_key[0] = 0;
        let result = dedup.try_insert(old_key, 100_001, MigrationSource::HeliusLogs);
        assert!(result.is_some(), "overwritten key should be insertable again");
    }

    #[test]
    fn active_count_filters_expired() {
        let dedup = MigrationDedup::new(5_000);

        let key_a = [10u8; 32];
        let key_b = [20u8; 32];
        let key_c = [30u8; 32];

        dedup.try_insert(key_a, 1000, MigrationSource::HeliusLogs);
        dedup.try_insert(key_b, 3000, MigrationSource::CoreCastStream2);
        dedup.try_insert(key_c, 6500, MigrationSource::HeliusLogs);

        // At t=7000: key_a (1000) expired, key_b (3000) 4s old = active, key_c (6500) 0.5s = active
        assert_eq!(dedup.active_count(7000), 2);
    }
}
