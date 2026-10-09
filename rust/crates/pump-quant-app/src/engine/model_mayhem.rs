//! Operator decision 2026-10-09: **SKIP Mayhem-mode coins** in the paper model lane.
//!
//! A pump.fun BondingCurve with `is_mayhem_mode = true` (byte 81) has a program-managed virtual-SOL offset that
//! moves between trades (proc/OFFSET_bM3a_REPORT.md: captured curve 9zsF8bqM.. lines 377 -> 519), so the canonical
//! constant-offset curve model the executor and the trained prompt assume does not hold. Those markets are
//! excluded from the paper cohort BY NAME:
//!
//! * `refuse:mayhem_mode_excluded`  -- the curve's decoded mode is Mayhem;
//! * `refuse:curve_mode_unknown`    -- no decoded mode has been observed for the market (fail-closed: absence is
//!   never read as "not Mayhem"; a short/foreign/noncanonical account emits no mode event at all).
//!
//! The gate runs (a) before a prompt is cut at admission, (b) again when a verdict comes back (revalidation
//! against current state), and (c) on a management ADD (risk-increasing). REDUCE/EXIT and protection of an
//! already-held position are never blocked by it: the exclusion stops NEW exposure only.
//!
//! The mode map is armed-only (fed only while the model lane is on), bounded by the registry cap, and sticky:
//! a market once observed in Mayhem mode stays excluded even if a later observation disagrees (a conflicting
//! observation is counted as `mayhem:mode_conflict`).

use super::Engine;

/// Why a market is outside the paper cohort on curve-mode grounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveModeExclusion {
    /// The decoded BondingCurve says `is_mayhem_mode = true`.
    MayhemMode,
    /// No decoded mode observed for this market (fail-closed).
    ModeUnknown,
}

impl CurveModeExclusion {
    /// The named refusal reason (report key suffix after `refuse:`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CurveModeExclusion::MayhemMode => "mayhem_mode_excluded",
            CurveModeExclusion::ModeUnknown => "curve_mode_unknown",
        }
    }

    /// The named refusal for a bonding-curve QUOTE or MARK (exec_quote::curve_buy / curve_sell / spot price).
    /// Those formulas are validated only on the Ordinary population (non-Mayhem, SOL quote; proc/OFFSET_v4_REPORT.md
    /// §1). A Mayhem or mode-unknown curve does not inherit them: no fill, sell quote, liquidation value or mark is
    /// produced from its reserves. The raw reserves remain recorded as evidence (the curve cache is untouched).
    #[must_use]
    pub const fn quote_refusal(self) -> &'static str {
        match self {
            CurveModeExclusion::MayhemMode => "curve_quote_unsupported:mayhem_mode",
            CurveModeExclusion::ModeUnknown => "curve_quote_unsupported:curve_mode_unknown",
        }
    }
}

/// Cap on markets whose mode is remembered (same bound as the model registry). A market beyond the cap
/// stays UNKNOWN and is therefore refused -- the bound can never admit.
const CURVE_MODE_CAP: usize = 100_000;

impl Engine {
    /// Record one decoded curve-mode observation (from `AppEvent::CurveModeObserved`).
    pub(super) fn model_observe_curve_mode(&mut self, mint: [u8; 32], mayhem: bool, _slot: u64) {
        match self.model_curve_mayhem.get(&mint).copied() {
            Some(prev) => {
                if prev != mayhem {
                    self.mrep("mayhem:mode_conflict");
                }
                if mayhem && !prev {
                    // Sticky: a Mayhem observation always wins.
                    self.model_curve_mayhem.insert(mint, true);
                }
            }
            None => {
                if self.model_curve_mayhem.len() >= CURVE_MODE_CAP {
                    self.mrep("mayhem:mode_cap_unknown");
                    return;
                }
                self.model_curve_mayhem.insert(mint, mayhem);
                self.mrep(if mayhem {
                    "mayhem:mode_observed_mayhem"
                } else {
                    "mayhem:mode_observed_canonical"
                });
            }
        }
    }

    /// `None` when the market is inside the cohort on curve-mode grounds (decoded, not Mayhem); otherwise the
    /// named exclusion. Fail-closed: an unobserved market is `ModeUnknown`.
    #[must_use]
    pub fn model_curve_mode_exclusion(&self, mint: &[u8; 32]) -> Option<CurveModeExclusion> {
        match self.model_curve_mayhem.get(mint) {
            Some(false) => None,
            Some(true) => Some(CurveModeExclusion::MayhemMode),
            None => Some(CurveModeExclusion::ModeUnknown),
        }
    }

    /// Read-only (tests, status): the decoded mode, `None` = unknown.
    #[must_use]
    pub fn model_curve_mode(&self, mint: &[u8; 32]) -> Option<bool> {
        self.model_curve_mayhem.get(mint).copied()
    }
}
