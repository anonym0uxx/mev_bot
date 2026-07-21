//! Attention-velocity narrative leaves (constitution §29 / §21.4 / §46 / §783).
//!
//! All ratios and scores are fixed-point over [`FP_ONE`] (1.0 == 10_000). No
//! floating point is used or produced. Overflow-prone steps widen to 128-bit
//! and saturate back by contract; every such site is annotated.

/// Fixed-point scale: `FP_ONE` fixed-point units represent the real value 1.0.
///
/// Responsibility: single quantization unit for every ratio/score the engine
/// emits, satisfying the §22 no-float mandate at the feature boundary.
pub const FP_ONE: u64 = 10_000;

/// Saturating narrowing of an `i128` difference back into `i64` by contract.
///
/// Private helper (no test touches it). Overflow policy (§22): values outside
/// `i64` clamp to `i64::MIN`/`i64::MAX` rather than wrapping.
fn sat_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// Saturating narrowing of a `u128` into `u64` by contract.
///
/// Private helper. Overflow policy (§22): values above `u64::MAX` clamp.
fn sat_u64(v: u128) -> u64 {
    if v > u64::MAX as u128 {
        u64::MAX
    } else {
        v as u64
    }
}

/// Integer difference `a - b` of two `u64` samples, saturating into `i64`.
///
/// Private helper. Widens to `i128` so the subtraction cannot wrap, then
/// clamps (§22 explicit overflow).
fn diff_i64(a: u64, b: u64) -> i64 {
    sat_i64(a as i128 - b as i128)
}

// ---------------------------------------------------------------------------
// Leaf 1: nv_attention_series — level / velocity / acceleration
// ---------------------------------------------------------------------------

/// Attention level, velocity, and acceleration over integer windows.
///
/// Responsibility: the first/second discrete derivatives of an attention-level
/// series, mirroring `AttentionState.engagement_velocity` /
/// `engagement_acceleration` (§29.6) but computed in pure integers on the
/// deterministic side of the quantization boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionSeries {
    /// Current attention level: the newest sample.
    pub level: u64,
    /// Velocity: `level(t) - level(t - window)` (saturating `i64`).
    pub velocity: i64,
    /// Acceleration: `velocity(t) - velocity(t - window)` (saturating `i64`).
    pub acceleration: i64,
}

/// Compute level/velocity/acceleration of `samples` over an integer `window`.
///
/// Responsibility (§29.6 `AttentionStateReducer` derivatives): given attention
/// levels oldest→newest and a lookback `window` (in samples), return the newest
/// level, the first difference over the window (velocity), and the second
/// difference (acceleration).
///
/// Returns `None` when the input is insufficient (`window == 0`, or fewer than
/// `2*window + 1` samples), or when `2*window + 1` overflows `usize` — absence
/// is valid (§29.5), never fabricated. Deterministic and allocation-free.
pub fn nv_attention_series(samples: &[u64], window: usize) -> Option<AttentionSeries> {
    if window == 0 {
        return None;
    }
    // Explicit overflow (§22): guard the index arithmetic before indexing.
    let need = window.checked_mul(2).and_then(|x| x.checked_add(1))?;
    let n = samples.len();
    if n < need {
        return None;
    }
    let last = samples[n - 1];
    let mid = samples[n - 1 - window];
    let first = samples[n - 1 - 2 * window];
    let velocity = diff_i64(last, mid);
    let velocity_prev = diff_i64(mid, first);
    // i128 subtraction then saturating narrow (§22).
    let acceleration = sat_i64(velocity as i128 - velocity_prev as i128);
    Some(AttentionSeries {
        level: last,
        velocity,
        acceleration,
    })
}

// ---------------------------------------------------------------------------
// Leaf 2: nv_virality_coeff — branching factor of attention
// ---------------------------------------------------------------------------

/// Virality coefficient: new mentions generated per prior-active mention.
///
/// Responsibility (§29.7 amplification-graph / branching factor): a fixed-point
/// reproduction number `new_mentions / prior_active`, scaled by [`FP_ONE`].
/// A result `> FP_ONE` means each active mention is spawning more than one new
/// mention (super-critical spread); `< FP_ONE` means the cascade is dying.
///
/// Returns `None` when `prior_active == 0` (undefined, not zero — §29.5).
/// Overflow policy (§22): the numerator widens to `u128`; the final value
/// saturates into `u64`.
pub fn nv_virality_coeff(prior_active: u64, new_mentions: u64) -> Option<u64> {
    if prior_active == 0 {
        return None;
    }
    // u128 numerator cannot overflow for u64 inputs; divide then saturate.
    let scaled = new_mentions as u128 * FP_ONE as u128 / prior_active as u128;
    Some(sat_u64(scaled))
}

