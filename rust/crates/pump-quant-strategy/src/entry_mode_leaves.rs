//! # entry_mode_leaves — §24 EntryMode detector leaves (missing modes)
//!
//! §24's EntryMode set enumerates the admissible ways a lane opens a position.
//! Two modes named by the constitution had no detector leaf yet:
//!
//! * **PullbackContinuation** — an established uptrend that pulls back in a
//!   *controlled* way to a retest level which then *holds*, offering a
//!   continuation entry rather than chasing the extended leg.
//! * **NarrativeConfirmation** — a narrative-led candidate (attention/social
//!   velocity leads) that *then* earns independent on-chain corroboration. This
//!   is a **dormant, admission-gated** predicate: narrative signal never carries
//!   trade authority on its own (§28), so the detector stays inert until the
//!   narrative feature family is admitted, and even then requires on-chain
//!   confirmation before it is eligible.
//!
//! Each detector is a pure predicate over caller-supplied bar / retest /
//! narrative feature integers and returns an [`EntrySignal`]: an `eligible`
//! flag, a bounded `strength_bps` (0..=10 000), a [`SuggestedLane`] tag, and a
//! `dormant` flag (set when an admission gate held the predicate inert).
//!
//! ## Why a suggested-lane tag and not a new `Lane` variant
//! The lane vocabulary lives in the **dossier-locked**
//! `pump_quant_domain::market::Lane` enum, whose four variants carry stable
//! `#[repr(u8)]` discriminants that persisted journals and `DecisionRecord`s
//! decode against (`from_u8` fails closed on unknown). Adding variants there
//! would shift/relax that pinned encoding. So this leaf does **not** touch
//! `Lane`; it emits a [`SuggestedLane`] tag whose discriminants *mirror*
//! `Lane`'s exactly (`CreationSniper = 0 … ActiveMarketScalp = 3`), and the
//! engine maps the tag onto a real `Lane` by discriminant in a later wave. A
//! PullbackContinuation suggests `ActiveMarketScalp` (an already-live market);
//! a NarrativeConfirmation suggests `EarlyConfirmation` (early entry gated on
//! first confirmation of genuine activity — exactly the §24 semantics of that
//! lane).
//!
//! ## §24(d) exit-into-strength linkage
//! §24(d) requires that a position opened by these entry modes is *exited into
//! strength*, not held to exhaustion. The strength side of that linkage is the
//! **burst-climax detector** already implemented in the signals plane:
//! [`pump_quant_signals::microstructure::burst_phase`] returns
//! `BurstPhase::Climax` when the swap-arrival rate is strongly elevated but its
//! acceleration has stalled. A continuation/confirmation entry that runs into a
//! `Climax` reading is the §24(d) exit trigger — sell into the climax rather
//! than waiting for `BurstPhase::Exhaustion`. These entry leaves therefore
//! compose with that detector: they open, and the burst-climax phase closes.
//! (No dependency edge is added here — signals owns the climax detector; this
//! doc pins the composition the engine wires.)
//!
//! ## Constitution
//! §22: integer-only, no floats, no wall-clock, deterministic — identical
//! features always yield the same [`EntrySignal`]. §24: EntryMode detectors are
//! shared leaves that must run identically in LIVE/SHADOW/REPLAY; every
//! threshold is supplied via a params struct, never hardcoded in the predicate.

/// Basis-point denominator (100% = 10 000 bps); the ceiling for `strength_bps`.
pub const BPS_DENOM: u32 = 10_000;

/// A lane tag suggested by an EntryMode detector.
///
/// Discriminants mirror `pump_quant_domain::market::Lane` exactly so the engine
/// can map a tag onto the dossier-locked `Lane` by discriminant without this
/// crate depending on the domain enum or mutating its pinned encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SuggestedLane {
    /// Mirrors `Lane::CreationSniper` (= 0).
    CreationSniper = 0,
    /// Mirrors `Lane::EarlyConfirmation` (= 1).
    EarlyConfirmation = 1,
    /// Mirrors `Lane::GraduationTransition` (= 2).
    GraduationTransition = 2,
    /// Mirrors `Lane::ActiveMarketScalp` (= 3).
    ActiveMarketScalp = 3,
}

