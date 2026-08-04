//! PumpPortal live-source adapter.
//!
//! Receives raw `subscribeTokenTrade`/`subscribeNewToken`/`subscribeMigration`
//! WebSocket payloads, parses them into `CanonicalTx` / `RawTokenMetadata` via
//! the ingest crate parsers, translates to `AppEvent` variants via the
//! junction's translate module, and pushes the provenanced events into the
//! bounded junction queue.
//!
//! This module does NOT own a WebSocket connection — the caller (wire-up in
//! `pump-quant-app` or a binary) owns the socket and feeds raw payloads in.
//! This keeps the junction testable against fixtures (no network in tests).
//!
//! §24/criterion 109: no async, no floats, no panics, no per-event allocation
//! on the hot path. The `handle_payload` functions are the hot-path entry
//! points. They allocate only inside the parsers (which build `CanonicalTx`
//! or `RawTokenMetadata` — the minimal allocations the replay path makes).

use pump_quant_ingest::pumpportal_parse::{parse_pumpportal, parse_pumpportal_create, parse_pumpportal_migration};
use pump_quant_ingest::canonical::TxKind;

use crate::queue::BoundedJunctionQueue;
use crate::translate::{canonical_tx_to_market_trade, raw_token_metadata_to_event};
use crate::{ProvenanceSource, ProvenancedEvent};

use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

/// Process a raw PumpPortal trade-event payload (subscribeTokenTrade).
///
/// Returns `true` if an event was enqueued, `false` if the payload did not
/// produce a translatable event (parse failure, unknown kind, or queue full —
/// the queue's overflow counter records drops, not this return value).
///
/// The caller owns the WebSocket frame. This function is purely synchronous
/// and allocation-free outside the parser.
pub fn handle_trade_payload(
    payload: &[u8],
    slot: u64,
    queue: &BoundedJunctionQueue,
) -> bool {
    // Parse the raw WS payload into a CanonicalTx.
    let Some(tx) = parse_pumpportal(payload) else {
        return false;
    };

    // Graduation events → Migration, not MarketTrade.
    if tx.kind == TxKind::Graduation {
        let mint = Mint(tx.mint);
        let event = AppEvent::Migration { mint, slot };
        let provenanced = ProvenancedEvent {
            event,
            source: ProvenanceSource::PumpPortalTrade,
            slot,
            is_live: true,
        };
        return queue.push(provenanced, slot);
    }

    // Translate to AppEvent::MarketTrade.
    let Some(provenanced) = canonical_tx_to_market_trade(&tx, slot, true) else {
        return false;
    };

    queue.push(provenanced, slot)
}

/// Process a raw PumpPortal token-creation payload (subscribeNewToken).
///
/// Token creation events announce a new mint entering the bonding curve.
/// This NOW pushes a `TokenMetadata` event into the junction queue so the
/// engine's `observe_token_metadata` path fires — feeding the category
/// rotation state, creator-credibility tracking, and launch counting.
///
/// Returns `true` if the creation was parsed, classified, and enqueued.
/// Returns `false` on parse failure or zero-mint (nothing to attribute).
pub fn handle_create_payload(
    payload: &[u8],
    slot: u64,
    queue: &BoundedJunctionQueue,
) -> bool {
    let Some(raw) = parse_pumpportal_create(payload) else {
        return false;
    };

    // Translate RawTokenMetadata → AppEvent::TokenMetadata (with category
    // classification under the V1 taxonomy).
    let Some(provenanced) = raw_token_metadata_to_event(&raw, slot, true) else {
        return false;
    };

    queue.push(provenanced, slot)
}

