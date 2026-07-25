//! **PRE-REGISTERED** adoption test for the schema-2 representation changes:
//! the holder-growth VELOCITY fingerprint dimension and the CONCENTRATION recall
//! conditioner.
//!
//! # Why this file exists, and why it is not a P&L test
//!
//! A similarity-index change is an **information** change, not a strategy change.
//! Judging it by net SOL on a synthetic tape measures the tape's generator, not the
//! index — and the generator was written by the same hand as the field. The brain
//! is decision-inert with the haircut off, so the golden tape's net cannot move at
//! all; a "P&L improvement" here would be a bug, not evidence.
//!
//! The right question is whether the recall estimator got **sharper**: do the
//! classes it forms have tighter realized-outcome distributions, and does it still
//! reach `Known` as often? Those are the two halves of the bias/variance trade a
//! nearest-neighbour estimator lives on, and a field that buys one by destroying the
//! other is a loss.
//!
//! # THE PRE-REGISTERED RULE (written before any number below was measured)
//!
//! ## Dispersion statistic
//!
//! **Within-class dispersion** is the **IQR of realized net lamports inside a recall
//! class**: `RecallStats::p75_net_lamports - p25_net_lamports`, which the estimator
//! already computes by nearest-rank order statistic — integer, no interpolation, no
//! synthesized value. Aggregated across queries by the **median of the per-query
//! IQRs** (again nearest-rank). IQR is chosen over MAD because it is *already* what
//! the estimator publishes, so the statistic measured is literally the number a
//! consumer would read, not a proxy for it.
//!
//! ## Ablation
//!
//! "Without the field" is realised by pinning the velocity bucket of every episode
//! AND of the query to the ladder's neutral rung. A field pinned to a constant
//! contributes exactly `0` to every Hamming distance and exactly `0` to every
//! weighted distance, so the ablated index **ranks identically to schema 1**. That
//! equivalence is asserted, not assumed
//! (`a_pinned_velocity_field_is_exactly_the_schema_one_ranking`).
//!
//! ## LAW V1 — VELOCITY, SIGNAL ARM
//!
//! On a corpus where realized net genuinely depends on holder-growth velocity and
//! velocity is dissociated from acceleration, the aggregate within-class IQR **must
//! fall by at least `DISPERSION_IMPROVEMENT_BAR_BPS` = 2 000 bp (20%)** relative to
//! the ablated index, at identical `min_sample`, `max_distance` and `top_m`.
//!
//! ## LAW V2 — VELOCITY, NULL ARM (the anti-self-deception leg)
//!
//! On a corpus where realized net depends on acceleration ONLY — velocity still
//! dissociated, but pure noise with respect to the outcome — the same statistic
//! **must NOT improve by more than `NULL_ARM_TOLERANCE_BPS` = 2 000 bp**.
//!
//! Without this leg the test is worthless. *Any* extra partitioning axis shrinks
//! classes, and smaller classes have smaller order statistics by sampling alone. V2
//! is what distinguishes "the field carries information" from "the field shatters
//! the corpus".
//!
//! ## LAW V3 — COVERAGE
//!
//! The count of queries reaching `RecallVerdict::Known` **must not fall by more than
//! `COVERAGE_LOSS_BAR_BPS` = 1 000 bp (10%)** relative to the ablated index, on
//! **both** arms. A field that buys precision by pushing the corpus below the sample
//! floor has destroyed recall, which is a loss and not a gain.
//!
//! ## LAW C1/C2/C3 — the CONCENTRATION CONDITIONER
//!
//! Identical structure, applied to `RecallFilter::with_concentration` on the subset
//! of queries that carry a `Known` reading:
//!
//! * **C1** — on a corpus where net depends on the concentration band, the
//!   within-class IQR over the `Known` subset must fall by at least
//!   `DISPERSION_IMPROVEMENT_BAR_BPS`.
//! * **C2** — on a corpus where net does not depend on it, the improvement must not
//!   exceed `NULL_ARM_TOLERANCE_BPS`.
//! * **C3** — the conditioner must be **exactly inert** for an `Unknown` QUERY
//!   (byte-identical verdicts), must only ever narrow, and must **fail closed** at
//!   zero band coverage: a band-pinned query over a corpus that carries no readings
//!   returns a refusal, never the pooled estimate under a band label. Coverage of
//!   the `Known` subset is REPORTED, not barred: it is a property of the world (how
//!   often the holder ledger reaches an `Exact` basis), not of this code, and the
//!   honest thing is to publish it.
//!
//! ## Failure disposition
//!
//! If V1 fails, or V2 fails, or V3 fails, the velocity field is **reverted**. If
//! C1/C2/C3 fail, the conditioner is **reverted**. Stated here so the disposition
//! is not renegotiated after seeing the numbers.
//!
//! # What this CANNOT establish, stated plainly
//!
//! This corpus is synthetic. It establishes a **conditional**: *if* forward net
//! depends on holder-growth velocity, the schema-2 index can see it and schema 1
//! could not. It does **not** establish that holder-growth velocity predicts
//! memecoin returns — no synthetic tape can, and the persisted corpus is empty
//! today. That is the honest boundary of the claim, and the null arm is the only
//! reason the conditional is worth anything at all.

