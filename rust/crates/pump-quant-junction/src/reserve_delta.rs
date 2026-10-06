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
use pump_quant_app::Engine;
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
    recv_unix_ms: Option<i64>,
) -> Option<ProvenancedEvent> {
    let prev = previous?;

    // Reserves are u64. Compute the DIFFERENCE in i128 (both widened values are exact,
    // and a u64 - u64 difference always fits i128) so that two VALID large reserves with
    // a small delta are not rejected merely because an individual reserve exceeds
    // i64::MAX. Only a delta that genuinely cannot be represented as i64 is rejected.
    let delta_vsol: i64 = i128::from(current.virtual_sol)
        .checked_sub(i128::from(prev.virtual_sol))
        .and_then(|d| i64::try_from(d).ok())?;
    let delta_vtoken: i64 = i128::from(current.virtual_token)
        .checked_sub(i128::from(prev.virtual_token))
        .and_then(|d| i64::try_from(d).ok())?;

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
        i64::try_from(delta_vtoken.unsigned_abs()).ok()?
    } else {
        // Sell: token reserve increased. signed_base = -|delta_vtoken|.
        // delta_vtoken should be positive — the absolute value is the volume.
        if delta_vtoken < 0 {
            // Inconsistent: vsol down AND vtoken down — not a valid trade.
            return None;
        }
        i64::try_from(delta_vtoken.unsigned_abs())
            .ok()?
            .checked_neg()?
    };

    // Price: post-trade execution price = vsol_after * PRICE_SCALE / vtoken_after.
    const PRICE_SCALE: i128 = 1_000_000_000;
    // u64->i128 is a widening conversion (infallible); u64::MAX*1e9 < i128::MAX; divisor guarded above.
    #[allow(clippy::arithmetic_side_effects)]
    // LINT-ALLOW(hot_arith): u64*1e9 < i128::MAX, divisor>0 guarded
    let price_fp: i128 = if current.virtual_token > 0 {
        (current.virtual_sol as i128) * PRICE_SCALE / (current.virtual_token as i128)
    } else {
        return None; // Degenerate: zero token reserves.
    };

    // Quote volume: |delta_vsol| in lamports.
    let quote_lamports: u64 = delta_vsol.unsigned_abs();

    // Liquidity: post-trade virtual SOL reserves.
    let liquidity_lamports: u64 = current.virtual_sol;

    let event = AppEvent::MarketTrade {
        mint: Mint(*mint_bytes),
        price_fp,
        quote_lamports,
        liquidity_lamports,
        signed_base,
        buyer_entity: 0, // Unknown trader — conservative for unique-buyer counting.
        // Account data has no signer, so this path cannot name the trader: the junction's
        // `(mint, slot)` join supplies the address from the instruction print.
        trader_pubkey: None,
        age_slots: 0, // Unknown age — conservative for hold-horizon.
        // The account notification's wire receive time (see `LaserStreamUpdate::Account`).
        // This producer is the only one with a real `price_fp`, so its clock is what the
        // live ledger's windows are keyed on.
        recv_unix_ms,
        // The account notification's slot: known here, so carried (not dropped).
        slot: Some(slot),
        fee_lamports: None,
        cu_consumed: None,
        // Decoded from a pump.fun `BondingCurve` account, so the venue is the curve by construction.
        venue: Some(pump_quant_app::event::TradeVenue::PumpFun),
    };

    Some(ProvenancedEvent {
        event,
        source: ProvenanceSource::HeliusReserveDelta,
        slot,
        is_live,
    })
}

/// Whether both reserve deltas are representable as `i64` — the derivation's ONLY
/// out-of-range rejection. Lets a caller separate a genuinely out-of-range observation
/// from an ordinary no-trade (first sighting / zero delta / inconsistent curve), which
/// are all legitimate `None`s. Reserves are widened to i128, so only a delta that cannot
/// fit `i64` returns `false`.
#[must_use]
pub fn delta_representable(prev: Option<&ReserveSnapshot>, current: &PumpCurve) -> bool {
    let Some(p) = prev else {
        return true;
    };
    let fits = |a: u64, b: u64| {
        i128::from(a)
            .checked_sub(i128::from(b))
            .and_then(|d| i64::try_from(d).ok())
            .is_some()
    };
    fits(current.virtual_sol, p.virtual_sol) && fits(current.virtual_token, p.virtual_token)
}

/// Why a reserve observation yielded no `MarketTrade`.
///
/// [`derive_market_trade_from_delta`] returns `None` for two fundamentally different
/// reasons, and the difference is a *flow-completeness* question, not a diagnostic:
///
/// * [`DeltaMiss::NoPrint`] — no print existed. A first sighting (no previous snapshot),
///   or reserves that did not move (a `complete`-flag flip on migration, a spurious
///   notification). Nothing was lost; a quiet window is honest.
/// * [`DeltaMiss::UpstreamDropped`] — a print moved the curve but the derivation refused
///   it: a delta outside `i64`, a self-inconsistent curve (both reserves moved the same
///   way), or degenerate post-trade reserves (zero token side). That print is dropped
///   **before** the flow reducer ever sees it, so every flow window that contains its
///   instant is missing a print and must not be served as complete or as a quiet market.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaMiss {
    /// No print existed to drop.
    NoPrint,
    /// A print existed and was dropped upstream of the flow reducer.
    UpstreamDropped,
}

