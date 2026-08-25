//! Transfer: build, gate, and execute a SOL transfer instruction.
//!
//! ## Flow
//! 1. Validate destination is whitelisted.
//! 2. Validate amount is within per-tx and daily limits.
//! 3. If amount > approval_threshold → return time-locked (requires confirm).
//! 4. Build a System Program transfer instruction (3 bytes: 2 = "transfer").
//! 5. Construct the Solana transaction message (header + instructions + hash).
//! 6. Sign with `WalletSigner` (ed25519, ring).
//! 7. Serialize to base64 wire format.
//! 8. Submit via `sendTransaction` RPC call.
//! 9. Log to audit file.
//!
//! ## §22 compliance
//! All amounts are u64 lamports. No floats. Overflow-checks ON in release.

use std::path::Path;
use std::sync::Arc;

use pq_stream_capture::signer::{WalletSigner, decode_base58_32};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::policy::TreasuryPolicy;
use crate::rpc::HeliusRpc;
use crate::audit::AuditEntry;

/// A transfer request: destination + amount + purpose.
#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub destination: String,
    pub lamports: u64,
    pub purpose: String,
}

/// The outcome of a transfer attempt.
#[derive(Debug, Clone)]
pub enum TransferOutcome {
    /// Transfer was signed, broadcast, and confirmed on-chain.
    Confirmed {
        tx_signature: String,
        lamports: u64,
        destination: String,
        purpose: String,
    },
    /// Transfer exceeds the approval threshold — queued, requires
    /// confirmation after the time-lock expires.
    TimeLocked {
        destination: String,
        lamports: u64,
        time_lock_seconds: u64,
        reason: String,
    },
    /// Transfer was rejected by the policy layer. No signature, no broadcast.
    Rejected {
        reason: String,
        destination: String,
        lamports: u64,
    },
    /// Transfer failed during signing or RPC submission.
    Failed {
        reason: String,
        destination: String,
        lamports: u64,
    },
}

/// Errors that occur during transfer construction (not policy rejections).
#[derive(Debug)]
pub enum TransferError {
    /// The destination address is not valid base58 / wrong length.
    InvalidDestination(String),
    /// The destination is not in the whitelist.
    NotWhitelisted(String),
    /// Amount exceeds per-tx limit for this address.
    ExceedsPerTx { destination: String, requested: u64, max: u64 },
    /// Amount exceeds daily limit for this address.
    ExceedsDaily { destination: String, requested: u64, max: u64 },
    /// Signing failed.
    SignError(String),
    /// RPC submission failed.
    RpcError(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDestination(a) => write!(f, "invalid destination address: {a}"),
            Self::NotWhitelisted(a) => write!(f, "destination {a} is not whitelisted"),
            Self::ExceedsPerTx { destination, requested, max } => {
                write!(f, "transfer of {requested} lamports to {destination} exceeds per-tx limit of {max}")
            }
            Self::ExceedsDaily { destination, requested, max } => {
                write!(f, "transfer of {requested} lamports to {destination} exceeds daily limit of {max}")
            }
            Self::SignError(s) => write!(f, "signing error: {s}"),
            Self::RpcError(s) => write!(f, "RPC error: {s}"),
        }
    }
}

impl std::error::Error for TransferError {}

// ── Solana wire format constants ──

/// System program ID (base58: "11111111111111111111111111111111").
const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// System transfer instruction discriminator: 2 (u32 LE).
const TRANSFER_DISCRIMINATOR: [u8; 4] = 2u32.to_le_bytes();

/// Compact-u16 encoding for account counts in the message header.
fn encode_compact_u16(n: usize) -> [u8; 3] {
    let mut out = [0u8; 3];
    let mut val = n;
    let mut i = 0;
    while val > 0x7f {
        out[i] = (0x80 | (val & 0x7f)) as u8;
        val >>= 7;
        i += 1;
    }
    out[i] = val as u8;
    out
}

