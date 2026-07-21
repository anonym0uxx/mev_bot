//! # position_scaler — HotPathPositionScaler intra-position scale-in/out (criterion 75)
//!
//! A single pure decision function, [`scale_decision`], that turns the current
//! position + flow state and the economic-gate viability band into an intra-position
//! scale increment. It is the **sole** scale-in/out path: because it is a pure
//! function of its inputs, live, shadow, and replay call the identical code and,
//! given the identical `(ScalpPositionState, FlowState, SizeBand, current_size)`,
//! produce the identical [`ScaleAction`] — this is provable by test (drive the
//! same event sequence twice → identical decision).
//!
//! It reuses the §22 reducer types ([`crate::scalp_position`]) and the economic
//! gate ([`crate::economic_gate::SizeBand`]) rather than reimplementing sizing:
//! scale-ins stay strictly inside `[x_min, x_max]` and never grow past `x_max`.
//!
//! ## Constitution
//! §33: enter with a minimal probe, scale in **only** on deterministic confirmation,
//! cap total per-position size at `x_max`; §22: integer/fixed-point, saturating.
//! Deterministic — confirmation is computed from decoded flow, never a clock/RNG.

use crate::economic_gate::{SizeBand, Verdict};
use crate::scalp_position::{FlowState, ScalpPositionState};

/// The intra-position scale action produced by the scaler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleAction {
    /// Add `add` lamports to the position (a scale-in rung).
    ScaleIn {
        /// Lamports to add.
        add: u64,
    },
    /// Hold the current size (no confirmation, or already at cap).
    Hold,
    /// Remove `remove` lamports (de-risk on deterioration).
    ScaleOut {
        /// Lamports to remove.
        remove: u64,
    },
    /// The economic gate refuses the candidate — no live capital is deployed.
    Blocked,
}

/// Whether deterministic confirmation to scale in is present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// Confirmed: authentic, accelerating, favorable, in-profit flow.
    Confirmed,
    /// Denied — one or more confirmation conditions failed.
    Denied,
}

/// Deterministic scale-in confirmation from decoded flow (helper leaf).
///
/// Confirmation requires **all** of: net-positive cumulative volume delta
/// (`cvd_fp > 0`), authenticity at or above `min_authenticity_fp` (fabricated
/// bursts are rejected), non-negative arrival acceleration (flow not decelerating),
/// and the position in profit (`last_price_fp > entry_price_fp`). Pure over the
/// reducer state.
pub fn scale_confirmation(
    pos: &ScalpPositionState,
    flow: &FlowState,
    min_authenticity_fp: u32,
) -> Confirmation {
    let in_profit = pos.last_price_fp > pos.entry_price_fp;
    let authentic = flow.authenticity_fp >= min_authenticity_fp;
    let favorable = flow.cvd_fp > 0 && flow.arrival_accel_fp >= 0;
    if in_profit && authentic && favorable {
        Confirmation::Confirmed
    } else {
        Confirmation::Denied
    }
}

/// Whether decoded flow shows deterioration warranting de-risk (helper leaf).
///
/// Deterioration is net-negative flow (`cvd_fp < 0`) **or** authenticity below the
/// fabrication threshold — either is grounds to shed risk.
pub fn scale_deterioration(flow: &FlowState, min_authenticity_fp: u32) -> bool {
    flow.cvd_fp < 0 || flow.authenticity_fp < min_authenticity_fp
}

/// The single intra-position scale-in/out decision (leaf **ps_scale**).
///
/// Rules, evaluated in order:
/// 1. If the economic gate refuses the candidate (`band.verdict == Refuse`) →
///    [`ScaleAction::Blocked`] (never deploy live capital where non-viable).
/// 2. If `current_size == 0` → [`ScaleAction::Hold`]: the scaler manages an open
///    position; opening the probe is a separate decision.
/// 3. If flow shows deterioration → [`ScaleAction::ScaleOut`] removing one rung
///    (`min(current_size, x_min)`), de-risking toward flat.
/// 4. If confirmed and below the `x_max` cap → [`ScaleAction::ScaleIn`] by one
///    rung (`x_min`), topped so `current + add <= x_max`.
/// 5. Otherwise → [`ScaleAction::Hold`] (at cap, or unconfirmed).
///
/// The scale-in rung is the gate's `x_min` (the smallest floor-clearing size), so
/// every added increment itself clears the round-trip cost floor and the running
/// total is capped at `x_max`. Pure and deterministic.
pub fn scale_decision(
    pos: &ScalpPositionState,
    flow: &FlowState,
    band: &SizeBand,
    current_size_lamports: u64,
    min_authenticity_fp: u32,
) -> ScaleAction {
    if band.verdict == Verdict::Refuse {
        return ScaleAction::Blocked;
    }
    if current_size_lamports == 0 {
        return ScaleAction::Hold;
    }

    if scale_deterioration(flow, min_authenticity_fp) {
        let rung = band.x_min.max(1);
        let remove = rung.min(current_size_lamports);
        return ScaleAction::ScaleOut { remove };
    }

    if scale_confirmation(pos, flow, min_authenticity_fp) == Confirmation::Confirmed {
        if current_size_lamports >= band.x_max {
            return ScaleAction::Hold; // already at the per-position cap.
        }
        let headroom = band.x_max - current_size_lamports;
        let rung = band.x_min.max(1);
        let add = rung.min(headroom);
        if add == 0 {
            return ScaleAction::Hold;
        }
        return ScaleAction::ScaleIn { add };
    }

    ScaleAction::Hold
}