/// Classify a `None` from [`derive_market_trade_from_delta`], so a caller can tell an
/// ordinary no-trade observation from a dropped print.
///
/// This mirrors the derivation's rejections exactly and is only meaningful when the
/// derivation returned `None` (the consistency is pinned by a test below). A valid print
/// never reaches the fall-through: the final arm exists so the classifier fails *closed*
/// rather than misreading an unclassified rejection as an ordinary no-trade.
#[must_use]
pub fn classify_delta_miss(prev: Option<&ReserveSnapshot>, current: &PumpCurve) -> DeltaMiss {
    let Some(p) = prev else {
        return DeltaMiss::NoPrint; // first sighting: no print existed
    };
    // Same widened arithmetic as the derivation: only a delta that cannot fit `i64`
    // is out of range, never a merely large reserve.
    let delta_vsol = i128::from(current.virtual_sol).checked_sub(i128::from(p.virtual_sol));
    let delta_vtoken = i128::from(current.virtual_token).checked_sub(i128::from(p.virtual_token));
    let (Some(dv), Some(dt)) = (
        delta_vsol.and_then(|d| i64::try_from(d).ok()),
        delta_vtoken.and_then(|d| i64::try_from(d).ok()),
    ) else {
        return DeltaMiss::UpstreamDropped; // delta outside i64
    };
    if dv == 0 || dt == 0 {
        return DeltaMiss::NoPrint; // nothing moved
    }
    if (dv > 0) == (dt > 0) {
        return DeltaMiss::UpstreamDropped; // self-inconsistent curve
    }
    if current.virtual_token == 0 {
        return DeltaMiss::UpstreamDropped; // degenerate post-trade reserves
    }
    // Either a valid print (the classifier was misused) or a rejection not enumerated
    // above; both fail closed rather than being read as an ordinary no-trade.
    DeltaMiss::UpstreamDropped
}

