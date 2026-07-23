//! §21.6 bar + market-structure consumption and the §21.5 activity aggregates —
//! the app-side fold that turns raw per-mint trade flow into (a) trade-count bars
//! feeding the `pump_quant_features::market_structure` family and (b) the recent
//! activity window feeding the `pump_quant_signals::active_market_universe`
//! promotion screen.
//!
//! ## The bar clock is the per-mint trade counter (§22)
//!
//! Wall-clock bars are unavailable by design (no wall-clock reads in decisions),
//! and event-time bars would make bar identity depend on tape density. Instead
//! each mint's bars close every `trades_per_bar` trades: the builder's "time"
//! axis is the mint's own monotonically increasing trade counter. This is the
//! volume-clock/trade-clock construction of the bar literature (Easley,
//! López de Prado & O'Hara 2012: sampling by activity, not by seconds, aligns
//! samples with information arrival) — deterministic, integer, replay-identical.
//!
//! ## Reduce-only consumption (§33/§56.2)
//!
//! Market structure never *authorizes* anything (§21.6: "a visually attractive
//! pattern without independently validated on-chain support authorizes
//! nothing"). The engine consumes the trend classification exclusively as a
//! reduce-only size haircut (contradicted structure shrinks size; confirmed
//! structure merely leaves the §33 fraction untouched) and as a scale-in block
//! (never ADD risk against structure). No boost above the sizing envelope.
//!
//! Bounded state (§99): at most [`STRUCT_TRACK_CAP`] mints, each holding one
//! open bar, a fixed ring of closed bars, and a fixed activity ring.

use std::collections::{BTreeMap, VecDeque};

use pump_quant_features::bar::{Bar, BarBuilder, BarSpec};
use pump_quant_features::market_structure::{
    swing_highs, swing_lows, swing_structure, TrendStructure,
};
use pump_quant_features::structure_ext::realized_vol_bps;
use pump_quant_features::types::{Side, TradeEvent};
use pump_quant_signals::setup_classifier::MarketState;
use pump_quant_strategy::entry_mode_leaves::PullbackFeatures;

/// Closed bars retained per mint. `swing_structure` needs `left+right+1` bars to
/// find one pivot; 8 bars give a two-pivot history at neighborhood 1 while
/// keeping the per-mint footprint fixed (§99).
const BARS_CAP: usize = 8;

/// Swing-pivot neighborhood (bars on each side that must be strictly lower/
/// higher). 1 is the tightest well-defined pivot — appropriate for the very
/// short bar histories of second-scale memecoin scalps.
const SWING_NEIGHBORHOOD: usize = 1;

/// Most-recent trades retained per mint for the §21.5 activity window
/// (tick, buyer entity, quote lamports). 16 covers several promotion windows at
/// golden-tape density while staying a fixed-size scan.
const ACTIVITY_RING: usize = 16;

/// Maximum mints tracked (matches the lane track cap; weakest-recency evicted).
pub const STRUCT_TRACK_CAP: usize = 4_096;

/// Per-mint fold: the open-bar builder, the closed-bar ring, and the activity
/// ring. All fixed-size once constructed.
#[derive(Debug)]
struct MintMicro {
    /// Trade-count-clock bar builder (see module docs).
    builder: BarBuilder,
    /// Closed bars, oldest first, at most [`BARS_CAP`]. A [`VecDeque`] so the
    /// per-trade `record()` path evicts the oldest closed bar in O(1)
    /// (`pop_front`) rather than the O(n) `Vec::remove(0)` memmove (O4). Readers
    /// obtain a single contiguous ordered `&[Bar]` for the frozen `swing_*` /
    /// `realized_vol_bps` folds via [`MintMicro::with_bars`].
    bars: VecDeque<Bar>,
    /// Per-mint trade counter — the bar clock and the event-id source.
    n_trades: u64,
    /// Recent (tick, entity, quote_lamports) samples, a fixed ring.
    activity: [(u64, u64, u64); ACTIVITY_RING],
    /// Ring write cursor / total samples written (ring index = len % cap).
    activity_len: u64,
    /// Last logical tick this mint was touched (eviction recency).
    last_touch: u64,
}

