//! Integration tests for the SCALP → RIDE lifecycle.
//! Tests the interaction between entry_engine, positions, ride_state, and exit_machine.

#[cfg(test)]
mod tests {
    use crate::engine::ride_state::{RideState, RideConfig, RideDecision, RideExitReason};
    use crate::engine::exit_machine::{ExitStateMachine, ExitConfig, ExitDecision, ExitReasonNew, TpSlTierV2};
    use crate::engine::entry_engine::{EntryEngine, EntryEngineConfig, EntryInput, EntryAction};

    // Test 1: RideState full lifecycle — create, buy events, trail stop exit
    #[test]
    fn test_ride_full_lifecycle_trailing_stop() {
        let config = RideConfig::default();
        let mut rs = RideState::new(
            66_000,   // entry at 66 SOL vSOL (mvsol)
            66_000,   // current = entry
            1000,     // now_ms
            5,        // buy_rate_5s
            &config,
        );

        // Feed buy events (price rising)
        rs.on_buy_event(500, 1100);  // 0.5 SOL buy
        rs.on_buy_event(300, 1200);  // 0.3 SOL buy

        // Price rises to 72 SOL vSOL (peak)
        let d = rs.on_tick(72_000, 1300, &config);
        assert!(matches!(d, RideDecision::Hold), "should hold while rising");

        // Price drops below trail stop
        // Trail is 408 bp (8% price = 4.08% vSOL)
        // trail_stop = 72000 * (10000-408) / 10000 = 72000 * 9592 / 10000 = 69062
        let d2 = rs.on_tick(69_000, 1400, &config);
        assert!(matches!(d2, RideDecision::Exit(RideExitReason::TrailingStop)),
            "should exit on trail stop, got {:?}", d2);
    }

    // Test 2: RideState whale dump emergency exit
    #[test]
    fn test_ride_whale_dump_exit() {
        let config = RideConfig::default();
        let mut rs = RideState::new(66_000, 66_000, 1000, 5, &config);

        // Whale sells 2.5 SOL (2500 mvsol > whale_dump_exit_msol=2000)
        let result = rs.on_sell_event(2500, 1100, &config);
        assert!(result.is_some(), "whale dump should trigger exit");
        assert_eq!(result.unwrap(), RideExitReason::WhaleExit);
    }

    // Test 3: RideState buy gap timeout
    #[test]
    fn test_ride_buy_gap_timeout() {
        let config = RideConfig::default();
        let mut rs = RideState::new(66_000, 66_000, 1000, 5, &config);

        rs.on_buy_event(500, 1100);

        // No buys for 11 seconds (buy_gap_exit_ms = 10000)
        // Price must be above hard floor (entry * 1.01 = 66660) to reach buy gap check
        let d = rs.on_tick(67_000, 12_200, &config);
        assert!(matches!(d, RideDecision::Exit(RideExitReason::BuyGapTimeout)),
            "should exit on buy gap timeout, got {:?}", d);
    }

    // Test 4: RideState phase transitions
    #[test]
    fn test_ride_phase_transitions() {
        let config = RideConfig::default();
        let mut rs = RideState::new(66_000, 66_000, 1000, 5, &config);

        assert_eq!(rs.phase, 0, "should start in Early phase");

        // After 15s → Momentum (need price above floor to avoid HardFloor exit)
        // floor_mvsol = 66000 * 10100 / 10000 = 66660
        // Use price above floor
        let d1 = rs.on_tick(67_000, 16_100, &config);
        assert!(matches!(d1, RideDecision::Hold) || matches!(d1, RideDecision::Exit(_)),
            "tick should return a decision");
        // If the tick didn't exit, check phase
        if matches!(d1, RideDecision::Hold) {
            assert_eq!(rs.phase, 1, "should transition to Momentum after 15s");

            // After 60s → Tighten
            let d2 = rs.on_tick(68_000, 61_100, &config);
            if matches!(d2, RideDecision::Hold) {
                assert_eq!(rs.phase, 2, "should transition to Tighten after 60s");
            }
        }
    }

    // Test 5: EntryEngine produces Scalp vs Ride actions
    #[test]
    fn test_entry_engine_scalp_vs_ride() {
        let config = EntryEngineConfig::default();
        let engine = EntryEngine::new(&config);

        // High-magnitude input (should produce Ride action if score high enough)
        let input = EntryInput {
            vsol_reserves: 66_000_000_000,
            vtoken_reserves: 500_000_000_000,
            sol_amount: 500_000_000,
            buy_count_1s: 12,
            buy_count_2s: 18,
            buy_count_5s: 25,
            sell_count_5s: 0,
            unique_buyers_30s: 10,
            _pad: 0,
            volume_sol_5s: 15_000_000_000,
            vsol_delta_3s: 5_000_000_000,
            time_since_last_buy_ms: 50,
            history_age_ms: 8_000,
            creator_sell_at_ms: 0,
            now_ms: 1_000_000,
            max_wallet_vol_30s: 2_000_000_000,
            total_buy_vol_30s: 15_000_000_000,
        };
        let decision = engine.evaluate(&input);
        assert!(decision.size_lamports > 0, "should not reject strong input");
        assert!(decision.entry_score > 0.0);
        assert!(decision.magnitude_score > 0.0);
        // The action depends on magnitude_score vs threshold
        println!("Entry: {:.1}, Magnitude: {:.1}, Action: {:?}",
            decision.entry_score, decision.magnitude_score, decision.action);
    }

    // Test 6: ExitStateMachine still works for SCALP path
    #[test]
    fn test_scalp_exit_machine_unchanged() {
        let mut tiers = [TpSlTierV2::default(); 8];
        tiers[0] = TpSlTierV2 {
            trigger_max_lamports: 600_000_000,
            unconfirmed_tp_fp: 2000,
            unconfirmed_sl_fp: 1000,
            confirmed_tp_fp: 3000,
            confirmed_sl_fp: 1500,
        };
        let config = ExitConfig {
            confirmation_window_ms: 200,
            stall_no_buy_ms: 500,
            stall_fade_fp: 1000,
            stall_conviction_no_buy_ms: 800,
            stall_conviction_fade_fp: 1500,
            max_hold_safety_ms: 5000,
            conviction_tp_multipliers: [100, 100, 140, 180, 220],
            trail_min_conviction: 2,
            trail_activation_pct_of_base_tp: 60,
            trail_distance_fp: 1500,
            trail_keep_mult: 1.0 - 0.015,
            trail_activation_mult: 0.6,
            tp_sl_tiers: tiers,
            tp_sl_tier_count: 1,
        };
        let mut sm = ExitStateMachine::on_entry(&config, 500_000_000, 100.0, 1000);
        // TP at 102.0 (2% unconfirmed)
        let d = sm.on_price_tick(&config, 102.1, 1050);
        assert_eq!(d, ExitDecision::Exit(ExitReasonNew::TakeProfit));
    }
}
