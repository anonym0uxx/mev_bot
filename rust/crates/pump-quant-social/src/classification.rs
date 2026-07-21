//! Eight-state source classification with confidence and decay (constitution §29.8).
//!
//! # Responsibility
//! Fold the ten decomposed determinant scores (plus their derived flags) into exactly
//! one [`SourceState`], with a confidence and a decay half-life. The decision order is
//! **fade-first**: the disqualifying states are tested before any positive tier, and
//! `PRE_FLOW_ALPHA` is reachable only by beating the D3 selection control — the
//! PUBLIC_BURNED presumption in code. Deterministic integer throughout (§22).

use crate::types::{DeterminantScore, SourceState};

/// The bundle of determinant evidence a classification consumes. Callers assemble it
/// from the `determinants` module outputs; the derived flags (`shill_suspect`,
/// `post_peak_persistent`, `bot_farm`, `echo_heavy`) come from the D5/D2/D7/D8 result
/// structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterminantBundle {
    /// D1 reconciled markouts.
    pub d1: DeterminantScore,
    /// D2 lifecycle timing.
    pub d2: DeterminantScore,
    /// D3 selection-control excess.
    pub d3: DeterminantScore,
    /// D4 selectivity.
    pub d4: DeterminantScore,
    /// D5 skin-in-the-game.
    pub d5: DeterminantScore,
    /// D6 integrity.
    pub d6: DeterminantScore,
    /// D7 audience authenticity.
    pub d7: DeterminantScore,
    /// D8 originality / network position.
    pub d8: DeterminantScore,
    /// D9 category-conditional skill.
    pub d9: DeterminantScore,
    /// D10 clustering-as-distribution.
    pub d10: DeterminantScore,
    /// D5 derived: buy-before-call/distribute-into-call suspicion.
    pub shill_suspect: bool,
    /// D2 derived: persistent post-peak posting.
    pub post_peak_persistent: bool,
    /// D7 derived: bot / manufactured-engagement audience.
    pub bot_farm: bool,
    /// D8 derived: predominantly an echo node.
    pub echo_heavy: bool,
    /// Total reconciled call sample behind the source (governs INSUFFICIENT_SAMPLE).
    pub total_sample: u32,
}

/// Thresholds governing classification. All bps. Static-by-design config (§29.8):
/// the fade-first defaults live in [`ClassificationConfig::fade_first_default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassificationConfig {
    /// Minimum reconciled sample below which the source is `INSUFFICIENT_SAMPLE`.
    pub min_sample: u32,
    /// D3 excess a source must clear to be considered pre-flow alpha.
    pub pre_flow_excess_bps: i64,
    /// D1 markout a source must clear to be considered pre-flow alpha.
    pub pre_flow_markout_bps: i64,
    /// D1 markout a source must clear to be a flow amplifier.
    pub amplifier_markout_bps: i64,
    /// D8 originator share below which (independent of the echo_heavy flag) a source
    /// classifies as a copy-echo account.
    pub echo_originality_bps: i64,
    /// D7 authenticity below which (independent of the bot_farm flag) a source
    /// classifies as an engagement farm.
    pub bot_farm_authenticity_bps: i64,
    /// D2 lifecycle value at or below which a source is a late exit-liquidity
    /// promoter (independent of the persistent flag).
    pub late_exit_timing_bps: i64,
    /// Decay half-life (ns) attached to the resulting classification — no state is
    /// permanent from one call (§29.8).
    pub decay_half_life_ns: u64,
}

impl ClassificationConfig {
    /// The PUBLIC_BURNED-presumption defaults (§29.8): a source must clear a high D3
    /// excess *and* real D1 markout from a favourable lifecycle posture to earn
    /// `PRE_FLOW_ALPHA`, and thin evidence resolves to `INSUFFICIENT_SAMPLE`.
    #[must_use]
    pub const fn fade_first_default() -> Self {
        Self {
            min_sample: 12,
            pre_flow_excess_bps: 1_500,
            pre_flow_markout_bps: 1_000,
            amplifier_markout_bps: 300,
            echo_originality_bps: 3_000,
            bot_farm_authenticity_bps: 0,
            late_exit_timing_bps: -2_000,
            // ~14 days in ns: recent evidence dominates, stale trust decays away.
            decay_half_life_ns: 14 * 24 * 60 * 60 * 1_000_000_000,
        }
    }
}

