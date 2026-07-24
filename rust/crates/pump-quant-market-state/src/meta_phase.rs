//! §21.4 **meta lifecycle phase**, including the decay detection the rotation
//! vocabulary was missing.
//!
//! ## Why this exists
//! [`crate::meta::rotation_between`] can say a category is *emerging* (gaining
//! launch share with rising flow) or *saturating* (holds share, flow no longer
//! keeping pace). It cannot say a category is **decaying** — participation,
//! attention and realized outcomes all rolling over from a prior peak — because
//! a two-snapshot diff has no notion of a peak to fall from. That absence is
//! expensive in exactly one direction: entering a meta that has already rolled
//! over is one of the most reliable ways to lose money in memecoin rotation, and
//! a system that cannot *name* that state cannot avoid it, cannot condition size
//! on it, and cannot recall having been burned by it before.
//!
//! This module adds the missing axis: a bounded per-category series of
//! participation / attention / realized-outcome samples, and a peak-relative
//! classifier over a bounded lookback window.
//!
//! ## The estimator
//! Over the newest `window` samples with `slot <= as_of_slot`:
//!
//! * for participation and attention, the **peak** is the maximum in the window
//!   taken at its *latest* occurrence (so a plateau sitting at its maximum is
//!   not "falling"), and the decline is
//!   `(peak - latest) * 10_000 / peak` in bps;
//! * for realized outcome — which is signed and may be negative, making a
//!   ratio-to-peak meaningless — the drop is the **absolute** bps difference
//!   `peak - latest`;
//! * a measure counts as falling when its peak strictly precedes the latest
//!   sample *and* its decline clears the named threshold;
//! * `min_falling_measures` of the three falling ⇒ [`MetaPhase::Decaying`].
//!
//! When nothing is falling, the remaining phases are named from breadth and the
//! attention trend across the window: broad + flat ⇒ `Saturated`, broad + rising
//! ⇒ `Hot`, narrow + rising ⇒ `Emerging`.
//!
//! ## §6.4 fail-closed — two distinct unknowns
//! [`MetaPhaseTracker::estimate_as_of`] returns `None` when there is no
//! estimate at all: an untracked category, or fewer than
//! [`MetaPhaseThresholds::min_samples`] samples at or before the cutoff. When an
//! estimate does exist, [`MetaPhaseEstimate::phase`] is itself an `Option`, and
//! it is `None` for a state the measures genuinely do not name — a narrow, flat,
//! not-yet-falling meta. The phase axis is *ordinal*: "quiet" is not the same
//! reading as "emerging", and defaulting one to the other would fabricate a
//! position on the lifecycle. Neither unknown is ever collapsed to a neutral
//! phase.
//!
//! ## §20 point-in-time safety
//! Sample selection is strictly `slot <= as_of_slot`, newest-first over a
//! monotonic series. A sample recorded later can never change an earlier
//! estimate.
//!
//! ## §22/§99
//! Pure integer bps math, no float, no clock, no RNG. Bounded on both axes:
//! [`MetaPhaseThresholds::max_categories`] categories, each holding at most
//! [`META_PHASE_SERIES_CAP`] samples in a fixed ring with oldest-first eviction.

use crate::common::{BoundedMap, Completeness, EntityId};

/// Ring capacity of one category's phase series (§99/§57 memory law).
///
/// Thirty-two samples covers four full default lookback windows, so a window
/// query is always answerable from retained state while the per-category
/// footprint stays fixed.
pub const META_PHASE_SERIES_CAP: usize = 32;

/// Default lookback, in samples, for the peak-relative classifier (§21.4).
///
/// Eight. Long enough for a peak and a roll-over to both sit inside the window;
/// short enough that a peak from an entirely previous cycle of the meta does not
/// keep the category pinned to `Decaying` forever.
pub const META_PHASE_WINDOW_SAMPLES: usize = 8;

/// Default minimum samples before any phase may be named (§6.4).
///
/// Three: a peak, a decline from it, and one more point so the decline is a
/// trajectory rather than a single tick.
pub const META_PHASE_MIN_SAMPLES: usize = 3;

/// Default decline from the in-window peak, in bps, for participation or
/// attention to count as falling (§21.4). 2 500 bps = 25% off the peak.
pub const META_PHASE_MIN_DECLINE_BPS: u64 = 2_500;

