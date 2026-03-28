//! Gate stack for MEV backrun trigger filtering.
//!
//! 12+ sequential gates, each a single branch — zero allocation, zero heap.
//! Evaluated on every incoming trade; first rejection short-circuits.

use crate::feeds::{FeedSource, TradeEvent};

// ── Configuration ───────────────────────────────────────────────────

/// All thresholds expressed in lamports / integer counts — no float in the hot path.
#[derive(Clone, Debug)]
pub struct GateConfig {
    pub trigger_min_buy_lamports: u64,
    pub trigger_max_buy_lamports: u64,
    pub min_vsol_lamports: u64,
    pub max_vsol_lamports: u64,
    pub max_token_age_ms: u64,
    pub min_unique_buyers: u16,
    pub pre_trigger_min_buys_1s: u16,
    pub pre_trigger_min_buys_2s: u16,
    pub pre_trigger_min_buys_5s: u16,
    pub pre_trigger_max_gap_ms: u64,
    pub pre_trigger_min_vsol_accel: u64,
    pub pre_trigger_min_sell_count_5s: u16,
    pub pre_trigger_max_vsol_delta_3s: u64,
    pub creator_sell_ttl_ms: u64,
    pub pre_trigger_min_volume_5s_lamports: u64,
    pub max_trigger_isolation: f64,
    pub trigger_min_score: f64,
    pub blocked_sources: Vec<FeedSource>,
    /// Threshold for LargeTriggerLowBuyers gate (default 1.5 SOL).
    pub large_trigger_lamports: u64,
    /// Minimum unique buyers when trigger exceeds large_trigger_lamports.
    pub large_trigger_min_unique_buyers: u16,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            trigger_min_buy_lamports: 100_000_000,          // 0.1 SOL
            trigger_max_buy_lamports: 10_000_000_000,       // 10 SOL
            min_vsol_lamports: 3_000_000_000,               // 3 SOL
            max_vsol_lamports: 85_000_000_000,              // 85 SOL
            max_token_age_ms: 120_000,                      // 2 minutes
            min_unique_buyers: 3,
            pre_trigger_min_buys_1s: 1,
            pre_trigger_min_buys_2s: 2,
            pre_trigger_min_buys_5s: 3,
            pre_trigger_max_gap_ms: 3_000,                  // 3s
            pre_trigger_min_vsol_accel: 100_000_000,        // 0.1 SOL
            pre_trigger_min_sell_count_5s: 0,
            pre_trigger_max_vsol_delta_3s: 30_000_000_000,  // 30 SOL
            creator_sell_ttl_ms: 60_000,                    // 1 minute
            pre_trigger_min_volume_5s_lamports: 500_000_000, // 0.5 SOL
            max_trigger_isolation: 0.5,
            trigger_min_score: 0.35,
            blocked_sources: Vec::new(),
            large_trigger_lamports: 1_500_000_000,          // 1.5 SOL
            large_trigger_min_unique_buyers: 5,
        }
    }
}

// ── Rejection reasons ───────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum GateRejectReason {
    NotBuy,
    TriggerTooSmall,
    TriggerTooLarge,
    VSolOutOfRange,
    TokenTooOld,
    NotEnoughUniqueBuyers,
    /// trigger > 1.5 SOL but < 5 unique buyers
    LargeTriggerLowBuyers,
    /// time since last buy > max_gap_ms
    StaleGap,
    InsufficientCrowd2s,
    InsufficientCrowd5s,
    InsufficientVSolAccel,
    /// buy_count_1s < min
    StaleMomentum1s,
    InsufficientSellCount,
    VSolDeltaTooHigh,
    CreatorSellRecent,
    /// net flow ratio < 0.2 (count-based proxy)
    SellPressure,
    /// isolation ratio > max
    TriggerTooIsolated,
    ScoreTooLow(f64),
    SourceBlocked,
}

impl std::fmt::Display for GateRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScoreTooLow(s) => write!(f, "ScoreTooLow({:.4})", s),
            other => write!(f, "{:?}", other),
        }
    }
}

// ── Gate stack ──────────────────────────────────────────────────────

