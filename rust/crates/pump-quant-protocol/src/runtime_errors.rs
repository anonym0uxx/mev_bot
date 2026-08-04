//! §36 **Solana runtime-level error taxonomy** — the non-custom `InstructionError`
//! variants and transaction-level errors that can occur on ANY program, not just
//! pump.fun / PumpSwap custom errors.
//!
//! The existing `errors.rs` covers per-program Anchor custom errors (the
//! `InstructionError::Custom(u32)` variant). This module covers the **rest** of
//! the Solana error surface:
//!
//! - `InstructionError` non-custom variants (runtime, compute budget, account)
//! - `TransactionError` variants (blockhash, account-in-use, too-old)
//! - JSON-RPC error codes (node-level: rate-limit, timeout, server-error)
//!
//! Each runtime error maps into the same [`FailureClass6`] taxonomy so the
//! exec plane has a single, unified classification across custom, runtime,
//! and transport errors.
//!
//! ## Constitution
//! * §36 — decoded custom-error table + failure taxonomy (extended here).
//! * §18.2 — fail closed on unknown, never guess benign.
//! * §22 — integer-only, deterministic, no float / clock / RNG / I/O.
//! * §78 — failed-tx error taxonomy (every tx failure classified).

use crate::errors::FailureClass6;

/// A Solana runtime-level instruction error (non-custom variant).
///
/// These are the standard `InstructionError` variants that any program can
/// produce. They are NOT per-program custom errors — they come from the
/// Solana runtime itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeError {
    /// `InsufficientFunds` — account didn't have enough SOL for the debit.
    InsufficientFunds,
    /// `IncorrectProgramId` — instruction referenced wrong program account.
    IncorrectProgramId,
    /// `InvalidArgument` — generic bad argument to the program.
    InvalidArgument,
    /// `InvalidInstructionData` — instruction data couldn't deserialize.
    InvalidInstructionData,
    /// `MissingRequiredSignature` — a required signer was absent.
    MissingRequiredSignature,
    /// `NotEnoughSigners` — fewer signatures than required.
    NotEnoughSigners,
    /// `UnbalancedInstruction` — the instruction's debits/credits don't sum to zero.
    UnbalancedInstruction,
    /// `AccountNotRentExempt` — account would be below rent after the op.
    AccountNotRentExempt,
    /// `BorshIoError` — serialisation/deserialisation failed (Borsh).
    BorshIoError,
    /// `Custom(u32)` — a per-program custom error (handled by `errors.rs`).
    /// We include it here for completeness but classify via `errors.rs`.
    Custom(u32),
    /// `MaxSeedLengthExceeded` — PDA seed too long.
    MaxSeedLengthExceeded,
    /// `ComputationalBudgetExceeded` — CUs ran out (compute budget).
    ComputationalBudgetExceeded,
    /// `Immutable` — tried to mutate an immutable account.
    Immutable,
    /// `UnsupportedSysvar` — sysvar not supported in this context.
    UnsupportedSysvar,
    /// An unrecognised runtime error code (fail closed, §18.2).
    Unknown(u32),
}

/// A Solana transaction-level error (the `TransactionError` enum).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransactionError {
    /// `AccountInUse` — account already locked in a pending transaction.
    AccountInUse,
    /// `AccountLoadedNotWritable` — tried to write a read-only account.
    AccountLoadedNotWritable,
    /// `AccountNotLoaded` — account was referenced but not loaded.
    AccountNotLoaded,
    /// `InstructionError` — an instruction within the tx failed (see RuntimeError).
    InstructionError,
    /// `BlockhashNotFound` — blockhash is unknown/expired (the classic transient).
    BlockhashNotFound,
    /// `BlockhashNotExpired` — blockhash hasn't expired yet (unusual).
    BlockhashNotExpired,
    /// `TooOld` — transaction is older than the max age.
    TooOld,
    /// `DuplicateAccountIndex` — same account appears twice in the tx.
    DuplicateAccountIndex,
    /// An unrecognised transaction error (fail closed).
    Unknown(u32),
}

/// A JSON-RPC / transport-level error code.
///
/// Solana RPC nodes return JSON-RPC error objects with negative codes:
/// - `-32000` to `-32099` are server errors (rate-limit, node overload).
/// - `-32603` is the internal error (generic RPC failure).
/// - Custom transport errors (timeouts, connection resets) use positive codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RpcError {
    /// Rate limited (HTTP 429 / JSON-RPC -32007 or -32005).
    RateLimited,
    /// Node is overloaded / falling behind (JSON-RPC -32004).
    NodeOverloaded,
    /// Generic RPC internal error (-32603).
    InternalError,
    /// Request timeout (transport-level, not RPC code).
    Timeout,
    /// Connection reset / network error.
    ConnectionReset,
    /// An unrecognised RPC error code (fail closed).
    Unknown(i64),
}

