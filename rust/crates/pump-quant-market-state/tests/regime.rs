//! Tests for `MarketRegimeState` classification and reducer.
//! Expectations computed by hand against `RegimeThresholds::default()`.

use pump_quant_market_state::regime::{
    classify, MarketEvent, MarketRegimeReducer, RegimeLevel, RegimeObservation, RegimeThresholds,
    Skew,
};

// Default thresholds recap:
//  sol_shock_bps        (-1000, -300, 300, 1000)
//  launch_velocity      (5, 50, 200)
//  graduation_rate_bps  (100, 500, 1500)
//  imbalance_bps        (-3000, -1000, 1000, 3000)
//  rug_rate_bps         (500, 2000, 5000)
//  congestion_bps       (3000, 6000, 8500)
//  fee_regime           (10_000, 100_000, 1_000_000)
//  route_degradation_bps(500, 2000, 5000)
//  liquidity_index      (100, 1_000, 10_000)   [inverted stress]

#[test]
fn missing_observations_classify_as_unknown_none() {
    let obs = RegimeObservation::default(); // all zero / None
    let st = classify(&obs, &RegimeThresholds::default());
    assert_eq!(st.sol_price_shock, None); // no SOL feed
    assert_eq!(st.graduation_rate, None); // zero launches
    assert_eq!(st.buy_sell_imbalance, None); // zero flow
    assert_eq!(st.rug_collapse_rate, None); // live_markets None
    assert_eq!(st.network_congestion, None);
    assert_eq!(st.fee_regime, None);
    assert_eq!(st.route_degradation, None); // zero attempts
    assert_eq!(st.liquidity_regime, None);
    // launch velocity is always defined (0 launches -> Low).
    assert_eq!(st.launch_velocity, RegimeLevel::Low);
}

#[test]
fn sol_price_shock_skew_buckets() {
    let th = RegimeThresholds::default();
    let mk = |bps: i64| {
        classify(
            &RegimeObservation {
                sol_price_change_bps: Some(bps),
                ..RegimeObservation::default()
            },
            &th,
        )
        .sol_price_shock
        .unwrap()
    };
    assert_eq!(mk(-1500), Skew::StrongDown); // <= -1000
    assert_eq!(mk(-1000), Skew::StrongDown); // boundary inclusive
    assert_eq!(mk(-500), Skew::Down); // <= -300
    assert_eq!(mk(0), Skew::Neutral);
    assert_eq!(mk(400), Skew::Up); // >= 300
    assert_eq!(mk(1000), Skew::StrongUp); // >= 1000
    assert_eq!(mk(5000), Skew::StrongUp);
}

#[test]
fn launch_velocity_and_graduation_rate() {
    let th = RegimeThresholds::default();
    // 100 launches -> Elevated (>=50, <200). 20 graduations -> rate = 2000 bps.
    // 2000 bps >= 1500 -> High graduation rate.
    let obs = RegimeObservation {
        launches: 100,
        graduations: 20,
        ..RegimeObservation::default()
    };
    let st = classify(&obs, &th);
    assert_eq!(st.launch_velocity, RegimeLevel::Elevated);
    assert_eq!(st.graduation_rate, Some(RegimeLevel::High));

    // 4 launches -> Low (<5). 0 graduations -> 0 bps -> Low.
    let obs2 = RegimeObservation {
        launches: 4,
        graduations: 0,
        ..RegimeObservation::default()
    };
    let st2 = classify(&obs2, &th);
    assert_eq!(st2.launch_velocity, RegimeLevel::Low);
    assert_eq!(st2.graduation_rate, Some(RegimeLevel::Low));

    // 300 launches -> High (>=200). 6 graduations -> 200 bps -> Normal (>=100,<500).
    let obs3 = RegimeObservation {
        launches: 300,
        graduations: 6,
        ..RegimeObservation::default()
    };
    let st3 = classify(&obs3, &th);
    assert_eq!(st3.launch_velocity, RegimeLevel::High);
    assert_eq!(st3.graduation_rate, Some(RegimeLevel::Normal));
}

#[test]
fn buy_sell_imbalance_signed() {
    let th = RegimeThresholds::default();
    // buys 900, sells 100 -> net 800, total 1000 -> 8000 bps >= 3000 StrongUp.
    let obs = RegimeObservation {
        buys: 900,
        sells: 100,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obs, &th).buy_sell_imbalance, Some(Skew::StrongUp));

    // buys 100, sells 900 -> -8000 bps <= -3000 StrongDown.
    let obs2 = RegimeObservation {
        buys: 100,
        sells: 900,
        ..RegimeObservation::default()
    };
    assert_eq!(
        classify(&obs2, &th).buy_sell_imbalance,
        Some(Skew::StrongDown)
    );

    // buys 550, sells 450 -> net 100, total 1000 -> 1000 bps >= 1000 Up.
    let obs3 = RegimeObservation {
        buys: 550,
        sells: 450,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obs3, &th).buy_sell_imbalance, Some(Skew::Up));

    // balanced 500/500 -> 0 bps Neutral.
    let obs4 = RegimeObservation {
        buys: 500,
        sells: 500,
        ..RegimeObservation::default()
    };
    assert_eq!(classify(&obs4, &th).buy_sell_imbalance, Some(Skew::Neutral));
}

