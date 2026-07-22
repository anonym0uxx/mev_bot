//! Leaf `ex_blockhash_cache`: blockhash validity-window logic.
//!
//! Models the `BlockhashCache` the legacy `sell_engine.rs` consulted before
//! building a TX (`blockhash_cache.get_sync()` returning `None` when
//! empty/stale). A Solana `recentBlockhash` is only accepted by the runtime for
//! a bounded number of slots after the block it names; past that window the TX
//! is rejected as expired. This leaf captures that window check with pure
//! integer slot arithmetic.
//!
//! ## Responsibility
//! Decide whether a cached blockhash is still within its validity window given
//! the current slot, and provide a tiny cache state type wrapping that check.
//!
//! ## Constitution refs
//! - §22: integer slots only (`u64`).
//! - Overflow: age uses `saturating_sub`, so a current slot behind the cached
//!   slot yields age `0` (treated as valid) rather than underflowing.

/// The Solana default: a blockhash is valid for ~150 slots after its block.
pub const DEFAULT_MAX_AGE_SLOTS: u64 = 150;

/// Return whether a blockhash cached at `cached_slot` is still valid at
/// `cur_slot`, given a maximum age of `max_age_slots`.
///
/// Valid iff `cur_slot - cached_slot <= max_age_slots`. If `cur_slot` is behind
/// `cached_slot` (e.g. a lagging RPC read), the age saturates to `0`, so the
/// blockhash is considered valid.
pub fn blockhash_valid(cached_slot: u64, cur_slot: u64, max_age_slots: u64) -> bool {
    cur_slot.saturating_sub(cached_slot) <= max_age_slots
}

/// Minimal cached-blockhash state: the 32-byte hash plus the slot at which it
/// was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockhashCache {
    /// The cached recent blockhash (32 raw bytes).
    pub blockhash: [u8; 32],
    /// The slot at which this blockhash was observed / cached.
    pub cached_slot: u64,
}

impl BlockhashCache {
    /// Create a cache entry for `blockhash` observed at `cached_slot`.
    pub fn new(blockhash: [u8; 32], cached_slot: u64) -> Self {
        Self {
            blockhash,
            cached_slot,
        }
    }

    /// Whether the cached blockhash is still valid at `cur_slot` under
    /// `max_age_slots`. Thin wrapper over [`blockhash_valid`].
    pub fn is_valid(&self, cur_slot: u64, max_age_slots: u64) -> bool {
        blockhash_valid(self.cached_slot, cur_slot, max_age_slots)
    }

    /// Whether the cached blockhash is valid at `cur_slot` under the Solana
    /// default window ([`DEFAULT_MAX_AGE_SLOTS`]).
    pub fn is_valid_default(&self, cur_slot: u64) -> bool {
        self.is_valid(cur_slot, DEFAULT_MAX_AGE_SLOTS)
    }

    /// Replace the cached hash and slot (called when the blockhash updater
    /// observes a fresher blockhash).
    pub fn update(&mut self, blockhash: [u8; 32], cached_slot: u64) {
        self.blockhash = blockhash;
        self.cached_slot = cached_slot;
    }

    /// Remaining slots before this blockhash expires under `max_age_slots`.
    /// Returns `0` once the window has been fully consumed.
    pub fn slots_remaining(&self, cur_slot: u64, max_age_slots: u64) -> u64 {
        let age = cur_slot.saturating_sub(self.cached_slot);
        max_age_slots.saturating_sub(age)
    }
}