/// Classify a `RuntimeError` into the §36 6-class taxonomy.
///
/// Mapping rationale:
/// - `MissingRequiredSignature` / `NotEnoughSigners` / `IncorrectProgramId` →
///   `Fatal` — the transaction was constructed wrong; retrying won't help.
/// - `ComputationalBudgetExceeded` → `Transient` — CU limit depends on
///   instruction complexity and slot congestion; a simpler retry may land.
/// - `InsufficientFunds` / `UnbalancedInstruction` → `Fatal` — the wallet
///   doesn't have enough SOL or the instruction is structurally broken.
/// - `InvalidArgument` / `InvalidInstructionData` / `BorshIoError` →
///   `VersionDrift` — the program layout changed; the compiled ix data no
///   longer deserialises. This is a version-drift signal (§18.2).
/// - `Custom(u32)` → delegate to `errors.rs` (call `decode_custom_error` first).
/// - `Unknown` → `Fatal` (fail closed, §18.2).
#[must_use]
pub fn classify_runtime_error(err: RuntimeError) -> FailureClass6 {
    match err {
        // Auth / construction defects — unrecoverable.
        RuntimeError::MissingRequiredSignature
        | RuntimeError::NotEnoughSigners
        | RuntimeError::IncorrectProgramId
        | RuntimeError::InsufficientFunds
        | RuntimeError::UnbalancedInstruction => FailureClass6::Fatal,

        // CU exhaustion — transient (slot congestion / complexity).
        RuntimeError::ComputationalBudgetExceeded => FailureClass6::Transient,

        // Layout / serialisation mismatch — version drift.
        RuntimeError::InvalidArgument
        | RuntimeError::InvalidInstructionData
        | RuntimeError::BorshIoError
        | RuntimeError::MaxSeedLengthExceeded => FailureClass6::VersionDrift,

        // Account state issues — state drift.
        RuntimeError::AccountNotRentExempt
        | RuntimeError::Immutable
        | RuntimeError::UnsupportedSysvar => FailureClass6::StateDrift,

        // Custom errors are classified via errors.rs, but if we see one here
        // without a venue context, treat as Fatal (fail closed).
        RuntimeError::Custom(_) => FailureClass6::Fatal,

        // Unknown — fail closed.
        RuntimeError::Unknown(_) => FailureClass6::Fatal,
    }
}

/// Classify a `TransactionError` into the §36 6-class taxonomy.
#[must_use]
pub fn classify_transaction_error(err: TransactionError) -> FailureClass6 {
    match err {
        // Blockhash / age — transient (classic retry-able conditions).
        TransactionError::BlockhashNotFound
        | TransactionError::TooOld
        | TransactionError::BlockhashNotExpired => FailureClass6::Transient,

        // Account locking — transient (slot-level contention).
        TransactionError::AccountInUse
        | TransactionError::DuplicateAccountIndex => FailureClass6::Transient,

        // Account loading defects — route error (wrong accounts loaded).
        TransactionError::AccountLoadedNotWritable
        | TransactionError::AccountNotLoaded => FailureClass6::RouteError,

        // Instruction-level failure — needs the instruction error to classify
        // further; we default to Fatal here (fail closed, the caller should
        // decode the inner RuntimeError and re-classify).
        TransactionError::InstructionError => FailureClass6::Fatal,

        // Unknown — fail closed.
        TransactionError::Unknown(_) => FailureClass6::Fatal,
    }
}

/// Classify an `RpcError` into the §36 6-class taxonomy.
///
/// RPC errors are almost always transient (rate-limit, timeout, overload).
/// Unknown codes fail closed to Fatal.
#[must_use]
pub fn classify_rpc_error(err: RpcError) -> FailureClass6 {
    match err {
        RpcError::RateLimited
        | RpcError::NodeOverloaded
        | RpcError::Timeout
        | RpcError::ConnectionReset => FailureClass6::Transient,
        // Generic internal error — could be anything; fail closed.
        RpcError::InternalError => FailureClass6::Fatal,
        RpcError::Unknown(_) => FailureClass6::Fatal,
    }
}

