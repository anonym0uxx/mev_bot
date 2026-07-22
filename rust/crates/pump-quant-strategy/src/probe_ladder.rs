//! # probe_ladder — capital-scaling probe ladder + wallet-survival floor (criterion 27)
//!
//! Two deterministic leaves for Section 33 Layer-1 sizing:
//!
//! * [`advance_ladder`] — a `ProbeLadder` state machine that grows probe size
//!   **only** on reconciled-positive rungs, contracts/halts on deterioration, and
//!   caps total per-position size. A probe never scales itself up on unverified
//!   or merely-neutral evidence.
//! * [`wallet_floor_guard`] — a hard veto that refuses any size which would push
//!   the reconciled balance below the survival floor (`deployable = balance −
//!   floor`), regardless of what the ladder wants.
//!
//! ## Constitution
//! §22: integer/fixed-point only, explicit overflow (checked/saturating). Pure and
//! deterministic — the reconciled-balance and outcome inputs are supplied by the
//! caller; no clock, RNG, or I/O here.

// ===========================================================================
// Rung sizing schedule
// ===========================================================================

/// Immutable ladder configuration: the probe-size schedule and the hard cap.
///
/// Rung `r` has size `base_probe_lamports * 2^r`, saturated at `max_total_lamports`.
/// The schedule is a pure function of `(base, r)` so live/shadow/replay agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LadderConfig {
    /// Smallest (rung-0) probe size in lamports.
    pub base_probe_lamports: u64,
    /// Highest rung index the ladder may occupy.
    pub max_rung: u8,
    /// Absolute per-position size cap in lamports (never exceeded).
    pub max_total_lamports: u64,
}

impl LadderConfig {
    /// A deterministic fixture used by the property tests.
    pub fn test() -> Self {
        LadderConfig {
            base_probe_lamports: 1_000,
            max_rung: 4,
            max_total_lamports: 12_000,
        }
    }

    /// Size at rung `r`: `base * 2^r`, saturating and clamped to the total cap.
    ///
    /// Deterministic and overflow-safe: the shift saturates before the clamp.
    pub fn size_at_rung(&self, rung: u8) -> u64 {
        let r = rung.min(self.max_rung);
        let mult = 1u64.checked_shl(r as u32).unwrap_or(u64::MAX);
        self.base_probe_lamports
            .saturating_mul(mult)
            .min(self.max_total_lamports)
    }
}

// ===========================================================================
// Ladder state machine (leaf: pl_advance)
// ===========================================================================

/// Reconciled outcome of the most recent rung — the only evidence that may move
/// the ladder. `Positive` requires a *reconciled* (on-chain settled) gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RungOutcome {
    /// Reconciled-positive: the rung settled favorably on chain.
    ReconciledPositive,
    /// Reconciled-neutral: settled but neither confirms nor deteriorates.
    Neutral,
    /// Deterioration: the position is decaying / the probe thesis is failing.
    Deteriorated,
}

/// State of the probe ladder for one position.
///
/// `Probing` occupies a rung while still gathering confirmation; `Scaled` is a
/// rung reached after at least one reconciled-positive advance; `Halted` is
/// terminal (deterioration seen) and never re-advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeLadder {
    /// Still probing at `rung` (rung 0 is the initial probe).
    Probing {
        /// Current rung index.
        rung: u8,
    },
    /// Scaled to `rung` after reconciled-positive confirmation.
    Scaled {
        /// Current rung index.
        rung: u8,
    },
    /// Deterministically halted after deterioration — no further scale-in.
    Halted {
        /// Rung the ladder halted at (for de-risk sizing / audit).
        rung: u8,
    },
}

impl ProbeLadder {
    /// A fresh ladder at the initial probe (rung 0).
    #[inline]
    pub fn new() -> Self {
        ProbeLadder::Probing { rung: 0 }
    }

    /// The current rung index regardless of variant.
    #[inline]
    pub fn rung(self) -> u8 {
        match self {
            ProbeLadder::Probing { rung }
            | ProbeLadder::Scaled { rung }
            | ProbeLadder::Halted { rung } => rung,
        }
    }

