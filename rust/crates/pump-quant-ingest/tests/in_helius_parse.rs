#![allow(unused_imports)]
use pump_quant_ingest::canonical::*;
use pump_quant_ingest::helius_parse::*;

const SIG_ZERO: &str = "1111111111111111111111111111111111111111111111111111111111111111"; // 64 → [0;64]

// Build a Helius logsNotification with a given slot, err, and log lines.
fn logs_notification(slot: u64, err: &str, logs: &[&str]) -> String {
    let logs_json: Vec<String> = logs.iter().map(|l| format!("\"{l}\"")).collect();
    format!(
        r#"{{"jsonrpc":"2.0","method":"logsNotification","params":{{
            "result":{{"context":{{"slot":{slot}}},
                "value":{{"signature":"{SIG_ZERO}","err":{err},"logs":[{}]}}}},
            "subscription":1}}}}"#,
        logs_json.join(",")
    )
}

#[test]
fn buy_trade_parses_direction_and_slot() {
    let text = logs_notification(
        777,
        "null",
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program log: Instruction: Buy",
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
        ],
    );
    let tx = parse_helius(text.as_bytes()).unwrap();
    assert_eq!(tx.kind, TxKind::Trade);
    assert_eq!(tx.direction, TradeDirection::Buy);
    assert_eq!(tx.slot, 777);
    assert_eq!(tx.signature, [0u8; 64]);
    // logsSubscribe carries no account keys → mint/amounts zero.
    assert_eq!(tx.mint, [0u8; 32]);
    assert_eq!(tx.sol_delta, 0);
    assert_eq!(tx.token_delta, 0);
    assert_eq!(tx.source, SourceKind::HeliusWsLogs);
}

#[test]
fn sell_trade_detected() {
    let text = logs_notification(
        10,
        "null",
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program log: Instruction: Sell",
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success",
        ],
    );
    let tx = parse_helius(text.as_bytes()).unwrap();
    assert_eq!(tx.kind, TxKind::Trade);
    assert_eq!(tx.direction, TradeDirection::Sell);
}

#[test]
fn graduation_markers_yield_graduation_kind() {
    // Raydium AMM invoke marker.
    let raydium = logs_notification(
        1,
        "null",
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [2]",
            "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 success",
        ],
    );
    let tx = parse_helius(raydium.as_bytes()).unwrap();
    assert_eq!(tx.kind, TxKind::Graduation);
    assert_eq!(tx.direction, TradeDirection::Unknown);

    // PumpSwap CreatePool marker.
    let create_pool = logs_notification(
        2,
        "null",
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program log: Instruction: CreatePool",
        ],
    );
    assert_eq!(
        parse_helius(create_pool.as_bytes()).unwrap().kind,
        TxKind::Graduation
    );

    // pump.fun Migrate marker.
    let migrate = logs_notification(
        3,
        "null",
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program log: Instruction: Migrate",
        ],
    );
    assert_eq!(
        parse_helius(migrate.as_bytes()).unwrap().kind,
        TxKind::Graduation
    );
}

#[test]
fn non_pump_and_failed_and_non_notification_return_none() {
    // No pump.fun invocation, no graduation marker → None.
    let unrelated = logs_notification(
        1,
        "null",
        &[
            "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [1]",
            "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
        ],
    );
    assert!(parse_helius(unrelated.as_bytes()).is_none());

    // Failed tx (err != null) → None even with a graduation marker present.
    let failed = logs_notification(
        1,
        r#"{"InstructionError":[0,"Custom"]}"#,
        &[
            "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
            "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [2]",
        ],
    );
    assert!(parse_helius(failed.as_bytes()).is_none());

    // Non-notification control message → None.
    let ctrl = r#"{"jsonrpc":"2.0","result":42,"id":1}"#;
    assert!(parse_helius(ctrl.as_bytes()).is_none());
}

#[test]
fn bytes_contains_semantics() {
    assert!(bytes_contains(b"hello world", b"world"));
    assert!(bytes_contains(b"hello world", b"hello"));
    assert!(!bytes_contains(b"hello world", b"xyz"));
    // Needle longer than haystack → false, no panic (legacy behavior).
    assert!(!bytes_contains(b"hi", b"hello"));
    assert!(!bytes_contains(b"anything", b""));
}