impl MintMicro {
    /// Call `f` with the closed-bar ring as a single contiguous ordered
    /// (oldest→newest) `&[Bar]`. Zero-copy when the deque is already contiguous
    /// (`as_slices().0`); when it has physically wrapped, its bounded
    /// (≤ [`BARS_CAP`]) contents are cloned once into a small buffer. The slice is
    /// byte-identical to what the old `Vec` presented, so every structure readout —
    /// and the golden digest — is unchanged.
    #[inline]
    fn with_bars<R>(&self, f: impl FnOnce(&[Bar]) -> R) -> R {
        let (a, b) = self.bars.as_slices();
        if b.is_empty() {
            f(a)
        } else {
            let mut buf: Vec<Bar> = Vec::with_capacity(a.len() + b.len());
            buf.extend_from_slice(a);
            buf.extend_from_slice(b);
            f(&buf)
        }
    }
}

/// One canonical trade observation for the fold — the §21.7 leakage-relevant
/// facts of one decoded swap plus the logical tick it was seen at.
#[derive(Clone, Copy, Debug)]
pub struct TradeObs {
    /// Logical tick (freshness + activity-window stamp).
    pub now: u64,
    /// Reserve-derived execution price, fixed-point.
    pub price_fp: i128,
    /// Base units transacted (unsigned magnitude).
    pub base_qty: u64,
    /// Quote lamports transacted.
    pub quote_lamports: u64,
    /// Entity-resolved aggressor (§28 dedup upstream).
    pub buyer_entity: u64,
    /// Aggressor side (§21.7).
    pub is_buy: bool,
}

/// Aggregates of the recent activity window, consumed by the §21.5 promotion
/// screen via `pump_quant_signals::active_market_universe`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityWindow {
    /// Trades observed within the window.
    pub trades: u32,
    /// Distinct buyer entities within the window.
    pub entities: u32,
    /// Total quote lamports transacted within the window.
    pub volume_lamports: u64,
}

/// The bounded per-mint bar/structure/activity fold the engine feeds on every
/// `MarketTrade` and queries at promotion and gate time.
#[derive(Debug, Default)]
pub struct StructureState {
    mints: BTreeMap<[u8; 32], MintMicro>,
    /// Trades per bar (the bar clock interval), fixed at construction from
    /// config — never baked in (§102).
    trades_per_bar: u64,
}

impl StructureState {
    /// A fresh fold closing a bar every `trades_per_bar` trades (clamped ≥1).
    #[must_use]
    pub fn new(trades_per_bar: u64) -> Self {
        Self {
            mints: BTreeMap::new(),
            trades_per_bar: trades_per_bar.max(1),
        }
    }

