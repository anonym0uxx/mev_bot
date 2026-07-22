//! `wl_candidate` leaf — the typed discovery record.
//!
//! Responsibility: define the immutable, `Copy`, fully-typed value that flows
//! through the whole watchlist: the [`Candidate`], its owning discovery [`Lane`],
//! the [`Mint`] key, and the fixed-point [`Features`] snapshot captured at
//! discovery. No behaviour beyond construction, accessors, and the per-lane
//! evidence priors that ranking and dedup depend on.
//!
//! Constitution: §22 (all fields integer / fixed-point, deterministic),
//! §102 (per-lane weight priors are named constants with rationale, not magic).

/// A Solana mint address: a 32-byte public key.
///
/// Newtype so it can be a deterministic `BTreeMap` key (total `Ord`) without
/// hashing randomness. Responsibility: identity of a discovered token. §22.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Mint(pub [u8; 32]);

impl Mint {
    /// Construct a mint from its 32 raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32 bytes of the mint.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 32] {
        self.0
    }
}

/// A discovery lane: one of the constitution's independently-attributed setup
/// families that can surface a candidate (§ StrategyRuntime opportunity lens).
///
/// Responsibility: identify *which* lane observed a mint, so evidence can be
/// weighted per lane and realized net-SOL attributed per lane. Ordering of the
/// discriminants is stable and is used as the final deterministic tie-break.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Lane {
    /// Extremely-early low-cap entry at creation. Most speculative evidence.
    CreationSniper,
    /// Early entry after first confirming on-chain flow. Strongest early prior.
    EarlyConfirmation,
    /// Graduation / migration transition play.
    GraduationTransition,
    /// Active post-migration market scalp (Section 24 lane).
    ActiveMarketScalp,
}

impl Lane {
    /// Every lane, in stable discriminant order. Fixed-size: the watchlist's
    /// per-lane state arrays are sized from this (§99 bounded).
    pub const ALL: [Lane; 4] = [
        Lane::CreationSniper,
        Lane::EarlyConfirmation,
        Lane::GraduationTransition,
        Lane::ActiveMarketScalp,
    ];

    /// Number of lanes; the fixed width of every per-lane array. §99.
    pub const COUNT: usize = Self::ALL.len();

    /// Dense array index for this lane (0..[`Lane::COUNT`]).
    ///
    /// Responsibility: map a lane to a fixed-array slot for bounded per-lane
    /// accounting. §99.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Lane::CreationSniper => 0,
            Lane::EarlyConfirmation => 1,
            Lane::GraduationTransition => 2,
            Lane::ActiveMarketScalp => 3,
        }
    }

    /// The a-priori evidence weight of this lane, in basis points
    /// ([`crate::rank::WEIGHT_ONE`] = 1.0×).
    ///
    /// These are **static-by-design priors** (§102): documented starting
    /// values, not fitted magic numbers. They express relative confidence in a
    /// lane's raw discovery signal before realized performance is known, and are
    /// overridable at ranking time via [`crate::rank::LaneWeights`].
    ///
    /// Rationale: confirmed early flow (`EarlyConfirmation`) carries the
    /// strongest short-horizon evidence, so it is a premium; `CreationSniper`
    /// fires before confirmation and is discounted; `GraduationTransition` and
    /// `ActiveMarketScalp` sit around the 1.0× baseline.
    #[must_use]
    pub const fn default_weight_bp(self) -> u32 {
        match self {
            Lane::CreationSniper => 8_000,        // 0.80×
            Lane::EarlyConfirmation => 12_000,    // 1.20×
            Lane::GraduationTransition => 11_000, // 1.10×
            Lane::ActiveMarketScalp => 10_000,    // 1.00× baseline
        }
    }
}

/// Fixed-point feature snapshot captured at discovery time.
///
/// Responsibility: carry the small, bounded set of decoded on-chain quantities
/// a discovery lane observed, in integer / fixed-point form (§22). Purely data;
/// ranking reads only `discovery_score` on the candidate, but downstream stages
/// consume these features.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Features {
    /// Pool liquidity at discovery, in lamports.
    pub liquidity_lamports: u64,
    /// Buy pressure over the observation window, in basis points (10_000 = 100%).
    pub buy_pressure_bp: u32,
    /// Count of distinct buyer entities observed (entity-deduplicated upstream).
    pub unique_buyers: u32,
    /// Age of the market at discovery, in slots.
    pub age_slots: u32,
}

/// A single discovery observation: one lane's claim that one mint is worth
/// watching, at one logical time, with a fixed-point discovery score.
///
/// Responsibility: the immutable unit of the watchlist (`wl_candidate`, §22).
/// `Copy` so the bounded state can move records without allocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
    /// The discovered token.
    pub mint: Mint,
    /// The lane that made this observation.
    pub lane: Lane,
    /// Raw discovery score in caller-defined fixed-point units. Ranking is
    /// monotonic in this value; larger means stronger raw signal. §22.
    pub discovery_score: u64,
    /// Logical discovery time (a `u64` tick supplied by the caller; never a
    /// wall-clock read here). Used for recency decay and TTL. §22.
    pub discovered_at: u64,
    /// Fixed-point feature snapshot at discovery.
    pub features: Features,
}

impl Candidate {
    /// Construct a candidate.
    ///
    /// Responsibility: the single typed constructor for `wl_candidate`. §22.
    #[must_use]
    pub const fn new(
        mint: Mint,
        lane: Lane,
        discovery_score: u64,
        discovered_at: u64,
        features: Features,
    ) -> Self {
        Self {
            mint,
            lane,
            discovery_score,
            discovered_at,
            features,
        }
    }

    /// The strength of this record's lane evidence, used to break ties when the
    /// same mint is discovered by more than one lane.
    ///
    /// Defined as `discovery_score × lane.default_weight_bp` in `u128` so it
    /// never overflows for any `u64 × u32` product. Deterministic (§22); larger
    /// is stronger. `lane_weights` overrides the per-lane basis-point weight.
    #[inline]
    #[must_use]
    pub fn evidence_strength(&self, weight_bp: u32) -> u128 {
        u128::from(self.discovery_score) * u128::from(weight_bp)
    }
}