// ---------------------------------------------------------------------------
// Leaf 3: nv_attention_money_divergence
// ---------------------------------------------------------------------------

/// Relationship between attention velocity and on-chain money velocity.
///
/// Responsibility (§29.6 attention-vs-flow reconciliation): classify whether
/// attention is leading price/flow (speculative), confirmed by it, lagging it,
/// or saturating. Drives the fade-first / corroboration logic downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionMoneyDivergence {
    /// Attention rising, money flat/absent — early & unconfirmed.
    AttentionLeads,
    /// Both attention and money rising together — corroborated.
    Confirmed,
    /// Money rising, attention flat — flow without narrative (often late).
    MoneyLeads,
    /// Neither rising above the deadband — saturating/decaying.
    Saturating,
}

/// Classify attention-vs-money divergence given both velocities and a deadband.
///
/// Responsibility: map the sign/magnitude of attention and money velocities to
/// [`AttentionMoneyDivergence`]. A velocity counts as "rising" only when it
/// strictly exceeds `threshold` (`threshold >= 0`, a symmetric deadband that
/// suppresses noise). Pure, total, deterministic.
pub fn nv_attention_money_divergence(
    attn_velocity: i64,
    money_velocity: i64,
    threshold: i64,
) -> AttentionMoneyDivergence {
    let attn_up = attn_velocity > threshold;
    let money_up = money_velocity > threshold;
    match (attn_up, money_up) {
        (true, true) => AttentionMoneyDivergence::Confirmed,
        (true, false) => AttentionMoneyDivergence::AttentionLeads,
        (false, true) => AttentionMoneyDivergence::MoneyLeads,
        (false, false) => AttentionMoneyDivergence::Saturating,
    }
}

// ---------------------------------------------------------------------------
// Leaf 4: nv_lifecycle_stage
// ---------------------------------------------------------------------------

/// Narrative lifecycle stage of a single token's attention.
///
/// Responsibility (§29.6 "new / accelerating / saturated / decaying attention"
/// as an ordered lifecycle): where the narrative sits on its arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStage {
    /// Below the attention floor — still forming, no sustained signal.
    Formation,
    /// Accelerating attention above the floor — the emergence window.
    Emergence,
    /// Super-critical spread (virality coefficient ≥ 1.0) still accelerating.
    Virality,
    /// Above the floor but decelerating — attention saturating.
    Saturation,
    /// Attention velocity negative — the narrative is decaying.
    Decay,
}

/// Classify the lifecycle stage from a series and virality coefficient.
///
/// Responsibility: deterministic staged classifier. Ordered rules (first match
/// wins), so every stage is reachable and the mapping is total:
/// 1. `velocity < 0` → [`LifecycleStage::Decay`].
/// 2. `level < formation_level` → [`LifecycleStage::Formation`] (below floor).
/// 3. `virality_coeff_fp >= FP_ONE` and `acceleration >= 0` →
///    [`LifecycleStage::Virality`] (super-critical, still building).
/// 4. `acceleration > 0` → [`LifecycleStage::Emergence`].
/// 5. otherwise (`acceleration <= 0`) → [`LifecycleStage::Saturation`].
///
/// `virality_coeff_fp` is the fixed-point output of [`nv_virality_coeff`]
/// (use `0` when undefined, which cannot select Virality). Pure/total.
pub fn nv_lifecycle_stage(
    series: &AttentionSeries,
    virality_coeff_fp: u64,
    formation_level: u64,
) -> LifecycleStage {
    if series.velocity < 0 {
        return LifecycleStage::Decay;
    }
    if series.level < formation_level {
        return LifecycleStage::Formation;
    }
    if virality_coeff_fp >= FP_ONE && series.acceleration >= 0 {
        return LifecycleStage::Virality;
    }
    if series.acceleration > 0 {
        return LifecycleStage::Emergence;
    }
    LifecycleStage::Saturation
}

// ---------------------------------------------------------------------------
// Leaf 5: nv_pre_legibility
// ---------------------------------------------------------------------------

