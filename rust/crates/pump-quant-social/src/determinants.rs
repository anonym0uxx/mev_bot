//! The ten §29.8 determinant scorers (D1–D10).
//!
//! # Responsibility
//! Each scorer folds decomposed, reconciled, survivorship-free evidence into a single
//! [`DeterminantScore`] (value bps + sample size + confidence), applying §29.8 time
//! decay per sample where a sample carries an age. All integer, all deterministic
//! (§22): callers pass already-measured ages in nanoseconds; nothing reads a clock.
//!
//! Sign convention (uniform): higher `value_bps` = more alpha-favourable.

use crate::fixedpoint::{
    clamp_bps, confidence_bps, decay_weight_bps, markout_bps, ratio_bps, weighted_mean_bps,
    BPS_SCALE,
};
use crate::types::{DeterminantScore, LifecyclePhase};

/// Half-saturation sample counts used for each determinant's confidence curve.
///
/// Static-by-design (§29.8 estimator discipline): these set how many reconciled
/// calls are needed before a determinant is half-trusted. Larger = more sceptical.
pub const CONF_HALF_SAT_MARKOUT: u32 = 20;
/// Half-saturation for the lifecycle/timing determinant.
pub const CONF_HALF_SAT_TIMING: u32 = 15;
/// Half-saturation for the selection-control determinant (the load-bearing one).
pub const CONF_HALF_SAT_SELECTION: u32 = 25;

// ---------------------------------------------------------------------------
// D1 — Reconciled call markouts (ground truth)
// ---------------------------------------------------------------------------

/// One reconciled call's forward prices at the four §29.8 markout horizons plus the
/// call's age (for time decay). Prices are integer market-state units (e.g. lamports
/// per token base unit); their absolute scale is irrelevant, only the ratio matters.
///
/// Deletions are *included* — the caller must supply the full survivorship-free
/// history; this struct carries no "deleted" gate because D1 does not filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkoutSample {
    /// Executable price captured at call time.
    pub price_at_call: u64,
    /// Price +5 minutes later.
    pub price_5m: u64,
    /// Price +30 minutes later.
    pub price_30m: u64,
    /// Price +2 hours later.
    pub price_2h: u64,
    /// Price +24 hours later.
    pub price_24h: u64,
    /// Age of the call in ns (used for time decay).
    pub age_ns: u64,
}

/// **D1 Reconciled call markouts (ground truth, §29.8).**
///
/// For each call, blends the four horizon markouts by `horizon_weights`
/// (`[+5m, +30m, +2h, +24h]`), then decays-and-averages across calls by call age.
/// Returns the decomposed determinant. Empty input → [`DeterminantScore::empty`].
#[must_use]
pub fn d1_reconciled_markouts(
    samples: &[MarkoutSample],
    horizon_weights: [i64; 4],
    half_life_ns: u64,
) -> DeterminantScore {
    if samples.is_empty() {
        return DeterminantScore::empty();
    }
    let mut per_call: Vec<(i64, i64)> = Vec::with_capacity(samples.len());
    for s in samples {
        let horizons = [
            (markout_bps(s.price_at_call, s.price_5m), horizon_weights[0]),
            (
                markout_bps(s.price_at_call, s.price_30m),
                horizon_weights[1],
            ),
            (markout_bps(s.price_at_call, s.price_2h), horizon_weights[2]),
            (
                markout_bps(s.price_at_call, s.price_24h),
                horizon_weights[3],
            ),
        ];
        let blended = weighted_mean_bps(&horizons);
        let decay = decay_weight_bps(s.age_ns, half_life_ns);
        per_call.push((blended, decay));
    }
    DeterminantScore {
        value_bps: weighted_mean_bps(&per_call),
        sample_size: samples.len() as u32,
        confidence_bps: confidence_bps(samples.len() as u32, CONF_HALF_SAT_MARKOUT),
    }
}

// ---------------------------------------------------------------------------
// D2 — Lifecycle timing
// ---------------------------------------------------------------------------