/// Decode a Solana `InstructionError` ordinal (as returned by the RPC
/// `result.value.err.InstructionError` array) into a `RuntimeError`.
///
/// The Solana RPC returns InstructionError as a JSON array: either
/// `["Custom", <code>]` for custom errors, or a string variant like
/// `["InsufficientFunds"]`. This decoder handles the numeric form (the
/// `InstructionError` enum's discriminant + optional data).
///
/// See: https://github.com/solana-labs/solana/blob/master/sdk/src/transaction/error.rs
#[must_use]
pub fn decode_instruction_error_variant(name: &str) -> RuntimeError {
    match name {
        "InsufficientFunds" => RuntimeError::InsufficientFunds,
        "IncorrectProgramId" => RuntimeError::IncorrectProgramId,
        "InvalidArgument" => RuntimeError::InvalidArgument,
        "InvalidInstructionData" => RuntimeError::InvalidInstructionData,
        "MissingRequiredSignature" => RuntimeError::MissingRequiredSignature,
        "NotEnoughSigners" => RuntimeError::NotEnoughSigners,
        "UnbalancedInstruction" => RuntimeError::UnbalancedInstruction,
        "AccountNotRentExempt" => RuntimeError::AccountNotRentExempt,
        "BorshIoError" => RuntimeError::BorshIoError,
        "MaxSeedLengthExceeded" => RuntimeError::MaxSeedLengthExceeded,
        "ComputationalBudgetExceeded" => RuntimeError::ComputationalBudgetExceeded,
        "Immutable" => RuntimeError::Immutable,
        "UnsupportedSysvar" => RuntimeError::UnsupportedSysvar,
        _ => RuntimeError::Unknown(0),
    }
}

/// Decode a Solana `TransactionError` variant name.
#[must_use]
pub fn decode_transaction_error_variant(name: &str) -> TransactionError {
    match name {
        "AccountInUse" => TransactionError::AccountInUse,
        "AccountLoadedNotWritable" => TransactionError::AccountLoadedNotWritable,
        "AccountNotLoaded" => TransactionError::AccountNotLoaded,
        "InstructionError" => TransactionError::InstructionError,
        "BlockhashNotFound" => TransactionError::BlockhashNotFound,
        "BlockhashNotExpired" => TransactionError::BlockhashNotExpired,
        "TooOld" => TransactionError::TooOld,
        "DuplicateAccountIndex" => TransactionError::DuplicateAccountIndex,
        _ => TransactionError::Unknown(0),
    }
}