/// The clocked LaserStream `accountSubscribe` path's drop-classification step, extracted so
/// the daemon and its integration test share ONE body.
///
/// Called exactly when [`derive_market_trade_from_delta`] returned `None` for a reserve
/// snapshot pair. It classifies the miss and, when the refusal was an
/// [`DeltaMiss::UpstreamDropped`] that carries a wire receive time, records the drop on the
/// engine via [`Engine::note_flow_upstream_drop`] so the decision join refuses any 300 s flow
/// window that contains it (by name, never as a quiet market). Returns the classification so
/// the caller keeps its own accounting (the daemon still counts `delta_no_trade` /
/// `delta_out_of_range`).
///
/// A `recv_unix_ms` of `None` is a drop with no known receive instant: the engine call is
/// deliberately skipped (no fabricated time, no silent fallback) — the classification is
/// still returned so the caller can count it.
#[must_use]
pub fn note_curve_snapshot_outcome(
    engine: &mut Engine,
    mint: [u8; 32],
    prev: Option<&ReserveSnapshot>,
    current: &PumpCurve,
    recv_unix_ms: Option<i64>,
) -> DeltaMiss {
    let miss = classify_delta_miss(prev, current);
    if miss == DeltaMiss::UpstreamDropped {
        if let Some(ms) = recv_unix_ms {
            engine.note_flow_upstream_drop(mint, ms);
        }
    }
    miss
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
    /// A u64 reserve above i64::MAX cannot be represented as a signed delta, so the
    /// derivation rejects it (checked conversion) instead of wrapping.
    #[test]
    fn delta_representable_separates_out_of_range_from_ordinary_no_trade() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Ordinary no-trade: a small, representable delta.
        assert!(delta_representable(
            Some(&prev),
            &make_curve(31_000_000_000, 1_000_000_000)
        ));
        // No previous snapshot is NOT out-of-range.
        assert!(delta_representable(
            None,
            &make_curve(31_000_000_000, 1_000_000_000)
        ));
        // Genuinely out-of-range: the SOL delta exceeds i64.
        assert!(!delta_representable(
            Some(&prev),
            &make_curve(u64::MAX, 1_000_000_000)
        ));
    }

    #[test]
    fn out_of_range_reserve_is_rejected() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        let cur = make_curve(u64::MAX, 1_000_000_000);
        assert!(
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &cur, 1000, true, None)
                .is_none()
        );
    }

    /// Two VALID reserves that each exceed i64::MAX with a small POSITIVE delta must
    /// still produce a trade — the delta is computed in i128, so the narrowness of an
    /// individual reserve is irrelevant.
    #[test]
    fn large_reserves_small_positive_delta_preserved() {
        let big = i64::MAX as u64 + 1000;
        let prev = ReserveSnapshot {
            virtual_sol: big,
            virtual_token: big + 5000,
            slot: 900,
        };
        // Buy: vsol +500, vtoken -10 (both reserves still > i64::MAX)
        let curve = make_curve(big + 500, big + 4990);
        let pe = derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None)
            .expect("valid large reserves with a small delta must not be rejected");
        if let AppEvent::MarketTrade {
            signed_base,
            quote_lamports,
            ..
        } = pe.event
        {
            assert_eq!(signed_base, 10, "|delta_vtoken| = 10");
            assert_eq!(quote_lamports, 500, "|delta_vsol| = 500");
        } else {
            panic!("expected MarketTrade");
        }
    }

    /// Same, with a small NEGATIVE delta (sell).
    #[test]
    fn large_reserves_small_negative_delta_preserved() {
        let big = i64::MAX as u64 + 1000;
        let prev = ReserveSnapshot {
            virtual_sol: big + 500,
            virtual_token: big + 4990,
            slot: 900,
        };
        // Sell: vsol -500, vtoken +10
        let curve = make_curve(big, big + 5000);
        let pe = derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None)
            .expect("valid large reserves with a small negative delta must not be rejected");
        if let AppEvent::MarketTrade { signed_base, .. } = pe.event {
            assert_eq!(signed_base, -10, "-|delta_vtoken| = -10");
        } else {
            panic!("expected MarketTrade");
        }
    }

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
            None,
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
        assert!(result.is_none());

        // Both down — also inconsistent
        curve = make_curve(29_000_000_000, 900_000_000);
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None)
                .unwrap();
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
        let result =
            derive_market_trade_from_delta(&[0xAB; 32], Some(prev), &curve, 1000, true, None);
        assert!(result.is_none());
    }

    /// The classifier must agree with the derivation: an ordinary no-trade observation is
    /// `NoPrint`, and a print that genuinely moved the curve but was refused is
    /// `UpstreamDropped`.
    #[test]
    fn classify_separates_dropped_prints_from_ordinary_no_trade() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        // Ordinary: first sighting, and reserves that did not move.
        assert_eq!(
            classify_delta_miss(None, &make_curve(31_000_000_000, 1_000_000_000)),
            DeltaMiss::NoPrint
        );
        assert_eq!(
            classify_delta_miss(Some(&prev), &make_curve(30_000_000_000, 1_000_000_000)),
            DeltaMiss::NoPrint
        );
        // Dropped: out-of-range delta.
        assert_eq!(
            classify_delta_miss(Some(&prev), &make_curve(u64::MAX, 1_000_000_000)),
            DeltaMiss::UpstreamDropped
        );
        // Dropped: self-inconsistent curve (both reserves moved up).
        assert_eq!(
            classify_delta_miss(Some(&prev), &make_curve(31_000_000_000, 1_100_000_000)),
            DeltaMiss::UpstreamDropped
        );
        // Dropped: degenerate post-trade reserves (zero token side).
        assert_eq!(
            classify_delta_miss(Some(&prev), &make_curve(31_000_000_000, 0)),
            DeltaMiss::UpstreamDropped
        );
    }

    /// Pin the classifier against the derivation on every rejection: each fixture is a
    /// `None` from the derivation, and the classifier's label matches what was lost.
    #[test]
    fn classifier_agrees_with_the_derivation_on_every_rejection() {
        let prev = ReserveSnapshot {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            slot: 900,
        };
        let cases: [(Option<ReserveSnapshot>, PumpCurve, DeltaMiss); 6] = [
            (
                None,
                make_curve(31_000_000_000, 999_000_000),
                DeltaMiss::NoPrint,
            ),
            (
                Some(prev),
                make_curve(30_000_000_000, 1_000_000_000),
                DeltaMiss::NoPrint,
            ),
            (
                Some(prev),
                make_curve(30_000_000_000, 1_001_000_000),
                DeltaMiss::NoPrint,
            ),
            (
                Some(prev),
                make_curve(u64::MAX, 1_000_000_000),
                DeltaMiss::UpstreamDropped,
            ),
            (
                Some(prev),
                make_curve(31_000_000_000, 1_100_000_000),
                DeltaMiss::UpstreamDropped,
            ),
            (
                Some(prev),
                make_curve(31_000_000_000, 0),
                DeltaMiss::UpstreamDropped,
            ),
        ];
        for (p, cur, want) in cases {
            let derived = derive_market_trade_from_delta(
                &[0xAB; 32],
                p,
                &cur,
                1000,
                true,
                Some(1_700_000_000_000),
            );
            assert!(derived.is_none(), "fixture must be a rejection: {want:?}");
            assert_eq!(
                classify_delta_miss(p.as_ref(), &cur),
                want,
                "classifier disagreed on {want:?}"
            );
        }
    }
}
