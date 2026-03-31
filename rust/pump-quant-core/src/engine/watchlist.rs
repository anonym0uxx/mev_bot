//! Two-phase entry watchlist — zero-heap, fixed-size, L1-resident.
//!
//! ARCHITECTURE: The entry engine flags a token as RIDE-worthy, but instead of
//! immediately committing capital, we add it to a fixed-size watchlist. Capital
//! is only committed when a **confirming buy** arrives for a watched mint.
//!
//! This eliminates 62% of trades that are "dead on arrival" (buysAfterEntry=0,
//! WR=0%), saving ~1.2 mSOL/trade in wasted fees.
//!
//! PERFORMANCE:
//! - 64 slots × 64 bytes = 4KB (fits in L1 cache)
//! - Lookup: linear scan with u64 mint hash (branch-free comparison)
//! - Insert: O(1) clock-hand eviction
//! - Zero heap allocation, zero String, zero Vec
//!
//! LATENCY BUDGET: <30ns lookup, <20ns insert, <10ns expire check

use super::entry_engine::EntryInput;
use super::kelly_sizing::EntryConviction;
use crate::feeds::TradeEvent;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum watchlist capacity. 64 slots × 64 bytes = 4KB = fits L1.
const WATCHLIST_CAPACITY: usize = 64;

/// Default expiry: 1500ms. Token must receive 2 confirming buys within this
/// window or the watchlist slot is recycled. Reduced from 2000ms — memecoins
/// that don't confirm in 1.5s are statistically dead-on-arrival.
const DEFAULT_EXPIRY_MS: u64 = 1_500;

/// Minimum SOL amount for a confirming buy to trigger promotion (mvsol).
/// Filters out dust buys that don't represent real interest.
const MIN_CONFIRM_MVSOL: u32 = 30; // 0.03 SOL

// ---------------------------------------------------------------------------
// WatchSlot — 64 bytes, 1 cache line, #[repr(C, align(64))]
// ---------------------------------------------------------------------------

/// A single watchlist entry. Stores everything needed to open a position
/// when the confirming buy arrives, so we don't need to re-evaluate.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct WatchSlot {
    /// First 8 bytes of mint as u64 hash (for fast comparison).
    mint_hash: u64,
    /// Full 32-byte mint (needed for open_position).
    /// We overlap with padding — only first 8 bytes are used for lookup.
    /// Full mint stored separately in mint_store.
    watch_start_ms: u64,            // 8-15
    /// Cached entry decision fields (avoid re-evaluation on promote).
    score: u32,                     // 16-19  (f64 × 10000, clamped)
    magnitude: u32,                 // 20-23  (f64 × 10000, clamped)
    size_lamports: u32,             // 24-27  (clamped to u32, max ~4.29 SOL)
    /// EntryConviction fields (packed).
    p_permille: u16,                // 28-29
    r_x100: u16,                    // 30-31
    f_permille: u16,                // 32-33
    conviction_tier: u8,            // 34
    /// State: 0 = empty, 1 = watching, 2 = promoted (consumed), 3 = partial_confirm (awaiting 2nd buy).
    state: u8,                      // 35
    /// vSOL reserves at watch time (for slippage check on promote).
    entry_vsol_reserves: u32,       // 36-39  (mvsol = lamports / 1e6)
    /// Expiry ms (absolute timestamp).
    expiry_ms: u64,                 // 40-47
    /// Original trade signature prefix (dedup).
    sig_prefix: u64,                // 48-55
    /// First confirming buy sig prefix (for 2-buy dedup). Reuses padding.
    confirm1_sig_prefix: [u8; 8],   // 56-63
}

const _: () = assert!(core::mem::size_of::<WatchSlot>() == 64);

impl WatchSlot {
    const EMPTY: Self = WatchSlot {
        mint_hash: 0,
        watch_start_ms: 0,
        score: 0,
        magnitude: 0,
        size_lamports: 0,
        p_permille: 0,
        r_x100: 0,
        f_permille: 0,
        conviction_tier: 0,
        state: 0,
        entry_vsol_reserves: 0,
        expiry_ms: 0,
        sig_prefix: 0,
        confirm1_sig_prefix: [0; 8],
    };

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.state == 0
    }

    #[inline(always)]
    fn is_watching(&self) -> bool {
        self.state == 1 || self.state == 3
    }

    #[inline(always)]
    fn is_partial_confirm(&self) -> bool {
        self.state == 3
    }

    #[inline(always)]
    fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expiry_ms
    }
}