impl SuggestedLane {
    /// The stable discriminant, equal to the mirrored `Lane` discriminant.
    #[inline]
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Which EntryMode a detector represents (for attribution / journaling).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EntryMode {
    /// Controlled pullback to a holding retest inside an uptrend.
    PullbackContinuation = 0,
    /// Narrative-led candidate that earns on-chain confirmation.
    NarrativeConfirmation = 1,
}

/// The eligibility + strength result of an EntryMode detector leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntrySignal {
    /// Which mode produced this signal.
    pub mode: EntryMode,
    /// Whether the mode's conditions are met and the entry is admissible now.
    pub eligible: bool,
    /// Conviction of the setup in basis points, clamped to `0..=BPS_DENOM`.
    /// Zero whenever `eligible` is `false`.
    pub strength_bps: u32,
    /// The lane the engine should map this entry onto.
    pub suggested_lane: SuggestedLane,
    /// `true` when an admission gate held the predicate inert (the reading was
    /// not even evaluated for eligibility). Distinct from a merely-ineligible
    /// evaluated candidate.
    pub dormant: bool,
}

impl EntrySignal {
    /// Construct an ineligible (but evaluated, non-dormant) signal.
    #[inline]
    fn ineligible(mode: EntryMode, lane: SuggestedLane) -> Self {
        EntrySignal {
            mode,
            eligible: false,
            strength_bps: 0,
            suggested_lane: lane,
            dormant: false,
        }
    }

    /// Construct a dormant (admission-gated, unevaluated) signal.
    #[inline]
    fn dormant(mode: EntryMode, lane: SuggestedLane) -> Self {
        EntrySignal {
            mode,
            eligible: false,
            strength_bps: 0,
            suggested_lane: lane,
            dormant: true,
        }
    }
}

// ---------------------------------------------------------------------------
// PullbackContinuation
// ---------------------------------------------------------------------------

/// Bar / market-structure features for the PullbackContinuation detector.
///
/// All fields are deterministic point-in-time measures from the §21.6
/// bar/market-structure feature family — no wall-clock, no floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullbackFeatures {
    /// A prior uptrend (higher-highs / higher-lows structure) is confirmed.
    pub uptrend_confirmed: bool,
    /// Structure is intact — no lower-low invalidation of the trend.
    pub structure_intact: bool,
    /// Price is currently at/above the retest level (prior breakout / support),
    /// i.e. the retest is holding rather than breaking down through it.
    pub retest_level_holding: bool,
    /// How many bars the retest level has held without breaking.
    pub retest_hold_bars: u32,
    /// Depth of the pullback from the swing high as a fraction of the prior
    /// up-leg, in bps. A controlled continuation retraces within a band.
    pub pullback_depth_bps: u32,
    /// Sell-side volume during the pullback relative to the up-leg, in bps.
    /// Low ⇒ controlled (profit-taking); high ⇒ distribution.
    pub pullback_sell_volume_bps: u32,
}

/// Thresholds for the PullbackContinuation detector (operator-tunable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullbackParams {
    /// Minimum bars the retest must have held to count as holding.
    pub min_retest_hold_bars: u32,
    /// Minimum pullback depth (bps) — shallower is trend-noise, not a pullback.
    pub min_pullback_depth_bps: u32,
    /// Maximum pullback depth (bps) — deeper is a breakdown, not a continuation.
    pub max_pullback_depth_bps: u32,
    /// Maximum pullback sell volume (bps) for the pullback to count as
    /// controlled rather than distributive.
    pub max_pullback_sell_volume_bps: u32,
    /// Bars-held value at/above which the hold sub-score saturates its cap.
    pub hold_saturation_bars: u32,
}

