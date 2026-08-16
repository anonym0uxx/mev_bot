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

/// The independent §71.2 discovery lane that surfaced a candidate — a
/// provenance tag **distinct from** the setup-archetype [`Lane`].
///
/// Responsibility: name the ACTUAL ingest lane that observed a mint, so realized
/// net-SOL can be attributed to the lane that earned it. Two different discovery
/// lanes can present as the SAME setup archetype — an on-chain creation sighting
/// and a social caller both surface as [`Lane::CreationSniper`]; a narrative
/// blast and a live attention-velocity reading both surface as
/// [`Lane::EarlyConfirmation`]. Keying `wl_lane_performance` on the setup
/// archetype alone therefore cross-contaminates per-lane learning (a losing
/// social caller taints a winning creation sniper). This enum separates the
/// independent lanes so reflection keys on the real provenance (§71 reflection
/// integrity). Ordering is stable; it never participates in ranking (that keys
/// on [`Lane`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum DiscoveryLane {
    /// A fresh on-chain creation sighting (a decoded launch), before anyone
    /// trades or shills it. Presents as `CreationSniper`.
    OnchainCreation,
    /// A social-source caller / mention (calls, shills). Presents as
    /// `CreationSniper`.
    SocialCaller,
    /// The narrative + live attention-velocity field (`virality = attention =
    /// money`). Presents as `EarlyConfirmation`.
    NarrativeAttentionVelocity,
    /// Smart-money / wallet-graph followable activity. Presents as
    /// `GraduationTransition`.
    WalletSmartMoney,
    /// Active on-chain numeric market flow (the self-authorizing lane). Presents
    /// as `ActiveMarketScalp`.
    ActiveMarket,
    /// A DESIGNATED-caller alpha call — a curated X follow or a Discord alpha
    /// room whose event carries `is_designated_caller` (§29). Structurally an
    /// early call, so it presents as `CreationSniper` like [`Self::SocialCaller`],
    /// but it is attributed as its OWN lane: a paid alpha room must earn its net
    /// SOL independently of the open social-caller firehose (§71 reflection
    /// integrity), so reflection can up/down-weight or retire the room on its own
    /// realized outcome. Set explicitly at the emit seam via
    /// [`Candidate::with_discovery_lane`]; never derived from a bare setup.
    AlphaCall,
}

impl DiscoveryLane {
    /// Every discovery lane, in stable discriminant order. Fixed-size: the
    /// per-lane net-SOL ledger is sized from this (§99 bounded).
    pub const ALL: [DiscoveryLane; 6] = [
        DiscoveryLane::OnchainCreation,
        DiscoveryLane::SocialCaller,
        DiscoveryLane::NarrativeAttentionVelocity,
        DiscoveryLane::WalletSmartMoney,
        DiscoveryLane::ActiveMarket,
        DiscoveryLane::AlphaCall,
    ];

    /// Number of discovery lanes; the fixed width of the per-lane ledger. §99.
    pub const COUNT: usize = Self::ALL.len();