use pump_quant_brain::concentration::{ConcentrationReading, ConcentrationShape};
use pump_quant_brain::episode::{
    DiscoveryLane, Episode, EpisodeContext, EpisodeOutcome, ExitReason,
};
use pump_quant_brain::fingerprint::{
    signature_hamming, unweighted_distance, weighted_distance, FeatureWeights, SetupFingerprint,
    FIELD_COUNT, F_HOLDER_GROWTH_ACCEL, F_HOLDER_GROWTH_VELOCITY, SIGNATURE_BITS,
};
use pump_quant_brain::hash::mix_u32;
use pump_quant_brain::recall::{
    nearest_rank_index, EpisodicIndex, RecallFilter, RecallParams, RecallVerdict, P50,
};

// ---------------------------------------------------------------------------
// PRE-REGISTERED BARS (§102 — named consts, fixed before measurement)
// ---------------------------------------------------------------------------

/// LAW V1/C1: minimum relative fall in aggregate within-class IQR, basis points.
const DISPERSION_IMPROVEMENT_BAR_BPS: i64 = 2_000;

/// LAW V2/C2: maximum tolerated "improvement" on a null corpus, basis points.
const NULL_ARM_TOLERANCE_BPS: i64 = 2_000;

/// LAW V3: maximum tolerated fall in the `Known`-query count, basis points.
const COVERAGE_LOSS_BAR_BPS: i64 = 1_000;

/// Episodes per corpus. `900 = 30 cells x 30`, so every `(velocity, acceleration)`
/// cell clears [`MIN_SAMPLE`] with margin in the full index and every acceleration
/// cell clears it with a large margin in the ablated one.
const CORPUS_N: u64 = 900;

/// Sample floor, held IDENTICAL across every arm (the rule demands equal
/// `min_sample`).
const MIN_SAMPLE: u32 = 8;

/// Similarity radius. **Zero**, deliberately: at radius 0 a "recall class" is
/// exactly one bucket cell, so the full index classes on `(velocity, acceleration)`
/// and the ablated index classes on `acceleration` alone. The ablation is then a
/// strict, exactly-characterised COARSENING of the partition rather than a fuzzy
/// change in neighbourhood shape.
const MAX_DISTANCE: u32 = 0;

/// Stage-2 candidate cap, set above the largest class so truncation never silently
/// becomes the thing being measured.
const TOP_M: usize = 512;

/// Velocity ladder levels.
const VEL_LEVELS: u64 = 6;
/// Acceleration ladder levels.
const ACCEL_LEVELS: u64 = 5;
/// The velocity ladder's neutral rung — where a refusal collapses, and therefore
/// the value the ablation pins.
const VEL_NEUTRAL_BUCKET: u8 = 2;

/// Per-episode outcome noise amplitude, lamports. Bounded and deterministic.
const NOISE_AMPLITUDE: i128 = 400_000;
/// Effect size of the driving variable, lamports per bucket step.
const EFFECT_PER_BUCKET: i128 = 3_000_000;

// ---------------------------------------------------------------------------
// Deterministic corpus construction (no RNG, §22)
// ---------------------------------------------------------------------------

/// Two independent avalanche hashes of the same index, so the velocity and
/// acceleration draws are dissociated by construction rather than by hope.
fn h_vel(i: u64) -> u64 {
    u64::from(mix_u32((i as u32).wrapping_mul(2_654_435_761)))
}
fn h_accel(i: u64) -> u64 {
    u64::from(mix_u32((i as u32).wrapping_add(0x9E37_79B9)))
}
fn h_noise(i: u64) -> u64 {
    u64::from(mix_u32((i as u32) ^ 0x5BF0_3635))
}
fn h_conc(i: u64) -> u64 {
    u64::from(mix_u32((i as u32).wrapping_mul(0x85EB_CA6B) ^ 0x27D4_EB2F))
}

