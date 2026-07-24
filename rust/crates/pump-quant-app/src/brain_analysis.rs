//! LAW B6 — the **strategy-analysis export**: what the brain knows, rendered as a
//! bounded, deterministic, integer-only JSON artifact that something outside this
//! process can actually act on.
//!
//! # Why this module exists
//!
//! Before it, the brain *observed* and *reported* and nothing consumed the report
//! for strategy analysis. Episodes were recorded, recall was exposed on
//! [`crate::engine::Report`] — and reflection, the exit tournament, promotion and
//! the research supervisor were all blind to it. The brain could tell you that a
//! setup class had bled for thirty trades and nothing anywhere would research it,
//! test it, block it or retire it. This module is the artifact that closes that:
//! one file, written alongside `live_status.json`, that a research consumer reads.
//!
//! # The five hard rules of the artifact
//!
//! **1. Info-time, never wall-clock.** `info_time_ns` and `tick` come from the
//! event stream. Two replays of one tape write byte-identical files (§22/§54).
//!
//! **2. Integers only.** No `f32`/`f64` is reachable from any value here, and the
//! writer emits plain decimal — no scientific notation, no `NaN`, no `1e9`. A
//! consumer parsing this with a strict integer parser will never be surprised
//! (§22).
//!
//! **3. Every array is bounded and totally ordered.** Each collection carries a
//! named `*_CAP` const and a documented sort key that is total, so the artifact is
//! a pure function of state and not of iteration order (§99/§22).
//!
//! **4. An `unknown` verdict emits its refusal reason and NO estimate.** Every
//! estimate field on an unknown row is `null`. This is the artifact-level mirror
//! of [`pump_quant_brain::recall::RecallVerdict::Unknown`] being structurally
//! incapable of carrying a number: a consumer must not be able to read a value the
//! brain refused to give. A `null` is the honest answer; a `0` would be a lie with
//! a type (§46).
//!
//! **5. Report plane only.** Nothing here is read back by a decision. The one
//! decision-plane consumer of this module's *logic* is [`lane_decay`], which
//! [`crate::reflect`] may use under `brain_reflect_enable` — and that path is
//! reduce-only and default OFF.
//!
//! # `retirement_flags` — the §56 sequential-retirement INPUT
//!
//! The point of the exercise. [`retirement_flags`] asks, of every discovery lane,
//! style lens, setup class and alpha source: *has this decayed on our own realized
//! evidence?* Two shapes count as decay, and both must clear
//! [`Config::brain_decay_min_sample`]:
//!
//! * **Conditioned-negative** — the subject's conditioned realized performance is
//!   negative over a real sample. Not "flat", not "worse than hoped": negative.
//! * **Window regression** — the subject's RECENT window is materially worse than
//!   its earlier window, by at least [`DECAY_WINDOW_MARGIN_BP`] of the earlier
//!   window's magnitude. This catches the lane whose lifetime aggregate still looks
//!   fine because of one early runner.
//!
//! **The boundary, stated because it is the whole point.** A retirement flag is
//! *not* a retirement. §56 sequential retirement is a **governed** decision that
//! runs under the §51 FDR/PBO promotion statistics and the §52 baseline verdict; an
//! episodic-recall summary is neither of those and must never be allowed to
//! substitute for them. Episodic recall is a sample the strategy generated for
//! itself, on a schedule it chose, over markets it selected — the single most
//! overfit-prone evidence in the building. What it is genuinely good for is
//! *nomination*: telling the weekly governance review which four subjects out of
//! forty are worth spending an FDR-corrected test on. So this module emits
//! candidates with a reason string and a sample count, and the retirement decision
//! stays where §51/§52 put it. Nothing in this crate consumes `retirement_flags`
//! to retire anything, and `tests/brain_strategy.rs` pins that the flags are
//! decision-inert.
//!
//! Integer-only (§22), bounded (§99), named consts with §-citations (§102).

use std::io::Write;

use pump_quant_brain::archetype::{
    archetype_performance, best_paying_lens, StyleLens, ARCHETYPE_MIN_SAMPLE, STYLE_LENSES,
};
use pump_quant_brain::episode::DiscoveryLane as BrainLane;
use pump_quant_brain::fingerprint::{MetaSaturationState, VenuePhase};
use pump_quant_brain::meta_timeline::{MetaMatchParams, MetaSnapshot, META_MAX_MATCHES_DEFAULT};
use pump_quant_brain::recall::{RecallStats, RecallUnknown, RecallVerdict};
use pump_quant_governance::retirement_review::{RetirementNomination, ReviewSubject};
use pump_quant_social::types::SourceRef;
use pump_quant_watchlist::candidate::{DiscoveryLane, Lane};
use pump_quant_watchlist::lane_performance::{DiscoveryLanePerformance, LanePerformance};

use crate::brain::{BrainPlane, ConditionedClass};
use crate::reflect::LaneDecay;
use crate::social_plane::{
    CallerTrustRow, FollowRecoRow, SocialPlane, SupportNeed, TrustVerdictRow, UnfollowRow,
};

// ---------------------------------------------------------------------------
// Named constants (§102/§99)
// ---------------------------------------------------------------------------

/// The artifact's record tag. Bumped alongside [`SCHEMA_VERSION`] if the field set
/// ever changes shape; a consumer that does not recognise the pair must refuse the
/// file rather than guess at it.
pub const RECORD_TAG: &str = "brain_analysis_v1";

/// The artifact's schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// §99 bound on exported setup classes. The engine's traded-class set is itself
/// capped at [`crate::brain::BRAIN_TRADED_CLASS_CAP`]; this is the export's own
/// independent bound so a cap change upstream cannot silently unbound the file.
pub const ANALYSIS_CLASS_CAP: usize = 32;

/// §99 bound on lens scoreboard rows. Structurally `LENS_COUNT × 2 phases = 8`;
/// stated as a const so the writer's bound does not depend on an upstream count.
pub const ANALYSIS_LENS_CAP: usize = 8;

/// §99 bound on exported meta-state rows.
pub const ANALYSIS_META_CAP: usize = 16;

/// §99 bound on exported past-meta match rows.
pub const ANALYSIS_PAST_META_CAP: usize = 16;

/// §99 bound on exported caller-trust rows.
pub const ANALYSIS_TRUST_CAP: usize = 16;

/// §99 bound on exported follow / unfollow rows.
pub const ANALYSIS_FOLLOW_CAP: usize = 16;

/// §99 bound on exported capture-work-list rows.
pub const ANALYSIS_NEEDS_CAP: usize = 16;

/// §99 bound on exported retirement-flag rows. Deliberately small: a review list
/// of forty candidates is not a review list, it is a backlog. The flags are sorted
/// worst-realized-first, so the cap keeps the *most* damaging nominations.
pub const ANALYSIS_RETIREMENT_CAP: usize = 16;

/// §46/§102 default sample floor for ANY decay flag (a retirement nomination or a
/// LAW B7 lane downweight).
///
/// `12`, deliberately above the brain's own `MIN_SAMPLE_DEFAULT` of 8. A recall
/// estimate at n=8 is allowed to *inform* — it is one input among many and it can
/// only ever shrink a size. A decay flag is different in kind: it nominates a
/// whole lane or style for retirement review, and a spurious nomination costs
/// governance attention, which is the scarcest resource in the loop. Requiring
/// half again as much evidence for the louder claim is the cheapest available
/// guard against nominating on noise (§46).
pub const BRAIN_DECAY_MIN_SAMPLE_DEFAULT: u32 = 12;

