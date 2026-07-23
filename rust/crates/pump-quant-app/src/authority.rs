//! The authority/evidence boundary of the laptop build (§38, §56.3, §64).
//!
//! Three laws live here, wired to the real governance/evaluator leaves so the
//! app can no longer produce UNLABELED evidence:
//!
//! 1. **Fill-model grading (§38):** every report the engine emits is tagged
//!    with the fill model that produced it and the [`EvidenceStatus`] it can
//!    honestly claim. Modes A/B (signal replay, optimistic ceiling) carry NO
//!    profitability claim and can never satisfy promotion; only Mode C
//!    (calibrated adversarial) may support movement toward live probe.
//! 2. **Promotion readiness (§64):** the engine grades its CURRENT evidence
//!    against the governance crate's [`ProbeReadinessGate`] fail-closed —
//!    every criterion the laptop cannot attest is `false`. A paper run
//!    therefore reports exactly which criteria block probe eligibility,
//!    instead of implying readiness it does not have. Missing live adapters
//!    surface as *missing capability* (`AwaitingLiveCapability` in the
//!    governance lifecycle), never as a human-approval requirement.
//! 3. **Strategy identity (§56.3/§19):** the strategy's config identity is
//!    hashed through the governance crate's canonical SHA-256 (StrategyHash)
//!    and paired with the protocol-registry version hashes, so every report
//!    and every future registry record binds to a reproducible identity.

use crate::config::{Config, FillModeCfg};
use pump_quant_evaluator::evidence_status::EvidenceStatus;
use pump_quant_evaluator::promotion_verdict::{PromotionBlockReason, PromotionStatisticalVerdict};
use pump_quant_governance::canonical::CanonicalValue;
use pump_quant_governance::hashing::{strategy_hash, StrategyHash};
use pump_quant_governance::strategy_registry::{
    EvidenceGrade, FillModelClass, ProbeCriterion, ProbeReadinessGate,
};
use pump_quant_ingest::social_parse::fnv1a_64;
use pump_quant_protocol::registry::{registry_version, Venue};

/// Map the operator's configured fill mode onto the §38 model class and the
/// evidence status it may claim. All laptop runs are [`EvidenceStatus::Paper`]
/// — no capital moves, nothing reconciles to chain — the distinction that
/// matters is WHICH paper: optimistic ceiling vs calibrated adversarial.
#[must_use]
pub fn evidence_of(mode: FillModeCfg) -> (EvidenceStatus, FillModelClass) {
    let class = match mode {
        FillModeCfg::SignalReplay => FillModelClass::CausalReplay,
        FillModeCfg::OptimisticCeiling => FillModelClass::OptimisticCeiling,
        FillModeCfg::AdversarialRealistic | FillModeCfg::AdversarialPessimistic => {
            FillModelClass::CalibratedAdversarial
        }
    };
    (EvidenceStatus::Paper, class)
}

/// The engine's honest promotion-readiness report (report-only; §64).
#[derive(Clone, Debug)]
pub struct PromotionReadiness {
    /// What the current run's evidence IS (always `Paper` on the laptop).
    pub evidence_status: EvidenceStatus,
    /// The §38 fill-model class that produced the evidence.
    pub fill_model: FillModelClass,
    /// The fail-closed §64 evidence grade the laptop can currently attest.
    pub grade: EvidenceGrade,
    /// The governance probe gate's verdict over that grade (`Err` = first
    /// failed criterion; a paper run ALWAYS fails at least one).
    pub probe_gate: Result<(), ProbeCriterion>,
    /// §51 combined FDR + PBO/CSCV statistical promotion verdict, CONSULTED as a
    /// hard-blocker: a challenger family that does not survive Benjamini–Hochberg
    /// FDR correction or is at/above the PBO overfitting threshold (or whose CSCV
    /// matrix is inadmissible) blocks promotion regardless of the other criteria.
    /// Report-only, but never ignored — it gates `live_probe_eligible` and, when it
    /// is the binding constraint, surfaces in `blocked_on`.
    pub stat_verdict: PromotionStatisticalVerdict,
    /// True only when the fill model is Mode C AND the §52 baseline verdict
    /// defeats AND the probe gate passes AND the §51 statistical gate does not
    /// block — i.e. never on a pure laptop run.
    pub live_probe_eligible: bool,
    /// The single most actionable blocker, as a stable label.
    pub blocked_on: &'static str,
}