    /// Dense array index (0..[`DiscoveryLane::COUNT`]) for bounded per-lane
    /// accounting. §99.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            DiscoveryLane::OnchainCreation => 0,
            DiscoveryLane::SocialCaller => 1,
            DiscoveryLane::NarrativeAttentionVelocity => 2,
            DiscoveryLane::WalletSmartMoney => 3,
            DiscoveryLane::ActiveMarket => 4,
            DiscoveryLane::AlphaCall => 5,
        }
    }

    /// The setup archetype [`Lane`] this discovery lane presents as (many-to-one:
    /// creation-sighting and social-caller both present as `CreationSniper`).
    #[must_use]
    pub const fn setup_lane(self) -> Lane {
        match self {
            DiscoveryLane::OnchainCreation
            | DiscoveryLane::SocialCaller
            | DiscoveryLane::AlphaCall => Lane::CreationSniper,
            DiscoveryLane::NarrativeAttentionVelocity => Lane::EarlyConfirmation,
            DiscoveryLane::WalletSmartMoney => Lane::GraduationTransition,
            DiscoveryLane::ActiveMarket => Lane::ActiveMarketScalp,
        }
    }

    /// The default discovery lane for a bare setup archetype — used only when a
    /// [`Candidate`] is constructed without an explicit provenance (the inverse
    /// of [`Self::setup_lane`] on the canonical representative of each archetype).
    /// Emit sites that need the precise lane (e.g. a social caller, an alpha-room
    /// [`Self::AlphaCall`], and a creation sighting, which all share
    /// `CreationSniper`) override it with [`Candidate::with_discovery_lane`].
    /// `CreationSniper`'s canonical representative stays [`Self::OnchainCreation`],
    /// so this mapping is unchanged by the alpha-call lane (additive).
    #[must_use]
    pub const fn from_setup(lane: Lane) -> Self {
        match lane {
            Lane::CreationSniper => DiscoveryLane::OnchainCreation,
            Lane::EarlyConfirmation => DiscoveryLane::NarrativeAttentionVelocity,
            Lane::GraduationTransition => DiscoveryLane::WalletSmartMoney,
            Lane::ActiveMarketScalp => DiscoveryLane::ActiveMarket,
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
    // ---- Rev-13 entry quality filter (walk-forward validated 2026-08-12) ----
    /// Buy-side trade ratio: (# buy trades / total trades) × 10_000.
    /// Computed from the bounded trade ring at discovery. A ratio below the
    /// configured minimum indicates the token lacks organic buy demand.
    /// 0 when no trades have been observed yet.
    pub buy_ratio_bp: u32,
    /// Largest single trade (in lamports) observed in the pre-entry trade ring.
    /// A whale-dominated token where one trade dwarfs the rest is a risk:
    /// the whale's exit will crater the curve. 0 when no trades observed.
    pub max_trade_lamports: u64,
    /// Number of trades in the bounded ring at discovery. Used to ensure
    /// the buy_ratio and max_trade signals are statistically meaningful
    /// (not computed on 2 trades).
    pub trades_observed: u32,
    /// Cumulative quote volume (lamports) of all trades in the bounded ring.
    /// Rev-14 entry quality filter: tokens with <2 SOL cumulative volume lack
    /// sufficient liquidity for reversion entry. Computed as the sum of
    /// `quote_qty` across the ring (last 64 trades).
    pub volume_lamports: u64,
    // ---- Rev-14 wangr intelligence (Aug 2026 pump.fun graduation study) ----
    //
    // Five signals from the wangr.com analysis of 567,876 pump.fun tokens
    // (2,770 graduations). All default to 0 / 255 = UNOBSERVED, which is a
    // no-op for every gate filter: the filter checks are guarded by config
    // enable flags AND a non-sentinel value, so a golden tape that never feeds
    // the new events produces byte-identical decisions.
    //
    // The engine enriches these fields at gate time (gate_evaluate) from its
    // own state — they are NOT populated by the lane's features_for / emit
    // (which stay zero-sentinel for the watchlist candidate).
    /// Token standard: 0=unobserved, 1=legacy SPL, 2=mayhem/token2022.
    /// Wangr: legacy tokens graduate 5× more often (1.66% vs 0.32%).
    /// Fed by `AppEvent::MarketAuxiliary`; 0 when unobserved.
    pub token_standard: u8,
    /// Symbol string length (character count). Wangr: 4-6 char symbols
    /// are most common among graduated tokens. 0 when unobserved.
    /// Fed by `AppEvent::MarketAuxiliary`.
    pub symbol_len: u8,
    /// Day of week: 0=unobserved, 1=Mon, 2=Tue, …, 7=Sun (ISO-8601 order).
    /// Wangr: Friday highest graduation rate (0.68%); Tuesday lowest (0.04%).
    /// Fed by `AppEvent::TimeSignal`; 0 when no time signal has been fed.
    pub dow: u8,
    /// Hour of day in UTC: 0-23 valid, 255=unobserved.
    /// Wangr: 3-5 UTC (1.02%, 0.94%) and 10-11 UTC (0.95%, 0.93%) are best.
    /// Fed by `AppEvent::TimeSignal`; 255 when no time signal has been fed.
    pub hour_utc: u8,
    /// Lifetime launch count of this mint's creator entity. Wangr: the top
    /// 20 creator wallets have 8-13 graduations each (vs 0.43% base rate).
    /// Sourced from the engine's existing `creator_launches` map at gate
    /// time; 0 when the creator is unknown or has no prior launches.
    pub creator_launches: u32,
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
    /// The setup-archetype lane that made this observation (ranking / evidence
    /// weight key).
    pub lane: Lane,
    /// The independent §71.2 discovery lane provenance (net-SOL attribution key).
    /// Defaulted from `lane` in [`Candidate::new`]; set precisely at the emit
    /// seam via [`Candidate::with_discovery_lane`] when two lanes share an
    /// archetype. Never participates in ranking (§71 reflection integrity).
    pub discovery_lane: DiscoveryLane,
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
            discovery_lane: DiscoveryLane::from_setup(lane),
            discovery_score,
            discovered_at,
            features,
        }
    }

    /// Tag this candidate with its precise §71.2 discovery-lane provenance,
    /// overriding the archetype-derived default. Used at the ingest/emit seam so
    /// that two lanes sharing a setup archetype (creation-sighting vs
    /// social-caller; narrative vs attention-velocity) attribute their realized
    /// net-SOL independently. Pure, `const`, does not touch ranking fields. §22.
    #[inline]
    #[must_use]
    pub const fn with_discovery_lane(mut self, discovery_lane: DiscoveryLane) -> Self {
        self.discovery_lane = discovery_lane;
        self
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
