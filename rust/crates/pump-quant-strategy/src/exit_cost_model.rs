//! # exit_cost_model — phase-asymmetric executable-exit-cost model (criterion 101)
//!
//! The executable exit-cost model is **phase-asymmetric** and each phase's model is
//! forbidden from being applied to the other:
//!
//! * **Pre-migration (curve):** analytic from decoded bonding-curve state —
//!   a curve-schedule impact term + an empirically-calibrated latency-window
//!   adverse-drift term + measured failure/retry adders, the last inflating the
//!   whole by the attempt multiplier `1 / (1 − failure_rate)` ([`curve_exit_cost_bps`]).
//! * **Post-migration (pool):** a reserves / realized-impact estimate from the
//!   constant-product pool state + latency drift ([`pool_exit_cost_bps`]).
//!
//! [`executable_exit_cost_bps`] dispatches on [`Phase`] and enforces the ban:
//! supplying the pool model in the curve phase (or vice-versa) returns
//! [`PhaseModelError`] rather than silently mispricing.
//!
//! ## Constitution
//! §22 (no floats — bps integer/fixed-point, `u128` intermediates), §34.4/§21.7
//! (decoded-curve/reserve impact), §715(b) expected-landing-state evaluation. Pure
//! and deterministic given decoded state.

use crate::scalp_position::Phase;

/// Basis-points scale (`10_000 == 100%`).
pub const BPS: u32 = 10_000;

/// Decoded bonding-curve state + adverse-cost inputs for the pre-migration model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveExitInputs {
    /// Effective curve reserve (lamports) governing schedule impact.
    pub curve_reserve_lamports: u64,
    /// Size being exited (lamports).
    pub size_lamports: u64,
    /// Calibrated latency-window adverse drift, bps.
    pub latency_drift_bps: u32,
    /// Measured failure rate, bps (`>= 10_000` ⇒ certain failure ⇒ `None`).
    pub failure_rate_bps: u32,
    /// Measured retry adder, bps.
    pub retry_adder_bps: u32,
}

/// Decoded pool reserves + latency for the post-migration model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolExitInputs {
    /// Base (token) reserve, lamports.
    pub base_reserve_lamports: u64,
    /// Quote reserve, lamports (kept for symmetry / future terms).
    pub quote_reserve_lamports: u64,
    /// Size being exited (lamports of base).
    pub size_lamports: u64,
    /// Calibrated latency-window adverse drift, bps.
    pub latency_drift_bps: u32,
}

/// Why a phase-model dispatch was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseModelError {
    /// The curve model was applied outside the curve phase, or missing when needed.
    CurveModelMisapplied,
    /// The pool model was applied outside the pool phase, or missing when needed.
    PoolModelMisapplied,
    /// The pre-migration model has certain failure (`failure_rate_bps >= 10_000`).
    CertainFailure,
}

/// Pre-migration curve-schedule exit cost in bps (leaf helper).
///
/// `base = curve_schedule_impact + latency_drift + retry_adder`, where the
/// schedule impact is the linear reserve approximation `size · 10_000 /
/// curve_reserve`. The base is then inflated by the attempt multiplier
/// `10_000 / (10_000 − failure_rate)` because failed exits still pay cost and land
/// nothing. Returns `Err(CertainFailure)` when `failure_rate_bps >= 10_000`.
/// Saturates at `u32::MAX`; all intermediates are `u128`.
pub fn curve_exit_cost_bps(inp: &CurveExitInputs) -> Result<u32, PhaseModelError> {
    if inp.failure_rate_bps >= BPS {
        return Err(PhaseModelError::CertainFailure);
    }
    let reserve = (inp.curve_reserve_lamports as u128).max(1);
    let schedule_impact = (inp.size_lamports as u128).saturating_mul(BPS as u128) / reserve;
    let base = schedule_impact
        .saturating_add(inp.latency_drift_bps as u128)
        .saturating_add(inp.retry_adder_bps as u128);
    // Attempt-multiplier inflation: base * 10_000 / (10_000 - failure_rate).
    let denom = (BPS - inp.failure_rate_bps) as u128;
    let inflated = base.saturating_mul(BPS as u128) / denom;
    Ok(inflated.min(u32::MAX as u128) as u32)
}

/// Post-migration pool realized-impact exit cost in bps (leaf helper).
///
/// Constant-product realized impact of selling `size` base into the pool is
/// `size · 10_000 / (base_reserve + size)` — the proceeds shortfall versus the
/// marginal price — plus the latency drift. Saturates at `u32::MAX`; `u128`
/// intermediates.
pub fn pool_exit_cost_bps(inp: &PoolExitInputs) -> u32 {
    let size = inp.size_lamports as u128;
    let denom = (inp.base_reserve_lamports as u128)
        .saturating_add(size)
        .max(1);
    let impact = size.saturating_mul(BPS as u128) / denom;
    let total = impact.saturating_add(inp.latency_drift_bps as u128);
    total.min(u32::MAX as u128) as u32
}

/// Phase-dispatching executable exit-cost model (leaf **ec_cost**).
///
/// * `Phase::Curve` — requires the curve model and **forbids** the pool model:
///   `pool.is_some()` or `curve.is_none()` → [`PhaseModelError`].
/// * `Phase::Pool` — requires the pool model and **forbids** the curve model.
///
/// This is what makes the two models phase-asymmetric and non-interchangeable: a
/// caller cannot price a curve exit with pool reserves or a pool exit with the
/// curve schedule. Pure and deterministic.
pub fn executable_exit_cost_bps(
    phase: Phase,
    curve: Option<&CurveExitInputs>,
    pool: Option<&PoolExitInputs>,
) -> Result<u32, PhaseModelError> {
    match phase {
        Phase::Curve => {
            if pool.is_some() {
                return Err(PhaseModelError::PoolModelMisapplied);
            }
            let c = curve.ok_or(PhaseModelError::CurveModelMisapplied)?;
            curve_exit_cost_bps(c)
        }
        Phase::Pool => {
            if curve.is_some() {
                return Err(PhaseModelError::CurveModelMisapplied);
            }
            let p = pool.ok_or(PhaseModelError::PoolModelMisapplied)?;
            Ok(pool_exit_cost_bps(p))
        }
    }
}
