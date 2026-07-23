//! §36 **per-registry decoded custom-error table + 6-class failure taxonomy**.
//!
//! On-chain program failures surface as Anchor *custom error codes* (the
//! `Custom(u32)` variant of `InstructionError`), which are **per-program**:
//! the same numeric code means different things on pump.fun versus PumpSwap.
//! This module records, per [`Venue`], the mapping from custom code to a named
//! [`DecodedProgramError`], and layers a richer **6-class failure taxonomy**
//! ([`FailureClass6`]) on top for the executor/engine to consult.
//!
//! This is an **additive** layer. The existing 3-class taxonomy in
//! `pump_quant_strategy::safety_integrity` (`Transient` / `Construction` /
//! `Unknown`) is unchanged and still authoritative for the coarse
//! retry-with-capital / quarantine decision; the 6-class taxonomy here is the
//! finer decode the exec plane will use for route selection, version-drift
//! handling, and state-drift re-planning.
//!
//! The code→name tables are **recorded metadata** (§18.2 discipline): they are
//! curated constants, seeded from the programs' published Anchor error enums,
//! and bumped only when a human re-verifies against chain. Nothing here reads a
//! clock, RNG, network, or float; every function is a pure, total lookup /
//! match.
//!
//! ## Constitution
//! * §36 — decoded custom-error table + failure taxonomy.
//! * §18.2 — per-venue, version-controlled recorded facts; fail closed on
//!   unknown (`Unknown(code)` / `FailureClass6::Fatal`), never guess benign.
//! * §22 — integer-only, deterministic, no float / clock / RNG / I/O.

use crate::registry::Venue;

/// A decoded on-chain program (custom) error.
///
/// Unit variants name the known Anchor errors of the supported venues; the
/// `Unknown(code)` variant carries the raw code for any unrecognised value so
/// decoding fails closed rather than mislabelling (§18.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodedProgramError {
    /// pump.fun `6000 NotAuthorized` — signer lacks authority for the action.
    NotAuthorized,
    /// pump.fun `6001 AlreadyInitialized` — global/account already initialized.
    AlreadyInitialized,
    /// pump.fun `6002 TooMuchSolRequired` — **buy slippage**: the buy would cost
    /// more SOL than the caller's `max_sol_cost` guard permits.
    TooMuchSolRequired,
    /// pump.fun `6003 TooLittleSolReceived` — **sell slippage**: the sell would
    /// return less SOL than the caller's `min_sol_output` guard permits.
    TooLittleSolReceived,
    /// pump.fun `6004 MintDoesNotMatchBondingCurve` — mint/curve pairing wrong.
    MintDoesNotMatchBondingCurve,
    /// pump.fun `6005 BondingCurveComplete` — curve already migrated/complete;
    /// trades on the curve are rejected.
    BondingCurveComplete,
    /// pump.fun `6006 BondingCurveNotComplete` — action requires a completed
    /// curve but it is still live.
    BondingCurveNotComplete,
    /// pump.fun `6007 NotInitialized` — account/global not yet initialized.
    NotInitialized,
    /// PumpSwap `6000 ExceededSlippage` — swap output outside slippage guard
    /// (the AMM analogue of buy/sell slippage).
    ExceededSlippage,
    /// PumpSwap `6001 InvalidPoolState` — pool reserves / flags inconsistent
    /// with the attempted operation.
    InvalidPoolState,
    /// PumpSwap `6002 InvalidPoolTokenAccounts` — supplied token accounts do
    /// not belong to / match the pool.
    InvalidPoolTokenAccounts,
    /// PumpSwap `6003 InsufficientLiquidity` — pool lacks liquidity to fill.
    InsufficientLiquidity,
    /// PumpSwap `6004 PoolDisabled` — pool is disabled / paused.
    PoolDisabled,
    /// An unrecognised custom error, carrying its raw code (fail closed).
    Unknown(u32),
}

/// The §36 6-class failure taxonomy — a finer decode than the coarse 3-class
/// `safety_integrity::FailureClass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FailureClass6 {
    /// A slippage / guard tripped: the market moved between build and land.
    /// Benign — a candidate for a re-priced retry under exec policy.
    GuardOrSlippage = 0,
    /// On-chain state changed under us (curve completed, already/​not
    /// initialized, pool state inconsistent): re-plan against fresh state.
    StateDrift = 1,
    /// A routing/targeting defect (wrong mint/curve pairing, wrong pool token
    /// accounts): the transaction was aimed at the wrong accounts.
    RouteError = 2,
    /// The compiled program/layout/discriminator did not match the pinned
    /// registry entry: a version mismatch, never a capital retry.
    VersionDrift = 3,
    /// A transient chain/transport condition (blockhash expiry, congestion,
    /// dropped tx) with no authoritative program error.
    Transient = 4,
    /// Unrecoverable: authorization failure, an unknown program error, or a
    /// hard invariant violation. Fail closed (§18.2).
    Fatal = 5,
}

