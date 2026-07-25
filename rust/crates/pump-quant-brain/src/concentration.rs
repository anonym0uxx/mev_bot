//! The holder-distribution **parallel stream** — a conditioning channel that rides
//! *beside* the fingerprint, never inside it (constitution 6.4, 22, 102).
//!
//! # Why this is not a fingerprint field
//!
//! [`crate::fingerprint`] is a similarity index. Every field in it is a mandatory
//! dimension: it contributes to the Hamming prefilter and to the weighted rank for
//! **every** episode, whether or not that episode's value was ever measured. None
//! of its ladders has an UNKNOWN rung, so a refusal collapses onto the neutral
//! bucket. For a *derivative* of holder count — valid under
//! `pump_quant_app::holder_flow::HolderCountBasis::DeltaOnly`, and therefore
//! measured on nearly every episode — that is a small, bounded cost.
//!
//! Concentration is not that. It is a **level** quantity: every share has the
//! tracked supply in its denominator, and that denominator is the true supply only
//! under `HolderCountBasis::Exact`, which requires a creation sighting before the
//! first swap and is permanently falsified by any pre-window seller. Coverage is
//! consequently thin. Putting a thin-coverage level into the fingerprint would do
//! two bad things at once:
//!
//! 1. park the majority of episodes in a **fabricated** neutral bucket, and
//! 2. make "we could not measure this" **indistinguishable** from "we measured it
//!    and it was low" — the exact §6.4 failure this codebase refuses everywhere
//!    else.
//!
//! So concentration is carried as a parallel channel, in the same shape
//! `MentionProvenance` rides beside the dossier-locked `Mention` type: the primary
//! record stays untouched and the side channel is consulted only where it exists.
//!
//! # The two quantities, and why they are DIFFERENT types
//!
//! * [`ConcentrationReading`] — the **absolute level** (what share of the float the
//!   top ten hold). Requires `Exact`. Its `Unknown` arm carries **no number**, so a
//!   caller cannot reach a share from a basis that does not support one.
//! * [`ConcentrationTrajectory`] — the **direction and rate of change** of the
//!   *tracked cohort's internal* concentration. This is a derivative, and it is
//!   valid under `DeltaOnly`, for a reason that is worth stating precisely: the
//!   denominator of the internal statistic is our OWN tracked supply, which is
//!   fully known to us, and every buy that moves supply into an entity necessarily
//!   enters our ledger (a buyer becomes tracked at the moment they buy). The
//!   unobserved mass — pre-window holders' existing stacks — is an *unchanging*
//!   omission, so it biases the LEVEL without dominating the CHANGE.
//!
//! They are separate types precisely so no consumer can substitute one for the
//! other. A trajectory is not a share and can never be read as one.
//!
//! ## The trajectory's honest limitation (stated, not hidden)
//!
//! The tracked cohort GROWS. A newly arriving entity mechanically dilutes any raw
//! top-N share, so a raw share trajectory on a growing ledger would read
//! "dispersing" on essentially every live market — a measurement of arrival, not of
//! distribution. The trajectory is therefore taken on the **size-normalized**
//! inequality term (Herfindahl rescaled so perfect equality is 0 and total capture
//! is 10 000), which is the standard adjustment for exactly that confound. It does
//! not eliminate it. A skeptic should attack this first.
//!
//! # Purity
//!
//! Integer only (§22), every band edge a named const with its citation (§102), no
//! float, no wall clock, no RNG, no allocation. Every type here is `Copy` and
//! fixed-size so it can ride inside [`crate::episode::EpisodeContext`] without
//! moving the episode off the stack.

/// Basis-point scale (100% == 10 000 bp), shared by every band ladder here (§22).
pub const BPS_SCALE: u32 = 10_000;

// ---------------------------------------------------------------------------
// Band ladders (§102: every boundary is a named const)
// ---------------------------------------------------------------------------

