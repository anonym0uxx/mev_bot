#![allow(unused_imports)]
use pump_quant_protocol::ix::*;

#[test]
fn buy_data_matches_manual_layout() {
    let min_tokens_out = 1_234_567u64;
    let max_sol_cost = 9_876_543_210u64;
    let data = build_buy_ix(BuyParams {
        min_tokens_out,
        max_sol_cost,
    });

    // Reconstruct expected bytes independently.
    let mut want = Vec::new();
    want.extend_from_slice(&[102, 6, 61, 18, 1, 218, 235, 234]);
    want.extend_from_slice(&min_tokens_out.to_le_bytes());
    want.extend_from_slice(&max_sol_cost.to_le_bytes());

    assert_eq!(data, want);
    assert_eq!(data.len(), 24);
    // Legacy: minTokens at offset 8, solAmount at offset 16.
    assert_eq!(&data[0..8], &BUY_DISCRIMINATOR);
    assert_eq!(
        u64::from_le_bytes(data[8..16].try_into().unwrap()),
        min_tokens_out
    );
    assert_eq!(
        u64::from_le_bytes(data[16..24].try_into().unwrap()),
        max_sol_cost
    );
}

#[test]
fn sell_data_matches_manual_layout() {
    let token_amount = 42_000_000_000u64;
    let min_sol_out = 555_000_000u64;
    let data = build_sell_ix(SellParams {
        token_amount,
        min_sol_out,
    });

    let mut want = Vec::new();
    want.extend_from_slice(&[51, 230, 133, 164, 1, 127, 131, 173]);
    want.extend_from_slice(&token_amount.to_le_bytes());
    want.extend_from_slice(&min_sol_out.to_le_bytes());

    assert_eq!(data, want);
    assert_eq!(data.len(), 24);
    assert_eq!(&data[0..8], &SELL_DISCRIMINATOR);
    assert_eq!(
        u64::from_le_bytes(data[8..16].try_into().unwrap()),
        token_amount
    );
    assert_eq!(
        u64::from_le_bytes(data[16..24].try_into().unwrap()),
        min_sol_out
    );
}

#[test]
fn discriminators_differ() {
    assert_ne!(BUY_DISCRIMINATOR, SELL_DISCRIMINATOR);
}

#[test]
fn serialization_is_deterministic() {
    let p = BuyParams {
        min_tokens_out: 7,
        max_sol_cost: 11,
    };
    assert_eq!(build_buy_ix(p), build_buy_ix(p));
}

#[test]
fn handles_extreme_values() {
    let data = build_buy_ix(BuyParams {
        min_tokens_out: u64::MAX,
        max_sol_cost: 0,
    });
    assert_eq!(&data[8..16], &u64::MAX.to_le_bytes());
    assert_eq!(&data[16..24], &0u64.to_le_bytes());
}
