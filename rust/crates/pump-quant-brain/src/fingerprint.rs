//! `SetupFingerprint` — quantize a decision-time market/setup state into a compact
//! integer signature (constitution 22 integer-only, 102 named thresholds).
//!
//! # What this module is
//!
//! At the instant the engine is deciding whether to take a trade it already holds
//! ~20 integer descriptors of the situation: order-flow imbalance, CVD, trend and
//! range structure, burst phase, realized vol, liquidity, buyer breadth, token age,
//! venue phase, attention velocity, narrative class, authenticity, holder-growth
//! acceleration, creator class, meta category and saturation, whether a designated
//! caller is present, the round-trip cost, and the information-time-of-day.
//!
//! This module folds those into two artefacts:
//!
//! * [`SetupFingerprint::signature`] — a packed [`u128`] used as a **Hamming
//!   prefilter**. One contiguous 16-byte load per candidate, one `xor`, one
//!   `count_ones`. This is the microsecond path.
//! * [`SetupFingerprint::buckets`] — the `[u8; FIELD_COUNT]` ordinal vector used
//!   for the **precise weighted-L1 rank** of the small prefiltered candidate set.
//!
//! # The encoding, and why it is what it is
//!
//! A naive binary packing of ordinals makes Hamming distance meaningless: buckets
//! `3 (0b011)` and `4 (0b100)` are adjacent in the market but three bits apart in
//! the code. So each field is encoded by *kind*:
//!
//! * [`FieldKind::Ordinal`] fields use **thermometer (unary) encoding**: bucket `k`
//!   sets the low `k` bits of the field's `levels - 1` bit window. The Hamming
//!   distance between two thermometer codes is therefore **exactly** `|k_a - k_b|`
//!   — the prefilter measures true ordinal distance, not an accident of binary
//!   representation.
//! * [`FieldKind::Nominal`] fields (venue phase, narrative class, creator class,
//!   meta category slot, designated-caller flag) use **one-hot encoding**: equal
//!   contributes `0`, different contributes exactly [`NOMINAL_MISMATCH_COST`] `= 2`
//!   bits. Nominal fields have no meaningful ordering, so an ordinal distance
//!   between them would be a lie.
//!
//! The consequence is the invariant this module's tests pin down:
//! `signature_hamming(a, b) == unweighted_distance(a, b)` for every pair. The
//! fast prefilter and the exact unweighted metric are *the same number*, so the
//! prefilter is not a heuristic approximation of the rank — it is the rank with
//! uniform weights. [`FeatureWeights`] then re-weights the ordinal vector in
//! stage 2 to emphasise structure and flow over, say, time-of-day.
//!
//! Total packed width is [`SIGNATURE_BITS`] bits, which fits a single `u128` with
//! headroom; the unused high bits are always zero in both operands so they can
//! never contribute to a distance.
//!
//! # Monotonicity contract
//!
//! Every scalar field maps through [`ladder_bucket`] over a strictly-ascending,
//! named-const edge array. That makes bucketing **monotone non-decreasing** in the
//! raw input: `x <= y` implies `bucket(x) <= bucket(y)`. Edges are *inclusive lower
//! bounds of the upper bucket* (`x >= edge` promotes), and that boundary rule is
//! tested. Monotonicity is what lets a human reason about the fingerprint at all:
//! "more buying pressure" can only ever move the OFI bucket up.
//!
//! Nothing here reads a wall clock. The time-of-day bucket folds a caller-supplied
//! *information-time* nanosecond stamp, which is exactly what replay feeds it.

use crate::hash::mix_u32;

/// Number of fields in a fingerprint (constitution 102).
pub const FIELD_COUNT: usize = 20;

/// Hamming cost charged when two [`FieldKind::Nominal`] fields differ: one-hot
/// codes differ in exactly two bit positions (constitution 102).
pub const NOMINAL_MISMATCH_COST: u32 = 2;

/// Basis-point scale: `10_000 bp == 100%` (constitution 22 fixed-point ratios).
pub const BPS_SCALE: i64 = 10_000;

/// Nanoseconds in one 24-hour day (constitution 102). Used only for *information
/// time* modulo arithmetic — never a wall-clock read.
pub const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

/// Number of time-of-day buckets (constitution 102): eight three-hour blocks.
pub const TIME_OF_DAY_BUCKETS: u64 = 8;

/// Number of nominal slots the `meta_category_id` space is mixed down into
/// (constitution 102). Distinct metas can share a slot in the *prefilter*; exact
/// meta identity is preserved in [`crate::episode::EpisodeContext`] and enforced by
/// [`crate::recall::RecallFilter`], so a slot collision can only ever widen the
/// candidate set, never corrupt a conditioned estimate.
pub const META_CATEGORY_SLOTS: u32 = 16;

// ---------------------------------------------------------------------------
// Field enums
// ---------------------------------------------------------------------------

/// Swing-structure classification of recent price action (constitution 21.6).
/// Treated as *ordinal* — `Down < Range < Up` is a real axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrendStructure {
    /// Lower highs and lower lows.
    Down,
    /// No dominant swing direction.
    Range,
    /// Higher highs and higher lows.
    Up,
}

impl TrendStructure {
    /// Ordinal position in the `Down < Range < Up` axis.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Down => 0,
            Self::Range => 1,
            Self::Up => 2,
        }
    }
}

/// Range compression / expansion state (constitution 21.6). Ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RangeState {
    /// Volatility squeeze; range is contracting.
    Compressed,
    /// Neither squeezing nor expanding.
    Normal,
    /// Range is expanding.
    Expanded,
}

impl RangeState {
    /// Ordinal position in the `Compressed < Normal < Expanded` axis.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Compressed => 0,
            Self::Normal => 1,
            Self::Expanded => 2,
        }
    }
}

/// Where the current volume burst sits in its lifecycle (constitution 21.7).
/// Ordinal — this is a time-ordered lifecycle, not a set of labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BurstPhase {
    /// No burst in progress.
    None,
    /// Arrival intensity just stepped up.
    Onset,
    /// Peak arrival intensity.
    Climax,
    /// Intensity decaying off the peak.
    Exhaustion,
}

impl BurstPhase {
    /// Ordinal position in the burst lifecycle.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Onset => 1,
            Self::Climax => 2,
            Self::Exhaustion => 3,
        }
    }
}

/// Which venue the token is trading on (constitution 100).
///
/// **Nominal, and deliberately so.** The bonding curve and the migrated pool have
/// different fee, slippage and adversary structure; there is no "half way between"
/// them. §100 forbids pooling their outcomes into one estimate, and
/// [`crate::recall::RecallFilter`] makes that structurally impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VenuePhase {
    /// Pre-migration bonding curve.
    Curve,
    /// Post-migration constant-product pool.
    Pool,
}

