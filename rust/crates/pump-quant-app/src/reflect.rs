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
//!
//! # LAW B7 — the brain-informed, REDUCE-ONLY lane downweight
//!
//! The base pass above reads one number per lane: that lane's realized net-SOL
//! *aggregate*. An aggregate is a blunt instrument. A lane can look perfectly
//! healthy in aggregate — one early runner carrying twenty subsequent bleeders —
//! while every *conditioned setup class* it currently surfaces has decayed. §71
//! asks reflection to enhance discovery; discovering the same decayed setups more
//! slowly is the enhancement episodic memory can actually supply.
//!
//! So when [`Config::brain_reflect_enable`] is armed, [`reflect_with_brain`] takes
//! a [`LaneDecay`] flag set derived from conditioned recall
//! ([`crate::brain_analysis::lane_decay`]) and applies an ADDITIONAL downweight of
//! `brain_reflect_step_bp` to any flagged lane.
//!
//! Three properties are structural, not remembered:
//!
//! 1. **Reduce-only.** [`LaneDecay`] carries `bool` flags and the adjustment is a
//!    `saturating_sub`. There is no field, and no branch, that can raise a weight.
//!    This is the same discipline as LAW B3's [`crate::brain::BrainSizeVerdict`]:
//!    recall may shrink conviction, never inflate it, because "this class won
//!    before, so bet more" is precisely where a strategy-generated sample overfits
//!    (§46).
//! 2. **Inside the envelope.** The extra step is applied before the same
//!    `[floor, ceiling]` clamp, and `Config::validate` refuses a step wider than
//!    the envelope, so an armed pass can never jump the §56.2 floor.
//! 3. **Fail-closed.** A lane with no conditioned evidence, or evidence below
//!    `brain_decay_min_sample`, is simply not flagged — the flag set is built by
//!    [`crate::brain_analysis::lane_decay`], which refuses below the floor.
//!
//! [`reflect`] is retained as exactly the previous behaviour and is what an
//! unarmed engine gets, so the disarmed path is byte-identical to the pre-LAW-B7
//! engine.

use crate::config::Config;
use pump_quant_watchlist::candidate::Lane;
use pump_quant_watchlist::lane_performance::LanePerformance;
use pump_quant_watchlist::rank::LaneWeights;

/// §56.2/§102 LAW B7 default extra downweight for a lane whose conditioned setups
/// have decayed, bps.
///
/// Set to `250` — exactly one base `reflect_weight_step_bp`. The claim the brain
/// is making ("your aggregate says fine, your conditioned setups say bleeding") is
/// evidence of the same order as the aggregate itself, not stronger: it is
/// measured over the same realized trades, merely partitioned more honestly. A
/// step larger than the base one would assert that the partitioned view
/// *dominates* the aggregate, which nothing in our own data supports. One step
/// means an armed reflection halves a decayed lane's emphasis growth and doubles
/// its decay, and nothing more dramatic than that.
pub const BRAIN_REFLECT_STEP_BP_DEFAULT: u32 = 250;

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

/// LAW B7: which lanes' **conditioned setup classes** have decayed on our own
/// realized evidence.
///
/// A `bool` per [`Lane`], and deliberately nothing else. There is no "improved"
/// flag and no magnitude, because there is no reduce-only way to spend either: a
/// magnitude would invite a proportional *up*-weight the next time someone edits
/// this file, and the type is the guard against that edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneDecay {
    decayed: [bool; Lane::COUNT],
}

impl LaneDecay {
    /// The all-clear: no lane flagged. What an engine with no brain evidence —
    /// or with evidence below the §46 floor — must produce.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            decayed: [false; Lane::COUNT],
        }
    }

    /// Flag `lane` as decayed. Idempotent, and there is no `clear` — a flag set is
    /// built once per reflection from a fresh read of the index.
    pub fn set(&mut self, lane: Lane) {
        self.decayed[lane.index()] = true;
    }

    /// Whether `lane` is flagged.
    #[must_use]
    pub fn is_decayed(&self, lane: Lane) -> bool {
        self.decayed[lane.index()]
    }

    /// How many lanes are flagged (report plane).
    #[must_use]
    pub fn count(&self) -> u32 {
        self.decayed.iter().filter(|d| **d).count() as u32
    }

    /// Whether nothing is flagged — the fail-closed state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// Run one reflection pass, mutating `weights` in place and returning the per-lane
/// deltas. Direction is set by the sign of each lane's realized net-SOL; magnitude
/// is capped by `reflect_weight_step_bp`; the result is clamped to the envelope.
pub fn reflect(
    performance: &LanePerformance,
    weights: &mut LaneWeights,
    cfg: &Config,
) -> Vec<WeightDelta> {
    reflect_with_brain(performance, weights, cfg, &LaneDecay::none())
}

