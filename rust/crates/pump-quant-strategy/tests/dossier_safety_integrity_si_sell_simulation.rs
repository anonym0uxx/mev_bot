// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_sell_simulation').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn empty_reserves_unprovable() {
    let pos = Position {
        token_amount: 100,
        mint: 1,
    };
    let empty = DecodedMarket {
        base_reserve: 0,
        quote_reserve: 0,
        constructible: true,
    };
    assert_eq!(
        prove_sellable(&pos, &empty),
        Err(SellUnprovable::InsufficientLiquidity)
    );
}
#[test]
fn unconstructible_unprovable() {
    let pos = Position {
        token_amount: 100,
        mint: 1,
    };
    let mkt = DecodedMarket {
        base_reserve: 1_000_000,
        quote_reserve: 1_000_000,
        constructible: false,
    };
    assert_eq!(
        prove_sellable(&pos, &mkt),
        Err(SellUnprovable::Unconstructible)
    );
}
#[test]
fn simulatable_sell_carries_out_amount() {
    let pos = Position {
        token_amount: 1_000,
        mint: 1,
    };
    let mkt = DecodedMarket {
        base_reserve: 1_000_000,
        quote_reserve: 1_000_000,
        constructible: true,
    };
    let proof = prove_sellable(&pos, &mkt).expect("should be provable");
    // out = 1_000_000 * 1000 / (1_000_000 + 1000) = 999
    assert_eq!(proof.out_amount, 999);
}
