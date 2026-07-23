//! Section 27 **creator-classification taxonomy** — a deterministic archetype
//! label over the point-in-time creator/deployer components that already exist
//! elsewhere in this crate.
//!
//! Where this lives and why: the raw §27 deployer components
//! ([`crate::deployer_credibility`]) — prior-CA count, serial-deploy burst
//! occupancy, verified-vs-self-claimed partnerships — are produced here in
//! `pump-quant-wallet-graph`, and the [`CreatorState`] reducer that supplies
//! distribution/dump measures lives beside them in the market-state plane. This
//! classifier is a *pure reducer over those already-measured integers*, so it
//! belongs next to the credibility features it consumes rather than in the
//! signals/strategy planes that will *weight* its output. It emits an archetype
//! label; it never emits a verdict or a size (§6.4 "creator risk is a derived,
//! separable value" — the label feeds archetype/risk weighting downstream, it
//! does not gate).
//!
//! ## §6.4 unknown-stays-unknown
//! When the evidence base is thin — too few prior launches, no resolved
//! terminal outcomes, no distinctive per-launch tell — the classifier returns
//! [`CreatorClass::Unknown`] rather than guessing an archetype. A missing
//! history is never silently coerced into a benign or a malicious label.
//!
//! ## Determinism & arithmetic law (§22)
//! Every input is a caller-supplied measured integer. The classifier is a pure,
//! total, order-independent function: identical inputs always yield the same
//! label. All ratios are computed by widening to `u128` and dividing in basis
//! points ([`crate::BPS_DENOM`]); there is no float, wall-clock, RNG, or I/O,
//! and no input (including saturated `u32::MAX` / `u64::MAX` values) can panic.

use crate::BPS_DENOM;

/// A creator/deployer behavioral archetype (§27).
///
/// The variants are ordered from most-extractive to most-constructive; the
/// classifier resolves them by a fixed priority cascade (see
/// [`classify_creator`]), not by this declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CreatorClass {
    /// Repeatedly ships launches that end in a rug / hard dump — a confirmed
    /// serial rugger (requires resolved terminal outcomes as evidence).
    SerialRug = 0,
    /// Cycles many launches in rapid succession and dumps holdings to harvest
    /// fees / exit liquidity, without necessarily a clean "rug" label.
    VolumeFarmer = 1,
    /// Ships launches that briefly run and then die — short median survival,
    /// not necessarily malicious, but structurally a fast fade.
    ShortLivedRunner = 2,
    /// Builds durable, high-retention communities across launches with low
    /// distribution pressure.
    CommunityBuilder = 3,
    /// Repeatedly launches driven by an active livestream / streamer meta.
    StreamerMeta = 4,
    /// This launch is a metadata-mimicry clone of a pre-existing token.
    Copycat = 5,
    /// Evidence is too thin to assign an archetype (§6.4).
    Unknown = 6,
}

impl CreatorClass {
    /// Stable `u8` label for compact serialization / downstream keying.
    #[inline]
    #[must_use]
    pub fn label(self) -> u8 {
        self as u8
    }
}

/// Already-measured, point-in-time creator components the classifier reduces.
///
/// Every field is a caller-supplied integer produced upstream (deployer
/// credibility features, the creator-state reducer, launch-trajectory survival
/// tracking, metadata-similarity scoring). No field is derived here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatorInputs {
    /// Number of prior launches attributed to this creator strictly before the
    /// decision slot (point-in-time; from [`crate::deployer_credibility`]).
    pub prior_launch_count: u32,
    /// Of the prior launches, how many have a *known resolved* terminal outcome
    /// (the evidence base for outcome-derived classes; thin ⇒ `Unknown`).
    pub resolved_launch_count: u32,
    /// Of the resolved launches, how many ended in a rug / hard dump.
    pub rugged_launch_count: u32,
    /// Largest number of launches the creator shipped inside one serial window
    /// (rapid-fire burst occupancy, from the deployer-credibility detector).
    pub max_launches_in_window: u32,
    /// Aggregate creator distribution / dump intensity across launches, in bps
    /// of holdings sold shortly after launch (from the creator-state reducer's
    /// sold-fraction measures).
    pub dump_intensity_bps: u32,
    /// Median survival duration of the creator's prior launches, in seconds.
    pub median_survival_secs: u64,
    /// Community retention / cohesion measure across the creator's tokens (bps).
    pub community_retention_bps: u32,
    /// Fraction of the creator's launches driven by an active livestream /
    /// streamer meta (bps).
    pub streamer_launch_ratio_bps: u32,
    /// Metadata-mimicry: similarity of *this* launch to a pre-existing token
    /// (bps); a per-launch tell knowable even for a first-time deployer.
    pub copycat_similarity_bps: u32,
}