impl PullbackParams {
    /// A deterministic fixture / sane default.
    #[must_use]
    pub const fn test() -> Self {
        PullbackParams {
            min_retest_hold_bars: 2,
            min_pullback_depth_bps: 1_000,
            max_pullback_depth_bps: 5_000,
            max_pullback_sell_volume_bps: 4_000,
            hold_saturation_bars: 8,
        }
    }
}

/// Detect a PullbackContinuation entry (leaf **em_pullback_continuation**).
///
/// Eligible iff the uptrend is confirmed and intact, the retest level is
/// holding for at least `min_retest_hold_bars`, the pullback depth is inside
/// the `[min_pullback_depth_bps, max_pullback_depth_bps]` band (deep enough to
/// be a real pullback, shallow enough not to be a breakdown), and the pullback
/// sell volume is at/below `max_pullback_sell_volume_bps` (controlled).
///
/// Strength (only when eligible) is the mean of three bounded sub-scores, each
/// in bps, so the result is monotone in "more confirmation":
/// * **hold** — `min(retest_hold_bars, hold_saturation_bars)` scaled to full
///   scale at saturation;
/// * **control** — how far below the sell-volume ceiling the pullback sits;
/// * **depth-centering** — closeness of the pullback depth to the centre of the
///   admissible band (a mid-band retrace scores highest).
///
/// Pure integer, deterministic, panic-free on any input.
#[must_use]
pub fn detect_pullback_continuation(f: &PullbackFeatures, p: &PullbackParams) -> EntrySignal {
    let lane = SuggestedLane::ActiveMarketScalp;
    let mode = EntryMode::PullbackContinuation;

    let depth_in_band = f.pullback_depth_bps >= p.min_pullback_depth_bps
        && f.pullback_depth_bps <= p.max_pullback_depth_bps;
    let eligible = f.uptrend_confirmed
        && f.structure_intact
        && f.retest_level_holding
        && f.retest_hold_bars >= p.min_retest_hold_bars
        && depth_in_band
        && f.pullback_sell_volume_bps <= p.max_pullback_sell_volume_bps;

    if !eligible {
        return EntrySignal::ineligible(mode, lane);
    }

    // Hold sub-score: linear to full scale at hold_saturation_bars.
    let hold_score = if p.hold_saturation_bars == 0 {
        BPS_DENOM
    } else {
        let held = f.retest_hold_bars.min(p.hold_saturation_bars);
        scale_bps(held, p.hold_saturation_bars)
    };

    // Control sub-score: distance below the sell-volume ceiling, as a fraction
    // of the ceiling. A zero ceiling means any (== 0) volume scores full.
    let control_score = if p.max_pullback_sell_volume_bps == 0 {
        BPS_DENOM
    } else {
        let head = p
            .max_pullback_sell_volume_bps
            .saturating_sub(f.pullback_sell_volume_bps);
        scale_bps(head, p.max_pullback_sell_volume_bps)
    };

    // Depth-centering sub-score: 1 - |depth - centre| / half-width, in bps.
    let depth_score = depth_centering_bps(
        f.pullback_depth_bps,
        p.min_pullback_depth_bps,
        p.max_pullback_depth_bps,
    );

    let strength_bps = mean3(hold_score, control_score, depth_score);

    EntrySignal {
        mode,
        eligible: true,
        strength_bps,
        suggested_lane: lane,
        dormant: false,
    }
}

// ---------------------------------------------------------------------------
// NarrativeConfirmation
// ---------------------------------------------------------------------------

/// Features for the NarrativeConfirmation detector.
///
/// The narrative side *leads*; the on-chain side *confirms*. Both are decoded
/// integer measures — attention/inflow are signed fixed-point, breadth is a
/// cluster-adjusted count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NarrativeConfirmationFeatures {
    /// Narrative / attention velocity (signed fixed-point) — the lead signal.
    pub narrative_velocity: i64,
    /// Independent (cluster-adjusted) buyers now corroborating the narrative.
    pub confirming_independent_buyers: u32,
    /// Net on-chain inflow corroborating the narrative (signed fixed-point).
    pub confirming_net_inflow: i64,
    /// A sell route mechanically exists (never enter an untradeable market).
    pub mechanically_sellable: bool,
}