/// §56.11/§102 window-regression margin: the recent window must be worse than the
/// earlier window by at least 25% of the earlier window's magnitude before the
/// difference counts as decay.
///
/// Without a margin, any subject whose two halves differ at all would flag, which
/// is every subject. 2_500 bp says the deterioration has to be a quarter of what
/// the subject used to make — large enough that noise on a 12-sample split does
/// not routinely clear it, small enough that a genuinely dying edge is caught
/// before it has given everything back.
pub const DECAY_WINDOW_MARGIN_BP: i128 = 2_500;

/// §46 minimum episodes per HALF before the window-regression test may run at all.
/// A "recent window" of two trades is an anecdote (§46).
pub const DECAY_WINDOW_MIN_HALF: u32 = 6;

/// §99 bound on the alpha sources examined for retirement flags per export.
pub const ANALYSIS_SOURCE_CAP: usize = 16;

// ---------------------------------------------------------------------------
// Small honest scalars
// ---------------------------------------------------------------------------

/// Narrow a lamport quantity to the artifact's `i64` money width, saturating.
///
/// The brain carries realized net as `i128` because intermediate sums must not
/// wrap (§22). The wire format is `i64`, which covers ±9.2×10^18 lamports —
/// nine billion SOL, several orders of magnitude beyond any reachable bankroll. A
/// saturation here therefore means the state is already corrupt; saturating is
/// still the right response, because emitting a wrapped number would hand a
/// consumer a plausible-looking lie.
#[must_use]
pub const fn narrow_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// The artifact's name for a venue phase (§100 — phases are never pooled, so the
/// name is always present and never "both").
#[must_use]
pub const fn venue_phase_name(p: VenuePhase) -> &'static str {
    match p {
        VenuePhase::Curve => "curve",
        VenuePhase::Pool => "pool",
    }
}

/// The artifact's name for a brain discovery lane.
#[must_use]
pub const fn brain_lane_name(l: BrainLane) -> &'static str {
    match l {
        BrainLane::NewMint => "new_mint",
        BrainLane::Migration => "migration",
        BrainLane::WhaleFollow => "whale_follow",
        BrainLane::SocialCall => "social_call",
        BrainLane::Watchlist => "watchlist",
        BrainLane::Rescan => "rescan",
    }
}

/// The artifact's name for an app setup lane.
#[must_use]
pub const fn setup_lane_name(l: Lane) -> &'static str {
    match l {
        Lane::CreationSniper => "creation_sniper",
        Lane::EarlyConfirmation => "early_confirmation",
        Lane::GraduationTransition => "graduation_transition",
        Lane::ActiveMarketScalp => "active_market_scalp",
    }
}

/// The artifact's name for an app discovery lane (§71.2 — the INDEPENDENT lane,
/// not the setup archetype it presents as).
#[must_use]
pub const fn discovery_lane_name(l: DiscoveryLane) -> &'static str {
    match l {
        DiscoveryLane::OnchainCreation => "onchain_creation",
        DiscoveryLane::SocialCaller => "social_caller",
        DiscoveryLane::NarrativeAttentionVelocity => "narrative_attention_velocity",
        DiscoveryLane::WalletSmartMoney => "wallet_smart_money",
        DiscoveryLane::ActiveMarket => "active_market",
        DiscoveryLane::AlphaCall => "alpha_call",
    }
}

/// The artifact's name for a social platform ordinal. Out-of-range ordinals are
/// impossible from this crate's own crosswalks; the fallback keeps the writer
/// total rather than panicking on the report plane (§18).
#[must_use]
pub const fn platform_name(code: u8) -> &'static str {
    match code {
        0 => "x",
        1 => "telegram",
        2 => "discord",
        3 => "stream",
        4 => "aggregator",
        _ => "unknown",
    }
}

/// The artifact's name for a trust tier ordinal.
#[must_use]
pub const fn trust_tier_name(code: u8) -> &'static str {
    match code {
        0 => "unproven",
        1 => "watch",
        2 => "trusted",
        3 => "demoted",
        _ => "unproven",
    }
}

/// The artifact's name for a §28 source-exposure ordinal.
#[must_use]
pub const fn exposure_name(code: u8) -> &'static str {
    match code {
        0 => "niche",
        1 => "crowded",
        2 => "public_burned",
        _ => "unset",
    }
}

/// The artifact's name for a meta lifecycle phase.
#[must_use]
pub const fn meta_phase_name(s: MetaSaturationState) -> &'static str {
    match s {
        MetaSaturationState::Emerging => "emerging",
        MetaSaturationState::Hot => "hot",
        MetaSaturationState::Saturated => "saturated",
        MetaSaturationState::Decaying => "decaying",
    }
}

/// The artifact's stable label for a recall refusal. The labels are a closed
/// vocabulary a consumer can branch on; the numbers behind them (`n_matched`,
/// `min_sample`) are NOT re-exported into an estimate field, because a refusal's
/// diagnostics are diagnostics, not a distribution.
#[must_use]
pub const fn refusal_name(u: RecallUnknown) -> &'static str {
    match u {
        RecallUnknown::EmptyIndex => "empty_index",
        RecallUnknown::NoEpisodeInScope => "no_episode_in_scope",
        RecallUnknown::NoCandidateInRadius { .. } => "no_candidate_in_radius",
        RecallUnknown::InsufficientSample { .. } => "insufficient_sample",
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One exported setup class. The estimate fields are `Option` so the writer can
/// emit `null` for a refusal — the type carries the same "no estimate exists"
/// guarantee the recall verdict does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassRow {
    /// Packed fingerprint signature (rendered as a decimal string: a `u128` does
    /// not fit a JSON number a strict consumer will accept).
    pub signature: u128,
    /// `curve` | `pool`.
    pub venue_phase: &'static str,
    /// Conditioning meta category.
    pub meta_category: u32,
    /// Conditioning discovery lane.
    pub discovery_lane: &'static str,
    /// `None` for a refusal; the refusal reason travels in `unknown_reason`.
    pub stats: Option<RecallStats>,
    /// `Some(label)` exactly when `stats` is `None`.
    pub unknown_reason: Option<&'static str>,
}

/// One exported style-lens scoreboard row, phase-separated (§100).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LensRow {
    /// Lens name.
    pub lens: &'static str,
    /// `curve` | `pool`.
    pub venue_phase: &'static str,
    /// `None` for a refusal.
    pub stats: Option<RecallStats>,
    /// `Some(label)` exactly when `stats` is `None`.
    pub unknown_reason: Option<&'static str>,
}

/// The single best-paying lens across both phases, or `None` when no lens clears
/// the sample floor with a positive median.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BestLensRow {
    /// Lens name.
    pub lens: &'static str,
    /// `curve` | `pool`.
    pub venue_phase: &'static str,
    /// Median realized net, lamports.
    pub median_net_lamports: i64,
    /// Episodes behind it.
    pub n: u32,
}

/// One exported meta lifecycle row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaRow {
    /// Category id.
    pub meta_category: u32,
    /// `emerging` | `hot` | `saturated` | `decaying` | `unknown`.
    pub phase: &'static str,
    /// Snapshots behind the row.
    pub n: u32,
    /// Fall in participant breadth from this meta's peak, bps of the peak. `0`
    /// when the peak is the current value (no decline) — a genuine measured zero,
    /// not a stand-in for "unknown": a meta with no snapshot never reaches here.
    pub participation_decline_bp: u32,
    /// Fall in realized net from this meta's peak, bps of `|peak|`, signed.
    /// Positive means "worse than peak"; `0` when it is still at its peak.
    pub outcome_decline_bp: i64,
}

/// One exported past-meta match — "does this match the current meta, or a past
/// one, and what did that past one actually pay?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PastMetaRow {
    /// The current meta used as the query.
    pub current_meta: u32,
    /// The matched past meta.
    pub past_meta: u32,
    /// Snapshot distance between them.
    pub distance: u32,
    /// Realized net that followed the past match, lamports.
    pub past_realized_net_lamports: i64,
    /// Snapshots behind the match.
    pub n: u32,
}

