//! Integer AMM order-flow / microstructure feature catalog (constitution 21.7).
//!
//! Responsibility: compute the swap-flow-derived microstructure features that
//! *do* transfer to constant-product AMMs — CVD and delta velocity, order-flow
//! imbalance, VWAP / anchored VWAP, trade-size distribution and large-print
//! detection, swap-arrival intensity, and CVD/price divergence — with **no**
//! floating point (constitution 22) and **no** imagined limit-order-book concept
//! (constitution 21.7: "classical LOB microstructure ... must not be imported as
//! if it did"). Every function is pure and computed point-in-time from the trade
//! slice it is given; the [`RollingFlowWindow`] adds a memory-bounded streaming
//! form (constitution 57).
//!
//! These are *research-gated hypotheses*, not assumed-predictive signals
//! (constitution 21.7 / 46): this module computes them faithfully; admission and
//! wash/cluster screening live in other planes.

use crate::types::{FeatureError, Side, TradeEvent, PRICE_SCALE};
use std::collections::VecDeque;

/// Cumulative volume delta: running net of buy-side minus sell-side quote volume
/// over `trades` (constitution 21.7). The primary order-flow-intent proxy.
///
/// Accumulates in [`i128`] with `saturating_add` as the explicit overflow
/// contract (constitution 22); realistic lamport volumes over any bounded window
/// are far below the [`i128`] range, so saturation is a hard safety backstop, not
/// an expected path.
#[must_use]
pub fn cumulative_volume_delta(trades: &[TradeEvent]) -> i128 {
    let mut cvd: i128 = 0;
    for t in trades {
        cvd = cvd.saturating_add(t.signed_quote());
    }
    cvd
}

/// CVD delta velocity: change in CVD per second between two samples
/// (constitution 21.7 "delta velocity/acceleration"), scaled by [`PRICE_SCALE`]
/// to retain fractional resolution without floating point.
///
/// Returns `None` when `to_ts_ns <= from_ts_ns` (no positive time base). Units are
/// quote-units per second, computed as `delta * NS_PER_SEC / dt_ns` (integer,
/// truncating), using [`i128`] and `saturating_mul` for the widening step
/// (constitution 22). `PRICE_SCALE` is not applied here — CVD is a quote-volume
/// quantity, not a price.
#[must_use]
pub fn cvd_velocity_per_sec(
    cvd_from: i128,
    from_ts_ns: u64,
    cvd_to: i128,
    to_ts_ns: u64,
) -> Option<i128> {
    if to_ts_ns <= from_ts_ns {
        return None;
    }
    let dt = i128::from(to_ts_ns - from_ts_ns);
    let ns_per_sec: i128 = 1_000_000_000;
    let delta = cvd_to.saturating_sub(cvd_from);
    // delta[quote] * (ns/sec) / dt[ns] = quote/sec. Integer (truncating) division.
    Some(delta.saturating_mul(ns_per_sec) / dt)
}

/// Signed order-flow imbalance in base units over `trades` (constitution 21.7):
/// buy base volume minus sell base volume. [`i128`], saturating.
#[must_use]
pub fn order_flow_imbalance_base(trades: &[TradeEvent]) -> i128 {
    let mut ofi: i128 = 0;
    for t in trades {
        ofi = ofi.saturating_add(t.signed_base());
    }
    ofi
}

/// Order-flow imbalance normalized to basis points of total base volume
/// (constitution 21.7 aggressor-side skew): `(buy - sell) / (buy + sell) * 10_000`.
///
/// Returns `None` when total base volume is zero (imbalance undefined). The result
/// lies in `[-10_000, 10_000]`. Computed with [`i128`] widening then narrowed to
/// [`i32`]; the ratio is always in range so the narrowing cannot truncate.
#[must_use]
pub fn order_flow_imbalance_bps(trades: &[TradeEvent]) -> Option<i32> {
    let mut buy: i128 = 0;
    let mut sell: i128 = 0;
    for t in trades {
        match t.side {
            Side::Buy => buy = buy.saturating_add(i128::from(t.base_qty)),
            Side::Sell => sell = sell.saturating_add(i128::from(t.base_qty)),
        }
    }
    let total = buy.saturating_add(sell);
    if total == 0 {
        return None;
    }
    let net = buy - sell;
    // net * 10_000 / total, exact within i128; |result| <= 10_000.
    let bps = net.saturating_mul(10_000) / total;
    Some(bps as i32)
}

/// Volume-weighted average price over `trades` in [`PRICE_SCALE`] units
/// (constitution 21.7 VWAP). Weight is base volume.
///
/// Returns `None` when total base volume is zero. Uses [`i128`] for the weighted
/// sum with `saturating_mul`/`saturating_add` as the explicit overflow contract
/// (constitution 22): with prices in `PRICE_SCALE` units the product `price_fp *
/// base_qty` is comfortably within [`i128`] for any realistic AMM print.
#[must_use]
pub fn vwap_fp(trades: &[TradeEvent]) -> Option<i128> {
    let mut num: i128 = 0;
    let mut den: i128 = 0;
    for t in trades {
        let w = i128::from(t.base_qty);
        num = num.saturating_add(t.price_fp.saturating_mul(w));
        den = den.saturating_add(w);
    }
    if den == 0 {
        None
    } else {
        Some(num / den)
    }
}