#[test]
fn rug_rate_and_route_degradation_and_liquidity() {
    let th = RegimeThresholds::default();
    // 30 rugs / 1000 live = 300 bps -> Low (<500).
    // route 50 failures / 1000 attempts = 500 bps -> Normal (>=500,<2000).
    // liquidity 500 -> inverted: <1000, >=100 -> Elevated stress.
    let obs = RegimeObservation {
        rugs: 30,
        live_markets: Some(1000),
        route_attempts: 1000,
        route_failures: 50,
        liquidity_index: Some(500),
        ..RegimeObservation::default()
    };
    let st = classify(&obs, &th);
    assert_eq!(st.rug_collapse_rate, Some(RegimeLevel::Low));
    assert_eq!(st.route_degradation, Some(RegimeLevel::Normal));
    assert_eq!(st.liquidity_regime, Some(RegimeLevel::Elevated));

    // High-stress liquidity: index 50 (<100) -> High stress.
    let obs2 = RegimeObservation {
        liquidity_index: Some(50),
        ..RegimeObservation::default()
    };
    assert_eq!(
        classify(&obs2, &th).liquidity_regime,
        Some(RegimeLevel::High)
    );
    // Deep liquidity: index 50_000 (>=10_000) -> Low stress.
    let obs3 = RegimeObservation {
        liquidity_index: Some(50_000),
        ..RegimeObservation::default()
    };
    assert_eq!(
        classify(&obs3, &th).liquidity_regime,
        Some(RegimeLevel::Low)
    );
}

#[test]
fn fee_and_congestion_buckets() {
    let th = RegimeThresholds::default();
    // fee 500_000 -> Elevated (>=100_000,<1_000_000); congestion 9000 -> High (>=8500)
    let obs = RegimeObservation {
        median_priority_fee: Some(500_000),
        slot_fullness_bps: Some(9000),
        ..RegimeObservation::default()
    };
    let st = classify(&obs, &th);
    assert_eq!(st.fee_regime, Some(RegimeLevel::Elevated));
    assert_eq!(st.network_congestion, Some(RegimeLevel::High));
}

#[test]
fn reducer_accumulates_and_classifies() {
    let mut r = MarketRegimeReducer::new();
    // 60 launches, 5 graduations.
    for _ in 0..60 {
        r.ingest(&MarketEvent::Launch);
    }
    for _ in 0..5 {
        r.ingest(&MarketEvent::Graduation);
    }
    // buys 80, sells 20.
    for _ in 0..80 {
        r.ingest(&MarketEvent::Buy);
    }
    for _ in 0..20 {
        r.ingest(&MarketEvent::Sell);
    }
    // 10 route attempts, 3 failures.
    for i in 0..10 {
        r.ingest(&MarketEvent::RouteAttempt { succeeded: i >= 3 });
    }
    r.set_live_markets(200);
    for _ in 0..4 {
        r.ingest(&MarketEvent::Rug);
    }

    let obs = r.observation();
    assert_eq!(obs.launches, 60);
    assert_eq!(obs.graduations, 5);
    assert_eq!(obs.buys, 80);
    assert_eq!(obs.sells, 20);
    assert_eq!(obs.route_attempts, 10);
    assert_eq!(obs.route_failures, 3);
    assert_eq!(obs.rugs, 4);

    let st = r.classify(&RegimeThresholds::default());
    // 60 launches -> Elevated. graduation rate 5/60 = 833 bps -> Elevated (>=500,<1500).
    assert_eq!(st.launch_velocity, RegimeLevel::Elevated);
    assert_eq!(st.graduation_rate, Some(RegimeLevel::Elevated));
    // imbalance (80-20)/100 = 6000 bps -> StrongUp.
    assert_eq!(st.buy_sell_imbalance, Some(Skew::StrongUp));
    // route degradation 3/10 = 3000 bps -> Elevated (>=2000,<5000).
    assert_eq!(st.route_degradation, Some(RegimeLevel::Elevated));
    // rug rate 4/200 = 200 bps -> Low (<500).
    assert_eq!(st.rug_collapse_rate, Some(RegimeLevel::Low));
}

#[test]
fn property_levels_monotonic_in_input() {
    // For launch velocity: increasing launches never decreases the level.
    let th = RegimeThresholds::default();
    let level_ord = |l: RegimeLevel| l as u8;
    let mut prev = 0u8;
    for launches in [0u64, 4, 5, 49, 50, 199, 200, 10_000] {
        let st = classify(
            &RegimeObservation {
                launches,
                ..RegimeObservation::default()
            },
            &th,
        );
        let cur = level_ord(st.launch_velocity);
        assert!(cur >= prev, "launch velocity not monotonic at {launches}");
        prev = cur;
    }
    // For liquidity (inverted): increasing depth never increases stress level.
    let mut prev_stress = u8::MAX;
    for depth in [0u64, 99, 100, 999, 1000, 9999, 10_000, 1_000_000] {
        let st = classify(
            &RegimeObservation {
                liquidity_index: Some(depth),
                ..RegimeObservation::default()
            },
            &th,
        );
        let cur = level_ord(st.liquidity_regime.unwrap());
        assert!(
            cur <= prev_stress,
            "liquidity stress not monotonic at {depth}"
        );
        prev_stress = cur;
    }
}
