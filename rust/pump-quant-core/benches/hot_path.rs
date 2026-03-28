//! Hot-path benchmarks for the pump-quant MEV backrunner.
//!
//! Targets:
//!   gate_stack_pass       < 500ns
//!   gate_stack_reject     < 500ns
//!   scorer_compute        < 1µs
//!   mint_history_push     < 2µs
//!   simulate_buy          < 100ns
//!   simulate_sell         < 100ns
//!   full_decision_pass    < 5µs

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use pump_quant_core::core::{MintHistory, MintHistoryMap, TradeRecord};
use pump_quant_core::engine::bonding_curve::{simulate_buy, simulate_sell};
use pump_quant_core::engine::gates::{GateConfig, GateStack};
use pump_quant_core::engine::scorer::{ScoreConfig, Scorer};
use pump_quant_core::feeds::{FeedSource, TradeEvent};

// ─── Helpers ────────────────────────────────────────────────────────

fn make_trade_event(sol_amount: u64, vsol: u64, is_buy: bool) -> TradeEvent {
    TradeEvent {
        mint: [0xAAu8; 32],
        trader: [0x01u8; 32],
        sig: [0u8; 64],
        sig_prefix: [0u8; 8],
        sol_amount,
        token_amount: 1_000_000,
        vsol_reserves: vsol,
        vtoken_reserves: 800_000_000_000u64,
        market_cap_sol: 50_000_000_000,
        slot: 100,
        timestamp_ms: 1_000_000,
        is_buy,
        source: FeedSource::PumpPortal,
        bonding_curve: [0x02u8; 32],
        assoc_bonding_curve: [0x03u8; 32],
    }
}

fn make_trade_record(ts_ms: u64, sol: u64, is_buy: bool, trader_byte: u8) -> TradeRecord {
    TradeRecord {
        timestamp_ms: ts_ms,
        sol_amount: sol,
        token_amount: 1_000_000,
        is_buy,
        _pad0: [0; 7],
        trader: [trader_byte; 32],
        vsol_reserves: 10_000_000_000,
        vtoken_reserves: 800_000_000_000,
        market_cap_sol: 50_000_000_000,
        slot: 100,
        sig_prefix: [trader_byte; 8],
        _pad1: [0; 24],
    }
}

fn build_warm_history(now_ms: u64) -> MintHistory {
    let mint = [0xAAu8; 32];
    let mut history = MintHistory::new(mint, now_ms - 30_000);

    // Fill with 60 trades over the last 30 seconds — realistic warm state
    for i in 0..60u64 {
        let ts = now_ms - 30_000 + i * 500; // one trade every 500ms
        let trader_byte = (i % 10) as u8 + 1; // 10 unique buyers
        let is_buy = i % 5 != 0; // 80% buys, 20% sells
        let sol = 200_000_000 + (i % 5) * 100_000_000; // 0.2-0.6 SOL
        let trade = make_trade_record(ts, sol, is_buy, trader_byte);
        history.push(trade, ts);
    }
    history
}

// ─── 2a. Gate stack evaluation ──────────────────────────────────────

fn bench_gate_stack(c: &mut Criterion) {
    let gate_stack = GateStack::new(GateConfig::default());

    // Passing event: 0.5 SOL buy, 10 SOL vSol
    let trade = make_trade_event(500_000_000, 10_000_000_000, true);

    // Pre-computed aggregates that pass all gates
    let history_age_ms = 10_000u64;
    let unique_buyers_30s = 5u16;
    let buy_count_1s = 2u16;
    let buy_count_2s = 3u16;
    let buy_count_5s = 5u16;
    let sell_count_5s = 1u16;
    let volume_sol_5s = 5_000_000_000u64;
    let vsol_delta_3s = 500_000_000u64;
    let time_since_last_buy_ms = 500u64;
    let creator_sell_at_ms = 0u64;
    let now_ms = 1_010_000u64;
    let score = 0.5f64;

    c.bench_function("gate_stack_pass", |b| {
        b.iter(|| {
            black_box(gate_stack.evaluate(
                black_box(&trade),
                black_box(history_age_ms),
                black_box(unique_buyers_30s),
                black_box(buy_count_1s),
                black_box(buy_count_2s),
                black_box(buy_count_5s),
                black_box(sell_count_5s),
                black_box(volume_sol_5s),
                black_box(vsol_delta_3s),
                black_box(time_since_last_buy_ms),
                black_box(creator_sell_at_ms),
                black_box(now_ms),
                black_box(score),
            ))
        })
    });

    // Rejecting event: trigger too small (gate 2, very early rejection)
    let small_trade = make_trade_event(10_000, 10_000_000_000, true);

    c.bench_function("gate_stack_reject_early", |b| {
        b.iter(|| {
            black_box(gate_stack.evaluate(
                black_box(&small_trade),
                black_box(history_age_ms),
                black_box(unique_buyers_30s),
                black_box(buy_count_1s),
                black_box(buy_count_2s),
                black_box(buy_count_5s),
                black_box(sell_count_5s),
                black_box(volume_sol_5s),
                black_box(vsol_delta_3s),
                black_box(time_since_last_buy_ms),
                black_box(creator_sell_at_ms),
                black_box(now_ms),
                black_box(score),
            ))
        })
    });
}

// ─── 2b. Scorer ─────────────────────────────────────────────────────