/// One call's lifecycle phase plus its age (for decay). D2 input (§29.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleSample {
    /// The phase the token was in when the call was posted.
    pub phase: LifecyclePhase,
    /// Age of the call in ns.
    pub age_ns: u64,
}

/// Result of D2 with the derived "persistent post-peak" flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D2Result {
    /// The decomposed lifecycle-timing score.
    pub score: DeterminantScore,
    /// True when the decayed share of post-peak calls dominates — exit-liquidity
    /// promotion regardless of tone (§29.8).
    pub post_peak_persistent: bool,
}

/// Per-phase timing value in bps: pre-flow is strongly favourable, with-flow neutral,
/// post-peak strongly adverse. Static-by-design mapping (the sign, not the magnitude,
/// is what D2 asserts).
const PHASE_PREFLOW_BPS: i64 = 8_000;
const PHASE_WITHFLOW_BPS: i64 = 0;
const PHASE_POSTPEAK_BPS: i64 = -8_000;

/// **D2 Lifecycle timing (§29.8).**
///
/// Decay-weights each call's phase value and averages. Also computes the decayed
/// post-peak share; when it exceeds `post_peak_threshold_bps` the source is flagged
/// as a persistent post-peak poster (exit-liquidity promotion).
#[must_use]
pub fn d2_lifecycle_timing(
    samples: &[LifecycleSample],
    half_life_ns: u64,
    post_peak_threshold_bps: i64,
) -> D2Result {
    if samples.is_empty() {
        return D2Result {
            score: DeterminantScore::empty(),
            post_peak_persistent: false,
        };
    }
    let mut pairs: Vec<(i64, i64)> = Vec::with_capacity(samples.len());
    let mut postpeak_pairs: Vec<(i64, i64)> = Vec::with_capacity(samples.len());
    for s in samples {
        let decay = decay_weight_bps(s.age_ns, half_life_ns);
        let v = match s.phase {
            LifecyclePhase::PreFlow => PHASE_PREFLOW_BPS,
            LifecyclePhase::WithFlow => PHASE_WITHFLOW_BPS,
            LifecyclePhase::PostPeak => PHASE_POSTPEAK_BPS,
        };
        pairs.push((v, decay));
        let is_pp = i64::from(s.phase == LifecyclePhase::PostPeak) * BPS_SCALE;
        postpeak_pairs.push((is_pp, decay));
    }
    let postpeak_share = weighted_mean_bps(&postpeak_pairs);
    D2Result {
        score: DeterminantScore {
            value_bps: weighted_mean_bps(&pairs),
            sample_size: samples.len() as u32,
            confidence_bps: confidence_bps(samples.len() as u32, CONF_HALF_SAT_TIMING),
        },
        post_peak_persistent: postpeak_share > post_peak_threshold_bps,
    }
}

// ---------------------------------------------------------------------------
// D3 — State-at-call selection control (mandatory for every ledger claim)
// ---------------------------------------------------------------------------

/// One call's realised markout paired with the matched-control markout (tokens at the
/// same lifecycle state / category / regime *without* the call). D3 input (§29.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionSample {
    /// The reconciled markout the call actually achieved (bps).
    pub call_markout_bps: i64,
    /// The matched-control markout (bps) — what the same state produced anyway.
    pub control_markout_bps: i64,
    /// Age of the call in ns.
    pub age_ns: u64,
}

/// **D3 State-at-call selection control (§29.8, mandatory).**
///
/// Returns the decayed mean *excess* return of calls over their matched controls.
/// An account that only calls already-running coins shows great raw markouts but
/// near-zero excess here — and no source may be rated `PRE_FLOW_ALPHA` without a
/// positive value from this determinant.
#[must_use]
pub fn d3_selection_control(samples: &[SelectionSample], half_life_ns: u64) -> DeterminantScore {
    if samples.is_empty() {
        return DeterminantScore::empty();
    }
    let mut pairs: Vec<(i64, i64)> = Vec::with_capacity(samples.len());
    for s in samples {
        let excess = s.call_markout_bps.saturating_sub(s.control_markout_bps);
        let decay = decay_weight_bps(s.age_ns, half_life_ns);
        pairs.push((excess, decay));
    }
    DeterminantScore {
        value_bps: weighted_mean_bps(&pairs),
        sample_size: samples.len() as u32,
        confidence_bps: confidence_bps(samples.len() as u32, CONF_HALF_SAT_SELECTION),
    }
}