/// Cumulative top-10 share ladder, basis points of tracked supply (§21.7).
///
/// Four bands. The upper two edges are the SAME bars
/// `pump_quant_app::holder_concentration` already uses for its haircut
/// (`TOP10_HAIRCUT_BPS = 5_000`, itself inherited from the §21.5 active-market
/// selector) and its veto leg (`TOP10_VETO_BPS = 7_500`), so the recall conditioner
/// and the sizing law cannot disagree about what "concentrated" means. The app
/// asserts that correspondence at compile time. The lowest edge, 2 500, is the
/// equal-weight reference for a ledger of forty holders — ten of forty equal
/// positions is exactly 25% — so band 0 is "measurably broader than equal weight".
pub const TOP10_BAND_EDGES_BPS: [u32; 3] = [2_500, 5_000, 7_500];

/// Whale-dominance ladder (arXiv 2512.00377 `top-N share x normalized HHI`), bp.
///
/// The upper two edges mirror the app's haircut (2 500) and veto (4 500) bars. The
/// lowest, 1 000, separates "essentially equal weight" (the score is 0 at perfect
/// equality by construction) from "there is a discernible whale".
pub const WHALE_DOMINANCE_BAND_EDGES_BPS: [u32; 3] = [1_000, 2_500, 4_500];

/// First-ten-buyer capture ladder (MemeTrans arXiv 2602.13480
/// `early_top10_hold_pct`), bp.
///
/// The edges are the app's equal-weight reference (5 000), its haircut bar
/// (reference + the published ~17 pp high-risk excess = 6 700) and its veto leg
/// (reference + twice that excess = 8 400).
pub const EARLY_TOP10_BAND_EDGES_BPS: [u32; 3] = [5_000, 6_700, 8_400];

/// Number of bands every ladder above produces: `edges + 1`.
pub const BAND_COUNT: u8 = 4;

/// Compile-time proof that every ladder here is strictly ascending and yields
/// [`BAND_COUNT`] bands (§102 — checked, not remembered).
const _: () = assert!(
    TOP10_BAND_EDGES_BPS[0] < TOP10_BAND_EDGES_BPS[1]
        && TOP10_BAND_EDGES_BPS[1] < TOP10_BAND_EDGES_BPS[2]
        && WHALE_DOMINANCE_BAND_EDGES_BPS[0] < WHALE_DOMINANCE_BAND_EDGES_BPS[1]
        && WHALE_DOMINANCE_BAND_EDGES_BPS[1] < WHALE_DOMINANCE_BAND_EDGES_BPS[2]
        && EARLY_TOP10_BAND_EDGES_BPS[0] < EARLY_TOP10_BAND_EDGES_BPS[1]
        && EARLY_TOP10_BAND_EDGES_BPS[1] < EARLY_TOP10_BAND_EDGES_BPS[2]
        && BAND_COUNT as usize == TOP10_BAND_EDGES_BPS.len() + 1,
    "concentration band ladders must be strictly ascending with BAND_COUNT bands"
);

/// Clamp a restored band ordinal into `0..BAND_COUNT` (§22 explicit narrowing —
/// a corrupt ordinal saturates into the top band, it never wraps).
#[must_use]
pub const fn clamp_band(b: u8) -> u8 {
    if b >= BAND_COUNT {
        BAND_COUNT - 1
    } else {
        b
    }
}

/// Band `x` on a strictly-ascending ladder of inclusive lower bounds.
///
/// Monotone non-decreasing by construction, boundary-inclusive (`x == edge`
/// promotes), and saturating at both ends — the same contract as
/// [`crate::fingerprint::ladder_bucket`], on unsigned band inputs.
#[must_use]
pub fn band_of(x: u32, edges: &[u32]) -> u8 {
    let mut band = 0u8;
    for edge in edges {
        if x >= *edge {
            band += 1;
        } else {
            break;
        }
    }
    band
}

// ---------------------------------------------------------------------------
// The absolute LEVEL reading
// ---------------------------------------------------------------------------