/// Build the fail-closed promotion readiness from the pieces the engine can
/// honestly attest today. Criteria the laptop CANNOT measure are hard `false`:
/// sequential live edge (no e-process over reconciled fills yet), sell
/// reliability (no live sell path), data health (no live feeds to attest),
/// reconciled trade count (paper fills reconcile to nothing).
#[must_use]
pub fn promotion_readiness(
    cfg: &Config,
    baselines_defeated: bool,
    drawdown_within_limits: bool,
    stat_verdict: PromotionStatisticalVerdict,
) -> PromotionReadiness {
    let (evidence_status, fill_model) = evidence_of(cfg.fill_mode);
    let grade = EvidenceGrade {
        fill_model,
        reconciled_trades: 0,
        baselines_defeated,
        sequential_edge_positive: false,
        sell_reliability_clean: false,
        drawdown_within_limits,
        data_health_strong: false,
    };
    // The probe minimum mirrors the §46 small-n guard the baseline verdict uses.
    let gate = ProbeReadinessGate {
        min_reconciled_trades: cfg.baseline_min_trades,
    };
    let probe_gate = gate.evaluate(&grade);
    let mode_c = matches!(fill_model, FillModelClass::CalibratedAdversarial);
    let stat_blocks = stat_verdict.blocks();
    let live_probe_eligible = mode_c && baselines_defeated && probe_gate.is_ok() && !stat_blocks;
    let blocked_on = if !mode_c {
        // §38: only Mode C may support movement toward live probe.
        "mode_c_required"
    } else if stat_blocks {
        // §51: a challenger that does not survive FDR or is overfit under PBO is
        // blocked BEFORE evidence-sufficiency — a statistically unsound edge is
        // not worth promoting even with clean data. Consulted, never ignored.
        match stat_verdict.reason {
            PromotionBlockReason::FdrOnly => "promotion_verdict:fdr",
            PromotionBlockReason::PboOnly => "promotion_verdict:pbo",
            PromotionBlockReason::Both => "promotion_verdict:fdr_pbo",
            // `stat_blocks` is true, so `Clear` is unreachable here; map defensively.
            PromotionBlockReason::Clear => "promotion_verdict:blocked",
        }
    } else if let Err(c) = probe_gate {
        match c {
            ProbeCriterion::SequentialEdge => "probe_gate:sequential_edge",
            ProbeCriterion::BaselinesDefeated => "probe_gate:baselines",
            ProbeCriterion::SellReliability => "probe_gate:sell_reliability",
            ProbeCriterion::Drawdown => "probe_gate:drawdown",
            ProbeCriterion::DataHealth => "probe_gate:data_health",
            ProbeCriterion::MinReconciledTrades => "probe_gate:min_reconciled_trades",
        }
    } else {
        // Every deterministic gate passed; only live adapters are missing.
        "awaiting_live_capability"
    };
    PromotionReadiness {
        evidence_status,
        fill_model,
        grade,
        probe_gate,
        stat_verdict,
        live_probe_eligible,
        blocked_on,
    }
}

/// The reproducible strategy identity (§56.3/§19): canonical config hash via
/// the governance SHA-256 domain, the FNV the journal digest is seeded with,
/// and a fold of the protocol registry version hashes for every known venue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyIdentity {
    /// Governance-canonical SHA-256 over the config identity text.
    pub strategy_hash: StrategyHash,
    /// FNV-1a/64 over the same identity (the journal-digest seed, §19).
    pub config_fnv: u64,
    /// FNV-1a/64 fold of `(version, hash)` for every protocol-registry venue.
    pub protocol_registry_fnv: u64,
}