// ---------------------------------------------------------------------------
// D4 — Selectivity
// ---------------------------------------------------------------------------

/// **D4 Selectivity (§29.8).**
///
/// Precision at a fixed call budget, discounted for volume-spam. When calls/day is
/// within `budget_calls_per_day` there is no discount; above it, the hit-rate is
/// scaled by `budget / calls_per_day`, so a spammer's precision is heavily
/// discounted. `calls_per_day_milli` is calls-per-day × 1000 (fixed-point, avoids
/// float). `sample_size` is the number of reconciled calls behind `hit_rate_bps`.
#[must_use]
pub fn d4_selectivity(
    calls_per_day_milli: u64,
    hit_rate_bps: i64,
    budget_calls_per_day: u64,
    sample_size: u32,
) -> DeterminantScore {
    let budget_milli = budget_calls_per_day.saturating_mul(1000);
    let factor = if calls_per_day_milli <= budget_milli || calls_per_day_milli == 0 {
        BPS_SCALE
    } else {
        ratio_bps(budget_milli, calls_per_day_milli)
    };
    let value = clamp_bps(
        clamp_bps(hit_rate_bps)
            .saturating_mul(factor)
            .saturating_div(BPS_SCALE),
    );
    DeterminantScore {
        value_bps: value,
        sample_size,
        confidence_bps: confidence_bps(sample_size, CONF_HALF_SAT_MARKOUT),
    }
}

// ---------------------------------------------------------------------------
// D5 — Skin-in-the-game via wallet-graph join
// ---------------------------------------------------------------------------

/// Wallet-graph evidence joining a source to candidate linked wallets (D5, §29.8).
/// Edge counts are discovered off-path (funding / timing-correlation / metadata-reuse
/// joins with §28) and passed in as already-computed integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkinInGameEvidence {
    /// Funding-edge count linking the source to wallets.
    pub funding_edges: u32,
    /// Timing-correlation-edge count.
    pub timing_edges: u32,
    /// Metadata-reuse-edge count.
    pub metadata_reuse_edges: u32,
    /// Calls where a linked wallet bought *before* the call (accumulation).
    pub buy_before_call: u32,
    /// Calls where a linked wallet distributed *into* the call (dumping).
    pub distribute_into_call: u32,
    /// Total calls with wallet-graph coverage.
    pub total_calls: u32,
}

/// Result of D5 with the derived shill-suspect flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D5Result {
    /// The decomposed skin-in-the-game score (positive = aligned accumulation,
    /// negative = distribute-into-call dumping).
    pub score: DeterminantScore,
    /// True when distribute-into-call share crosses the suspicion threshold —
    /// `PAID_SHILL_SUSPECT` (§29.8: assume undisclosed positions until evidence).
    pub shill_suspect: bool,
}