/// Why a concentration LEVEL could not be produced (§6.4 UNKNOWN discipline).
///
/// Every arm is a REASON and none of them carries an estimate. The arms mirror
/// `pump_quant_app::holder_concentration::ConcentrationUnknown` one-for-one; the app
/// owns the ledger and does the mapping, and pins the correspondence with a test.
/// The brain does not depend on the app, so the enum is restated here rather than
/// imported — that is a deliberate layering cost, not an oversight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConcentrationUnknown {
    /// The producing law is not armed, so no reading was taken. A CONFIGURATION
    /// fact, kept distinct from every evidence fact: "we did not look" must be
    /// tellable apart from "we looked and could not tell".
    Disarmed,
    /// The mint has no holder ledger at all.
    Untracked,
    /// Delta-only basis: an unknown number of pre-window holders are missing from
    /// the denominator, so every share would be overstated by an unbounded amount.
    DeltaOnlyBasis,
    /// Incomplete basis: the entity ledger is cap-truncated.
    IncompleteBasis,
    /// Too few tracked entities for a share to have any dynamic range.
    ThinLedger,
    /// Tracked supply is zero — there are no shares to take.
    NoTrackedSupply,
}

impl ConcentrationUnknown {
    /// Dense ordinal for the wire format and the persisted record.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Disarmed => 0,
            Self::Untracked => 1,
            Self::DeltaOnlyBasis => 2,
            Self::IncompleteBasis => 3,
            Self::ThinLedger => 4,
            Self::NoTrackedSupply => 5,
        }
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Disarmed),
            1 => Some(Self::Untracked),
            2 => Some(Self::DeltaOnlyBasis),
            3 => Some(Self::IncompleteBasis),
            4 => Some(Self::ThinLedger),
            5 => Some(Self::NoTrackedSupply),
            _ => None,
        }
    }
}

/// The banded shape of a holder distribution. Reachable ONLY through
/// [`ConcentrationReading::Known`].
///
/// Bands, not raw basis points, for two reasons. First, this rides on every
/// episode and must stay `Copy`-cheap. Second, and more importantly, a recall
/// conditioner keyed on a raw share would match nothing: conditioning is a
/// partition, and a partition on a 10 000-valued axis is a partition into
/// singletons. Four bands is the coarsest partition that still separates the
/// published effect sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConcentrationShape {
    top10_band: u8,
    whale_dominance_band: u8,
    early_top10_band: u8,
}

impl ConcentrationShape {
    /// Band a measured distribution. Every input is basis points of tracked supply
    /// and every one is saturating, so an out-of-range reading lands in the top
    /// band rather than wrapping (§22).
    #[must_use]
    pub fn from_bps(top10_bps: u32, whale_dominance_bps: u32, early_top10_bps: u32) -> Self {
        Self {
            top10_band: band_of(top10_bps, &TOP10_BAND_EDGES_BPS),
            whale_dominance_band: band_of(whale_dominance_bps, &WHALE_DOMINANCE_BAND_EDGES_BPS),
            early_top10_band: band_of(early_top10_bps, &EARLY_TOP10_BAND_EDGES_BPS),
        }
    }

    /// Rebuild from persisted band ordinals, clamped to [`BAND_COUNT`].
    #[must_use]
    pub const fn from_bands(
        top10_band: u8,
        whale_dominance_band: u8,
        early_top10_band: u8,
    ) -> Self {
        Self {
            top10_band: clamp_band(top10_band),
            whale_dominance_band: clamp_band(whale_dominance_band),
            early_top10_band: clamp_band(early_top10_band),
        }
    }

    /// Cumulative top-10 share band — the conditioning axis (see
    /// [`ConcentrationReading::filter_code`]).
    #[must_use]
    pub const fn top10_band(&self) -> u8 {
        self.top10_band
    }

    /// Whale-dominance band (top-N share x normalized internal inequality).
    #[must_use]
    pub const fn whale_dominance_band(&self) -> u8 {
        self.whale_dominance_band
    }