    /// The current planned position size for this ladder state under `cfg`.
    #[inline]
    pub fn planned_size(self, cfg: &LadderConfig) -> u64 {
        cfg.size_at_rung(self.rung())
    }
}

impl Default for ProbeLadder {
    fn default() -> Self {
        Self::new()
    }
}

/// Advance the probe ladder on a reconciled rung outcome (leaf **pl_advance**).
///
/// * `ReconciledPositive` promotes to the next rung (up to `cfg.max_rung`) and
///   moves the ladder into `Scaled` — this is the **only** transition that grows
///   size, and only on reconciled evidence.
/// * `Neutral` holds the current rung (no growth on unverified/flat evidence).
/// * `Deteriorated` halts the ladder at its current rung; a halted ladder is
///   terminal and stays halted on every subsequent outcome.
///
/// Pure and deterministic: identical `(state, outcome)` always yields an
/// identical next state.
pub fn advance_ladder(state: ProbeLadder, outcome: RungOutcome, cfg: &LadderConfig) -> ProbeLadder {
    // Halted is terminal: never re-advance, whatever the outcome.
    if let ProbeLadder::Halted { rung } = state {
        return ProbeLadder::Halted { rung };
    }
    match outcome {
        RungOutcome::Deteriorated => ProbeLadder::Halted { rung: state.rung() },
        RungOutcome::Neutral => state,
        RungOutcome::ReconciledPositive => {
            let next = state.rung().saturating_add(1).min(cfg.max_rung);
            ProbeLadder::Scaled { rung: next }
        }
    }
}

// ===========================================================================
// Wallet-survival floor guard (leaf: pl_wallet_floor)
// ===========================================================================

/// Verdict of the wallet-survival floor guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloorVerdict {
    /// The size fits entirely inside deployable capital (`balance − floor`).
    Allowed,
    /// The size would push the reconciled balance below the survival floor.
    RefusedBelowFloor,
}

/// Absolute minimum survival floor: `0.5 SOL` in lamports (§1).
///
/// The derived floor is never lower than this, no matter how small the verified
/// balance is — half a SOL of survival capital is always reserved.
pub const MIN_SURVIVAL_FLOOR_LAMPORTS: u64 = 500_000_000;

/// Default floor fraction: `0.5` expressed in bps (`5_000 bps == 50%`) (§1).
pub const DEFAULT_FLOOR_FRACTION_BPS: u32 = 5_000;

/// Basis-points scale used by the floor derivation (`10_000 bps == 100%`).
const FLOOR_BPS_SCALE: u128 = 10_000;

/// Derive the wallet-survival floor from a verified starting balance (§1).
///
/// ```text
///     floor = max(MIN_SURVIVAL_FLOOR_LAMPORTS,
///                 verified_starting_balance * floor_fraction_bps / 10_000)
/// ```
///
/// The fraction is applied in `u128` (saturating to `u64::MAX`) and then floored
/// at the absolute `0.5 SOL` minimum. This is the derivation the existing
/// [`wallet_floor_guard`] enforcement veto consumes: the caller re-derives it on
/// every verified capital change and feeds the result in as `floor_lamports`.
/// Pure, integer, deterministic.
#[inline]
pub fn derive_survival_floor(
    verified_starting_balance_lamports: u64,
    floor_fraction_bps: u32,
) -> u64 {
    let fractional =
        (verified_starting_balance_lamports as u128 * floor_fraction_bps as u128) / FLOOR_BPS_SCALE;
    let fractional = fractional.min(u64::MAX as u128) as u64;
    fractional.max(MIN_SURVIVAL_FLOOR_LAMPORTS)
}

/// Deployable capital = `verified_balance − floor`, saturating at zero (§1).
///
/// The single verified figure that every downstream probe tier, exposure limit,
/// and calibration cap must derive from (see
/// [`capital_allocator::derive_sizing`](crate::capital_allocator::derive_sizing)).
/// A floor at or above the balance yields zero deployable capital. Pure integer,
/// deterministic.
#[inline]
pub fn deployable_capital(verified_balance_lamports: u64, floor_lamports: u64) -> u64 {
    verified_balance_lamports.saturating_sub(floor_lamports)
}