fn bench_scorer(c: &mut Criterion) {
    let scorer = Scorer::new(ScoreConfig::default(), 3_000_000_000, 85_000_000_000);

    let trigger_sol = 500_000_000u64;
    let vsol = 10_000_000_000u64;
    let unique_buyers = 5u16;
    let buy_count_1s = 2u16;
    let buy_count_2s = 3u16;
    let volume_5s = 3_000_000_000u64;
    let max_wallet_vol = 500_000_000u64;
    let total_buy_vol_30s = 5_000_000_000u64;

    c.bench_function("scorer_compute", |b| {
        b.iter(|| {
            black_box(scorer.compute(
                black_box(trigger_sol),
                black_box(vsol),
                black_box(unique_buyers),
                black_box(buy_count_1s),
                black_box(buy_count_2s),
                black_box(volume_5s),
                black_box(max_wallet_vol),
                black_box(total_buy_vol_30s),
            ))
        })
    });
}

// ─── 2c. MintHistory push + aggregate recompute ────────────────────

fn bench_mint_history(c: &mut Criterion) {
    let now_ms = 1_030_000u64;
    let base_history = build_warm_history(now_ms);

    // The new trade to push
    let new_trade = make_trade_record(now_ms, 300_000_000, true, 0xFF);

    c.bench_function("mint_history_push", |b| {
        b.iter_batched(
            || base_history.clone(),
            |mut history| {
                history.push(black_box(new_trade), black_box(now_ms));
                history
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

// ─── 2d. Bonding curve simulation ──────────────────────────────────

fn bench_bonding_curve(c: &mut Criterion) {
    c.bench_function("simulate_buy", |b| {
        b.iter(|| {
            black_box(simulate_buy(
                black_box(38_000_000_000),
                black_box(800_000_000_000),
                black_box(120_000_000),
                black_box(100),
            ))
        })
    });

    c.bench_function("simulate_sell", |b| {
        b.iter(|| {
            black_box(simulate_sell(
                black_box(38_000_000_000),
                black_box(800_000_000_000),
                black_box(1_000_000_000),
            ))
        })
    });
}

// ─── 2e. Full gate → score pipeline ────────────────────────────────

fn bench_full_decision(c: &mut Criterion) {
    let now_ms = 1_030_000u64;
    let mint = [0xAAu8; 32];

    // Pre-warm the MintHistoryMap
    let mut mint_map = MintHistoryMap::with_capacity(1024);
    {
        let history = mint_map.get_or_insert(&mint, now_ms - 30_000);
        // Fill with 60 trades
        for i in 0..60u64 {
            let ts = now_ms - 30_000 + i * 500;
            let trader_byte = (i % 10) as u8 + 1;
            let is_buy = i % 5 != 0;
            let sol = 200_000_000 + (i % 5) * 100_000_000;
            let trade = make_trade_record(ts, sol, is_buy, trader_byte);
            history.push(trade, ts);
        }
    }

    let gate_stack = GateStack::new(GateConfig::default());
    let scorer = Scorer::new(ScoreConfig::default(), 3_000_000_000, 85_000_000_000);

    // Incoming trigger event
    let event = make_trade_event(500_000_000, 10_000_000_000, true);

    // Build matching trade record for push
    let new_record = TradeRecord {
        timestamp_ms: now_ms,
        sol_amount: event.sol_amount,
        token_amount: event.token_amount,
        is_buy: event.is_buy,
        _pad0: [0; 7],
        trader: event.trader,
        vsol_reserves: event.vsol_reserves,
        vtoken_reserves: event.vtoken_reserves,
        market_cap_sol: event.market_cap_sol,
        slot: event.slot,
        sig_prefix: event.sig_prefix,
        _pad1: [0; 24],
    };

    c.bench_function("full_decision_pass", |b| {
        b.iter_batched(
            || mint_map.get(&mint).unwrap().clone(),
            |mut history| {
                // 1. Push trade into history (recomputes aggregates)
                history.push(black_box(new_record), black_box(now_ms));

                // 2. Compute score
                let sc = scorer.compute(
                    event.sol_amount,
                    event.vsol_reserves,
                    history.cached_unique_buyers_30s,
                    history.cached_buy_count_1s,
                    history.cached_buy_count_2s,
                    history.cached_volume_sol_5s,
                    0, // max_wallet_volume (would need tracking)
                    history.cached_volume_sol_5s * 6, // approximate 30s volume
                );

                // 3. Evaluate gate stack
                let age_ms = now_ms.saturating_sub(history.first_seen_ms);
                let vsol_delta_3s = event
                    .vsol_reserves
                    .saturating_sub(history.cached_vsol_oldest_3s);
                let time_since_last = 100u64; // simulated

                let _result = gate_stack.evaluate(
                    &event,
                    age_ms,
                    history.cached_unique_buyers_30s,
                    history.cached_buy_count_1s,
                    history.cached_buy_count_2s,
                    history.cached_buy_count_5s,
                    history.cached_sell_count_5s,
                    history.cached_volume_sol_5s,
                    vsol_delta_3s,
                    time_since_last,
                    history.creator_sell_at_ms,
                    now_ms,
                    sc.final_score,
                );

                black_box(_result)
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

// ─── Group + Main ───────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_gate_stack,
    bench_scorer,
    bench_mint_history,
    bench_bonding_curve,
    bench_full_decision,
);
criterion_main!(benches);