/// One exported caller-trust row. `score_bp` / `n_markouts` are `None` for an
/// unknown verdict — trust below the evidence floor has no score, by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustRow {
    /// The author.
    pub author_id: u64,
    /// The platform carrying most of their retained calls, or `None`.
    pub platform: Option<&'static str>,
    /// Earned tier (`unproven` for a refusal — the only tier a thin record has).
    pub tier: &'static str,
    /// Post-demotion trust score, bps. `None` for a refusal.
    pub score_bp: Option<i32>,
    /// Attributed markouts. `None` for a refusal.
    pub n_markouts: Option<u32>,
    /// Operator-set §28 exposure.
    pub exposure: &'static str,
}

/// One exported follow recommendation (research only, §110).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRow {
    /// The author.
    pub author_id: u64,
    /// Dominant platform.
    pub platform: &'static str,
    /// Attributed calls.
    pub n_calls: u32,
    /// Lead-time-weighted realized attribution, lamports.
    pub realized_net_attributed: i64,
    /// Median lead, ns.
    pub median_lead_ns: u64,
    /// Earned trust tier.
    pub trust_tier: &'static str,
}

/// One exported unfollow candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnfollowExportRow {
    /// The author.
    pub author_id: u64,
    /// Dominant platform.
    pub platform: &'static str,
    /// Attributed realized net — negative by definition of appearing here.
    pub realized_net_attributed: i64,
    /// Attributed calls.
    pub n_calls: u32,
}

/// One exported capture-work-list row: what the ingestion plane should go fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedRow {
    /// Stable kind label.
    pub kind: &'static str,
    /// Platform to poll, when the need names one.
    pub platform: Option<&'static str>,
    /// Author to build evidence for, when the need names one.
    pub author_id: Option<u64>,
    /// Market the need concerns.
    pub mint_id: Option<u64>,
}

/// What kind of thing a retirement flag is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlagSubject {
    /// An independent discovery lane (§71.2).
    Lane,
    /// A named style lens / archetype.
    Archetype,
    /// One conditioned setup class.
    SetupClass,
    /// A paid or curated alpha source.
    Source,
}

impl FlagSubject {
    /// The artifact's name for the subject kind.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lane => "lane",
            Self::Archetype => "archetype",
            Self::SetupClass => "setup_class",
            Self::Source => "source",
        }
    }
}

/// One **nomination** for the §56 weekly retirement review. NOT a retirement —
/// see the module docs for why that boundary is load-bearing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetirementFlag {
    /// What kind of subject.
    pub subject: FlagSubject,
    /// The subject's key (lane name, lens name, decimal signature, source key).
    pub key: String,
    /// Why it was nominated — a closed vocabulary a consumer can branch on.
    pub reason: &'static str,
    /// Episodes (or trades) behind the nomination. Always ≥ the configured floor.
    pub n: u32,
    /// The realized net that earned the nomination, lamports.
    pub realized_net_lamports: i64,
}

impl FlagSubject {
    /// The governance review's subject vocabulary. The two enums are kept
    /// separate on purpose — the app owns what it can *observe*, governance owns
    /// what may be *decided* — and this is the single crosswalk between them.
    #[must_use]
    pub const fn as_review_subject(self) -> ReviewSubject {
        match self {
            Self::Lane => ReviewSubject::Lane,
            Self::Archetype => ReviewSubject::Archetype,
            Self::SetupClass => ReviewSubject::SetupClass,
            Self::Source => ReviewSubject::Source,
        }
    }
}

impl RetirementFlag {
    /// Hand this flag to governance as a §56 review **agenda item**.
    ///
    /// This is the only bridge out of this module toward a retirement, and note
    /// what it produces: a [`RetirementNomination`], which has no method that
    /// returns a retirement. Reaching
    /// [`pump_quant_governance::retirement_review::ReviewOutcome::Retire`]
    /// additionally requires the §51 statistical verdict and the §52 baseline
    /// verdict, both supplied by governance, neither derivable from anything the
    /// brain knows. A caller holding a thousand damning flags and no statistical
    /// test cannot construct a retirement — there is no such function to call.
    #[must_use]
    pub const fn as_nomination(&self) -> RetirementNomination {
        RetirementNomination {
            subject: self.subject.as_review_subject(),
            n: self.n,
            realized_net_lamports: self.realized_net_lamports,
        }
    }
}

/// Reason label: the subject's conditioned realized performance is negative over
/// a sample that cleared the §46 floor.
pub const REASON_CONDITIONED_NEGATIVE: &str = "conditioned_negative";
/// Reason label: the subject's recent window is materially worse than its earlier
/// window, by at least [`DECAY_WINDOW_MARGIN_BP`].
pub const REASON_WINDOW_REGRESSION: &str = "window_regression";
/// Reason label: the subject's lifetime realized aggregate is negative over a
/// sample that cleared the §46 floor (used where only an aggregate exists — the
/// per-source and per-discovery-lane ledgers).
pub const REASON_AGGREGATE_NEGATIVE: &str = "aggregate_negative";

// ---------------------------------------------------------------------------
// The artifact
// ---------------------------------------------------------------------------

/// The whole `brain_analysis_v1` record. Every collection is already bounded and
/// sorted by [`build`]; the writer only renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrainAnalysis {
    /// Info-time of the snapshot, ns (never wall-clock).
    pub info_time_ns: u64,
    /// The engine's logical tick at the snapshot.
    pub tick: u64,
    /// Episodes held in the index.
    pub episodes_total: u64,
    /// Of which carry realized P&L from an admitted trade.
    pub episodes_admitted: u64,
    /// Conditioned setup classes, refusals included.
    pub setup_classes: Vec<ClassRow>,
    /// Per-lens realized scoreboard, phase-separated.
    pub lens_scoreboard: Vec<LensRow>,
    /// The single best-paying lens, or `None`.
    pub best_paying_lens: Option<BestLensRow>,
    /// Meta lifecycle state.
    pub meta_state: Vec<MetaRow>,
    /// Past-meta matches for the current metas.
    pub past_meta_matches: Vec<PastMetaRow>,
    /// Earned trust of the callers we act on.
    pub caller_trust: Vec<TrustRow>,
    /// Who to follow (research only, §110).
    pub follow_recommendations: Vec<FollowRow>,
    /// Who to drop.
    pub unfollow_candidates: Vec<UnfollowExportRow>,
    /// What the capture plane should fetch.
    pub support_inputs_needed: Vec<NeedRow>,
    /// §56 retirement-review nominations.
    pub retirement_flags: Vec<RetirementFlag>,
}

/// Everything [`build`] reads. A borrowed bundle rather than a long argument list,
/// so the engine's call site stays one line and adding an input later is not a
/// breaking signature change.
#[derive(Clone, Copy)]
pub struct AnalysisInputs<'a> {
    /// Info-time of the snapshot, ns.
    pub info_time_ns: u64,
    /// The engine's logical tick.
    pub tick: u64,
    /// The episodic memory plane.
    pub brain: &'a BrainPlane,
    /// The social abstraction plane.
    pub social: &'a SocialPlane,
    /// Realized net per setup lane.
    pub lane_perf: &'a LanePerformance,
    /// Realized net per INDEPENDENT discovery lane (§71.2).
    pub disc_perf: &'a DiscoveryLanePerformance,
    /// Realized net per paid/curated alpha source.
    pub alpha_source_net: &'a [(SourceRef, i64)],
    /// §46 sample floor for recall estimates.
    pub min_sample: u32,
    /// §46 sample floor for a decay flag (strictly the louder claim).
    pub decay_min_sample: u32,
}

