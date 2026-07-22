//! Typed row structs for every QuantMemoryStore table, plus the newtype
//! identifiers and fixed-point domain types they use.
//!
//! Responsibility: give each research-memory record an explicit, machine-checked
//! Rust type so nothing is stringly-typed and every numeric field is
//! integer/fixed-point (§22). These are pure data with public fields — the store
//! (`crate::store`) owns insertion/lookup and the capacity contract, the sealing
//! logic lives on `Experiment`, and the VOI logic reads `Hypothesis`.
//!
//! Constitution mapping: §29.9 (table set), §29.8 (source-quality determinants
//! and classification states), §29.7 (amplification-graph edges), §21.4 / §29.9
//! (meta categories, assignments, rotation snapshots), §56.10 (inference
//! lifecycle states).

/// Signed lamports / net-SOL quantity. Signed because impacts and markouts can be
/// negative (a consistently-late channel is negative-alpha, §29.7). Integer only
/// (§22). Width is `i128` so intermediate VOI products do not overflow at
/// realistic magnitudes; overflow past `i128` is handled explicitly in
/// `crate::voi`.
pub type Lamports = i128;

/// A fixed-point ratio in basis points (1 bp = 1/10_000). Used for probabilities,
/// confidences, and executable returns so no floating point enters an outcome
/// path (§22). See [`crate::voi::BPS_DENOM`].
pub type Bps = i64;

/// Nanosecond timestamp. Always supplied by an injected clock upstream; this crate
/// never reads a wall clock (§22). Deterministic ordering key.
pub type TimestampNs = u64;

macro_rules! id_newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);
    };
}

id_newtype!(
    /// Stable identifier of a [`Hypothesis`] (§56.10).
    HypothesisId
);
id_newtype!(
    /// Stable identifier of an [`Experiment`] (§56.1).
    ExperimentId
);
id_newtype!(
    /// Stable identifier of an [`ExperimentResult`] (§56.10).
    ResultId
);
id_newtype!(
    /// Stable identifier of a [`SocialCall`] (§29.8).
    SocialCallId
);
id_newtype!(
    /// Stable identifier of a [`CallMarkout`] (§29.8 D1).
    MarkoutId
);
id_newtype!(
    /// Stable identifier of a source in the [`SourceQualityEntry`] ledger (§29.8).
    SourceId
);
id_newtype!(
    /// Stable identifier of an [`AmplificationEdge`] (§29.7).
    EdgeId
);
id_newtype!(
    /// Stable identifier of a [`MetaCategory`] (§21.4 / §29.9).
    MetaCategoryId
);
id_newtype!(
    /// Stable identifier of a [`CategoryAssignment`] (§29.9).
    AssignmentId
);
id_newtype!(
    /// Stable identifier of a [`MetaRotationSnapshot`] (§29.9).
    SnapshotId
);

/// A 24-byte content fingerprint stored as a fixed array so a row never carries
/// raw adversarial social text into memory (§29.5 / §29.8: only hashes and scored
/// output cross into the research memory).
pub type ContentHash = [u8; 32];

/// Inference lifecycle state (§56.10 anti-contamination law). Only
/// [`InferenceState::ValidatedInference`] may influence production, through the
/// normal gates; the VOI queue ranks the *open* states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InferenceState {
    /// Raw captured observation, not yet a claim.
    Observation,
    /// A registered claim awaiting test.
    Hypothesis,
    /// Preliminary evidence, not yet validated.
    ProvisionalInference,
    /// Validated, current, in-regime — the only state that may reach production.
    ValidatedInference,
    /// Tested and disconfirmed; preserved permanently so it is never re-run.
    RejectedInference,
    /// Was valid, now out of date; edge half-life elapsed.
    ExpiredInference,
    /// Valid only within a specific market regime.
    RegimeSpecificInference,
}

impl InferenceState {
    /// True when the hypothesis is still an *open research question* (§56.10): it
    /// has neither been validated, rejected, nor expired, so it belongs in the
    /// value-of-information queue. `RegimeSpecificInference` is treated as open
    /// because it still has unresolved out-of-regime value to learn.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(
            self,
            InferenceState::Observation
                | InferenceState::Hypothesis
                | InferenceState::ProvisionalInference
                | InferenceState::RegimeSpecificInference
        )
    }
}

