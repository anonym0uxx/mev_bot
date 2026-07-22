//! # emergency_action — emergency-fix risk-reduction guard (criterion 58)
//!
//! A deterministic guard ([`evaluate_emergency`]) that admits a proposed emergency
//! action only when it is **monotonically non-increasing** on every risk/exposure
//! axis relative to the current settings — an emergency fix may disable entries,
//! reduce size, tighten a bound, or shrink route authority, but may never increase
//! size, loosen risk, enable entries, or expand route authority (constitution
//! §42). Any risk-increasing axis rejects the action; an admitted action is
//! automatically flagged for mandatory retrospective validation (quarantine).
//!
//! ## Constitution
//! §42 Emergency Fix Boundary. §22 integer/fixed-point; deterministic, pure.

/// Risk/exposure parameters an emergency action may touch. Every field is
/// oriented so that *smaller / more-restrictive* is *less* risky.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskParams {
    /// Maximum position size, lamports (lower = less risk).
    pub max_size_lamports: u64,
    /// Total exposure limit, lamports (lower = less risk).
    pub exposure_limit_lamports: u64,
    /// Slippage tolerance, bps (lower = tighter = less risk).
    pub slippage_tolerance_bps: u32,
    /// Whether new entries are enabled (`false` = safer).
    pub entries_enabled: bool,
    /// Route-authority breadth id (lower = fewer routes = less risk).
    pub route_authority: u32,
}

/// The specific risk axis a rejected action would have increased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskIncrease {
    /// Max size increased.
    SizeIncreased,
    /// Exposure limit increased.
    ExposureIncreased,
    /// Slippage tolerance loosened.
    SlippageLoosened,
    /// Entries were enabled from a disabled state.
    EntriesEnabled,
    /// Route authority expanded.
    RouteAuthorityExpanded,
}

/// The emergency-action verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmergencyVerdict {
    /// Admitted; `quarantined` is always `true` (mandatory retrospective replay).
    Accepted {
        /// Whether the action is quarantined pending retrospective validation.
        quarantined: bool,
    },
    /// Rejected because it would increase risk on `reason`.
    Rejected {
        /// The offending risk axis.
        reason: RiskIncrease,
    },
}

/// Evaluate a proposed emergency action against current settings (leaf **ea_guard**).
///
/// Admits iff the proposed parameters are non-increasing on *every* risk axis:
/// `max_size`, `exposure`, `slippage`, `route_authority` may only stay equal or
/// fall, and `entries_enabled` may only stay equal or go `true → false`. The first
/// violating axis (checked in a fixed order) determines the reject reason. An
/// admitted action is always quarantined. Pure and deterministic.
pub fn evaluate_emergency(current: &RiskParams, proposed: &RiskParams) -> EmergencyVerdict {
    if proposed.max_size_lamports > current.max_size_lamports {
        return EmergencyVerdict::Rejected {
            reason: RiskIncrease::SizeIncreased,
        };
    }
    if proposed.exposure_limit_lamports > current.exposure_limit_lamports {
        return EmergencyVerdict::Rejected {
            reason: RiskIncrease::ExposureIncreased,
        };
    }
    if proposed.slippage_tolerance_bps > current.slippage_tolerance_bps {
        return EmergencyVerdict::Rejected {
            reason: RiskIncrease::SlippageLoosened,
        };
    }
    // Enabling entries from a disabled state is a risk increase.
    if proposed.entries_enabled && !current.entries_enabled {
        return EmergencyVerdict::Rejected {
            reason: RiskIncrease::EntriesEnabled,
        };
    }
    if proposed.route_authority > current.route_authority {
        return EmergencyVerdict::Rejected {
            reason: RiskIncrease::RouteAuthorityExpanded,
        };
    }
    EmergencyVerdict::Accepted { quarantined: true }
}
