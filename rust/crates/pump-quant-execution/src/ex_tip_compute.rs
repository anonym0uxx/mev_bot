//! Leaf `ex_tip_compute`: priority / Jito tip sizing from congestion inputs.
//!
//! Distilled from two legacy sources:
//! - `mev/jito-bundle-builder.ts` `computeTip`, which floored a base tip and
//!   scaled it up.
//! - `momentum/sell_engine.rs`'s escalation ladder, where higher urgency levels
//!   added progressively larger priority premiums.
//!
//! The legacy TS used floats; here the whole computation is integer basis-point
//! math with a widened `u128` intermediate (constitution §22).
//!
//! ## Responsibility
//! Turn a base tip plus two live signals — network congestion (basis points)
//! and caller urgency (a small level) — into a single integer tip in lamports.
//!
//! ## Formula
//! ```text
//! congestion_factor_bps = 10_000 + congestion_bps          // 1.00 + congestion
//! urgency_factor_bps    = 10_000 + urgency * URGENCY_STEP_BPS
//! tip = base_tip * congestion_factor_bps / 10_000
//!               * urgency_factor_bps    / 10_000
//! ```
//! Both factors are `>= 10_000` (`1.0x`), so the result is always `>= base_tip`
//! — the configured base is a hard floor, matching the legacy
//! `max(cfg.jito_tip_lamports, ...)` behavior.
//!
//! ## Constitution refs
//! - §22: integer basis-point math only.
//! - Overflow: intermediates are `u128`; the result is saturated back to `u64`.

/// Basis-point premium added per urgency level. Urgency `u` multiplies the tip
/// by `1 + u * 0.5` (each level adds 50%), mirroring the steep priority-fee
/// growth of the legacy escalation ladder.
pub const URGENCY_STEP_BPS: u64 = 5_000;

/// One whole unit expressed in basis points (`1.0 == 10_000 bps`).
pub const BPS_ONE: u64 = 10_000;

/// Compute the tip in lamports from a base tip, congestion, and urgency.
///
/// - `base_tip`: configured minimum tip in lamports (acts as a floor).
/// - `congestion_bps`: network congestion as basis points of extra tip
///   (`0` = idle, `10_000` = double the base for congestion alone).
/// - `urgency`: caller urgency level; each level adds [`URGENCY_STEP_BPS`]
///   (50%) on top of the congestion-scaled tip.
///
/// Returns the scaled tip, saturated into `u64`. Never returns less than
/// `base_tip`.
pub fn compute_tip(base_tip: u64, congestion_bps: u32, urgency: u8) -> u64 {
    let congestion_factor_bps = BPS_ONE.saturating_add(u64::from(congestion_bps));
    let urgency_factor_bps =
        BPS_ONE.saturating_add(u64::from(urgency).saturating_mul(URGENCY_STEP_BPS));

    // Widen to u128 so the two-factor multiply cannot overflow before we divide.
    let mut tip = u128::from(base_tip);
    tip = tip * u128::from(congestion_factor_bps) / u128::from(BPS_ONE);
    tip = tip * u128::from(urgency_factor_bps) / u128::from(BPS_ONE);

    if tip > u128::from(u64::MAX) {
        u64::MAX
    } else {
        tip as u64
    }
}
