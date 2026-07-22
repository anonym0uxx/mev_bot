//! Streaming bar builder over canonical trade flow (constitution 21.6).
//!
//! Responsibility: fold an ordered stream of [`TradeEvent`]s into OHLCV bars,
//! either fixed-interval **time bars** or cumulative **volume bars**, built
//! *primarily from our own canonical flow* — the only leakage-proof, wash-
//! screenable source. Every emitted [`Bar`] binds back to the first and last
//! event it covers and never peeks past the trade that closed it, so bars are
//! point-in-time safe by construction (constitution 20). All arithmetic is
//! integer/fixed-point (constitution 22); the reducer holds at most one open bar,
//! so memory is bounded (constitution 57).

use crate::types::{EventId, FeatureError, Side, TradeEvent};

/// An OHLCV bar with order-flow decomposition and provenance (constitution 21.6).
///
/// Prices are fixed-point [`i128`] in [`crate::types::PRICE_SCALE`] units. Volumes
/// are integer base/quote units. Buy/sell decomposition supports the 21.7 order-
/// flow features without recomputation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    /// Information time of the first trade in the bar (bar open time).
    pub open_time_ns: u64,
    /// Information time of the last trade in the bar (bar close time). The bar
    /// contains no information after this instant (constitution 20).
    pub close_time_ns: u64,
    /// Price of the first trade.
    pub open_fp: i128,
    /// Maximum trade price in the bar.
    pub high_fp: i128,
    /// Minimum trade price in the bar.
    pub low_fp: i128,
    /// Price of the last trade.
    pub close_fp: i128,
    /// Total base units traded.
    pub base_volume: u64,
    /// Total quote units traded.
    pub quote_volume: u64,
    /// Base units bought by aggressors.
    pub buy_base_volume: u64,
    /// Base units sold by aggressors.
    pub sell_base_volume: u64,
    /// Number of trades folded into the bar.
    pub trade_count: u32,
    /// Provenance: first event id covered (constitution 21.6 bind-to-flow).
    pub first_event_id: EventId,
    /// Provenance: last event id covered.
    pub last_event_id: EventId,
}

impl Bar {
    fn open(t: &TradeEvent) -> Self {
        let (buy, sell) = match t.side {
            Side::Buy => (t.base_qty, 0),
            Side::Sell => (0, t.base_qty),
        };
        Self {
            open_time_ns: t.ts_ns,
            close_time_ns: t.ts_ns,
            open_fp: t.price_fp,
            high_fp: t.price_fp,
            low_fp: t.price_fp,
            close_fp: t.price_fp,
            base_volume: t.base_qty,
            quote_volume: t.quote_qty,
            buy_base_volume: buy,
            sell_base_volume: sell,
            trade_count: 1,
            first_event_id: t.event_id,
            last_event_id: t.event_id,
        }
    }

    /// Fold one trade into an open bar. Volumes use `saturating_add` as an explicit
    /// overflow contract (constitution 22): a bar's cumulative volume saturates at
    /// `u64::MAX`/`u32::MAX` rather than wrapping, which for the volume-bar close
    /// test still trips the threshold correctly.
    fn absorb(&mut self, t: &TradeEvent) {
        self.close_time_ns = t.ts_ns;
        self.close_fp = t.price_fp;
        self.last_event_id = t.event_id;
        if t.price_fp > self.high_fp {
            self.high_fp = t.price_fp;
        }
        if t.price_fp < self.low_fp {
            self.low_fp = t.price_fp;
        }
        self.base_volume = self.base_volume.saturating_add(t.base_qty);
        self.quote_volume = self.quote_volume.saturating_add(t.quote_qty);
        match t.side {
            Side::Buy => self.buy_base_volume = self.buy_base_volume.saturating_add(t.base_qty),
            Side::Sell => self.sell_base_volume = self.sell_base_volume.saturating_add(t.base_qty),
        }
        self.trade_count = self.trade_count.saturating_add(1);
    }
}

/// Bar aggregation policy (constitution 21.6 multi-timeframe bars).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarSpec {
    /// Fixed-interval time bars. A trade at time `t` belongs to bucket
    /// `(t - epoch_ns) / interval_ns`. Bars are emitted sparsely: only buckets
    /// that actually contained a trade produce a bar (empty buckets are skipped,
    /// so the builder never fabricates zero-volume candles — constitution 21.6
    /// "detect and reject missing/stale candles").
    Time {
        /// Bucket width in nanoseconds (must be non-zero).
        interval_ns: u64,
        /// Origin of the bucket grid.
        epoch_ns: u64,
    },
    /// Cumulative volume bars. The open bar closes when its `base_volume` reaches
    /// or exceeds `threshold_base`. Trades are atomic — never split across bars —
    /// so a single oversized trade closes a bar on its own (documented, so replay
    /// is deterministic).
    Volume {
        /// Base-unit volume that triggers a bar close (must be non-zero).
        threshold_base: u64,
    },
}