/// Bounded, zero-centred, deterministic noise term.
fn noise(i: u64) -> i128 {
    (i128::from(h_noise(i) % 2_001) - 1_000) * NOISE_AMPLITUDE / 1_000
}

/// Which quantity drives the outcome in a given arm.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Driver {
    /// LAW V1 signal arm: net depends on holder-growth velocity.
    Velocity,
    /// LAW V2 null arm: net depends on acceleration only; velocity is noise.
    Acceleration,
    /// LAW C1 signal arm: net depends on the concentration band.
    ConcentrationBand,
}

/// Whether the velocity field is visible to the index, or pinned to the neutral
/// rung (the schema-1 ablation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VelocityArm {
    Present,
    AblatedToNeutral,
}

/// Fraction of episodes carrying a `Known` concentration reading, in basis points.
/// Swept, because the whole question about a thin-coverage conditioner is how it
/// behaves as coverage falls.
type KnownRateBps = u64;

fn build_episode(
    i: u64,
    driver: Driver,
    arm: VelocityArm,
    known_rate_bps: KnownRateBps,
) -> Episode {
    let vel_bucket = (h_vel(i) % VEL_LEVELS) as u8;
    let accel_bucket = (h_accel(i) % ACCEL_LEVELS) as u8;

    let mut buckets = [0u8; FIELD_COUNT];
    buckets[F_HOLDER_GROWTH_ACCEL] = accel_bucket;
    buckets[F_HOLDER_GROWTH_VELOCITY] = match arm {
        VelocityArm::Present => vel_bucket,
        VelocityArm::AblatedToNeutral => VEL_NEUTRAL_BUCKET,
    };
    let fingerprint = SetupFingerprint::from_buckets(buckets);

    // The parallel stream: a deterministic `known_rate_bps` share of episodes carry
    // a banded reading, the rest carry an explicit refusal.
    let conc_h = h_conc(i);
    let concentration = if conc_h % 10_000 < known_rate_bps {
        let band = ((conc_h / 10_000) % 4) as u8;
        ConcentrationReading::Known(ConcentrationShape::from_bands(band, 0, 0))
    } else {
        ConcentrationReading::Unknown(
            pump_quant_brain::concentration::ConcentrationUnknown::DeltaOnlyBasis,
        )
    };
    let conc_band = concentration
        .shape()
        .map_or(0, ConcentrationShape::top10_band);

    let realized_net_lamports = match driver {
        Driver::Velocity => i128::from(vel_bucket) * EFFECT_PER_BUCKET + noise(i),
        Driver::Acceleration => i128::from(accel_bucket) * EFFECT_PER_BUCKET + noise(i),
        Driver::ConcentrationBand => i128::from(conc_band) * EFFECT_PER_BUCKET + noise(i),
    };

    let (_, concentration_trajectory) = EpisodeContext::disarmed_concentration();
    Episode::new(
        i + 1,
        fingerprint,
        EpisodeContext {
            mint_id: i,
            venue_phase: fingerprint.venue_phase(),
            meta_category_id: 0,
            discovery_lane: DiscoveryLane::NewMint,
            info_time_ns: i * 1_000_000,
            slot: i,
            concentration,
            concentration_trajectory,
        },
        EpisodeOutcome {
            realized_net_lamports,
            hold_duration_ns: 1_000,
            exit_reason: if realized_net_lamports >= 0 {
                ExitReason::TakeProfit
            } else {
                ExitReason::StopLoss
            },
            mfe_bps: 100,
            mae_bps: -50,
            was_admitted: true,
        },
    )
}

fn build_index(driver: Driver, arm: VelocityArm, known_rate_bps: KnownRateBps) -> EpisodicIndex {
    let mut idx = EpisodicIndex::with_capacity(CORPUS_N as usize + 16);
    for i in 0..CORPUS_N {
        idx.push(build_episode(i, driver, arm, known_rate_bps))
            .expect("monotone ids");
    }
    idx
}