/// Thresholds for the NarrativeConfirmation detector (operator-tunable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NarrativeConfirmationParams {
    /// Minimum narrative velocity for the lead signal to count.
    pub min_narrative_velocity: i64,
    /// Minimum independent confirming buyers.
    pub min_confirming_buyers: u32,
    /// Minimum confirming net inflow.
    pub min_confirming_inflow: i64,
    /// Confirming-buyer count at/above which the breadth sub-score saturates.
    pub buyer_saturation: u32,
}

impl NarrativeConfirmationParams {
    /// A deterministic fixture / sane default.
    #[must_use]
    pub const fn test() -> Self {
        NarrativeConfirmationParams {
            min_narrative_velocity: 1_000,
            min_confirming_buyers: 20,
            min_confirming_inflow: 500,
            buyer_saturation: 80,
        }
    }
}

/// Detect a NarrativeConfirmation entry (leaf **em_narrative_confirmation**) —
/// a **dormant, admission-gated** predicate.
///
/// `narrative_admitted` is the §28 / feature-admission gate: while it is
/// `false` the detector is *dormant* — it returns
/// [`EntrySignal::dormant`](EntrySignal) without ever evaluating eligibility,
/// because narrative signal carries no trade authority until the narrative
/// feature family has been admitted. Once admitted, the entry is eligible iff
/// the narrative lead clears `min_narrative_velocity` **and** independent
/// on-chain confirmation has arrived (buyers ≥ `min_confirming_buyers`, inflow
/// ≥ `min_confirming_inflow`) **and** the market is mechanically sellable.
///
/// Strength (only when eligible) is the mean of a breadth sub-score (confirming
/// buyers scaled to `buyer_saturation`) and a lead sub-score (narrative
/// velocity above its floor, scaled to twice the floor). Pure integer,
/// deterministic, panic-free on any input.
#[must_use]
pub fn detect_narrative_confirmation(
    f: &NarrativeConfirmationFeatures,
    p: &NarrativeConfirmationParams,
    narrative_admitted: bool,
) -> EntrySignal {
    let lane = SuggestedLane::EarlyConfirmation;
    let mode = EntryMode::NarrativeConfirmation;

    // Admission gate: dormant until the narrative feature family is admitted.
    if !narrative_admitted {
        return EntrySignal::dormant(mode, lane);
    }

    let eligible = f.narrative_velocity >= p.min_narrative_velocity
        && f.confirming_independent_buyers >= p.min_confirming_buyers
        && f.confirming_net_inflow >= p.min_confirming_inflow
        && f.mechanically_sellable;

    if !eligible {
        return EntrySignal::ineligible(mode, lane);
    }

    // Breadth sub-score.
    let breadth_score = if p.buyer_saturation == 0 {
        BPS_DENOM
    } else {
        let b = f.confirming_independent_buyers.min(p.buyer_saturation);
        scale_bps(b, p.buyer_saturation)
    };

    // Lead sub-score: narrative velocity above its floor, saturating at 2×floor.
    // Using i128 headroom keeps adversarial i64 extremes panic-free.
    let lead_score = {
        let floor = i128::from(p.min_narrative_velocity);
        let vel = i128::from(f.narrative_velocity);
        let span = floor.max(1); // width of the ramp above the floor
        let head = (vel - floor).clamp(0, span);
        // head/span * BPS_DENOM
        u32::try_from(head * i128::from(BPS_DENOM) / span).unwrap_or(BPS_DENOM)
    };

    let strength_bps = mean2(breadth_score, lead_score);

    EntrySignal {
        mode,
        eligible: true,
        strength_bps,
        suggested_lane: lane,
        dormant: false,
    }
}

// ---------------------------------------------------------------------------
// Bounded integer scoring helpers
// ---------------------------------------------------------------------------