impl VenuePhase {
    /// Dense ordinal used for one-hot encoding and filter-key packing.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Curve => 0,
            Self::Pool => 1,
        }
    }

    /// Inverse of [`VenuePhase::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Curve),
            1 => Some(Self::Pool),
            _ => None,
        }
    }
}

/// Coarse narrative family a token belongs to (constitution 21.4). Nominal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NarrativeClass {
    /// No identifiable narrative.
    Unclassified,
    /// Animal / mascot memes.
    Animal,
    /// Political / current-events memes.
    Political,
    /// Celebrity or influencer tie-in.
    Celebrity,
    /// Technology or AI themed.
    Tech,
    /// Derivative of an already-running meta.
    Derivative,
    /// Live-stream or streamer driven.
    Stream,
    /// Recurring seasonal or calendar meme.
    Seasonal,
}

impl NarrativeClass {
    /// Dense ordinal used for one-hot encoding.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Unclassified => 0,
            Self::Animal => 1,
            Self::Political => 2,
            Self::Celebrity => 3,
            Self::Tech => 4,
            Self::Derivative => 5,
            Self::Stream => 6,
            Self::Seasonal => 7,
        }
    }
}

/// Prior-behaviour classification of the token's creator (constitution 29.9).
/// Nominal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CreatorClass {
    /// Creator has no recorded history.
    Unknown,
    /// Creator has shipped tokens that survived migration.
    Proven,
    /// Creator has a recorded rug or instant-dump history.
    Toxic,
    /// Creator mints at high frequency (serial launcher).
    Serial,
}

impl CreatorClass {
    /// Dense ordinal used for one-hot encoding.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Proven => 1,
            Self::Toxic => 2,
            Self::Serial => 3,
        }
    }
}

/// Where a meta sits in its own lifecycle (constitution 21.4). Ordinal — this is a
/// time axis, and "how far through the meta are we" is exactly the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetaSaturationState {
    /// Few participants, rising attention.
    Emerging,
    /// Broad participation, attention still rising.
    Hot,
    /// Broad participation, attention flat — new entrants are exit liquidity.
    Saturated,
    /// Participation and attention both falling.
    Decaying,
}

impl MetaSaturationState {
    /// Ordinal position in the meta lifecycle.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Emerging => 0,
            Self::Hot => 1,
            Self::Saturated => 2,
            Self::Decaying => 3,
        }
    }

    /// Inverse of [`MetaSaturationState::ordinal`]; out-of-range input yields `None`.
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

// ---------------------------------------------------------------------------
// Bucket ladders (constitution 102: every boundary is a named const)
// ---------------------------------------------------------------------------

/// Order-flow-imbalance ladder, basis points of signed flow (constitution 21.7).
/// Seven buckets: heavy sell / sell / mild sell / balanced / mild buy / buy / heavy buy.
pub const OFI_EDGES_BPS: [i64; 6] = [-2_000, -500, -100, 100, 500, 2_000];

/// Signed-decade ladder for cumulative volume delta, where the decade is of
/// *lamports* (constitution 22). `+9` is one SOL of net buying; `+11` is ~100 SOL.
pub const CVD_DECADE_EDGES: [i64; 6] = [-11, -9, -7, 7, 9, 11];

/// Realized-volatility ladder, basis points per bar (constitution 21.6).
pub const REALIZED_VOL_EDGES_BPS: [i64; 5] = [50, 150, 400, 1_000, 2_500];

/// Pool-liquidity ladder as a decade of lamports (constitution 22).
/// `9` is ~1 SOL, `12` is ~1_000 SOL.
pub const LIQUIDITY_DECADE_EDGES: [i64; 6] = [8, 9, 10, 11, 12, 13];

/// Distinct-buyer-count ladder over the feature window (constitution 21.7).
pub const BUYER_BREADTH_EDGES: [i64; 4] = [3, 8, 20, 50];

/// Token-age ladder in nanoseconds of information time (constitution 20):
/// 60 s, 5 min, 30 min, 6 h, 24 h.
pub const TOKEN_AGE_EDGES_NS: [i64; 5] = [
    60 * 1_000_000_000,
    300 * 1_000_000_000,
    1_800 * 1_000_000_000,
    21_600 * 1_000_000_000,
    86_400 * 1_000_000_000,
];

/// Attention-velocity ladder, basis points of window-over-window growth
/// (constitution 21.4). The first edge is `0`, so bucket `0` is *decaying* attention.
pub const ATTENTION_VELOCITY_EDGES_BPS: [i64; 5] = [0, 500, 2_000, 7_500, 20_000];

/// Social-authenticity ladder, basis points (constitution 21.4). `10_000 bp` is
/// fully organic; low values indicate bot amplification.
pub const AUTHENTICITY_EDGES_BPS: [i64; 4] = [2_500, 5_000, 7_500, 9_000];

/// Holder-growth *acceleration* ladder, signed basis points (constitution 21.4).
pub const HOLDER_GROWTH_ACCEL_EDGES_BPS: [i64; 4] = [-500, 0, 500, 2_000];

/// Expected round-trip cost ladder in basis points — fee plus spread plus expected
/// slippage, both ways (constitution 24). This is the hurdle every edge must clear.
pub const ROUND_TRIP_COST_EDGES_BPS: [i64; 5] = [50, 100, 200, 400, 800];

/// Bucket `x` on a strictly-ascending ladder of inclusive lower bounds.
///
/// Returns the number of edges that `x` has reached: `0` when `x` is below every
/// edge, `edges.len()` when `x` is at or above all of them. Therefore the result is
/// in `0..=edges.len()` and the ladder defines `edges.len() + 1` buckets.
///
/// **Monotone non-decreasing** in `x` by construction, and **boundary-inclusive**:
/// `x == edges[i]` promotes into the bucket *above* `edges[i]`.
///
/// The caller is responsible for supplying strictly ascending edges; every ladder
/// const in this module is checked for that by test.
#[must_use]
pub fn ladder_bucket(x: i64, edges: &[i64]) -> u8 {
    let mut bucket = 0u8;
    for edge in edges {
        if x >= *edge {
            bucket += 1;
        } else {
            break;
        }
    }
    bucket
}