/// The engine's PRODUCTION similarity radius
/// (`pump_quant_app::brain::BRAIN_RECALL_MAX_DISTANCE_DEFAULT`). Restated here as a
/// named const because the brain crate cannot depend on the app; the diagnostic
/// below reports coverage at the real operating point as well as at the radius-0
/// worst case.
const MAX_DISTANCE_PRODUCTION: u32 = 8;

fn params_at(max_distance: u32) -> RecallParams {
    RecallParams {
        min_sample: MIN_SAMPLE,
        max_distance,
        top_m: TOP_M,
        weights: FeatureWeights::default(),
        require_admitted: true,
    }
}

fn params() -> RecallParams {
    params_at(MAX_DISTANCE)
}

/// Every query the sweep asks: one per `(velocity, acceleration)` cell.
fn query_grid(arm: VelocityArm) -> Vec<(u8, u8, SetupFingerprint)> {
    let mut out = Vec::new();
    for v in 0..VEL_LEVELS as u8 {
        for a in 0..ACCEL_LEVELS as u8 {
            let mut b = [0u8; FIELD_COUNT];
            b[F_HOLDER_GROWTH_ACCEL] = a;
            b[F_HOLDER_GROWTH_VELOCITY] = match arm {
                VelocityArm::Present => v,
                VelocityArm::AblatedToNeutral => VEL_NEUTRAL_BUCKET,
            };
            out.push((v, a, SetupFingerprint::from_buckets(b)));
        }
    }
    out
}

/// The aggregate statistic: `(median per-query IQR, Known-query count, total
/// queries)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Aggregate {
    median_iqr: i128,
    known: u32,
    total: u32,
}

fn median_i128(v: &mut [i128]) -> i128 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[nearest_rank_index(v.len(), P50)]
}

fn aggregate(idx: &EpisodicIndex, arm: VelocityArm, radius: u32) -> Aggregate {
    let p = params_at(radius);
    let mut iqrs: Vec<i128> = Vec::new();
    let mut known = 0u32;
    let mut total = 0u32;
    for (_v, _a, q) in query_grid(arm) {
        total += 1;
        if let RecallVerdict::Known(s) = idx.recall(&q, &p) {
            known += 1;
            iqrs.push(s.p75_net_lamports - s.p25_net_lamports);
        }
    }
    Aggregate {
        median_iqr: median_i128(&mut iqrs),
        known,
        total,
    }
}

/// Relative change in basis points: `(before - after) * 10_000 / before`. Positive
/// means the statistic FELL (an improvement for dispersion, a loss for coverage).
fn rel_fall_bps(before: i128, after: i128) -> i64 {
    if before == 0 {
        return 0;
    }
    let v = (before - after).saturating_mul(10_000) / before;
    i64::try_from(v).unwrap_or(i64::MAX)
}

// ===========================================================================
// Structural preconditions — the ablation and the dissociation
// ===========================================================================

/// The ablation is EXACT: a velocity field pinned to a constant contributes zero to
/// every distance, so the ablated index ranks identically to a schema-1 index that
/// never had the field. Without this the comparison below measures nothing.
#[test]
fn a_pinned_velocity_field_is_exactly_the_schema_one_ranking() {
    let w = FeatureWeights::default();
    for a in 0..ACCEL_LEVELS as u8 {
        for b in 0..ACCEL_LEVELS as u8 {
            let mut ba = [0u8; FIELD_COUNT];
            ba[F_HOLDER_GROWTH_ACCEL] = a;
            ba[F_HOLDER_GROWTH_VELOCITY] = VEL_NEUTRAL_BUCKET;
            let mut bb = [0u8; FIELD_COUNT];
            bb[F_HOLDER_GROWTH_ACCEL] = b;
            bb[F_HOLDER_GROWTH_VELOCITY] = VEL_NEUTRAL_BUCKET;
            let fa = SetupFingerprint::from_buckets(ba);
            let fb = SetupFingerprint::from_buckets(bb);
            // The distance is exactly the acceleration gap — the velocity field is
            // invisible, which is what "schema 1" means.
            assert_eq!(
                signature_hamming(fa.signature(), fb.signature()),
                u32::from(a.abs_diff(b))
            );
            assert_eq!(unweighted_distance(&fa, &fb), u32::from(a.abs_diff(b)));
            assert_eq!(
                weighted_distance(&fa, &fb, &w),
                u64::from(a.abs_diff(b)) * u64::from(w.w[F_HOLDER_GROWTH_ACCEL])
            );
        }
    }
    // And the bit budget the field cost.
    assert_eq!(SIGNATURE_BITS, 104);
}