    /// First-ten-buyer capture band.
    #[must_use]
    pub const fn early_top10_band(&self) -> u8 {
        self.early_top10_band
    }
}

/// A concentration LEVEL reading, or a labelled refusal to produce one.
///
/// Mirrors [`crate::recall::RecallVerdict`] deliberately: the `Unknown` arm has no
/// estimate field, so "we could not measure this" is not representable as a number
/// and cannot be accidentally consumed as one. **There is no `unwrap_or`, no
/// `Default`, and no accessor that yields a band from an `Unknown`.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConcentrationReading {
    /// The ledger supported a distribution reading, banded.
    Known(ConcentrationShape),
    /// It did not, and here is why. No estimate exists, by construction.
    Unknown(ConcentrationUnknown),
}

/// Filter-key code for an `Unknown` reading. Zero, so an episode recorded before
/// this channel existed (or by a disarmed producer) reads as Unknown rather than as
/// a fabricated band (§6.4).
///
/// `Unknown` is a **first-class band that episodes carry and the filter never
/// matches on**: no `RecallFilter` constructor can pin this code, and a pinned band
/// excludes episodes carrying it. See `RecallFilter::with_concentration`.
pub const CONCENTRATION_CODE_UNKNOWN: u8 = 0;

/// Number of distinct filter codes: one for `Unknown` plus one per band.
pub const CONCENTRATION_CODE_COUNT: u8 = 1 + BAND_COUNT;

impl ConcentrationReading {
    /// `true` when a reading is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The banded shape, or `None`. This is the **only** way to reach a band.
    #[must_use]
    pub const fn shape(&self) -> Option<&ConcentrationShape> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown(_) => None,
        }
    }

    /// Why the reading was declined, or `None` if it was not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<ConcentrationUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }

    /// The dense code this reading contributes to the packed recall filter key.
    ///
    /// [`CONCENTRATION_CODE_UNKNOWN`] for a refusal; `1 + top10_band` for a
    /// reading. The top-10 share is the single conditioning axis on purpose:
    /// conditioning on all three bands at once would partition a 4x4x4 = 64-way
    /// space and shatter the corpus into singletons, buying precision by destroying
    /// recall. The other two bands ride on the episode for analysis and are not
    /// part of the partition.
    #[must_use]
    pub const fn filter_code(&self) -> u8 {
        match self {
            Self::Known(s) => 1 + s.top10_band,
            Self::Unknown(_) => CONCENTRATION_CODE_UNKNOWN,
        }
    }

    /// The export label for this reading's conditioning band (§46 refusal
    /// discipline on the artifact plane).
    ///
    /// A refusal renders `"unknown"` — the SAME token an out-of-range code would
    /// render, and never a band name. This is the only band-to-string path in the
    /// workspace, so an exporter cannot invent a label for a reading that has none.
    #[must_use]
    pub const fn band_label(&self) -> &'static str {
        concentration_code_label(self.filter_code())
    }
}

/// Render a packed filter code (see [`ConcentrationReading::filter_code`]) as its
/// stable artifact token.
///
/// `0` — and any code outside `1..=BAND_COUNT`, which a corrupt persisted record
/// could supply — renders `"unknown"`. Saturating rather than panicking, and
/// refusing rather than guessing (§6.4/§22).
///
/// The four band names are the ladder in [`TOP10_BAND_EDGES_BPS`] read left to
/// right: below the equal-weight reference for a forty-holder ledger (`broad`),
/// above it (`moderate`), above the §21.5 active-market haircut bar (`concentrated`)
/// and above the veto bar (`extreme`).
#[must_use]
pub const fn concentration_code_label(code: u8) -> &'static str {
    match code {
        1 => "broad",
        2 => "moderate",
        3 => "concentrated",
        4 => "extreme",
        // `CONCENTRATION_CODE_UNKNOWN` and every out-of-range code.
        _ => "unknown",
    }
}

