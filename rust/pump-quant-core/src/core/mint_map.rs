use arrayvec::ArrayVec;
use hashbrown::HashMap;

use super::trade_record::TradeRecord;

pub const RING_CAP: usize = 64;

#[derive(Clone)]
pub struct MintHistory {
    pub trades: [TradeRecord; RING_CAP], // 8192 bytes
    pub head: u32,                       // write pointer (next slot)
    pub count: u32,                      // valid entries, saturates at RING_CAP
    pub mint: [u8; 32],
    pub first_seen_ms: u64,
    pub last_trade_ms: u64,
    pub creator_sell_at_ms: u64, // 0 = no creator sell detected

    // Pre-computed aggregates — updated on every push()
    pub cached_unique_buyers_30s: u16,
    pub cached_buy_count_1s: u16,
    pub cached_buy_count_2s: u16,
    pub cached_buy_count_5s: u16,
    pub cached_sell_count_5s: u16,
    pub cached_volume_sol_5s: u64, // lamports
    pub cached_vsol_oldest_3s: u64, // oldest vSol in 3s window (for delta calc)
}

impl MintHistory {
    /// Create a new empty MintHistory for the given mint.
    pub fn new(mint: [u8; 32], now_ms: u64) -> Self {
        Self {
            trades: [TradeRecord::ZERO; RING_CAP],
            head: 0,
            count: 0,
            mint,
            first_seen_ms: now_ms,
            last_trade_ms: 0,
            creator_sell_at_ms: 0,
            cached_unique_buyers_30s: 0,
            cached_buy_count_1s: 0,
            cached_buy_count_2s: 0,
            cached_buy_count_5s: 0,
            cached_sell_count_5s: 0,
            cached_volume_sol_5s: 0,
            cached_vsol_oldest_3s: 0,
        }
    }

    /// Push a trade into the ring buffer and recompute all cached aggregates.
    pub fn push(&mut self, trade: TradeRecord, now_ms: u64) {
        self.write_trade(trade, now_ms);
        self.recompute_aggregates(now_ms);
    }

    /// Pre-warm only — same as push but does NOT update cached aggregates.
    /// Used by Helius pre-warmer path.
    pub fn add_trade_to_history(&mut self, trade: TradeRecord, now_ms: u64) {
        self.write_trade(trade, now_ms);
    }

    /// Write a trade into the ring buffer at head, advance head, update bookkeeping.
    #[inline(always)]
    fn write_trade(&mut self, trade: TradeRecord, now_ms: u64) {
        let idx = self.head as usize;
        self.trades[idx] = trade;
        self.head = ((self.head + 1) % RING_CAP as u32) as u32;
        if (self.count as usize) < RING_CAP {
            self.count += 1;
        }
        self.last_trade_ms = now_ms;
    }

    /// Walk backwards from head through valid entries, calling `f` on each trade
    /// whose timestamp >= `cutoff_ms`. Stops early when an entry falls below cutoff.
    /// Zero allocation.
    pub fn for_each_in_window<F: FnMut(&TradeRecord)>(&self, cutoff_ms: u64, mut f: F) {
        let n = self.count as usize;
        if n == 0 {
            return;
        }
        for i in 0..n {
            // Walk backwards from (head - 1)
            let idx = if self.head as usize >= 1 + i {
                self.head as usize - 1 - i
            } else {
                RING_CAP + self.head as usize - 1 - i
            };
            let trade = &self.trades[idx];
            if trade.timestamp_ms < cutoff_ms {
                break;
            }
            f(trade);
        }
    }

    /// Count unique buyers in the window >= cutoff_ms.
    /// For n <= 12: O(n²) brute-force (faster due to no sort overhead).
    /// Otherwise: ArrayVec sort + dedup.
    pub fn unique_buyers_in_window(&self, cutoff_ms: u64) -> u16 {
        let mut buyers: ArrayVec<[u8; 32], 64> = ArrayVec::new();

        self.for_each_in_window(cutoff_ms, |trade| {
            if trade.is_buy && !buyers.is_full() {
                buyers.push(trade.trader);
            }
        });

        if buyers.len() <= 12 {
            // O(n²) brute-force dedup
            let mut unique = 0u16;
            for i in 0..buyers.len() {
                let mut is_dup = false;
                for j in 0..i {
                    if buyers[i] == buyers[j] {
                        is_dup = true;
                        break;
                    }
                }
                if !is_dup {
                    unique += 1;
                }
            }
            unique
        } else {
            // Sort + manual dedup for larger sets
            buyers.sort_unstable();
            let mut unique = if buyers.is_empty() { 0u16 } else { 1u16 };
            for i in 1..buyers.len() {
                if buyers[i] != buyers[i - 1] {
                    unique += 1;
                }
            }
            unique
        }
    }

