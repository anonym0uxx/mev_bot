//! Derive `MarketTrade` events from on-chain reserve deltas.
//!
//! When Helius `accountSubscribe` notifies us that a bonding-curve PDA changed,
//! we decode the new reserves. If we tracked the PREVIOUS reserves, the delta
//! between them IS the trade: `delta_vsol > 0` → buy (SOL entered the curve),
//! `delta_vsol < 0` → sell (SOL left). This is real on-chain data observed
//! from the account state, NOT a synthesised feed.
//!
//! This is the fail-closed path for paper trading when PumpPortal's
//! `subscribeTokenTrade` is unavailable (requires a funded API key). The
//! derived `MarketTrade` feeds the numeric lane, which provides
//! `liquidity_lamports` — the gate's `NoNumericConfirmation` reject is
//! resolved without a paid feed.
//!
//! HOT-PATH LAW (§24/criterion 109): no async, no floats, no panics, no
//! per-event allocation. All money paths are integer lamports.

use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::decode::PumpCurve;

use crate::{ProvenanceSource, ProvenancedEvent};

/// Previous reserve snapshot for a single mint, used to compute deltas.
#[derive(Clone, Copy, Debug)]
pub struct ReserveSnapshot {
    /// Virtual SOL reserves (lamports) at the last observation.
    pub virtual_sol: u64,
    /// Virtual token reserves (base units) at the last observation.
    pub virtual_token: u64,
    /// Slot at which the snapshot was taken.
    pub slot: u64,
}

/// The derivation result: either a trade was observed or the reserves
/// are unchanged (e.g. only the `complete` flag flipped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaResult {
    /// A trade was observed — the delta is non-zero.
    Trade,
    /// No trade — the reserves are unchanged (or this is the first snapshot).
    NoTrade,
}

/// Derive a `MarketTrade` from the delta between a previous reserve snapshot
/// and a newly decoded `PumpCurve`.
///
/// Returns `None` when:
/// - The previous snapshot is `None` (first observation — no delta to compute).
/// - The virtual_sol delta is zero (no SOL moved; the change was elsewhere,
///   e.g. the `complete` flag flipping on migration).
/// - The virtual_token delta is zero (degenerate state).
///
/// On success, returns a `ProvenancedEvent` carrying `AppEvent::MarketTrade`
/// with provenance `HeliusReserveDelta` — distinguishing it from a direct
/// trade feed so downstream code can tell derived-from-state apart from
/// observed-from-trade.
///
/// # Derivation rules (pump.fun bonding curve constant-product)
///
/// - **Buy**: trader sends SOL, receives tokens. `virtual_sol` increases,
///   `virtual_token` decreases. `signed_base = +|delta_vtoken|`.
/// - **Sell**: trader sends tokens, receives SOL. `virtual_sol` decreases,
///   `virtual_token` increases. `signed_base = -|delta_vtoken|`.
///
/// The `quote_lamports` is `|delta_vsol|` — the SOL that moved. The
/// `liquidity_lamports` is the post-trade `virtual_sol` — the pool depth
/// the numeric lane reads. The `price_fp` is the post-trade execution price
/// `vsol_after * PRICE_SCALE / vtoken_after` — integer division, no float.
///
/// `buyer_entity` is `0` (unknown trader). The engine's unique-buyer counter
/// treats 0 as a single unknown entity — conservative, never inflating the
/// count. `age_slots` is 0 (the market's creation slot is not available from
/// account data; the engine treats unknown age conservatively).
#[must_use]
pub fn derive_market_trade_from_delta(
    mint_bytes: &[u8; 32],
    previous: Option<ReserveSnapshot>,
    current: &PumpCurve,
    slot: u64,
    is_live: bool,
) -> Option<ProvenancedEvent> {
    let prev = previous?;

    let delta_vsol: i64 = current.virtual_sol as i64 - prev.virtual_sol as i64;
    let delta_vtoken: i64 = current.virtual_token as i64 - prev.virtual_token as i64;

    // No SOL moved → no trade (could be a `complete` flag flip on migration,
    // or a spurious notification). Fail-closed: emit nothing.
    if delta_vsol == 0 || delta_vtoken == 0 {
        return None;
    }

    // Direction: buy if vsol increased (SOL entered curve), sell if decreased.
    // On a valid constant-product curve these are anti-correlated: a buy has
    // delta_vsol > 0 AND delta_vtoken < 0; a sell has delta_vsol < 0 AND
    // delta_vtoken > 0. If they are SAME-sign, the curve is inconsistent —
    // fail-closed, emit nothing.
    let signed_base: i64 = if delta_vsol > 0 {
        // Buy: token reserve decreased. signed_base = |delta_vtoken|.
        // delta_vtoken should be negative — the absolute value is the volume.
        if delta_vtoken > 0 {
            // Inconsistent: vsol up AND vtoken up — not a valid trade.
            return None;
        }
        delta_vtoken.unsigned_abs() as i64
    } else {
        // Sell: token reserve increased. signed_base = -|delta_vtoken|.
        // delta_vtoken should be positive — the absolute value is the volume.
        if delta_vtoken < 0 {
            // Inconsistent: vsol down AND vtoken down — not a valid trade.
            return None;
        }
        -(delta_vtoken.unsigned_abs() as i64)
    };

    // Price: post-trade execution price = vsol_after * PRICE_SCALE / vtoken_after.
    const PRICE_SCALE: i128 = 1_000_000_000;
    let price_fp: i128 = if current.virtual_token > 0 {
        (current.virtual_sol as i128) * PRICE_SCALE / (current.virtual_token as i128)
    } else {
        return None; // Degenerate: zero token reserves.
    };

    // Quote volume: |delta_vsol| in lamports.
    let quote_lamports: u64 = delta_vsol.unsigned_abs() as u64;

    // Liquidity: post-trade virtual SOL reserves.
    let liquidity_lamports: u64 = current.virtual_sol;

    let event = AppEvent::MarketTrade {
        mint: Mint(*mint_bytes),
        price_fp,
        quote_lamports,
        liquidity_lamports,
        signed_base,
        buyer_entity: 0, // Unknown trader — conservative for unique-buyer counting.
        age_slots: 0,    // Unknown age — conservative for hold-horizon.
    };

    Some(ProvenancedEvent {
        event,
        source: ProvenanceSource::HeliusReserveDelta,
        slot,
        is_live,
    })
}

