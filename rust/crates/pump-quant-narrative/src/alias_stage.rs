//! Alias stage: novelty vs saturation, computed from the mint stream.
//!
//! This is the profit-relevant coordinate and it needs neither the model nor the
//! social plane. Measured walk-forward on the Slinky corpus (active stratum,
//! `trade_count >= 50`, n = 124,711): names attaching to a currently-hot alias
//! graduate **2.00%** (P(peak > 200 SOL) 3.88%) while names attaching to no hot
//! alias graduate **2.89%** (P(peak > 200 SOL) 4.98%), degrading monotonically
//! with more hot terms. So "attached to a hot term" IS the saturation
//! coordinate: hot means the crowd is already there.
//!
//! Everything here is integer arithmetic over caller-supplied observations. The
//! crate holds no growing state, so the per-alias ring buffer lives in the
//! daemon and arrives as [`AliasObservation`]. No floats, no clock, no I/O.

/// How early in an alias's life a token is attaching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasStage {
    /// The alias is new to OUR stream (or younger than the novelty window).
    Novel,
    /// Attached, spreading, not yet crowded.
    Rising,
    /// Attached and decelerating — the wave is cresting.
    Cresting,
    /// Crowded. Thousands of copycats have already launched.
    Saturated,
}

/// Per-alias observations supplied by the daemon at the decision instant.
///
/// Every count is a count of OTHER mints seen before this one, so the structure
/// is causal by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AliasObservation {
    /// When our pipeline first observed this alias, if ever. `None` means the
    /// alias is new to us at this instant.
    pub alias_first_seen_ms: Option<u64>,
    /// Distinct prior mints sharing this alias in the trailing 60s.
    pub prior_count_60s: u32,
    /// ... trailing 5 minutes.
    pub prior_count_5m: u32,
    /// ... trailing 1 hour.
    pub prior_count_1h: u32,
    /// ... trailing 24 hours.
    pub prior_count_24h: u32,
    /// Prior count in the trailing 24h divided by the prior-week daily rate,
    /// scaled by 100. 100 = flat, 300 = 3x the baseline rate. Saturating.
    pub accel_x100: u32,
    /// Distinct deliberate misspellings of the alias seen in the trailing hour.
    /// Copycats misspell precisely to dodge filters, so a variant wave is a
    /// late-stage tell.
    pub variant_count_1h: u32,
}

/// Stage thresholds. Named constants rather than `Config` fields: the sealed
/// journal digest seeds on `format!("{cfg:?}")`, so a new field would move the
/// digest on every record with no behaviour change behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageThresholds {
    /// Trailing-24h shared-alias count at or above which the alias is crowded.
    /// Mirrors the walk-forward definition that measured the saturated cohort.
    pub saturated_prior_24h_min: u32,
    /// Acceleration (x100) at or above which crowding counts as saturation.
    pub saturated_accel_x100: u32,
    /// Trailing-1h shared-alias count at or above which the wave is cresting.
    pub cresting_prior_1h_min: u32,
    /// Misspelling variants in the trailing hour at or above which the meta is
    /// treated as saturated regardless of counts.
    pub variant_saturated_min: u32,
    /// Alias age (seconds) at or below which an attachment is still novel.
    pub novel_age_s_max: u64,
}

/// Thresholds v1.
pub const STAGE_THRESHOLDS_V1: StageThresholds = StageThresholds {
    saturated_prior_24h_min: 5,
    saturated_accel_x100: 300,
    cresting_prior_1h_min: 3,
    variant_saturated_min: 3,
    novel_age_s_max: 900,
};

/// Classify an alias attachment into a stage.
///
/// Order is the cascade: a misspelling wave outranks counts (it is the later
/// signal), crowding outranks age, and novelty is the residual.
#[must_use]
pub fn nv_alias_stage(now_ms: u64, obs: &AliasObservation, t: &StageThresholds) -> AliasStage {
    if obs.variant_count_1h >= t.variant_saturated_min {
        return AliasStage::Saturated;
    }
    if obs.prior_count_24h >= t.saturated_prior_24h_min && obs.accel_x100 >= t.saturated_accel_x100
    {
        return AliasStage::Saturated;
    }
    if obs.prior_count_1h >= t.cresting_prior_1h_min {
        return AliasStage::Cresting;
    }
    let age_s = match obs.alias_first_seen_ms {
        None => 0,
        Some(first) => now_ms.saturating_sub(first) / 1_000,
    };
    if age_s <= t.novel_age_s_max {
        return AliasStage::Novel;
    }
    AliasStage::Rising
}

impl AliasStage {
    /// Whether the stage is admissible as POSITIVE evidence of a rising
    /// narrative (Lane L of the entry precondition).
    #[must_use]
    pub const fn is_positive_attachment(self) -> bool {
        matches!(self, AliasStage::Novel | AliasStage::Rising)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> AliasObservation {
        AliasObservation::default()
    }

    #[test]
    fn alias_new_to_us_is_novel() {
        let mut o = obs();
        o.alias_first_seen_ms = None;
        assert_eq!(
            nv_alias_stage(1_000_000, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Novel
        );
    }

    #[test]
    fn young_alias_without_crowding_is_novel_then_rising() {
        let mut o = obs();
        o.alias_first_seen_ms = Some(1_000_000);
        assert_eq!(
            nv_alias_stage(1_000_000 + 60_000, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Novel
        );
        // Same alias, long-lived, still uncrowded -> Rising.
        assert_eq!(
            nv_alias_stage(1_000_000 + 4_000_000, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Rising
        );
    }

    #[test]
    fn crowding_is_saturation_and_outranks_age() {
        let mut o = obs();
        o.alias_first_seen_ms = Some(1_000_000); // very young
        o.prior_count_24h = 5;
        o.accel_x100 = 300;
        assert_eq!(
            nv_alias_stage(1_000_000 + 1_000, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Saturated
        );
        // High counts WITHOUT acceleration is cresting, not saturated.
        let mut o2 = obs();
        o2.prior_count_24h = 40;
        o2.accel_x100 = 100;
        o2.prior_count_1h = 3;
        assert_eq!(
            nv_alias_stage(9_000_000, &o2, &STAGE_THRESHOLDS_V1),
            AliasStage::Cresting
        );
    }

    #[test]
    fn misspelling_wave_is_late_stage_even_when_counts_are_low() {
        let mut o = obs();
        o.alias_first_seen_ms = None;
        o.variant_count_1h = 3;
        assert_eq!(
            nv_alias_stage(5_000, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Saturated
        );
    }

    #[test]
    fn arithmetic_is_saturating_not_wrapping() {
        let mut o = obs();
        o.alias_first_seen_ms = Some(u64::MAX);
        // now_ms before first_seen must not wrap into a huge age.
        assert_eq!(
            nv_alias_stage(0, &o, &STAGE_THRESHOLDS_V1),
            AliasStage::Novel
        );
    }

    #[test]
    fn only_novel_and_rising_are_positive() {
        assert!(AliasStage::Novel.is_positive_attachment());
        assert!(AliasStage::Rising.is_positive_attachment());
        assert!(!AliasStage::Cresting.is_positive_attachment());
        assert!(!AliasStage::Saturated.is_positive_attachment());
    }
}