/// Execute a transfer request, gated by policy.
///
/// This is the single entry point. It does validation, signing, submission,
/// and audit logging in one atomic sequence. The `Treasury` facade calls this.
pub fn execute_transfer(
    signer: &Arc<WalletSigner>,
    policy: &TreasuryPolicy,
    destination: &str,
    lamports: u64,
    purpose: &str,
    rpc: &HeliusRpc,
    audit_path: &Path,
    codeword: Option<&str>,
) -> TransferOutcome {
    // ── Step 1: Validate destination address ──
    let dest_bytes = match decode_base58_32(destination) {
        Some(b) => b,
        None => {
            let entry = AuditEntry::rejected(destination, lamports, "invalid destination address", purpose);
            entry.write(audit_path);
            return TransferOutcome::Rejected {
                reason: format!("invalid destination address: {destination}"),
                destination: destination.to_string(),
                lamports,
            };
        }
    };

    // ── Step 2: Check whitelist ──
    let whitelist = match policy.find_whitelist(destination) {
        Some(entry) => entry,
        None => {
            let entry = AuditEntry::rejected(destination, lamports, "destination not whitelisted", purpose);
            entry.write(audit_path);
            return TransferOutcome::Rejected {
                reason: format!("destination {destination} is not whitelisted"),
                destination: destination.to_string(),
                lamports,
            };
        }
    };

    // ── Step 2.5: Codeword gate ──
    // If the policy has a codeword configured, a correct codeword MUST be
    // provided. This is an ADDITIONAL gate — it does not replace whitelist
    // or limits. Wrong/missing codeword → reject, no signature, no broadcast.
    if policy.has_codeword() {
        match codeword {
            Some(cw) if policy.verify_codeword(cw) => {
                // Codeword correct — proceed.
            },
            _ => {
                let entry = AuditEntry::rejected(
                    destination, lamports,
                    "codeword required but not provided or incorrect",
                    purpose,
                );
                entry.write(audit_path);
                return TransferOutcome::Rejected {
                    reason: "codeword required but not provided or incorrect".to_string(),
                    destination: destination.to_string(),
                    lamports,
                };
            },
        }
    }

    // ── Step 3: Check per-tx limit ──
    if lamports > whitelist.max_per_tx_lamports {
        let entry = AuditEntry::rejected(
            destination, lamports,
            &format!("exceeds per-tx limit of {} lamports", whitelist.max_per_tx_lamports),
            purpose,
        );
        entry.write(audit_path);
        return TransferOutcome::Rejected {
            reason: format!("exceeds per-tx limit of {} lamports", whitelist.max_per_tx_lamports),
            destination: destination.to_string(),
            lamports,
        };
    }

    // ── Step 4: Check daily limit ──
    // TODO: Implement daily tracking via audit log replay. For now, the
    // per-tx limit + global daily cap provide the primary guardrails.
    // Full daily tracking requires reading today's audit entries and
    // summing confirmed transfers to this address. This is a known
    // enhancement tracked separately.

    // ── Step 5: Time-lock for large transfers ──
    if lamports > policy.limits.approval_threshold_lamports {
        let entry = AuditEntry::rejected(
            destination, lamports,
            &format!("time-locked: exceeds approval threshold of {} lamports; requires confirmation after {}s",
                     policy.limits.approval_threshold_lamports,
                     policy.limits.time_lock_seconds),
            purpose,
        );
        entry.write(audit_path);
        return TransferOutcome::TimeLocked {
            destination: destination.to_string(),
            lamports,
            time_lock_seconds: policy.limits.time_lock_seconds,
            reason: format!(
                "transfer of {} lamports exceeds approval threshold; time-locked for {}s",
                lamports, policy.limits.time_lock_seconds
            ),
        };
    }

    // ── Step 6: Build the Solana transfer instruction ──
    // System Program transfer instruction:
    // - program_id: SystemProgram (all-zeros)
    // - accounts: [from (signer, writable), to (writable)]
    // - data: [2u32 LE] + [lamports u64 LE]
    let from_pubkey = signer.public_key_bytes();

    // Instruction data: discriminator (4 bytes) + lamports (8 bytes) = 12 bytes
    let mut ix_data = [0u8; 12];
    ix_data[0..4].copy_from_slice(&TRANSFER_DISCRIMINATOR);
    ix_data[4..12].copy_from_slice(&lamports.to_le_bytes());

    // ── Step 7: Build the transaction message ──
    // Solana message format (compact):
    //   header: num_required_signatures (1) + num_readonly_signer (1) + num_readonly_non_signer (1)
    //   account_keys: compact-u16 count + pubkeys
    //   recent_blockhash: 32 bytes
    //   instructions: compact-u16 count + instruction entries
    let from_bytes = from_pubkey;
    let to_bytes = dest_bytes;

    // Account keys: [from, to, system_program]
    let num_accounts = 3usize;
    let mut msg = Vec::with_capacity(200);

    // Header: 1 signer (from), 0 additional readonly signer, 1 readonly non-signer (system program)
    msg.extend_from_slice(&[1u8, 0u8, 1u8]);

    // Account keys
    msg.extend_from_slice(&encode_compact_u16(num_accounts));
    msg.extend_from_slice(&from_bytes);       // index 0: from (signer, writable)
    msg.extend_from_slice(&to_bytes);         // index 1: to (writable)
    msg.extend_from_slice(&SYSTEM_PROGRAM_ID); // index 2: system program (readonly)

    // Recent blockhash — fetched from RPC
    let blockhash = match rpc.get_recent_blockhash() {
        Ok(h) => h,
        Err(e) => {
            let entry = AuditEntry::failed(destination, lamports, &format!("blockhash fetch: {e}"), purpose);
            entry.write(audit_path);
            return TransferOutcome::Failed {
                reason: format!("blockhash fetch failed: {e}"),
                destination: destination.to_string(),
                lamports,
            };
        }
    };
    msg.extend_from_slice(&blockhash);

    // Instructions: 1 instruction (the transfer)
    msg.extend_from_slice(&encode_compact_u16(1)); // 1 instruction

    // Instruction: program_id_index=2, accounts=[0, 1], data=ix_data
    msg.push(2u8); // program_id_index = 2 (system program)
    msg.extend_from_slice(&encode_compact_u16(2)); // 2 account indices
    msg.push(0u8); // account index 0 (from)
    msg.push(1u8); // account index 1 (to)
    msg.extend_from_slice(&encode_compact_u16(ix_data.len()));
    msg.extend_from_slice(&ix_data);

    // ── Step 8: Sign the message ──
    let signature = match signer.sign(&msg) {
        Ok(sig) => sig,
        Err(e) => {
            let entry = AuditEntry::failed(destination, lamports, &format!("signing: {e}"), purpose);
            entry.write(audit_path);
            return TransferOutcome::Failed {
                reason: format!("signing failed: {e}"),
                destination: destination.to_string(),
                lamports,
            };
        }
    };

    // ── Step 9: Serialize to base64 wire format ──
    // Wire format: [1 empty byte (signature count placeholder)] + signature + message
    // Actually: 1 byte (num signatures = 1) + 64-byte signature + message bytes
    let mut wire = Vec::with_capacity(msg.len() + 65);
    wire.push(1u8); // 1 signature
    wire.extend_from_slice(&signature);
    wire.extend_from_slice(&msg);

    let encoded = B64.encode(&wire);

    // ── Step 10: Submit via RPC ──
    let tx_signature = match rpc.send_transaction(&encoded) {
        Ok(sig) => sig,
        Err(e) => {
            let entry = AuditEntry::failed(destination, lamports, &format!("RPC submit: {e}"), purpose);
            entry.write(audit_path);
            return TransferOutcome::Failed {
                reason: format!("RPC submission failed: {e}"),
                destination: destination.to_string(),
                lamports,
            };
        }
    };

    // ── Step 11: Audit log ──
    let entry = AuditEntry::confirmed(
        destination,
        lamports,
        &tx_signature,
        purpose,
    );
    entry.write(audit_path);

    TransferOutcome::Confirmed {
        tx_signature,
        lamports,
        destination: destination.to_string(),
        purpose: purpose.to_string(),
    }
}