pub struct GateStack {
    config: GateConfig,
    /// Pre-computed: max_trigger_isolation scaled to millionths for integer comparison.
    /// trigger * 1_000_000 / (vol5s + trigger) <= isolation_threshold_fp
    isolation_threshold_fp: u64,
}

impl GateStack {
    pub fn new(config: GateConfig) -> Self {
        let isolation_threshold_fp = (config.max_trigger_isolation * 1_000_000.0) as u64;
        Self {
            config,
            isolation_threshold_fp,
        }
    }

    /// Access the underlying config.
    pub fn config(&self) -> &GateConfig {
        &self.config
    }

    /// Evaluate all gates in sequence. Returns `Ok(())` if all pass,
    /// `Err(reason)` on first failure.
    ///
    /// All pre-computed signals come from `MintHistory` cached fields —
    /// no recomputation here.
    #[inline]
    pub fn evaluate(
        &self,
        event: &TradeEvent,
        history_age_ms: u64,
        unique_buyers_30s: u16,
        buy_count_1s: u16,
        buy_count_2s: u16,
        buy_count_5s: u16,
        sell_count_5s: u16,
        volume_sol_5s: u64,
        vsol_delta_3s: u64,
        time_since_last_buy_ms: u64,
        creator_sell_at_ms: u64,
        now_ms: u64,
        score: f64,
    ) -> Result<(), GateRejectReason> {
        let c = &self.config;

        // ── Gate 0: Source blocked ──────────────────────────────────
        // Cheapest check — single byte compare + small vec scan.
        if c.blocked_sources.contains(&event.source) {
            return Err(GateRejectReason::SourceBlocked);
        }

        // ── Gate 1: Must be a buy ──────────────────────────────────
        if !event.is_buy {
            return Err(GateRejectReason::NotBuy);
        }

        // ── Gate 2: Trigger buy size range ─────────────────────────
        if event.sol_amount < c.trigger_min_buy_lamports {
            return Err(GateRejectReason::TriggerTooSmall);
        }
        if event.sol_amount > c.trigger_max_buy_lamports {
            return Err(GateRejectReason::TriggerTooLarge);
        }

        // ── Gate 3: vSol reserves in range ─────────────────────────
        if event.vsol_reserves < c.min_vsol_lamports || event.vsol_reserves > c.max_vsol_lamports {
            return Err(GateRejectReason::VSolOutOfRange);
        }

        // ── Gate 4: Token age ──────────────────────────────────────
        if history_age_ms > c.max_token_age_ms {
            return Err(GateRejectReason::TokenTooOld);
        }

        // ── Gate 5: Minimum unique buyers (30s) ────────────────────
        if unique_buyers_30s < c.min_unique_buyers {
            return Err(GateRejectReason::NotEnoughUniqueBuyers);
        }

        // ── Gate 6: Large trigger needs more buyers ────────────────
        if event.sol_amount > c.large_trigger_lamports
            && unique_buyers_30s < c.large_trigger_min_unique_buyers
        {
            return Err(GateRejectReason::LargeTriggerLowBuyers);
        }

        // ── Gate 7: Stale gap — time since last buy ────────────────
        if time_since_last_buy_ms > c.pre_trigger_max_gap_ms {
            return Err(GateRejectReason::StaleGap);
        }

        // ── Gate 8: Crowd depth (buy counts) ───────────────────────
        if buy_count_2s < c.pre_trigger_min_buys_2s {
            return Err(GateRejectReason::InsufficientCrowd2s);
        }
        if buy_count_5s < c.pre_trigger_min_buys_5s {
            return Err(GateRejectReason::InsufficientCrowd5s);
        }

        // ── Gate 9: Momentum — 1s buys ─────────────────────────────
        if buy_count_1s < c.pre_trigger_min_buys_1s {
            return Err(GateRejectReason::StaleMomentum1s);
        }

        // ── Gate 10: vSol acceleration ─────────────────────────────
        if vsol_delta_3s < c.pre_trigger_min_vsol_accel {
            return Err(GateRejectReason::InsufficientVSolAccel);
        }

        // ── Gate 11: vSol delta cap ────────────────────────────────
        if vsol_delta_3s > c.pre_trigger_max_vsol_delta_3s {
            return Err(GateRejectReason::VSolDeltaTooHigh);
        }

        // ── Gate 12: Sell count minimum ────────────────────────────
        if sell_count_5s < c.pre_trigger_min_sell_count_5s {
            return Err(GateRejectReason::InsufficientSellCount);
        }

        // ── Gate 13: Creator sell recency ──────────────────────────
        if creator_sell_at_ms > 0
            && now_ms.saturating_sub(creator_sell_at_ms) < c.creator_sell_ttl_ms
        {
            return Err(GateRejectReason::CreatorSellRecent);
        }

        // ── Gate 14: Sell pressure — net flow ratio ≥ 0.2 ──────────
        // Using count-based proxy: (buy5 - sell5) / (buy5 + sell5) >= 0.2
        // Integer form: 5*(buy - sell) >= (buy + sell) → 4*buy >= 6*sell
        {
            let total_count = buy_count_5s as u64 + sell_count_5s as u64;
            if total_count > 0 {
                let buy_4x = (buy_count_5s as u64).saturating_mul(4);
                let sell_6x = (sell_count_5s as u64).saturating_mul(6);
                if buy_4x < sell_6x {
                    return Err(GateRejectReason::SellPressure);
                }
            }
        }

        // ── Gate 15: Volume floor ──────────────────────────────────
        if volume_sol_5s < c.pre_trigger_min_volume_5s_lamports {
            return Err(GateRejectReason::InsufficientCrowd5s);
        }

        // ── Gate 16: Trigger isolation ─────────────────────────────
        // trigger / (vol5s + trigger) <= max_isolation
        // Integer: trigger * 1_000_000 <= isolation_threshold_fp * (vol5s + trigger)
        // Use u128 to guard against overflow.
        {
            let denom = volume_sol_5s.saturating_add(event.sol_amount);
            if denom > 0 {
                let lhs = (event.sol_amount as u128) * 1_000_000;
                let rhs = (self.isolation_threshold_fp as u128) * (denom as u128);
                if lhs > rhs {
                    return Err(GateRejectReason::TriggerTooIsolated);
                }
            }
        }

        // ── Gate 17: Score (LAST — most expensive) ─────────────────
        if score < c.trigger_min_score {
            return Err(GateRejectReason::ScoreTooLow(score));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::FeedSource;

    fn make_event(sol_amount: u64, vsol_reserves: u64, is_buy: bool) -> TradeEvent {
        TradeEvent {
            mint: [0u8; 32],
            trader: [1u8; 32],
            sig: [0u8; 64],
            sig_prefix: [0u8; 8],
            sol_amount,
            token_amount: 1_000_000,
            vsol_reserves,
            vtoken_reserves: 100_000_000,
            market_cap_sol: 50_000_000_000,
            slot: 100,
            timestamp_ms: 1_000_000,
            is_buy,
            source: FeedSource::PumpPortal,
            bonding_curve: [0u8; 32],
            assoc_bonding_curve: [0u8; 32],
        }
    }

    /// Helper: all-passing params for a standard event.
    fn passing_params() -> (TradeEvent, u64, u16, u16, u16, u16, u16, u64, u64, u64, u64, u64, f64) {
        (
            make_event(500_000_000, 10_000_000_000, true),
            10_000,          // history_age_ms
            5,               // unique_buyers_30s
            2,               // buy_count_1s
            3,               // buy_count_2s
            5,               // buy_count_5s
            1,               // sell_count_5s
            5_000_000_000,   // volume_sol_5s (5 SOL)
            500_000_000,     // vsol_delta_3s
            500,             // time_since_last_buy_ms
            0,               // creator_sell_at_ms
            1_010_000,       // now_ms
            0.5,             // score
        )
    }

    #[test]
    fn rejects_sell() {
        let stack = GateStack::new(GateConfig::default());
        let event = make_event(500_000_000, 10_000_000_000, false);
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 1, 2_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::NotBuy));
    }