/// Build the artifact from engine state. Pure: no clock, no I/O, no RNG.
#[must_use]
pub fn build(inputs: &AnalysisInputs<'_>) -> BrainAnalysis {
    let classes = inputs.brain.conditioned_classes();
    let (episodes_total, episodes_admitted) = inputs.brain.episode_counts();

    BrainAnalysis {
        info_time_ns: inputs.info_time_ns,
        tick: inputs.tick,
        episodes_total,
        episodes_admitted,
        setup_classes: class_rows(&classes),
        lens_scoreboard: lens_rows(inputs),
        best_paying_lens: best_lens_row(inputs),
        meta_state: meta_rows(inputs),
        past_meta_matches: past_meta_rows(inputs),
        caller_trust: trust_rows(inputs),
        follow_recommendations: follow_rows(inputs),
        unfollow_candidates: unfollow_rows(inputs),
        support_inputs_needed: need_rows(inputs),
        retirement_flags: retirement_flags(inputs, &classes),
    }
}

/// Setup-class rows. Sorted by `(sample DESC, median net DESC, signature ASC)` so
/// the strongest evidence leads; refusals carry `n = 0` for the sort and sink to
/// the bottom in signature order, which is total.
fn class_rows(classes: &[ConditionedClass]) -> Vec<ClassRow> {
    let mut out: Vec<ClassRow> = classes
        .iter()
        .map(|c| ClassRow {
            signature: c.signature,
            venue_phase: venue_phase_name(c.venue_phase),
            meta_category: c.meta_category_id,
            discovery_lane: brain_lane_name(c.discovery_lane),
            stats: c.verdict.stats().copied(),
            unknown_reason: match c.verdict {
                RecallVerdict::Known(_) => None,
                RecallVerdict::Unknown(u) => Some(refusal_name(u)),
            },
        })
        .collect();
    out.sort_by(|a, b| {
        let (an, am) = a
            .stats
            .map_or((0u32, 0i128), |s| (s.n_matched, s.median_net_lamports));
        let (bn, bm) = b
            .stats
            .map_or((0u32, 0i128), |s| (s.n_matched, s.median_net_lamports));
        bn.cmp(&an)
            .then(bm.cmp(&am))
            .then(a.signature.cmp(&b.signature))
    });
    out.truncate(ANALYSIS_CLASS_CAP);
    out
}

/// Lens rows, in `(phase ordinal, lens ordinal)` order — a fixed grid, so the
/// consumer sees the same eight slots every time and a missing lens is visibly a
/// refusal rather than an absence.
fn lens_rows(inputs: &AnalysisInputs<'_>) -> Vec<LensRow> {
    let floor = inputs.min_sample.max(ARCHETYPE_MIN_SAMPLE);
    let index = inputs.brain.index();
    let mut out: Vec<LensRow> = Vec::new();
    for phase in [VenuePhase::Curve, VenuePhase::Pool] {
        for lens in STYLE_LENSES {
            let verdict = archetype_performance(index, lens, phase, floor);
            out.push(LensRow {
                lens: lens.name(),
                venue_phase: venue_phase_name(phase),
                stats: verdict.stats().copied(),
                unknown_reason: match verdict {
                    RecallVerdict::Known(_) => None,
                    RecallVerdict::Unknown(u) => Some(refusal_name(u)),
                },
            });
        }
    }
    out.truncate(ANALYSIS_LENS_CAP);
    out
}

/// The best-paying lens across both phases. Ranked by median first (the robust
/// statistic — one outlier cannot crown a style), then sample, then phase ordinal;
/// `None` when no lens clears the floor with a positive median (§46: "least bad"
/// is not "paying").
fn best_lens_row(inputs: &AnalysisInputs<'_>) -> Option<BestLensRow> {
    let floor = inputs.min_sample.max(ARCHETYPE_MIN_SAMPLE);
    let index = inputs.brain.index();
    let mut best: Option<(VenuePhase, StyleLens, RecallStats)> = None;
    for phase in [VenuePhase::Curve, VenuePhase::Pool] {
        let Some((lens, stats)) = best_paying_lens(index, phase, floor) else {
            continue;
        };
        // `(median, n, −phase ordinal)` descending — a total order, so the winner
        // never depends on which phase was examined first.
        let key = (
            stats.median_net_lamports,
            i128::from(stats.n_matched),
            -i128::from(phase.ordinal()),
        );
        let better = match &best {
            None => true,
            Some((bp, _, b)) => {
                key > (
                    b.median_net_lamports,
                    i128::from(b.n_matched),
                    -i128::from(bp.ordinal()),
                )
            }
        };
        if better {
            best = Some((phase, lens, stats));
        }
    }
    best.map(|(phase, lens, stats)| BestLensRow {
        lens: lens.name(),
        venue_phase: venue_phase_name(phase),
        median_net_lamports: narrow_i64(stats.median_net_lamports),
        n: stats.n_matched,
    })
}

/// Meta rows, ascending by category id.
fn meta_rows(inputs: &AnalysisInputs<'_>) -> Vec<MetaRow> {
    let timeline = inputs.brain.meta_timeline();
    let mut out: Vec<MetaRow> = Vec::new();
    for snap in timeline.current_metas() {
        let Some(stats) = timeline.meta_lifecycle_stats(snap.meta_category_id) else {
            continue;
        };
        // Participation decline: how far breadth has fallen from its own peak, as
        // a share of that peak. Integer division truncates toward zero, so the
        // reported decline is a LOWER bound — the honest direction (§22).
        let participation_decline_bp = if stats.peak_participant_breadth == 0 {
            0
        } else {
            let fall = stats
                .peak_participant_breadth
                .saturating_sub(snap.participant_breadth);
            u32::try_from(
                u64::from(fall).saturating_mul(10_000) / u64::from(stats.peak_participant_breadth),
            )
            .unwrap_or(u32::MAX)
        };
        // Outcome decline: the same shape over realized net, scaled by |peak|.
        // Signed, so a meta that is ABOVE its previous peak reports a negative
        // decline rather than a clamped zero.
        let peak_abs = stats.peak_net_lamports.saturating_abs();
        let outcome_decline_bp = if peak_abs == 0 {
            0
        } else {
            narrow_i64(
                stats
                    .peak_net_lamports
                    .saturating_sub(stats.terminal_net_lamports)
                    .saturating_mul(10_000)
                    / peak_abs,
            )
        };
        out.push(MetaRow {
            meta_category: snap.meta_category_id,
            phase: meta_phase_name(snap.saturation),
            n: stats.n_snapshots,
            participation_decline_bp,
            outcome_decline_bp,
        });
    }
    out.sort_by_key(|m| m.meta_category);
    out.truncate(ANALYSIS_META_CAP);
    out
}

/// Past-meta matches: each current meta is used as a query against the timeline.
/// Sorted by `(current, distance ASC, past ASC)` — nearest analogue first.
fn past_meta_rows(inputs: &AnalysisInputs<'_>) -> Vec<PastMetaRow> {
    let params = MetaMatchParams {
        max_matches: META_MAX_MATCHES_DEFAULT,
        ..MetaMatchParams::default()
    };
    let mut out: Vec<PastMetaRow> = Vec::new();
    for snap in inputs.brain.meta_timeline().current_metas() {
        let query = MetaSnapshot {
            meta_category_id: snap.meta_category_id,
            info_time_ns: snap.info_time_ns,
            saturation: snap.saturation,
            aggregate_net_lamports: snap.aggregate_net_lamports,
            participant_breadth: snap.participant_breadth,
            episode_count: snap.episode_count,
        };
        for m in inputs.brain.match_past_meta(&query, &params) {
            out.push(PastMetaRow {
                current_meta: snap.meta_category_id,
                past_meta: m.meta_category_id,
                distance: m.distance,
                past_realized_net_lamports: narrow_i64(m.subsequent_net_lamports),
                n: m.n_snapshots,
            });
        }
    }
    out.sort_by(|a, b| {
        a.current_meta
            .cmp(&b.current_meta)
            .then(a.distance.cmp(&b.distance))
            .then(a.past_meta.cmp(&b.past_meta))
    });
    out.truncate(ANALYSIS_PAST_META_CAP);
    out
}

