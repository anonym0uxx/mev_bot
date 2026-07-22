//! Anti-bundle economic heuristic — cumulative-fees-vs-activity FLOOR filter
//! (constitution §70.10, §21.7/§26 risk-priced participation).
//!
//! A genuine, broadly-traded token pays a *plausible* amount of cumulative
//! priority/tip fees relative to its apparent on-chain activity: real competing
//! participants bid up fees. A deployer that is the ONLY real participant
//! (bundle / wash launch) has no reason to overpay — it minimizes fees, so its
//! cumulative-fees-per-activity floor comes out implausibly LOW versus its
//! advertised transaction count / trader breadth. This module computes that
//! floor metric and emits a **two-sided fade prior** (never a standalone hard
//! veto): the flag is a fade/veto *covariate* for the supervisor's
//! `safety_integrity` + `economic_gate`, consistent with §21.7/§26.
//!
//! This is the opposite-sign companion to
//! [`crate::launch_trajectory::analyze_creation_window`], which measures
//! implausibly HIGH competitor spend (adverse selection). Here we flag
//! implausibly LOW cumulative spend (manufactured/wash activity).
//!
//! # Constitution constraints (§22)
//!
//! Pure, deterministic, integer-only. Fees are lamports (`u64`/`u128`),
//! intensity is scaled micro-lamports-per-activity, the fade is basis points.
//! `u128` widening on every product; explicit division guards. Bounded state
//! (§99): nothing accumulates across calls. Live fee/activity decoding is
//! server-side; callers feed already-decoded fixtures.

use crate::launch_trajectory::FirstSlotTx;

/// Fixed-point scale for the fee-intensity metric: fees are reported per unit
/// of activity multiplied by this factor so that sub-lamport intensities remain
/// integer-representable.
pub const INTENSITY_SCALE: u128 = 1_000_000;

/// Configuration for the anti-bundle fee-floor heuristic (§70.10).
///
/// Responsibility: the (externally-tuned, recorded-prior) thresholds separating
/// a plausible fee footprint from an implausibly-cheap manufactured one.
/// Constitution §22: integer parameters, `Copy` for cheap threading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeFloorConfig {
    /// Minimum activity below which the sample is too small to judge — the
    /// verdict is [`FeeFloorStatus::InsufficientActivity`] and the fade is 0.
    pub min_activity: u64,
    /// Floor fee-intensity (scaled micro-lamports per activity unit, see
    /// [`INTENSITY_SCALE`]). Intensity strictly below this is implausibly low.
    pub floor_intensity: u64,
}

impl FeeFloorConfig {
    /// A neutral default: require at least 8 units of activity, and treat an
    /// average combined fee below 5_000 lamports/activity as implausibly cheap
    /// (`5_000 * INTENSITY_SCALE` in scaled units).
    ///
    /// Responsibility: portable default prior (§70.10). Constitution §22: pure.
    pub const fn neutral() -> Self {
        FeeFloorConfig {
            min_activity: 8,
            floor_intensity: 5_000 * INTENSITY_SCALE as u64,
        }
    }
}

impl Default for FeeFloorConfig {
    fn default() -> Self {
        Self::neutral()
    }
}

/// Coarse verdict of the fee-floor heuristic.
///
/// Responsibility: enumerate the three economically-distinct outcomes so the
/// supervisor can branch explicitly (§70.10). Constitution §22: data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeFloorStatus {
    /// Too little activity to judge — no fade applied.
    InsufficientActivity,
    /// Fee footprint is consistent with genuine competing participation.
    Plausible,
    /// Cumulative fees are implausibly low for the advertised activity:
    /// bundle/wash-flagged (a fade covariate, not a veto).
    ImplausiblyLow,
}

/// Result of the anti-bundle fee-floor assessment (§70.10).
///
/// Responsibility: the fade covariate consumed by `safety_integrity` and
/// `economic_gate`. `fade_bps` is a magnitude in `0..=10_000`: 0 = no fade,
/// larger = a stronger (bundle/wash) fade prior. Constitution §22: integer/bps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeePlausibility {
    /// Coarse verdict.
    pub status: FeeFloorStatus,
    /// Measured fee-intensity (scaled micro-lamports per activity unit).
    pub intensity: u64,
    /// Activity count the intensity was measured over.
    pub activity_count: u64,
    /// Fade magnitude in basis points (`0..=10_000`). Non-zero only when the
    /// status is [`FeeFloorStatus::ImplausiblyLow`].
    pub fade_bps: u32,
}