/// Process a raw PumpPortal migration payload (subscribeMigration).
///
/// Migration events signal a mint graduating from the bonding curve to a
/// pool. This pushes a `Migration` event into the junction queue so the
/// engine flips the market's venue-mechanics phase.
///
/// Returns `true` if the migration was parsed and enqueued.
pub fn handle_migration_payload(
    payload: &[u8],
    slot: u64,
    queue: &BoundedJunctionQueue,
) -> bool {
    let Some(mint_bytes) = parse_pumpportal_migration(payload) else {
        return false;
    };

    if mint_bytes == [0u8; 32] {
        return false;
    }

    let mint = Mint(mint_bytes);
    let event = AppEvent::Migration { mint, slot };
    let provenanced = ProvenancedEvent {
        event,
        source: ProvenanceSource::PumpPortalTrade,
        slot,
        is_live: true,
    };

    queue.push(provenanced, slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProvenanceSource;

    const SIG_ZERO: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const KEY_ZERO: &str = "11111111111111111111111111111111";
    const KEY_REAL: &str = "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump";

    fn buy_payload(sol: &str, tokens: u64) -> Vec<u8> {
        format!(
            r#"{{"txType":"buy","signature":"{SIG_ZERO}","mint":"{KEY_REAL}","traderPublicKey":"{KEY_ZERO}","solAmount":{sol},"tokenAmount":{tokens},"vSolInBondingCurve":30.5,"vTokensInBondingCurve":1000000000000,"marketCapSol":45.25,"timestamp":1234567890}}"#
        ).into_bytes()
    }

    fn create_payload(name: &str, symbol: &str) -> Vec<u8> {
        format!(
            r#"{{"signature":"{SIG_ZERO}","mint":"{KEY_REAL}","txType":"create","name":"{name}","symbol":"{symbol}","traderPublicKey":"{KEY_REAL}"}}"#
        ).into_bytes()
    }

    fn migration_payload() -> Vec<u8> {
        format!(
            r#"{{"mint":"{KEY_REAL}","txType":"migrate"}}"#
        ).into_bytes()
    }

    #[test]
    fn test_trade_payload_enqueues_market_trade() {
        let queue = BoundedJunctionQueue::with_capacity(16);
        let payload = buy_payload("1.5", 1_000_000);
        let ok = handle_trade_payload(&payload, 12345, &queue);
        assert!(ok, "trade should parse and enqueue");
        let event = queue.pop().unwrap();
        assert_eq!(event.source, ProvenanceSource::PumpPortalTrade);
        assert!(event.is_live);
        assert!(matches!(event.event, AppEvent::MarketTrade { .. }));
    }

    #[test]
    fn test_create_payload_enqueues_token_metadata() {
        let queue = BoundedJunctionQueue::with_capacity(16);
        let payload = create_payload("Test Coin", "TEST");
        let ok = handle_create_payload(&payload, 100, &queue);
        assert!(ok, "create should parse, classify, and enqueue");
        let event = queue.pop().unwrap();
        assert!(matches!(event.event, AppEvent::TokenMetadata { .. }));
        assert_eq!(event.source, ProvenanceSource::PumpPortalTrade);
        assert!(event.is_live);
    }

    #[test]
    fn test_migration_payload_enqueues_migration() {
        let queue = BoundedJunctionQueue::with_capacity(16);
        let payload = migration_payload();
        let ok = handle_migration_payload(&payload, 200, &queue);
        assert!(ok, "migration should parse and enqueue");
        let event = queue.pop().unwrap();
        assert!(matches!(event.event, AppEvent::Migration { .. }));
    }

    #[test]
    fn test_garbage_payload_rejected() {
        let queue = BoundedJunctionQueue::with_capacity(16);
        let garbage = b"not json at all";
        assert!(!handle_trade_payload(garbage, 100, &queue));
        assert!(!handle_create_payload(garbage, 100, &queue));
        assert!(!handle_migration_payload(garbage, 100, &queue));
    }

    #[test]
    fn test_queue_overflow_on_flood() {
        let queue = BoundedJunctionQueue::with_capacity(2);
        let payload = buy_payload("1.5", 1_000_000);
        for _ in 0..10 {
            handle_trade_payload(&payload, 100, &queue);
        }
        assert!(queue.depth() <= 2);
    }
}