/// Caller-trust rows, ascending by author id (a total order that does not depend
/// on a score which may be absent).
fn trust_rows(inputs: &AnalysisInputs<'_>) -> Vec<TrustRow> {
    let mut out: Vec<TrustRow> = inputs
        .social
        .trust_rows()
        .into_iter()
        .map(|r: CallerTrustRow| {
            let (tier, score_bp, n_markouts) = match r.verdict {
                TrustVerdictRow::Known {
                    trust_score_bp,
                    n_markouts,
                    tier_code,
                    ..
                } => (
                    trust_tier_name(tier_code),
                    Some(trust_score_bp),
                    Some(n_markouts),
                ),
                // §46: a refusal carries a TIER (always `unproven`) and no numbers.
                TrustVerdictRow::Unknown { tier_code } => (trust_tier_name(tier_code), None, None),
            };
            TrustRow {
                author_id: r.author_id,
                platform: inputs
                    .social
                    .dominant_platform_of(r.author_id)
                    .map(platform_name),
                tier,
                score_bp,
                n_markouts,
                exposure: exposure_name(r.exposure_code),
            }
        })
        .collect();
    out.sort_by_key(|r| r.author_id);
    out.truncate(ANALYSIS_TRUST_CAP);
    out
}

/// Follow rows, best attribution first then author id.
fn follow_rows(inputs: &AnalysisInputs<'_>) -> Vec<FollowRow> {
    let mut out: Vec<FollowRow> = inputs
        .social
        .follow_rows()
        .into_iter()
        .map(|r: FollowRecoRow| FollowRow {
            author_id: r.author_id,
            platform: platform_name(r.platform_code),
            n_calls: r.n_calls,
            realized_net_attributed: narrow_i64(r.realized_net_attributed_lamports),
            median_lead_ns: r.median_lead_ns,
            trust_tier: trust_tier_name(r.trust_tier_code),
        })
        .collect();
    out.sort_by(|a, b| {
        b.realized_net_attributed
            .cmp(&a.realized_net_attributed)
            .then(a.author_id.cmp(&b.author_id))
    });
    out.truncate(ANALYSIS_FOLLOW_CAP);
    out
}

/// Unfollow rows, worst attribution first then author id.
fn unfollow_rows(inputs: &AnalysisInputs<'_>) -> Vec<UnfollowExportRow> {
    let mut out: Vec<UnfollowExportRow> = inputs
        .social
        .unfollow_rows()
        .into_iter()
        .map(|r: UnfollowRow| UnfollowExportRow {
            author_id: r.author_id,
            platform: platform_name(r.platform_code),
            realized_net_attributed: narrow_i64(r.realized_net_attributed_lamports),
            n_calls: r.n_calls,
        })
        .collect();
    out.sort_by(|a, b| {
        a.realized_net_attributed
            .cmp(&b.realized_net_attributed)
            .then(a.author_id.cmp(&b.author_id))
    });
    out.truncate(ANALYSIS_FOLLOW_CAP);
    out
}

/// Capture-work-list rows, ordered by `(kind, mint, author, platform)` — total,
/// and stable against the order the support estimator happened to emit them in.
fn need_rows(inputs: &AnalysisInputs<'_>) -> Vec<NeedRow> {
    let mut out: Vec<NeedRow> = inputs
        .social
        .needs()
        .into_iter()
        .map(|n| match n {
            SupportNeed::MoreOriginators { mint_id, .. } => NeedRow {
                kind: "more_originators",
                platform: None,
                author_id: None,
                mint_id: Some(mint_id),
            },
            SupportNeed::ContentDigests { mint_id, .. } => NeedRow {
                kind: "content_digests",
                platform: None,
                author_id: None,
                mint_id: Some(mint_id),
            },
            SupportNeed::PlatformCoverage {
                mint_id,
                platform_code,
            } => NeedRow {
                kind: "platform_coverage",
                platform: Some(platform_name(platform_code)),
                author_id: None,
                mint_id: Some(mint_id),
            },
            SupportNeed::AuthorTrackRecord { mint_id, author_id } => NeedRow {
                kind: "author_track_record",
                platform: None,
                author_id: Some(author_id),
                mint_id: Some(mint_id),
            },
            SupportNeed::SourceExposure { mint_id, author_id } => NeedRow {
                kind: "source_exposure",
                platform: None,
                author_id: Some(author_id),
                mint_id: Some(mint_id),
            },
        })
        .collect();
    out.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then(a.mint_id.cmp(&b.mint_id))
            .then(a.author_id.cmp(&b.author_id))
            .then(a.platform.cmp(&b.platform))
    });
    out.dedup();
    out.truncate(ANALYSIS_NEEDS_CAP);
    out
}

// ---------------------------------------------------------------------------
// Decay detection — the §56 retirement-review INPUT
// ---------------------------------------------------------------------------

/// Whether a conditioned distribution is **conditioned-negative**: a real sample
/// whose median AND mean realized net are both below zero.
///
/// Both, not either. A negative median with a positive mean is a subject that
/// pays rarely and hugely — a lottery, but not necessarily a losing one, and the
/// exit ladder owns that shape, not the retirement review. A negative mean with a
/// positive median is a subject with a fat left tail — again a risk-management
/// problem, not an existence problem. Requiring both is the conjunction that says
/// "this is not paying, in either the typical or the aggregate sense".
#[must_use]
pub fn is_conditioned_negative(stats: &RecallStats, min_sample: u32) -> bool {
    stats.n_matched >= min_sample && stats.median_net_lamports < 0 && stats.mean_net_lamports < 0
}

/// Whether a realized series has **regressed**: its recent half is worse than its
/// earlier half by at least [`DECAY_WINDOW_MARGIN_BP`] of the earlier half's
/// magnitude.
///
/// `series` must be in information-time order, oldest first. Fail-closed: fewer
/// than `2 × DECAY_WINDOW_MIN_HALF` points, or an earlier half that summed to
/// exactly zero (no magnitude to be a fraction of), is never a regression.
#[must_use]
pub fn is_window_regression(series: &[i128]) -> bool {
    let n = series.len();
    if n < 2 * DECAY_WINDOW_MIN_HALF as usize {
        return false;
    }
    let half = n / 2;
    let mut early: i128 = 0;
    for v in &series[..half] {
        early = early.saturating_add(*v);
    }
    let mut recent: i128 = 0;
    for v in &series[half..] {
        recent = recent.saturating_add(*v);
    }
    // Compare per-episode means so an odd split does not bias the test.
    let early_mean = early / half as i128;
    let recent_mean = recent / (n - half) as i128;
    let magnitude = early_mean.saturating_abs();
    if magnitude == 0 {
        return false;
    }
    let drop = early_mean.saturating_sub(recent_mean);
    drop.saturating_mul(10_000) >= magnitude.saturating_mul(DECAY_WINDOW_MARGIN_BP)
}