/// Signed decade of an integer magnitude: `sign(x) * floor(log10(|x|))`, with
/// `signed_decade(0) == 0`.
///
/// This is the standard scale-reducer for money-like quantities that span many
/// orders of magnitude (constitution 22): it is exact integer arithmetic, it is
/// monotone non-decreasing in `x`, and it compresses a lamport range of `1e0..1e18`
/// into a small ordinal the fingerprint can afford to carry. Provided as a public
/// helper so callers produce `cvd_decade` / `liquidity_decade` the one canonical way
/// rather than each inventing their own.
#[must_use]
pub fn signed_decade(x: i128) -> i32 {
    let mag = x.unsigned_abs();
    if mag == 0 {
        return 0;
    }
    let mut decade = 0i32;
    let mut probe = 10u128;
    while probe <= mag && decade < 38 {
        decade += 1;
        // `probe` cannot overflow before `decade` hits the 38 guard: 10^38 < u128::MAX.
        probe *= 10;
    }
    if x < 0 {
        -decade
    } else {
        decade
    }
}

/// Fold an *information-time* nanosecond stamp into one of
/// [`TIME_OF_DAY_BUCKETS`] equal blocks of the UTC day.
///
/// Pure modulo arithmetic on the caller's timestamp — this is never a wall-clock
/// read, which is what makes replay reproduce the identical bucket.
///
/// Note the axis is *cyclic* while the thermometer encoding is *linear*: bucket 0
/// and bucket 7 are adjacent in the world but 7 apart in the code. That is a
/// deliberate accepted distortion, bounded by giving this field the smallest
/// default weight in [`FeatureWeights`] ([`W_TIME_OF_DAY`]). Session identity is a
/// weak prior; structure and flow are the signal.
#[must_use]
pub fn time_of_day_bucket(info_time_ns: u64) -> u8 {
    let within_day = info_time_ns % NS_PER_DAY;
    let block = NS_PER_DAY / TIME_OF_DAY_BUCKETS;
    // `within_day < NS_PER_DAY` so the quotient is `< TIME_OF_DAY_BUCKETS`.
    (within_day / block) as u8
}

// ---------------------------------------------------------------------------
// Field table
// ---------------------------------------------------------------------------

/// Whether a field's buckets carry an ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Buckets are ordered; encoded as a thermometer code so Hamming distance
    /// equals `|delta bucket|`.
    Ordinal,
    /// Buckets are unordered labels; encoded one-hot so any mismatch costs exactly
    /// [`NOMINAL_MISMATCH_COST`].
    Nominal,
}

/// Static description of one fingerprint field: its name, kind, cardinality, and
/// its window inside the packed [`u128`] signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// Stable field name, used in diagnostics and in the persisted schema audit.
    pub name: &'static str,
    /// Ordinal or nominal (drives both the encoding and the distance cost).
    pub kind: FieldKind,
    /// Number of distinct buckets; valid bucket values are `0..levels`.
    pub levels: u8,
    /// Offset of this field's window within the packed signature, in bits.
    pub bit_offset: u8,
    /// Width of this field's window, in bits: `levels - 1` for
    /// [`FieldKind::Ordinal`], `levels` for [`FieldKind::Nominal`].
    pub bit_width: u8,
}

/// Field index: order-flow imbalance.
pub const F_OFI: usize = 0;
/// Field index: signed decade of cumulative volume delta.
pub const F_CVD_DECADE: usize = 1;
/// Field index: swing trend structure.
pub const F_TREND_STRUCTURE: usize = 2;
/// Field index: range compression/expansion state.
pub const F_RANGE_STATE: usize = 3;
/// Field index: volume-burst lifecycle phase.
pub const F_BURST_PHASE: usize = 4;
/// Field index: realized volatility.
pub const F_REALIZED_VOL: usize = 5;
/// Field index: signed decade of pool liquidity.
pub const F_LIQUIDITY_DECADE: usize = 6;
/// Field index: distinct-buyer breadth.
pub const F_BUYER_BREADTH: usize = 7;
/// Field index: token age in information time.
pub const F_TOKEN_AGE: usize = 8;
/// Field index: venue phase (curve vs pool).
pub const F_VENUE_PHASE: usize = 9;
/// Field index: attention velocity.
pub const F_ATTENTION_VELOCITY: usize = 10;
/// Field index: narrative class.
pub const F_NARRATIVE_CLASS: usize = 11;
/// Field index: social authenticity.
pub const F_AUTHENTICITY: usize = 12;
/// Field index: holder-growth acceleration.
pub const F_HOLDER_GROWTH_ACCEL: usize = 13;
/// Field index: creator class.
pub const F_CREATOR_CLASS: usize = 14;
/// Field index: meta-category slot.
pub const F_META_CATEGORY: usize = 15;
/// Field index: meta saturation state.
pub const F_META_SATURATION: usize = 16;
/// Field index: designated-caller-present flag.
pub const F_DESIGNATED_CALLER: usize = 17;
/// Field index: expected round-trip cost.
pub const F_ROUND_TRIP_COST: usize = 18;
/// Field index: information-time-of-day block.
pub const F_TIME_OF_DAY: usize = 19;

/// The authoritative field table. `bit_offset` values are the running sum of
/// `bit_width`; `tests::field_table_layout_is_consistent` proves it, so the table
/// can be read as data rather than trusted as prose.
pub const FIELD_SPECS: [FieldSpec; FIELD_COUNT] = [
    FieldSpec {
        name: "ofi",
        kind: FieldKind::Ordinal,
        levels: 7,
        bit_offset: 0,
        bit_width: 6,
    },
    FieldSpec {
        name: "cvd_decade",
        kind: FieldKind::Ordinal,
        levels: 7,
        bit_offset: 6,
        bit_width: 6,
    },
    FieldSpec {
        name: "trend_structure",
        kind: FieldKind::Ordinal,
        levels: 3,
        bit_offset: 12,
        bit_width: 2,
    },
    FieldSpec {
        name: "range_state",
        kind: FieldKind::Ordinal,
        levels: 3,
        bit_offset: 14,
        bit_width: 2,
    },
    FieldSpec {
        name: "burst_phase",
        kind: FieldKind::Ordinal,
        levels: 4,
        bit_offset: 16,
        bit_width: 3,
    },
    FieldSpec {
        name: "realized_vol",
        kind: FieldKind::Ordinal,
        levels: 6,
        bit_offset: 19,
        bit_width: 5,
    },
    FieldSpec {
        name: "liquidity_decade",
        kind: FieldKind::Ordinal,
        levels: 7,
        bit_offset: 24,
        bit_width: 6,
    },
    FieldSpec {
        name: "buyer_breadth",
        kind: FieldKind::Ordinal,
        levels: 5,
        bit_offset: 30,
        bit_width: 4,
    },
    FieldSpec {
        name: "token_age",
        kind: FieldKind::Ordinal,
        levels: 6,
        bit_offset: 34,
        bit_width: 5,
    },
    FieldSpec {
        name: "venue_phase",
        kind: FieldKind::Nominal,
        levels: 2,
        bit_offset: 39,
        bit_width: 2,
    },
    FieldSpec {
        name: "attention_velocity",
        kind: FieldKind::Ordinal,
        levels: 6,
        bit_offset: 41,
        bit_width: 5,
    },
    FieldSpec {
        name: "narrative_class",
        kind: FieldKind::Nominal,
        levels: 8,
        bit_offset: 46,
        bit_width: 8,
    },
    FieldSpec {
        name: "authenticity",
        kind: FieldKind::Ordinal,
        levels: 5,
        bit_offset: 54,
        bit_width: 4,
    },
    FieldSpec {
        name: "holder_growth_accel",
        kind: FieldKind::Ordinal,
        levels: 5,
        bit_offset: 58,
        bit_width: 4,
    },
    FieldSpec {
        name: "creator_class",
        kind: FieldKind::Nominal,
        levels: 4,
        bit_offset: 62,
        bit_width: 4,
    },
    FieldSpec {
        name: "meta_category",
        kind: FieldKind::Nominal,
        levels: 16,
        bit_offset: 66,
        bit_width: 16,
    },
    FieldSpec {
        name: "meta_saturation",
        kind: FieldKind::Ordinal,
        levels: 4,
        bit_offset: 82,
        bit_width: 3,
    },
    FieldSpec {
        name: "designated_caller",
        kind: FieldKind::Nominal,
        levels: 2,
        bit_offset: 85,
        bit_width: 2,
    },
    FieldSpec {
        name: "round_trip_cost",
        kind: FieldKind::Ordinal,
        levels: 6,
        bit_offset: 87,
        bit_width: 5,
    },
    FieldSpec {
        name: "time_of_day",
        kind: FieldKind::Ordinal,
        levels: 8,
        bit_offset: 92,
        bit_width: 7,
    },
];