/// Anchored VWAP: [`vwap_fp`] restricted to trades with `ts_ns >= anchor_ns`
/// (constitution 21.7 anchored VWAP, anchored to launch/migration/session).
///
/// Returns `None` if no trade at or after the anchor carries base volume.
#[must_use]
pub fn anchored_vwap_fp(trades: &[TradeEvent], anchor_ns: u64) -> Option<i128> {
    let mut num: i128 = 0;
    let mut den: i128 = 0;
    for t in trades {
        if t.ts_ns < anchor_ns {
            continue;
        }
        let w = i128::from(t.base_qty);
        num = num.saturating_add(t.price_fp.saturating_mul(w));
        den = den.saturating_add(w);
    }
    if den == 0 {
        None
    } else {
        Some(num / den)
    }
}

/// Count of "large prints": trades whose `base_qty` is `>= threshold_base`
/// (constitution 21.7 large-print / whale-print detection).
#[must_use]
pub fn large_print_count(trades: &[TradeEvent], threshold_base: u64) -> u32 {
    let mut n: u32 = 0;
    for t in trades {
        if t.base_qty >= threshold_base {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Trade-size distribution as counts over caller-supplied ascending base-size
/// edges (constitution 21.7 trade-size distribution / histogram shape).
///
/// `edges` must be sorted ascending. The returned vector has `edges.len() + 1`
/// buckets: bucket `i < edges.len()` counts trades with `base_qty < edges[i]` and
/// `>= edges[i-1]` (or `>= 0` for `i == 0`); the final bucket counts trades with
/// `base_qty >= edges.last()`. All-integer, deterministic.
#[must_use]
pub fn trade_size_histogram(trades: &[TradeEvent], edges: &[u64]) -> Vec<u32> {
    let mut buckets = vec![0u32; edges.len() + 1];
    for t in trades {
        // partition_point returns the number of edges strictly <= is wrong; we want
        // the first edge strictly greater than base_qty -> that index is the bucket.
        let idx = edges.partition_point(|&e| e <= t.base_qty);
        buckets[idx] = buckets[idx].saturating_add(1);
    }
    buckets
}

/// Swap-arrival intensity: number of trades whose `ts_ns` falls in the half-open
/// window `(now_ns - window_ns, now_ns]` (constitution 21.7 swap-arrival intensity
/// / burst dynamics). A higher count is a denser arrival burst.
///
/// `window_ns` of zero yields zero (an empty window). Uses `saturating_sub` so a
/// window wider than `now_ns` clamps the lower bound at 0.
#[must_use]
pub fn arrival_intensity(trades: &[TradeEvent], now_ns: u64, window_ns: u64) -> u32 {
    if window_ns == 0 {
        return 0;
    }
    let lo = now_ns.saturating_sub(window_ns);
    let mut n: u32 = 0;
    for t in trades {
        if t.ts_ns > lo && t.ts_ns <= now_ns {
            n = n.saturating_add(1);
        }
    }
    n
}

/// CVD / price divergence classification (constitution 21.7): price making a
/// higher high while CVD fails to confirm is buy-pressure exhaustion; the inverse
/// is the bullish case; same-sign moves are confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvdDivergence {
    /// Price and CVD moved the same direction (both up or both down): the move is
    /// order-flow-confirmed.
    Confirmation,
    /// Price rose but CVD did not (`cvd_delta <= 0`): buy-pressure exhaustion.
    Bearish,
    /// Price fell but CVD did not (`cvd_delta >= 0`): sell-pressure exhaustion.
    Bullish,
    /// Price was flat: no directional signal.
    Neutral,
}

/// Classify divergence between a price change and a CVD change over the same
/// interval (constitution 21.7). Pure integer sign comparison.
#[must_use]
pub fn classify_divergence(price_delta_fp: i128, cvd_delta: i128) -> CvdDivergence {
    use core::cmp::Ordering;
    match price_delta_fp.cmp(&0) {
        Ordering::Greater => {
            if cvd_delta > 0 {
                CvdDivergence::Confirmation
            } else {
                CvdDivergence::Bearish
            }
        }
        Ordering::Less => {
            if cvd_delta < 0 {
                CvdDivergence::Confirmation
            } else {
                CvdDivergence::Bullish
            }
        }
        Ordering::Equal => CvdDivergence::Neutral,
    }
}

/// A memory-bounded rolling window of recent trades with O(1) incremental
/// aggregates (constitution 21.7 rolling-window features, 57 memory bound).
///
/// Responsibility: maintain CVD, order-flow imbalance, buy/sell volume, and
/// arrival count over the most recent `window_ns` of information time, subject to
/// a hard `capacity` on retained trades. Eviction is by time (drop trades older
/// than `newest_ts - window_ns`) and by capacity (drop the oldest when full). All
/// aggregates are kept incrementally in [`i128`] so queries are O(1) and never
/// touch floating point.
#[derive(Debug, Clone)]
pub struct RollingFlowWindow {
    window_ns: u64,
    capacity: usize,
    ring: VecDeque<TradeEvent>,
    cvd: i128,
    ofi_base: i128,
    buy_base: i128,
    sell_base: i128,
    newest_ts_ns: Option<u64>,
}

impl RollingFlowWindow {
    /// Create a window of width `window_ns` retaining at most `capacity` trades.
    /// Returns [`FeatureError::InvalidConfiguration`] if either is zero.
    pub fn new(window_ns: u64, capacity: usize) -> Result<Self, FeatureError> {
        if window_ns == 0 || capacity == 0 {
            return Err(FeatureError::InvalidConfiguration);
        }
        Ok(Self {
            window_ns,
            capacity,
            ring: VecDeque::new(),
            cvd: 0,
            ofi_base: 0,
            buy_base: 0,
            sell_base: 0,
            newest_ts_ns: None,
        })
    }

    fn add_aggregates(&mut self, t: &TradeEvent) {
        self.cvd = self.cvd.saturating_add(t.signed_quote());
        self.ofi_base = self.ofi_base.saturating_add(t.signed_base());
        match t.side {
            Side::Buy => self.buy_base = self.buy_base.saturating_add(i128::from(t.base_qty)),
            Side::Sell => self.sell_base = self.sell_base.saturating_add(i128::from(t.base_qty)),
        }
    }

    fn remove_aggregates(&mut self, t: &TradeEvent) {
        self.cvd = self.cvd.saturating_sub(t.signed_quote());
        self.ofi_base = self.ofi_base.saturating_sub(t.signed_base());
        match t.side {
            Side::Buy => self.buy_base = self.buy_base.saturating_sub(i128::from(t.base_qty)),
            Side::Sell => self.sell_base = self.sell_base.saturating_sub(i128::from(t.base_qty)),
        }
    }

    /// Push a trade. Enforces non-decreasing information time (constitution 20):
    /// an out-of-order trade is rejected with
    /// [`FeatureError::NonMonotonicTimestamp`]. After insertion, evicts trades
    /// that fell outside `window_ns` and, if still over `capacity`, evicts oldest.
    pub fn push(&mut self, t: TradeEvent) -> Result<(), FeatureError> {
        if let Some(prev) = self.newest_ts_ns {
            if t.ts_ns < prev {
                return Err(FeatureError::NonMonotonicTimestamp {
                    previous_ns: prev,
                    offending_ns: t.ts_ns,
                });
            }
        }
        self.newest_ts_ns = Some(t.ts_ns);
        self.add_aggregates(&t);
        self.ring.push_back(t);
        self.evict();
        Ok(())
    }

    fn evict(&mut self) {
        // Time eviction: drop everything older than the window relative to newest.
        if let Some(newest) = self.newest_ts_ns {
            let lo = newest.saturating_sub(self.window_ns);
            while let Some(front) = self.ring.front() {
                if front.ts_ns <= lo {
                    let old = self.ring.pop_front().expect("front checked"); // LINT-ALLOW(hot_panic): infallible — guarded by the enclosing `while let Some(front)`
                    self.remove_aggregates(&old);
                } else {
                    break;
                }
            }
        }
        // Capacity eviction: hard memory bound (constitution 57).
        while self.ring.len() > self.capacity {
            if let Some(old) = self.ring.pop_front() {
                self.remove_aggregates(&old);
            } else {
                break;
            }
        }
    }

    /// Number of retained trades.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the window is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Current cumulative volume delta over the retained window.
    #[must_use]
    pub fn cvd(&self) -> i128 {
        self.cvd
    }

    /// Current signed order-flow imbalance (base units) over the retained window.
    #[must_use]
    pub fn ofi_base(&self) -> i128 {
        self.ofi_base
    }

    /// Current buy base volume over the retained window.
    #[must_use]
    pub fn buy_base(&self) -> i128 {
        self.buy_base
    }

    /// Current sell base volume over the retained window.
    #[must_use]
    pub fn sell_base(&self) -> i128 {
        self.sell_base
    }

    /// Order-flow imbalance in basis points over the retained window, or `None`
    /// when no base volume is present. Matches [`order_flow_imbalance_bps`].
    #[must_use]
    pub fn ofi_bps(&self) -> Option<i32> {
        let total = self.buy_base.saturating_add(self.sell_base);
        if total == 0 {
            return None;
        }
        let net = self.buy_base - self.sell_base;
        Some((net.saturating_mul(10_000) / total) as i32)
    }

    /// Borrow the retained trades in arrival order (inspection/audit).
    pub fn trades(&self) -> impl Iterator<Item = &TradeEvent> {
        self.ring.iter()
    }
}

/// Convert a raw price and [`PRICE_SCALE`] into a fixed-point price, saturating on
/// overflow (constitution 22 helper). Provided for tests/consumers constructing
/// [`TradeEvent`]s from integer prices.
#[must_use]
pub fn to_price_fp(price: i128) -> i128 {
    price.saturating_mul(PRICE_SCALE)
}