/// The corpus must genuinely DISSOCIATE velocity from acceleration, or LAW V1 would
/// be measuring acceleration wearing a velocity label.
#[test]
fn the_corpus_dissociates_velocity_from_acceleration() {
    let mut cell = [[0u32; ACCEL_LEVELS as usize]; VEL_LEVELS as usize];
    let mut vel_count = [0u32; VEL_LEVELS as usize];
    let mut accel_sum_by_vel = [0u64; VEL_LEVELS as usize];
    for i in 0..CORPUS_N {
        let v = (h_vel(i) % VEL_LEVELS) as usize;
        let a = (h_accel(i) % ACCEL_LEVELS) as usize;
        cell[v][a] += 1;
        vel_count[v] += 1;
        accel_sum_by_vel[v] += a as u64;
    }
    // Every joint cell is populated well past the sample floor.
    for (v, row) in cell.iter().enumerate() {
        for (a, n) in row.iter().enumerate() {
            assert!(
                *n >= MIN_SAMPLE,
                "cell (vel {v}, accel {a}) has only {n} episodes"
            );
        }
    }
    // Mean acceleration is flat across velocity levels: velocity carries no
    // acceleration information. Integer test — mean acceleration x1000 per
    // velocity level must sit inside +/- 15% of the overall mean.
    let total: u64 = accel_sum_by_vel.iter().sum();
    let n: u64 = vel_count.iter().map(|c| u64::from(*c)).sum();
    let overall = total * 1_000 / n;
    for v in 0..VEL_LEVELS as usize {
        let m = accel_sum_by_vel[v] * 1_000 / u64::from(vel_count[v]);
        let dev = m.abs_diff(overall) * 10_000 / overall;
        println!("DISSOCIATION vel={v} mean_accel_x1000={m} overall={overall} dev_bps={dev}");
        assert!(
            dev <= 1_500,
            "velocity level {v} biases acceleration by {dev} bp"
        );
    }
}

// ===========================================================================
// LAW V1 / V2 / V3 — the velocity field
// ===========================================================================

#[test]
fn law_v1_v2_v3_velocity_field_adoption() {
    // --- SIGNAL ARM (LAW V1) --------------------------------------------
    let sig_ablated = build_index(Driver::Velocity, VelocityArm::AblatedToNeutral, 0);
    let sig_full = build_index(Driver::Velocity, VelocityArm::Present, 0);
    let a_sig = aggregate(&sig_ablated, VelocityArm::AblatedToNeutral, MAX_DISTANCE);
    let f_sig = aggregate(&sig_full, VelocityArm::Present, MAX_DISTANCE);
    let sig_fall = rel_fall_bps(a_sig.median_iqr, f_sig.median_iqr);

    // --- NULL ARM (LAW V2) ----------------------------------------------
    let null_ablated = build_index(Driver::Acceleration, VelocityArm::AblatedToNeutral, 0);
    let null_full = build_index(Driver::Acceleration, VelocityArm::Present, 0);
    let a_null = aggregate(&null_ablated, VelocityArm::AblatedToNeutral, MAX_DISTANCE);
    let f_null = aggregate(&null_full, VelocityArm::Present, MAX_DISTANCE);
    let null_fall = rel_fall_bps(a_null.median_iqr, f_null.median_iqr);

    println!(
        "LAW V1 SIGNAL  ablated_median_iqr={} full_median_iqr={} fall_bps={} \
         ablated_known={}/{} full_known={}/{}",
        a_sig.median_iqr,
        f_sig.median_iqr,
        sig_fall,
        a_sig.known,
        a_sig.total,
        f_sig.known,
        f_sig.total
    );
    println!(
        "LAW V2 NULL    ablated_median_iqr={} full_median_iqr={} fall_bps={} \
         ablated_known={}/{} full_known={}/{}",
        a_null.median_iqr,
        f_null.median_iqr,
        null_fall,
        a_null.known,
        a_null.total,
        f_null.known,
        f_null.total
    );

    // Coverage, both arms (LAW V3).
    let sig_cov_loss = rel_fall_bps(i128::from(a_sig.known), i128::from(f_sig.known));
    let null_cov_loss = rel_fall_bps(i128::from(a_null.known), i128::from(f_null.known));
    println!("LAW V3 COVERAGE signal_loss_bps={sig_cov_loss} null_loss_bps={null_cov_loss}");

    assert!(
        sig_fall >= DISPERSION_IMPROVEMENT_BAR_BPS,
        "LAW V1 FAILED: within-class IQR fell only {sig_fall} bp against a \
         pre-registered {DISPERSION_IMPROVEMENT_BAR_BPS} bp bar — REVERT the field"
    );
    assert!(
        null_fall <= NULL_ARM_TOLERANCE_BPS,
        "LAW V2 FAILED: the null corpus 'improved' by {null_fall} bp, past the \
         pre-registered {NULL_ARM_TOLERANCE_BPS} bp tolerance — the gain is \
         partitioning, not information — REVERT the field"
    );
    assert!(
        sig_cov_loss <= COVERAGE_LOSS_BAR_BPS,
        "LAW V3 FAILED: signal-arm Known-rate fell {sig_cov_loss} bp — REVERT"
    );
    assert!(
        null_cov_loss <= COVERAGE_LOSS_BAR_BPS,
        "LAW V3 FAILED: null-arm Known-rate fell {null_cov_loss} bp — REVERT"
    );
}