/// **D5 Skin-in-the-game via wallet-graph join (§29.8, the single most
/// discriminating determinant).**
///
/// Buy-before-call raises the value (aligned accumulation); distribute-into-call
/// lowers it (dumping on followers). `shill_suspect` fires when the decayed
/// distribute-into-call share of covered calls exceeds `shill_threshold_bps`.
#[must_use]
pub fn d5_skin_in_game(ev: &SkinInGameEvidence, shill_threshold_bps: i64) -> D5Result {
    if ev.total_calls == 0 {
        return D5Result {
            score: DeterminantScore::empty(),
            shill_suspect: false,
        };
    }
    let edge_strength = ev
        .funding_edges
        .saturating_add(ev.timing_edges)
        .saturating_add(ev.metadata_reuse_edges);
    // Wallet-graph coverage gives us confidence; edges + coverage bound it.
    let coverage_conf = confidence_bps(ev.total_calls, CONF_HALF_SAT_MARKOUT);
    let buy_share = ratio_bps(u64::from(ev.buy_before_call), u64::from(ev.total_calls));
    let dump_share = ratio_bps(
        u64::from(ev.distribute_into_call),
        u64::from(ev.total_calls),
    );
    // Aligned accumulation is favourable; dumping into calls is adverse and weighs
    // harder (2x) because it is the direct exit-liquidity signature.
    let value = clamp_bps(buy_share.saturating_sub(dump_share.saturating_mul(2)));
    // Having *any* wallet edges at all is itself skin-in-the-game evidence that
    // sharpens confidence; fold a small edge bonus into confidence, not value.
    let edge_bonus = ratio_bps(
        u64::from(edge_strength.min(ev.total_calls)),
        u64::from(ev.total_calls),
    );
    let conf = ((i64::from(coverage_conf) + edge_bonus / 4).clamp(0, BPS_SCALE)) as u16;
    D5Result {
        score: DeterminantScore {
            value_bps: value,
            sample_size: ev.total_calls,
            confidence_bps: conf,
        },
        shill_suspect: dump_share > shill_threshold_bps,
    }
}

// ---------------------------------------------------------------------------
// D6 — Integrity (deletion of losing calls, edits, disclosure)
// ---------------------------------------------------------------------------

/// Deletion / edit / disclosure evidence for D6 integrity (§29.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrityEvidence {
    /// Losing calls that were later deleted (scrubbing the track record).
    pub deleted_losing_calls: u32,
    /// Total losing calls observed (survivorship-free denominator).
    pub total_losing_calls: u32,
    /// Number of post-hoc content edits observed.
    pub edit_count: u32,
    /// Total calls (for confidence).
    pub total_calls: u32,
    /// Whether the source discloses positions.
    pub disclosure_present: bool,
}

/// Bonus/penalty magnitudes for D6, static-by-design safety-style constants.
const DISCLOSURE_BONUS_BPS: i64 = 1_500;
const EDIT_PENALTY_PER_BPS: i64 = 300;
const EDIT_PENALTY_CAP_BPS: i64 = 3_000;

/// **D6 Integrity (§29.8).**
///
/// Starts from neutral and subtracts the share of *losing* calls that were deleted
/// (the strongest integrity violation), subtracts a capped per-edit penalty, and adds
/// a disclosure bonus. Higher = more honest track record.
#[must_use]
pub fn d6_integrity(ev: &IntegrityEvidence) -> DeterminantScore {
    if ev.total_calls == 0 {
        return DeterminantScore::empty();
    }
    let deletion_ratio = ratio_bps(
        u64::from(ev.deleted_losing_calls),
        u64::from(ev.total_losing_calls),
    );
    let edit_penalty =
        (i64::from(ev.edit_count).saturating_mul(EDIT_PENALTY_PER_BPS)).min(EDIT_PENALTY_CAP_BPS);
    let disclosure = if ev.disclosure_present {
        DISCLOSURE_BONUS_BPS
    } else {
        0
    };
    let value = clamp_bps(
        BPS_SCALE
            .saturating_sub(deletion_ratio)
            .saturating_sub(edit_penalty)
            .saturating_add(disclosure)
            // Re-centre so a clean record sits near neutral-positive, not pinned high.
            .saturating_sub(BPS_SCALE / 2),
    );
    DeterminantScore {
        value_bps: value,
        sample_size: ev.total_calls,
        confidence_bps: confidence_bps(ev.total_calls, CONF_HALF_SAT_MARKOUT),
    }
}

// ---------------------------------------------------------------------------
// D7 — Audience authenticity
// ---------------------------------------------------------------------------