/// `num / den` expressed in bps, clamped to `0..=BPS_DENOM`. `den == 0` yields
/// full scale. Widened through `u64` so no product overflows.
#[inline]
fn scale_bps(num: u32, den: u32) -> u32 {
    if den == 0 {
        return BPS_DENOM;
    }
    let r = u64::from(num) * u64::from(BPS_DENOM) / u64::from(den);
    u32::try_from(r).unwrap_or(BPS_DENOM).min(BPS_DENOM)
}

/// Depth-centering score in bps: full scale when `depth` sits at the centre of
/// `[lo, hi]`, falling linearly to zero at either edge. A degenerate band
/// (`hi <= lo`) yields full scale.
#[inline]
fn depth_centering_bps(depth: u32, lo: u32, hi: u32) -> u32 {
    if hi <= lo {
        return BPS_DENOM;
    }
    let centre = lo + (hi - lo) / 2;
    let half = ((hi - lo) / 2).max(1);
    let dist = depth.abs_diff(centre).min(half);
    scale_bps(half - dist, half)
}

/// Arithmetic mean of two bps sub-scores (no overflow: sum fits in `u32`).
#[inline]
fn mean2(a: u32, b: u32) -> u32 {
    ((u64::from(a) + u64::from(b)) / 2) as u32
}