    /// Whether nothing has been observed yet (zero-cost fast path).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mints.is_empty()
    }

    /// Fold one canonical trade observation into the mint's bars + activity.
    pub fn record(&mut self, mint: [u8; 32], obs: &TradeObs) {
        let TradeObs {
            now,
            price_fp,
            base_qty,
            quote_lamports,
            buyer_entity,
            is_buy,
        } = *obs;
        if !self.mints.contains_key(&mint) && self.mints.len() >= STRUCT_TRACK_CAP {
            // Evict the least-recently-touched mint (bounded state, §99).
            if let Some((&weakest, _)) = self.mints.iter().min_by_key(|(_, m)| m.last_touch) {
                self.mints.remove(&weakest);
            }
        }
        let tpb = self.trades_per_bar;
        let e = self.mints.entry(mint).or_insert_with(|| MintMicro {
            // `interval_ns = trades_per_bar` over the trade-counter clock: the
            // bucket function floor(counter / trades_per_bar) closes a bar every
            // `trades_per_bar` trades exactly. Cannot fail: interval ≥ 1.
            builder: BarBuilder::new(BarSpec::Time {
                interval_ns: tpb,
                epoch_ns: 0,
            })
            .expect("interval clamped >= 1"), // LINT-ALLOW(hot_panic): documented-infallible — `trades_per_bar` is clamped `.max(1)`, so BarSpec::Time interval is always ≥ 1
            bars: VecDeque::with_capacity(BARS_CAP),
            n_trades: 0,
            activity: [(0, 0, 0); ACTIVITY_RING],
            activity_len: 0,
            last_touch: now,
        });
        e.last_touch = now;
        let t = TradeEvent {
            event_id: e.n_trades,
            // Information time IS the per-mint trade counter (module docs):
            // strictly monotonic, so the builder's non-monotonic error is
            // unreachable and the push is infallible in practice.
            ts_ns: e.n_trades,
            price_fp,
            base_qty,
            quote_qty: quote_lamports,
            side: if is_buy { Side::Buy } else { Side::Sell },
        };
        e.n_trades = e.n_trades.saturating_add(1);
        if let Ok(Some(closed)) = e.builder.push(t) {
            // Bounded ring (§99): O(1) front-eviction on the per-trade path (O4).
            if e.bars.len() == BARS_CAP {
                e.bars.pop_front();
            }
            e.bars.push_back(closed);
        }
        let idx = (e.activity_len % ACTIVITY_RING as u64) as usize;
        e.activity[idx] = (now, buyer_entity, quote_lamports);
        e.activity_len = e.activity_len.saturating_add(1);
    }

    /// The swing-structure trend over the mint's closed bars, or `Undefined`
    /// until at least `min_bars` bars exist. Never authorizes — the engine
    /// consumes this reduce-only (module docs).
    #[must_use]
    pub fn trend(&self, mint: &[u8; 32], min_bars: usize) -> TrendStructure {
        match self.mints.get(mint) {
            Some(m) if m.bars.len() >= min_bars.max(SWING_NEIGHBORHOOD * 2 + 1) => {
                m.with_bars(|bars| swing_structure(bars, SWING_NEIGHBORHOOD, SWING_NEIGHBORHOOD))
            }
            _ => TrendStructure::Undefined,
        }
    }

    /// §24 LAW 6: realized volatility (bps) over the mint's most recent `window`
    /// closed bars, via `pump_quant_features::structure_ext::realized_vol_bps`. A
    /// fixed-length window keeps the value comparable across markets (the helper
    /// grows with window length). `None` until at least two bars have closed.
    #[must_use]
    pub fn recent_vol_bps(&self, mint: &[u8; 32], window: usize) -> Option<i128> {
        let m = self.mints.get(mint)?;
        let n = m.bars.len();
        if n < 2 {
            return None;
        }
        let start = n.saturating_sub(window.max(2));
        m.with_bars(|bars| realized_vol_bps(&bars[start..]))
    }

    /// §25 LAW 4: reconstruct the integer [`MarketState`] the setup-archetype
    /// classifier consumes, from the mint's closed bars. `None` until at least two
    /// bars have closed (a single bar has no prior window / structural level).
    ///
    /// The current window is the most recent closed bar; the prior window is the
    /// bar before it (for the compression proxy). Structural levels are the last
    /// swing pivot before the current bar, falling back to the prior bars' max
    /// high / min low when no pivot exists. The anchored VWAP and the net CVD are
    /// folded from the last `window` bars' quote/flow. All integer / fixed-point
    /// (§22); prices saturate into `u64` (realistic AMM prices are well within
    /// range). Reduce-only consumption: the state authorizes nothing — it only
    /// discriminates the archetype tag.
    #[must_use]
    pub fn market_state(&self, mint: &[u8; 32], window: usize) -> Option<MarketState> {
        let m = self.mints.get(mint)?;
        let n = m.bars.len();
        if n < 2 {
            return None;
        }
        Some(m.with_bars(|bars| {
            let cur = &bars[n - 1];
            let prior = &bars[n - 2];
            let head = &bars[..n - 1];
            let sh = swing_highs(bars, SWING_NEIGHBORHOOD, SWING_NEIGHBORHOOD);
            let sl = swing_lows(bars, SWING_NEIGHBORHOOD, SWING_NEIGHBORHOOD);
            let prior_high = sh
                .last()
                .map(|&i| bars[i].high_fp)
                .or_else(|| head.iter().map(|b| b.high_fp).max())
                .unwrap_or(0);
            let prior_low = sl
                .last()
                .map(|&i| bars[i].low_fp)
                .or_else(|| head.iter().map(|b| b.low_fp).min())
                .unwrap_or(0);
            let start = n.saturating_sub(window.max(2));
            let mut num: i128 = 0;
            let mut den: i128 = 0;
            let mut cvd: i128 = 0;
            for b in &bars[start..] {
                num = num.saturating_add(b.close_fp.saturating_mul(i128::from(b.base_volume)));
                den = den.saturating_add(i128::from(b.base_volume));
                cvd = cvd
                    .saturating_add(i128::from(b.buy_base_volume))
                    .saturating_sub(i128::from(b.sell_base_volume));
            }
            let vwap_fp = if den > 0 { num / den } else { cur.close_fp };
            MarketState {
                price_open_fp: to_state_price(cur.open_fp),
                price_fp: to_state_price(cur.close_fp),
                high_extreme_fp: to_state_price(cur.high_fp),
                low_extreme_fp: to_state_price(cur.low_fp),
                prior_high_fp: to_state_price(prior_high),
                prior_low_fp: to_state_price(prior_low),
                vwap_fp: to_state_price(vwap_fp),
                cvd_delta: cvd,
                range_bps: bar_range_bps(cur),
                prior_range_bps: bar_range_bps(prior),
            }
        }))
    }

    /// §24 LAW 11: reconstruct the [`PullbackFeatures`] the
    /// `detect_pullback_continuation` EntryMode leaf consumes, from the mint's
    /// closed bars. `None` until the swing structure is defined (≥ `min_bars`,
    /// itself ≥ `2·neighborhood+1`).
    ///
    /// * `uptrend_confirmed` / `structure_intact` come from the §21.6 swing-
    ///   structure classification (a higher-high/higher-low tape is an uptrend; a
    ///   lower-high/lower-low tape breaks the structure).
    /// * The retest LEVEL is the last swing high strictly below the peak (the
    ///   broken resistance now acting as support); `retest_level_holding` and
    ///   `retest_hold_bars` measure whether the recent bars hold at/above it.
    /// * `pullback_depth_bps` is the retrace from the peak high to the recent
    ///   trough low, in bps of the peak; `pullback_sell_volume_bps` is the sell
    ///   share of recent bar base volume. All integer / fixed-point (§22).
    #[must_use]
    pub fn pullback_features(&self, mint: &[u8; 32], min_bars: usize) -> Option<PullbackFeatures> {
        let m = self.mints.get(mint)?;
        let n = m.bars.len();
        if n < min_bars.max(SWING_NEIGHBORHOOD * 2 + 1) {
            return None;
        }
        Some(m.with_bars(|bars| {
            let trend = swing_structure(bars, SWING_NEIGHBORHOOD, SWING_NEIGHBORHOOD);
            let uptrend_confirmed = trend == TrendStructure::Uptrend;
            let structure_intact = trend != TrendStructure::Downtrend;
            let peak = bars.iter().map(|b| b.high_fp).max().unwrap_or(0);
            // Retest level: the last swing high strictly below the peak (broken
            // resistance retested), else the peak itself (degenerate — nothing held).
            let sh = swing_highs(bars, SWING_NEIGHBORHOOD, SWING_NEIGHBORHOOD);
            let level = sh
                .iter()
                .rev()
                .map(|&i| bars[i].high_fp)
                .find(|&h| h < peak)
                .unwrap_or(peak);
            let cur = &bars[n - 1];
            let retest_level_holding = cur.close_fp >= level;
            // Consecutive most-recent bars whose low holds at/above the retest level.
            let mut hold: u32 = 0;
            for b in bars.iter().rev() {
                if b.low_fp >= level {
                    hold = hold.saturating_add(1);
                } else {
                    break;
                }
            }
            let look = n.min(4);
            let recent = &bars[n - look..];
            let trough = recent.iter().map(|b| b.low_fp).min().unwrap_or(cur.low_fp);
            let pullback_depth_bps = if peak > 0 {
                (((peak - trough).max(0).saturating_mul(10_000)) / peak)
                    .clamp(0, i128::from(u32::MAX)) as u32
            } else {
                0
            };
            let mut buy: u128 = 0;
            let mut sell: u128 = 0;
            for b in recent {
                buy = buy.saturating_add(u128::from(b.buy_base_volume));
                sell = sell.saturating_add(u128::from(b.sell_base_volume));
            }
            let denom = buy.saturating_add(sell);
            let pullback_sell_volume_bps = sell
                .saturating_mul(10_000)
                .checked_div(denom)
                .map_or(0, |v| v.min(u128::from(u32::MAX)) as u32);
            PullbackFeatures {
                uptrend_confirmed,
                structure_intact,
                retest_level_holding,
                retest_hold_bars: hold,
                pullback_depth_bps,
                pullback_sell_volume_bps,
            }
        }))
    }

    /// Aggregates of the trades within the last `window_ticks` logical ticks
    /// (inclusive of `now`), from the fixed activity ring. Distinct-entity count
    /// is exact over the ring (≤ [`ACTIVITY_RING`] samples — a fixed scan).
    #[must_use]
    pub fn activity(&self, mint: &[u8; 32], now: u64, window_ticks: u64) -> ActivityWindow {
        let Some(m) = self.mints.get(mint) else {
            return ActivityWindow::default();
        };
        let live = (m.activity_len.min(ACTIVITY_RING as u64)) as usize;
        let mut w = ActivityWindow::default();
        let mut seen: [u64; ACTIVITY_RING] = [0; ACTIVITY_RING];
        let mut seen_n = 0usize;
        for &(at, entity, quote) in &m.activity[..live] {
            if now.saturating_sub(at) > window_ticks {
                continue;
            }
            w.trades += 1;
            w.volume_lamports = w.volume_lamports.saturating_add(quote);
            if !seen[..seen_n].contains(&entity) {
                seen[seen_n] = entity;
                seen_n += 1;
            }
        }
        w.entities = seen_n as u32;
        w
    }
}