    /// Recompute all cached_* aggregate fields by scanning the ring buffer.
    fn recompute_aggregates(&mut self, now_ms: u64) {
        #[cfg(debug_assertions)]
        let _t0 = std::time::Instant::now();

        let cutoff_1s = now_ms.saturating_sub(1_000);
        let cutoff_2s = now_ms.saturating_sub(2_000);
        let cutoff_3s = now_ms.saturating_sub(3_000);
        let cutoff_5s = now_ms.saturating_sub(5_000);
        let cutoff_30s = now_ms.saturating_sub(30_000);

        let mut buy_count_1s: u16 = 0;
        let mut buy_count_2s: u16 = 0;
        let mut buy_count_5s: u16 = 0;
        let mut sell_count_5s: u16 = 0;
        let mut volume_sol_5s: u64 = 0;

        // For vsol_oldest_3s: track the oldest vSol within the 3s window.
        let mut vsol_oldest_3s: u64 = 0;
        let mut oldest_ts_in_3s: u64 = u64::MAX;

        // Walk the 30s window (superset of all smaller windows).
        // We collect buyers for the 30s unique count separately.
        let n = self.count as usize;
        if n == 0 {
            self.cached_unique_buyers_30s = 0;
            self.cached_buy_count_1s = 0;
            self.cached_buy_count_2s = 0;
            self.cached_buy_count_5s = 0;
            self.cached_sell_count_5s = 0;
            self.cached_volume_sol_5s = 0;
            self.cached_vsol_oldest_3s = 0;
            return;
        }

        for i in 0..n {
            let idx = if self.head as usize >= 1 + i {
                self.head as usize - 1 - i
            } else {
                RING_CAP + self.head as usize - 1 - i
            };
            let trade = &self.trades[idx];
            if trade.timestamp_ms < cutoff_30s {
                break;
            }

            // 5s aggregates
            if trade.timestamp_ms >= cutoff_5s {
                if trade.is_buy {
                    buy_count_5s = buy_count_5s.saturating_add(1);
                } else {
                    sell_count_5s = sell_count_5s.saturating_add(1);
                }
                volume_sol_5s = volume_sol_5s.saturating_add(trade.sol_amount);

                // 2s aggregates (subset of 5s)
                if trade.timestamp_ms >= cutoff_2s {
                    if trade.is_buy {
                        buy_count_2s = buy_count_2s.saturating_add(1);
                    }

                    // 1s aggregates (subset of 2s)
                    if trade.timestamp_ms >= cutoff_1s {
                        if trade.is_buy {
                            buy_count_1s = buy_count_1s.saturating_add(1);
                        }
                    }
                }
            }

            // 3s window: track oldest vSol
            if trade.timestamp_ms >= cutoff_3s {
                if trade.timestamp_ms < oldest_ts_in_3s {
                    oldest_ts_in_3s = trade.timestamp_ms;
                    vsol_oldest_3s = trade.vsol_reserves;
                }
            }
        }

        self.cached_buy_count_1s = buy_count_1s;
        self.cached_buy_count_2s = buy_count_2s;
        self.cached_buy_count_5s = buy_count_5s;
        self.cached_sell_count_5s = sell_count_5s;
        self.cached_volume_sol_5s = volume_sol_5s;
        self.cached_vsol_oldest_3s = vsol_oldest_3s;

        // Unique buyers in 30s — use the dedicated method
        self.cached_unique_buyers_30s = self.unique_buyers_in_window(cutoff_30s);

        #[cfg(debug_assertions)]
        {
            let elapsed = _t0.elapsed();
            if elapsed.as_micros() > 10 {
                tracing::warn!(
                    elapsed_us = elapsed.as_micros(),
                    "mint_map aggregate recompute slow"
                );
            }
        }
    }
}

// ── MintHistoryMap ──────────────────────────────────────────────────

pub struct MintHistoryMap {
    inner: HashMap<[u8; 32], Box<MintHistory>>,
    last_evict_ms: u64,
}

impl MintHistoryMap {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            inner: HashMap::with_capacity(n),
            last_evict_ms: 0,
        }
    }

    /// Get or create a MintHistory for the given mint.
    pub fn get_or_insert(&mut self, mint: &[u8; 32], now_ms: u64) -> &mut MintHistory {
        self.inner
            .entry(*mint)
            .or_insert_with(|| Box::new(MintHistory::new(*mint, now_ms)))
    }

    pub fn get(&self, mint: &[u8; 32]) -> Option<&MintHistory> {
        self.inner.get(mint).map(|b| b.as_ref())
    }

    pub fn get_mut(&mut self, mint: &[u8; 32]) -> Option<&mut MintHistory> {
        self.inner.get_mut(mint).map(|b| b.as_mut())
    }

    /// Remove entries where last_trade_ms < now_ms - stale_threshold_ms.
    pub fn evict_stale(&mut self, now_ms: u64, stale_threshold_ms: u64) {
        let cutoff = now_ms.saturating_sub(stale_threshold_ms);
        self.inner.retain(|_, history| history.last_trade_ms >= cutoff);
        self.last_evict_ms = now_ms;
    }
}
