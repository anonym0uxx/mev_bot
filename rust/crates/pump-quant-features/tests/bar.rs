//! Leaf tests for the bar builder (constitution 21.6), with computed expectations.

use pump_quant_features::bar::{Bar, BarBuilder, BarSpec};
use pump_quant_features::types::{FeatureError, Side, TradeEvent};

fn trade(id: u64, ts: u64, price: i128, base: u64, quote: u64, side: Side) -> TradeEvent {
    TradeEvent {
        event_id: id,
        ts_ns: ts,
        price_fp: price,
        base_qty: base,
        quote_qty: quote,
        side,
    }
}

#[test]
fn invalid_config_rejected() {
    assert_eq!(
        BarBuilder::new(BarSpec::Time {
            interval_ns: 0,
            epoch_ns: 0
        })
        .unwrap_err(),
        FeatureError::InvalidConfiguration
    );
    assert_eq!(
        BarBuilder::new(BarSpec::Volume { threshold_base: 0 }).unwrap_err(),
        FeatureError::InvalidConfiguration
    );
}

#[test]
fn time_bars_bucket_and_ohlcv() {
    let mut b = BarBuilder::new(BarSpec::Time {
        interval_ns: 100,
        epoch_ns: 0,
    })
    .unwrap();

    // Bucket 0: two trades.
    assert_eq!(b.push(trade(1, 0, 10, 5, 50, Side::Buy)).unwrap(), None);
    assert_eq!(b.push(trade(2, 50, 12, 3, 36, Side::Buy)).unwrap(), None);
    // Bucket 1 begins -> closes bucket 0.
    let closed0 = b
        .push(trade(3, 120, 11, 2, 22, Side::Sell))
        .unwrap()
        .unwrap();
    assert_eq!(
        closed0,
        Bar {
            open_time_ns: 0,
            close_time_ns: 50,
            open_fp: 10,
            high_fp: 12,
            low_fp: 10,
            close_fp: 12,
            base_volume: 8,
            quote_volume: 86,
            buy_base_volume: 8,
            sell_base_volume: 0,
            trade_count: 2,
            first_event_id: 1,
            last_event_id: 2,
        }
    );

    // Bucket 2 begins -> closes bucket 1 (only the sell at t=120).
    let closed1 = b
        .push(trade(4, 250, 9, 4, 36, Side::Sell))
        .unwrap()
        .unwrap();
    assert_eq!(closed1.open_time_ns, 120);
    assert_eq!(closed1.close_time_ns, 120);
    assert_eq!(closed1.base_volume, 2);
    assert_eq!(closed1.sell_base_volume, 2);
    assert_eq!(closed1.buy_base_volume, 0);
    assert_eq!(closed1.trade_count, 1);

    // Flush the final open bar (bucket 2).
    let last = b.flush().unwrap();
    assert_eq!(last.open_time_ns, 250);
    assert_eq!(last.close_fp, 9);
    assert_eq!(last.base_volume, 4);
    assert!(b.flush().is_none());
}

#[test]
fn time_bars_skip_empty_buckets() {
    // A large time gap must NOT fabricate empty candles: only buckets with trades
    // emit bars (constitution 21.6 no fabricated/stale candles).
    let mut b = BarBuilder::new(BarSpec::Time {
        interval_ns: 100,
        epoch_ns: 0,
    })
    .unwrap();
    assert_eq!(b.push(trade(1, 10, 5, 1, 5, Side::Buy)).unwrap(), None);
    // Jump 5 buckets ahead: closes bucket 0, opens bucket 5, no empties in between.
    let closed = b.push(trade(2, 550, 6, 1, 6, Side::Buy)).unwrap().unwrap();
    assert_eq!(closed.open_time_ns, 10);
    assert_eq!(closed.trade_count, 1);
    let last = b.flush().unwrap();
    assert_eq!(last.open_time_ns, 550);
}

#[test]
fn volume_bars_close_on_threshold() {
    let mut b = BarBuilder::new(BarSpec::Volume { threshold_base: 10 }).unwrap();
    assert_eq!(b.push(trade(1, 0, 10, 4, 40, Side::Buy)).unwrap(), None); // cum 4
    assert_eq!(b.push(trade(2, 1, 10, 3, 30, Side::Buy)).unwrap(), None); // cum 7
    let bar = b.push(trade(3, 2, 11, 5, 55, Side::Sell)).unwrap().unwrap(); // cum 12 -> close
    assert_eq!(bar.base_volume, 12);
    assert_eq!(bar.trade_count, 3);
    assert_eq!(bar.buy_base_volume, 7);
    assert_eq!(bar.sell_base_volume, 5);
    assert_eq!(bar.open_fp, 10);
    assert_eq!(bar.close_fp, 11);
}

#[test]
fn volume_bars_oversized_single_trade_closes_immediately() {
    let mut b = BarBuilder::new(BarSpec::Volume { threshold_base: 10 }).unwrap();
    let bar = b
        .push(trade(1, 0, 10, 20, 200, Side::Buy))
        .unwrap()
        .unwrap();
    assert_eq!(bar.base_volume, 20);
    assert_eq!(bar.trade_count, 1);
    assert!(b.open_bar().is_none());
}

#[test]
fn non_monotonic_timestamp_is_rejected() {
    let mut b = BarBuilder::new(BarSpec::Volume {
        threshold_base: 100,
    })
    .unwrap();
    b.push(trade(1, 500, 10, 1, 10, Side::Buy)).unwrap();
    let err = b.push(trade(2, 499, 10, 1, 10, Side::Buy)).unwrap_err();
    assert_eq!(
        err,
        FeatureError::NonMonotonicTimestamp {
            previous_ns: 500,
            offending_ns: 499,
        }
    );
}

#[test]
fn total_volume_conservation_across_bars() {
    // Independent check: summing base volume over all emitted+flushed bars equals
    // the total base volume pushed, for volume bars.
    let mut b = BarBuilder::new(BarSpec::Volume { threshold_base: 7 }).unwrap();
    let sizes = [3u64, 4, 5, 2, 9, 1, 6, 8];
    let mut emitted_total = 0u64;
    let mut ts = 0u64;
    for (i, &s) in sizes.iter().enumerate() {
        ts += 1;
        if let Some(bar) = b
            .push(trade(i as u64, ts, 10, s, s * 10, Side::Buy))
            .unwrap()
        {
            emitted_total += bar.base_volume;
        }
    }
    if let Some(bar) = b.flush() {
        emitted_total += bar.base_volume;
    }
    assert_eq!(emitted_total, sizes.iter().sum::<u64>());
}