impl FailureClass6 {
    /// Stable `u8` tag for compact serialization / journaling.
    #[inline]
    #[must_use]
    pub fn tag(self) -> u8 {
        self as u8
    }

    /// Whether exec policy may re-price and retry *with capital*. Only
    /// [`FailureClass6::GuardOrSlippage`] and [`FailureClass6::Transient`] are
    /// retryable; every other class must not silently re-spend.
    #[inline]
    #[must_use]
    pub fn retryable_with_capital(self) -> bool {
        matches!(
            self,
            FailureClass6::GuardOrSlippage | FailureClass6::Transient
        )
    }

    /// Whether this class demands a fresh state read / re-plan before any
    /// further action (state or version drift).
    #[inline]
    #[must_use]
    pub fn requires_replan(self) -> bool {
        matches!(
            self,
            FailureClass6::StateDrift | FailureClass6::VersionDrift
        )
    }
}

/// Transport / meta context that is *not* itself a program custom error but
/// governs how a (possibly-`Unknown`) program error is classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FailureContext {
    /// The failure had no authoritative program error — it was a transport
    /// transient (blockhash expired, node congestion, tx dropped/​not landed).
    pub transport_transient: bool,
    /// The program / account layout version did not match the pinned registry
    /// entry, so any decoded code is untrustworthy (§18.2 version drift).
    pub version_mismatch: bool,
}

/// The recorded pump.fun custom-error table (code → decoded error).
const PUMPFUN_ERRORS: &[(u32, DecodedProgramError)] = &[
    (6000, DecodedProgramError::NotAuthorized),
    (6001, DecodedProgramError::AlreadyInitialized),
    (6002, DecodedProgramError::TooMuchSolRequired),
    (6003, DecodedProgramError::TooLittleSolReceived),
    (6004, DecodedProgramError::MintDoesNotMatchBondingCurve),
    (6005, DecodedProgramError::BondingCurveComplete),
    (6006, DecodedProgramError::BondingCurveNotComplete),
    (6007, DecodedProgramError::NotInitialized),
];

/// The recorded PumpSwap custom-error table (code → decoded error).
const PUMPSWAP_ERRORS: &[(u32, DecodedProgramError)] = &[
    (6000, DecodedProgramError::ExceededSlippage),
    (6001, DecodedProgramError::InvalidPoolState),
    (6002, DecodedProgramError::InvalidPoolTokenAccounts),
    (6003, DecodedProgramError::InsufficientLiquidity),
    (6004, DecodedProgramError::PoolDisabled),
];

/// The recorded custom-error table for `venue`.
///
/// Exposed so callers can enumerate / round-trip the known codes. The slices
/// are ordered by ascending code.
#[must_use]
pub const fn error_table(venue: Venue) -> &'static [(u32, DecodedProgramError)] {
    match venue {
        Venue::PumpFun => PUMPFUN_ERRORS,
        Venue::PumpSwap => PUMPSWAP_ERRORS,
    }
}

/// Decode an on-chain custom error `code` for `venue` into a
/// [`DecodedProgramError`], returning [`DecodedProgramError::Unknown`] (carrying
/// the raw code) for any value not in that venue's recorded table.
///
/// Per-venue: identical `code` values decode differently across venues (e.g.
/// `6002` is `TooMuchSolRequired` on pump.fun but `InvalidPoolTokenAccounts` on
/// PumpSwap).
#[must_use]
pub fn decode_custom_error(venue: Venue, code: u32) -> DecodedProgramError {
    let table = error_table(venue);
    let mut i = 0;
    while i < table.len() {
        if table[i].0 == code {
            return table[i].1;
        }
        i += 1;
    }
    DecodedProgramError::Unknown(code)
}

/// Reverse lookup: the recorded custom-error `code` for a decoded error on
/// `venue`, or `None` for [`DecodedProgramError::Unknown`] / a variant that is
/// not part of this venue's table.
#[must_use]
pub fn error_code(venue: Venue, err: DecodedProgramError) -> Option<u32> {
    if let DecodedProgramError::Unknown(_) = err {
        return None;
    }
    let table = error_table(venue);
    let mut i = 0;
    while i < table.len() {
        if table[i].1 == err {
            return Some(table[i].0);
        }
        i += 1;
    }
    None
}