/// Pre-legibility score in `[0, FP_ONE]` — earliness before public trackers.
///
/// Responsibility (§783 one-step-ahead doctrine, pre-legibility preference):
/// the edge is being early to identification, before aggregators/terminals make
/// the narrative legible; being a late follower is the trap. High score ⇒ young,
/// broad-forming, not-yet-listed narrative.
///
/// Formula (all fixed-point over [`FP_ONE`]):
/// * `aggregator_listed` ⇒ `0` (already public/legible — no edge, §783 trap).
/// * `unique_sources == 0` ⇒ `0` (no signal; unknown stays unknown, §29.5).
/// * else `raw = FP_ONE - min(FP_ONE, narrative_age_windows * age_step_fp)`
///   (older ⇒ lower), discounted by concentration:
///   `score = raw * (FP_ONE - min(source_concentration_fp, FP_ONE)) / FP_ONE`
///   (single-source echo ⇒ discounted, breadth ⇒ preserved).
///
/// `source_concentration_fp` is the fraction of mentions from the top source
/// in `[0, FP_ONE]` (`FP_ONE` == fully concentrated). Result is monotonic:
/// decreasing in age and in concentration. Overflow policy (§22): saturating.
pub fn nv_pre_legibility(
    unique_sources: u32,
    source_concentration_fp: u64,
    narrative_age_windows: u32,
    aggregator_listed: bool,
    age_step_fp: u64,
) -> u64 {
    if aggregator_listed || unique_sources == 0 {
        return 0;
    }
    // Saturating age penalty (u128 product then clamp to FP_ONE).
    let age_penalty = sat_u64(narrative_age_windows as u128 * age_step_fp as u128).min(FP_ONE);
    let raw = FP_ONE - age_penalty; // age_penalty <= FP_ONE, cannot underflow.
    let conc = source_concentration_fp.min(FP_ONE);
    let genuine = FP_ONE - conc; // conc <= FP_ONE, cannot underflow.
                                 // Fixed-point multiply (u128 intermediate), then narrow.
    sat_u64(raw as u128 * genuine as u128 / FP_ONE as u128)
}

// ---------------------------------------------------------------------------
// Leaf 6: nv_meta_emergence — category-level emergence (§21.4)
// ---------------------------------------------------------------------------

/// Category-level (not per-token) narrative-emergence readout.
///
/// Responsibility (§21.4 `MetaRotationState`; §29.7(c) meta-emergence): flow
/// rotates by narrative category, so emergence is a breadth phenomenon across
/// many tokens in a category, never a single token. This aggregates per-token
/// attention velocities into a category signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaEmergence {
    /// Count of tokens whose velocity strictly exceeds the accel threshold.
    pub accelerating_tokens: u32,
    /// Saturating sum of all supplied token velocities.
    pub category_velocity: i64,
    /// Emerging iff `accelerating_tokens >= min_breadth` and net velocity > 0.
    pub emerging: bool,
}

/// Aggregate per-token attention velocities into a category emergence signal.
///
/// Responsibility (§21.4): count how many tokens in the category are
/// accelerating (velocity `> accel_threshold`), sum the category-wide velocity
/// (saturating, §22), and declare emergence only when the accelerating breadth
/// meets `min_breadth` *and* net category velocity is positive — enforcing the
/// "category-level, never per-token entry" rule (§29.7(c)). Pure/total; folds
/// over a caller-owned slice, so memory is bounded by the caller.
pub fn nv_meta_emergence(
    token_velocities: &[i64],
    accel_threshold: i64,
    min_breadth: u32,
) -> MetaEmergence {
    let mut accelerating: u32 = 0;
    let mut sum: i128 = 0;
    for &v in token_velocities {
        if v > accel_threshold {
            accelerating = accelerating.saturating_add(1);
        }
        sum += v as i128; // i128 accumulator cannot overflow for realistic n.
    }
    let category_velocity = sat_i64(sum);
    let emerging = accelerating >= min_breadth && category_velocity > 0;
    MetaEmergence {
        accelerating_tokens: accelerating,
        category_velocity,
        emerging,
    }
}

// ---------------------------------------------------------------------------
// Leaf 7: nv_candidate_score — corroboration-tier, fade-first composite
// ---------------------------------------------------------------------------