// ---------------------------------------------------------------------------
// Mint store — separate array for full 32-byte mints
// ---------------------------------------------------------------------------

/// Full mint addresses stored separately to keep WatchSlot at 64 bytes.
/// Indexed by the same slot index as the watchlist.
struct MintStore {
    mints: [[u8; 32]; WATCHLIST_CAPACITY],
}

impl MintStore {
    const fn new() -> Self {
        MintStore {
            mints: [[0u8; 32]; WATCHLIST_CAPACITY],
        }
    }
}

// ---------------------------------------------------------------------------
// PromoteResult — returned when a confirming buy matches a watched mint
// ---------------------------------------------------------------------------

/// Data needed to open a position after watchlist promotion.
/// All fields are copied from the WatchSlot — no references.
pub struct PromoteResult {
    pub mint: [u8; 32],
    pub score: f64,
    pub magnitude: f64,
    pub conviction: EntryConviction,
    pub watch_duration_ms: u64,
    pub entry_vsol_reserves_mvsol: u32,
}

// ---------------------------------------------------------------------------
// Watchlist — the main structure
// ---------------------------------------------------------------------------

/// Fixed-size, zero-heap watchlist for two-phase entry.
pub struct Watchlist {
    slots: [WatchSlot; WATCHLIST_CAPACITY],
    mints: MintStore,
    /// Clock-hand for round-robin eviction.
    hand: u8,
    /// Configurable expiry window.
    expiry_ms: u64,
    /// Stats counters.
    pub watches_added: u64,
    pub watches_promoted: u64,
    pub watches_expired: u64,
    pub watches_evicted: u64,
}

impl Watchlist {
    pub fn new() -> Self {
        Self::with_expiry(DEFAULT_EXPIRY_MS)
    }

    pub fn with_expiry(expiry_ms: u64) -> Self {
        Watchlist {
            slots: [WatchSlot::EMPTY; WATCHLIST_CAPACITY],
            mints: MintStore::new(),
            hand: 0,
            expiry_ms,
            watches_added: 0,
            watches_promoted: 0,
            watches_expired: 0,
            watches_evicted: 0,
        }
    }

    /// Extract u64 hash from first 8 bytes of mint.
    #[inline(always)]
    fn mint_hash(mint: &[u8; 32]) -> u64 {
        u64::from_le_bytes([
            mint[0], mint[1], mint[2], mint[3],
            mint[4], mint[5], mint[6], mint[7],
        ])
    }

    /// Extract u64 prefix from signature.
    #[inline(always)]
    fn sig_prefix(sig: &[u8; 64]) -> u64 {
        u64::from_le_bytes([
            sig[0], sig[1], sig[2], sig[3],
            sig[4], sig[5], sig[6], sig[7],
        ])
    }

    /// Check if a mint is already being watched.
    /// Returns slot index if found, None otherwise.
    /// Linear scan — 64 × 8-byte comparison = ~30ns worst case.
    #[inline(always)]
    pub fn find(&self, mint: &[u8; 32], now_ms: u64) -> Option<usize> {
        let hash = Self::mint_hash(mint);
        for i in 0..WATCHLIST_CAPACITY {
            let slot = &self.slots[i];
            if slot.mint_hash == hash && slot.is_watching() && !slot.is_expired(now_ms) {
                return Some(i);
            }
        }
        None
    }

    /// Add a new mint to the watchlist with cached entry decision.
    /// Uses clock-hand eviction if full. Returns slot index.
    #[inline]
    pub fn watch(
        &mut self,
        trade: &TradeEvent,
        score: f64,
        magnitude: f64,
        conviction: &EntryConviction,
        now_ms: u64,
    ) -> usize {
        let hash = Self::mint_hash(&trade.mint);
        let sig_pfx = Self::sig_prefix(&trade.sig);

        // First pass: find empty or expired slot
        for i in 0..WATCHLIST_CAPACITY {
            let slot = &self.slots[i];
            if slot.is_empty() || slot.is_expired(now_ms) {
                if slot.is_watching() {
                    self.watches_expired += 1;
                }
                self.write_slot(i, hash, sig_pfx, trade, score, magnitude, conviction, now_ms);
                return i;
            }
        }

        // No empty slots — evict at clock hand
        let evict_idx = self.hand as usize % WATCHLIST_CAPACITY;
        self.hand = self.hand.wrapping_add(1);
        if self.slots[evict_idx].is_watching() {
            self.watches_evicted += 1;
        }
        self.write_slot(evict_idx, hash, sig_pfx, trade, score, magnitude, conviction, now_ms);
        evict_idx
    }