/// Audience-authenticity evidence for D7 (§29.8). All fields are already-measured
/// bps ratios or small counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudienceEvidence {
    /// Reply diversity (bps): higher = more distinct repliers.
    pub reply_diversity_bps: i64,
    /// Bot-reply ratio (bps): higher = more bot replies.
    pub bot_reply_ratio_bps: i64,
    /// Count of raid-pattern events (coordinated engagement bursts).
    pub raid_pattern_count: u32,
    /// Semantic copy-echo density in the replies (bps).
    pub copy_echo_density_bps: i64,
    /// Observed engagement velocity (bps of audience size per unit).
    pub engagement_velocity_bps: i64,
    /// Expected engagement velocity for this audience size (bps).
    pub expected_velocity_bps: i64,
    /// Sample size behind the measurement.
    pub sample_size: u32,
}

const RAID_PENALTY_PER_BPS: i64 = 500;
const RAID_PENALTY_CAP_BPS: i64 = 4_000;

/// Result of D7 with the derived engagement-farm flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D7Result {
    /// The decomposed authenticity score (higher = more authentic).
    pub score: DeterminantScore,
    /// True when bot ratio / velocity anomaly dominates — `ENGAGEMENT_FARM`.
    pub bot_farm: bool,
}

/// **D7 Audience authenticity (§29.8).**
///
/// Authenticity rises with reply diversity and falls with bot-reply ratio, raid
/// patterns, copy-echo density, and any excess of engagement velocity over the
/// expected velocity for the audience size (manufactured engagement). `bot_farm`
/// fires when bot ratio exceeds `bot_ratio_threshold_bps` or velocity is more than
/// double expected.
#[must_use]
pub fn d7_audience_authenticity(ev: &AudienceEvidence, bot_ratio_threshold_bps: i64) -> D7Result {
    if ev.sample_size == 0 {
        return D7Result {
            score: DeterminantScore::empty(),
            bot_farm: false,
        };
    }
    let raid_penalty = (i64::from(ev.raid_pattern_count).saturating_mul(RAID_PENALTY_PER_BPS))
        .min(RAID_PENALTY_CAP_BPS);
    let velocity_anomaly = ev
        .engagement_velocity_bps
        .saturating_sub(ev.expected_velocity_bps)
        .max(0);
    let value = clamp_bps(
        ev.reply_diversity_bps
            .saturating_sub(ev.bot_reply_ratio_bps)
            .saturating_sub(ev.copy_echo_density_bps)
            .saturating_sub(raid_penalty)
            .saturating_sub(velocity_anomaly),
    );
    let bot_farm = ev.bot_reply_ratio_bps > bot_ratio_threshold_bps
        || ev.engagement_velocity_bps > ev.expected_velocity_bps.saturating_mul(2);
    D7Result {
        score: DeterminantScore {
            value_bps: value,
            sample_size: ev.sample_size,
            confidence_bps: confidence_bps(ev.sample_size, CONF_HALF_SAT_TIMING),
        },
        bot_farm,
    }
}

// ---------------------------------------------------------------------------
// D8 — Originality and network position
// ---------------------------------------------------------------------------

/// Result of D8 with the derived echo-heavy flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D8Result {
    /// Decomposed originality score = originator share in bps (higher = originator).
    pub score: DeterminantScore,
    /// True when the source is predominantly an echo — `COPY_ECHO_ACCOUNT`.
    pub echo_heavy: bool,
}

/// **D8 Originality and network position (§29.8).**
///
/// Originator share across the amplification graph: `originator / (originator +
/// echo)`. Echo centrality is reach, not alpha, so a high echo count *lowers* the
/// value. `echo_heavy` fires when the originator share is below `echo_threshold_bps`.
#[must_use]
pub fn d8_originality(originator_count: u32, echo_count: u32, echo_threshold_bps: i64) -> D8Result {
    let total = originator_count.saturating_add(echo_count);
    if total == 0 {
        return D8Result {
            score: DeterminantScore::empty(),
            echo_heavy: false,
        };
    }
    let originator_share = ratio_bps(u64::from(originator_count), u64::from(total));
    D8Result {
        score: DeterminantScore {
            value_bps: originator_share,
            sample_size: total,
            confidence_bps: confidence_bps(total, CONF_HALF_SAT_TIMING),
        },
        echo_heavy: originator_share < echo_threshold_bps,
    }
}

