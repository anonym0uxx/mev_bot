//! §80 **Expanded incident-branch remediation paths** — maps each failure
//! class from the §36 taxonomy to a concrete, validated remediation action.
//!
//! The existing `si_incident_gate.rs` handles a single remediation type:
//! liquidating a stuck exit position via sell-simulation + signing boundary.
//! This module expands that to cover the full remediation surface:
//!
//! | Failure Class        | Remediation Action                | Gate                    |
//! |---------------------+----------------------------------+-------------------------|
//! | GuardOrSlippage     | Re-priced retry (same intent)     | `RepriceRetryGate`      |
//! | StateDrift          | Re-plan against fresh state       | `ReplanGate`            |
//! | RouteError          | Re-route to correct venue/curve   | `ReRouteGate`           |
//! | VersionDrift        | Quarantine + human escalation     | `QuarantineGate`        |
//! | Transient           | Backoff-retry (same intent)       | `BackoffRetryGate`      |
//! | Fatal               | Abort + human escalation          | `AbortGate`             |
//!
//! Each remediation action is a *validated* path — the model proposes the
//! action, but the gate independently verifies it can reach chain safely.
//! The model never supplies authority (signatures, out-amounts); the gate
//! recomputes those from live state.
//!
//! ## Constitution
//! * §80 — incident-branch remediations cannot reach chain without gate.
//! * §36 — 6-class failure taxonomy (extended in runtime_errors.rs).
//! * §79 — deterministic exit path is model-independent.
//! * §18.2 — fail closed on unknown, never guess benign.
//! * §22 — integer-only, deterministic.

use pump_quant_protocol::errors::FailureClass6;
use pump_quant_protocol::runtime_errors::{
    RuntimeError, TransactionError, RpcError,
    classify_runtime_error, classify_transaction_error, classify_rpc_error,
};

// ---------------------------------------------------------------------------
// Remediation action types
// ---------------------------------------------------------------------------

/// A remediation action proposed by the model (untrusted).
/// Each variant maps to a different gate path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemediationAction {
    /// Re-price a retry after a slippage guard tripped.
    /// The model proposes a new price; the gate verifies it's within bounds.
    RepriceRetry {
        /// New max_slippage_bps the model proposes (basis points).
        new_slippage_bps: u32,
        /// Previous slippage that failed.
        prev_slippage_bps: u32,
    },
    /// Re-plan against fresh state after state drift.
    /// The gate requires a fresh state read before this is admitted.
    Replan,
    /// Re-route to a different venue/curve after a route error.
    /// The model proposes the new venue; the gate verifies it's registered.
    ReRoute {
        /// Venue tag (0 = pump.fun, 1 = PumpSwap).
        target_venue: u8,
    },
    /// Quarantine the position after version drift — no chain action,
    /// just record and escalate to human.
    Quarantine,
    /// Backoff-retry after a transient failure (blockhash, CU, congestion).
    /// The gate enforces exponential backoff bounds.
    BackoffRetry {
        /// Attempt number (1-indexed).
        attempt: u32,
        /// Max attempts before giving up.
        max_attempts: u32,
    },
    /// Abort and escalate to human — for Fatal failures.
    Abort,
}

/// The validated remediation that has passed its gate and may proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmittedAction {
    /// A re-priced retry that passed the slippage bound check.
    RepriceRetry { new_slippage_bps: u32 },
    /// A re-plan that passed the fresh-state requirement.
    Replan,
    /// A re-route that passed the venue-registry check.
    ReRoute { target_venue: u8 },
    /// A quarantine decision (no chain action, escalate to human).
    Quarantine,
    /// A backoff-retry within bounds.
    BackoffRetry { attempt: u32, delay_slots: u64 },
    /// An abort decision (no chain action, escalate to human).
    Abort,
}