/// A classification result: the state, the confidence it was reached with, and the
/// decay half-life after which it must be revisited (§29.8: never permanent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    /// The assigned source state.
    pub state: SourceState,
    /// Confidence in bps (0..=10_000) — the min confidence of the driving
    /// determinants, so a decision resting on thin evidence stays low-confidence.
    pub confidence_bps: u16,
    /// Half-life (ns) after which this classification decays and must be recomputed.
    pub decay_half_life_ns: u64,
}

/// Minimum of two confidences (bps).
fn min_conf(a: u16, b: u16) -> u16 {
    if a < b {
        a
    } else {
        b
    }
}

/// **Classify a source into one of the eight §29.8 states (fade-first).**
///
/// Decision order (each check is a fade gate before the positive tiers):
/// 1. `total_sample < min_sample` → `InsufficientSample` (the PUBLIC_BURNED default).
/// 2. `shill_suspect` (D5) → `PaidShillSuspect`.
/// 3. `bot_farm` or D7 below threshold → `EngagementFarm`.
/// 4. `echo_heavy` or D8 originator share below threshold → `CopyEchoAccount`.
/// 5. `post_peak_persistent` or D2 at/below late-exit threshold →
///    `LateExitLiquidityPromoter`.
/// 6. D3 excess ≥ threshold **and** D1 ≥ threshold **and** D2 favourable (>0) →
///    `PreFlowAlpha` (the only path, and only via the control).
/// 7. D1 ≥ amplifier threshold → `FlowAmplifier`.
/// 8. otherwise → `OrganicCommunityNode`.
///
/// Confidence is the minimum confidence of the determinants that gated the chosen
/// branch, so a decision resting on one thin determinant cannot report high
/// confidence.
#[must_use]
pub fn classify(bundle: &DeterminantBundle, cfg: &ClassificationConfig) -> Classification {
    let hl = cfg.decay_half_life_ns;

    // 1. Fade-first default: insufficient reconciled evidence.
    if bundle.total_sample < cfg.min_sample {
        return Classification {
            state: SourceState::InsufficientSample,
            confidence_bps: crate::fixedpoint::confidence_bps(
                bundle.total_sample,
                cfg.min_sample.max(1),
            ),
            decay_half_life_ns: hl,
        };
    }

    // 2. Wallet-graph shill suspicion (D5) — strongest fade.
    if bundle.shill_suspect {
        return Classification {
            state: SourceState::PaidShillSuspect,
            confidence_bps: bundle.d5.confidence_bps,
            decay_half_life_ns: hl,
        };
    }

    // 3. Inauthentic audience (D7).
    if bundle.bot_farm || bundle.d7.value_bps < cfg.bot_farm_authenticity_bps {
        return Classification {
            state: SourceState::EngagementFarm,
            confidence_bps: bundle.d7.confidence_bps,
            decay_half_life_ns: hl,
        };
    }

    // 4. Echo node (D8) — reach mistaken for alpha.
    if bundle.echo_heavy || bundle.d8.value_bps < cfg.echo_originality_bps {
        return Classification {
            state: SourceState::CopyEchoAccount,
            confidence_bps: bundle.d8.confidence_bps,
            decay_half_life_ns: hl,
        };
    }

    // 5. Late exit-liquidity promotion (D2).
    if bundle.post_peak_persistent || bundle.d2.value_bps <= cfg.late_exit_timing_bps {
        return Classification {
            state: SourceState::LateExitLiquidityPromoter,
            confidence_bps: bundle.d2.confidence_bps,
            decay_half_life_ns: hl,
        };
    }

    // 6. Pre-flow alpha — ONLY via the D3 selection control (§29.8 mandate).
    if bundle.d3.value_bps >= cfg.pre_flow_excess_bps
        && bundle.d1.value_bps >= cfg.pre_flow_markout_bps
        && bundle.d2.value_bps > 0
    {
        return Classification {
            state: SourceState::PreFlowAlpha,
            confidence_bps: min_conf(
                bundle.d3.confidence_bps,
                min_conf(bundle.d1.confidence_bps, bundle.d2.confidence_bps),
            ),
            decay_half_life_ns: hl,
        };
    }

    // 7. Flow amplifier — positive markout, rides flow, does not beat the control.
    if bundle.d1.value_bps >= cfg.amplifier_markout_bps {
        return Classification {
            state: SourceState::FlowAmplifier,
            confidence_bps: bundle.d1.confidence_bps,
            decay_half_life_ns: hl,
        };
    }

    // 8. Authentic but no demonstrated pre-flow edge.
    Classification {
        state: SourceState::OrganicCommunityNode,
        confidence_bps: min_conf(bundle.d1.confidence_bps, bundle.d7.confidence_bps),
        decay_half_life_ns: hl,
    }
}