    #[inline(always)]
    fn write_slot(
        &mut self,
        idx: usize,
        hash: u64,
        sig_pfx: u64,
        trade: &TradeEvent,
        score: f64,
        magnitude: f64,
        conviction: &EntryConviction,
        now_ms: u64,
    ) {
        let vsol_mvsol = (trade.vsol_reserves / 1_000_000) as u32;
        self.slots[idx] = WatchSlot {
            mint_hash: hash,
            watch_start_ms: now_ms,
            score: (score * 10_000.0).min(u32::MAX as f64) as u32,
            magnitude: (magnitude * 10_000.0).min(u32::MAX as f64) as u32,
            size_lamports: conviction.size_lamports.min(u32::MAX as u64) as u32,
            p_permille: conviction.p_permille,
            r_x100: conviction.r_x100,
            f_permille: conviction.f_permille,
            conviction_tier: conviction.conviction_tier,
            state: 1, // watching
            entry_vsol_reserves: vsol_mvsol,
            expiry_ms: now_ms + self.expiry_ms,
            sig_prefix: sig_pfx,
            confirm1_sig_prefix: [0; 8],
        };
        self.mints.mints[idx] = trade.mint;
        self.watches_added += 1;
    }

    /// Strong-interest threshold: a single buy ≥ 0.10 SOL skips straight to promotion.
    const STRONG_INTEREST_MVSOL: u32 = 100; // 0.10 SOL in mvsol

    /// Try to promote a watched mint on a confirming buy.
    /// Returns PromoteResult if the mint is watched and the buy qualifies.
    ///
    /// Two-buy confirmation state machine:
    ///   state 1 (watching) + buy → state 3 (partial_confirm) [stores first confirm sig]
    ///   state 3 (partial_confirm) + different buy → promote (state 2)
    ///   Strong-interest shortcut: single buy ≥ 0.10 SOL → immediate promote
    ///
    /// Qualification (both buys):
    /// 1. Mint is in watchlist and not expired
    /// 2. Buy is from a DIFFERENT transaction (sig_prefix differs from entry AND confirm1)
    /// 3. Buy amount >= MIN_CONFIRM_MVSOL
    /// 4. vSOL reserves haven't moved more than 10% from watch time
    #[inline]
    pub fn try_promote(
        &mut self,
        trade: &TradeEvent,
        now_ms: u64,
    ) -> Option<PromoteResult> {
        let idx = self.find(&trade.mint, now_ms)?;
        let slot = &self.slots[idx];

        // Must be a buy
        if !trade.is_buy {
            return None;
        }

        // Different transaction than the one that triggered the watch
        let sig_pfx = Self::sig_prefix(&trade.sig);
        if sig_pfx == slot.sig_prefix {
            return None;
        }

        // Minimum buy size
        let buy_mvsol = (trade.sol_amount / 1_000_000) as u32;
        if buy_mvsol < MIN_CONFIRM_MVSOL {
            return None;
        }

        // Slippage + velocity checks — SKIP when vSOL data is absent (ShredStream entries).
        // ShredStream trades arrive with vsol_reserves=0 because decoded shred transactions
        // don't include account state. PumpPortal will enrich later via dedup.
        let current_vsol_mvsol = (trade.vsol_reserves / 1_000_000) as u32;
        let entry_vsol = slot.entry_vsol_reserves;
        if entry_vsol > 0 && current_vsol_mvsol > 0 {
            // Slippage check: vSOL shouldn't have moved >10% from watch time
            let delta = if current_vsol_mvsol > entry_vsol {
                current_vsol_mvsol - entry_vsol
            } else {
                entry_vsol - current_vsol_mvsol
            };
            if delta * 10 > entry_vsol {
                return None;
            }

            // vSOL Velocity Check: reject if price has FALLEN >3% since watch time.
            // Allow small dips — vSOL naturally oscillates between buys on pump.fun.
            if current_vsol_mvsol < entry_vsol {
                let drop_bp = ((entry_vsol - current_vsol_mvsol) as u64 * 10_000) / entry_vsol as u64;
                if drop_bp > 300 {
                    return None;
                }
            }
        }

        // ── 2-Buy State Machine ────────────────────────────────────
        if slot.state == 1 {
            // State 1 (watching) → first confirming buy
            if buy_mvsol >= Self::STRONG_INTEREST_MVSOL {
                // Strong-interest shortcut: ≥0.10 SOL → immediate promotion
                return self.finalize_promote(idx, now_ms);
            }
            // Normal path: transition to partial_confirm (state 3)
            let sig_bytes = trade.sig[..8].try_into().unwrap_or([0u8; 8]);
            self.slots[idx].confirm1_sig_prefix = sig_bytes;
            self.slots[idx].state = 3; // partial_confirm
            return None;
        }

        if slot.state == 3 {
            // State 3 (partial_confirm) → second confirming buy
            // Must differ from BOTH entry sig AND first confirm sig
            let confirm1_pfx = u64::from_le_bytes(slot.confirm1_sig_prefix);
            if sig_pfx == confirm1_pfx {
                return None; // same as first confirm — dedup
            }
            // Second distinct confirming buy → promote
            return self.finalize_promote(idx, now_ms);
        }

        None
    }