/// Default absolute drop, in bps, for realized outcome to count as falling.
///
/// 1 000 bps = ten percentage points of realized return. Realized outcome is
/// signed, so this is an absolute difference from the in-window peak, never a
/// ratio (a ratio to a negative or zero peak has no meaning).
pub const META_PHASE_MIN_OUTCOME_DROP_BPS: u64 = 1_000;

/// Default number of the three measures that must be falling for
/// [`MetaPhase::Decaying`] (§21.4). Two of three: one rolling over is noise,
/// two agreeing is the meta.
pub const META_PHASE_MIN_FALLING_MEASURES: u8 = 2;

/// Default participation at/above which a meta counts as *broad* (§21.4) — the
/// boundary between `Emerging` (few participants) and `Hot`/`Saturated` (broad
/// participation).
pub const META_PHASE_BROAD_PARTICIPATION: u64 = 25;

/// Default half-width, in bps, of the "attention is flat" band (§21.4).
/// Within ±500 bps across the window, attention is neither rising nor falling —
/// broad participation with flat attention is the definition of `Saturated`.
pub const META_PHASE_ATTENTION_FLAT_BAND_BPS: i64 = 500;

/// Default category capacity of the tracker (§99/§57 memory law).
pub const META_PHASE_TRACKER_CAP: usize = 256;

/// Basis-point denominator (§22).
const BPS_DENOM: i128 = 10_000;

/// Where a meta sits in its own lifecycle (§21.4).
///
/// Ordinal — this is a time axis, and "how far through the meta are we" is
/// exactly the question. The discriminants are the dense ordinals the downstream
/// episodic-recall fingerprint uses, so the mapping across the crate boundary is
/// the identity on [`Self::ordinal`] and cannot silently drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum MetaPhase {
    /// Few participants, rising attention.
    Emerging = 0,
    /// Broad participation, attention still rising.
    Hot = 1,
    /// Broad participation, attention flat — new entrants are exit liquidity.
    Saturated = 2,
    /// Participation, attention and/or realized outcomes falling from a peak.
    Decaying = 3,
}

impl MetaPhase {
    /// Ordinal position in the meta lifecycle.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Emerging),
            1 => Some(Self::Hot),
            2 => Some(Self::Saturated),
            3 => Some(Self::Decaying),
            _ => None,
        }
    }
}

/// One observation of a meta's health at a known slot (§20).
///
/// All three measures are caller-supplied point-in-time facts; nothing here
/// derives them. `participation` and `attention` are non-negative magnitudes
/// (distinct participating wallets/creators, and an attention level in whatever
/// integer unit the attention plane emits); `realized_outcome_bps` is signed —
/// the realized return of the category's tokens over the sampling interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MetaSample {
    /// Slot at which the measures were observed.
    pub slot: u64,
    /// Participation breadth (distinct participants).
    pub participation: u64,
    /// Attention level.
    pub attention: u64,
    /// Realized outcome over the sampling interval, signed bps.
    pub realized_outcome_bps: i64,
}

/// Named, versioned gates for the phase classifier (§102 — every comparison in
/// [`classify_phase`] reads a field here, never a literal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaPhaseThresholds {
    /// Lookback, in samples, for peak detection and trend measurement.
    pub window: usize,
    /// Minimum samples at or before the cutoff before any phase is named.
    pub min_samples: usize,
    /// Decline from the in-window peak, bps, for participation/attention to
    /// count as falling.
    pub min_decline_bps: u64,
    /// Absolute drop from the in-window peak, bps, for realized outcome to
    /// count as falling.
    pub min_outcome_drop_bps: u64,
    /// How many of the three measures must fall for `Decaying`.
    pub min_falling_measures: u8,
    /// Participation at/above which the meta counts as broad.
    pub broad_participation: u64,
    /// Half-width of the flat-attention band, bps.
    pub attention_flat_band_bps: i64,
    /// Maximum tracked categories (§99).
    pub max_categories: usize,
}

impl MetaPhaseThresholds {
    /// The named-const default configuration.
    pub const DEFAULT: Self = MetaPhaseThresholds {
        window: META_PHASE_WINDOW_SAMPLES,
        min_samples: META_PHASE_MIN_SAMPLES,
        min_decline_bps: META_PHASE_MIN_DECLINE_BPS,
        min_outcome_drop_bps: META_PHASE_MIN_OUTCOME_DROP_BPS,
        min_falling_measures: META_PHASE_MIN_FALLING_MEASURES,
        broad_participation: META_PHASE_BROAD_PARTICIPATION,
        attention_flat_band_bps: META_PHASE_ATTENTION_FLAT_BAND_BPS,
        max_categories: META_PHASE_TRACKER_CAP,
    };