/// Derive the identity from the live config. The Debug encoding of the Copy
/// config struct is deterministic for a fixed build (documented since the
/// digest-seed law landed); the canonical-encoding upgrade is noted in the
/// connectivity ledger as a stricter future form.
#[must_use]
pub fn strategy_identity(cfg: &Config) -> StrategyIdentity {
    let text = format!("{cfg:?}");
    let strategy_hash = strategy_hash(&CanonicalValue::Text(text.clone()));
    let config_fnv = fnv1a_64(text.as_bytes());
    let mut reg_bytes = Vec::new();
    for venue in [Venue::PumpFun, Venue::PumpSwap] {
        let (version, hash) = registry_version(venue);
        reg_bytes.extend_from_slice(&version.to_le_bytes());
        reg_bytes.extend_from_slice(&hash);
    }
    StrategyIdentity {
        strategy_hash,
        config_fnv,
        protocol_registry_fnv: fnv1a_64(&reg_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-blocking §51 verdict (nothing to promote / statistically clean).
    fn clear_stat() -> PromotionStatisticalVerdict {
        PromotionStatisticalVerdict {
            fdr_blocks: false,
            pbo_blocks: false,
            pbo_bps: Some(0),
            reason: PromotionBlockReason::Clear,
        }
    }

    #[test]
    fn optimistic_ceiling_is_never_probe_eligible() {
        let mut cfg = Config::dev_portable();
        cfg.fill_mode = FillModeCfg::OptimisticCeiling;
        // Even with every laptop-attestable criterion at its best...
        let r = promotion_readiness(&cfg, true, true, clear_stat());
        assert!(!r.live_probe_eligible);
        assert_eq!(r.blocked_on, "mode_c_required");
        assert_eq!(r.evidence_status, EvidenceStatus::Paper);
    }

    #[test]
    fn mode_c_paper_still_fails_probe_gate_fail_closed() {
        let mut cfg = Config::dev_portable();
        cfg.fill_mode = FillModeCfg::AdversarialRealistic;
        let r = promotion_readiness(&cfg, true, true, clear_stat());
        // Mode C clears the fill-model law, but the laptop cannot attest
        // sequential live edge / sell reliability / reconciled trades — the
        // probe gate must fail closed, never silently pass.
        assert!(!r.live_probe_eligible);
        assert!(r.probe_gate.is_err());
        assert!(r.blocked_on.starts_with("probe_gate:"));
    }

    // ------------------------------------------------------------------------
    // LAW 14 (§51): the FDR/PBO promotion verdict is a CONSULTED hard-blocker.
    // ------------------------------------------------------------------------
    use pump_quant_evaluator::fdr::Hypothesis;
    use pump_quant_evaluator::promotion_verdict::promotion_verdict;

    /// A family whose candidate is not among the BH-FDR discoveries trips the §51
    /// gate; even under Mode C with baselines defeated and drawdown clean, the
    /// readiness report must BLOCK promotion and name the statistical gate as the
    /// binding reason — proving the verdict is consulted, not ignored.
    #[test]
    fn fdr_trip_blocks_promotion_in_readiness() {
        let mut cfg = Config::dev_portable();
        cfg.fill_mode = FillModeCfg::AdversarialRealistic;
        // candidate 2 (p = 0.5) is not a BH discovery at alpha 5% -> FDR blocks;
        // the skilled perf matrix keeps PBO at 0, so the block is FDR-only.
        let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
        let perf = vec![
            vec![100, 100, 100, 100],
            vec![10, 10, 10, 10],
            vec![20, 20, 20, 20],
            vec![30, 30, 30, 30],
        ];
        let v = promotion_verdict(&fam, 50_000, 2, &perf, 5_000);
        assert!(v.blocks() && v.fdr_blocks && !v.pbo_blocks);
        let blocked = promotion_readiness(&cfg, true, true, v);
        assert!(!blocked.live_probe_eligible);
        assert_eq!(blocked.blocked_on, "promotion_verdict:fdr");
        assert!(blocked.stat_verdict.blocks());

        // Same everything, but a CLEAR statistical verdict: the §51 gate no longer
        // binds and the report falls through to the evidence-sufficiency gate —
        // the A/B that proves the statistical gate is what blocked above.
        let clear = promotion_readiness(&cfg, true, true, clear_stat());
        assert!(clear.blocked_on.starts_with("probe_gate:"));
        assert!(!clear.stat_verdict.blocks());
    }

    /// An inadmissible CSCV matrix fails the PBO gate closed and blocks promotion —
    /// overfitting that cannot even be measured is not a silent pass (§51).
    #[test]
    fn pbo_inadmissible_blocks_promotion_in_readiness() {
        let mut cfg = Config::dev_portable();
        cfg.fill_mode = FillModeCfg::AdversarialRealistic;
        let fam = vec![Hypothesis::new(1, 5_000)];
        // single-row perf -> TooFewTrials -> pbo_blocks, pbo_bps None (fail closed).
        let v = promotion_verdict(&fam, 50_000, 1, &[vec![1, 2]], 5_000);
        assert!(v.pbo_blocks && v.pbo_bps.is_none());
        let r = promotion_readiness(&cfg, true, true, v);
        assert!(!r.live_probe_eligible);
        assert!(r.blocked_on.starts_with("promotion_verdict:"));
    }

    #[test]
    fn signal_replay_carries_no_claim() {
        let (status, class) = evidence_of(FillModeCfg::SignalReplay);
        assert_eq!(status, EvidenceStatus::Paper);
        assert_eq!(class, FillModelClass::CausalReplay);
    }

    #[test]
    fn identity_is_deterministic_and_config_sensitive() {
        let a = strategy_identity(&Config::dev_portable());
        let b = strategy_identity(&Config::dev_portable());
        assert_eq!(a, b, "same config -> same identity");
        let mut cfg = Config::dev_portable();
        cfg.gate_expected_move_bps += 1;
        let c = strategy_identity(&cfg);
        assert_ne!(a.strategy_hash, c.strategy_hash);
        assert_ne!(a.config_fnv, c.config_fnv);
        assert_eq!(
            a.protocol_registry_fnv, c.protocol_registry_fnv,
            "registry identity is config-independent"
        );
    }
}