/// Compute the fee-intensity metric: cumulative fees per unit of activity,
/// scaled by [`INTENSITY_SCALE`]. Returns `0` when `activity_count == 0`.
///
/// `intensity = total_fees_lamports * INTENSITY_SCALE / activity_count`.
///
/// Responsibility: the pure floor metric (§70.10). Constitution §22: `u128`
/// widening, integer division, `activity_count == 0` guard, saturating cast.
#[inline]
pub fn fee_intensity(total_fees_lamports: u128, activity_count: u64) -> u64 {
    if activity_count == 0 {
        return 0;
    }
    (total_fees_lamports.saturating_mul(INTENSITY_SCALE) / activity_count as u128)
        .min(u64::MAX as u128) as u64
}

/// Assess whether a token's cumulative fee footprint is implausibly low for its
/// activity (§70.10).
///
/// Logic, in order:
/// 1. `activity_count < cfg.min_activity` => [`FeeFloorStatus::InsufficientActivity`],
///    `fade_bps = 0`.
/// 2. Otherwise compute intensity. `intensity >= cfg.floor_intensity` =>
///    [`FeeFloorStatus::Plausible`], `fade_bps = 0`.
/// 3. Otherwise [`FeeFloorStatus::ImplausiblyLow`] with
///    `fade_bps = (floor - intensity) * 10_000 / floor` (clamped to `10_000`);
///    the further below the floor, the stronger the fade.
///
/// Responsibility: single entry point emitting the two-sided fade covariate
/// (§70.10). Constitution §22: integer/bps, division guards, no veto here.
#[inline]
pub fn assess_fee_floor(
    total_fees_lamports: u128,
    activity_count: u64,
    cfg: &FeeFloorConfig,
) -> FeePlausibility {
    if activity_count < cfg.min_activity {
        return FeePlausibility {
            status: FeeFloorStatus::InsufficientActivity,
            intensity: fee_intensity(total_fees_lamports, activity_count),
            activity_count,
            fade_bps: 0,
        };
    }
    let intensity = fee_intensity(total_fees_lamports, activity_count);
    if cfg.floor_intensity == 0 || intensity >= cfg.floor_intensity {
        return FeePlausibility {
            status: FeeFloorStatus::Plausible,
            intensity,
            activity_count,
            fade_bps: 0,
        };
    }
    let deficit = cfg.floor_intensity - intensity;
    let fade_bps = ((deficit as u128 * 10_000) / cfg.floor_intensity as u128).min(10_000) as u32;
    FeePlausibility {
        status: FeeFloorStatus::ImplausiblyLow,
        intensity,
        activity_count,
        fade_bps,
    }
}

/// Sum the combined `priority_fee_lamports + tip_lamports` across a slice of
/// creation-window / first-slot transactions.
///
/// Responsibility: cumulative-fee accumulator reusing the already-decoded fee
/// fields on [`FirstSlotTx`] (§70.10). Constitution §22: `u128` accumulation,
/// `saturating_add` per tx.
#[inline]
pub fn cumulative_fees_lamports(txs: &[FirstSlotTx]) -> u128 {
    let mut total: u128 = 0;
    for t in txs {
        let combined = t.priority_fee_lamports.saturating_add(t.tip_lamports);
        total = total.saturating_add(combined as u128);
    }
    total
}

