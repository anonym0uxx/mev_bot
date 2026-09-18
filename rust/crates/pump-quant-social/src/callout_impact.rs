//! Derived callout-impact features for the model observation (operator §29.8/§29.9).
//!
//! # Responsibility
//! Fold the evidence this crate already derives — reconciled call markouts (D1),
//! lifecycle timing (D2), the copy-echo / amplification graph (D7/D8), the
//! per-source quality classification, and per-source realized net SOL (§74) — into
//! ONE flat, integer-only feature block a trading model may observe. This module
//! adds no new estimators: it calls [`crate::determinants::d1_reconciled_markouts`],
//! [`crate::determinants::d2_lifecycle_timing`], [`detect_copy_echo`],
//! [`build_amplification_graph`], [`originator_fraction_bps`],
//! [`density_bps`] and the ledgers, and encodes their outputs.
//!
//! # Licence boundary: no raw social text, ever
//! Scraped message text is licence-excluded *and* is a prompt-injection surface, so
//! it may never enter a training corpus or a model observation. Consequently
//! [`CalloutImpactFeatures`] contains **no `String` field and no field capable of
//! carrying text**: every member is a `u32`/`i32` count, basis-point value, or
//! signed divergence. [`build_features`] is pure and neither takes nor returns a
//! `String`; it consumes only integers that were already measured on-chain or in
//! the deterministic reducers. The sole textual surface is
//! [`CalloutImpactFeatures::render_tokens`], which renders those integers as
//! `k=v` tokens — the corpus builder owns the line label and still never sees
//! message text.
//!
//! # Unobserved is not zero
//! A `0` produced by measurement and a `0` meaning "no evidence" are different
//! facts, so the block carries explicit sentinels (see [`UNOBSERVED_COUNT`],
//! [`UNOBSERVED_UNSIGNED_BP`], [`UNOBSERVED_SIGNED_BP`]) and
//! [`CalloutImpactFeatures::is_observed`] distinguishes the all-sentinel block. A
//! builder that sees `false` omits the block entirely rather than emitting
//! fabricated zeros, and [`CalloutImpactFeatures::render_tokens`] returns the empty
//! string for it. Sentinels also survive per-field inside an otherwise observed
//! block, so "we saw callouts but have no reconciled markout yet" stays
//! distinguishable from "markout measured exactly zero".
//!
//! # Determinism (§22)
//! Integer / basis-point only — no `f64`. No clock, RNG, network, or filesystem:
//! every age or timestamp is an already-measured `u64` supplied by the caller, and
//! the block is a deterministic function of its inputs.

use crate::amplification::{build_amplification_graph, originator_fraction_bps};
use crate::classification::ClassificationConfig;
use crate::copy_echo::{copy_echo_density_bps as density_bps, detect_copy_echo, ChannelCall};
use crate::determinants::{
    d1_reconciled_markouts, d2_lifecycle_timing, LifecycleSample, MarkoutSample,
};
use crate::fixedpoint::{clamp_bps, clamp_i128_to_i64, BPS_SCALE};
use crate::ledger::{SourceOutcomeLedger, SourceQualityLedger};
use crate::types::{SourceRef, SourceState};

/// Sentinel for a count field that carried no measurement.
///
/// Counts are non-negative and the window is caller-supplied, so `0` is also a
/// legitimate *measured* count; a block whose counts are zero is therefore only
/// ever produced together with the bp sentinels below (see
/// [`CalloutImpactFeatures::unobserved`]).
pub const UNOBSERVED_COUNT: u32 = 0;

/// Sentinel for an *unsigned* basis-point field that carried no measurement:
/// `u32::MAX`.
///
/// Every unsigned bp field here is bounded by [`BPS_SCALE`] (10_000) or twice it
/// (20_000, for the centred encodings), so `u32::MAX` can never be a real
/// measurement and a genuinely measured `0` stays distinguishable from "no
/// evidence".
pub const UNOBSERVED_UNSIGNED_BP: u32 = u32::MAX;

