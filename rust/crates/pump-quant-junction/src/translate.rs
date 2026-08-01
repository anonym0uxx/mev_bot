//! Translation: CanonicalTx → AppEvent::MarketTrade, and decoded bonding-curve
//! account snapshots → AppEvent::OnchainConfirm.
//!
//! HOT-PATH LAW (§24/criterion 109): no async, no floats, no panics, no
//! per-event allocation. All money paths are integer lamports. The decode
//! functions are pure and allocation-free.

use pump_quant_app::event::AppEvent;
use pump_quant_ingest::canonical::{CanonicalTx, TradeDirection, TxKind};
use pump_quant_domain::ids::Mint;

use crate::{ProvenanceSource, ProvenancedEvent};

/// Convert a 32-byte mint into the engine's `Mint` type.
/// Both are `[u8; 32]` — this is a zero-cost semantic bridge.
fn mint_from_bytes(bytes: &[u8; 32]) -> Mint {
    // Mint is a transparent wrapper around [u8; 32] in the app crate.
    // We construct it directly — no allocation, no copy beyond the fixed array.
    Mint(*bytes)
}

/// Translate a CanonicalTx trade event into an AppEvent::MarketTrade.
///
/// This is the primary translation. The CanonicalTx carries signed deltas and
/// reserve information from the provider parser; the AppEvent::MarketTrade
/// carries price (fixed-point), quote volume, liquidity, signed base volume,
/// buyer entity, and age — all integers.
///
/// Returns `None` if the CanonicalTx cannot be translated (e.g. graduation
/// events that should produce AppEvent::Migration instead, or unknown
/// direction).
pub fn canonical_tx_to_market_trade(
    tx: &CanonicalTx,
    slot: u64,
    is_live: bool,
) -> Option<ProvenancedEvent> {
    // Graduation events are not MarketTrade — they become Migration.
    if tx.kind == TxKind::Graduation {
        return None;
    }

    // Unknown direction trades cannot be translated — the engine requires
    // a signed base to feed CVD.
    if tx.direction == TradeDirection::Unknown {
        return None;
    }

    let mint = mint_from_bytes(&tx.mint);
    if mint.0 == [0u8; 32] {
        // No mint — Helius logsSubscribe carries no account keys. Cannot
        // route to a market.
        return None;
    }

    // Derive price from reserves: price = vsol / vtoken (fixed-point).
    // Both are u128; the fixed-point scale is PRICE_SCALE (1e9).
    // This is integer division — no floats.
    const PRICE_SCALE: i128 = 1_000_000_000;

    let price_fp: i128 = if tx.vtoken_reserves > 0 {
        // price = vsol * PRICE_SCALE / vtoken
        // vsol is u128, but fits in i128 (SOL supply < 2^63 lamports).
        (tx.vsol_reserves as i128) * PRICE_SCALE / (tx.vtoken_reserves as i128)
    } else {
        0
    };

    // Quote volume = |sol_delta| in lamports.
    let quote_lamports: u64 = tx.sol_delta.unsigned_abs() as u64;

    // Liquidity = virtual SOL reserves after the trade (pool depth proxy).
    let liquidity_lamports: u64 = tx.vsol_reserves as u64;

    // Signed base volume: positive = buy, negative = sell.
    // token_delta is signed from the trader's perspective: buy → positive,
    // sell → negative. We flip to the engine's convention.
    let signed_base: i64 = match tx.direction {
        TradeDirection::Buy => tx.token_delta as i64,
        TradeDirection::Sell => -(tx.token_delta.unsigned_abs() as i64),
        _ => return None,
    };

    // Buyer entity: derive a stable per-entity id from the trader pubkey.
    // We use a simple fnv1a hash of the 32-byte trader key — no allocation,
    // no float, deterministic.
    let buyer_entity: u64 = stable_entity_id(&tx.trader);

    // Age in slots: if both slot and tx.slot are present, the difference is
    // the market age. If tx.slot is 0 (PumpPortal doesn't carry slot), age is
    // 0 — the engine treats this as "unknown age" which is safe (it only
    // affects hold-horizon, not entry authorization).
    let age_slots: u32 = if tx.slot > 0 && slot >= tx.slot {
        (slot - tx.slot) as u32
    } else {
        0
    };

    let event = AppEvent::MarketTrade {
        mint,
        price_fp,
        quote_lamports,
        liquidity_lamports,
        signed_base,
        buyer_entity,
        age_slots,
    };

    Some(ProvenancedEvent {
        event,
        source: match tx.source {
            pump_quant_ingest::canonical::SourceKind::PumpPortal => {
                ProvenanceSource::PumpPortalTrade
            }
            pump_quant_ingest::canonical::SourceKind::HeliusWsLogs => {
                // HeliusWsLogs produces CanonicalTx via logsSubscribe.
                // In the junction, this maps to the trade feed source.
                ProvenanceSource::PumpPortalTrade // fallback — logsSubscribe is deprecated
            }
        },
        slot: if tx.slot > 0 { tx.slot } else { slot },
        is_live,
    })
}