/// Record the result of a derivation attempt, for stats tracking.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeltaStats {
    /// Number of trades derived from reserve deltas.
    pub trades_derived: u64,
    /// Number of snapshots that produced no trade (first sighting or no delta).
    pub no_trade: u64,
    /// Number of snapshots rejected for inconsistent reserves.
    pub inconsistent: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_curve(vsol: u64, vtoken: u64) -> PumpCurve {
        PumpCurve {
            virtual_sol: vsol,
            virtual_token: vtoken,
            real_sol: vsol.saturating_sub(30_000_000_000),
            real_token: 0,
            complete: false,
        }
    }

    #[test]
    fn first_snapshot_produces_no_trade() {
        let curve = make_curve(31_000_000_000, 999_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            None, // No previous snapshot
            &curve,
            1000,
            true,
        );
        assert!(result.is_none());
    }

    #[test]
    fn buy_delta_produces_positive_signed_base() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Buy: vsol up, vtoken down
        let curve = make_curve(31_000_000_000, 990_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_some());
        let pe = result.unwrap();
        if let AppEvent::MarketTrade {
            signed_base,
            quote_lamports,
            liquidity_lamports,
            price_fp,
            buyer_entity,
            ..
        } = pe.event
        {
            assert!(signed_base > 0, "buy → positive signed_base");
            assert_eq!(signed_base, 10_000_000); // |delta_vtoken| = 10M
            assert_eq!(quote_lamports, 1_000_000_000); // |delta_vsol| = 1 SOL
            assert_eq!(liquidity_lamports, 31_000_000_000); // post-trade vsol
            assert!(price_fp > 0);
            assert_eq!(buyer_entity, 0); // unknown trader
        } else {
            panic!("expected MarketTrade");
        }
        assert_eq!(pe.source, ProvenanceSource::HeliusReserveDelta);
        assert!(pe.is_live);
    }

    #[test]
    fn sell_delta_produces_negative_signed_base() {
        let prev = ReserveSnapshot {
            virtual_sol: 35_000_000_000,
            virtual_token: 900_000_000,
            slot: 900,
        };
        // Sell: vsol down, vtoken up
        let curve = make_curve(34_000_000_000, 910_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_some());
        let pe = result.unwrap();
        if let AppEvent::MarketTrade { signed_base, .. } = pe.event {
            assert!(signed_base < 0, "sell → negative signed_base");
            assert_eq!(signed_base, -10_000_000); // -|delta_vtoken|
        } else {
            panic!("expected MarketTrade");
        }
    }

    #[test]
    fn zero_delta_produces_no_trade() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Same reserves — only the `complete` flag changed (migration)
        let curve = make_curve(30_000_000_000, 1_000_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_none());
    }

    #[test]
    fn inconsistent_reserves_rejected() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Both up — not a valid constant-product trade
        let mut curve = make_curve(31_000_000_000, 1_100_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_none());

        // Both down — also inconsistent
        curve = make_curve(29_000_000_000, 900_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_none());
    }

    #[test]
    fn price_fp_uses_post_trade_reserves() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Buy: vsol=31, vtoken=990M → price = 31e9 * 1e9 / 990M
        let curve = make_curve(31_000_000_000, 990_000_000);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        ).unwrap();
        if let AppEvent::MarketTrade { price_fp, .. } = result.event {
            // 31_000_000_000 * 1_000_000_000 / 990_000_000
            let expected = (31_000_000_000_000_000_000u128 / 990_000_000) as i128;
            assert_eq!(price_fp, expected);
        } else {
            panic!("expected MarketTrade");
        }
    }

    #[test]
    fn zero_vtoken_after_trade_rejected() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        let curve = make_curve(31_000_000_000, 0);
        let result = derive_market_trade_from_delta(
            &[0xAB; 32],
            Some(prev),
            &curve,
            1000,
            true,
        );
        assert!(result.is_none());
    }
}