/// Sentinel for a *signed* basis-point field that carried no measurement:
/// `i32::MIN`.
///
/// Every signed field here is a bounded markout or divergence clamped into a range
/// strictly above `i32::MIN`, so `i32::MIN` is unreachable as a measurement.
pub const UNOBSERVED_SIGNED_BP: i32 = i32::MIN;

/// The fixed token order [`CalloutImpactFeatures::render_tokens`] emits. Part of
/// the contract: reordering it changes what the model sees.
pub const TOKEN_ORDER: [&str; 9] = [
    "callout_count_300s",
    "distinct_sources_300s",
    "markout_strength_bp",
    "lifecycle_phase_bp",
    "amplification_bp",
    "copy_echo_density_bps",
    "attention_money_divergence_bp",
    "source_quality_bp",
    "originator_fraction_bp",
];

/// Static-by-design thresholds and half-lives for [`build_features`].
///
/// §29.8 estimator discipline: the only constants a scorer may hold live in an
/// explicit config struct, never inline in the math. Carrying the
/// [`ClassificationConfig`] here keeps the feature block and the quality ledger
/// from silently disagreeing about the fade-first defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutImpactConfig {
    /// Width of the "recent callout" window in ns (the `300s` fields). Measured
    /// back from the newest callout timestamp in the slice — never from a clock.
    pub window_ns: u64,
    /// Shingle-Jaccard threshold in bps at which two calls count as copy-echo.
    pub copy_echo_similarity_threshold_bps: i64,
    /// Time window in ns inside which a later similar call counts as an echo.
    pub copy_echo_window_ns: u64,
    /// Lag in ns at which an amplification edge's weight decays to zero.
    pub amplification_max_lag_ns: u64,
    /// D1 horizon blend weights, `[+5m, +30m, +2h, +24h]`.
    pub markout_horizon_weights: [i64; 4],
    /// D1 time-decay half-life in ns for reconciled call markouts.
    pub markout_half_life_ns: u64,
    /// D2 time-decay half-life in ns for lifecycle timing.
    pub lifecycle_half_life_ns: u64,
    /// D2 decayed post-peak share in bps above which posting is treated as
    /// persistent post-peak (exit-liquidity promotion).
    pub post_peak_threshold_bps: i64,
    /// Lamports that count as one unit of "money" when converting a source's
    /// realized net SOL into bps (1 SOL = `1_000_000_000` lamports).
    pub money_quantum_lamports: i64,
    /// Classification thresholds used by the caller when folding its determinant
    /// bundle into the quality ledger.
    pub classification: ClassificationConfig,
}

impl CalloutImpactConfig {
    /// The §29.8 defaults: a 300 s callout window, strict copy-echo similarity, a
    /// one-minute amplification lag, a one-day markout half-life, a
    /// fourteen-day lifecycle half-life, a 50 % post-peak threshold, and 1 SOL as
    /// the money quantum.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            window_ns: 300 * 1_000_000_000,
            copy_echo_similarity_threshold_bps: 6_000,
            copy_echo_window_ns: 300 * 1_000_000_000,
            amplification_max_lag_ns: 60 * 1_000_000_000,
            markout_horizon_weights: [1, 1, 1, 1],
            markout_half_life_ns: 24 * 60 * 60 * 1_000_000_000,
            lifecycle_half_life_ns: 14 * 24 * 60 * 60 * 1_000_000_000,
            post_peak_threshold_bps: 5_000,
            money_quantum_lamports: 1_000_000_000,
            classification: ClassificationConfig::fade_first_default(),
        }
    }
}