/// Operator-tunable classification thresholds (versioned, never magic in the
/// decision path — every comparison in [`classify_creator`] reads a field here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatorThresholds {
    /// Minimum prior-launch count before any *history*-derived behavioral class
    /// (serial-runner / community-builder) may be assigned (§6.4 evidence gate).
    pub min_history_launches: u32,
    /// Minimum resolved terminal outcomes before [`CreatorClass::SerialRug`]
    /// may be assigned (§6.4 evidence gate for outcome-derived classes).
    pub min_resolved_for_rug: u32,
    /// Minimum rugged-launch count for [`CreatorClass::SerialRug`].
    pub serial_rug_min_count: u32,
    /// At/above this rugged-of-resolved ratio ⇒ serial rug (bps).
    pub serial_rug_ratio_bps: u32,
    /// At/above this serial-window occupancy ⇒ volume-farmer candidate.
    pub volume_farmer_window_launches: u32,
    /// At/above this dump intensity (with the window occupancy) ⇒ volume farmer.
    pub volume_farmer_dump_bps: u32,
    /// At/above this metadata similarity ⇒ [`CreatorClass::Copycat`] (bps).
    pub copycat_similarity_bps: u32,
    /// Strictly below this median survival ⇒ [`CreatorClass::ShortLivedRunner`].
    pub short_lived_survival_secs: u64,
    /// At/above this streamer-launch ratio ⇒ [`CreatorClass::StreamerMeta`].
    pub streamer_ratio_bps: u32,
    /// At/above this retention ⇒ community-builder candidate (bps).
    pub community_retention_bps: u32,
    /// At/above this median survival ⇒ community-builder candidate (seconds).
    pub community_min_survival_secs: u64,
    /// At/below this dump intensity ⇒ community-builder eligible (bps).
    pub community_max_dump_bps: u32,
}

impl CreatorThresholds {
    /// A deterministic fixture used by the tests and as a sane default.
    #[must_use]
    pub const fn test() -> Self {
        CreatorThresholds {
            min_history_launches: 2,
            min_resolved_for_rug: 2,
            serial_rug_min_count: 2,
            serial_rug_ratio_bps: 5_000,
            volume_farmer_window_launches: 3,
            volume_farmer_dump_bps: 4_000,
            copycat_similarity_bps: 7_000,
            short_lived_survival_secs: 3_600,
            streamer_ratio_bps: 6_000,
            community_retention_bps: 6_000,
            community_min_survival_secs: 86_400,
            community_max_dump_bps: 2_000,
        }
    }
}

/// Ratio of `num` to `den` in basis points, widened through `u128`. Returns `0`
/// when `den == 0` (callers gate on a positive denominator before relying on
/// the value). Saturates into `u32` so an adversarial numerator cannot panic.
#[inline]
fn ratio_bps_u32(num: u32, den: u32) -> u32 {
    if den == 0 {
        return 0;
    }
    let r = u128::from(num) * u128::from(BPS_DENOM) / u128::from(den);
    u32::try_from(r).unwrap_or(u32::MAX)
}