/// Compile-time proof that every reachable band code has a distinct label and that
/// none of them collides with the refusal token (§102 — checked, not remembered).
const _: () = assert!(
    CONCENTRATION_CODE_COUNT == 5
        && concentration_code_label(CONCENTRATION_CODE_UNKNOWN).as_bytes()[0] == b'u'
        && concentration_code_label(1).as_bytes()[0] == b'b'
        && concentration_code_label(2).as_bytes()[0] == b'm'
        && concentration_code_label(3).as_bytes()[0] == b'c'
        && concentration_code_label(4).as_bytes()[0] == b'e'
        && concentration_code_label(BAND_COUNT + 1).as_bytes()[0] == b'u',
    "the concentration label table must cover exactly CONCENTRATION_CODE_COUNT codes \
     with distinct band tokens and an out-of-range refusal"
);

// ---------------------------------------------------------------------------
// The TRAJECTORY reading (a derivative — valid under DeltaOnly)
// ---------------------------------------------------------------------------

/// Why a concentration TRAJECTORY could not be produced (§6.4).
///
/// Deliberately a different enum from [`ConcentrationUnknown`]: the trajectory
/// refuses for different reasons than the level does. Most importantly it does NOT
/// refuse on `DeltaOnlyBasis`, because a derivative of the tracked cohort's own
/// internal distribution is observable there — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrajectoryUnknown {
    /// The producing law is not armed.
    Disarmed,
    /// The mint has no holder ledger at all.
    Untracked,
    /// The ledger is cap-truncated, so even the tracked cohort is not fully
    /// observed and its internal distribution is biased in an unbounded way.
    IncompleteBasis,
    /// Fewer than two samples spanning the minimum interval — there is no change
    /// to measure yet. This is the dominant arm early in a mint's life and it is
    /// an honest "not yet", never a zero.
    InsufficientHistory,
    /// The ledger is too short for an internal distribution statistic to have any
    /// dynamic range.
    ThinLedger,
}

impl TrajectoryUnknown {
    /// Dense ordinal for the wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Disarmed => 0,
            Self::Untracked => 1,
            Self::IncompleteBasis => 2,
            Self::InsufficientHistory => 3,
            Self::ThinLedger => 4,
        }
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Disarmed),
            1 => Some(Self::Untracked),
            2 => Some(Self::IncompleteBasis),
            3 => Some(Self::InsufficientHistory),
            4 => Some(Self::ThinLedger),
            _ => None,
        }
    }
}

/// Which way the tracked cohort's internal concentration is moving. Ordinal —
/// `Dispersing < Flat < Concentrating` is a real axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrajectoryDirection {
    /// Internal inequality is falling: the float is spreading across more, more
    /// evenly-sized tracked positions.
    Dispersing,
    /// Inside the [`TRAJECTORY_FLAT_DEADBAND_BPS`] deadband.
    Flat,
    /// Internal inequality is rising: supply is gathering into fewer hands.
    Concentrating,
}

impl TrajectoryDirection {
    /// Ordinal position on the `Dispersing < Flat < Concentrating` axis.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Dispersing => 0,
            Self::Flat => 1,
            Self::Concentrating => 2,
        }
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Dispersing),
            1 => Some(Self::Flat),
            2 => Some(Self::Concentrating),
            _ => None,
        }
    }
}

/// Deadband (bp of normalized-HHI change per normalization window) inside which a
/// trajectory reads [`TrajectoryDirection::Flat`] (§102).
///
/// One hundred basis points — a 1% move in the normalized inequality term per
/// minute. Below that the reading is dominated by the integer quantization of the
/// share arithmetic and by single-entity arrivals, so calling it a direction would
/// be reading noise. A deadband is what makes `Flat` an actual measurement rather
/// than the vanishing-probability event "exactly zero".
pub const TRAJECTORY_FLAT_DEADBAND_BPS: i64 = 100;

