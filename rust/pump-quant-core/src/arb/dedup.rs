//! Migration event deduplicator.
//!
//! DashMap-based dedup for migration events arriving from multiple sources
//! (Helius logsSubscribe, CoreCast stream 2). Uses 32-byte mint as key,
//! tracks first detection source + timestamp. Evicts stale entries (>TTL)
//! on insert to prevent unbounded growth.

use std::sync::Arc;

use dashmap::DashMap;

use crate::feeds::MigrationSource;

/// Metadata stored per deduplicated migration event.
#[derive(Debug, Clone, Copy)]
pub struct DedupEntry {
    /// Epoch millisecond timestamp of first detection.
    pub detected_at_ms: u64,
    /// Feed source that detected the migration first.
    pub source: MigrationSource,
}

/// Thread-safe migration event deduplicator.
///
/// Concurrent access via `DashMap` sharding — no global lock.
/// Evicts stale entries (older than `ttl_ms`) on every `try_insert` call,
/// keeping memory bounded to the active migration window (~10-30 entries max).
pub struct MigrationDedup {
    map: Arc<DashMap<[u8; 32], DedupEntry>>,
    ttl_ms: u64,
}

impl MigrationDedup {
    /// Create a new deduplicator with the given TTL window in milliseconds.
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            map: Arc::new(DashMap::with_capacity(32)),
            ttl_ms,
        }
    }

    /// Attempt to register a new migration event.
    ///
    /// Returns `Some(DedupEntry)` if this is the **first** detection of this mint
    /// within the TTL window. Returns `None` if already seen (duplicate from
    /// a second source or repeated delivery).
    ///
    /// Also evicts stale entries older than TTL on every call to prevent
    /// unbounded memory growth.
    #[inline(always)]
    pub fn try_insert(
        &self,
        mint: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
    ) -> Option<DedupEntry> {
        self.evict_stale(ts_ms);

        // Check if already present and still within TTL
        if let Some(existing) = self.map.get(&mint) {
            if ts_ms.saturating_sub(existing.detected_at_ms) < self.ttl_ms {
                return None; // duplicate
            }
            // Entry expired — drop the ref so we can overwrite
            drop(existing);
        }

        // Try to insert; if another thread raced us, the first writer wins
        let entry = DedupEntry {
            detected_at_ms: ts_ms,
            source,
        };
        match self.map.entry(mint) {
            dashmap::mapref::entry::Entry::Occupied(occ) => {
                // Re-check: could have been inserted between our get and entry call
                if ts_ms.saturating_sub(occ.get().detected_at_ms) < self.ttl_ms {
                    None
                } else {
                    // Expired entry — overwrite
                    let mut occ = occ;
                    occ.insert(entry);
                    Some(entry)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(vac) => {
                vac.insert(entry);
                Some(entry)
            }
        }
    }

    /// Evict all entries older than TTL. O(n) scan via `retain()` —
    /// cheap for the expected 10-30 active entries.
    fn evict_stale(&self, now_ms: u64) {
        self.map
            .retain(|_mint, entry| now_ms.saturating_sub(entry.detected_at_ms) < self.ttl_ms);
    }

    /// Current number of tracked (non-evicted) entries. For diagnostics.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
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
    fn evict_stale_removes_expired_entries() {
        let dedup = MigrationDedup::new(5_000);

        let mint_a = [10u8; 32];
        let mint_b = [20u8; 32];
        let mint_c = [30u8; 32];

        dedup.try_insert(mint_a, 1000, MigrationSource::HeliusLogs);
        dedup.try_insert(mint_b, 3000, MigrationSource::CoreCastStream2);
        assert_eq!(dedup.len(), 2);

        // Insert mint_c at t=7000 — mint_a (t=1000) should be evicted (>5s old)
        // mint_b (t=3000) should survive (only 4s old)
        dedup.try_insert(mint_c, 7000, MigrationSource::HeliusLogs);
        assert_eq!(dedup.len(), 2); // mint_b + mint_c; mint_a evicted
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
}