/// Hard survival-floor veto (leaf **pl_wallet_floor**).
///
/// Deployable capital is `reconciled_balance − floor` (saturating at zero). A
/// `size` strictly greater than deployable is refused; a size that exactly
/// consumes deployable capital (leaving the balance at the floor) is allowed.
/// This veto overrides the ladder: even a reconciled-positive rung cannot breach
/// the floor. Pure, integer, deterministic.
#[inline]
pub fn wallet_floor_guard(
    size_lamports: u64,
    reconciled_balance_lamports: u64,
    floor_lamports: u64,
) -> FloorVerdict {
    let deployable = reconciled_balance_lamports.saturating_sub(floor_lamports);
    if size_lamports > deployable {
        FloorVerdict::RefusedBelowFloor
    } else {
        FloorVerdict::Allowed
    }
}

// ===========================================================================
// Tests — survival-floor + deployable-capital derivation (leaf: pl_floor_derive)
// ===========================================================================

#[cfg(test)]
mod floor_derive_tests {
    use super::*;

    const SOL: u64 = 1_000_000_000;

    #[test]
    fn fraction_dominates_above_the_minimum() {
        // 10 SOL * 50% = 5 SOL, well above the 0.5 SOL absolute minimum.
        assert_eq!(
            derive_survival_floor(10 * SOL, DEFAULT_FLOOR_FRACTION_BPS),
            5 * SOL
        );
        // 4 SOL * 25% = 1 SOL.
        assert_eq!(derive_survival_floor(4 * SOL, 2_500), SOL);
    }

    #[test]
    fn minimum_floor_dominates_for_small_balances() {
        // 0.5 SOL * 50% = 0.25 SOL -> clamped up to the 0.5 SOL minimum.
        assert_eq!(
            derive_survival_floor(SOL / 2, DEFAULT_FLOOR_FRACTION_BPS),
            MIN_SURVIVAL_FLOOR_LAMPORTS
        );
        // Zero balance still reserves the absolute minimum.
        assert_eq!(
            derive_survival_floor(0, DEFAULT_FLOOR_FRACTION_BPS),
            MIN_SURVIVAL_FLOOR_LAMPORTS
        );
        // A zero fraction still yields the absolute minimum.
        assert_eq!(
            derive_survival_floor(100 * SOL, 0),
            MIN_SURVIVAL_FLOOR_LAMPORTS
        );
    }

    #[test]
    fn exact_boundary_at_one_sol_balance() {
        // 1 SOL * 50% = 0.5 SOL exactly == the minimum (either branch agrees).
        assert_eq!(
            derive_survival_floor(SOL, DEFAULT_FLOOR_FRACTION_BPS),
            MIN_SURVIVAL_FLOOR_LAMPORTS
        );
    }

    #[test]
    fn full_fraction_never_overflows() {
        // 100% of u64::MAX saturates cleanly in u128 math.
        assert_eq!(derive_survival_floor(u64::MAX, 10_000), u64::MAX);
    }

    #[test]
    fn deployable_is_balance_minus_floor_saturating() {
        assert_eq!(deployable_capital(10 * SOL, 5 * SOL), 5 * SOL);
        // Floor equal to balance -> zero deployable.
        assert_eq!(deployable_capital(5 * SOL, 5 * SOL), 0);
        // Floor above balance -> saturates to zero, never underflows.
        assert_eq!(deployable_capital(3 * SOL, 5 * SOL), 0);
    }

    #[test]
    fn derivation_feeds_the_enforcement_veto_consistently() {
        // End-to-end: derive floor, derive deployable, and confirm the guard
        // admits exactly up to deployable and refuses one lamport more.
        let balance = 8 * SOL;
        let floor = derive_survival_floor(balance, DEFAULT_FLOOR_FRACTION_BPS); // 4 SOL
        let deployable = deployable_capital(balance, floor); // 4 SOL
        assert_eq!(floor, 4 * SOL);
        assert_eq!(deployable, 4 * SOL);
        assert_eq!(
            wallet_floor_guard(deployable, balance, floor),
            FloorVerdict::Allowed
        );
        assert_eq!(
            wallet_floor_guard(deployable + 1, balance, floor),
            FloorVerdict::RefusedBelowFloor
        );
    }
}