    /// Whether the configuration can yield a meaningful phase. A window or
    /// sample floor below the three points a peak-and-decline needs, a
    /// zero `min_falling_measures` (which would call every meta decaying), or a
    /// negative flat band make every estimate refuse (§6.4).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.min_samples >= META_PHASE_MIN_SAMPLES
            && self.window >= self.min_samples
            && self.min_falling_measures > 0
            && self.min_falling_measures <= 3
            && self.attention_flat_band_bps >= 0
            && self.max_categories > 0
    }
}

impl Default for MetaPhaseThresholds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Peak-relative measures behind a phase verdict, kept inspectable rather than
/// collapsed into the label (criterion 47 multi-dimensional inspectability).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaPhaseEstimate {
    /// The named lifecycle phase, or `None` when the measures name no phase
    /// (§6.4 — a quiet, flat, not-yet-falling meta is not "emerging").
    pub phase: Option<MetaPhase>,
    /// Samples inside the window that were at or before the cutoff.
    pub samples_used: u32,
    /// Slot of the newest sample used (always `<= as_of_slot`).
    pub latest_slot: u64,
    /// Participation decline from its in-window peak, bps of that peak.
    pub participation_decline_bps: u64,
    /// Attention decline from its in-window peak, bps of that peak.
    pub attention_decline_bps: u64,
    /// Realized-outcome drop from its in-window peak, absolute bps.
    pub outcome_drop_bps: u64,
    /// How many of the three measures are falling from a strictly prior peak.
    pub falling_measures: u8,
    /// Attention change across the window, first→last, signed bps of the first
    /// value. Saturates to [`i64::MAX`] when attention rose from zero.
    pub attention_change_bps: i64,
    /// Latest participation level (the breadth the phase cascade reads).
    pub latest_participation: u64,
    /// Slot of the in-window participation peak.
    pub peak_participation_slot: u64,
    /// Slot of the in-window attention peak.
    pub peak_attention_slot: u64,
    /// Whether the series has lost samples to the capacity bound; when
    /// `Incomplete`, the in-window peak is a lower bound on the true peak.
    pub completeness: Completeness,
}

/// Result of recording one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaSampleWrite {
    /// The sample was recorded.
    Recorded,
    /// Refused: the slot precedes the newest recorded slot for that category
    /// (§20 — information time never moves backwards).
    NonMonotonic,
    /// Refused: the category capacity is exhausted (§99). Nothing was evicted,
    /// so already-tracked categories keep full fidelity.
    AtCapacity,
}

/// Relative change from `first` to `last` in signed bps of `first`.
///
/// When `first == 0` a ratio is undefined: a rise from nothing saturates to
/// [`i64::MAX`] and a flat zero is `0`. Documented saturation, never a float.
fn change_bps(first: u64, last: u64) -> i64 {
    if first == 0 {
        return if last > 0 { i64::MAX } else { 0 };
    }
    let delta = i128::from(last) - i128::from(first);
    let r = delta.saturating_mul(BPS_DENOM) / i128::from(first);
    if r > i128::from(i64::MAX) {
        i64::MAX
    } else if r < i128::from(i64::MIN) {
        i64::MIN
    } else {
        r as i64
    }
}

/// Decline from `peak` to `latest` in bps of `peak`. `0` when `peak == 0` or
/// `latest >= peak`.
fn decline_bps(peak: u64, latest: u64) -> u64 {
    if peak == 0 || latest >= peak {
        return 0;
    }
    let d = u128::from(peak) - u128::from(latest);
    let r = d.saturating_mul(10_000) / u128::from(peak);
    u64::try_from(r).unwrap_or(u64::MAX)
}

/// Absolute drop from `peak` to `latest` in bps. `0` when `latest >= peak`.
fn drop_abs_bps(peak: i64, latest: i64) -> u64 {
    if latest >= peak {
        return 0;
    }
    let d = i128::from(peak) - i128::from(latest);
    u64::try_from(d).unwrap_or(u64::MAX)
}