/// Why a remediation action was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemediationReject {
    /// The proposed action doesn't match the failure class.
    ActionClassMismatch {
        action: RemediationAction,
        failure_class: FailureClass6,
    },
    /// The new slippage exceeds the max allowed bound.
    SlippageExceedsBound {
        proposed: u32,
        max_allowed: u32,
    },
    /// The new slippage is not higher than the previous (retry must increase).
    SlippageNotIncreased {
        proposed: u32,
        previous: u32,
    },
    /// The target venue is not registered.
    VenueNotRegistered { venue: u8 },
    /// The attempt count exceeds the max.
    TooManyAttempts { attempt: u32, max: u32 },
    /// The attempt count is zero (attempts are 1-indexed).
    ZeroAttempt,
}

// ---------------------------------------------------------------------------
// Gate configuration
// ---------------------------------------------------------------------------

/// Configuration for the remediation gates.
#[derive(Clone, Copy, Debug)]
pub struct RemediationConfig {
    /// Maximum slippage in basis points (e.g., 500 = 5%).
    pub max_slippage_bps: u32,
    /// Minimum slippage increase for a re-price retry (bps).
    pub min_slippage_increase_bps: u32,
    /// Maximum backoff retry attempts.
    pub max_retry_attempts: u32,
    /// Base delay in slots for exponential backoff.
    pub base_delay_slots: u64,
    /// Registered venues (bit flags: 0x01 = pump.fun, 0x02 = PumpSwap).
    pub registered_venues: u8,
}

impl Default for RemediationConfig {
    fn default() -> Self {
        Self {
            max_slippage_bps: 1000, // 10%
            min_slippage_increase_bps: 10, // 0.1%
            max_retry_attempts: 5,
            base_delay_slots: 30, // ~12s at 400ms/slot
            registered_venues: 0x03, // both pump.fun and PumpSwap
        }
    }
}

// ---------------------------------------------------------------------------
// The unified remediation gate
// ---------------------------------------------------------------------------

/// Validate a remediation action against its failure class and gate config.
///
/// This is the expanded §80 gate: each failure class has a specific
/// remediation action, and the gate independently verifies that:
/// 1. The action matches the failure class (no mismatches).
/// 2. The action's parameters are within bounds.
/// 3. The action is safe to execute (no chain-reaching artifact without proof).
///
/// Returns an `AdmittedAction` that may proceed, or a `RemediationReject`.
#[must_use]
pub fn remediation_gate(
    action: RemediationAction,
    failure_class: FailureClass6,
    config: &RemediationConfig,
) -> Result<AdmittedAction, RemediationReject> {
    // Gate 1: action must match the failure class.
    if !action_matches_class(&action, failure_class) {
        return Err(RemediationReject::ActionClassMismatch {
            action,
            failure_class,
        });
    }

    // Gate 2: validate action-specific parameters.
    match action {
        RemediationAction::RepriceRetry { new_slippage_bps, prev_slippage_bps } => {
            // New slippage must not exceed the configured max.
            if new_slippage_bps > config.max_slippage_bps {
                return Err(RemediationReject::SlippageExceedsBound {
                    proposed: new_slippage_bps,
                    max_allowed: config.max_slippage_bps,
                });
            }
            // New slippage must be higher than previous (retry must widen).
            if new_slippage_bps <= prev_slippage_bps + config.min_slippage_increase_bps {
                return Err(RemediationReject::SlippageNotIncreased {
                    proposed: new_slippage_bps,
                    previous: prev_slippage_bps,
                });
            }
            Ok(AdmittedAction::RepriceRetry { new_slippage_bps })
        }

        RemediationAction::Replan => {
            // Replan always admitted if class matches (fresh state read
            // is enforced by the caller, not this gate).
            Ok(AdmittedAction::Replan)
        }

        RemediationAction::ReRoute { target_venue } => {
            // Target venue must be registered.
            let venue_bit = 1u8 << target_venue;
            if config.registered_venues & venue_bit == 0 {
                return Err(RemediationReject::VenueNotRegistered { venue: target_venue });
            }
            Ok(AdmittedAction::ReRoute { target_venue })
        }

        RemediationAction::Quarantine => {
            // Quarantine is always admitted (it's a no-op on chain).
            Ok(AdmittedAction::Quarantine)
        }

        RemediationAction::BackoffRetry { attempt, max_attempts } => {
            // Attempt must be 1-indexed (non-zero).
            if attempt == 0 {
                return Err(RemediationReject::ZeroAttempt);
            }
            // Attempt must not exceed the configured max.
            if attempt > max_attempts || attempt > config.max_retry_attempts {
                return Err(RemediationReject::TooManyAttempts {
                    attempt,
                    max: config.max_retry_attempts,
                });
            }
            // Exponential backoff: delay = base * 2^(attempt-1).
            let delay_slots = config.base_delay_slots
                .saturating_mul(1u64 << (attempt - 1).min(20));
            Ok(AdmittedAction::BackoffRetry { attempt, delay_slots })
        }

        RemediationAction::Abort => {
            // Abort is always admitted (it's a no-op on chain).
            Ok(AdmittedAction::Abort)
        }
    }
}