/// LAW B7: one reflection pass with an optional **reduce-only** brain adjustment.
///
/// Identical to [`reflect`] except that a lane flagged in `decay` loses an extra
/// `cfg.brain_reflect_step_bp`, and only when `cfg.brain_reflect_enable` is armed.
/// The adjustment is applied to the post-base weight and then clamped to the same
/// §56.2 envelope, so:
///
/// * a lane at the floor stays at the floor (no lane is ever killed);
/// * a lane whose aggregate is positive still moves UP, just less far — the brain
///   can slow a lane's promotion, never reverse the sign of the objective;
/// * `decay.is_empty()` ⇒ byte-identical output to [`reflect`].
pub fn reflect_with_brain(
    performance: &LanePerformance,
    weights: &mut LaneWeights,
    cfg: &Config,
    decay: &LaneDecay,
) -> Vec<WeightDelta> {
    let step = cfg.reflect_weight_step_bp;
    let floor = cfg.reflect_weight_floor_bp;
    let ceiling = cfg.reflect_weight_ceiling_bp;
    let armed = cfg.brain_reflect_enable;

    let mut deltas = Vec::with_capacity(Lane::COUNT);
    for lane in Lane::ALL {
        let net = performance.net_sol(lane);
        let before = weights.get(lane);
        let base = match net.cmp(&0) {
            std::cmp::Ordering::Greater => before.saturating_add(step).min(ceiling),
            std::cmp::Ordering::Less => before.saturating_sub(step).max(floor),
            std::cmp::Ordering::Equal => before.clamp(floor, ceiling),
        };
        // LAW B7: reduce-only, envelope-bounded, and unreachable unless armed AND
        // the lane cleared the §46 sample floor upstream.
        let after = if armed && decay.is_decayed(lane) {
            base.saturating_sub(cfg.brain_reflect_step_bp)
                .clamp(floor, ceiling)
        } else {
            base
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

#[cfg(test)]
mod tests {
    use super::*;

    fn weights_at(bp: u32) -> LaneWeights {
        let mut w = LaneWeights::default();
        for lane in Lane::ALL {
            w.set(lane, bp);
        }
        w
    }

    #[test]
    fn disarmed_brain_pass_is_identical_to_the_base_pass() {
        let cfg = Config::dev_portable();
        let mut perf = LanePerformance::new();
        perf.record(Lane::CreationSniper, 5_000);
        perf.record(Lane::ActiveMarketScalp, -5_000);
        let mut decay = LaneDecay::none();
        decay.set(Lane::CreationSniper);

        let mut a = weights_at(10_000);
        let da = reflect(&perf, &mut a, &cfg);
        let mut b = weights_at(10_000);
        // Armed flag set, but `brain_reflect_enable` is OFF by default.
        let db = reflect_with_brain(&perf, &mut b, &cfg, &decay);
        assert_eq!(da, db);
        for lane in Lane::ALL {
            assert_eq!(a.get(lane), b.get(lane));
        }
    }

    #[test]
    fn armed_decay_only_ever_reduces_and_stays_in_the_envelope() {
        let mut cfg = Config::dev_portable();
        cfg.brain_reflect_enable = true;
        let mut perf = LanePerformance::new();
        // A lane whose AGGREGATE is positive: the base pass wants to raise it.
        perf.record(Lane::CreationSniper, 5_000_000);
        let mut decay = LaneDecay::none();
        decay.set(Lane::CreationSniper);

        let mut plain = weights_at(10_000);
        reflect(&perf, &mut plain, &cfg);
        let mut armed = weights_at(10_000);
        reflect_with_brain(&perf, &mut armed, &cfg, &decay);
        assert!(
            armed.get(Lane::CreationSniper) < plain.get(Lane::CreationSniper),
            "a decayed lane must move LESS far up than the aggregate alone would"
        );
        // Reduce-only: the armed result is never ABOVE the unarmed one, on any lane.
        for lane in Lane::ALL {
            assert!(armed.get(lane) <= plain.get(lane));
        }
    }

    #[test]
    fn a_decayed_lane_at_the_floor_is_never_killed() {
        let mut cfg = Config::dev_portable();
        cfg.brain_reflect_enable = true;
        let mut perf = LanePerformance::new();
        perf.record(Lane::ActiveMarketScalp, -9_000_000);
        let mut decay = LaneDecay::none();
        for lane in Lane::ALL {
            decay.set(lane);
        }
        let mut w = weights_at(cfg.reflect_weight_floor_bp);
        reflect_with_brain(&perf, &mut w, &cfg, &decay);
        for lane in Lane::ALL {
            assert_eq!(
                w.get(lane),
                cfg.reflect_weight_floor_bp,
                "§56.2 floor is absolute: no lane is silently killed"
            );
        }
    }

    #[test]
    fn empty_decay_is_the_fail_closed_state() {
        let d = LaneDecay::none();
        assert!(d.is_empty());
        assert_eq!(d.count(), 0);
        for lane in Lane::ALL {
            assert!(!d.is_decayed(lane));
        }
    }
}
