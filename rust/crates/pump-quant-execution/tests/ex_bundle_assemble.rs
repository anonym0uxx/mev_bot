#![allow(unused_imports)]
use pump_quant_execution::ex_bundle_assemble::*;

fn trade(len: usize) -> SignedTxRef {
    SignedTxRef {
        kind: TxKind::Trade,
        signed: true,
        bytes_len: len,
    }
}
fn tip(len: usize) -> SignedTxRef {
    SignedTxRef {
        kind: TxKind::Tip,
        signed: true,
        bytes_len: len,
    }
}

#[test]
fn valid_trade_plus_tip() {
    let txs = [trade(600), tip(200)];
    let b = assemble_bundle(&txs).expect("valid");
    assert_eq!(b.tx_count, 2);
    assert_eq!(b.tip_index, 1);
    assert_eq!(b.total_bytes, 800);
}

#[test]
fn multiple_trades_then_tip() {
    let txs = [trade(300), trade(400), trade(500), tip(100)];
    let b = assemble_bundle(&txs).expect("valid");
    assert_eq!(b.tx_count, 4);
    assert_eq!(b.tip_index, 3);
    assert_eq!(b.total_bytes, 1_300);
}

#[test]
fn empty_is_rejected() {
    assert!(assemble_bundle(&[]).is_none());
}

#[test]
fn over_five_is_rejected() {
    let txs = [
        trade(10),
        trade(10),
        trade(10),
        trade(10),
        trade(10),
        tip(10),
    ];
    assert_eq!(txs.len(), 6);
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn exactly_five_is_allowed() {
    let txs = [trade(10), trade(10), trade(10), trade(10), tip(10)];
    let b = assemble_bundle(&txs).expect("valid 5");
    assert_eq!(b.tx_count, 5);
    assert_eq!(b.tip_index, 4);
}

#[test]
fn unsigned_tx_is_rejected() {
    let txs = [
        SignedTxRef {
            kind: TxKind::Trade,
            signed: false,
            bytes_len: 100,
        },
        tip(100),
    ];
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn oversize_tx_is_rejected() {
    let txs = [trade(MAX_TX_BYTES + 1), tip(100)];
    assert!(assemble_bundle(&txs).is_none());
    // Exactly at the limit is fine.
    let ok = [trade(MAX_TX_BYTES), tip(100)];
    assert!(assemble_bundle(&ok).is_some());
}

#[test]
fn zero_length_tx_is_rejected() {
    let txs = [trade(0), tip(100)];
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn tip_not_last_is_rejected() {
    let txs = [tip(100), trade(100)];
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn missing_tip_is_rejected() {
    let txs = [trade(100), trade(100)];
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn two_tips_is_rejected() {
    let txs = [trade(100), tip(50), tip(50)];
    assert!(assemble_bundle(&txs).is_none());
}

#[test]
fn tip_only_no_trade_is_rejected() {
    let txs = [tip(100)];
    assert!(assemble_bundle(&txs).is_none());
}