/// Saturating conversion of a fixed-point price (`i128`) into the classifier's
/// `u64` price field: negatives (never a real price) clamp to 0, huge values to
/// `u64::MAX`. Deterministic, integer (§22).
#[inline]
#[must_use]
fn to_state_price(v: i128) -> u64 {
    v.clamp(0, i128::from(u64::MAX)) as u64
}

/// A bar's high-low range as basis points of its open price, saturating into
/// `u32`. Zero when the open is non-positive (degenerate bar). §22.
#[inline]
#[must_use]
fn bar_range_bps(b: &Bar) -> u32 {
    if b.open_fp <= 0 {
        return 0;
    }
    let range = (b.high_fp - b.low_fp).max(0);
    ((range.saturating_mul(10_000)) / b.open_fp).clamp(0, i128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint(tag: u8) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = tag;
        b
    }

    /// Feed `n` trades at the given prices (one per trade), all buys, entity 7.
    fn feed(s: &mut StructureState, m: [u8; 32], prices: &[i128], now: u64) {
        for &p in prices {
            s.record(
                m,
                &TradeObs {
                    now,
                    price_fp: p,
                    base_qty: 1_000,
                    quote_lamports: 500,
                    buyer_entity: 7,
                    is_buy: true,
                },
            );
        }
    }

    #[test]
    fn bars_close_every_trades_per_bar() {
        let mut s = StructureState::new(4);
        let m = mint(1);
        // 4 trades: bar 0 still open (closes when bucket 1 begins).
        feed(&mut s, m, &[100, 101, 102, 103], 1);
        assert_eq!(s.trend(&m, 1), TrendStructure::Undefined);
        // 5th trade opens bucket 1 and closes bar 0.
        feed(&mut s, m, &[104], 1);
        let mm = s.mints.get(&m).unwrap();
        assert_eq!(mm.bars.len(), 1);
        assert_eq!(mm.bars[0].open_fp, 100);
        assert_eq!(mm.bars[0].close_fp, 103);
        assert_eq!(mm.bars[0].trade_count, 4);
    }

    #[test]
    fn rising_zigzag_classifies_uptrend_and_falling_downtrend() {
        // A trend needs SWING PIVOTS (higher highs AND higher lows), not a
        // monotone ladder — a straight line has no pivots and stays Undefined
        // (fade-first: less structure evidence, not more).
        let mut s = StructureState::new(2);
        let m = mint(2);
        // 2-trade bars; closed bars zigzag upward: swing highs 130→150,
        // swing lows 95→115 (the 13th trade closes the 6th bar).
        let up: [i128; 13] = [
            100, 110, 105, 95, 120, 130, 125, 115, 140, 150, 145, 135, 160,
        ];
        feed(&mut s, m, &up, 1);
        assert_eq!(s.trend(&m, 3), TrendStructure::Uptrend);

        let m2 = mint(3);
        // Mirror: swing highs 205→185, swing lows 165→145.
        let down: [i128; 13] = [
            180, 190, 195, 205, 175, 165, 185, 175, 155, 145, 155, 165, 140,
        ];
        feed(&mut s, m2, &down, 1);
        assert_eq!(s.trend(&m2, 3), TrendStructure::Downtrend);
    }

    #[test]
    fn monotone_ladder_stays_undefined() {
        let mut s = StructureState::new(2);
        let m = mint(7);
        let up: Vec<i128> = (0..14).map(|i| 1_000 + i * 10).collect();
        feed(&mut s, m, &up, 1);
        assert_eq!(s.trend(&m, 3), TrendStructure::Undefined);
    }

    #[test]
    fn trend_undefined_below_min_bars() {
        let mut s = StructureState::new(2);
        let m = mint(4);
        feed(&mut s, m, &[100, 101, 102], 1); // one closed bar only
        assert_eq!(s.trend(&m, 3), TrendStructure::Undefined);
    }

    #[test]
    fn activity_window_counts_trades_entities_volume() {
        let mut s = StructureState::new(8);
        let m = mint(5);
        let ob = |now: u64, entity: u64, is_buy: bool| TradeObs {
            now,
            price_fp: 100,
            base_qty: 1,
            quote_lamports: 400,
            buyer_entity: entity,
            is_buy,
        };
        s.record(m, &ob(10, 1, true));
        s.record(m, &ob(11, 2, true));
        s.record(m, &ob(12, 1, false));
        // Outside the window relative to now=30, window=10.
        let w = s.activity(&m, 30, 10);
        assert_eq!(w, ActivityWindow::default());
        // Inside a wide window: 3 trades, 2 entities, 1200 lamports.
        let w = s.activity(&m, 12, 10);
        assert_eq!(w.trades, 3);
        assert_eq!(w.entities, 2);
        assert_eq!(w.volume_lamports, 1_200);
    }

    #[test]
    fn bounded_eviction_holds_cap() {
        let mut s = StructureState::new(8);
        for i in 0..(STRUCT_TRACK_CAP + 10) {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&(i as u64).to_le_bytes());
            s.record(
                b,
                &TradeObs {
                    now: i as u64,
                    price_fp: 100,
                    base_qty: 1,
                    quote_lamports: 1,
                    buyer_entity: 1,
                    is_buy: true,
                },
            );
        }
        assert!(s.mints.len() <= STRUCT_TRACK_CAP);
    }

    #[test]
    fn ring_overwrite_keeps_recent_activity_exact() {
        let mut s = StructureState::new(64);
        let m = mint(6);
        // Overfill the ring; only the last ACTIVITY_RING samples survive.
        for i in 0..(ACTIVITY_RING as u64 + 5) {
            s.record(
                m,
                &TradeObs {
                    now: i,
                    price_fp: 100,
                    base_qty: 1,
                    quote_lamports: 10,
                    buyer_entity: i,
                    is_buy: true,
                },
            );
        }
        let w = s.activity(&m, ACTIVITY_RING as u64 + 4, u64::MAX);
        assert_eq!(w.trades, ACTIVITY_RING as u32);
        assert_eq!(w.entities, ACTIVITY_RING as u32);
    }
}