/// Classify a decoded program error plus its [`FailureContext`] into the §36
/// 6-class taxonomy.
///
/// Priority:
/// 1. A **version mismatch** dominates — any decoded code is untrustworthy
///    under layout drift ⇒ [`FailureClass6::VersionDrift`].
/// 2. Otherwise the decoded error's own class:
///    * slippage / guard ⇒ [`FailureClass6::GuardOrSlippage`];
///    * curve/init/pool-state inconsistencies ⇒ [`FailureClass6::StateDrift`];
///    * mint-curve / pool-token targeting defects ⇒ [`FailureClass6::RouteError`];
///    * authorization failure ⇒ [`FailureClass6::Fatal`].
/// 3. An **unknown** program error is `Transient` when the context flags a
///    transport transient (no real program error landed), else `Fatal`
///    (fail closed — never auto-retry an unrecognised program error).
///
/// Pure, total, deterministic.
#[must_use]
pub fn classify_failure6(err: DecodedProgramError, ctx: &FailureContext) -> FailureClass6 {
    use DecodedProgramError::*;

    if ctx.version_mismatch {
        return FailureClass6::VersionDrift;
    }

    match err {
        // Slippage / output-guard tripped: market moved.
        TooMuchSolRequired | TooLittleSolReceived | ExceededSlippage => {
            FailureClass6::GuardOrSlippage
        }
        // On-chain state changed / inconsistent under us.
        AlreadyInitialized
        | BondingCurveComplete
        | BondingCurveNotComplete
        | NotInitialized
        | InvalidPoolState
        | InsufficientLiquidity
        | PoolDisabled => FailureClass6::StateDrift,
        // Wrong accounts / mint-curve or pool-token targeting.
        MintDoesNotMatchBondingCurve | InvalidPoolTokenAccounts => FailureClass6::RouteError,
        // Authorization: unrecoverable.
        NotAuthorized => FailureClass6::Fatal,
        // Unrecognised: transient only if the context says transport-level.
        Unknown(_) => {
            if ctx.transport_transient {
                FailureClass6::Transient
            } else {
                FailureClass6::Fatal
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_6002_decodes_per_venue() {
        assert_eq!(
            decode_custom_error(Venue::PumpFun, 6002),
            DecodedProgramError::TooMuchSolRequired
        );
        // Same code, different venue, different meaning.
        assert_eq!(
            decode_custom_error(Venue::PumpSwap, 6002),
            DecodedProgramError::InvalidPoolTokenAccounts
        );
    }

    #[test]
    fn unknown_code_decodes_to_unknown_carrying_code() {
        assert_eq!(
            decode_custom_error(Venue::PumpFun, 9_999),
            DecodedProgramError::Unknown(9_999)
        );
        assert_eq!(
            decode_custom_error(Venue::PumpSwap, 1),
            DecodedProgramError::Unknown(1)
        );
    }

    #[test]
    fn pumpfun_table_round_trips() {
        for &(code, err) in error_table(Venue::PumpFun) {
            assert_eq!(decode_custom_error(Venue::PumpFun, code), err);
            assert_eq!(error_code(Venue::PumpFun, err), Some(code));
        }
    }

    #[test]
    fn pumpswap_table_round_trips() {
        for &(code, err) in error_table(Venue::PumpSwap) {
            assert_eq!(decode_custom_error(Venue::PumpSwap, code), err);
            assert_eq!(error_code(Venue::PumpSwap, err), Some(code));
        }
    }

    #[test]
    fn error_code_is_none_for_unknown() {
        assert_eq!(
            error_code(Venue::PumpFun, DecodedProgramError::Unknown(6002)),
            None
        );
    }

    #[test]
    fn error_code_is_none_for_wrong_venue_variant() {
        // ExceededSlippage is a PumpSwap variant; not in the pump.fun table.
        assert_eq!(
            error_code(Venue::PumpFun, DecodedProgramError::ExceededSlippage),
            None
        );
    }

    #[test]
    fn version_mismatch_dominates_all() {
        let ctx = FailureContext {
            transport_transient: true,
            version_mismatch: true,
        };
        // Even a clean slippage code is classified as version drift.
        assert_eq!(
            classify_failure6(DecodedProgramError::TooMuchSolRequired, &ctx),
            FailureClass6::VersionDrift
        );
    }

    #[test]
    fn slippage_codes_are_guard_or_slippage() {
        let ctx = FailureContext::default();
        for err in [
            DecodedProgramError::TooMuchSolRequired,
            DecodedProgramError::TooLittleSolReceived,
            DecodedProgramError::ExceededSlippage,
        ] {
            assert_eq!(classify_failure6(err, &ctx), FailureClass6::GuardOrSlippage);
        }
    }

    #[test]
    fn state_codes_are_state_drift() {
        let ctx = FailureContext::default();
        for err in [
            DecodedProgramError::AlreadyInitialized,
            DecodedProgramError::BondingCurveComplete,
            DecodedProgramError::BondingCurveNotComplete,
            DecodedProgramError::NotInitialized,
            DecodedProgramError::InvalidPoolState,
            DecodedProgramError::InsufficientLiquidity,
            DecodedProgramError::PoolDisabled,
        ] {
            assert_eq!(classify_failure6(err, &ctx), FailureClass6::StateDrift);
        }
    }

    #[test]
    fn routing_codes_are_route_error() {
        let ctx = FailureContext::default();
        for err in [
            DecodedProgramError::MintDoesNotMatchBondingCurve,
            DecodedProgramError::InvalidPoolTokenAccounts,
        ] {
            assert_eq!(classify_failure6(err, &ctx), FailureClass6::RouteError);
        }
    }

    #[test]
    fn authorization_is_fatal() {
        let ctx = FailureContext::default();
        assert_eq!(
            classify_failure6(DecodedProgramError::NotAuthorized, &ctx),
            FailureClass6::Fatal
        );
    }

    #[test]
    fn unknown_transient_vs_fatal_depends_on_context() {
        let transient = FailureContext {
            transport_transient: true,
            version_mismatch: false,
        };
        assert_eq!(
            classify_failure6(DecodedProgramError::Unknown(42), &transient),
            FailureClass6::Transient
        );

        let no_ctx = FailureContext::default();
        assert_eq!(
            classify_failure6(DecodedProgramError::Unknown(42), &no_ctx),
            FailureClass6::Fatal
        );
    }

    #[test]
    fn full_pipeline_decode_then_classify() {
        // pump.fun 6002 → slippage guard.
        let e = decode_custom_error(Venue::PumpFun, 6002);
        assert_eq!(
            classify_failure6(e, &FailureContext::default()),
            FailureClass6::GuardOrSlippage
        );
        // pump.fun 6005 → curve complete → state drift.
        let e = decode_custom_error(Venue::PumpFun, 6005);
        assert_eq!(
            classify_failure6(e, &FailureContext::default()),
            FailureClass6::StateDrift
        );
    }

    #[test]
    fn retryable_and_replan_predicates() {
        assert!(FailureClass6::GuardOrSlippage.retryable_with_capital());
        assert!(FailureClass6::Transient.retryable_with_capital());
        assert!(!FailureClass6::StateDrift.retryable_with_capital());
        assert!(!FailureClass6::Fatal.retryable_with_capital());
        assert!(!FailureClass6::VersionDrift.retryable_with_capital());

        assert!(FailureClass6::StateDrift.requires_replan());
        assert!(FailureClass6::VersionDrift.requires_replan());
        assert!(!FailureClass6::GuardOrSlippage.requires_replan());
    }

    #[test]
    fn class_tags_are_stable_and_distinct() {
        let tags = [
            FailureClass6::GuardOrSlippage.tag(),
            FailureClass6::StateDrift.tag(),
            FailureClass6::RouteError.tag(),
            FailureClass6::VersionDrift.tag(),
            FailureClass6::Transient.tag(),
            FailureClass6::Fatal.tag(),
        ];
        assert_eq!(tags, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn adversarial_codes_never_panic() {
        for code in [0u32, u32::MAX, 5999, 6008, 7000] {
            let a = decode_custom_error(Venue::PumpFun, code);
            let b = decode_custom_error(Venue::PumpSwap, code);
            let _ = classify_failure6(a, &FailureContext::default());
            let _ = classify_failure6(b, &FailureContext::default());
        }
    }

    #[test]
    fn deterministic_repeat() {
        let ctx = FailureContext::default();
        let e = decode_custom_error(Venue::PumpFun, 6003);
        assert_eq!(classify_failure6(e, &ctx), classify_failure6(e, &ctx));
    }
}