/// Composite narrative candidate score in `[0, 1000]`.
///
/// Responsibility: fuse the narrative leaves into one corroboration-tier score.
/// Component point ceilings sum to exactly 1000:
/// * lifecycle stage — Emergence 350, Virality 300, Saturation/Formation 100,
///   Decay 0 (earliest sustainable stage scores highest);
/// * attention-money divergence — Confirmed 300, AttentionLeads 200,
///   MoneyLeads 120, Saturating 0;
/// * virality — up to 200, `min(200, coeff_fp * 200 / (2*FP_ONE))` (a
///   coefficient of 2.0 saturates the band);
/// * pre-legibility — up to 150, `pre_legibility_fp * 150 / FP_ONE`.
///
/// Fade-first / corroboration-tier law: if `money_confirmed` is false the score
/// is hard-capped at 500 — narrative alone can never dominate, and never
/// authorizes a trade on its own. Deterministic; overflow-safe (§22, u128
/// intermediates, all sub-scores bounded by construction).
pub fn nv_candidate_score(
    stage: LifecycleStage,
    divergence: AttentionMoneyDivergence,
    virality_coeff_fp: u64,
    pre_legibility_fp: u64,
    money_confirmed: bool,
) -> u64 {
    let stage_pts: u64 = match stage {
        LifecycleStage::Emergence => 350,
        LifecycleStage::Virality => 300,
        LifecycleStage::Formation => 100,
        LifecycleStage::Saturation => 100,
        LifecycleStage::Decay => 0,
    };
    let divergence_pts: u64 = match divergence {
        AttentionMoneyDivergence::Confirmed => 300,
        AttentionMoneyDivergence::AttentionLeads => 200,
        AttentionMoneyDivergence::MoneyLeads => 120,
        AttentionMoneyDivergence::Saturating => 0,
    };
    // Virality band: coeff of 2.0 (== 2*FP_ONE) or more saturates 200 points.
    let virality_pts = sat_u64(virality_coeff_fp as u128 * 200 / (2 * FP_ONE as u128)).min(200);
    // Pre-legibility band: FP_ONE saturates 150 points.
    let prelegibility_pts = sat_u64(pre_legibility_fp.min(FP_ONE) as u128 * 150 / FP_ONE as u128);
    let raw = stage_pts + divergence_pts + virality_pts + prelegibility_pts; // <= 1000.
    if money_confirmed {
        raw
    } else {
        raw.min(500) // fade-first hard cap.
    }
}

// ---------------------------------------------------------------------------
// Leaf 8: nv_class_classify — narrative class
// ---------------------------------------------------------------------------

/// Narrative class of a token's story.
///
/// Responsibility (§29.6 narrative category; §21.4 rotation categories): the
/// qualitative kind of narrative, which governs decay speed and ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrativeClass {
    /// Rotational meme trend — fast, medium ceiling (default).
    Trend,
    /// Event/news-driven — sharp spike, mainstream-led, fast decay.
    News,
    /// Tech/utility narrative — broad sources, low spike, long-lived.
    Tech,
    /// Culture/community narrative — sustained, community-driven, high ceiling.
    Culture,
}

/// Feature vector for [`nv_class_classify`].
///
/// Responsibility: the deterministic inputs distinguishing narrative classes,
/// all integer/fixed-point (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassFeatures {
    /// Spike ratio (peak-window / baseline-window attention), fixed-point.
    pub spike_ratio_fp: u64,
    /// Whether a mainstream (non-crypto) platform led the narrative.
    pub mainstream_led: bool,
    /// Longevity: number of windows the narrative has persisted.
    pub longevity_windows: u32,
    /// Source breadth: count of independent sources.
    pub source_breadth: u32,
}

/// Classify the narrative class from its feature vector.
///
/// Responsibility: deterministic ordered classifier (first match wins, total):
/// 1. mainstream-led **and** `spike_ratio_fp >= spike_threshold_fp` →
///    [`NarrativeClass::News`] (event shock from outside crypto).
/// 2. `longevity_windows >= long_threshold` **and**
///    `source_breadth >= tech_breadth` **and** `spike < spike_threshold` →
///    [`NarrativeClass::Tech`] (steady broad build, no spike).
/// 3. `longevity_windows >= long_threshold` → [`NarrativeClass::Culture`]
///    (durable community narrative).
/// 4. otherwise → [`NarrativeClass::Trend`] (fast rotational default).
///
/// Pure/total; every class is reachable.
pub fn nv_class_classify(
    f: &ClassFeatures,
    spike_threshold_fp: u64,
    long_threshold: u32,
    tech_breadth: u32,
) -> NarrativeClass {
    if f.mainstream_led && f.spike_ratio_fp >= spike_threshold_fp {
        return NarrativeClass::News;
    }
    if f.longevity_windows >= long_threshold
        && f.source_breadth >= tech_breadth
        && f.spike_ratio_fp < spike_threshold_fp
    {
        return NarrativeClass::Tech;
    }
    if f.longevity_windows >= long_threshold {
        return NarrativeClass::Culture;
    }
    NarrativeClass::Trend
}

