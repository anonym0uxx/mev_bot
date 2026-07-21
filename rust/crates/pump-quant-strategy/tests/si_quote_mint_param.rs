#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn usdc_and_sol_decimals_differ() {
    let mkt = Market {
        fee_bps: 100,
        fixed_cost_whole: 1,
    };
    let usdc = round_trip_cost_quote(1_000_000, &mkt, QuoteMint::Usdc { decimals: 6 }).unwrap();
    let sol = round_trip_cost_quote(1_000_000, &mkt, QuoteMint::Sol { decimals: 9 }).unwrap();
    // fee round trip = 1_000_000 * 100/10000 * 2 = 20_000 for both;
    // fixed: usdc = 1 * 1e6, sol = 1 * 1e9 -> different totals.
    assert_eq!(usdc, 20_000 + 1_000_000);
    assert_eq!(sol, 20_000 + 1_000_000_000);
    assert_ne!(usdc, sol);
}
#[test]
fn undecoded_quote_refuses() {
    let mkt = Market {
        fee_bps: 100,
        fixed_cost_whole: 1,
    };
    assert_eq!(
        round_trip_cost_quote(1_000_000, &mkt, QuoteMint::Undecoded),
        None
    );
}