// ---------------------------------------------------------------------------
// D9 — Category-conditional skill
// ---------------------------------------------------------------------------

/// Per-meta reconciled performance (D9, §29.8). Most callers have edge, if any, only
/// inside their meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaPerf {
    /// Identifier of the meta / narrative category.
    pub meta_id: u64,
    /// Reconciled markout in that meta (bps).
    pub markout_bps: i64,
    /// Sample size within the meta.
    pub sample_size: u32,
    /// Age of the meta's most recent evidence in ns (for decay).
    pub age_ns: u64,
}

/// **D9 Category-conditional skill (§29.8).**
///
/// Aggregates per-meta reconciled markouts weighted by `sample_size × time-decay`, so
/// a stale or thin meta contributes little. Returns the decayed cross-meta skill
/// value and the total sample.
#[must_use]
pub fn d9_category_skill(perfs: &[MetaPerf], half_life_ns: u64) -> DeterminantScore {
    if perfs.is_empty() {
        return DeterminantScore::empty();
    }
    let mut pairs: Vec<(i64, i64)> = Vec::with_capacity(perfs.len());
    let mut total_sample: u32 = 0;
    for p in perfs {
        let decay = decay_weight_bps(p.age_ns, half_life_ns);
        let weight = clamp_i128_weight(i64::from(p.sample_size), decay);
        pairs.push((p.markout_bps, weight));
        total_sample = total_sample.saturating_add(p.sample_size);
    }
    DeterminantScore {
        value_bps: weighted_mean_bps(&pairs),
        sample_size: total_sample,
        confidence_bps: confidence_bps(total_sample, CONF_HALF_SAT_MARKOUT),
    }
}

/// `sample × decay / BPS_SCALE`, saturating to `i64`. Internal helper for D9 weights.
fn clamp_i128_weight(sample: i64, decay_bps: i64) -> i64 {
    let w = (sample as i128) * (decay_bps as i128) / (BPS_SCALE as i128);
    crate::fixedpoint::clamp_i128_to_i64(w)
}

// ---------------------------------------------------------------------------
// D10 — Call clustering as a distribution signal
// ---------------------------------------------------------------------------

/// **D10 Call clustering (§29.8).**
///
/// Peer-reviewed evidence associates influencer clustering with *steeper subsequent
/// declines*, so convergence of multiple tracked sources on one token is treated as a
/// distribution / saturation signal **by default**: the value is negative and grows
/// more negative with `cluster_size`. It only becomes an entry-favourable value when
/// `admission_proven` is set (an experiment proved otherwise), in which case the
/// admitted markout is used directly.
#[must_use]
pub fn d10_clustering(
    cluster_size: u32,
    penalty_per_source_bps: i64,
    admission_proven: bool,
    admitted_markout_bps: i64,
) -> DeterminantScore {
    if admission_proven {
        return DeterminantScore {
            value_bps: clamp_bps(admitted_markout_bps),
            sample_size: cluster_size.max(1),
            confidence_bps: confidence_bps(cluster_size.max(1), CONF_HALF_SAT_TIMING),
        };
    }
    // Default fade: each additional converging source deepens the distribution
    // presumption. A single source (cluster_size <= 1) is no cluster → 0.
    let extra = cluster_size.saturating_sub(1);
    let penalty = i64::from(extra).saturating_mul(penalty_per_source_bps);
    DeterminantScore {
        value_bps: clamp_bps(-penalty),
        sample_size: cluster_size.max(1),
        confidence_bps: confidence_bps(cluster_size.max(1), CONF_HALF_SAT_TIMING),
    }
}