/// The derived callout-impact feature block: everything a model may observe about
/// a callout cluster, and nothing that can carry social text.
///
/// Every field is an integer count, an unsigned basis-point value, or a signed
/// basis-point value. The unsigned bp fields come in two documented encodings:
/// * `lifecycle_phase_bp` / `source_quality_bp` are **centred**: neutral is
///   [`BPS_SCALE`] (10_000), adverse is below it, favourable above it, and the
///   range is `0..=2 × BPS_SCALE`.
/// * `amplification_bp` / `copy_echo_density_bps` / `originator_fraction_bp` are
///   plain `0..=BPS_SCALE` values.
///
/// A field whose input slice was absent carries its sentinel rather than a
/// fabricated zero; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutImpactFeatures {
    /// Distinct callouts on the token inside the config window; sentinelled at
    /// [`UNOBSERVED_COUNT`] when no callouts fell in the window.
    pub callout_count_300s: u32,
    /// Distinct sources behind those callouts.
    pub distinct_sources_300s: u32,
    /// D1 reconciled markout strength in bp (blended across the four horizons and
    /// decay-averaged). Sentinelled at [`UNOBSERVED_SIGNED_BP`] when no reconciled
    /// markout sample was supplied.
    pub markout_strength_bp: i32,
    /// D2 lifecycle timing, centred: `d2.value_bps + BPS_SCALE` in `0..=20_000`, so
    /// pre-flow is above 10_000 and post-peak below. Sentinelled at
    /// [`UNOBSERVED_UNSIGNED_BP`] when no lifecycle sample was supplied.
    pub lifecycle_phase_bp: u32,
    /// Peak amplification-edge weight in bp (`0..=BPS_SCALE`): how strongly any one
    /// originator→echo pair was amplified. Reach, not alpha (§29.8).
    pub amplification_bp: u32,
    /// D7 semantic copy-echo density in bp (`0..=BPS_SCALE`): the share of calls in
    /// the window that are echoes of an earlier call from a different source.
    pub copy_echo_density_bps: u32,
    /// Signed divergence between measured attention and realized money, in bp:
    /// attention (`amplification_bp`) minus the source's realized net SOL converted
    /// at [`CalloutImpactConfig::money_quantum_lamports`] per bp-unit, clamped into
    /// `±BPS_SCALE`. Positive means reach outran the money the source actually
    /// earned. Sentinelled at [`UNOBSERVED_SIGNED_BP`] when the source has no
    /// reconciled outcome.
    pub attention_money_divergence_bp: i32,
    /// Per-source quality from the ledger's classification, centred and scaled by
    /// the classification's confidence: `state_value_bp × confidence / BPS_SCALE +
    /// BPS_SCALE`. Sentinelled at [`UNOBSERVED_UNSIGNED_BP`] when the source is
    /// unclassified.
    pub source_quality_bp: u32,
    /// D8 network position in bp (`0..=BPS_SCALE`): the subject's originator share
    /// in the amplification graph (out-degree over total degree).
    pub originator_fraction_bp: u32,
}

impl CalloutImpactFeatures {
    /// The all-sentinel block: every count zero, every unsigned bp field
    /// [`UNOBSERVED_UNSIGNED_BP`], every signed bp field
    /// [`UNOBSERVED_SIGNED_BP`]. No evidence, and explicitly not zeros.
    #[must_use]
    pub const fn unobserved() -> Self {
        Self {
            callout_count_300s: UNOBSERVED_COUNT,
            distinct_sources_300s: UNOBSERVED_COUNT,
            markout_strength_bp: UNOBSERVED_SIGNED_BP,
            lifecycle_phase_bp: UNOBSERVED_UNSIGNED_BP,
            amplification_bp: UNOBSERVED_UNSIGNED_BP,
            copy_echo_density_bps: UNOBSERVED_UNSIGNED_BP,
            attention_money_divergence_bp: UNOBSERVED_SIGNED_BP,
            source_quality_bp: UNOBSERVED_UNSIGNED_BP,
            originator_fraction_bp: UNOBSERVED_UNSIGNED_BP,
        }
    }