/// **DIAGNOSTIC, not a law** — where the velocity field *does* cost coverage.
///
/// LAW V3 was pre-registered against the [`CORPUS_N`]-sized corpus and passed
/// there with zero loss, but a reader should not conclude the field is free. It is
/// not. Adding a six-level dimension multiplies the number of `(velocity,
/// acceleration)` cells by six, so at radius 0 the corpus needs roughly six times
/// as many episodes to keep every class above the sample floor.
///
/// This sweep publishes the crossover instead of leaving a skeptic to find it. No
/// bar is asserted on it: inventing a threshold *after* seeing the numbers is
/// exactly the sin pre-registration exists to prevent, so this is reported and the
/// disposition stays with LAWs V1–V3.
///
/// `radius=8` is the engine's real operating point
/// ([`MAX_DISTANCE_PRODUCTION`]); `radius=0` is the worst case, where a class is a
/// single bucket cell and the shattering is maximal.
///
/// # READ THE `radius=8` COLUMN WITH SUSPICION — it is DEGENERATE here
///
/// This corpus holds every field except the two holder derivatives constant, so the
/// largest reachable Hamming distance between any two episodes is
/// `(VEL_LEVELS - 1) + (ACCEL_LEVELS - 1) = 9`. A radius of 8 therefore admits
/// almost the entire corpus into every class, and once `TOP_M >= n` both arms
/// return the *identical* candidate set and the identical IQR. The zeros in that
/// column are an artefact of the corpus, **not** evidence that the field is free at
/// the production radius.
///
/// In the real engine the signature spans 21 fields and typical distances are far
/// larger, so radius 8 is genuinely selective — but this corpus cannot reproduce
/// that distance distribution, and pretending otherwise would be the exact
/// self-deception LAW V2 exists to prevent. The radius-0 column is the honest
/// measurement of the field's information content; the true production coverage
/// cost is UNMEASURED until a real corpus exists.
#[test]
fn diagnostic_velocity_coverage_cost_at_small_corpus_sizes() {
    println!("DIAGNOSTIC velocity coverage vs corpus size (min_sample={MIN_SAMPLE}, radius=0)");
    for n in [900u64, 480, 300, 240, 180, 120, 60] {
        let mut ablated = EpisodicIndex::with_capacity(n as usize + 16);
        let mut full = EpisodicIndex::with_capacity(n as usize + 16);
        for i in 0..n {
            ablated
                .push(build_episode(
                    i,
                    Driver::Velocity,
                    VelocityArm::AblatedToNeutral,
                    0,
                ))
                .expect("monotone");
            full.push(build_episode(i, Driver::Velocity, VelocityArm::Present, 0))
                .expect("monotone");
        }
        for (label, radius) in [
            ("radius=0 ", MAX_DISTANCE),
            ("radius=8*", MAX_DISTANCE_PRODUCTION),
        ] {
            let a = aggregate(&ablated, VelocityArm::AblatedToNeutral, radius);
            let f = aggregate(&full, VelocityArm::Present, radius);
            println!(
                "  n={n:<4} {label} ablated_known={}/{}  full_known={}/{}  \
                 coverage_loss_bps={}  ablated_iqr={}  full_iqr={}",
                a.known,
                a.total,
                f.known,
                f.total,
                rel_fall_bps(i128::from(a.known), i128::from(f.known)),
                a.median_iqr,
                f.median_iqr
            );
        }
    }
}