    /// Internal helper: finalize promotion from any state.
    #[inline(always)]
    fn finalize_promote(&mut self, idx: usize, now_ms: u64) -> Option<PromoteResult> {
        let slot = &self.slots[idx];
        let watch_duration = now_ms.saturating_sub(slot.watch_start_ms);
        let mint = self.mints.mints[idx];
        let entry_vsol = slot.entry_vsol_reserves;

        let result = PromoteResult {
            mint,
            score: slot.score as f64 / 10_000.0,
            magnitude: slot.magnitude as f64 / 10_000.0,
            conviction: EntryConviction {
                p_permille: slot.p_permille,
                r_x100: slot.r_x100,
                f_permille: slot.f_permille,
                size_lamports: slot.size_lamports as u64,
                conviction_tier: slot.conviction_tier,
                _pad: [0u8; 5],
            },
            watch_duration_ms: watch_duration,
            entry_vsol_reserves_mvsol: entry_vsol,
        };

        // Mark as promoted (consumed — won't match again)
        self.slots[idx].state = 2;
        self.watches_promoted += 1;

        Some(result)
    }

    /// Bulk-expire stale entries. Called periodically (e.g., every 100ms).
    /// Returns number of expired entries. Handles both state 1 (watching) and state 3 (partial_confirm).
    #[inline]
    pub fn expire_stale(&mut self, now_ms: u64) -> u32 {
        let mut expired = 0u32;
        for i in 0..WATCHLIST_CAPACITY {
            let slot = &mut self.slots[i];
            // is_watching() returns true for state 1 AND state 3
            if slot.is_watching() && slot.is_expired(now_ms) {
                slot.state = 0; // mark empty
                expired += 1;
                self.watches_expired += 1;
            }
        }
        expired
    }

    /// Current number of actively watched mints.
    #[inline]
    pub fn active_count(&self) -> u32 {
        let mut count = 0u32;
        for i in 0..WATCHLIST_CAPACITY {
            if self.slots[i].is_watching() {
                count += 1;
            }
        }
        count
    }