    /// Whether this block carries any evidence.
    ///
    /// `false` exactly for the [`CalloutImpactFeatures::unobserved`] sentinel, so a
    /// corpus builder omits the block instead of emitting fabricated zeros. Note
    /// that a block can be observed while individual fields still carry their
    /// sentinel — "no markout reconciled yet" is evidence of its own.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        *self != Self::unobserved()
    }

    /// Render the block as deterministic `k=v` tokens in [`TOKEN_ORDER`], matching
    /// the c11 live-flow style (space-joined, no quoting, integers only).
    ///
    /// Returns the empty string for the unobserved sentinel: there is nothing
    /// usable to say, and emitting zeros would assert measurements that were never
    /// made. The corpus builder prefixes its own line label.
    #[must_use]
    pub fn render_tokens(&self) -> String {
        if !self.is_observed() {
            return String::new();
        }
        format!(
            "callout_count_300s={} distinct_sources_300s={} markout_strength_bp={} \
             lifecycle_phase_bp={} amplification_bp={} copy_echo_density_bps={} \
             attention_money_divergence_bp={} source_quality_bp={} originator_fraction_bp={}",
            self.callout_count_300s,
            self.distinct_sources_300s,
            self.markout_strength_bp,
            self.lifecycle_phase_bp,
            self.amplification_bp,
            self.copy_echo_density_bps,
            self.attention_money_divergence_bp,
            self.source_quality_bp,
            self.originator_fraction_bp,
        )
    }
}

/// The callouts inside `window_ns` of the newest callout in the slice.
///
/// The window is anchored on the newest supplied timestamp, so the function never
/// reads a clock (§22) and is stable under input reordering. Clones are cheap and
/// bounded by the slice the caller already holds.
#[must_use]
fn calls_in_window(calls: &[ChannelCall], window_ns: u64) -> Vec<ChannelCall> {
    match calls.iter().map(|c| c.timestamp_ns).max() {
        None => Vec::new(),
        Some(latest) => calls
            .iter()
            .filter(|c| latest.saturating_sub(c.timestamp_ns) <= window_ns)
            .cloned()
            .collect(),
    }
}