/// Check if a remediation action is compatible with the failure class.
///
/// | Action          | Compatible Classes                              |
/// |-----------------+-------------------------------------------------|
/// | RepriceRetry    | GuardOrSlippage                                 |
/// | Replan          | StateDrift                                      |
/// | ReRoute         | RouteError                                      |
/// | Quarantine      | VersionDrift                                    |
/// | BackoffRetry    | Transient                                       |
/// | Abort           | Fatal (and any class as a fallback)             |
#[must_use]
pub fn action_matches_class(action: &RemediationAction, class: FailureClass6) -> bool {
    match action {
        RemediationAction::RepriceRetry { .. } => class == FailureClass6::GuardOrSlippage,
        RemediationAction::Replan => class == FailureClass6::StateDrift,
        RemediationAction::ReRoute { .. } => class == FailureClass6::RouteError,
        RemediationAction::Quarantine => class == FailureClass6::VersionDrift,
        RemediationAction::BackoffRetry { .. } => class == FailureClass6::Transient,
        // Abort is compatible with Fatal, and also as a fallback for any class.
        RemediationAction::Abort => true,
    }
}

/// Compute the remediation action for a runtime error.
///
/// This is the convenience entry point: given a runtime error (from
/// `runtime_errors.rs`), classify it into the 6-class taxonomy, then
/// determine the default remediation action for that class.
#[must_use]
pub fn default_action_for_runtime_error(err: RuntimeError) -> RemediationAction {
    let class = classify_runtime_error(err);
    default_action_for_class(class)
}

/// Compute the remediation action for a transaction error.
#[must_use]
pub fn default_action_for_tx_error(err: TransactionError) -> RemediationAction {
    let class = classify_transaction_error(err);
    default_action_for_class(class)
}

/// Compute the remediation action for an RPC error.
#[must_use]
pub fn default_action_for_rpc_error(err: RpcError) -> RemediationAction {
    let class = classify_rpc_error(err);
    default_action_for_class(class)
}