/// Total number of packed signature bits actually used (constitution 102).
/// The remaining `128 - SIGNATURE_BITS` bits are always zero in every fingerprint,
/// so they can never contribute to a Hamming distance.
pub const SIGNATURE_BITS: u32 = 99;

/// Encode one field's bucket into its window of the packed signature.
///
/// `bucket` is clamped to `levels - 1`; a caller cannot corrupt a neighbouring
/// field's window by passing an out-of-range bucket.
#[must_use]
pub const fn encode_field(spec: &FieldSpec, bucket: u8) -> u128 {
    let max = spec.levels - 1;
    let b = if bucket > max { max } else { bucket };
    let payload = match spec.kind {
        // Thermometer: bucket k sets the low k bits.
        FieldKind::Ordinal => (1u128 << b) - 1,
        // One-hot: bucket k sets exactly bit k.
        FieldKind::Nominal => 1u128 << b,
    };
    payload << spec.bit_offset
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// The raw, unquantized decision-time state the engine hands to the brain.
///
/// Every member is an integer or a small enum (constitution 22). Deliberately this
/// struct takes **raw** quantities — nanoseconds of age, basis points of OFI — and
/// not pre-computed buckets: bucketing lives here, behind named-const ladders, so
/// two call sites can never disagree about where a boundary is. That is the whole
/// point of a fingerprint being comparable across time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupInputs {
    /// Order-flow imbalance over the feature window, signed basis points.
    pub ofi_bps: i64,
    /// Signed decade of cumulative volume delta in lamports (see [`signed_decade`]).
    pub cvd_decade: i32,
    /// Swing trend structure.
    pub trend_structure: TrendStructure,
    /// Range compression / expansion state.
    pub range_state: RangeState,
    /// Volume-burst lifecycle phase.
    pub burst_phase: BurstPhase,
    /// Realized volatility over the feature window, basis points per bar.
    pub realized_vol_bps: i64,
    /// Signed decade of quote-side pool liquidity in lamports.
    pub liquidity_decade: i32,
    /// Distinct buyers observed in the feature window.
    pub buyer_breadth: u32,
    /// Token age in nanoseconds of *information time*.
    pub token_age_ns: u64,
    /// Bonding curve or migrated pool (constitution 100).
    pub venue_phase: VenuePhase,
    /// Attention growth window-over-window, signed basis points.
    pub attention_velocity_bps: i64,
    /// Coarse narrative family.
    pub narrative_class: NarrativeClass,
    /// Organic-versus-amplified score, basis points (`10_000` = fully organic).
    pub authenticity_bps: i64,
    /// Second derivative of holder count, signed basis points.
    pub holder_growth_accel_bps: i64,
    /// Creator prior-behaviour class.
    pub creator_class: CreatorClass,
    /// Exact meta-category identifier (mixed down to a slot for the signature; the
    /// exact id is what conditioned recall filters on).
    pub meta_category_id: u32,
    /// Where the meta sits in its lifecycle.
    pub meta_saturation_state: MetaSaturationState,
    /// Whether a designated (tracked, scored) caller has called this mint.
    pub designated_caller_present: bool,
    /// Expected round-trip cost in basis points: fees + spread + slippage, both ways.
    pub round_trip_cost_bps: i64,
    /// Decision-time *information-time* stamp in nanoseconds, folded into a
    /// time-of-day block. Never a wall-clock read.
    pub info_time_ns: u64,
}

