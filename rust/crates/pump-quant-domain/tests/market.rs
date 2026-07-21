//! Tests for Venue / Lane / Side vocabulary: stable discriminants, round-trip
//! decode, classification predicates, ordering.

use pump_quant_domain::market::{Lane, Side, Venue};

#[test]
fn venue_stable_discriminants_and_roundtrip() {
    // Discriminants are load-bearing wire values; assert them explicitly.
    assert_eq!(Venue::PumpFun.as_u8(), 0);
    assert_eq!(Venue::PumpSwap.as_u8(), 1);
    assert_eq!(Venue::Raydium.as_u8(), 2);
    // Round-trip every variant.
    for v in Venue::ALL {
        assert_eq!(Venue::from_u8(v.as_u8()), Some(v));
    }
    // Unknown fails closed.
    assert_eq!(Venue::from_u8(3), None);
    assert_eq!(Venue::from_u8(255), None);
    // ALL is complete and ordered.
    assert_eq!(Venue::ALL.len(), 3);
}

#[test]
fn venue_mechanics_classification() {
    // Exactly one venue is a bonding curve; the others are AMM pools; partition
    // is total and disjoint.
    for v in Venue::ALL {
        assert_ne!(
            v.is_bonding_curve(),
            v.is_amm_pool(),
            "{v} must be exactly one of curve/pool"
        );
    }
    assert!(Venue::PumpFun.is_bonding_curve());
    assert!(!Venue::PumpFun.is_amm_pool());
    assert!(Venue::PumpSwap.is_amm_pool());
    assert!(Venue::Raydium.is_amm_pool());
}

#[test]
fn venue_display() {
    assert_eq!(format!("{}", Venue::PumpFun), "PumpFun");
    assert_eq!(format!("{}", Venue::PumpSwap), "PumpSwap");
    assert_eq!(format!("{}", Venue::Raydium), "Raydium");
}

#[test]
fn lane_stable_discriminants_and_scalp_flag() {
    assert_eq!(Lane::CreationSniper.as_u8(), 0);
    assert_eq!(Lane::EarlyConfirmation.as_u8(), 1);
    assert_eq!(Lane::GraduationTransition.as_u8(), 2);
    assert_eq!(Lane::ActiveMarketScalp.as_u8(), 3);
    for l in Lane::ALL {
        assert_eq!(Lane::from_u8(l.as_u8()), Some(l));
    }
    assert_eq!(Lane::from_u8(4), None);
    // Exactly one lane is the scalp lane.
    let scalps: Vec<Lane> = Lane::ALL.into_iter().filter(|l| l.is_scalp()).collect();
    assert_eq!(scalps, vec![Lane::ActiveMarketScalp]);
    assert_eq!(Lane::ALL.len(), 4);
}

#[test]
fn side_opposite_is_involution() {
    // opposite twice is identity for both sides.
    assert_eq!(Side::Buy.opposite(), Side::Sell);
    assert_eq!(Side::Sell.opposite(), Side::Buy);
    for s in [Side::Buy, Side::Sell] {
        assert_eq!(s.opposite().opposite(), s);
    }
    assert_eq!(format!("{}", Side::Buy), "Buy");
    assert_eq!(format!("{}", Side::Sell), "Sell");
}

#[test]
fn enums_order_by_discriminant() {
    assert!(Venue::PumpFun < Venue::PumpSwap);
    assert!(Venue::PumpSwap < Venue::Raydium);
    assert!(Lane::CreationSniper < Lane::ActiveMarketScalp);
    assert!(Side::Buy < Side::Sell);
}