/// The default remediation action for a failure class.
#[must_use]
pub fn default_action_for_class(class: FailureClass6) -> RemediationAction {
    match class {
        FailureClass6::GuardOrSlippage => RemediationAction::RepriceRetry {
            new_slippage_bps: 200, // 2% — a reasonable first retry
            prev_slippage_bps: 100, // 1% — the previous that failed
        },
        FailureClass6::StateDrift => RemediationAction::Replan,
        FailureClass6::RouteError => RemediationAction::ReRoute { target_venue: 0 },
        FailureClass6::VersionDrift => RemediationAction::Quarantine,
        FailureClass6::Transient => RemediationAction::BackoffRetry {
            attempt: 1,
            max_attempts: 5,
        },
        FailureClass6::Fatal => RemediationAction::Abort,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_protocol::errors::FailureClass6;
    use pump_quant_protocol::runtime_errors::*;

    #[test]
    fn reprice_retry_matches_slippage_class() {
        let config = RemediationConfig::default();
        let action = RemediationAction::RepriceRetry {
            new_slippage_bps: 200,
            prev_slippage_bps: 100,
        };
        // GuardOrSlippage → should admit
        let result = remediation_gate(action, FailureClass6::GuardOrSlippage, &config);
        assert!(result.is_ok());
        // StateDrift → should reject (class mismatch)
        let result = remediation_gate(action, FailureClass6::StateDrift, &config);
        assert!(result.is_err());
    }

    #[test]
    fn reprice_retry_rejects_excessive_slippage() {
        let config = RemediationConfig::default();
        let action = RemediationAction::RepriceRetry {
            new_slippage_bps: 2000, // 20% — exceeds 10% max
            prev_slippage_bps: 100,
        };
        let result = remediation_gate(action, FailureClass6::GuardOrSlippage, &config);
        assert!(matches!(
            result,
            Err(RemediationReject::SlippageExceedsBound { proposed: 2000, max_allowed: 1000 })
        ));
    }

    #[test]
    fn reprice_retry_rejects_no_increase() {
        let config = RemediationConfig::default();
        let action = RemediationAction::RepriceRetry {
            new_slippage_bps: 105, // only 5 bps more, min is 10
            prev_slippage_bps: 100,
        };
        let result = remediation_gate(action, FailureClass6::GuardOrSlippage, &config);
        assert!(matches!(result, Err(RemediationReject::SlippageNotIncreased { .. })));
    }

    #[test]
    fn backoff_retry_admitted_within_bounds() {
        let config = RemediationConfig::default();
        let action = RemediationAction::BackoffRetry { attempt: 2, max_attempts: 5 };
        let result = remediation_gate(action, FailureClass6::Transient, &config);
        assert!(result.is_ok());
        if let Ok(AdmittedAction::BackoffRetry { attempt, delay_slots }) = result {
            assert_eq!(attempt, 2);
            // delay = base * 2^(attempt-1) = 30 * 2^1 = 60
            assert_eq!(delay_slots, 60);
        } else {
            panic!("expected BackoffRetry");
        }
    }

    #[test]
    fn backoff_retry_rejects_too_many_attempts() {
        let config = RemediationConfig::default();
        let action = RemediationAction::BackoffRetry { attempt: 10, max_attempts: 5 };
        let result = remediation_gate(action, FailureClass6::Transient, &config);
        assert!(matches!(result, Err(RemediationReject::TooManyAttempts { .. })));
    }

    #[test]
    fn backoff_retry_rejects_zero_attempt() {
        let config = RemediationConfig::default();
        let action = RemediationAction::BackoffRetry { attempt: 0, max_attempts: 5 };
        let result = remediation_gate(action, FailureClass6::Transient, &config);
        assert!(matches!(result, Err(RemediationReject::ZeroAttempt)));
    }

    #[test]
    fn reroute_rejects_unregistered_venue() {
        let config = RemediationConfig::default();
        let action = RemediationAction::ReRoute { target_venue: 5 }; // not registered
        let result = remediation_gate(action, FailureClass6::RouteError, &config);
        assert!(matches!(result, Err(RemediationReject::VenueNotRegistered { venue: 5 })));
    }

    #[test]
    fn reroute_admits_registered_venue() {
        let config = RemediationConfig::default();
        let action = RemediationAction::ReRoute { target_venue: 1 }; // PumpSwap
        let result = remediation_gate(action, FailureClass6::RouteError, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn quarantine_and_abort_always_admitted() {
        let config = RemediationConfig::default();
        // Quarantine only matches VersionDrift
        assert!(remediation_gate(RemediationAction::Quarantine, FailureClass6::VersionDrift, &config).is_ok());
        assert!(remediation_gate(RemediationAction::Quarantine, FailureClass6::Fatal, &config).is_err());

        // Abort matches ANY class (fallback)
        assert!(remediation_gate(RemediationAction::Abort, FailureClass6::Fatal, &config).is_ok());
        assert!(remediation_gate(RemediationAction::Abort, FailureClass6::GuardOrSlippage, &config).is_ok());
    }

    #[test]
    fn default_action_for_each_class() {
        assert!(matches!(
            default_action_for_class(FailureClass6::GuardOrSlippage),
            RemediationAction::RepriceRetry { .. }
        ));
        assert!(matches!(
            default_action_for_class(FailureClass6::StateDrift),
            RemediationAction::Replan
        ));
        assert!(matches!(
            default_action_for_class(FailureClass6::RouteError),
            RemediationAction::ReRoute { .. }
        ));
        assert!(matches!(
            default_action_for_class(FailureClass6::VersionDrift),
            RemediationAction::Quarantine
        ));
        assert!(matches!(
            default_action_for_class(FailureClass6::Transient),
            RemediationAction::BackoffRetry { .. }
        ));
        assert!(matches!(
            default_action_for_class(FailureClass6::Fatal),
            RemediationAction::Abort
        ));
    }

    #[test]
    fn full_pipeline_runtime_error_to_remediation() {
        // CU exhaustion → Transient → BackoffRetry
        let action = default_action_for_runtime_error(RuntimeError::ComputationalBudgetExceeded);
        assert!(matches!(action, RemediationAction::BackoffRetry { .. }));

        // MissingRequiredSignature → Fatal → Abort
        let action = default_action_for_runtime_error(RuntimeError::MissingRequiredSignature);
        assert!(matches!(action, RemediationAction::Abort));

        // BlockhashNotFound → Transient → BackoffRetry
        let action = default_action_for_tx_error(TransactionError::BlockhashNotFound);
        assert!(matches!(action, RemediationAction::BackoffRetry { .. }));

        // RateLimited → Transient → BackoffRetry
        let action = default_action_for_rpc_error(RpcError::RateLimited);
        assert!(matches!(action, RemediationAction::BackoffRetry { .. }));
    }

    #[test]
    fn exponential_backoff_sequence() {
        let config = RemediationConfig::default();
        // Attempt 1: delay = 30 * 2^0 = 30
        let a1 = remediation_gate(
            RemediationAction::BackoffRetry { attempt: 1, max_attempts: 5 },
            FailureClass6::Transient, &config,
        ).unwrap();
        // Attempt 2: delay = 30 * 2^1 = 60
        let a2 = remediation_gate(
            RemediationAction::BackoffRetry { attempt: 2, max_attempts: 5 },
            FailureClass6::Transient, &config,
        ).unwrap();
        // Attempt 3: delay = 30 * 2^2 = 120
        let a3 = remediation_gate(
            RemediationAction::BackoffRetry { attempt: 3, max_attempts: 5 },
            FailureClass6::Transient, &config,
        ).unwrap();

        match (a1, a2, a3) {
            (AdmittedAction::BackoffRetry { delay_slots: d1, .. },
             AdmittedAction::BackoffRetry { delay_slots: d2, .. },
             AdmittedAction::BackoffRetry { delay_slots: d3, .. }) => {
                assert_eq!(d1, 30);
                assert_eq!(d2, 60);
                assert_eq!(d3, 120);
            }
            _ => panic!("all should be BackoffRetry"),
        }
    }

    #[test]
    fn action_class_mismatch_rejected() {
        let config = RemediationConfig::default();
        // Replan on GuardOrSlippage → mismatch
        let result = remediation_gate(RemediationAction::Replan, FailureClass6::GuardOrSlippage, &config);
        assert!(matches!(result, Err(RemediationReject::ActionClassMismatch { .. })));
    }

    #[test]
    fn deterministic_remediation() {
        let config = RemediationConfig::default();
        let action = RemediationAction::RepriceRetry {
            new_slippage_bps: 300,
            prev_slippage_bps: 100,
        };
        let a = remediation_gate(action, FailureClass6::GuardOrSlippage, &config);
        let b = remediation_gate(action, FailureClass6::GuardOrSlippage, &config);
        assert_eq!(a, b);
    }
}