/// Map a brain discovery lane back onto the app's setup [`Lane`] whose weight
/// reflection controls.
///
/// The forward crosswalk ([`crate::brain::discovery_lane_of`]) is many-to-one —
/// `SocialCaller` and `AlphaCall` both present as `SocialCall` — so this reverse
/// map goes to the SETUP lane, which is the level reflection actually reweights
/// and at which the two collapse anyway. `Migration` is unreachable from the app's
/// own lanes today; it is mapped to `GraduationTransition`, the setup lane a
/// migration sighting would present as, so a restored journal from a future
/// ingest path lands somewhere defensible rather than nowhere.
#[must_use]
pub const fn setup_lane_of(l: BrainLane) -> Lane {
    match l {
        BrainLane::NewMint | BrainLane::SocialCall => Lane::CreationSniper,
        BrainLane::Watchlist => Lane::EarlyConfirmation,
        BrainLane::WhaleFollow | BrainLane::Migration => Lane::GraduationTransition,
        BrainLane::Rescan => Lane::ActiveMarketScalp,
    }
}

/// LAW B7: which setup lanes' conditioned setup classes have decayed.
///
/// For each setup lane, pool the `Known` conditioned classes whose brain discovery
/// lane maps onto it, and flag the lane when:
///
/// * the pooled sample reaches `decay_min_sample` (**fail-closed**: below the
///   floor there is no flag, not a weak one), AND
/// * the sample-weighted sum of class medians is strictly negative — i.e. the
///   lane's setups, weighted by how much evidence each carries, are bleeding.
///
/// Refusals contribute nothing at all: a class recall declined to speak about is
/// not evidence for decay any more than it is evidence against it.
///
/// The weighted sum is the honest pooling here. Averaging medians unweighted would
/// let a 12-episode class and a 40-episode class vote equally; summing raw episode
/// nets would re-pool exactly the phase/meta/lane partitions §100 exists to keep
/// apart. Weighting each class's own median by its own sample keeps the partition
/// and still respects evidence mass.
#[must_use]
pub fn lane_decay(classes: &[ConditionedClass], decay_min_sample: u32) -> LaneDecay {
    let mut n_by_lane = [0u64; Lane::COUNT];
    let mut weighted = [0i128; Lane::COUNT];
    for c in classes {
        let Some(stats) = c.verdict.stats() else {
            continue;
        };
        let idx = setup_lane_of(c.discovery_lane).index();
        n_by_lane[idx] = n_by_lane[idx].saturating_add(u64::from(stats.n_matched));
        weighted[idx] = weighted[idx].saturating_add(
            stats
                .median_net_lamports
                .saturating_mul(i128::from(stats.n_matched)),
        );
    }
    let mut out = LaneDecay::none();
    for lane in Lane::ALL {
        let idx = lane.index();
        if n_by_lane[idx] < u64::from(decay_min_sample) {
            continue; // §46 fail-closed.
        }
        if weighted[idx] < 0 {
            out.set(lane);
        }
    }
    out
}

/// Build the §56 retirement-review nominations.
///
/// Four subject families, each fail-closed at `decay_min_sample`:
///
/// * **setup_class** — a conditioned class that is conditioned-negative.
/// * **archetype** — a style lens whose phase-separated realized performance is
///   conditioned-negative.
/// * **lane** — an INDEPENDENT discovery lane (§71.2) whose realized aggregate is
///   negative over enough trades. Discovery lanes are flagged on their own ledger
///   rather than on pooled recall, because §71.2 exists precisely so a paid alpha
///   room is judged separately from the open social firehose.
/// * **source** — a paid/curated alpha source whose attributed realized net is
///   negative.
///
/// Sorted worst-realized-net first, then by `(subject, key)` — total, so the cap
/// keeps the most damaging nominations and the order never depends on iteration.
#[must_use]
pub fn retirement_flags(
    inputs: &AnalysisInputs<'_>,
    classes: &[ConditionedClass],
) -> Vec<RetirementFlag> {
    let floor = inputs.decay_min_sample;
    let mut out: Vec<RetirementFlag> = Vec::new();

    // --- setup classes -----------------------------------------------------
    for c in classes {
        let Some(stats) = c.verdict.stats() else {
            continue; // A refusal is not evidence of decay (§46).
        };
        if !is_conditioned_negative(stats, floor) {
            continue;
        }
        out.push(RetirementFlag {
            subject: FlagSubject::SetupClass,
            key: c.signature.to_string(),
            reason: REASON_CONDITIONED_NEGATIVE,
            n: stats.n_matched,
            realized_net_lamports: narrow_i64(stats.median_net_lamports),
        });
    }

    // --- style lenses ------------------------------------------------------
    let lens_floor = floor.max(ARCHETYPE_MIN_SAMPLE);
    let index = inputs.brain.index();
    for phase in [VenuePhase::Curve, VenuePhase::Pool] {
        for lens in STYLE_LENSES {
            let verdict = archetype_performance(index, lens, phase, lens_floor);
            let Some(stats) = verdict.stats() else {
                continue;
            };
            if !is_conditioned_negative(stats, floor) {
                continue;
            }
            let mut key = String::with_capacity(32);
            key.push_str(lens.name());
            key.push(':');
            key.push_str(venue_phase_name(phase));
            out.push(RetirementFlag {
                subject: FlagSubject::Archetype,
                key,
                reason: REASON_CONDITIONED_NEGATIVE,
                n: stats.n_matched,
                realized_net_lamports: narrow_i64(stats.median_net_lamports),
            });
        }
    }

    // --- independent discovery lanes (§71.2) -------------------------------
    for lane in DiscoveryLane::ALL {
        let trades = inputs.disc_perf.trade_count(lane);
        if trades < u64::from(floor) {
            continue; // §46 fail-closed.
        }
        let net = inputs.disc_perf.net_sol(lane);
        if net >= 0 {
            continue;
        }
        out.push(RetirementFlag {
            subject: FlagSubject::Lane,
            key: discovery_lane_name(lane).to_string(),
            reason: REASON_AGGREGATE_NEGATIVE,
            n: u32::try_from(trades).unwrap_or(u32::MAX),
            realized_net_lamports: net,
        });
    }

    // --- paid / curated alpha sources --------------------------------------
    // The per-source ledger carries a net but no trade count, so the §46 floor is
    // applied to the ONE thing it can be applied to honestly: a source with no
    // realized attribution at all is never nominated, and the reason string says
    // `aggregate_negative` so a reviewer knows the nomination rests on an
    // aggregate rather than on a conditioned distribution.
    for (source, net) in inputs.alpha_source_net.iter().take(ANALYSIS_SOURCE_CAP) {
        if *net >= 0 {
            continue;
        }
        let mut key = String::with_capacity(24);
        key.push_str(source_kind_name(source.kind));
        key.push(':');
        key.push_str(&source.id.to_string());
        out.push(RetirementFlag {
            subject: FlagSubject::Source,
            key,
            reason: REASON_AGGREGATE_NEGATIVE,
            // No per-source trade count exists in this ledger; `0` here means
            // "count not carried by this ledger", and the reason string is what
            // tells the reviewer the evidence shape. It is never used as a sample.
            n: 0,
            realized_net_lamports: *net,
        });
    }

    out.sort_by(|a, b| {
        a.realized_net_lamports
            .cmp(&b.realized_net_lamports)
            .then(a.subject.cmp(&b.subject))
            .then(a.key.cmp(&b.key))
    });
    out.truncate(ANALYSIS_RETIREMENT_CAP);
    out
}

