//! Tests for the §21.7 launch-sale-trajectory + creation-window families
//! (criterion 104). Expectations are computed independently over multiple
//! inputs incl. edge cases.

use pump_quant_signals::launch_trajectory::*;

#[test]
fn sale_trajectory_basic() {
    // Three buys from two entities: entity 1 buys 100 + 300 = 400 ; entity 2 buys 100.
    // total = 500. duration 4000-1000 = 3000 ms. tx_count 3. unique 2.
    // breadth = 2*10000/3 = 6666. top1 concentration = 400/500 = 8000 bps.
    // max per buyer = 400.
    // progress 2000 -> 9000 over 3000ms: (9000-2000)*1000/3000 = 7000000/3000 = 2333 bps/s.
    let txs = [
        SaleTx {
            ts_ms: 1000,
            buyer_entity: 1,
            base_amount: 100,
        },
        SaleTx {
            ts_ms: 2500,
            buyer_entity: 2,
            base_amount: 100,
        },
        SaleTx {
            ts_ms: 4000,
            buyer_entity: 1,
            base_amount: 300,
        },
    ];
    let t = analyze_sale_trajectory(&txs, 1, 2000, 9000);
    assert_eq!(t.duration_ms, 3000);
    assert_eq!(t.tx_count, 3);
    assert_eq!(t.unique_buyers, 2);
    assert_eq!(t.breadth_bps, 6666);
    assert_eq!(t.max_per_buyer_base, 400);
    assert_eq!(t.top_n_concentration_bps, 8000);
    assert_eq!(t.tier_velocity_bps_per_s, 2333);
}

#[test]
fn sale_trajectory_top_n_and_broad() {
    // 4 entities holding 500,300,150,50 = 1000 total. top2 = 800 -> 8000 bps.
    let txs = [
        SaleTx {
            ts_ms: 10,
            buyer_entity: 1,
            base_amount: 500,
        },
        SaleTx {
            ts_ms: 20,
            buyer_entity: 2,
            base_amount: 300,
        },
        SaleTx {
            ts_ms: 30,
            buyer_entity: 3,
            base_amount: 150,
        },
        SaleTx {
            ts_ms: 40,
            buyer_entity: 4,
            base_amount: 50,
        },
    ];
    let t = analyze_sale_trajectory(&txs, 2, 0, 10000);
    assert_eq!(t.unique_buyers, 4);
    assert_eq!(t.top_n_concentration_bps, 8000);
    // breadth: 4 unique / 4 txs = 10000 bps (fully broad).
    assert_eq!(t.breadth_bps, 10000);
    assert_eq!(t.max_per_buyer_base, 500);
}

#[test]
fn sale_trajectory_empty_and_zero_duration() {
    assert_eq!(
        analyze_sale_trajectory(&[], 3, 0, 100),
        SaleTrajectory::default()
    );
    // single tx: duration 0 -> tier velocity 0 (guard).
    let txs = [SaleTx {
        ts_ms: 5,
        buyer_entity: 9,
        base_amount: 100,
    }];
    let t = analyze_sale_trajectory(&txs, 1, 0, 10000);
    assert_eq!(t.duration_ms, 0);
    assert_eq!(t.tier_velocity_bps_per_s, 0);
    assert_eq!(t.top_n_concentration_bps, 10000); // one holder = all.
}

#[test]
fn creation_window_stats() {
    // Competitors: spends = pf+tip.
    // e1: 100+50=150 (bundle, sniper) ; e2: 200+0=200 ; e1 again: 10+10=20.
    // txs=3 ; unique tippers {1,2}=2 ; max=200 ; sum=370 mean=123 (370/3).
    // bundle txs = 1 -> 1*10000/3 = 3333 bps. sniper count = 1.
    let txs = [
        FirstSlotTx {
            tipper_entity: 1,
            priority_fee_lamports: 100,
            tip_lamports: 50,
            is_bundle: true,
            is_known_sniper: true,
        },
        FirstSlotTx {
            tipper_entity: 2,
            priority_fee_lamports: 200,
            tip_lamports: 0,
            is_bundle: false,
            is_known_sniper: false,
        },
        FirstSlotTx {
            tipper_entity: 1,
            priority_fee_lamports: 10,
            tip_lamports: 10,
            is_bundle: false,
            is_known_sniper: false,
        },
    ];
    let s = analyze_creation_window(&txs);
    assert_eq!(s.tx_count, 3);
    assert_eq!(s.unique_tippers, 2);
    assert_eq!(s.max_spend_lamports, 200);
    assert_eq!(s.mean_spend_lamports, 123);
    assert_eq!(s.bundle_participation_bps, 3333);
    assert_eq!(s.sniper_cohort_count, 1);
}

#[test]
fn creation_window_empty() {
    assert_eq!(analyze_creation_window(&[]), CreationWindowStats::default());
}