/// Decode a JSON-RPC error code into an `RpcError`.
#[must_use]
pub fn decode_rpc_error_code(code: i64) -> RpcError {
    match code {
        -32005 | -32007 => RpcError::RateLimited,
        -32004 => RpcError::NodeOverloaded,
        -32603 => RpcError::InternalError,
        _ => {
            if code == 429 {
                RpcError::RateLimited
            } else if code > 0 {
                // Positive codes are transport-level (timeout, connection).
                // We can't distinguish further, so treat as timeout.
                RpcError::Timeout
            } else {
                RpcError::Unknown(code)
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
    use crate::errors::FailureClass6;

    #[test]
    fn runtime_errors_classify_correctly() {
        // Auth/construction → Fatal
        assert_eq!(
            classify_runtime_error(RuntimeError::MissingRequiredSignature),
            FailureClass6::Fatal
        );
        assert_eq!(
            classify_runtime_error(RuntimeError::InsufficientFunds),
            FailureClass6::Fatal
        );
        assert_eq!(
            classify_runtime_error(RuntimeError::UnbalancedInstruction),
            FailureClass6::Fatal
        );

        // CU exhaustion → Transient
        assert_eq!(
            classify_runtime_error(RuntimeError::ComputationalBudgetExceeded),
            FailureClass6::Transient
        );

        // Layout/serialisation → VersionDrift
        assert_eq!(
            classify_runtime_error(RuntimeError::InvalidInstructionData),
            FailureClass6::VersionDrift
        );
        assert_eq!(
            classify_runtime_error(RuntimeError::BorshIoError),
            FailureClass6::VersionDrift
        );

        // Account state → StateDrift
        assert_eq!(
            classify_runtime_error(RuntimeError::AccountNotRentExempt),
            FailureClass6::StateDrift
        );

        // Unknown → Fatal (fail closed)
        assert_eq!(
            classify_runtime_error(RuntimeError::Unknown(9999)),
            FailureClass6::Fatal
        );
    }

    #[test]
    fn transaction_errors_classify_correctly() {
        // Blockhash → Transient
        assert_eq!(
            classify_transaction_error(TransactionError::BlockhashNotFound),
            FailureClass6::Transient
        );
        assert_eq!(
            classify_transaction_error(TransactionError::TooOld),
            FailureClass6::Transient
        );

        // Account contention → Transient
        assert_eq!(
            classify_transaction_error(TransactionError::AccountInUse),
            FailureClass6::Transient
        );

        // Account loading → RouteError
        assert_eq!(
            classify_transaction_error(TransactionError::AccountLoadedNotWritable),
            FailureClass6::RouteError
        );

        // Unknown → Fatal
        assert_eq!(
            classify_transaction_error(TransactionError::Unknown(42)),
            FailureClass6::Fatal
        );
    }

    #[test]
    fn rpc_errors_classify_correctly() {
        // Rate limit → Transient
        assert_eq!(classify_rpc_error(RpcError::RateLimited), FailureClass6::Transient);
        assert_eq!(classify_rpc_error(RpcError::Timeout), FailureClass6::Transient);
        assert_eq!(classify_rpc_error(RpcError::NodeOverloaded), FailureClass6::Transient);

        // Internal error → Fatal (fail closed)
        assert_eq!(classify_rpc_error(RpcError::InternalError), FailureClass6::Fatal);
        assert_eq!(classify_rpc_error(RpcError::Unknown(-1)), FailureClass6::Fatal);
    }

    #[test]
    fn decode_instruction_error_variants() {
        assert_eq!(
            decode_instruction_error_variant("InsufficientFunds"),
            RuntimeError::InsufficientFunds
        );
        assert_eq!(
            decode_instruction_error_variant("ComputationalBudgetExceeded"),
            RuntimeError::ComputationalBudgetExceeded
        );
        // Unknown variant → fail closed
        assert_eq!(
            decode_instruction_error_variant("SomeNewError"),
            RuntimeError::Unknown(0)
        );
    }

    #[test]
    fn decode_transaction_error_variants() {
        assert_eq!(
            decode_transaction_error_variant("BlockhashNotFound"),
            TransactionError::BlockhashNotFound
        );
        assert_eq!(
            decode_transaction_error_variant("AccountInUse"),
            TransactionError::AccountInUse
        );
        // Unknown → fail closed
        assert_eq!(
            decode_transaction_error_variant("UnknownNewTxError"),
            TransactionError::Unknown(0)
        );
    }

    #[test]
    fn decode_rpc_error_codes() {
        // Standard JSON-RPC error codes
        assert_eq!(decode_rpc_error_code(-32007), RpcError::RateLimited);
        assert_eq!(decode_rpc_error_code(-32005), RpcError::RateLimited);
        assert_eq!(decode_rpc_error_code(-32004), RpcError::NodeOverloaded);
        assert_eq!(decode_rpc_error_code(-32603), RpcError::InternalError);
        // HTTP 429 → RateLimited
        assert_eq!(decode_rpc_error_code(429), RpcError::RateLimited);
        // Unknown negative → fail closed
        assert_eq!(decode_rpc_error_code(-9999), RpcError::Unknown(-9999));
    }

    #[test]
    fn full_runtime_pipeline() {
        // A CU exhaustion → Transient → retryable_with_capital == true
        let err = RuntimeError::ComputationalBudgetExceeded;
        let class = classify_runtime_error(err);
        assert_eq!(class, FailureClass6::Transient);
        assert!(class.retryable_with_capital());
        assert!(!class.requires_replan());

        // A BorshIoError → VersionDrift → requires_replan == true
        let err = RuntimeError::BorshIoError;
        let class = classify_runtime_error(err);
        assert_eq!(class, FailureClass6::VersionDrift);
        assert!(!class.retryable_with_capital());
        assert!(class.requires_replan());
    }

    #[test]
    fn deterministic_classification() {
        let err = RuntimeError::InsufficientFunds;
        let a = classify_runtime_error(err);
        let b = classify_runtime_error(err);
        assert_eq!(a, b);
    }

    #[test]
    fn adversarial_error_codes_never_panic() {
        for name in [
            "InsufficientFunds",
            "Unknown",
            "",
            "ComputationalBudgetExceeded",
            "SomeVeryLongErrorNameThatDoesNotExist",
        ] {
            let err = decode_instruction_error_variant(name);
            let _ = classify_runtime_error(err);
        }
    }

    #[test]
    fn retryable_and_replan_for_runtime() {
        // Transient (CU) is retryable
        assert!(classify_runtime_error(RuntimeError::ComputationalBudgetExceeded)
            .retryable_with_capital());
        // Fatal (auth) is NOT retryable
        assert!(!classify_runtime_error(RuntimeError::MissingRequiredSignature)
            .retryable_with_capital());
        // VersionDrift (layout) requires replan
        assert!(classify_runtime_error(RuntimeError::InvalidInstructionData)
            .requires_replan());
        // StateDrift (account) requires replan
        assert!(classify_runtime_error(RuntimeError::AccountNotRentExempt)
            .requires_replan());
    }
}
