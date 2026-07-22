//! Tests for the creator-state reducer. Expectations are computed by hand.

use pump_quant_market_state::creator::{CreatorEvent, CreatorStateReducer};

#[test]
fn uninitialized_snapshot_is_unknown_for_supply_fractions() {
    let r = CreatorStateReducer::new(64);
    let s = r.snapshot();
    assert!(!s.initialized);
    assert_eq!(s.position_fraction_of_supply_bps, None);
    assert_eq!(s.sold_fraction_of_peak_bps, None);
    assert_eq!(s.current_position, 0);
}

#[test]
fn init_then_partial_sell_math() {
    let mut r = CreatorStateReducer::new(64);
    // Creator starts with 200_000 tokens out of 1_000_000 supply (20% = 2000bps)
    r.ingest(&CreatorEvent::Init {
        initial_tokens: 200_000,
        total_supply: 1_000_000,
        slot: 100,
    });
    // Creator buys 50_000 more for 3 SOL.
    r.ingest(&CreatorEvent::Buy {
        tokens: 50_000,
        quote_lamports: 3_000_000_000,
        slot: 110,
    });
    // Peak position now = 200_000 + 50_000 = 250_000.
    // Creator sells 100_000 for 5 SOL at slot 130.
    r.ingest(&CreatorEvent::Sell {
        tokens: 100_000,
        quote_lamports: 5_000_000_000,
        slot: 130,
    });
    let s = r.snapshot();
    assert!(s.initialized);
    assert_eq!(s.peak_position, 250_000);
    // current = 200_000 + 50_000 - 100_000 = 150_000
    assert_eq!(s.current_position, 150_000);
    assert_eq!(s.tokens_bought, 50_000);
    assert_eq!(s.tokens_sold, 100_000);
    assert_eq!(s.buy_count, 1);
    assert_eq!(s.sell_count, 1);
    assert_eq!(s.first_sell_slot, Some(130));
    assert!(!s.oversold);
    // sold fraction of peak = 100_000 / 250_000 = 4000 bps.
    assert_eq!(s.sold_fraction_of_peak_bps, Some(4000));
    // position fraction of supply = 150_000 / 1_000_000 = 1500 bps.
    assert_eq!(s.position_fraction_of_supply_bps, Some(1500));
    // realized quote 5 SOL, spent 3 SOL.
    assert_eq!(s.quote_realized, 5_000_000_000);
    assert_eq!(s.quote_spent, 3_000_000_000);
}

#[test]
fn full_dump_to_zero_position() {
    let mut r = CreatorStateReducer::new(64);
    r.ingest(&CreatorEvent::Init {
        initial_tokens: 100_000,
        total_supply: 1_000_000,
        slot: 1,
    });
    r.ingest(&CreatorEvent::Sell {
        tokens: 100_000,
        quote_lamports: 2_000_000_000,
        slot: 2,
    });
    let s = r.snapshot();
    assert_eq!(s.current_position, 0);
    assert_eq!(s.position_fraction_of_supply_bps, Some(0));
    assert_eq!(s.sold_fraction_of_peak_bps, Some(10_000)); // sold 100% of peak
    assert!(!s.oversold);
}

#[test]
fn oversell_is_flagged_and_position_floors_at_zero() {
    let mut r = CreatorStateReducer::new(64);
    r.ingest(&CreatorEvent::Init {
        initial_tokens: 100,
        total_supply: 1_000,
        slot: 1,
    });
    // Sells more than attributed acquisition (attribution gap).
    r.ingest(&CreatorEvent::Sell {
        tokens: 150,
        quote_lamports: 10,
        slot: 2,
    });
    let s = r.snapshot();
    assert!(s.oversold);
    assert_eq!(s.current_position, 0); // clamped
}

#[test]
fn creator_linked_clusters_are_deduplicated() {
    let mut r = CreatorStateReducer::new(64);
    r.ingest(&CreatorEvent::Init {
        initial_tokens: 0,
        total_supply: 1_000,
        slot: 1,
    });
    // Same linked cluster buys three times -> counts once.
    for slot in 2..5 {
        r.ingest(&CreatorEvent::LinkedBuy {
            cluster: 77,
            tokens: 10,
            slot,
        });
    }
    // A different linked cluster.
    r.ingest(&CreatorEvent::LinkedBuy {
        cluster: 78,
        tokens: 10,
        slot: 6,
    });
    let s = r.snapshot();
    assert_eq!(s.creator_linked_clusters, 2);
}

#[test]
fn first_sell_slot_is_earliest_only() {
    let mut r = CreatorStateReducer::new(64);
    r.ingest(&CreatorEvent::Init {
        initial_tokens: 1_000,
        total_supply: 10_000,
        slot: 1,
    });
    r.ingest(&CreatorEvent::Sell {
        tokens: 100,
        quote_lamports: 1,
        slot: 50,
    });
    r.ingest(&CreatorEvent::Sell {
        tokens: 100,
        quote_lamports: 1,
        slot: 40, // earlier slot arrives later in stream (reorder) — still keep 50
    });
    let s = r.snapshot();
    // first_sell_slot records the FIRST sell ingested (slot 50), not the min.
    // This is by-contract: it is the first observed sell in stream order.
    assert_eq!(s.first_sell_slot, Some(50));
    assert_eq!(s.sell_count, 2);
}

#[test]
fn property_position_conservation_over_many_inputs() {
    // For any sequence of buys/sells, current_position == max(0, init+bought-sold)
    for seed in 0..50u64 {
        let mut r = CreatorStateReducer::new(16);
        let init = (seed % 5) * 1000;
        let supply = 100_000;
        r.ingest(&CreatorEvent::Init {
            initial_tokens: init,
            total_supply: supply,
            slot: 0,
        });
        let mut bought: u128 = 0;
        let mut sold: u128 = 0;
        let n = 1 + seed % 8;
        for k in 0..n {
            if (seed + k) % 2 == 0 {
                let t = 100 * (1 + (seed + k) % 4);
                bought += u128::from(t);
                r.ingest(&CreatorEvent::Buy {
                    tokens: t,
                    quote_lamports: t,
                    slot: k + 1,
                });
            } else {
                let t = 50 * (1 + (seed + k) % 4);
                sold += u128::from(t);
                r.ingest(&CreatorEvent::Sell {
                    tokens: t,
                    quote_lamports: t,
                    slot: k + 1,
                });
            }
        }
        let s = r.snapshot();
        let gross = u128::from(init) + bought;
        let expected = gross.saturating_sub(sold);
        assert_eq!(s.current_position, expected);
        assert_eq!(s.tokens_bought, bought);
        assert_eq!(s.tokens_sold, sold);
        // position fraction never exceeds 10_000 bps of supply.
        if let Some(bps) = s.position_fraction_of_supply_bps {
            assert!(bps <= 10_000, "seed {seed}: {bps} bps");
        }
    }
}
