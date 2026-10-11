//! Instruction LIMITS are not executed quantities. Real captured, successful PumpSwap buys (mainnet wire lines) go through
//! the production parse -> classify -> event path:
//!
//!   * `amm_buy_unlimited_bound`: `max_quote_amount_in == u64::MAX` (no limit) with a finite executed amount;
//!   * `amm_buy_finite_bound`: a finite limit that differs from the executed amount.
//!
//! Assertions: the bound never appears as a quantity in ANY emitted event; the executed quote appears exactly once, on the
//! verified-event-derived `AmmSwap`, and equals the signer's own balance change in the same transaction.
use pump_quant_app::event::AppEvent;
use pump_quant_junction::laserstream::{
    classify_pump_instructions, instructions_to_events_with_meta, parse_ndjson_line,
    LaserStreamUpdate, PumpInstruction,
};

fn load(name: &str) -> pump_quant_junction::laserstream::LaserStreamTx {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let line = std::fs::read_to_string(p).unwrap();
    match parse_ndjson_line(line.trim()).expect("fixture parses") {
        LaserStreamUpdate::Transaction(t) => t,
        _ => panic!("not a transaction"),
    }
}

/// The signer's SOL spent excluding the network fee, from the transaction's own balance arrays (independent of the
/// decoder under test). Only meaningful when the signer is the swap's economic payer (not a routed transaction).
fn independent_signer_spend(tx: &pump_quant_junction::laserstream::LaserStreamTx) -> u64 {
    let b = tx.balances.as_ref().expect("balances");
    b.pre_sol[0] - b.post_sol[0] - tx.fee_lamports.expect("fee")
}

fn run(name: &str, expect_limit: u64) {
    let tx = load(name);
    assert_eq!(tx.tx_ok, Some(true), "fixture is a verified-success tx");
    let ixs = classify_pump_instructions(&tx);
    let buy_limit = ixs.iter().find_map(|i| match i {
        PumpInstruction::PumpSwapBuy {
            max_quote_amount_in,
            ..
        } => Some(*max_quote_amount_in),
        _ => None,
    });
    assert_eq!(
        buy_limit,
        Some(expect_limit),
        "the limit is preserved under its own name"
    );
    let events = instructions_to_events_with_meta(
        &ixs,
        tx.slot,
        true,
        tx.recv_unix_ms,
        tx.fee_lamports,
        tx.cu_consumed,
    );
    let mut executed: Vec<u64> = Vec::new();
    for e in &events {
        match &e.event {
            AppEvent::MarketTrade { quote_lamports, .. } => {
                assert_ne!(*quote_lamports, expect_limit, "a bound is never a quote");
                assert_eq!(
                    *quote_lamports, 0,
                    "an instruction print claims no executed quote"
                );
            }
            AppEvent::AmmSwap { quote_lamports, .. } => executed.push(*quote_lamports),
            _ => {}
        }
    }
    assert_eq!(
        executed.len(),
        1,
        "executed quantity arrives exactly once, from the verified event"
    );
    assert_ne!(executed[0], expect_limit);
    assert!(executed[0] > 0 && executed[0] < u64::MAX / 2);
    // Whatever the limit, nothing but the executed amount is a quote anywhere.
    for e in &events {
        if let AppEvent::AmmSwap { quote_lamports, .. } = &e.event {
            assert_eq!(*quote_lamports, executed[0]);
        }
    }
    if name.contains("finite") {
        // The signer is the economic payer here: the event's quote equals the signer's own balance change.
        assert_eq!(executed[0], independent_signer_spend(&tx));
    }
}

#[test]
fn an_unlimited_bound_never_becomes_a_quantity() {
    run("amm_buy_unlimited_bound.ndjson", u64::MAX);
}

#[test]
fn a_finite_bound_that_differs_from_the_executed_amount_never_becomes_a_quantity() {
    run("amm_buy_finite_bound.ndjson", 68_424_999);
}