/// Streaming, memory-bounded bar reducer (constitution 21.6/22/57).
///
/// Feed trades in non-decreasing `ts_ns` order via [`BarBuilder::push`]; each call
/// returns `Some(bar)` when a bar closes. Call [`BarBuilder::flush`] to emit the
/// final partial bar. The builder holds at most one open bar.
#[derive(Debug, Clone)]
pub struct BarBuilder {
    spec: BarSpec,
    open: Option<Bar>,
    /// Bucket index of the open time bar (`Time` spec only).
    open_bucket: u64,
    /// Last ingested timestamp, for the monotonicity invariant.
    last_ts_ns: Option<u64>,
}

impl BarBuilder {
    /// Create a builder for `spec`. Returns [`FeatureError::InvalidConfiguration`]
    /// if the interval, epoch grid, or threshold is degenerate (zero interval or
    /// zero threshold), which has no well-defined bucketing.
    pub fn new(spec: BarSpec) -> Result<Self, FeatureError> {
        match spec {
            BarSpec::Time { interval_ns: 0, .. } => return Err(FeatureError::InvalidConfiguration),
            BarSpec::Volume { threshold_base: 0 } => {
                return Err(FeatureError::InvalidConfiguration)
            }
            _ => {}
        }
        Ok(Self {
            spec,
            open: None,
            open_bucket: 0,
            last_ts_ns: None,
        })
    }

    /// Bucket index for a timestamp under a `Time` spec.
    fn bucket_of(interval_ns: u64, epoch_ns: u64, ts_ns: u64) -> u64 {
        // ts is monotonic and, in practice, >= epoch; saturating_sub makes any
        // pre-epoch trade fall into bucket 0 deterministically rather than wrapping.
        ts_ns.saturating_sub(epoch_ns) / interval_ns
    }

    /// Ingest one trade. Returns `Some(bar)` if this trade caused a bar to close
    /// (the *previous* bar for time bars, or the just-completed bar for volume
    /// bars). Enforces non-decreasing information time (constitution 20): a trade
    /// older than the last one is rejected with
    /// [`FeatureError::NonMonotonicTimestamp`] rather than silently reordered.
    pub fn push(&mut self, t: TradeEvent) -> Result<Option<Bar>, FeatureError> {
        if let Some(prev) = self.last_ts_ns {
            if t.ts_ns < prev {
                return Err(FeatureError::NonMonotonicTimestamp {
                    previous_ns: prev,
                    offending_ns: t.ts_ns,
                });
            }
        }
        self.last_ts_ns = Some(t.ts_ns);

        match self.spec {
            BarSpec::Time {
                interval_ns,
                epoch_ns,
            } => Ok(self.push_time(interval_ns, epoch_ns, t)),
            BarSpec::Volume { threshold_base } => Ok(self.push_volume(threshold_base, t)),
        }
    }

    fn push_time(&mut self, interval_ns: u64, epoch_ns: u64, t: TradeEvent) -> Option<Bar> {
        let bucket = Self::bucket_of(interval_ns, epoch_ns, t.ts_ns);
        match &mut self.open {
            Some(bar) if bucket == self.open_bucket => {
                bar.absorb(&t);
                None
            }
            _ => {
                // New bucket (or first trade): close and emit any open bar, then
                // start a fresh one. Empty intervening buckets are skipped, so no
                // fabricated candles are produced.
                let closed = self.open.take();
                self.open = Some(Bar::open(&t));
                self.open_bucket = bucket;
                closed
            }
        }
    }

    fn push_volume(&mut self, threshold_base: u64, t: TradeEvent) -> Option<Bar> {
        match &mut self.open {
            Some(bar) => {
                bar.absorb(&t);
                if bar.base_volume >= threshold_base {
                    self.open.take()
                } else {
                    None
                }
            }
            None => {
                let bar = Bar::open(&t);
                if bar.base_volume >= threshold_base {
                    // Oversized first trade closes its own bar immediately.
                    Some(bar)
                } else {
                    self.open = Some(bar);
                    None
                }
            }
        }
    }

    /// Emit the final open bar, if any, without waiting for a closing trade. After
    /// `flush` the builder holds no open bar (but retains its monotonicity cursor).
    pub fn flush(&mut self) -> Option<Bar> {
        self.open.take()
    }

    /// Borrow the currently open (not yet closed) bar, for inspection.
    #[must_use]
    pub fn open_bar(&self) -> Option<&Bar> {
        self.open.as_ref()
    }
}