// ===========================================================================
// LAW C1 / C2 / C3 — the concentration conditioner
// ===========================================================================

/// Aggregate over queries that carry a `Known` concentration reading, with and
/// without the conditioner armed.
fn conditioner_aggregate(idx: &EpisodicIndex, armed: bool) -> Aggregate {
    let p = params();
    let mut iqrs: Vec<i128> = Vec::new();
    let mut known = 0u32;
    let mut total = 0u32;
    for (_v, _a, q) in query_grid(VelocityArm::Present) {
        for band in 0..4u8 {
            total += 1;
            let reading = ConcentrationReading::Known(ConcentrationShape::from_bands(band, 0, 0));
            let mut filter = RecallFilter::for_query(&q);
            if armed {
                filter = filter.with_concentration(&reading);
            }
            if let RecallVerdict::Known(s) = idx.recall_conditioned(&q, &p, &filter) {
                known += 1;
                iqrs.push(s.p75_net_lamports - s.p25_net_lamports);
            }
        }
    }
    Aggregate {
        median_iqr: median_i128(&mut iqrs),
        known,
        total,
    }
}

#[test]
fn law_c1_c2_concentration_conditioner_adoption() {
    // Full coverage first: the conditioner's ceiling, i.e. what it buys when every
    // episode carries a reading. Anything it cannot achieve here it can never
    // achieve at realistic coverage.
    let sig = build_index(Driver::ConcentrationBand, VelocityArm::Present, 10_000);
    let off = conditioner_aggregate(&sig, false);
    let on = conditioner_aggregate(&sig, true);
    let sig_fall = rel_fall_bps(off.median_iqr, on.median_iqr);
    println!(
        "LAW C1 SIGNAL  off_median_iqr={} on_median_iqr={} fall_bps={} \
         off_known={}/{} on_known={}/{}",
        off.median_iqr, on.median_iqr, sig_fall, off.known, off.total, on.known, on.total
    );

    // NULL arm: the outcome depends on velocity, the concentration band is noise.
    let null = build_index(Driver::Velocity, VelocityArm::Present, 10_000);
    let noff = conditioner_aggregate(&null, false);
    let non = conditioner_aggregate(&null, true);
    let null_fall = rel_fall_bps(noff.median_iqr, non.median_iqr);
    println!(
        "LAW C2 NULL    off_median_iqr={} on_median_iqr={} fall_bps={} \
         off_known={}/{} on_known={}/{}",
        noff.median_iqr, non.median_iqr, null_fall, noff.known, noff.total, non.known, non.total
    );

    assert!(
        sig_fall >= DISPERSION_IMPROVEMENT_BAR_BPS,
        "LAW C1 FAILED: conditioned IQR fell only {sig_fall} bp — REVERT the conditioner"
    );
    assert!(
        null_fall <= NULL_ARM_TOLERANCE_BPS,
        "LAW C2 FAILED: the null corpus 'improved' by {null_fall} bp — REVERT"
    );
}