/// A registered research hypothesis and the four value-of-information inputs the
/// prioritisation queue ranks on (§56.10). Numeric inputs are integer/fixed-point
/// (§22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hypothesis {
    /// Stable identifier.
    pub id: HypothesisId,
    /// Schema version this row was written under (see [`crate::schema`]).
    pub schema_version: u32,
    /// Fingerprint of the hypothesis statement text (statement itself is stored
    /// off-memory; §29.5).
    pub statement_hash: ContentHash,
    /// Expected net-SOL impact **if the hypothesis is true**, per reference edge
    /// horizon, in lamports (may be negative for a fade/avoid hypothesis).
    pub expected_impact_lamports: Lamports,
    /// Probability the hypothesis is true given prior evidence, in basis points
    /// (0..=10_000).
    pub prob_true_bps: Bps,
    /// Cost to run the deciding experiment, in lamports (non-negative).
    pub cost_to_test_lamports: u64,
    /// Edge half-life in seconds: how long the edge remains exploitable before it
    /// decays, which scales how much realised value confirming it can capture.
    pub edge_half_life_secs: u64,
    /// Current inference lifecycle state (§56.10).
    pub status: InferenceState,
}

/// A registered, immutable-once-sealed experiment (§56.1). Sealing computes a
/// deterministic content hash; after sealing the safe mutation API refuses every
/// change and [`Experiment::verify_integrity`] detects any out-of-band tamper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Experiment {
    /// Stable identifier.
    pub id: ExperimentId,
    /// Hypothesis this experiment tests.
    pub hypothesis_id: HypothesisId,
    /// Schema version this row was written under.
    pub schema_version: u32,
    /// Fingerprint of the experiment's registered design/title text.
    pub title_hash: ContentHash,
    /// Fingerprint of the causal-mechanism statement (§56.10).
    pub causal_mechanism_hash: ContentHash,
    /// Fingerprint of the sealed dataset manifest the experiment runs against
    /// (§56.9: data must be sealed, versioned, manifested).
    pub dataset_hash: ContentHash,
    /// Configuration hash of the run (`ConfigHash`, §56.3).
    pub config_hash: u64,
    /// Registration timestamp supplied by an injected clock (never read here).
    pub created_at_ns: TimestampNs,
    /// Whether the experiment has been sealed.
    pub sealed: bool,
    /// The content hash recorded at seal time; `None` until sealed.
    pub seal_hash: Option<crate::hashing::SealHash>,
}

/// The reconciled outcome of a sealed experiment (§56.10). Results reference their
/// experiment and are only meaningful once that experiment is sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentResult {
    /// Stable identifier.
    pub id: ResultId,
    /// Experiment this result belongs to.
    pub experiment_id: ExperimentId,
    /// Reconciled out-of-sample net-SOL effect, lamports (signed).
    pub net_sol_effect_lamports: Lamports,
    /// Statistical significance, basis points (e.g. one minus p-value ×10_000).
    pub significance_bps: Bps,
    /// Resulting inference lifecycle state after evaluation (§56.10).
    pub outcome: InferenceState,
    /// Reconciliation timestamp (injected clock).
    pub reconciled_at_ns: TimestampNs,
}

/// Where in the token lifecycle a social call was posted (§29.8 D2). Persistent
/// post-peak posting is exit-liquidity promotion regardless of tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleTiming {
    /// Before breadth expansion.
    PreFlow,
    /// During active flow.
    WithFlow,
    /// After the peak — distribution warning.
    PostPeak,
    /// Lifecycle state not yet determinable.
    Unknown,
}

/// Source classification states of the `SocialSourceQualityLedger` (§29.8). Never
/// bullish/bearish by default; never permanent from one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceClassification {
    /// Beats the D3 state-at-call selection control before flow.
    PreFlowAlpha,
    /// Rides existing flow rather than leading it.
    FlowAmplifier,
    /// Consistently late — a fade/avoid signal.
    LateExitLiquidityPromoter,
    /// Buy-before-call / distribute-into-call pattern suspected (§29.8 D5).
    PaidShillSuspect,
    /// Manufactured engagement.
    EngagementFarm,
    /// Echo node, not an originator (§29.8 D8).
    CopyEchoAccount,
    /// Organic community node.
    OrganicCommunityNode,
    /// Not enough data — the default entry state.
    InsufficientSample,
}

/// One attributable social/alpha call: account × token × timestamp × content hash
/// (§29.8). Raw text never enters — only the content hash and scored fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialCall {
    /// Stable identifier.
    pub id: SocialCallId,
    /// Source (channel/account) that made the call.
    pub source_id: SourceId,
    /// Fingerprint identifying the called token (mint), stored as a hash.
    pub token_hash: ContentHash,
    /// Capture timestamp (injected clock) — the markout reference time.
    pub captured_at_ns: TimestampNs,
    /// Fingerprint of the call content (§29.5: no raw adversarial text stored).
    pub content_hash: ContentHash,
    /// Lifecycle state of the token at call time (§29.8 D2).
    pub timing: LifecycleTiming,
}