// ---------------------------------------------------------------------------
// Leaf 9: nv_platform_lead — mainstream→crypto lag (Signal-Horizon Law)
// ---------------------------------------------------------------------------

/// Lead/lag relationship between two platforms' first-mention instants.
///
/// Responsibility (§46 Signal-Horizon Matching Law; §29.7 multi-platform
/// horizon classification): whether mainstream social attention precedes
/// crypto-social pickup, and by how much — the mainstream→crypto lag is the
/// window in which crypto entry can be front-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformLead {
    /// Mainstream led crypto by the given number of time units (the setup).
    MainstreamLeads(u64),
    /// First mentions within `tolerance` of each other.
    Simultaneous,
    /// Crypto-social led mainstream by the given number of time units.
    CryptoLeads(u64),
    /// At least one first-mention instant is unknown (§29.5 — never fabricated).
    NoData,
}

/// Determine the platform lead/lag from two integer first-mention instants.
///
/// Responsibility: with instants on a common monotonic integer clock (smaller
/// == earlier), classify the lead into [`PlatformLead`]. A gap within
/// `tolerance` is [`PlatformLead::Simultaneous`]. Either input `None` yields
/// [`PlatformLead::NoData`] — absence stays absence (§29.5), never zero-lag.
/// Overflow policy (§22): the gap widens to `u128` and saturates into `u64`.
pub fn nv_platform_lead(
    mainstream_first: Option<u64>,
    crypto_first: Option<u64>,
    tolerance: u64,
) -> PlatformLead {
    let (m, c) = match (mainstream_first, crypto_first) {
        (Some(m), Some(c)) => (m, c),
        _ => return PlatformLead::NoData,
    };
    if c > m {
        let gap = sat_u64(c as u128 - m as u128);
        if gap <= tolerance {
            PlatformLead::Simultaneous
        } else {
            PlatformLead::MainstreamLeads(gap)
        }
    } else if m > c {
        let gap = sat_u64(m as u128 - c as u128);
        if gap <= tolerance {
            PlatformLead::Simultaneous
        } else {
            PlatformLead::CryptoLeads(gap)
        }
    } else {
        PlatformLead::Simultaneous
    }
}

// ---------------------------------------------------------------------------
// Leaf 10: nv_narrative_ceiling — class-conditioned reach ceiling
// ---------------------------------------------------------------------------

/// Estimated narrative reach ceiling, class-conditioned.
///
/// Responsibility (§29.6 narrative ceiling; §21.4 class-specific ceilings):
/// project a maximum reach from the current reach and the narrative class. Each
/// class carries a fixed-point ceiling multiple (a domain ceiling table, not a
/// hardcoded output — the result scales with the live `current_reach` input):
/// * [`NarrativeClass::News`] — 2.0× (fast, low ceiling, event fades);
/// * [`NarrativeClass::Trend`] — 5.0× (rotational, medium);
/// * [`NarrativeClass::Tech`] — 8.0× (slower, high);
/// * [`NarrativeClass::Culture`] — 12.0× (durable, highest).
///
/// `regime_multiplier_fp` scales the whole ceiling for the market regime
/// (`FP_ONE` == neutral). Result is `current_reach × class_mult ×
/// regime_multiplier`. Overflow policy (§22): u128 intermediates, saturating
/// into `u64`. Deterministic.
pub fn nv_narrative_ceiling(
    class: NarrativeClass,
    current_reach: u64,
    regime_multiplier_fp: u64,
) -> u64 {
    let class_mult_fp: u64 = match class {
        NarrativeClass::News => 2 * FP_ONE,
        NarrativeClass::Trend => 5 * FP_ONE,
        NarrativeClass::Tech => 8 * FP_ONE,
        NarrativeClass::Culture => 12 * FP_ONE,
    };
    // current_reach * class_mult / FP_ONE, then * regime / FP_ONE. u128 keeps
    // the products exact until the final saturating narrow.
    let stage_one = current_reach as u128 * class_mult_fp as u128 / FP_ONE as u128;
    let stage_two = stage_one * regime_multiplier_fp as u128 / FP_ONE as u128;
    sat_u64(stage_two)
}