/// Classify a bounded, ascending-by-slot window of samples into a phase
/// (§21.4).
///
/// `window` must already be the point-in-time selection (every sample at or
/// before the decision slot) in ascending slot order; the tracker guarantees
/// this. `completeness` reports whether the underlying series had evicted
/// samples, which makes the in-window peak a lower bound.
///
/// Returns `None` — no estimate at all — when the configuration is invalid or
/// the window holds fewer than `th.min_samples` samples (§6.4). The returned
/// estimate's `phase` is itself `None` when the measures name no phase.
///
/// Pure, total, integer, panic-free on any input.
#[must_use]
pub fn classify_phase(
    window: &[MetaSample],
    th: &MetaPhaseThresholds,
    completeness: Completeness,
) -> Option<MetaPhaseEstimate> {
    if !th.is_valid() || window.len() < th.min_samples {
        return None;
    }
    let first = window.first()?;
    let latest = window.last()?;

    // Peaks at their LATEST occurrence: a plateau sitting at its maximum is not
    // falling, which keeps `Decaying` a claim about a roll-over, not a tie.
    let mut peak_part = first.participation;
    let mut peak_part_idx = 0usize;
    let mut peak_part_slot = first.slot;
    let mut peak_att = first.attention;
    let mut peak_att_idx = 0usize;
    let mut peak_att_slot = first.slot;
    let mut peak_out = first.realized_outcome_bps;
    let mut peak_out_idx = 0usize;
    for (i, s) in window.iter().enumerate() {
        if s.participation >= peak_part {
            peak_part = s.participation;
            peak_part_idx = i;
            peak_part_slot = s.slot;
        }
        if s.attention >= peak_att {
            peak_att = s.attention;
            peak_att_idx = i;
            peak_att_slot = s.slot;
        }
        if s.realized_outcome_bps >= peak_out {
            peak_out = s.realized_outcome_bps;
            peak_out_idx = i;
        }
    }
    let last_idx = window.len() - 1;

    let participation_decline_bps = decline_bps(peak_part, latest.participation);
    let attention_decline_bps = decline_bps(peak_att, latest.attention);
    let outcome_drop_bps = drop_abs_bps(peak_out, latest.realized_outcome_bps);

    let part_falling = peak_part_idx < last_idx && participation_decline_bps >= th.min_decline_bps;
    let att_falling = peak_att_idx < last_idx && attention_decline_bps >= th.min_decline_bps;
    let out_falling = peak_out_idx < last_idx && outcome_drop_bps >= th.min_outcome_drop_bps;

    let falling_measures = u8::from(part_falling) + u8::from(att_falling) + u8::from(out_falling);

    let attention_change_bps = change_bps(first.attention, latest.attention);
    let broad = latest.participation >= th.broad_participation;
    let rising = attention_change_bps > th.attention_flat_band_bps;
    let flat = attention_change_bps.saturating_abs() <= th.attention_flat_band_bps;

    // Cascade, first match wins. Decay dominates: a meta that has rolled over is
    // decaying even if its absolute participation is still broad.
    let phase = if falling_measures >= th.min_falling_measures {
        Some(MetaPhase::Decaying)
    } else if broad && flat {
        Some(MetaPhase::Saturated)
    } else if broad && rising {
        Some(MetaPhase::Hot)
    } else if !broad && rising {
        Some(MetaPhase::Emerging)
    } else {
        // Narrow and not rising, or falling but under the decay gates: the
        // measures name no lifecycle position. Refuse rather than default.
        None
    };

    Some(MetaPhaseEstimate {
        phase,
        samples_used: u32::try_from(window.len()).unwrap_or(u32::MAX),
        latest_slot: latest.slot,
        participation_decline_bps,
        attention_decline_bps,
        outcome_drop_bps,
        falling_measures,
        attention_change_bps,
        latest_participation: latest.participation,
        peak_participation_slot: peak_part_slot,
        peak_attention_slot: peak_att_slot,
        completeness,
    })
}

/// One category's bounded, monotonically-slotted sample series.
#[derive(Clone, Debug)]
struct PhaseSeries {
    /// Ascending by slot; the oldest is evicted on overflow.
    samples: Vec<MetaSample>,
    last_slot: u64,
    dropped: u64,
}

impl PhaseSeries {
    fn new() -> Self {
        PhaseSeries {
            samples: Vec::new(),
            last_slot: 0,
            dropped: 0,
        }
    }