    #[test]
    fn rejects_trigger_too_small() {
        let stack = GateStack::new(GateConfig::default());
        let event = make_event(10_000, 10_000_000_000, true);
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 1, 2_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::TriggerTooSmall));
    }

    #[test]
    fn passes_all_gates() {
        let stack = GateStack::new(GateConfig::default());
        let (event, age, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, score) = passing_params();
        let result = stack.evaluate(&event, age, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, score);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn rejects_score_too_low() {
        let stack = GateStack::new(GateConfig::default());
        let (event, age, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, _) = passing_params();
        let result = stack.evaluate(&event, age, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, 0.1);
        assert_eq!(result, Err(GateRejectReason::ScoreTooLow(0.1)));
    }

    #[test]
    fn rejects_creator_sell_recent() {
        let config = GateConfig {
            creator_sell_ttl_ms: 60_000,
            ..GateConfig::default()
        };
        let stack = GateStack::new(config);
        let (event, age, ub, b1, b2, b5, s5, vol, vd, gap, _, _, score) = passing_params();
        let now_ms = 1_100_000u64;
        let creator_sell_at = 1_080_000u64; // 20s ago, within 60s TTL
        let result = stack.evaluate(
            &event, age, ub, b1, b2, b5, s5, vol, vd, gap, creator_sell_at, now_ms, score,
        );
        assert_eq!(result, Err(GateRejectReason::CreatorSellRecent));
    }

    #[test]
    fn rejects_sell_pressure() {
        let stack = GateStack::new(GateConfig::default());
        let event = make_event(500_000_000, 10_000_000_000, true);
        // buy_count_5s=5 (passes crowd gate), sell_count_5s=10 → 4*5=20 < 6*10=60 → SellPressure
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 10, 5_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::SellPressure));
    }

    #[test]
    fn rejects_trigger_isolation() {
        let config = GateConfig {
            max_trigger_isolation: 0.3,
            ..GateConfig::default()
        };
        let stack = GateStack::new(config);
        // trigger = 5 SOL, vol5s = 1 SOL → isolation = 5/6 ≈ 0.83 > 0.3
        let event = make_event(5_000_000_000, 10_000_000_000, true);
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 1, 1_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::TriggerTooIsolated));
    }

    #[test]
    fn rejects_blocked_source() {
        let config = GateConfig {
            blocked_sources: vec![FeedSource::Helius],
            ..GateConfig::default()
        };
        let stack = GateStack::new(config);
        let mut event = make_event(500_000_000, 10_000_000_000, true);
        event.source = FeedSource::Helius;
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 1, 5_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::SourceBlocked));
    }

    #[test]
    fn rejects_vsol_out_of_range_low() {
        let stack = GateStack::new(GateConfig::default());
        let event = make_event(500_000_000, 1_000_000_000, true); // vsol = 1 SOL < min 3 SOL
        let result = stack.evaluate(
            &event, 10_000, 5, 2, 3, 5, 1, 5_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::VSolOutOfRange));
    }

    #[test]
    fn rejects_token_too_old() {
        let stack = GateStack::new(GateConfig::default());
        let (event, _, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, score) = passing_params();
        // age = 200s > 120s max
        let result = stack.evaluate(&event, 200_000, ub, b1, b2, b5, s5, vol, vd, gap, cs, now, score);
        assert_eq!(result, Err(GateRejectReason::TokenTooOld));
    }

    #[test]
    fn rejects_stale_gap() {
        let stack = GateStack::new(GateConfig::default());
        let (event, age, ub, b1, b2, b5, s5, vol, vd, _, cs, now, score) = passing_params();
        // time_since_last_buy = 5000ms > 3000ms max
        let result = stack.evaluate(&event, age, ub, b1, b2, b5, s5, vol, vd, 5_000, cs, now, score);
        assert_eq!(result, Err(GateRejectReason::StaleGap));
    }

    #[test]
    fn rejects_large_trigger_low_buyers() {
        let stack = GateStack::new(GateConfig::default());
        // trigger = 2 SOL > 1.5 SOL threshold, but only 4 unique buyers < 5
        let event = make_event(2_000_000_000, 10_000_000_000, true);
        let result = stack.evaluate(
            &event, 10_000, 4, 2, 3, 5, 1, 5_000_000_000, 500_000_000, 500, 0, 1_010_000, 0.5,
        );
        assert_eq!(result, Err(GateRejectReason::LargeTriggerLowBuyers));
    }
}