/// Magnitude ladder for the trajectory, absolute bp of normalized-HHI change per
/// normalization window (§102). Four bands, geometric from the deadband.
pub const TRAJECTORY_MAGNITUDE_EDGES_BPS: [i64; 3] = [
    TRAJECTORY_FLAT_DEADBAND_BPS,
    TRAJECTORY_FLAT_DEADBAND_BPS * 5,
    TRAJECTORY_FLAT_DEADBAND_BPS * 25,
];

/// A measured trajectory: which way, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrajectoryShape {
    direction: TrajectoryDirection,
    magnitude_band: u8,
}

impl TrajectoryShape {
    /// Band a measured rate of change of the normalized internal inequality,
    /// signed bp per normalization window.
    ///
    /// The sign selects the direction (with the deadband), the magnitude selects
    /// the band. `i64::MIN` is handled by `unsigned_abs`, so there is no panicking
    /// negation (§22).
    #[must_use]
    pub fn from_rate_bps(rate_bps: i64) -> Self {
        let direction = if rate_bps >= TRAJECTORY_FLAT_DEADBAND_BPS {
            TrajectoryDirection::Concentrating
        } else if rate_bps <= -TRAJECTORY_FLAT_DEADBAND_BPS {
            TrajectoryDirection::Dispersing
        } else {
            TrajectoryDirection::Flat
        };
        let magnitude = i64::try_from(rate_bps.unsigned_abs()).unwrap_or(i64::MAX);
        let mut magnitude_band = 0u8;
        for edge in &TRAJECTORY_MAGNITUDE_EDGES_BPS {
            if magnitude >= *edge {
                magnitude_band += 1;
            } else {
                break;
            }
        }
        Self {
            direction,
            magnitude_band,
        }
    }

    /// Rebuild from persisted ordinals.
    #[must_use]
    pub const fn from_parts(direction: TrajectoryDirection, magnitude_band: u8) -> Self {
        Self {
            direction,
            magnitude_band: clamp_band(magnitude_band),
        }
    }

    /// Which way the tracked cohort's internal concentration is moving.
    #[must_use]
    pub const fn direction(&self) -> TrajectoryDirection {
        self.direction
    }

    /// How fast, banded (`0..BAND_COUNT`).
    #[must_use]
    pub const fn magnitude_band(&self) -> u8 {
        self.magnitude_band
    }
}

/// A concentration TRAJECTORY reading, or a labelled refusal.
///
/// A separate type from [`ConcentrationReading`] so that a trajectory can never be
/// substituted for a level. The trajectory is a derivative of the tracked cohort's
/// OWN internal distribution and is therefore valid on a delta-only ledger; the
/// level is not. Collapsing them into one type would erase exactly that
/// distinction, which is the distinction the whole basis discipline exists to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConcentrationTrajectory {
    /// A change was measurable over the tracked cohort.
    Known(TrajectoryShape),
    /// It was not, and here is why. No rate exists, by construction.
    Unknown(TrajectoryUnknown),
}

impl ConcentrationTrajectory {
    /// `true` when a trajectory is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The measured shape, or `None`. The **only** way to reach a direction.
    #[must_use]
    pub const fn shape(&self) -> Option<&TrajectoryShape> {
        match self {
            Self::Known(s) => Some(s),
            Self::Unknown(_) => None,
        }
    }