impl Default for SetupInputs {
    /// A neutral, mid-ladder setup: balanced flow, ranging structure, no burst, no
    /// narrative, unknown creator, curve phase. Useful as a test baseline that can
    /// be perturbed one field at a time.
    fn default() -> Self {
        Self {
            ofi_bps: 0,
            cvd_decade: 0,
            trend_structure: TrendStructure::Range,
            range_state: RangeState::Normal,
            burst_phase: BurstPhase::None,
            realized_vol_bps: 0,
            liquidity_decade: 0,
            buyer_breadth: 0,
            token_age_ns: 0,
            venue_phase: VenuePhase::Curve,
            attention_velocity_bps: 0,
            narrative_class: NarrativeClass::Unclassified,
            authenticity_bps: 0,
            holder_growth_accel_bps: 0,
            creator_class: CreatorClass::Unknown,
            meta_category_id: 0,
            meta_saturation_state: MetaSaturationState::Emerging,
            designated_caller_present: false,
            round_trip_cost_bps: 0,
            info_time_ns: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

/// Default weight, order-flow imbalance (constitution 102). Flow is the signal.
pub const W_OFI: u32 = 6;
/// Default weight, CVD decade (constitution 102).
pub const W_CVD_DECADE: u32 = 5;
/// Default weight, trend structure (constitution 102). Structure is the signal.
pub const W_TREND_STRUCTURE: u32 = 8;
/// Default weight, range state (constitution 102).
pub const W_RANGE_STATE: u32 = 6;
/// Default weight, burst phase (constitution 102).
pub const W_BURST_PHASE: u32 = 7;
/// Default weight, realized volatility (constitution 102).
pub const W_REALIZED_VOL: u32 = 4;
/// Default weight, liquidity decade (constitution 102).
pub const W_LIQUIDITY_DECADE: u32 = 5;
/// Default weight, buyer breadth (constitution 102).
pub const W_BUYER_BREADTH: u32 = 4;
/// Default weight, token age (constitution 102).
pub const W_TOKEN_AGE: u32 = 3;
/// Default weight, venue phase (constitution 100/102). Highest weight in the table:
/// even though [`crate::recall::RecallFilter`] already hard-partitions on phase,
/// weighting it heavily means an accidentally unfiltered comparison still cannot
/// rank a cross-phase episode as "near".
pub const W_VENUE_PHASE: u32 = 10;
/// Default weight, attention velocity (constitution 102).
pub const W_ATTENTION_VELOCITY: u32 = 4;
/// Default weight, narrative class (constitution 102).
pub const W_NARRATIVE_CLASS: u32 = 3;
/// Default weight, authenticity (constitution 102).
pub const W_AUTHENTICITY: u32 = 3;
/// Default weight, holder-growth acceleration (constitution 102).
pub const W_HOLDER_GROWTH_ACCEL: u32 = 3;
/// Default weight, creator class (constitution 102).
pub const W_CREATOR_CLASS: u32 = 2;
/// Default weight, meta category (constitution 102).
pub const W_META_CATEGORY: u32 = 6;
/// Default weight, meta saturation (constitution 102).
pub const W_META_SATURATION: u32 = 5;
/// Default weight, designated caller present (constitution 102).
pub const W_DESIGNATED_CALLER: u32 = 2;
/// Default weight, round-trip cost (constitution 102). Cost is not a nuisance
/// variable — a setup that only worked at 50 bp of friction is not the same setup
/// at 400 bp.
pub const W_ROUND_TRIP_COST: u32 = 5;
/// Default weight, time of day (constitution 102). Lowest in the table: session
/// identity is a weak prior and the encoding of a cyclic axis is lossy.
pub const W_TIME_OF_DAY: u32 = 1;

/// Per-field integer weights for the stage-2 weighted-L1 rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureWeights {
    /// Weight per field, indexed by the `F_*` field-index constants.
    pub w: [u32; FIELD_COUNT],
}

impl Default for FeatureWeights {
    fn default() -> Self {
        let mut w = [0u32; FIELD_COUNT];
        w[F_OFI] = W_OFI;
        w[F_CVD_DECADE] = W_CVD_DECADE;
        w[F_TREND_STRUCTURE] = W_TREND_STRUCTURE;
        w[F_RANGE_STATE] = W_RANGE_STATE;
        w[F_BURST_PHASE] = W_BURST_PHASE;
        w[F_REALIZED_VOL] = W_REALIZED_VOL;
        w[F_LIQUIDITY_DECADE] = W_LIQUIDITY_DECADE;
        w[F_BUYER_BREADTH] = W_BUYER_BREADTH;
        w[F_TOKEN_AGE] = W_TOKEN_AGE;
        w[F_VENUE_PHASE] = W_VENUE_PHASE;
        w[F_ATTENTION_VELOCITY] = W_ATTENTION_VELOCITY;
        w[F_NARRATIVE_CLASS] = W_NARRATIVE_CLASS;
        w[F_AUTHENTICITY] = W_AUTHENTICITY;
        w[F_HOLDER_GROWTH_ACCEL] = W_HOLDER_GROWTH_ACCEL;
        w[F_CREATOR_CLASS] = W_CREATOR_CLASS;
        w[F_META_CATEGORY] = W_META_CATEGORY;
        w[F_META_SATURATION] = W_META_SATURATION;
        w[F_DESIGNATED_CALLER] = W_DESIGNATED_CALLER;
        w[F_ROUND_TRIP_COST] = W_ROUND_TRIP_COST;
        w[F_TIME_OF_DAY] = W_TIME_OF_DAY;
        Self { w }
    }
}

impl FeatureWeights {
    /// Uniform weights of `1`. With these, [`weighted_distance`] is exactly
    /// [`unweighted_distance`], which is exactly the signature Hamming distance —
    /// the identity that ties stage 1 to stage 2.
    #[must_use]
    pub const fn uniform() -> Self {
        Self {
            w: [1u32; FIELD_COUNT],
        }
    }

    /// Sum of all weights; used to reason about the maximum attainable distance.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.w.iter().map(|x| u64::from(*x)).sum()
    }
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

/// The quantized signature of a setup: a packed [`u128`] for the fast prefilter and
/// the ordinal bucket vector for the precise rank.
///
/// Construct with [`SetupFingerprint::from_inputs`]. The two representations are
/// always consistent because they are produced together and the struct is immutable
/// from the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupFingerprint {
    signature: u128,
    buckets: [u8; FIELD_COUNT],
}

impl SetupFingerprint {
    /// Quantize raw decision-time state into a fingerprint.
    ///
    /// Pure and total: every input value maps somewhere (ladders saturate at both
    /// ends), so there is no error path and no way to produce a partial fingerprint.
    #[must_use]
    pub fn from_inputs(inputs: &SetupInputs) -> Self {
        let mut buckets = [0u8; FIELD_COUNT];

        buckets[F_OFI] = ladder_bucket(inputs.ofi_bps, &OFI_EDGES_BPS);
        buckets[F_CVD_DECADE] = ladder_bucket(i64::from(inputs.cvd_decade), &CVD_DECADE_EDGES);
        buckets[F_TREND_STRUCTURE] = inputs.trend_structure.ordinal();
        buckets[F_RANGE_STATE] = inputs.range_state.ordinal();
        buckets[F_BURST_PHASE] = inputs.burst_phase.ordinal();
        buckets[F_REALIZED_VOL] = ladder_bucket(inputs.realized_vol_bps, &REALIZED_VOL_EDGES_BPS);
        buckets[F_LIQUIDITY_DECADE] =
            ladder_bucket(i64::from(inputs.liquidity_decade), &LIQUIDITY_DECADE_EDGES);
        buckets[F_BUYER_BREADTH] =
            ladder_bucket(i64::from(inputs.buyer_breadth), &BUYER_BREADTH_EDGES);
        buckets[F_TOKEN_AGE] =
            ladder_bucket(clamp_u64_to_i64(inputs.token_age_ns), &TOKEN_AGE_EDGES_NS);
        buckets[F_VENUE_PHASE] = inputs.venue_phase.ordinal();
        buckets[F_ATTENTION_VELOCITY] =
            ladder_bucket(inputs.attention_velocity_bps, &ATTENTION_VELOCITY_EDGES_BPS);
        buckets[F_NARRATIVE_CLASS] = inputs.narrative_class.ordinal();
        buckets[F_AUTHENTICITY] = ladder_bucket(inputs.authenticity_bps, &AUTHENTICITY_EDGES_BPS);
        buckets[F_HOLDER_GROWTH_ACCEL] = ladder_bucket(
            inputs.holder_growth_accel_bps,
            &HOLDER_GROWTH_ACCEL_EDGES_BPS,
        );
        buckets[F_CREATOR_CLASS] = inputs.creator_class.ordinal();
        buckets[F_META_CATEGORY] = meta_category_slot(inputs.meta_category_id);
        buckets[F_META_SATURATION] = inputs.meta_saturation_state.ordinal();
        buckets[F_DESIGNATED_CALLER] = u8::from(inputs.designated_caller_present);
        buckets[F_ROUND_TRIP_COST] =
            ladder_bucket(inputs.round_trip_cost_bps, &ROUND_TRIP_COST_EDGES_BPS);
        buckets[F_TIME_OF_DAY] = time_of_day_bucket(inputs.info_time_ns);

        Self {
            signature: pack_signature(&buckets),
            buckets,
        }
    }

