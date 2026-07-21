#![allow(unused_imports)]
use pump_quant_ingest::base58::*;
use pump_quant_ingest::canonical::*;
use pump_quant_ingest::pumpportal_parse::*;

// A 64-char base58 string of all '1' decodes to 64 zero bytes; a 32-char all
// '1' string decodes to 32 zero bytes. These are used as known-decode
// signature / pubkey so expectations are computed independently of the parser.
const SIG_ZERO: &str = "1111111111111111111111111111111111111111111111111111111111111111"; // 64
const KEY_ZERO: &str = "11111111111111111111111111111111"; // 32

fn buy_payload(sol: &str, tokens: u64) -> String {
    format!(
        r#"{{"txType":"buy","signature":"{SIG_ZERO}","mint":"{KEY_ZERO}",
            "traderPublicKey":"{KEY_ZERO}","solAmount":{sol},"tokenAmount":{tokens},
            "vSolInBondingCurve":30.5,"vTokensInBondingCurve":1000000000000,
            "marketCapSol":45.25,"timestamp":1234567890}}"#
    )
}

#[test]
fn decimal_sol_to_lamports_is_exact_integer() {
    // Each expected value computed by hand from 1 SOL = 1_000_000_000 lamports.
    assert_eq!(decimal_sol_to_lamports("1.5"), Some(1_500_000_000));
    assert_eq!(decimal_sol_to_lamports("0.000000001"), Some(1));
    assert_eq!(decimal_sol_to_lamports("2"), Some(2_000_000_000));
    assert_eq!(decimal_sol_to_lamports("123.456"), Some(123_456_000_000));
    assert_eq!(decimal_sol_to_lamports("0.123456789"), Some(123_456_789));
    // 10th fractional digit is below lamport granularity → truncated.
    assert_eq!(decimal_sol_to_lamports("1.9999999999"), Some(1_999_999_999));
    assert_eq!(decimal_sol_to_lamports("0.0000000001"), Some(0));
    // ".5" (empty int part) is allowed.
    assert_eq!(decimal_sol_to_lamports(".5"), Some(500_000_000));
    // Invalid inputs.
    assert_eq!(decimal_sol_to_lamports("abc"), None);
    assert_eq!(decimal_sol_to_lamports("-1.0"), None);
    assert_eq!(decimal_sol_to_lamports(""), None);
    // Overflow of u128 in the integer part.
    let huge = "9".repeat(60);
    assert_eq!(decimal_sol_to_lamports(&huge), None);
}

#[test]
fn buy_produces_signed_deltas_and_fields() {
    let tx = parse_pumpportal(buy_payload("1.5", 1_000_000).as_bytes()).unwrap();
    assert_eq!(tx.direction, TradeDirection::Buy);
    assert_eq!(tx.kind, TxKind::Trade);
    assert_eq!(tx.source, SourceKind::PumpPortal);
    // Buy: SOL spent (negative), tokens received (positive).
    assert_eq!(tx.sol_delta, -1_500_000_000);
    assert_eq!(tx.token_delta, 1_000_000);
    // Known-zero decodes.
    assert_eq!(tx.signature, [0u8; 64]);
    assert_eq!(tx.mint, [0u8; 32]);
    assert_eq!(tx.trader, [0u8; 32]);
    // Reserves / market cap converted from decimal SOL to lamports.
    assert_eq!(tx.vsol_reserves, 30_500_000_000);
    assert_eq!(tx.vtoken_reserves, 1_000_000_000_000);
    assert_eq!(tx.market_cap_lamports, 45_250_000_000);
    assert_eq!(tx.timestamp_ms, 1_234_567_890);
    // PumpPortal supplies no slot.
    assert_eq!(tx.slot, 0);
}

#[test]
fn sell_flips_delta_signs() {
    let payload = format!(
        r#"{{"txType":"sell","signature":"{SIG_ZERO}","mint":"{KEY_ZERO}",
            "solAmount":0.25,"tokenAmount":42}}"#
    );
    let tx = parse_pumpportal(payload.as_bytes()).unwrap();
    assert_eq!(tx.direction, TradeDirection::Sell);
    // Sell: SOL received (positive), tokens spent (negative).
    assert_eq!(tx.sol_delta, 250_000_000);
    assert_eq!(tx.token_delta, -42);
    // Absent optional fields default to zero.
    assert_eq!(tx.vsol_reserves, 0);
    assert_eq!(tx.market_cap_lamports, 0);
    assert_eq!(tx.timestamp_ms, 0);
    assert_eq!(tx.trader, [0u8; 32]);
}

#[test]
fn several_amounts_independently_checked() {
    // Different magnitudes; expected lamports computed independently.
    let cases: &[(&str, u64, i128, i128)] = &[
        ("0.000000001", 1, -1, 1),
        ("2", 5, -2_000_000_000, 5),
        ("123.456", 7, -123_456_000_000, 7),
    ];
    for &(sol, tokens, want_sol, want_tok) in cases {
        let tx = parse_pumpportal(buy_payload(sol, tokens).as_bytes()).unwrap();
        assert_eq!(tx.sol_delta, want_sol, "sol for {sol}");
        assert_eq!(tx.token_delta, want_tok, "tok for {sol}");
    }
}

#[test]
fn non_trade_and_control_messages_return_none() {
    // create event → not a trade.
    let create = format!(
        r#"{{"txType":"create","signature":"{SIG_ZERO}","mint":"{KEY_ZERO}",
            "traderPublicKey":"{KEY_ZERO}","name":"x","symbol":"y"}}"#
    );
    assert!(parse_pumpportal(create.as_bytes()).is_none());

    // subscription ack: no signature field.
    let ack = r#"{"message":"Successfully subscribed"}"#;
    assert!(parse_pumpportal(ack.as_bytes()).is_none());

    // missing mint.
    let no_mint = format!(r#"{{"txType":"buy","signature":"{SIG_ZERO}"}}"#);
    assert!(parse_pumpportal(no_mint.as_bytes()).is_none());

    // malformed JSON.
    assert!(parse_pumpportal(b"{not json").is_none());
}

#[test]
fn base58_decode_matches_hand_computed_values() {
    // Single-char and short strings, computed from num = num*58 + digit_index.
    // Alphabet index: '1'=0, '2'=1, ... so decode("2") == [1].
    assert_eq!(decode("2"), Some(vec![1]));
    // "21": 1*58 + 0 = 58.
    assert_eq!(decode("21"), Some(vec![58]));
    // "22": 1*58 + 1 = 59.
    assert_eq!(decode("22"), Some(vec![59]));
    // Leading '1's are leading zero bytes.
    assert_eq!(decode(KEY_ZERO), Some(vec![0u8; 32]));
    assert_eq!(decode(SIG_ZERO), Some(vec![0u8; 64]));
    // Out-of-alphabet character.
    assert_eq!(decode("0"), None); // '0' not in base58 alphabet
}
