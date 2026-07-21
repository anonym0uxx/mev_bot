//! The reflection pass: let realized net-SOL reshape future discovery.
//!
//! The constitution's §71 closes a loop most scanners leave open — reflection must
//! *enhance discovery*, not just grade it. Here that is mechanical: each lane's
//! realized net-SOL (tracked in `watchlist::LanePerformance`) nudges that lane's
//! discovery weight. A lane that has been paying its way gains emphasis; a lane
//! bleeding SOL loses it. The single objective the loop optimises is net SOL.
//!
//! Two governance guards keep the loop honest (§56 parameter envelope):
//! - **Bounded step.** No single reflection may move a weight by more than
//!   `reflect_weight_step_bp`, so a lucky streak cannot swing the engine.
//! - **Floor and ceiling.** A weight can never be driven to zero (no lane is
//!   silently killed) nor allowed to dominate; it stays in `[floor, ceiling]`.
//!
//! The pass is a pure function of `(performance, weights, config)`, so replay
//! reproduces the adapted weights exactly.

use crate::config::Config;
use pump_quant_watchlist::candidate::Lane;
use pump_quant_watchlist::lane_performance::LanePerformance;
use pump_quant_watchlist::rank::LaneWeights;

/// How one lane's weight moved during a reflection pass — for the decision journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeightDelta {
    /// The lane whose weight changed.
    pub lane: Lane,
    /// Weight before, bps.
    pub before_bp: u32,
    /// Weight after, bps.
    pub after_bp: u32,
    /// Realized net-SOL that drove the change, lamports.
    pub net_lamports: i64,
}

/// Run one reflection pass, mutating `weights` in place and returning the per-lane
/// deltas. Direction is set by the sign of each lane's realized net-SOL; magnitude
/// is capped by `reflect_weight_step_bp`; the result is clamped to the envelope.
pub fn reflect(
    performance: &LanePerformance,
    weights: &mut LaneWeights,
    cfg: &Config,
) -> Vec<WeightDelta> {
    let step = cfg.reflect_weight_step_bp;
    let floor = cfg.reflect_weight_floor_bp;
    let ceiling = cfg.reflect_weight_ceiling_bp;

    let mut deltas = Vec::with_capacity(Lane::COUNT);
    for lane in Lane::ALL {
        let net = performance.net_sol(lane);
        let before = weights.get(lane);
        let after = match net.cmp(&0) {
            std::cmp::Ordering::Greater => before.saturating_add(step).min(ceiling),
            std::cmp::Ordering::Less => before.saturating_sub(step).max(floor),
            std::cmp::Ordering::Equal => before.clamp(floor, ceiling),
        };
        if after != before {
            weights.set(lane, after);
        }
        deltas.push(WeightDelta {
            lane,
            before_bp: before,
            after_bp: after,
            net_lamports: net,
        });
    }
    deltas
}
