//! §101 (CRITERION 101) curve-analytic authenticity estimator — a phase-selected,
//! REPORT-plane alternative to the reserve/impact estimator.
//!
//! The live authenticity/supported-appreciation screen prices the appreciation a
//! given net inflow can *legitimately* cause with a **reserve/impact** (linear
//! marginal) model: `supported ≈ net · 10_000 / reserve`. That linear marginal is
//! the correct first-order term for an AMM **pool**, but on a pre-migration
//! **bonding curve** the finite price move along the quadratic schedule is larger
//! than the marginal by a convexity term. This module adds the distinct
//! **curve-analytic** estimator and a phase dispatcher that selects it for the
//! curve phase.
//!
//! ## Digest-safety: report-plane alternative, no `Config` field
//!
//! The §19 config-identity digest seed folds `format!("{cfg:?}")` into the journal,
//! so adding ANY `Config` field would move the golden digest. This estimator is
//! therefore exposed as a **report-plane alternative** (per the criterion-101
//! directive's escape hatch) rather than a `Config` flag: the selector's default
//! (`use_curve_analytic = false`) reproduces the reserve/impact path byte-for-byte,
//! and nothing here is read by a live sizing/gating/screen decision — so the golden
//! decision path is unchanged. Integer/fixed-point only (§22).

use pump_quant_strategy::scalp_position::Phase;

/// Basis-point scale (`10_000 == 100%`).
const BPS: u128 = 10_000;

/// Which supported-appreciation model priced a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthPhaseModel {
    /// Linear reserve/impact marginal (the pre-§101 live model; AMM-pool-correct).
    ReserveImpact,
    /// Bonding-curve-analytic finite move along the quadratic schedule.
    CurveAnalytic,
}

/// Reserve/impact (linear marginal) supported appreciation, bps: `net·10_000/reserve`.
/// This is the estimator the live authenticity screen already uses. `reserve` is
/// clamped to ≥ 1 to keep the division total and deterministic (§22).
#[inline]
#[must_use]
pub fn reserve_impact_supported_bps(net_inflow: u64, reserve: u64) -> u128 {
    let reserve = u128::from(reserve).max(1);
    u128::from(net_inflow).saturating_mul(BPS) / reserve
}

/// Curve-analytic supported appreciation, bps. On a quadratic bonding-curve
/// schedule (price ∝ reserve²), adding `net` to a reserve `R` moves price by the
/// FINITE ratio `((R+net)² − R²)/R² = (2·net·R + net²)/R²`, in bps. This exceeds
/// the linear marginal `net/R` by the convexity term `net²/R²` — the curve's
/// second-order self-impact a linear model omits. `reserve` clamped to ≥ 1.
#[inline]
#[must_use]
pub fn curve_analytic_supported_bps(net_inflow: u64, reserve: u64) -> u128 {
    let r = u128::from(reserve).max(1);
    let net = u128::from(net_inflow);
    // (2·net·R + net²) · 10_000 / R²   — all u128, saturating.
    let numer = net
        .saturating_mul(2)
        .saturating_mul(r)
        .saturating_add(net.saturating_mul(net));
    numer.saturating_mul(BPS) / r.saturating_mul(r)
}

/// Select the supported-appreciation model for `phase`. The POOL phase always uses
/// the reserve/impact model. The CURVE phase uses the curve-analytic model ONLY
/// when `use_curve_analytic` is set; with it unset (the default) the curve phase
/// falls back to reserve/impact — byte-identical to the pre-§101 behaviour, so the
/// live path is unchanged when this alternative is not opted into.
#[inline]
#[must_use]
pub fn select_model(phase: Phase, use_curve_analytic: bool) -> AuthPhaseModel {
    match phase {
        Phase::Pool => AuthPhaseModel::ReserveImpact,
        Phase::Curve => {
            if use_curve_analytic {
                AuthPhaseModel::CurveAnalytic
            } else {
                AuthPhaseModel::ReserveImpact
            }
        }
    }
}