/// Forward-executable markout horizon (§29.8 D1): +5m, +30m, +2h, +24h from
/// capture time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkoutHorizon {
    /// +5 minutes.
    M5,
    /// +30 minutes.
    M30,
    /// +2 hours.
    H2,
    /// +24 hours.
    H24,
}

/// A reconciled forward-executable return for a single call at a single horizon
/// (§29.8 D1, the ground-truth determinant). Return is fixed-point basis points,
/// signed, computed from reconstructed market state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallMarkout {
    /// Stable identifier.
    pub id: MarkoutId,
    /// Call this markout scores.
    pub call_id: SocialCallId,
    /// Horizon from capture time.
    pub horizon: MarkoutHorizon,
    /// Forward executable return in basis points (signed).
    pub executable_return_bps: Bps,
}

/// A row of the `source_quality_ledger` (§29.8): a source's current classification
/// with decomposed sample size, confidence, and decay, so a claim is never a bare
/// number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceQualityEntry {
    /// Source this scorecard is for.
    pub source_id: SourceId,
    /// Current classification state.
    pub classification: SourceClassification,
    /// Confidence in the classification, basis points (0..=10_000).
    pub confidence_bps: Bps,
    /// Number of reconciled calls behind the score (sample size).
    pub sample_size: u32,
    /// Mean D1 markout at the +30m horizon, basis points (signed) — the headline
    /// decomposed determinant kept inline for cheap ranking.
    pub mean_markout_30m_bps: Bps,
    /// Last update timestamp (injected clock) — feeds time decay.
    pub updated_at_ns: TimestampNs,
}

/// Kind of amplification-graph edge (§29.7). Originators are targets; amplifiers
/// are reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// Quote of another post.
    Quote,
    /// Reply to another post.
    Reply,
    /// Repost/retweet.
    Repost,
    /// Telegram native forward (forward-provenance walking, §29.7).
    Forward,
}

/// A timestamped directed edge in the amplification graph (§29.7): who amplified
/// whom, when, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmplificationEdge {
    /// Stable identifier.
    pub id: EdgeId,
    /// Amplifying source (reach).
    pub from_source: SourceId,
    /// Amplified source (candidate originator).
    pub to_source: SourceId,
    /// Token the amplified call concerned, as a hash.
    pub token_hash: ContentHash,
    /// Edge timestamp (injected clock) — used for timestamp ordering (§29.8 D8).
    pub observed_at_ns: TimestampNs,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// Meta-category lifecycle state (§29.9 meta reflection: emerging, accelerating,
/// saturating, dying).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaLifecycle {
    /// Newly detected category.
    Emerging,
    /// Gaining share/velocity.
    Accelerating,
    /// Attention saturated.
    Saturating,
    /// Losing share.
    Dying,
}

/// A row of `meta_categories` (§21.4 / §29.9): a named narrative meta and its
/// current lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaCategory {
    /// Stable identifier.
    pub id: MetaCategoryId,
    /// Fingerprint of the category name/definition (text stored off-memory).
    pub name_hash: ContentHash,
    /// Current lifecycle state.
    pub lifecycle: MetaLifecycle,
    /// When this state was last computed (injected clock).
    pub updated_at_ns: TimestampNs,
}

/// A row of `category_assignments` (§29.9): a timestamped, confidence-scored
/// assignment of a token to a meta category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryAssignment {
    /// Stable identifier.
    pub id: AssignmentId,
    /// Category assigned to.
    pub category_id: MetaCategoryId,
    /// Token assigned, as a hash.
    pub token_hash: ContentHash,
    /// Assignment confidence, basis points (0..=10_000).
    pub confidence_bps: Bps,
    /// Assignment timestamp (injected clock).
    pub assigned_at_ns: TimestampNs,
}

/// A row of `meta_rotation_snapshots` (§29.9): a point-in-time reading of a meta
/// category's rotation state and launch-share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaRotationSnapshot {
    /// Stable identifier.
    pub id: SnapshotId,
    /// Category this snapshot describes.
    pub category_id: MetaCategoryId,
    /// Snapshot timestamp (injected clock).
    pub taken_at_ns: TimestampNs,
    /// Lifecycle/rotation state at snapshot time.
    pub lifecycle: MetaLifecycle,
    /// Category share of recent launches, basis points (0..=10_000).
    pub launch_share_bps: Bps,
}