/// Arithmetic mean of three bps sub-scores (widened to avoid overflow).
#[inline]
fn mean3(a: u32, b: u32, c: u32) -> u32 {
    ((u64::from(a) + u64::from(b) + u64::from(c)) / 3) as u32
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- PullbackContinuation ------------------------------------------------

    fn pb_ok() -> PullbackFeatures {
        PullbackFeatures {
            uptrend_confirmed: true,
            structure_intact: true,
            retest_level_holding: true,
            retest_hold_bars: 4,
            pullback_depth_bps: 3_000,
            pullback_sell_volume_bps: 1_000,
        }
    }

    fn pb(f: PullbackFeatures) -> EntrySignal {
        detect_pullback_continuation(&f, &PullbackParams::test())
    }

    #[test]
    fn pullback_eligible_baseline() {
        let s = pb(pb_ok());
        assert!(s.eligible);
        assert!(!s.dormant);
        assert_eq!(s.mode, EntryMode::PullbackContinuation);
        assert_eq!(s.suggested_lane, SuggestedLane::ActiveMarketScalp);
        assert!(s.strength_bps > 0 && s.strength_bps <= BPS_DENOM);
    }

    #[test]
    fn pullback_requires_uptrend() {
        let mut f = pb_ok();
        f.uptrend_confirmed = false;
        let s = pb(f);
        assert!(!s.eligible);
        assert_eq!(s.strength_bps, 0);
    }

    #[test]
    fn pullback_requires_structure_intact() {
        let mut f = pb_ok();
        f.structure_intact = false;
        assert!(!pb(f).eligible);
    }

    #[test]
    fn pullback_requires_retest_holding() {
        let mut f = pb_ok();
        f.retest_level_holding = false;
        assert!(!pb(f).eligible);
    }

    #[test]
    fn pullback_hold_bars_boundary() {
        let mut f = pb_ok();
        f.retest_hold_bars = 2; // == min
        assert!(pb(f).eligible);
        f.retest_hold_bars = 1; // < min
        assert!(!pb(f).eligible);
    }

    #[test]
    fn pullback_depth_band_boundaries() {
        let mut lo = pb_ok();
        lo.pullback_depth_bps = 1_000; // == min, in band
        assert!(pb(lo).eligible);

        let mut below = pb_ok();
        below.pullback_depth_bps = 999; // too shallow
        assert!(!pb(below).eligible);

        let mut hi = pb_ok();
        hi.pullback_depth_bps = 5_000; // == max, in band
        assert!(pb(hi).eligible);

        let mut above = pb_ok();
        above.pullback_depth_bps = 5_001; // breakdown
        assert!(!pb(above).eligible);
    }

    #[test]
    fn pullback_sell_volume_boundary() {
        let mut f = pb_ok();
        f.pullback_sell_volume_bps = 4_000; // == max controlled
        assert!(pb(f).eligible);
        f.pullback_sell_volume_bps = 4_001; // distributive
        assert!(!pb(f).eligible);
    }

    #[test]
    fn pullback_strength_monotone_in_hold() {
        let mut a = pb_ok();
        a.retest_hold_bars = 2;
        let mut b = pb_ok();
        b.retest_hold_bars = 8;
        assert!(pb(b).strength_bps >= pb(a).strength_bps);
    }

    #[test]
    fn pullback_strength_monotone_in_control() {
        let mut noisy = pb_ok();
        noisy.pullback_sell_volume_bps = 3_500;
        let mut clean = pb_ok();
        clean.pullback_sell_volume_bps = 0;
        assert!(pb(clean).strength_bps >= pb(noisy).strength_bps);
    }

    #[test]
    fn pullback_depth_centre_scores_highest() {
        // Centre of [1000, 5000] is 3000.
        let mut mid = pb_ok();
        mid.pullback_depth_bps = 3_000;
        let mut edge = pb_ok();
        edge.pullback_depth_bps = 1_000;
        assert!(pb(mid).strength_bps >= pb(edge).strength_bps);
    }

    #[test]
    fn pullback_strength_bounded() {
        let f = pb_ok();
        assert!(pb(f).strength_bps <= BPS_DENOM);
    }

    #[test]
    fn pullback_adversarial_no_panic() {
        let f = PullbackFeatures {
            uptrend_confirmed: true,
            structure_intact: true,
            retest_level_holding: true,
            retest_hold_bars: u32::MAX,
            pullback_depth_bps: u32::MAX,
            pullback_sell_volume_bps: u32::MAX,
        };
        // depth out of band ⇒ ineligible, but must not panic.
        let s = pb(f);
        assert!(!s.eligible);
    }

    #[test]
    fn pullback_adversarial_eligible_no_panic() {
        // In-band extremes that still classify eligible.
        let f = PullbackFeatures {
            uptrend_confirmed: true,
            structure_intact: true,
            retest_level_holding: true,
            retest_hold_bars: u32::MAX,
            pullback_depth_bps: 3_000,
            pullback_sell_volume_bps: 0,
        };
        let s = pb(f);
        assert!(s.eligible);
        assert!(s.strength_bps <= BPS_DENOM);
    }

    // -- NarrativeConfirmation ----------------------------------------------

    fn nc_ok() -> NarrativeConfirmationFeatures {
        NarrativeConfirmationFeatures {
            narrative_velocity: 2_000,
            confirming_independent_buyers: 40,
            confirming_net_inflow: 1_000,
            mechanically_sellable: true,
        }
    }

    fn nc(f: NarrativeConfirmationFeatures, admitted: bool) -> EntrySignal {
        detect_narrative_confirmation(&f, &NarrativeConfirmationParams::test(), admitted)
    }

    #[test]
    fn narrative_dormant_until_admitted() {
        let s = nc(nc_ok(), false);
        assert!(s.dormant);
        assert!(!s.eligible);
        assert_eq!(s.strength_bps, 0);
        assert_eq!(s.suggested_lane, SuggestedLane::EarlyConfirmation);
    }

    #[test]
    fn narrative_eligible_when_admitted_and_confirmed() {
        let s = nc(nc_ok(), true);
        assert!(s.eligible);
        assert!(!s.dormant);
        assert_eq!(s.mode, EntryMode::NarrativeConfirmation);
        assert!(s.strength_bps > 0 && s.strength_bps <= BPS_DENOM);
    }

    #[test]
    fn narrative_admitted_but_unconfirmed_is_ineligible_not_dormant() {
        let mut f = nc_ok();
        f.confirming_independent_buyers = 0; // no on-chain confirmation
        let s = nc(f, true);
        assert!(!s.eligible);
        assert!(!s.dormant); // it WAS evaluated, just failed confirmation
    }

    #[test]
    fn narrative_requires_velocity_floor() {
        let mut f = nc_ok();
        f.narrative_velocity = 999; // below 1000
        assert!(!nc(f, true).eligible);
        f.narrative_velocity = 1_000; // == floor
        assert!(nc(f, true).eligible);
    }

    #[test]
    fn narrative_requires_buyer_floor() {
        let mut f = nc_ok();
        f.confirming_independent_buyers = 19; // below 20
        assert!(!nc(f, true).eligible);
        f.confirming_independent_buyers = 20; // == floor
        assert!(nc(f, true).eligible);
    }

    #[test]
    fn narrative_requires_inflow_floor() {
        let mut f = nc_ok();
        f.confirming_net_inflow = 499;
        assert!(!nc(f, true).eligible);
        f.confirming_net_inflow = 500;
        assert!(nc(f, true).eligible);
    }

    #[test]
    fn narrative_requires_sellable() {
        let mut f = nc_ok();
        f.mechanically_sellable = false;
        assert!(!nc(f, true).eligible);
    }

    #[test]
    fn narrative_strength_monotone_in_breadth() {
        let mut a = nc_ok();
        a.confirming_independent_buyers = 20;
        let mut b = nc_ok();
        b.confirming_independent_buyers = 80;
        assert!(nc(b, true).strength_bps >= nc(a, true).strength_bps);
    }

    #[test]
    fn narrative_strength_bounded() {
        let mut f = nc_ok();
        f.confirming_independent_buyers = u32::MAX;
        f.narrative_velocity = i64::MAX;
        f.confirming_net_inflow = i64::MAX;
        let s = nc(f, true);
        assert!(s.eligible);
        assert!(s.strength_bps <= BPS_DENOM);
    }

    #[test]
    fn narrative_adversarial_negative_no_panic() {
        let f = NarrativeConfirmationFeatures {
            narrative_velocity: i64::MIN,
            confirming_independent_buyers: 0,
            confirming_net_inflow: i64::MIN,
            mechanically_sellable: false,
        };
        let s = nc(f, true);
        assert!(!s.eligible);
    }

    #[test]
    fn narrative_dormant_dominates_bad_features() {
        // Even garbage features return dormant (not evaluated) when not admitted.
        let f = NarrativeConfirmationFeatures {
            narrative_velocity: i64::MIN,
            confirming_independent_buyers: u32::MAX,
            confirming_net_inflow: i64::MAX,
            mechanically_sellable: true,
        };
        assert!(nc(f, false).dormant);
    }

    // -- SuggestedLane mirrors Lane -----------------------------------------

    #[test]
    fn suggested_lane_discriminants_mirror_domain_lane() {
        assert_eq!(SuggestedLane::CreationSniper.as_u8(), 0);
        assert_eq!(SuggestedLane::EarlyConfirmation.as_u8(), 1);
        assert_eq!(SuggestedLane::GraduationTransition.as_u8(), 2);
        assert_eq!(SuggestedLane::ActiveMarketScalp.as_u8(), 3);
    }

    // -- helper coverage -----------------------------------------------------

    #[test]
    fn scale_bps_clamps_and_guards() {
        assert_eq!(scale_bps(1, 2), 5_000);
        assert_eq!(scale_bps(5, 0), BPS_DENOM); // zero denom ⇒ full
        assert_eq!(scale_bps(10, 5), BPS_DENOM); // over-full clamps
        assert_eq!(scale_bps(u32::MAX, 1), BPS_DENOM);
    }

    #[test]
    fn depth_centering_edges_and_centre() {
        assert_eq!(depth_centering_bps(3_000, 1_000, 5_000), BPS_DENOM); // centre
        assert_eq!(depth_centering_bps(1_000, 1_000, 5_000), 0); // low edge
        assert_eq!(depth_centering_bps(5_000, 1_000, 5_000), 0); // high edge
        assert_eq!(depth_centering_bps(7, 9, 9), BPS_DENOM); // degenerate band
    }

    #[test]
    fn deterministic_repeat() {
        let f = pb_ok();
        assert_eq!(pb(f), pb(f));
        let g = nc_ok();
        assert_eq!(nc(g, true), nc(g, true));
    }
}