    /// Rebuild a fingerprint from a persisted bucket vector.
    ///
    /// The signature is *recomputed* rather than trusted, so a corrupted or
    /// stale-schema signature on disk can never enter the index (constitution 22
    /// replay-safety: the encoding is derived, never restored).
    #[must_use]
    pub fn from_buckets(buckets: [u8; FIELD_COUNT]) -> Self {
        let mut clamped = buckets;
        for (b, spec) in clamped.iter_mut().zip(FIELD_SPECS.iter()) {
            let max = spec.levels - 1;
            if *b > max {
                *b = max;
            }
        }
        Self {
            signature: pack_signature(&clamped),
            buckets: clamped,
        }
    }

    /// The packed signature used by the stage-1 Hamming prefilter.
    #[must_use]
    pub const fn signature(&self) -> u128 {
        self.signature
    }

    /// The ordinal bucket vector used by the stage-2 weighted-L1 rank.
    #[must_use]
    pub const fn buckets(&self) -> &[u8; FIELD_COUNT] {
        &self.buckets
    }

    /// Bucket of a single field, by `F_*` index. Returns `None` if out of range.
    #[must_use]
    pub fn bucket(&self, field: usize) -> Option<u8> {
        self.buckets.get(field).copied()
    }

    /// The venue phase this fingerprint was taken in (constitution 100). Recall
    /// uses this to partition the index; it is never a free parameter.
    #[must_use]
    pub fn venue_phase(&self) -> VenuePhase {
        VenuePhase::from_ordinal(self.buckets[F_VENUE_PHASE]).unwrap_or(VenuePhase::Curve)
    }
}

/// Clamp a `u64` into `i64` range for ladder comparison. Ages beyond ~292 years of
/// nanoseconds saturate into the top bucket, which is the correct behaviour.
const fn clamp_u64_to_i64(x: u64) -> i64 {
    if x > i64::MAX as u64 {
        i64::MAX
    } else {
        x as i64
    }
}

/// Mix an exact `meta_category_id` down to its nominal signature slot.
#[must_use]
pub fn meta_category_slot(meta_category_id: u32) -> u8 {
    (mix_u32(meta_category_id) % META_CATEGORY_SLOTS) as u8
}

/// Pack an ordinal bucket vector into the signature word.
#[must_use]
pub fn pack_signature(buckets: &[u8; FIELD_COUNT]) -> u128 {
    let mut sig = 0u128;
    for (b, spec) in buckets.iter().zip(FIELD_SPECS.iter()) {
        sig |= encode_field(spec, *b);
    }
    sig
}

/// Stage-1 distance: population count of the XOR of two packed signatures.
///
/// One `xor` and one `count_ones` (a single `popcnt` instruction on any deploy CPU
/// worth using). By the encoding contract this equals [`unweighted_distance`].
#[must_use]
pub const fn signature_hamming(a: u128, b: u128) -> u32 {
    (a ^ b).count_ones()
}

/// Unweighted ordinal distance between two fingerprints: `|delta|` per ordinal
/// field, [`NOMINAL_MISMATCH_COST`] per differing nominal field.
///
/// Provably identical to [`signature_hamming`] of the two signatures — see the
/// module docs and `tests::hamming_equals_unweighted_l1`.
#[must_use]
pub fn unweighted_distance(a: &SetupFingerprint, b: &SetupFingerprint) -> u32 {
    let mut acc = 0u32;
    for (i, spec) in FIELD_SPECS.iter().enumerate() {
        acc += field_cost(spec, a.buckets[i], b.buckets[i]);
    }
    acc
}

/// Weighted stage-2 distance: `sum_i weight_i * cost_i`.
///
/// Accumulated in `u64` and saturating (constitution 22 explicit overflow
/// strategy). The true maximum is far below `u64::MAX` — max field cost is 15,
/// times 20 fields, times a weight — so saturation is a belt-and-braces guard
/// rather than a live path.
#[must_use]
pub fn weighted_distance(a: &SetupFingerprint, b: &SetupFingerprint, w: &FeatureWeights) -> u64 {
    let mut acc = 0u64;
    for (i, spec) in FIELD_SPECS.iter().enumerate() {
        let cost = u64::from(field_cost(spec, a.buckets[i], b.buckets[i]));
        acc = acc.saturating_add(cost.saturating_mul(u64::from(w.w[i])));
    }
    acc
}

/// Per-field distance cost under the encoding contract.
#[must_use]
fn field_cost(spec: &FieldSpec, a: u8, b: u8) -> u32 {
    match spec.kind {
        FieldKind::Ordinal => u32::from(a.abs_diff(b)),
        FieldKind::Nominal => {
            if a == b {
                0
            } else {
                NOMINAL_MISMATCH_COST
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic, non-default probe setup used across tests.
    fn probe() -> SetupInputs {
        SetupInputs {
            ofi_bps: 750,
            cvd_decade: 10,
            trend_structure: TrendStructure::Up,
            range_state: RangeState::Compressed,
            burst_phase: BurstPhase::Onset,
            realized_vol_bps: 300,
            liquidity_decade: 11,
            buyer_breadth: 25,
            token_age_ns: 400 * 1_000_000_000,
            venue_phase: VenuePhase::Curve,
            attention_velocity_bps: 3_000,
            narrative_class: NarrativeClass::Animal,
            authenticity_bps: 8_000,
            holder_growth_accel_bps: 900,
            creator_class: CreatorClass::Proven,
            meta_category_id: 42,
            meta_saturation_state: MetaSaturationState::Hot,
            designated_caller_present: true,
            round_trip_cost_bps: 150,
            info_time_ns: 13 * 3_600 * 1_000_000_000,
        }
    }

    #[test]
    fn field_table_layout_is_consistent() {
        let mut offset = 0u8;
        for spec in FIELD_SPECS.iter() {
            let expect_width = match spec.kind {
                FieldKind::Ordinal => spec.levels - 1,
                FieldKind::Nominal => spec.levels,
            };
            assert_eq!(
                spec.bit_width, expect_width,
                "width mismatch for {}",
                spec.name
            );
            assert_eq!(spec.bit_offset, offset, "offset mismatch for {}", spec.name);
            offset += spec.bit_width;
        }
        assert_eq!(u32::from(offset), SIGNATURE_BITS);
        const { assert!(SIGNATURE_BITS <= 128, "signature must fit a u128") };
    }

    #[test]
    fn field_names_are_unique() {
        for (i, a) in FIELD_SPECS.iter().enumerate() {
            for b in FIELD_SPECS.iter().skip(i + 1) {
                assert_ne!(a.name, b.name, "duplicate field name {}", a.name);
            }
        }
    }

    #[test]
    fn all_ladders_are_strictly_ascending() {
        fn check(name: &str, edges: &[i64]) {
            for pair in edges.windows(2) {
                assert!(pair[0] < pair[1], "ladder {name} is not strictly ascending");
            }
        }
        check("ofi", &OFI_EDGES_BPS);
        check("cvd", &CVD_DECADE_EDGES);
        check("vol", &REALIZED_VOL_EDGES_BPS);
        check("liq", &LIQUIDITY_DECADE_EDGES);
        check("breadth", &BUYER_BREADTH_EDGES);
        check("age", &TOKEN_AGE_EDGES_NS);
        check("attention", &ATTENTION_VELOCITY_EDGES_BPS);
        check("authenticity", &AUTHENTICITY_EDGES_BPS);
        check("holder", &HOLDER_GROWTH_ACCEL_EDGES_BPS);
        check("cost", &ROUND_TRIP_COST_EDGES_BPS);
    }

    #[test]
    fn ladder_bucket_is_monotone() {
        let edges = &OFI_EDGES_BPS;
        let mut prev = ladder_bucket(-1_000_000, edges);
        let mut x = -5_000i64;
        while x <= 5_000 {
            let b = ladder_bucket(x, edges);
            assert!(b >= prev, "ladder went backwards at x={x}");
            prev = b;
            x += 7; // stride is coprime with the edges so boundaries get straddled
        }
    }

    #[test]
    fn ladder_boundaries_are_inclusive_lower_bounds() {
        // x == edge promotes into the upper bucket; x == edge - 1 does not.
        for (i, edge) in OFI_EDGES_BPS.iter().enumerate() {
            assert_eq!(ladder_bucket(*edge, &OFI_EDGES_BPS), (i + 1) as u8);
            assert_eq!(ladder_bucket(*edge - 1, &OFI_EDGES_BPS), i as u8);
        }
    }

    #[test]
    fn ladder_saturates_at_both_ends() {
        assert_eq!(ladder_bucket(i64::MIN, &OFI_EDGES_BPS), 0);
        assert_eq!(
            ladder_bucket(i64::MAX, &OFI_EDGES_BPS),
            OFI_EDGES_BPS.len() as u8
        );
    }

    #[test]
    fn signed_decade_is_exact_and_signed() {
        assert_eq!(signed_decade(0), 0);
        assert_eq!(signed_decade(1), 0);
        assert_eq!(signed_decade(9), 0);
        assert_eq!(signed_decade(10), 1);
        assert_eq!(signed_decade(999), 2);
        assert_eq!(signed_decade(1_000), 3);
        assert_eq!(signed_decade(1_000_000_000), 9);
        assert_eq!(signed_decade(-1_000_000_000), -9);
        assert_eq!(signed_decade(-1), 0);
    }

    #[test]
    fn signed_decade_is_monotone_on_positives() {
        let mut prev = signed_decade(0);
        let mut x = 1i128;
        while x < 1_000_000_000_000 {
            let d = signed_decade(x);
            assert!(d >= prev);
            prev = d;
            x = x * 3 + 1;
        }
    }

    #[test]
    fn time_of_day_bucket_partitions_the_day() {
        assert_eq!(time_of_day_bucket(0), 0);
        let block = NS_PER_DAY / TIME_OF_DAY_BUCKETS;
        for k in 0..TIME_OF_DAY_BUCKETS {
            assert_eq!(u64::from(time_of_day_bucket(k * block)), k);
            assert_eq!(u64::from(time_of_day_bucket(k * block + block - 1)), k);
        }
        // Wraps on the next day, identically.
        assert_eq!(time_of_day_bucket(NS_PER_DAY), 0);
        assert_eq!(time_of_day_bucket(NS_PER_DAY * 7 + block * 3), 3);
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let inputs = probe();
        let a = SetupFingerprint::from_inputs(&inputs);
        for _ in 0..64 {
            let b = SetupFingerprint::from_inputs(&inputs);
            assert_eq!(a, b);
            assert_eq!(a.signature(), b.signature());
            assert_eq!(a.buckets(), b.buckets());
        }
    }

    #[test]
    fn every_bucket_is_within_its_field_cardinality() {
        let fp = SetupFingerprint::from_inputs(&probe());
        for (i, spec) in FIELD_SPECS.iter().enumerate() {
            assert!(
                fp.buckets()[i] < spec.levels,
                "field {} bucket {} >= levels {}",
                spec.name,
                fp.buckets()[i],
                spec.levels
            );
        }
    }

    #[test]
    fn signature_uses_only_declared_bits() {
        let fp = SetupFingerprint::from_inputs(&probe());
        let mask = (1u128 << SIGNATURE_BITS) - 1;
        assert_eq!(fp.signature() & !mask, 0);
    }

    #[test]
    fn hamming_equals_unweighted_l1() {
        // Deterministic sweep: perturb every field over its full bucket range.
        let base = SetupFingerprint::from_inputs(&probe());
        for (i, spec) in FIELD_SPECS.iter().enumerate() {
            for lvl in 0..spec.levels {
                let mut b = *base.buckets();
                b[i] = lvl;
                let other = SetupFingerprint::from_buckets(b);
                assert_eq!(
                    signature_hamming(base.signature(), other.signature()),
                    unweighted_distance(&base, &other),
                    "field {} level {}",
                    spec.name,
                    lvl
                );
            }
        }
    }

    #[test]
    fn hamming_equals_unweighted_l1_on_multi_field_perturbations() {
        let base = SetupFingerprint::from_inputs(&probe());
        // Deterministic pseudo-sweep via a fixed multiplicative walk (no RNG).
        let mut seed = 1u32;
        for _ in 0..512 {
            let mut b = *base.buckets();
            for (i, spec) in FIELD_SPECS.iter().enumerate() {
                seed = mix_u32(seed.wrapping_add(i as u32));
                b[i] = (seed % u32::from(spec.levels)) as u8;
            }
            let other = SetupFingerprint::from_buckets(b);
            assert_eq!(
                signature_hamming(base.signature(), other.signature()),
                unweighted_distance(&base, &other)
            );
        }
    }

    #[test]
    fn one_bucket_change_moves_distance_by_a_bounded_predictable_amount() {
        let base_inputs = probe();
        let base = SetupFingerprint::from_inputs(&base_inputs);
        let weights = FeatureWeights::default();

        // Move the OFI field down exactly one bucket: 750 bp -> 400 bp crosses the
        // 500 bp edge, one ordinal step.
        let mut moved = base_inputs;
        moved.ofi_bps = 400;
        let fp = SetupFingerprint::from_inputs(&moved);
        assert_eq!(base.buckets()[F_OFI] - fp.buckets()[F_OFI], 1);
        assert_eq!(signature_hamming(base.signature(), fp.signature()), 1);
        assert_eq!(weighted_distance(&base, &fp, &weights), u64::from(W_OFI));
    }

    #[test]
    fn nominal_mismatch_costs_exactly_two_bits() {
        let base_inputs = probe();
        let base = SetupFingerprint::from_inputs(&base_inputs);
        let mut other_inputs = base_inputs;
        other_inputs.narrative_class = NarrativeClass::Tech;
        let other = SetupFingerprint::from_inputs(&other_inputs);
        assert_eq!(
            signature_hamming(base.signature(), other.signature()),
            NOMINAL_MISMATCH_COST
        );
        assert_eq!(
            weighted_distance(&base, &other, &FeatureWeights::default()),
            u64::from(NOMINAL_MISMATCH_COST) * u64::from(W_NARRATIVE_CLASS)
        );
    }

    #[test]
    fn distance_to_self_is_zero_and_symmetric() {
        let a = SetupFingerprint::from_inputs(&probe());
        let mut other_inputs = probe();
        other_inputs.buyer_breadth = 1;
        other_inputs.trend_structure = TrendStructure::Down;
        let b = SetupFingerprint::from_inputs(&other_inputs);
        let w = FeatureWeights::default();
        assert_eq!(weighted_distance(&a, &a, &w), 0);
        assert_eq!(unweighted_distance(&a, &a), 0);
        assert_eq!(weighted_distance(&a, &b, &w), weighted_distance(&b, &a, &w));
        assert_eq!(unweighted_distance(&a, &b), unweighted_distance(&b, &a));
    }

    #[test]
    fn ordinal_distance_grows_monotonically_with_bucket_gap() {
        let base = SetupFingerprint::from_inputs(&probe());
        let mut b = *base.buckets();
        let mut prev = 0u32;
        for lvl in 0..FIELD_SPECS[F_TOKEN_AGE].levels {
            b[F_TOKEN_AGE] = lvl;
            let other = SetupFingerprint::from_buckets(b);
            let d = unweighted_distance(&base, &other);
            let gap = u32::from(base.buckets()[F_TOKEN_AGE].abs_diff(lvl));
            assert_eq!(d, gap);
            if lvl > base.buckets()[F_TOKEN_AGE] {
                assert!(d > prev);
            }
            prev = d;
        }
    }

    #[test]
    fn uniform_weights_reproduce_the_prefilter_metric() {
        let a = SetupFingerprint::from_inputs(&probe());
        let mut inputs = probe();
        inputs.meta_category_id = 7;
        inputs.realized_vol_bps = 5_000;
        let b = SetupFingerprint::from_inputs(&inputs);
        assert_eq!(
            weighted_distance(&a, &b, &FeatureWeights::uniform()),
            u64::from(signature_hamming(a.signature(), b.signature()))
        );
    }

    #[test]
    fn from_buckets_clamps_out_of_range_and_recomputes_signature() {
        let mut b = [0u8; FIELD_COUNT];
        b[F_TREND_STRUCTURE] = 250; // way out of range
        let fp = SetupFingerprint::from_buckets(b);
        assert_eq!(
            fp.buckets()[F_TREND_STRUCTURE],
            FIELD_SPECS[F_TREND_STRUCTURE].levels - 1
        );
        assert_eq!(fp.signature(), pack_signature(fp.buckets()));
        // Clamping must not have leaked into a neighbouring field's window.
        assert_eq!(fp.buckets()[F_RANGE_STATE], 0);
    }

    #[test]
    fn venue_phase_round_trips_through_the_fingerprint() {
        let mut inputs = probe();
        inputs.venue_phase = VenuePhase::Pool;
        assert_eq!(
            SetupFingerprint::from_inputs(&inputs).venue_phase(),
            VenuePhase::Pool
        );
        inputs.venue_phase = VenuePhase::Curve;
        assert_eq!(
            SetupFingerprint::from_inputs(&inputs).venue_phase(),
            VenuePhase::Curve
        );
    }

    #[test]
    fn cross_phase_distance_is_dominated_by_the_phase_weight() {
        let mut curve = probe();
        curve.venue_phase = VenuePhase::Curve;
        let mut pool = probe();
        pool.venue_phase = VenuePhase::Pool;
        let a = SetupFingerprint::from_inputs(&curve);
        let b = SetupFingerprint::from_inputs(&pool);
        assert_eq!(
            weighted_distance(&a, &b, &FeatureWeights::default()),
            u64::from(NOMINAL_MISMATCH_COST) * u64::from(W_VENUE_PHASE)
        );
    }

    #[test]
    fn default_weights_rank_structure_above_time_of_day() {
        let w = FeatureWeights::default();
        assert!(w.w[F_TREND_STRUCTURE] > w.w[F_TIME_OF_DAY]);
        assert!(w.w[F_OFI] > w.w[F_TIME_OF_DAY]);
        assert!(w.w[F_VENUE_PHASE] >= *w.w.iter().max().expect("non-empty"));
        assert!(
            w.w.iter().all(|x| *x > 0),
            "a zero weight silently drops a field"
        );
        assert!(w.total() > 0);
    }

    #[test]
    fn bucket_accessor_is_bounds_checked() {
        let fp = SetupFingerprint::from_inputs(&probe());
        assert!(fp.bucket(F_OFI).is_some());
        assert!(fp.bucket(FIELD_COUNT).is_none());
    }

    #[test]
    fn meta_category_slot_is_stable_and_in_range() {
        for id in [0u32, 1, 42, 9_999, u32::MAX] {
            let s = meta_category_slot(id);
            assert_eq!(s, meta_category_slot(id));
            assert!(u32::from(s) < META_CATEGORY_SLOTS);
        }
    }
}
