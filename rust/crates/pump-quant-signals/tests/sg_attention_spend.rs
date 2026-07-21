//! Tests for §29.10 attention-spend computation + Tier-0 no-self-promotion
//! guard (criterion 110). Expectations computed independently.

use pump_quant_signals::attention_spend::*;

fn table() -> PriceTable {
    PriceTable {
        version: 7,
        packages: vec![
            PricePackage {
                package_id: 1,
                unit_price_lamports: 1_000_000,
            },
            PricePackage {
                package_id: 2,
                unit_price_lamports: 5_000_000,
            },
        ],
        valid_until_ts_ms: 10_000,
    }
}

#[test]
fn spend_sums_events_times_versioned_prices() {
    // 3 units of pkg1 (@1e6) + 2 units of pkg2 (@5e6) = 3e6 + 10e6 = 13e6.
    let events = [
        BoostEvent {
            package_id: 1,
            count: 3,
            observed_ts_ms: 5_000,
        },
        BoostEvent {
            package_id: 2,
            count: 2,
            observed_ts_ms: 6_000,
        },
    ];
    match compute_spend(&events, &table(), 9_000) {
        SpendEstimate::Amount {
            lamports,
            table_version,
        } => {
            assert_eq!(lamports, 13_000_000);
            assert_eq!(table_version, 7);
        }
        other => panic!("expected Amount, got {other:?}"),
    }
}

#[test]
fn spend_missing_on_stale() {
    let events = [BoostEvent {
        package_id: 1,
        count: 1,
        observed_ts_ms: 5_000,
    }];
    // as_of beyond valid_until -> MissingStale (never a fabricated number).
    assert_eq!(
        compute_spend(&events, &table(), 10_001),
        SpendEstimate::MissingStale
    );
}

#[test]
fn spend_missing_on_unknown_package() {
    let events = [BoostEvent {
        package_id: 99,
        count: 1,
        observed_ts_ms: 5_000,
    }];
    assert_eq!(
        compute_spend(&events, &table(), 5_000),
        SpendEstimate::MissingUnknownPackage
    );
}

#[test]
fn spend_zero_is_a_real_number_when_fresh() {
    // No events + fresh table = a verifiable zero spend, distinct from Missing.
    match compute_spend(&[], &table(), 1_000) {
        SpendEstimate::Amount { lamports, .. } => assert_eq!(lamports, 0),
        other => panic!("expected Amount(0), got {other:?}"),
    }
}

#[test]
fn no_self_promotion_guard_refuses_every_relationship() {
    // Tier-0 prohibition proven by construction: exhaustively enumerate every
    // relationship; NONE may yield an approval. PromotionAuthorization has no
    // approving variant, so this loop can only observe Refused.
    for rel in [
        SystemRelationship::Holds,
        SystemRelationship::Trades,
        SystemRelationship::Researches,
        SystemRelationship::Unrelated,
    ] {
        let req = PaidPromotionRequest {
            token: 42,
            relationship: rel,
            package_id: 1,
        };
        let auth = authorize_paid_promotion(req);
        match auth {
            PromotionAuthorization::Refused(r) => assert_eq!(r.relationship, rel),
        }
        assert!(!paid_promotion_permitted(req));
    }
}
