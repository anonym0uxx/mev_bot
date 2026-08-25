//! Treasury: policy-gated SOL transfers from the bot's hot wallet.
//!
//! ## Purpose
//! The bot already signs trades with `WalletSigner`. This crate adds a
//! policy-gated *fund movement* capability: moving SOL from the bot's hot
//! wallet to whitelisted destination addresses, subject to per-address limits,
//! daily caps, and time-locks.
//!
//! ## Design
//! - **Reuses `WalletSigner`** from `pq_stream_capture::signer` for key
//!   loading and ed25519 signing. No new crypto dependencies.
//! - **Policy is a TOML file** that Alon writes and deploys. The code reads
//!   it at startup and enforces every constraint in Rust.
//! - **Fail-closed by default**: unknown address, exceeded limit, missing
//!   policy, or any ambiguity → refuse and log.
//! - **Append-only audit log**: every attempt (approved or rejected) is
//!   recorded to a local JSONL file with tx signature + on-chain confirmation.
//! - **§22 compliant**: integer-only lamports, no floats on the money path.
//! - **Overflow-checks ON in release**: money is involved.
//!
//! ## What this crate does NOT do
//! - It does not load keys at import time. The daemon calls `Treasury::load`
//!   with a keypair path + expected address, same as the existing signer.
//! - It does not auto-send. Every transfer requires an explicit `request_transfer`
//!   call with destination + amount. The policy layer gates it.
//! - It does not hold keys longer than needed. The `WalletSigner` is held
//!   behind `Arc` and never exposed outside this crate.
//! - Agent (Hermes) does NOT call this directly. The daemon owns the
//!   `Treasury` instance. Alon triggers transfers via the daemon's CLI
//!   subcommand or a signed operator-order file — both human-initiated.

// ── Module layout ──
pub mod policy;
pub mod transfer;
pub mod rpc;
pub mod audit;

// Re-exports for ergonomic access.
pub use policy::{TreasuryPolicy, WhitelistEntry, TransferLimits};
pub use transfer::{TransferRequest, TransferOutcome, TransferError};
pub use rpc::HeliusRpc;
pub use audit::AuditEntry;

// ── Treasury facade ──

use std::sync::Arc;
use std::path::Path;

use pq_stream_capture::signer::WalletSigner;

/// The treasury. Holds the signer (behind Arc) and the loaded policy.
///
/// Construct via [`Treasury::load`], which reads the keypair file and policy
/// file in one fail-closed step.
pub struct Treasury {
    signer: Arc<WalletSigner>,
    policy: TreasuryPolicy,
    audit_path: std::path::PathBuf,
}

impl Treasury {
    /// Load the treasury from a keypair file and a policy file.
    ///
    /// # Arguments
    /// * `keypair_path` - Path to the Solana CLI keypair JSON file.
    /// * `expected_address` - The base58 address the keypair MUST match.
    ///   Fail-closed: mismatch → error.
    /// * `policy_path` - Path to the TOML policy file.
    /// * `audit_path` - Path to the append-only JSONL audit log.
    ///
    /// # Errors
    /// Returns an error if the keypair is unreadable, inconsistent, or does
    /// not match `expected_address`, or if the policy file is unreadable /
    /// malformed.
    pub fn load(
        keypair_path: &Path,
        expected_address: &str,
        policy_path: &Path,
        audit_path: &Path,
    ) -> Result<Self, TreasuryError> {
        let signer = WalletSigner::load_solana_keypair(keypair_path, expected_address)
            .map_err(TreasuryError::Signer)?;
        let policy_text = std::fs::read_to_string(policy_path)
            .map_err(|e| TreasuryError::PolicyRead(policy_path.display().to_string(), e.to_string()))?;
        let policy = TreasuryPolicy::from_toml(&policy_text)
            .map_err(TreasuryError::PolicyParse)?;

        Ok(Self {
            signer: Arc::new(signer),
            policy,
            audit_path: audit_path.to_path_buf(),
        })
    }

    /// The public address of the bot's hot wallet. Not a secret.
    #[must_use]
    pub fn wallet_address(&self) -> &str {
        self.signer.address()
    }

    /// Request a SOL transfer. The policy layer gates every aspect.
    ///
    /// # Arguments
    /// * `destination` - Base58 Solana address to send SOL to.
    /// * `lamports` - Amount in lamports (1 SOL = 1_000_000_000 lamports).
    ///   Integer-only, §22 compliant.
    /// * `purpose` - Human-readable reason for the transfer (logged in audit).
    /// * `codeword` - The codeword (if configured in policy). None if no
    ///   codeword is configured. The codeword is verified against the
    ///   SHA-256 hash stored in the policy file before any signing occurs.
    ///
    /// # Policy enforcement
    /// - Destination MUST be in the whitelist.
    /// - Codeword MUST match (if configured).
    /// - Amount MUST be <= per-tx limit for that address.
    /// - Amount MUST be <= remaining daily limit for that address.
    /// - If the amount exceeds the `approval_threshold`, the transfer is
    ///   queued (time-locked) and requires a second call to `confirm_queued`
    ///   after the time-lock expires. This prevents impulse/rushed transfers.
    ///
    /// # Errors
    /// Returns an error for any policy violation, RPC failure, or signing
    /// error. All attempts (success and failure) are logged to the audit file.
    pub fn request_transfer(
        &self,
        destination: &str,
        lamports: u64,
        purpose: &str,
        rpc: &HeliusRpc,
        codeword: Option<&str>,
    ) -> TransferOutcome {
        transfer::execute_transfer(
            &self.signer,
            &self.policy,
            destination,
            lamports,
            purpose,
            rpc,
            &self.audit_path,
            codeword,
        )
    }

    /// Access the loaded policy (for CLI status display).
    #[must_use]
    pub fn policy(&self) -> &TreasuryPolicy {
        &self.policy
    }
}

/// Errors that can occur during treasury initialization.
#[derive(Debug)]
pub enum TreasuryError {
    /// Key loading failed (unreadable, wrong wallet, corrupt, etc.)
    Signer(pq_stream_capture::signer::SignerError),
    /// Policy file could not be read.
    PolicyRead(String, String),
    /// Policy file could not be parsed.
    PolicyParse(String),
}

impl std::fmt::Display for TreasuryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signer(e) => write!(f, "signer error: {e}"),
            Self::PolicyRead(path, cause) => write!(f, "policy file unreadable at {path}: {cause}"),
            Self::PolicyParse(detail) => write!(f, "policy parse error: {detail}"),
        }
    }
}

impl std::error::Error for TreasuryError {}