/// Convenience: assess the fee floor directly over a first-slot tx slice, using
/// the transaction count as the activity denominator (§70.10).
///
/// Responsibility: end-to-end helper composing [`cumulative_fees_lamports`] and
/// [`assess_fee_floor`] over [`FirstSlotTx`] fixtures. Constitution §22: pure.
#[inline]
pub fn assess_first_slot_fee_floor(txs: &[FirstSlotTx], cfg: &FeeFloorConfig) -> FeePlausibility {
    let total = cumulative_fees_lamports(txs);
    assess_fee_floor(total, txs.len() as u64, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fst(pf: u64, tip: u64) -> FirstSlotTx {
        FirstSlotTx {
            tipper_entity: 0,
            priority_fee_lamports: pf,
            tip_lamports: tip,
            is_bundle: false,
            is_known_sniper: false,
        }
    }

    #[test]
    fn intensity_is_scaled_fees_per_activity() {
        // 100_000 lamports over 10 activity => 10_000 lamports/activity,
        // scaled by 1e6 => 10_000_000_000.
        assert_eq!(fee_intensity(100_000, 10), 10_000 * INTENSITY_SCALE as u64);
        // zero activity guarded.
        assert_eq!(fee_intensity(100_000, 0), 0);
        // zero fees => zero.
        assert_eq!(fee_intensity(0, 25), 0);
    }

    #[test]
    fn insufficient_activity_never_fades() {
        let cfg = FeeFloorConfig::neutral(); // min_activity 8
        let r = assess_fee_floor(1, 3, &cfg);
        assert_eq!(r.status, FeeFloorStatus::InsufficientActivity);
        assert_eq!(r.fade_bps, 0);
        assert_eq!(r.activity_count, 3);
    }

    #[test]
    fn plausible_when_fees_meet_floor() {
        let cfg = FeeFloorConfig::neutral(); // floor = 5_000 * 1e6, min_activity 8
                                             // 50 txs paying 6_000 lamports each => 300_000 total, intensity
                                             // 6_000 * 1e6 >= 5_000 * 1e6 => plausible.
        let r = assess_fee_floor(300_000, 50, &cfg);
        assert_eq!(r.status, FeeFloorStatus::Plausible);
        assert_eq!(r.fade_bps, 0);
        assert_eq!(r.intensity, 6_000 * INTENSITY_SCALE as u64);
    }

    #[test]
    fn implausibly_low_fades_proportionally() {
        let cfg = FeeFloorConfig::neutral(); // floor = 5_000 * 1e6
                                             // 100 txs paying only 1_000 lamports each => 100_000 total,
                                             // intensity = 1_000 * 1e6. deficit = 4_000/5_000 => 8_000 bps.
        let r = assess_fee_floor(100_000, 100, &cfg);
        assert_eq!(r.status, FeeFloorStatus::ImplausiblyLow);
        assert_eq!(r.intensity, 1_000 * INTENSITY_SCALE as u64);
        assert_eq!(r.fade_bps, 8_000);
    }

    #[test]
    fn near_zero_fees_fade_saturates_high() {
        let cfg = FeeFloorConfig::neutral();
        // 20 txs paying 1 lamport total => intensity ~ 50_000 (=1*1e6/20),
        // far below floor 5e9 => fade clamps near 10_000.
        let r = assess_fee_floor(1, 20, &cfg);
        assert_eq!(r.status, FeeFloorStatus::ImplausiblyLow);
        assert_eq!(r.fade_bps, 9_999);
    }

    #[test]
    fn zero_floor_config_is_always_plausible() {
        let cfg = FeeFloorConfig {
            min_activity: 1,
            floor_intensity: 0,
        };
        let r = assess_fee_floor(0, 100, &cfg);
        assert_eq!(r.status, FeeFloorStatus::Plausible);
        assert_eq!(r.fade_bps, 0);
    }

    #[test]
    fn cumulative_fees_sums_priority_and_tip() {
        let txs = [fst(1_000, 500), fst(2_000, 0), fst(0, 250)];
        assert_eq!(cumulative_fees_lamports(&txs), 3_750);
    }

    #[test]
    fn first_slot_helper_flags_cheap_wash_launch() {
        let cfg = FeeFloorConfig {
            min_activity: 3,
            floor_intensity: 2_000 * INTENSITY_SCALE as u64,
        };
        // 4 txs paying 100 lamports combined each => total 400, intensity
        // 100 * 1e6, well below the 2_000 * 1e6 floor => flagged.
        let txs = [fst(60, 40), fst(70, 30), fst(50, 50), fst(90, 10)];
        let r = assess_first_slot_fee_floor(&txs, &cfg);
        assert_eq!(r.status, FeeFloorStatus::ImplausiblyLow);
        assert_eq!(r.activity_count, 4);
        assert_eq!(r.intensity, 100 * INTENSITY_SCALE as u64);
        // deficit 1_900/2_000 => 9_500 bps.
        assert_eq!(r.fade_bps, 9_500);
    }

    #[test]
    fn empty_first_slot_is_insufficient() {
        let cfg = FeeFloorConfig::neutral();
        let r = assess_first_slot_fee_floor(&[], &cfg);
        assert_eq!(r.status, FeeFloorStatus::InsufficientActivity);
        assert_eq!(r.activity_count, 0);
        assert_eq!(r.fade_bps, 0);
    }
}