/// Classify a creator into exactly one [`CreatorClass`] (§27).
///
/// Priority cascade, first match wins, so the result is a total,
/// order-independent function of the inputs:
///
/// 1. **SerialRug** — confirmed repeated rugs (requires ≥ `min_resolved_for_rug`
///    resolved outcomes, ≥ `serial_rug_min_count` rugs, and a rugged-of-resolved
///    ratio ≥ `serial_rug_ratio_bps`). The most extractive read dominates.
/// 2. **VolumeFarmer** — rapid-fire serial launches (`max_launches_in_window ≥
///    volume_farmer_window_launches`) combined with heavy distribution
///    (`dump_intensity_bps ≥ volume_farmer_dump_bps`).
/// 3. **Copycat** — a per-launch metadata-mimicry tell at/above
///    `copycat_similarity_bps` (knowable without any launch history).
/// 4. **ShortLivedRunner** — sufficient history and a median survival strictly
///    below `short_lived_survival_secs`.
/// 5. **StreamerMeta** — streamer-driven launch ratio at/above `streamer_ratio_bps`.
/// 6. **CommunityBuilder** — sufficient history with high retention, long
///    survival, and low distribution.
/// 7. **Unknown** — thin evidence / no distinctive tell (§6.4).
///
/// Pure integer, deterministic, panic-free on any input.
#[must_use]
pub fn classify_creator(inp: &CreatorInputs, th: &CreatorThresholds) -> CreatorClass {
    use CreatorClass::*;

    let has_history = inp.prior_launch_count >= th.min_history_launches;
    let has_resolved = inp.resolved_launch_count >= th.min_resolved_for_rug;

    // 1. Confirmed serial rugger (outcome-derived; needs resolved evidence).
    if has_resolved
        && inp.rugged_launch_count >= th.serial_rug_min_count
        && ratio_bps_u32(inp.rugged_launch_count, inp.resolved_launch_count)
            >= th.serial_rug_ratio_bps
    {
        return SerialRug;
    }

    // 2. Volume farmer: rapid-fire serial launches + heavy distribution.
    if inp.max_launches_in_window >= th.volume_farmer_window_launches
        && inp.dump_intensity_bps >= th.volume_farmer_dump_bps
    {
        return VolumeFarmer;
    }

    // 3. Copycat clone: per-launch metadata-mimicry tell (history-independent).
    if inp.copycat_similarity_bps >= th.copycat_similarity_bps {
        return Copycat;
    }

    // 4. Short-lived runner: prior launches ran briefly then died.
    if has_history && inp.median_survival_secs < th.short_lived_survival_secs {
        return ShortLivedRunner;
    }

    // 5. Streamer meta: launches repeatedly driven by active livestreams.
    if inp.streamer_launch_ratio_bps >= th.streamer_ratio_bps {
        return StreamerMeta;
    }

    // 6. Community builder: durable, high-retention, low distribution.
    if has_history
        && inp.community_retention_bps >= th.community_retention_bps
        && inp.median_survival_secs >= th.community_min_survival_secs
        && inp.dump_intensity_bps <= th.community_max_dump_bps
    {
        return CommunityBuilder;
    }

    // 7. §6.4 unknown-stays-unknown.
    Unknown
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A thin-evidence baseline: no history, no tell — classifies to `Unknown`
    /// until a test flips a specific field.
    fn base() -> CreatorInputs {
        CreatorInputs {
            prior_launch_count: 0,
            resolved_launch_count: 0,
            rugged_launch_count: 0,
            max_launches_in_window: 0,
            dump_intensity_bps: 0,
            median_survival_secs: 1_000_000,
            community_retention_bps: 0,
            streamer_launch_ratio_bps: 0,
            copycat_similarity_bps: 0,
        }
    }

    fn classify(i: CreatorInputs) -> CreatorClass {
        classify_creator(&i, &CreatorThresholds::test())
    }

    #[test]
    fn labels_are_stable_and_distinct() {
        assert_eq!(CreatorClass::SerialRug.label(), 0);
        assert_eq!(CreatorClass::VolumeFarmer.label(), 1);
        assert_eq!(CreatorClass::ShortLivedRunner.label(), 2);
        assert_eq!(CreatorClass::CommunityBuilder.label(), 3);
        assert_eq!(CreatorClass::StreamerMeta.label(), 4);
        assert_eq!(CreatorClass::Copycat.label(), 5);
        assert_eq!(CreatorClass::Unknown.label(), 6);
    }

    #[test]
    fn thin_evidence_stays_unknown() {
        assert_eq!(classify(base()), CreatorClass::Unknown);
    }

    #[test]
    fn thin_evidence_beats_a_history_positive_read() {
        // High retention but zero launch history: cannot be CommunityBuilder.
        let mut f = base();
        f.community_retention_bps = 10_000;
        f.median_survival_secs = 10_000_000;
        assert_eq!(classify(f), CreatorClass::Unknown);
    }

    #[test]
    fn serial_rug_requires_resolved_evidence() {
        // Two rugs but only claimed once resolved: below evidence gate ⇒ not rug.
        let mut f = base();
        f.resolved_launch_count = 1;
        f.rugged_launch_count = 1;
        assert_ne!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn serial_rug_classified_at_threshold() {
        let mut f = base();
        f.resolved_launch_count = 4;
        f.rugged_launch_count = 2; // 2/4 = 5000 bps exactly == threshold
        assert_eq!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn serial_rug_boundary_just_below_ratio() {
        let mut f = base();
        f.resolved_launch_count = 5;
        f.rugged_launch_count = 2; // 2/5 = 4000 bps < 5000 ⇒ not serial rug
        assert_ne!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn serial_rug_min_count_gate() {
        // 100% ratio but only 1 rug: below serial_rug_min_count ⇒ not rug.
        let mut f = base();
        f.resolved_launch_count = 2; // meets min_resolved
        f.rugged_launch_count = 1; // ratio 5000 but count 1 < 2
        assert_ne!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn serial_rug_outranks_volume_farmer() {
        let mut f = base();
        f.resolved_launch_count = 3;
        f.rugged_launch_count = 3;
        // Also looks like a volume farmer:
        f.max_launches_in_window = 5;
        f.dump_intensity_bps = 9_000;
        assert_eq!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn volume_farmer_needs_both_window_and_dump() {
        let mut only_window = base();
        only_window.max_launches_in_window = 3;
        only_window.median_survival_secs = 10_000_000; // avoid short-lived
        assert_ne!(classify(only_window), CreatorClass::VolumeFarmer);

        let mut only_dump = base();
        only_dump.dump_intensity_bps = 9_000;
        assert_ne!(classify(only_dump), CreatorClass::VolumeFarmer);
    }

    #[test]
    fn volume_farmer_at_threshold() {
        let mut f = base();
        f.max_launches_in_window = 3; // == threshold
        f.dump_intensity_bps = 4_000; // == threshold
        assert_eq!(classify(f), CreatorClass::VolumeFarmer);
    }

    #[test]
    fn volume_farmer_boundary_just_below() {
        let mut f = base();
        f.max_launches_in_window = 2; // below 3
        f.dump_intensity_bps = 9_000;
        f.median_survival_secs = 10_000_000;
        assert_ne!(classify(f), CreatorClass::VolumeFarmer);
    }

    #[test]
    fn copycat_needs_no_history() {
        let mut f = base();
        f.copycat_similarity_bps = 7_000; // == threshold
        assert_eq!(classify(f), CreatorClass::Copycat);
    }

    #[test]
    fn copycat_boundary_just_below() {
        let mut f = base();
        f.copycat_similarity_bps = 6_999;
        assert_eq!(classify(f), CreatorClass::Unknown);
    }

    #[test]
    fn copycat_outranks_short_lived_and_streamer() {
        let mut f = base();
        f.copycat_similarity_bps = 8_000;
        f.prior_launch_count = 5;
        f.median_survival_secs = 10; // would be short-lived
        f.streamer_launch_ratio_bps = 9_000; // would be streamer
        assert_eq!(classify(f), CreatorClass::Copycat);
    }

    #[test]
    fn short_lived_runner_requires_history() {
        // Short survival but no history: gated to Unknown.
        let mut f = base();
        f.median_survival_secs = 10;
        assert_eq!(classify(f), CreatorClass::Unknown);
    }

    #[test]
    fn short_lived_runner_classified() {
        let mut f = base();
        f.prior_launch_count = 2; // meets history gate
        f.median_survival_secs = 3_599; // < 3600
        assert_eq!(classify(f), CreatorClass::ShortLivedRunner);
    }

    #[test]
    fn short_lived_boundary_at_threshold_is_not_short() {
        let mut f = base();
        f.prior_launch_count = 2;
        f.median_survival_secs = 3_600; // == threshold ⇒ NOT strictly below
        assert_ne!(classify(f), CreatorClass::ShortLivedRunner);
    }

    #[test]
    fn streamer_meta_classified_at_threshold() {
        let mut f = base();
        f.streamer_launch_ratio_bps = 6_000; // == threshold
        f.median_survival_secs = 10_000_000;
        assert_eq!(classify(f), CreatorClass::StreamerMeta);
    }

    #[test]
    fn streamer_boundary_just_below() {
        let mut f = base();
        f.streamer_launch_ratio_bps = 5_999;
        f.median_survival_secs = 10_000_000;
        assert_eq!(classify(f), CreatorClass::Unknown);
    }

    #[test]
    fn community_builder_all_conditions() {
        let mut f = base();
        f.prior_launch_count = 3;
        f.community_retention_bps = 6_000; // == threshold
        f.median_survival_secs = 86_400; // == threshold
        f.dump_intensity_bps = 2_000; // == max threshold
        assert_eq!(classify(f), CreatorClass::CommunityBuilder);
    }

    #[test]
    fn community_builder_fails_if_dump_too_high() {
        let mut f = base();
        f.prior_launch_count = 3;
        f.community_retention_bps = 9_000;
        f.median_survival_secs = 200_000;
        f.dump_intensity_bps = 2_001; // just over max
        assert_ne!(classify(f), CreatorClass::CommunityBuilder);
    }

    #[test]
    fn community_builder_fails_without_history() {
        let mut f = base();
        f.prior_launch_count = 1; // below history gate
        f.community_retention_bps = 9_000;
        f.median_survival_secs = 200_000;
        f.dump_intensity_bps = 0;
        assert_eq!(classify(f), CreatorClass::Unknown);
    }

    #[test]
    fn adversarial_saturated_inputs_do_not_panic() {
        let f = CreatorInputs {
            prior_launch_count: u32::MAX,
            resolved_launch_count: u32::MAX,
            rugged_launch_count: u32::MAX,
            max_launches_in_window: u32::MAX,
            dump_intensity_bps: u32::MAX,
            median_survival_secs: u64::MAX,
            community_retention_bps: u32::MAX,
            streamer_launch_ratio_bps: u32::MAX,
            copycat_similarity_bps: u32::MAX,
        };
        // Should resolve deterministically (SerialRug: max/max ratio == 10000).
        assert_eq!(classify(f), CreatorClass::SerialRug);
    }

    #[test]
    fn ratio_bps_guards_zero_denominator() {
        assert_eq!(ratio_bps_u32(5, 0), 0);
        assert_eq!(ratio_bps_u32(1, 2), 5_000);
        assert_eq!(ratio_bps_u32(u32::MAX, 1), u32::MAX); // saturates, no panic
    }

    #[test]
    fn deterministic_repeat() {
        let f = base();
        assert_eq!(classify(f), classify(f));
    }
}