    /// Remove a specific mint from the watchlist (e.g., on creator sell).
    /// Handles state 1 (watching) and state 3 (partial_confirm).
    #[inline]
    pub fn remove_mint(&mut self, mint: &[u8; 32]) {
        let hash = Self::mint_hash(mint);
        for i in 0..WATCHLIST_CAPACITY {
            // is_watching() covers both state 1 and state 3
            if self.slots[i].mint_hash == hash && self.slots[i].is_watching() {
                self.slots[i].state = 0;
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade(mint: [u8; 32], sig: [u8; 64], sol: u64, vsol: u64, is_buy: bool) -> TradeEvent {
        TradeEvent {
            mint,
            trader: [0u8; 32],
            sig,
            sig_prefix: [sig[0], sig[1], sig[2], sig[3], sig[4], sig[5], sig[6], sig[7]],
            sol_amount: sol,
            token_amount: 0,
            vsol_reserves: vsol,
            vtoken_reserves: 1_000_000_000_000_000,
            market_cap_sol: 0,
            slot: 0,
            timestamp_ms: 0,
            is_buy,
            source: crate::feeds::FeedSource::PumpPortal,
            bonding_curve: [0u8; 32],
            assoc_bonding_curve: [0u8; 32],
        }
    }

    fn default_conviction() -> EntryConviction {
        EntryConviction {
            p_permille: 542,
            r_x100: 485,
            f_permille: 248,
            size_lamports: 100_000_000,
            conviction_tier: 1,
            _pad: [0; 5],
        }
    }

    #[test]
    fn test_slot_size() {
        assert_eq!(core::mem::size_of::<WatchSlot>(), 64);
    }

    #[test]
    fn test_watchlist_size() {
        // 64 slots × 64 bytes = 4KB (L1 cache resident)
        assert_eq!(core::mem::size_of::<[WatchSlot; WATCHLIST_CAPACITY]>(), 4096);
    }

    #[test]
    fn test_watch_and_promote_two_buy() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];
        let sig1 = [0xBBu8; 64]; // entry
        let sig2 = [0xCCu8; 64]; // first confirm
        let sig3 = [0xDDu8; 64]; // second confirm

        let trade1 = make_trade(mint, sig1, 100_000_000, 30_000_000_000, true);
        let conv = default_conviction();

        // Watch
        wl.watch(&trade1, 0.75, 60.0, &conv, 1000);
        assert_eq!(wl.active_count(), 1);
        assert_eq!(wl.watches_added, 1);

        // First confirming buy (0.05 SOL < 0.10 SOL threshold) → partial_confirm
        let trade2 = make_trade(mint, sig2, 50_000_000, 30_500_000_000, true);
        let result = wl.try_promote(&trade2, 1200);
        assert!(result.is_none(), "first small buy should NOT promote");
        assert_eq!(wl.active_count(), 1); // still active (state 3)

        // Second confirming buy from different sig → promote
        let trade3 = make_trade(mint, sig3, 50_000_000, 30_500_000_000, true);
        let result = wl.try_promote(&trade3, 1300);
        assert!(result.is_some(), "second distinct buy SHOULD promote");
        let r = result.unwrap();
        assert_eq!(r.mint, mint);
        assert_eq!(r.watch_duration_ms, 300);
        assert_eq!(wl.watches_promoted, 1);

        // Should not promote again (state = promoted)
        let trade4 = make_trade(mint, [0xEEu8; 64], 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade4, 1400).is_none());
    }

