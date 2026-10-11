//! Engine-level semantics: an INSTRUCTION HINT is not an executed trade.
//! Through the real `Engine::tick` entry point:
//!   * a hint carrying an UNLIMITED bound (u64::MAX) or a FINITE bound that differs from the real spend never reaches the
//!     numeric trade ring, trade count, imbalance or volume aggregates; it still advances the feed clock (discovery/clock
//!     hint only);
//!   * an executed priced print (the real executed quantity) does reach them, exactly once;
//!   * replaying the same executed print (same event identity) adds nothing the second time.
//!
//! Synthetic quantities; the captured-transaction side of this contract is in `pump-quant-junction/tests`.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, FeatureBasis, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;

const MINT: [u8; 32] = [0x5A; 32];
const T0: i64 = 1_800_000_000_000;

fn mint() -> DomainMint {
    DomainMint::from_bytes(MINT)
}

fn engine() -> Engine {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    Engine::new(cfg, RunMode::Paper)
}

fn hint(quote: u64, base: i64, t: i64) -> AppEvent {
    // exactly what an instruction-derived pump.fun/PumpSwap print looks like after the semantics fix:
    // no price, no event identity, no corpus basis, quote 0, requested token quantity in `signed_base`.
    AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 0,
        quote_lamports: quote,
        liquidity_lamports: 0,
        signed_base: base,
        buyer_entity: 77,
        age_slots: 0,
        recv_unix_ms: Some(t),
        trader_pubkey: Some([9; 32]),
        slot: Some(1_000),
        fee_lamports: Some(5_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpSwap),
        event_id: None,
        feature: None,
    }
}

fn executed(quote: u64, base: i64, t: i64, id: u128) -> AppEvent {
    AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 40_000,
        quote_lamports: quote,
        liquidity_lamports: 30_000_000_000,
        signed_base: base,
        buyer_entity: 78,
        age_slots: 10,
        recv_unix_ms: Some(t),
        trader_pubkey: Some([8; 32]),
        slot: Some(1_001),
        fee_lamports: Some(5_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: Some(id),
        feature: Some(FeatureBasis {
            sol_lamports: -(quote as i64),
            tokens_raw: base,
            trader: [8; 32],
        }),
    }
}

fn agg(e: &Engine) -> Option<(u32, u64, u64)> {
    e.numeric_features(mint())
        .map(|f| (f.trades_observed, f.volume_lamports, f.max_trade_lamports))
}

#[test]
fn unlimited_and_finite_instruction_bounds_never_enter_trade_aggregates() {
    let mut e = engine();
    // unlimited bound: the instruction's max-spend was u64::MAX; the requested token quantity is a plain number
    e.tick(hint(0, 584_500, T0));
    // finite bound that differs from any real spend: quote must stay 0 (the producer sets it so); even if a producer
    // regressed and put a limit in `quote_lamports`, a hint must still not aggregate.
    e.tick(hint(68_424_999, 21_430_845_220, T0 + 1));
    e.tick(hint(u64::MAX, 1, T0 + 2));
    assert_eq!(
        agg(&e),
        None,
        "hints (even a regressed producer carrying a limit as a quote) create no numeric evidence"
    );
}

#[test]
fn a_real_executed_quantity_reaches_the_aggregates_exactly_once_and_replay_adds_nothing() {
    let mut e = engine();
    e.tick(hint(u64::MAX, 584_500, T0)); // pollution attempt first
    e.tick(executed(56_983_656, 21_430_845_220, T0 + 10, 0xABCD));
    let after_first = agg(&e).expect("the executed print is numeric evidence");
    assert_eq!(after_first.0, 1, "one executed trade");
    assert_eq!(after_first.1, 56_983_656, "volume is the EXECUTED amount");
    assert!(after_first.2 < u64::MAX / 2, "no bound in the aggregate");
    // replay: identical event identity delivered again (reconnect/overlap) adds nothing
    e.tick(executed(56_983_656, 21_430_845_220, T0 + 10, 0xABCD));
    assert_eq!(
        agg(&e),
        Some(after_first),
        "a replayed executed event is not aggregated twice"
    );
    // a DISTINCT executed event does count
    e.tick(executed(10_000_000, 3_000_000_000, T0 + 20, 0xABCE));
    assert_eq!(agg(&e).map(|a| a.0), Some(2));
}