    fn completeness(&self) -> Completeness {
        if self.dropped > 0 {
            Completeness::Incomplete
        } else {
            Completeness::Complete
        }
    }
}

/// A bounded, per-category meta-phase tracker (§21.4, §99).
///
/// Categories are held in a [`BoundedMap`], so a new category beyond capacity is
/// refused (never silently displacing a tracked one) and the map reports
/// [`Completeness::Incomplete`]. Each category's series is a fixed ring of
/// [`META_PHASE_SERIES_CAP`] samples with oldest-first eviction; an eviction
/// makes the series' completeness `Incomplete`, which propagates onto every
/// estimate so a consumer knows the in-window peak is a lower bound.
#[derive(Clone, Debug)]
pub struct MetaPhaseTracker {
    series: BoundedMap<PhaseSeries>,
    th: MetaPhaseThresholds,
}

impl MetaPhaseTracker {
    /// Create an empty tracker with the given thresholds.
    #[must_use]
    pub fn new(th: MetaPhaseThresholds) -> Self {
        MetaPhaseTracker {
            series: BoundedMap::with_capacity(th.max_categories.max(1)),
            th,
        }
    }

    /// Create an empty tracker with [`MetaPhaseThresholds::DEFAULT`].
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(MetaPhaseThresholds::DEFAULT)
    }

    /// The thresholds in force.
    #[must_use]
    pub const fn thresholds(&self) -> &MetaPhaseThresholds {
        &self.th
    }

    /// Number of tracked categories.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.series.len()
    }

    /// Whether no category is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }

    /// Whether the category capacity was exceeded (§6.4/§99).
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        self.series.completeness()
    }

    /// Samples evicted from `category`'s series by the ring bound.
    #[must_use]
    pub fn dropped_samples(&self, category: EntityId) -> u64 {
        self.series.get(category).map_or(0, |s| s.dropped)
    }

    /// Read-only view of a category's retained samples, ascending by slot.
    #[must_use]
    pub fn samples(&self, category: EntityId) -> Option<&[MetaSample]> {
        self.series.get(category).map(|s| s.samples.as_slice())
    }

    /// Record one observation for `category`.
    ///
    /// Refuses a sample whose slot precedes the newest recorded slot for that
    /// category (§20). Equal slots are accepted — two measures can share a slot
    /// — and are simply the newer observation of that instant.
    pub fn record(&mut self, category: EntityId, sample: MetaSample) -> MetaSampleWrite {
        let Some(series) = self.series.get_or_insert_with(category, PhaseSeries::new) else {
            return MetaSampleWrite::AtCapacity;
        };
        if !series.samples.is_empty() && sample.slot < series.last_slot {
            return MetaSampleWrite::NonMonotonic;
        }
        series.samples.push(sample);
        if series.samples.len() > META_PHASE_SERIES_CAP {
            series.samples.remove(0);
            series.dropped = series.dropped.saturating_add(1);
        }
        series.last_slot = sample.slot;
        MetaSampleWrite::Recorded
    }

    /// Point-in-time phase estimate for `category` as known at `as_of_slot`
    /// (§20).
    ///
    /// Selects the newest [`MetaPhaseThresholds::window`] samples with
    /// `slot <= as_of_slot` and classifies them. `None` when the category is
    /// untracked or fewer than `min_samples` are knowable at the cutoff — never
    /// a fabricated neutral phase (§6.4).
    #[must_use]
    pub fn estimate_as_of(&self, category: EntityId, as_of_slot: u64) -> Option<MetaPhaseEstimate> {
        let series = self.series.get(category)?;
        // Samples are ascending by slot; `partition_point` finds the first index
        // strictly after the cutoff without reading a single later sample's
        // measures into the estimate.
        let end = series.samples.partition_point(|s| s.slot <= as_of_slot);
        let start = end.saturating_sub(self.th.window);
        let window = series.samples.get(start..end)?;
        classify_phase(window, &self.th, series.completeness())
    }

    /// Convenience: just the named phase, or `None` when there is no estimate
    /// or the measures name no phase.
    #[must_use]
    pub fn phase_as_of(&self, category: EntityId, as_of_slot: u64) -> Option<MetaPhase> {
        self.estimate_as_of(category, as_of_slot)?.phase
    }
}

impl Default for MetaPhaseTracker {
    fn default() -> Self {
        Self::with_defaults()
    }
}