/// Translate a decoded bonding-curve account snapshot into AppEvent::OnchainConfirm.
///
/// CRITICAL (blocker 2, structural fix): `real_sol_lamports` enters through
/// `DecodedRealSol::from_curve`, whose sole constructor takes `&PumpCurve`
/// (which can only come from `decode_pump_curve`). A derived `u64`
/// (`vsol - 30 SOL`) cannot construct this type — the mistake is
/// unrepresentable, not merely tested for.
///
/// The identity `real_sol = vsol - 30 SOL` is a pump.fun invariant that will
/// usually hold. The integrity point is not that the numbers differ — it is
/// that their PROVENANCE differs: one is observed, one is computed from a
/// constant pump.fun has changed before and can change again.
pub fn decoded_snapshot_to_onchain_confirm(
    mint_bytes: &[u8; 32],
    curve: &pump_quant_protocol::decode::PumpCurve,
    slot: u64,
    is_live: bool,
) -> ProvenancedEvent {
    let mint = mint_from_bytes(mint_bytes);
    let real_sol = crate::DecodedRealSol::from_curve(curve);

    let event = AppEvent::OnchainConfirm {
        mint,
        virtual_sol_lamports: curve.virtual_sol,
        real_sol_lamports: real_sol.into_lamports(),
    };

    ProvenancedEvent {
        event,
        source: ProvenanceSource::HeliusAccountSubscribe,
        slot,
        is_live,
    }
}

/// Stable per-entity id from a 32-byte pubkey. Uses fnv1a-64 — the same hash
/// the ingest crate uses — so entity ids are consistent across crates.
/// Zero allocation, deterministic, no float.
fn stable_entity_id(pubkey: &[u8; 32]) -> u64 {
    // Inline fnv1a-64 — matches pump_quant_ingest::social_parse::fnv1a_64.
    // We inline rather than import to avoid a cross-crate dependency on a
    // function that lives in a social_parse module (semantic mismatch).
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash: u64 = FNV_OFFSET;
    for &byte in pubkey.iter() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_ingest::canonical::*;

    fn make_test_tx(direction: TradeDirection) -> CanonicalTx {
        CanonicalTx {
            slot: 1000,
            signature: [0u8; 64],
            mint: [0xAB; 32],
            trader: [0xCD; 32],
            sol_delta: if direction == TradeDirection::Buy {
                -500_000_000 // -0.5 SOL spent
            } else {
                500_000_000 // +0.5 SOL received
            },
            token_delta: if direction == TradeDirection::Buy {
                1_000_000 // tokens received
            } else {
                -1_000_000 // tokens sold
            },
            vsol_reserves: 30_000_000_000, // 30 SOL
            vtoken_reserves: 1_000_000_000,
            market_cap_lamports: 0,
            timestamp_ms: 0,
            direction,
            kind: TxKind::Trade,
            source: SourceKind::PumpPortal,
        }
    }

    #[test]
    fn test_buy_trade_translation() {
        let tx = make_test_tx(TradeDirection::Buy);
        let result = canonical_tx_to_market_trade(&tx, 1050, true).unwrap();

        if let AppEvent::MarketTrade {
            mint,
            price_fp,
            quote_lamports,
            liquidity_lamports,
            signed_base,
            buyer_entity,
            age_slots,
        } = result.event
        {
            assert_eq!(mint.0, [0xAB; 32]);
            assert!(price_fp > 0);
            assert_eq!(quote_lamports, 500_000_000);
            assert_eq!(liquidity_lamports, 30_000_000_000);
            assert!(signed_base > 0, "buy → positive signed_base");
            assert!(buyer_entity != 0);
            assert_eq!(age_slots, 50); // 1050 - 1000
        } else {
            panic!("expected MarketTrade");
        }

        assert!(result.is_live);
        assert_eq!(result.source, ProvenanceSource::PumpPortalTrade);
        assert_eq!(result.slot, 1000);
    }

    #[test]
    fn test_sell_trade_translation() {
        let tx = make_test_tx(TradeDirection::Sell);
        let result = canonical_tx_to_market_trade(&tx, 1050, true).unwrap();

        if let AppEvent::MarketTrade { signed_base, .. } = result.event {
            assert!(signed_base < 0, "sell → negative signed_base");
        } else {
            panic!("expected MarketTrade");
        }
    }

    #[test]
    fn test_graduation_not_translated() {
        let mut tx = make_test_tx(TradeDirection::Buy);
        tx.kind = TxKind::Graduation;
        assert!(canonical_tx_to_market_trade(&tx, 1000, true).is_none());
    }

    #[test]
    fn test_unknown_direction_not_translated() {
        let tx = make_test_tx(TradeDirection::Unknown);
        assert!(canonical_tx_to_market_trade(&tx, 1000, true).is_none());
    }

    #[test]
    fn test_onchain_confirm_from_decoded_snapshot() {
        let curve = pump_quant_protocol::decode::PumpCurve {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            real_sol: 5_000_000_000,
            real_token: 800_000_000,
            complete: false,
        };
        let result = decoded_snapshot_to_onchain_confirm(
            &[0xAB; 32],
            &curve,
            1000,
            true,
        );

        if let AppEvent::OnchainConfirm {
            mint,
            virtual_sol_lamports,
            real_sol_lamports,
        } = result.event
        {
            assert_eq!(mint.0, [0xAB; 32]);
            assert_eq!(virtual_sol_lamports, 30_000_000_000);
            assert_eq!(real_sol_lamports, 5_000_000_000);
        } else {
            panic!("expected OnchainConfirm");
        }

        assert_eq!(result.source, ProvenanceSource::HeliusAccountSubscribe);
        assert!(result.is_live);
    }

    #[test]
    fn test_zero_mint_not_translated() {
        let mut tx = make_test_tx(TradeDirection::Buy);
        tx.mint = [0u8; 32];
        assert!(canonical_tx_to_market_trade(&tx, 1000, true).is_none());
    }

    #[test]
    fn test_stable_entity_id_deterministic() {
        let key = [0xCD; 32];
        let id1 = stable_entity_id(&key);
        let id2 = stable_entity_id(&key);
        assert_eq!(id1, id2);
        assert_ne!(id1, 0);
    }
}