/// The artifact's name for an alpha-source platform kind.
#[must_use]
pub const fn source_kind_name(k: pump_quant_social::types::SourceKind) -> &'static str {
    use pump_quant_social::types::SourceKind as K;
    match k {
        K::X => "x",
        K::TikTok => "tiktok",
        K::Telegram => "telegram",
        K::Web => "web",
        K::Twitch => "twitch",
        K::Pump => "pump",
        K::Aggregator => "aggregator",
        K::Discord => "discord",
    }
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Append a JSON string literal, escaping the two characters that can break the
/// document plus the C0 controls. Every string this module emits today comes from
/// a closed `&'static str` vocabulary, so the escape is defence in depth against a
/// future key that does not (§18: the writer must not be able to emit invalid
/// JSON, whatever it is handed).
fn push_str_json(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u");
                let v = c as u32;
                for shift in [12u32, 8, 4, 0] {
                    let nyb = (v >> shift) & 0xF;
                    out.push(char::from_digit(nyb, 16).unwrap_or('0'));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append `"key":<i64>` or `"key":null`.
fn push_opt_i64(out: &mut String, key: &str, v: Option<i64>) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    match v {
        Some(n) => out.push_str(&n.to_string()),
        None => out.push_str("null"),
    }
}

/// Append `"key":<u64>` or `"key":null`.
fn push_opt_u64(out: &mut String, key: &str, v: Option<u64>) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    match v {
        Some(n) => out.push_str(&n.to_string()),
        None => out.push_str("null"),
    }
}

/// Append `"key":"<s>"` or `"key":null`.
fn push_opt_str(out: &mut String, key: &str, v: Option<&str>) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    match v {
        Some(s) => push_str_json(out, s),
        None => out.push_str("null"),
    }
}

/// Append `"key":<n>` for any integer.
fn push_num(out: &mut String, key: &str, v: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(v);
}