/// Supported appreciation (bps) under the phase-selected model.
#[inline]
#[must_use]
pub fn supported_bps(
    phase: Phase,
    use_curve_analytic: bool,
    net_inflow: u64,
    reserve: u64,
) -> u128 {
    match select_model(phase, use_curve_analytic) {
        AuthPhaseModel::ReserveImpact => reserve_impact_supported_bps(net_inflow, reserve),
        AuthPhaseModel::CurveAnalytic => curve_analytic_supported_bps(net_inflow, reserve),
    }
}

/// Authenticity margin (bps): `observed_appreciation − supported`. Positive ⇒ the
/// observed move exceeds what the phase-selected model says the inflow supports
/// (fabrication-suspect); non-positive ⇒ within support. Report-plane only.
#[inline]
#[must_use]
pub fn authenticity_margin_bps(
    observed_appreciation_bps: u64,
    phase: Phase,
    use_curve_analytic: bool,
    net_inflow: u64,
    reserve: u64,
) -> i128 {
    let supported = supported_bps(phase, use_curve_analytic, net_inflow, reserve);
    i128::from(observed_appreciation_bps) - supported.min(i128::MAX as u128) as i128
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A/B: on the SAME curve-phase input the curve-analytic estimator differs from
    /// the reserve/impact estimator — it is strictly larger by the convexity term.
    #[test]
    fn curve_analytic_differs_from_reserve_impact_on_curve_input() {
        let net = 4_000u64;
        let reserve = 100_000u64;
        let linear = reserve_impact_supported_bps(net, reserve);
        let curve = curve_analytic_supported_bps(net, reserve);
        // Reserve/impact (AMM marginal) = net·10_000/R = 400 bps.
        assert_eq!(linear, 400);
        // Curve-analytic finite move (2·net·R + net²)·10_000/R² = 816 bps.
        assert_eq!(curve, 816);
        assert_ne!(linear, curve, "the two estimators must differ");
        assert!(curve > linear, "curve schedule prices a larger finite move");
    }

    /// The default (curve-analytic OFF) reproduces the reserve/impact path for BOTH
    /// phases — this is why the live screen is byte-identical when not opted in.
    #[test]
    fn default_off_is_reserve_impact_for_both_phases() {
        assert_eq!(
            select_model(Phase::Curve, false),
            AuthPhaseModel::ReserveImpact
        );
        assert_eq!(
            select_model(Phase::Pool, false),
            AuthPhaseModel::ReserveImpact
        );
        let net = 4_000u64;
        let reserve = 100_000u64;
        assert_eq!(
            supported_bps(Phase::Curve, false, net, reserve),
            reserve_impact_supported_bps(net, reserve),
        );
    }

    /// The curve-analytic model is reachable ONLY in the curve phase when opted in;
    /// the pool phase always stays reserve/impact regardless of the flag.
    #[test]
    fn curve_analytic_selected_only_for_curve_phase_when_enabled() {
        assert_eq!(
            select_model(Phase::Curve, true),
            AuthPhaseModel::CurveAnalytic
        );
        assert_eq!(
            select_model(Phase::Pool, true),
            AuthPhaseModel::ReserveImpact,
            "pool phase never uses the curve-analytic model",
        );
    }

    /// The authenticity VERDICT can flip between the two models on the same input:
    /// an observed move that clears the linear support but not the (larger)
    /// curve-analytic support is fabrication-suspect under reserve/impact yet clean
    /// under the curve-analytic model — the substantive A/B difference.
    #[test]
    fn verdict_differs_between_models() {
        let reserve = 100_000u64;
        let net = 4_000u64;
        // reserve/impact support = 400 bps; curve-analytic support = 816 bps.
        // Observe 410 bps — above the linear bar, below the curve bar.
        let observed = 410u64;
        let linear_margin = authenticity_margin_bps(observed, Phase::Curve, false, net, reserve);
        let curve_margin = authenticity_margin_bps(observed, Phase::Curve, true, net, reserve);
        assert!(
            linear_margin > 0,
            "reserve/impact flags it as beyond support"
        );
        assert!(curve_margin < 0, "curve-analytic sees it as within support");
    }

    #[test]
    fn zero_reserve_is_total_not_a_panic() {
        // Reserve clamped to 1 — deterministic, never a division-by-zero.
        let _ = reserve_impact_supported_bps(10, 0);
        let _ = curve_analytic_supported_bps(10, 0);
    }
}