    #[test]
    fn test_strong_interest_immediate_promote() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];
        let sig1 = [0xBBu8; 64];
        let sig2 = [0xCCu8; 64];

        let trade1 = make_trade(mint, sig1, 100_000_000, 30_000_000_000, true);
        wl.watch(&trade1, 0.75, 60.0, &default_conviction(), 1000);

        // Single large buy ≥ 0.10 SOL → immediate promotion (no 2nd buy needed)
        let trade2 = make_trade(mint, sig2, 100_000_000, 30_500_000_000, true);
        let result = wl.try_promote(&trade2, 1100);
        assert!(result.is_some(), "0.10 SOL buy should promote immediately");
        assert_eq!(wl.watches_promoted, 1);
    }

    #[test]
    fn test_second_buy_same_as_first_confirm_rejected() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];
        let sig1 = [0xBBu8; 64];
        let sig2 = [0xCCu8; 64];

        let trade1 = make_trade(mint, sig1, 100_000_000, 30_000_000_000, true);
        wl.watch(&trade1, 0.75, 60.0, &default_conviction(), 1000);

        // First confirm → state 3
        let trade2 = make_trade(mint, sig2, 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade2, 1100).is_none());

        // Same sig as first confirm → dedup rejection
        let trade3 = make_trade(mint, sig2, 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade3, 1200).is_none(), "duplicate sig should be rejected");

        // But a different sig should promote
        let trade4 = make_trade(mint, [0xDDu8; 64], 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade4, 1300).is_some(), "different sig should promote");
    }

    #[test]
    #[test]
    fn test_falling_knife_rejected() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        // Watch at 30 SOL vSOL reserves
        let trade1 = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade1, 0.75, 60.0, &default_conviction(), 1000);

        // Confirming buy but vSOL has FALLEN >3% (28 SOL = -6.7%)
        let trade2 = make_trade(mint, [0xCCu8; 64], 50_000_000, 28_000_000_000, true);
        assert!(wl.try_promote(&trade2, 1100).is_none(), ">3% drop should be rejected");

        // Small dip ≤3% should still work (29.5 SOL = -1.7%)
        let trade3 = make_trade(mint, [0xDDu8; 64], 50_000_000, 29_500_000_000, true);
        assert!(wl.try_promote(&trade3, 1200).is_none(), "first buy goes to partial_confirm (small dip OK)");

        // Second buy with rising vSOL → promote
        let trade4 = make_trade(mint, [0xEEu8; 64], 50_000_000, 31_000_000_000, true);
        assert!(wl.try_promote(&trade4, 1300).is_some(), "second buy should promote");
    }

    #[test]
    fn test_partial_confirm_expires() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        let trade1 = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade1, 0.75, 60.0, &default_conviction(), 1000);

        // First confirm → state 3
        let trade2 = make_trade(mint, [0xCCu8; 64], 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade2, 1100).is_none());

        // Expire after expiry window
        let expired = wl.expire_stale(4000); // well past 1000 + 2000ms expiry
        assert_eq!(expired, 1, "partial_confirm should expire");
        assert_eq!(wl.active_count(), 0);
    }

    #[test]
    fn test_same_sig_not_promoted() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];
        let sig = [0xBBu8; 64];

        let trade = make_trade(mint, sig, 100_000_000, 30_000_000_000, true);
        wl.watch(&trade, 0.75, 60.0, &default_conviction(), 1000);

        // Same sig should NOT promote (it's the same transaction)
        let same_sig_trade = make_trade(mint, sig, 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&same_sig_trade, 1100).is_none());
    }

    #[test]
    fn test_sell_not_promoted() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        let trade1 = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade1, 0.75, 60.0, &default_conviction(), 1000);

        // Sell should NOT promote
        let sell = make_trade(mint, [0xCCu8; 64], 50_000_000, 30_500_000_000, false);
        assert!(wl.try_promote(&sell, 1100).is_none());
    }

    #[test]
    fn test_expiry() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        let trade = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade, 0.75, 60.0, &default_conviction(), 1000);

        // After expiry (1500ms), should not promote
        let trade2 = make_trade(mint, [0xCCu8; 64], 50_000_000, 30_500_000_000, true);
        assert!(wl.try_promote(&trade2, 2600).is_none()); // 1000 + 1600 > 1500ms expiry

        // Expire stale
        let expired = wl.expire_stale(3100);
        assert_eq!(expired, 1);
        assert_eq!(wl.active_count(), 0);
    }

    #[test]
    fn test_dust_buy_not_promoted() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        let trade = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade, 0.75, 60.0, &default_conviction(), 1000);

        // Dust buy (0.01 SOL = 10 mvsol < MIN_CONFIRM_MVSOL=30)
        let dust = make_trade(mint, [0xCCu8; 64], 10_000_000, 30_100_000_000, true);
        assert!(wl.try_promote(&dust, 1100).is_none());
    }

    #[test]
    fn test_slippage_rejection() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];

        let trade = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade, 0.75, 60.0, &default_conviction(), 1000);

        // Price moved >10% from watch point (33B → 36B = 10% up... border)
        // 30B → 34B = 13.3% > 10% → reject
        let slipped = make_trade(mint, [0xCCu8; 64], 50_000_000, 34_000_000_000, true);
        assert!(wl.try_promote(&slipped, 1100).is_none());
    }

    #[test]
    fn test_eviction_when_full() {
        let mut wl = Watchlist::new();
        let conv = default_conviction();

        // Fill all 64 slots
        for i in 0..64u8 {
            let mut mint = [0u8; 32];
            mint[0] = i;
            let trade = make_trade(mint, [i; 64], 100_000_000, 30_000_000_000, true);
            wl.watch(&trade, 0.75, 60.0, &conv, 1000);
        }
        assert_eq!(wl.active_count(), 64);

        // Add one more — should evict
        let mut mint65 = [0xFFu8; 32];
        let trade65 = make_trade(mint65, [0xFEu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade65, 0.75, 60.0, &conv, 1000);
        assert_eq!(wl.watches_evicted, 1);
        assert_eq!(wl.active_count(), 64); // one evicted, one added
    }

    #[test]
    fn test_remove_mint() {
        let mut wl = Watchlist::new();
        let mint = [0xAAu8; 32];
        let trade = make_trade(mint, [0xBBu8; 64], 100_000_000, 30_000_000_000, true);
        wl.watch(&trade, 0.75, 60.0, &default_conviction(), 1000);
        assert_eq!(wl.active_count(), 1);

        wl.remove_mint(&mint);
        assert_eq!(wl.active_count(), 0);
    }
}
