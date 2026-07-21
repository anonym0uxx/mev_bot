#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn empty_reserves_unprovable() {
    let pos = Position { token_amount: 100, mint: 1 };
    let empty = DecodedMarket { base_reserve: 0, quote_reserve: 0, constructible: true };
    assert_eq!(prove_sellable(&pos, &empty), Err(SellUnprovable::InsufficientLiquidity));
}
#[test]
fn unconstructible_unprovable() {
    let pos = Position { token_amount: 100, mint: 1 };
    let mkt = DecodedMarket { base_reserve: 1_000_000, quote_reserve: 1_000_000, constructible: false };
    assert_eq!(prove_sellable(&pos, &mkt), Err(SellUnprovable::Unconstructible));
}
#[test]
fn simulatable_sell_carries_out_amount() {
    let pos = Position { token_amount: 1_000, mint: 1 };
    let mkt = DecodedMarket { base_reserve: 1_000_000, quote_reserve: 1_000_000, constructible: true };
    let proof = prove_sellable(&pos, &mkt).expect("should be provable");
    // out = 1_000_000 * 1000 / (1_000_000 + 1000) = 999
    assert_eq!(proof.out_amount, 999);
}