impl BrainAnalysis {
    /// Serialize to the canonical `brain_analysis_v1` JSON: fixed key order,
    /// integer/decimal values only, `null` wherever the brain refused to give a
    /// number. Two runs over the same tape produce the byte-identical string.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one flat hand-rolled writer; splitting it
                                     // would hide the key order the schema IS.
    pub fn to_canonical_json(&self) -> String {
        let mut o = String::with_capacity(4_096);
        o.push_str("{\"record\":\"");
        o.push_str(RECORD_TAG);
        o.push_str("\",\"schema_version\":");
        o.push_str(&SCHEMA_VERSION.to_string());
        o.push_str(",\"info_time_ns\":");
        o.push_str(&self.info_time_ns.to_string());
        o.push_str(",\"tick\":");
        o.push_str(&self.tick.to_string());
        o.push_str(",\"episodes_total\":");
        o.push_str(&self.episodes_total.to_string());
        o.push_str(",\"episodes_admitted\":");
        o.push_str(&self.episodes_admitted.to_string());

        // ---- setup_classes -------------------------------------------------
        o.push_str(",\"setup_classes\":[");
        for (i, c) in self.setup_classes.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"signature\":");
            // A u128 signature exceeds the range a strict JSON consumer will
            // accept as a number, so it travels as a DECIMAL STRING — exact, and
            // never silently rounded by a float-backed parser.
            push_str_json(&mut o, &c.signature.to_string());
            o.push_str(",\"venue_phase\":");
            push_str_json(&mut o, c.venue_phase);
            o.push_str(",\"meta_category\":");
            o.push_str(&c.meta_category.to_string());
            o.push_str(",\"discovery_lane\":");
            push_str_json(&mut o, c.discovery_lane);
            o.push_str(",\"confidence\":");
            push_str_json(
                &mut o,
                if c.stats.is_some() {
                    "known"
                } else {
                    "unknown"
                },
            );
            o.push(',');
            push_opt_str(&mut o, "unknown_reason", c.unknown_reason);
            o.push(',');
            // §46: EVERY estimate field is null on a refusal. There is no path
            // here that reads a number out of an `Unknown` verdict, because the
            // verdict does not have one to read.
            push_opt_u64(&mut o, "n", c.stats.map(|s| u64::from(s.n_matched)));
            o.push(',');
            push_opt_i64(
                &mut o,
                "median_net_lamports",
                c.stats.map(|s| narrow_i64(s.median_net_lamports)),
            );
            o.push(',');
            push_opt_i64(
                &mut o,
                "mean_net_lamports",
                c.stats.map(|s| narrow_i64(s.mean_net_lamports)),
            );
            o.push(',');
            push_opt_u64(
                &mut o,
                "win_rate_bp",
                c.stats.map(|s| u64::from(s.win_rate_bp)),
            );
            o.push(',');
            push_opt_i64(
                &mut o,
                "p25_net_lamports",
                c.stats.map(|s| narrow_i64(s.p25_net_lamports)),
            );
            o.push(',');
            push_opt_i64(
                &mut o,
                "p75_net_lamports",
                c.stats.map(|s| narrow_i64(s.p75_net_lamports)),
            );
            o.push(',');
            push_opt_u64(&mut o, "median_hold_ns", c.stats.map(|s| s.median_hold_ns));
            o.push(',');
            push_opt_u64(
                &mut o,
                "nearest_distance",
                c.stats.map(|s| u64::from(s.nearest_distance)),
            );
            o.push('}');
        }
        o.push(']');

        // ---- lens_scoreboard -----------------------------------------------
        o.push_str(",\"lens_scoreboard\":[");
        for (i, l) in self.lens_scoreboard.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"lens\":");
            push_str_json(&mut o, l.lens);
            o.push_str(",\"venue_phase\":");
            push_str_json(&mut o, l.venue_phase);
            o.push_str(",\"confidence\":");
            push_str_json(
                &mut o,
                if l.stats.is_some() {
                    "known"
                } else {
                    "unknown"
                },
            );
            o.push(',');
            push_opt_str(&mut o, "unknown_reason", l.unknown_reason);
            o.push(',');
            push_opt_u64(&mut o, "n", l.stats.map(|s| u64::from(s.n_matched)));
            o.push(',');
            push_opt_i64(
                &mut o,
                "median_net_lamports",
                l.stats.map(|s| narrow_i64(s.median_net_lamports)),
            );
            o.push(',');
            push_opt_u64(
                &mut o,
                "win_rate_bp",
                l.stats.map(|s| u64::from(s.win_rate_bp)),
            );
            o.push('}');
        }
        o.push(']');

        // ---- best_paying_lens ----------------------------------------------
        o.push_str(",\"best_paying_lens\":");
        match &self.best_paying_lens {
            None => o.push_str("null"),
            Some(b) => {
                o.push_str("{\"lens\":");
                push_str_json(&mut o, b.lens);
                o.push_str(",\"venue_phase\":");
                push_str_json(&mut o, b.venue_phase);
                o.push_str(",\"median_net_lamports\":");
                o.push_str(&b.median_net_lamports.to_string());
                o.push_str(",\"n\":");
                o.push_str(&b.n.to_string());
                o.push('}');
            }
        }

        // ---- meta_state -----------------------------------------------------
        o.push_str(",\"meta_state\":[");
        for (i, m) in self.meta_state.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"meta_category\":");
            o.push_str(&m.meta_category.to_string());
            o.push_str(",\"phase\":");
            push_str_json(&mut o, m.phase);
            o.push_str(",\"n\":");
            o.push_str(&m.n.to_string());
            o.push(',');
            push_num(
                &mut o,
                "participation_decline_bp",
                &m.participation_decline_bp.to_string(),
            );
            o.push(',');
            push_num(
                &mut o,
                "outcome_decline_bp",
                &m.outcome_decline_bp.to_string(),
            );
            o.push('}');
        }
        o.push(']');

        // ---- past_meta_matches ----------------------------------------------
        o.push_str(",\"past_meta_matches\":[");
        for (i, p) in self.past_meta_matches.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"current_meta\":");
            o.push_str(&p.current_meta.to_string());
            o.push_str(",\"past_meta\":");
            o.push_str(&p.past_meta.to_string());
            o.push_str(",\"distance\":");
            o.push_str(&p.distance.to_string());
            o.push_str(",\"past_realized_net_lamports\":");
            o.push_str(&p.past_realized_net_lamports.to_string());
            o.push_str(",\"n\":");
            o.push_str(&p.n.to_string());
            o.push('}');
        }
        o.push(']');

        // ---- caller_trust ----------------------------------------------------
        o.push_str(",\"caller_trust\":[");
        for (i, t) in self.caller_trust.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"author_id\":");
            o.push_str(&t.author_id.to_string());
            o.push(',');
            push_opt_str(&mut o, "platform", t.platform);
            o.push_str(",\"tier\":");
            push_str_json(&mut o, t.tier);
            o.push(',');
            push_opt_i64(&mut o, "score_bp", t.score_bp.map(i64::from));
            o.push(',');
            push_opt_u64(&mut o, "n_markouts", t.n_markouts.map(u64::from));
            o.push_str(",\"exposure\":");
            push_str_json(&mut o, t.exposure);
            o.push('}');
        }
        o.push(']');

        // ---- follow_recommendations ------------------------------------------
        o.push_str(",\"follow_recommendations\":[");
        for (i, f) in self.follow_recommendations.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"author_id\":");
            o.push_str(&f.author_id.to_string());
            o.push_str(",\"platform\":");
            push_str_json(&mut o, f.platform);
            o.push_str(",\"n_calls\":");
            o.push_str(&f.n_calls.to_string());
            o.push_str(",\"realized_net_attributed\":");
            o.push_str(&f.realized_net_attributed.to_string());
            o.push_str(",\"median_lead_ns\":");
            o.push_str(&f.median_lead_ns.to_string());
            o.push_str(",\"trust_tier\":");
            push_str_json(&mut o, f.trust_tier);
            o.push('}');
        }
        o.push(']');

        // ---- unfollow_candidates ---------------------------------------------
        o.push_str(",\"unfollow_candidates\":[");
        for (i, u) in self.unfollow_candidates.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"author_id\":");
            o.push_str(&u.author_id.to_string());
            o.push_str(",\"platform\":");
            push_str_json(&mut o, u.platform);
            o.push_str(",\"realized_net_attributed\":");
            o.push_str(&u.realized_net_attributed.to_string());
            o.push_str(",\"n_calls\":");
            o.push_str(&u.n_calls.to_string());
            o.push('}');
        }
        o.push(']');

        // ---- support_inputs_needed -------------------------------------------
        o.push_str(",\"support_inputs_needed\":[");
        for (i, n) in self.support_inputs_needed.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"kind\":");
            push_str_json(&mut o, n.kind);
            o.push(',');
            push_opt_str(&mut o, "platform", n.platform);
            o.push(',');
            push_opt_u64(&mut o, "author_id", n.author_id);
            o.push(',');
            push_opt_u64(&mut o, "mint_id", n.mint_id);
            o.push('}');
        }
        o.push(']');

        // ---- retirement_flags -------------------------------------------------
        o.push_str(",\"retirement_flags\":[");
        for (i, f) in self.retirement_flags.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str("{\"subject\":");
            push_str_json(&mut o, f.subject.name());
            o.push_str(",\"key\":");
            push_str_json(&mut o, &f.key);
            o.push_str(",\"reason\":");
            push_str_json(&mut o, f.reason);
            o.push_str(",\"n\":");
            o.push_str(&f.n.to_string());
            o.push_str(",\"realized_net_lamports\":");
            o.push_str(&f.realized_net_lamports.to_string());
            o.push('}');
        }
        o.push_str("]}");
        o
    }

    /// Write the canonical JSON to `path`, creating parent directories as needed.
    /// Mirrors [`crate::live_status::LiveStatus::write_to_path`]: write, flush, and
    /// surface any partial write as an `Err`.
    ///
    /// # Errors
    /// Propagates any filesystem error from directory creation, create or write.
    pub fn write_to_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut f = std::fs::File::create(path)?;
        f.write_all(self.to_canonical_json().as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrowing_saturates_rather_than_wrapping() {
        assert_eq!(narrow_i64(5), 5);
        assert_eq!(narrow_i64(i128::from(i64::MAX) + 1), i64::MAX);
        assert_eq!(narrow_i64(i128::from(i64::MIN) - 1), i64::MIN);
    }

    #[test]
    fn window_regression_fails_closed_on_a_thin_series() {
        // Below 2 × DECAY_WINDOW_MIN_HALF points: never a regression, however bad.
        let thin: Vec<i128> = vec![100, 100, 100, -900, -900, -900];
        assert!(thin.len() < 2 * DECAY_WINDOW_MIN_HALF as usize);
        assert!(!is_window_regression(&thin));
    }

    #[test]
    fn window_regression_detects_a_real_deterioration_only() {
        let mut collapsing: Vec<i128> = vec![1_000; 6];
        collapsing.extend(std::iter::repeat_n(-1_000i128, 6));
        assert!(is_window_regression(&collapsing));

        // Flat: no regression.
        let flat: Vec<i128> = vec![1_000; 12];
        assert!(!is_window_regression(&flat));

        // A 10% deterioration is below the 25% margin: not a regression.
        let mut mild: Vec<i128> = vec![1_000; 6];
        mild.extend(std::iter::repeat_n(900i128, 6));
        assert!(!is_window_regression(&mild));

        // An earlier window that summed to zero has no magnitude to be a
        // fraction of: fail closed.
        let zero_base: Vec<i128> = vec![0, 0, 0, 0, 0, 0, -5, -5, -5, -5, -5, -5];
        assert!(!is_window_regression(&zero_base));
    }

    #[test]
    fn conditioned_negative_needs_both_median_and_mean() {
        let base = RecallStats {
            n_matched: 20,
            median_net_lamports: -10,
            mean_net_lamports: -10,
            win_count: 1,
            loss_count: 19,
            win_rate_bp: 500,
            p25_net_lamports: -20,
            p75_net_lamports: -1,
            median_hold_ns: 1,
            nearest_distance: 0,
            nearest_weighted_distance: 0,
            nearest_episode_id: 1,
        };
        assert!(is_conditioned_negative(&base, 12));
        // Below the floor: fail closed.
        assert!(!is_conditioned_negative(&base, 21));
        // Lottery shape (negative median, positive mean): the exit ladder's
        // problem, not the retirement review's.
        let lottery = RecallStats {
            mean_net_lamports: 5_000,
            ..base
        };
        assert!(!is_conditioned_negative(&lottery, 12));
        // Fat-left-tail shape: also not an existence problem.
        let fat_tail = RecallStats {
            median_net_lamports: 100,
            ..base
        };
        assert!(!is_conditioned_negative(&fat_tail, 12));
    }

    #[test]
    fn json_string_escaping_cannot_break_the_document() {
        let mut s = String::new();
        push_str_json(&mut s, "a\"b\\c\nd\u{1}");
        assert_eq!(s, "\"a\\\"b\\\\c\\nd\\u0001\"");
    }

    #[test]
    fn an_empty_analysis_is_still_a_valid_bounded_record() {
        let a = BrainAnalysis {
            info_time_ns: 7,
            tick: 3,
            episodes_total: 0,
            episodes_admitted: 0,
            setup_classes: Vec::new(),
            lens_scoreboard: Vec::new(),
            best_paying_lens: None,
            meta_state: Vec::new(),
            past_meta_matches: Vec::new(),
            caller_trust: Vec::new(),
            follow_recommendations: Vec::new(),
            unfollow_candidates: Vec::new(),
            support_inputs_needed: Vec::new(),
            retirement_flags: Vec::new(),
        };
        let j = a.to_canonical_json();
        assert!(j.starts_with("{\"record\":\"brain_analysis_v1\",\"schema_version\":1,"));
        assert!(j.ends_with("\"retirement_flags\":[]}"));
        assert!(j.contains("\"best_paying_lens\":null"));
        // No float syntax anywhere: no decimal point, no exponent form.
        assert!(!j.contains('.'));
        assert!(!j.contains("e+") && !j.contains("E+") && !j.contains("e-"));
    }
}