/// Distinct `source_id` count. Sorted-unique (no hashing) so the result is
/// deterministic regardless of platform hash seeding (§22).
#[must_use]
fn distinct_source_count(calls: &[ChannelCall]) -> u32 {
    let mut ids: Vec<u64> = calls.iter().map(|c| c.source_id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len().min(u32::MAX as usize) as u32
}

/// Narrow a bp value into a signed field type, clamped away from
/// [`UNOBSERVED_SIGNED_BP`] so a measurement can never be mistaken for the "no
/// evidence" sentinel.
#[must_use]
fn signed_bp_field(v: i64) -> i32 {
    v.clamp(i32::MIN as i64 + 1, i32::MAX as i64) as i32
}

/// Centre a signed bp value into an unsigned field: `clamp_bps(v) + BPS_SCALE`,
/// giving neutral = [`BPS_SCALE`] and a range of `0..=2 × BPS_SCALE`.
#[must_use]
fn centered_bp(v: i64) -> u32 {
    (clamp_bps(v) + BPS_SCALE).clamp(0, 2 * BPS_SCALE) as u32
}

/// Static-by-design mapping of a §29.8 source state onto a signed bp value
/// (higher = more alpha-favourable). This encodes the *sign* the fade-first
/// classifier already decided; it measures nothing new.
#[must_use]
fn state_value_bp(state: SourceState) -> i64 {
    match state {
        SourceState::PreFlowAlpha => 8_000,
        SourceState::FlowAmplifier => 3_000,
        SourceState::OrganicCommunityNode => 0,
        SourceState::InsufficientSample => 0,
        SourceState::CopyEchoAccount => -4_000,
        SourceState::EngagementFarm => -6_000,
        SourceState::PaidShillSuspect => -8_000,
        SourceState::LateExitLiquidityPromoter => -8_000,
    }
}

/// Centre and confidence-scale a classification into the `source_quality_bp`
/// field: `state_value_bp × confidence_bps / BPS_SCALE + BPS_SCALE`, in
/// `0..=2 × BPS_SCALE`. A thin-evidence classification therefore sits near
/// neutral no matter how extreme its state implies.
#[must_use]
fn encode_quality_bp(state_value_bp: i64, confidence_bps: u16) -> u32 {
    let scaled = (state_value_bp as i128) * (confidence_bps as i128) / (BPS_SCALE as i128);
    ((scaled + BPS_SCALE as i128).clamp(0, 2 * BPS_SCALE as i128)) as u32
}

/// Convert a signed lamport net into bps against `quantum_lamports` (the money
/// side of the attention-vs-money divergence). Widened through `i128`; clamped to
/// the canonical `±BPS_SCALE` band. A non-positive quantum yields `0` (no scale, no
/// signal).
#[must_use]
fn money_bp(net_sol_lamports: i64, quantum_lamports: i64) -> i64 {
    if quantum_lamports <= 0 {
        return 0;
    }
    let v = (net_sol_lamports as i128) * (BPS_SCALE as i128) / (quantum_lamports as i128);
    clamp_bps(clamp_i128_to_i64(v))
}

/// Build the [`CalloutImpactFeatures`] block from already-derived inputs.
///
/// Pure and integer-only: it **calls** the existing derivations
/// ([`crate::determinants::d1_reconciled_markouts`] for `markout_strength_bp`,
/// [`crate::determinants::d2_lifecycle_timing`] for `lifecycle_phase_bp`,
/// [`detect_copy_echo`] + [`build_amplification_graph`] for `amplification_bp`
/// and [`originator_fraction_bps`], [`density_bps`] for
/// `copy_echo_density_bps`) rather than recomputing any of them, and reads the
/// quality classification and realized net SOL from the two ledgers. It takes no
/// `String` and returns none; the inputs are counts, timestamps, prices, and
/// ledger entries.
///
/// Inputs:
/// * `calls` — callout events; only those inside `cfg.window_ns` of the newest
///   timestamp contribute (so all count/echo/amplification fields describe the
///   same window, and no clock is read).
/// * `markouts` — reconciled D1 samples (decay-weighted by
///   `cfg.markout_half_life_ns`, not windowed).
/// * `lifecycles` — D2 samples.
/// * `quality` — the per-source quality ledger; the entry for `subject.id` supplies
///   `source_quality_bp`.
/// * `outcomes` — the per-source realized-outcome ledger; the entry for `subject`
///   supplies the money side of `attention_money_divergence_bp`.
/// * `subject` — the source the block is about (kind + id; §29.8/§71 attribution).
/// * `cfg` — thresholds and half-lives.
///
/// Returns [`CalloutImpactFeatures::unobserved`] when nothing in the window
/// carried evidence (no windowed callouts, no markouts, no lifecycle samples, no
/// classification, no outcome) — never a fabricated block of zeros.
#[must_use]
pub fn build_features(
    calls: &[ChannelCall],
    markouts: &[MarkoutSample],
    lifecycles: &[LifecycleSample],
    quality: &SourceQualityLedger,
    outcomes: &SourceOutcomeLedger,
    subject: SourceRef,
    cfg: &CalloutImpactConfig,
) -> CalloutImpactFeatures {
    let windowed = calls_in_window(calls, cfg.window_ns);
    let classification = quality.get(subject.id);
    let outcome = outcomes.get(subject);

    if windowed.is_empty()
        && markouts.is_empty()
        && lifecycles.is_empty()
        && classification.is_none()
        && outcome.is_none()
    {
        return CalloutImpactFeatures::unobserved();
    }

    // D1 — call the existing reconciled-markout derivation; no recomputation.
    let markout_strength_bp = if markouts.is_empty() {
        UNOBSERVED_SIGNED_BP
    } else {
        let d1 = d1_reconciled_markouts(
            markouts,
            cfg.markout_horizon_weights,
            cfg.markout_half_life_ns,
        );
        signed_bp_field(d1.value_bps)
    };

    // D2 — call the existing lifecycle-timing derivation.
    let lifecycle_phase_bp = if lifecycles.is_empty() {
        UNOBSERVED_UNSIGNED_BP
    } else {
        let d2 = d2_lifecycle_timing(
            lifecycles,
            cfg.lifecycle_half_life_ns,
            cfg.post_peak_threshold_bps,
        );
        centered_bp(d2.score.value_bps)
    };

    // D7/D8 — call the existing copy-echo detection and graph scoring.
    let echo_edges = if windowed.is_empty() {
        Vec::new()
    } else {
        detect_copy_echo(
            &windowed,
            cfg.copy_echo_similarity_threshold_bps,
            cfg.copy_echo_window_ns,
        )
    };
    let graph = build_amplification_graph(&echo_edges, cfg.amplification_max_lag_ns);

    let amplification_bp = if windowed.is_empty() {
        UNOBSERVED_UNSIGNED_BP
    } else {
        graph
            .iter()
            .map(|e| e.weight_bps)
            .max()
            .map_or(0, |w| clamp_bps(w).clamp(0, BPS_SCALE) as u32)
    };

    let copy_echo_density_bps = if windowed.is_empty() {
        UNOBSERVED_UNSIGNED_BP
    } else {
        clamp_bps(density_bps(
            &windowed,
            cfg.copy_echo_similarity_threshold_bps,
            cfg.copy_echo_window_ns,
        ))
        .clamp(0, BPS_SCALE) as u32
    };

    let originator_fraction_bp = if windowed.is_empty() {
        UNOBSERVED_UNSIGNED_BP
    } else {
        clamp_bps(originator_fraction_bps(subject.id, &graph)).clamp(0, BPS_SCALE) as u32
    };

    let source_quality_bp = match classification {
        Some(c) => encode_quality_bp(state_value_bp(c.state), c.confidence_bps),
        None => UNOBSERVED_UNSIGNED_BP,
    };

    // Attention (measured amplification) minus money (realized net SOL). With no
    // callouts in the window there is no measured attention, which is `0`, not a
    // fabricated amplification value.
    let attention_money_divergence_bp = match outcome {
        None => UNOBSERVED_SIGNED_BP,
        Some(o) => {
            let attention_bp = if amplification_bp == UNOBSERVED_UNSIGNED_BP {
                0
            } else {
                i64::from(amplification_bp)
            };
            let realized_money_bp = money_bp(o.net_sol_lamports, cfg.money_quantum_lamports);
            clamp_bps(attention_bp.saturating_sub(realized_money_bp)) as i32
        }
    };

    CalloutImpactFeatures {
        callout_count_300s: windowed.len().min(u32::MAX as usize) as u32,
        distinct_sources_300s: distinct_source_count(&windowed),
        markout_strength_bp,
        lifecycle_phase_bp,
        amplification_bp,
        copy_echo_density_bps,
        attention_money_divergence_bp,
        source_quality_bp,
        originator_fraction_bp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::DeterminantBundle;
    use crate::types::{DeterminantScore, LifecyclePhase, SourceKind};

    fn score(value_bps: i64, confidence_bps: u16) -> DeterminantScore {
        DeterminantScore {
            value_bps,
            sample_size: 30,
            confidence_bps,
        }
    }

    fn callout(source_id: u64, timestamp_ns: u64, shingles: &[u32]) -> ChannelCall {
        ChannelCall {
            source_id,
            channel_id: 1,
            token_id: 1,
            timestamp_ns,
            shingles: shingles.to_vec(),
        }
    }

    fn empty_ledgers() -> (SourceQualityLedger, SourceOutcomeLedger) {
        (
            SourceQualityLedger::with_capacity(8),
            SourceOutcomeLedger::with_capacity(8),
        )
    }

    #[test]
    fn unobserved_sentinel_is_not_observed_and_renders_nothing() {
        let u = CalloutImpactFeatures::unobserved();
        assert!(!u.is_observed());
        assert_eq!(u.render_tokens(), "");
        // The sentinels are documented, distinct, and unreachable as measurements.
        assert_eq!(u.callout_count_300s, 0);
        assert_eq!(u.distinct_sources_300s, 0);
        assert_eq!(u.lifecycle_phase_bp, u32::MAX);
        assert_eq!(u.markout_strength_bp, i32::MIN);
        assert!(!TOKEN_ORDER.is_empty());
    }

    #[test]
    fn empty_inputs_yield_the_sentinel_without_panicking() {
        let (quality, outcomes) = empty_ledgers();
        let f = build_features(
            &[],
            &[],
            &[],
            &quality,
            &outcomes,
            SourceRef::new(SourceKind::X, 7),
            &CalloutImpactConfig::defaults(),
        );
        assert_eq!(f, CalloutImpactFeatures::unobserved());
        assert!(!f.is_observed());
        assert!(f.render_tokens().is_empty());
    }

    #[test]
    fn composed_case_renders_documented_order_and_integers() {
        let cfg = CalloutImpactConfig::defaults();
        let subject = SourceRef::new(SourceKind::X, 7);

        // Three callouts on one token: source 7 at t=0, source 8 repeats it 30 s
        // later (an echo), source 7 posts unrelated content at t=100 s.
        let calls = vec![
            callout(7, 0, &[1, 2, 3, 4]),
            callout(8, 30_000_000_000, &[1, 2, 3, 4]),
            callout(7, 100_000_000_000, &[9]),
        ];

        // One reconciled call: +10 % at +5 m, +20 % at +30 m, +30 % at +2 h,
        // +50 % at +24 h, no age → blended markout 2750 bp.
        let markouts = vec![MarkoutSample {
            price_at_call: 1_000,
            price_5m: 1_100,
            price_30m: 1_200,
            price_2h: 1_300,
            price_24h: 1_500,
            age_ns: 0,
        }];

        // One pre-flow call, fresh → D2 value 8000 bp → centred 18000.
        let lifecycles = vec![LifecycleSample {
            phase: LifecyclePhase::PreFlow,
            age_ns: 0,
        }];

        // Fold a bundle that classifies as PRE_FLOW_ALPHA with confidence 4000.
        let mut quality = SourceQualityLedger::with_capacity(8);
        let bundle = DeterminantBundle {
            d1: score(1_000, 5_000),
            d2: score(8_000, 5_000),
            d3: score(2_000, 4_000),
            d4: score(0, 0),
            d5: score(0, 0),
            d6: score(0, 0),
            d7: score(5_000, 5_000),
            d8: score(5_000, 5_000),
            d9: score(0, 0),
            d10: score(0, 0),
            shill_suspect: false,
            post_peak_persistent: false,
            bot_farm: false,
            echo_heavy: false,
            total_sample: 30,
        };
        let classification = quality.fold(7, &bundle, &cfg.classification);
        assert_eq!(classification.state, SourceState::PreFlowAlpha);
        assert_eq!(classification.confidence_bps, 4_000);

        // The subject lost 0.25 SOL realized → money side −2500 bp.
        let mut outcomes = SourceOutcomeLedger::with_capacity(8);
        outcomes.record(subject, -250_000_000);

        let f = build_features(
            &calls,
            &markouts,
            &lifecycles,
            &quality,
            &outcomes,
            subject,
            &cfg,
        );

        assert!(f.is_observed());
        assert_eq!(f.callout_count_300s, 3);
        assert_eq!(f.distinct_sources_300s, 2);
        assert_eq!(f.markout_strength_bp, 2_750);
        assert_eq!(f.lifecycle_phase_bp, 18_000);
        assert_eq!(f.amplification_bp, 5_000);
        assert_eq!(f.copy_echo_density_bps, 3_333);
        assert_eq!(f.attention_money_divergence_bp, 7_500);
        assert_eq!(f.source_quality_bp, 13_200);
        assert_eq!(f.originator_fraction_bp, 10_000);

        // The exact rendered line: TOKEN_ORDER, `k=v`, space-joined, integers only.
        assert_eq!(
            f.render_tokens(),
            "callout_count_300s=3 distinct_sources_300s=2 markout_strength_bp=2750 \
             lifecycle_phase_bp=18000 amplification_bp=5000 copy_echo_density_bps=3333 \
             attention_money_divergence_bp=7500 source_quality_bp=13200 \
             originator_fraction_bp=10000"
        );
    }
}