/// LAW C3 (i) — the conditioner degrades gracefully as coverage falls, and its
/// benefit is bounded by coverage rather than faked at low coverage.
///
/// This is the measurement that matters most in practice, because the real
/// `Exact`-basis coverage is low. Reported as a sweep, not barred.
#[test]
fn law_c3_conditioner_benefit_tracks_coverage() {
    println!("LAW C3 COVERAGE SWEEP (concentration conditioner)");
    let mut last = None;
    for rate in [10_000u64, 5_000, 2_500, 1_000, 500, 0] {
        let idx = build_index(Driver::ConcentrationBand, VelocityArm::Present, rate);
        let off = conditioner_aggregate(&idx, false);
        let on = conditioner_aggregate(&idx, true);
        let fall = rel_fall_bps(off.median_iqr, on.median_iqr);
        // How many corpus episodes actually carried a reading.
        let carried = (0..CORPUS_N)
            .filter(|i| {
                build_episode(*i, Driver::ConcentrationBand, VelocityArm::Present, rate)
                    .context()
                    .concentration
                    .is_known()
            })
            .count();
        // A "fall" computed over zero answered queries is not a fall — the armed
        // arm produced NO estimate, and `on_iqr = 0` is the absence of a number
        // rather than a tight distribution. Print the refusal instead of a
        // flattering 10 000 bp, or the sweep reads as "coverage improves precision"
        // when it means the opposite (§6.4 applied to the readout itself).
        let fall_col = if on.known == 0 {
            "fall_bps=n/a(all-refused)".to_string()
        } else {
            format!("fall_bps={fall}")
        };
        println!(
            "  known_rate_bps={rate} episodes_with_reading={carried}/{CORPUS_N} \
             off_iqr={} on_iqr={} {fall_col} on_known={}/{}",
            off.median_iqr, on.median_iqr, on.known, on.total
        );
        last = Some((off, on));
    }
    // At ZERO band coverage every armed query must REFUSE.
    //
    // AMENDMENT, recorded here rather than quietly applied: the original C3(i)
    // asserted the opposite — that at zero coverage the conditioner is exactly
    // inert — which was true only because the conditioner carried an escape code
    // letting unmeasured candidates pass an armed pin. That escape made an
    // "armed" recall over an unmeasured corpus return the POOLED estimate wearing
    // a band label, which is the §46 lie this crate refuses everywhere else. The
    // escape is gone; the honest bar at zero coverage is fail-closed refusal, and
    // that is what is asserted now. The `Unknown`-QUERY inertness leg (C3(ii)) is
    // untouched and still holds byte-identically.
    let (off, on) = last.expect("the sweep runs at least once");
    assert_eq!(
        on.known, 0,
        "at zero band coverage every band-pinned query must refuse, not pool"
    );
    assert_eq!(
        off.known, off.total,
        "…and the UNCONDITIONED arm must still answer, or the refusal above is a \
         property of the corpus rather than of the conditioner"
    );
}

/// LAW C3 (ii) — an `Unknown` QUERY is byte-identically unconditioned, and an armed
/// query only ever NARROWS. The two halves of "silently declines".
///
/// Note what is asserted and what is not: inertness is a property of an `Unknown`
/// *query*, not of an `Unknown` *candidate*. An armed query excludes unmeasured
/// candidates on purpose (see `RecallFilter::with_concentration`), so the second
/// leg here is monotone narrowing, which is exactly the guarantee a reduce-only
/// consumer needs.
#[test]
fn law_c3_the_conditioner_is_exactly_inert_where_it_has_no_data() {
    let p = params();
    let idx = build_index(Driver::ConcentrationBand, VelocityArm::Present, 5_000);
    let unknown = ConcentrationReading::Unknown(
        pump_quant_brain::concentration::ConcentrationUnknown::DeltaOnlyBasis,
    );
    for (_v, _a, q) in query_grid(VelocityArm::Present) {
        let plain = idx.recall(&q, &p);
        let conditioned = idx.recall_conditioned(
            &q,
            &p,
            &RecallFilter::for_query(&q).with_concentration(&unknown),
        );
        assert_eq!(plain, conditioned, "an Unknown query must not condition");

        // …and an armed query must never see FEWER than the Unknown candidates.
        let armed = idx.recall_conditioned(
            &q,
            &p,
            &RecallFilter::for_query(&q).with_concentration(&ConcentrationReading::Known(
                ConcentrationShape::from_bands(0, 0, 0),
            )),
        );
        if let (Some(a), Some(b)) = (armed.stats(), plain.stats()) {
            assert!(
                a.n_matched <= b.n_matched,
                "a conditioner may only ever narrow"
            );
        }
    }
}

/// The conditioner is one axis on purpose. Pinning all three bands would partition
/// a 4x4x4 space and shatter the corpus; this test states the arithmetic so a
/// future reader does not "improve" it into uselessness.
#[test]
fn the_conditioner_partitions_on_one_axis_not_sixty_four() {
    use pump_quant_brain::concentration::{BAND_COUNT, CONCENTRATION_CODE_COUNT};
    assert_eq!(CONCENTRATION_CODE_COUNT, BAND_COUNT + 1);
    // Distinct codes reachable: 4 bands + 1 Unknown = 5, not 4^3 + 1 = 65.
    assert_eq!(CONCENTRATION_CODE_COUNT, 5);
    let mut codes = Vec::new();
    for a in 0..BAND_COUNT {
        for b in 0..BAND_COUNT {
            for c in 0..BAND_COUNT {
                let code = ConcentrationReading::Known(ConcentrationShape::from_bands(a, b, c))
                    .filter_code();
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
    }
    assert_eq!(
        codes.len(),
        BAND_COUNT as usize,
        "the whale and early bands must NOT enter the partition"
    );
}