    /// Why the trajectory was declined, or `None` if it was not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<TrajectoryUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_ladders_are_monotone_and_boundary_inclusive() {
        for (i, edge) in TOP10_BAND_EDGES_BPS.iter().enumerate() {
            assert_eq!(band_of(*edge, &TOP10_BAND_EDGES_BPS), (i + 1) as u8);
            assert_eq!(band_of(*edge - 1, &TOP10_BAND_EDGES_BPS), i as u8);
        }
        assert_eq!(band_of(0, &TOP10_BAND_EDGES_BPS), 0);
        assert_eq!(band_of(u32::MAX, &TOP10_BAND_EDGES_BPS), BAND_COUNT - 1);
        // Monotone across the whole reachable range.
        let mut prev = 0u8;
        let mut x = 0u32;
        while x <= BPS_SCALE {
            let b = band_of(x, &TOP10_BAND_EDGES_BPS);
            assert!(b >= prev);
            prev = b;
            x += 37; // coprime stride so boundaries get straddled
        }
    }

    #[test]
    fn unknown_yields_no_band_by_any_route() {
        let u = ConcentrationReading::Unknown(ConcentrationUnknown::DeltaOnlyBasis);
        assert!(!u.is_known());
        assert_eq!(u.shape(), None);
        assert_eq!(
            u.unknown_reason(),
            Some(ConcentrationUnknown::DeltaOnlyBasis)
        );
        assert_eq!(u.filter_code(), CONCENTRATION_CODE_UNKNOWN);
        // And the same for the trajectory type.
        let t = ConcentrationTrajectory::Unknown(TrajectoryUnknown::InsufficientHistory);
        assert!(!t.is_known());
        assert_eq!(t.shape(), None);
    }

    /// The artifact vocabulary: four distinct band tokens, and EVERY refusal —
    /// every `Unknown` arm, and every out-of-range code a corrupt record could
    /// carry — renders `"unknown"` and never a band name (§6.4).
    #[test]
    fn band_labels_are_distinct_and_a_refusal_never_names_a_band() {
        let mut seen: Vec<&'static str> = Vec::new();
        for band in 0..BAND_COUNT {
            let r = ConcentrationReading::Known(ConcentrationShape::from_bands(band, 0, 0));
            let label = r.band_label();
            assert_ne!(label, "unknown", "band {band} must not render as a refusal");
            assert!(!seen.contains(&label), "duplicate label {label}");
            seen.push(label);
        }
        assert_eq!(seen, ["broad", "moderate", "concentrated", "extreme"]);

        for u in [
            ConcentrationUnknown::Disarmed,
            ConcentrationUnknown::Untracked,
            ConcentrationUnknown::DeltaOnlyBasis,
            ConcentrationUnknown::IncompleteBasis,
            ConcentrationUnknown::ThinLedger,
            ConcentrationUnknown::NoTrackedSupply,
        ] {
            assert_eq!(ConcentrationReading::Unknown(u).band_label(), "unknown");
        }
        // Every code outside the dense band range refuses too — a corrupt persisted
        // ordinal cannot be promoted into a band by the renderer.
        for code in CONCENTRATION_CODE_COUNT..=u8::MAX {
            assert_eq!(concentration_code_label(code), "unknown", "code {code}");
        }
        assert_eq!(
            concentration_code_label(CONCENTRATION_CODE_UNKNOWN),
            "unknown"
        );
    }

    #[test]
    fn known_filter_codes_are_dense_distinct_and_never_collide_with_unknown() {
        let mut seen = Vec::new();
        for b in 0..BAND_COUNT {
            let r = ConcentrationReading::Known(ConcentrationShape::from_bands(b, 0, 0));
            let c = r.filter_code();
            assert_ne!(
                c, CONCENTRATION_CODE_UNKNOWN,
                "band {b} collided with Unknown"
            );
            assert!(c < CONCENTRATION_CODE_COUNT);
            assert!(!seen.contains(&c), "duplicate code {c}");
            seen.push(c);
        }
        assert_eq!(seen.len(), BAND_COUNT as usize);
    }

    #[test]
    fn shape_bands_track_the_published_bars() {
        // Exactly at the app's haircut bar (5 000) the top-10 band steps to 2.
        let s = ConcentrationShape::from_bps(5_000, 0, 0);
        assert_eq!(s.top10_band(), 2);
        // Just below it, band 1.
        assert_eq!(ConcentrationShape::from_bps(4_999, 0, 0).top10_band(), 1);
        // Equal-weight forty-holder ledger: 25% ⇒ band 1 (at the lowest edge).
        assert_eq!(ConcentrationShape::from_bps(2_500, 0, 0).top10_band(), 1);
        // A whale-free ledger scores 0 dominance ⇒ band 0.
        assert_eq!(
            ConcentrationShape::from_bps(0, 0, 0).whale_dominance_band(),
            0
        );
        // The MemeTrans high-risk early cohort (reference + 17 pp) ⇒ band 2.
        assert_eq!(
            ConcentrationShape::from_bps(0, 0, 6_700).early_top10_band(),
            2
        );
    }

    #[test]
    fn from_bands_clamps_rather_than_wrapping() {
        let s = ConcentrationShape::from_bands(250, 250, 250);
        assert_eq!(s.top10_band(), BAND_COUNT - 1);
        assert_eq!(s.whale_dominance_band(), BAND_COUNT - 1);
        assert_eq!(s.early_top10_band(), BAND_COUNT - 1);
    }

    #[test]
    fn trajectory_direction_respects_the_deadband_and_is_signed() {
        let d = |r: i64| TrajectoryShape::from_rate_bps(r).direction();
        assert_eq!(d(0), TrajectoryDirection::Flat);
        assert_eq!(
            d(TRAJECTORY_FLAT_DEADBAND_BPS - 1),
            TrajectoryDirection::Flat
        );
        assert_eq!(
            d(-TRAJECTORY_FLAT_DEADBAND_BPS + 1),
            TrajectoryDirection::Flat
        );
        assert_eq!(
            d(TRAJECTORY_FLAT_DEADBAND_BPS),
            TrajectoryDirection::Concentrating
        );
        assert_eq!(
            d(-TRAJECTORY_FLAT_DEADBAND_BPS),
            TrajectoryDirection::Dispersing
        );
        // Magnitude is symmetric in the sign — direction carries the sign, band
        // carries the size, and neither leaks into the other.
        assert_eq!(
            TrajectoryShape::from_rate_bps(3_000).magnitude_band(),
            TrajectoryShape::from_rate_bps(-3_000).magnitude_band()
        );
        // No panic at the extremes (§22: `unsigned_abs`, not negation).
        assert_eq!(
            TrajectoryShape::from_rate_bps(i64::MIN).direction(),
            TrajectoryDirection::Dispersing
        );
        assert_eq!(
            TrajectoryShape::from_rate_bps(i64::MIN).magnitude_band(),
            BAND_COUNT - 1
        );
    }

    #[test]
    fn every_enum_ordinal_round_trips() {
        for o in 0u8..6 {
            let r = ConcentrationUnknown::from_ordinal(o).expect("in range");
            assert_eq!(r.ordinal(), o);
        }
        assert!(ConcentrationUnknown::from_ordinal(6).is_none());
        for o in 0u8..5 {
            let r = TrajectoryUnknown::from_ordinal(o).expect("in range");
            assert_eq!(r.ordinal(), o);
        }
        assert!(TrajectoryUnknown::from_ordinal(5).is_none());
        for o in 0u8..3 {
            let d = TrajectoryDirection::from_ordinal(o).expect("in range");
            assert_eq!(d.ordinal(), o);
        }
        assert!(TrajectoryDirection::from_ordinal(3).is_none());
    }

    /// The load-bearing separation: a trajectory and a level are different types
    /// and a `DeltaOnly` ledger legitimately produces the former while refusing the
    /// latter. This test states that as code.
    #[test]
    fn a_delta_only_ledger_can_carry_a_trajectory_but_not_a_level() {
        let level = ConcentrationReading::Unknown(ConcentrationUnknown::DeltaOnlyBasis);
        let traj = ConcentrationTrajectory::Known(TrajectoryShape::from_rate_bps(900));
        assert!(!level.is_known(), "no level from a delta-only basis");
        assert!(traj.is_known(), "but the internal change is observable");
        assert_eq!(
            traj.shape().map(TrajectoryShape::direction),
            Some(TrajectoryDirection::Concentrating)
        );
    }
}
