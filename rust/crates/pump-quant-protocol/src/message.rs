//! Solana legacy-message compiler: instructions → the exact bytes a wallet
//! signs and a sender submits.
//!
//! ## Responsibility
//! This is the junction the commission has been missing: *"the signer signs
//! bytes, the sender submits them, and nothing currently produces the bytes."*
//! This module produces them. It compiles a payer, a recent blockhash and an
//! ordered instruction list into a Solana **legacy** wire message
//! (`num_required_signatures ‖ num_readonly_signed ‖ num_readonly_unsigned ‖
//! account keys ‖ blockhash ‖ compiled instructions`, shortvec-length-prefixed
//! per SVM spec), and assembles the final wire transaction from that message
//! plus its signatures.
//!
//! Legacy (not v0) is deliberate for V1: no address-lookup tables are needed —
//! the largest instruction here is 23 accounts — and legacy removes an entire
//! class of table-resolution failure from the live path. A v0 compiler is a
//! later, separate leaf if ALTs ever pay for themselves.
//!
//! ## Account ordering rules (SVM message spec)
//! Unique keys, in four stable classes: writable signers first (payer first of
//! all), then read-only signers, then writable non-signers, then read-only
//! non-signers. A key referenced with mixed flags across instructions takes
//! the union (any-signer, any-writable). Order within a class is first
//! appearance — stable, so identical inputs compile to identical bytes (§22).
//!
//! ## Fail-closed bounds
//! * More than 127 required signers or 256 total keys → refuse.
//! * Compiled message over [`MAX_TX_BYTES`] (1232, the IPv6-MTU packet cap
//!   Solana enforces) → refuse at build time, not at the RPC boundary.
//!
//! ## Constitution
//! * §22 — integer only, deterministic, no I/O; identical inputs → identical
//!   bytes (what makes a signed transaction reproducible in replay).
//! * §18.2 — refusal over substitution on every bound.
//! * criterion 77a — [`canonical_message_bytes`] is the byte surface the
//!   fixture-parity gate differentials.

use crate::venue_accounts::{
    AccountMeta, ATA_PROGRAM_ID, COMPUTE_BUDGET_PROGRAM_ID, SYSTEM_PROGRAM_ID,
};

/// Solana's transaction packet cap in bytes (IPv6 MTU 1280 − 40 − 8).
pub const MAX_TX_BYTES: usize = 1232;

/// An ed25519 signature length in bytes.
pub const SIGNATURE_BYTES: usize = 64;

/// A single instruction: program, ordered account metas, raw data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// The program to invoke.
    pub program_id: [u8; 32],
    /// Ordered account metas (order is part of the instruction's meaning).
    pub accounts: Vec<AccountMeta>,
    /// Raw instruction data.
    pub data: Vec<u8>,
}

/// Why a message could not be compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageError {
    /// No instructions supplied.
    Empty,
    /// More unique account keys than a legacy message can index (256).
    TooManyAccounts,
    /// More than 127 required signers.
    TooManySigners,
    /// The compiled message (plus its signature envelope) exceeds
    /// [`MAX_TX_BYTES`].
    TooLarge,
    /// Signature count does not match the message's declared signer count.
    SignatureCountMismatch,
}

/// Append a shortvec (compact-u16) length prefix.
fn push_shortvec_len(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// A compiled legacy message: ordered unique keys + header counts + bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledMessage {
    /// The exact bytes to sign.
    pub bytes: Vec<u8>,
    /// Ordered unique account keys as compiled.
    pub account_keys: Vec<[u8; 32]>,
    /// Header: number of required signatures.
    pub num_required_signatures: u8,
    /// Header: read-only signed count.
    pub num_readonly_signed: u8,
    /// Header: read-only unsigned count.
    pub num_readonly_unsigned: u8,
}

/// Compile `instructions` into a legacy message signed by `payer` against
/// `recent_blockhash`.
///
/// `payer` is always the first key and is always a writable signer, whether or
/// not any instruction references it.
pub fn compile_message(
    payer: &[u8; 32],
    recent_blockhash: &[u8; 32],
    instructions: &[Instruction],
) -> Result<CompiledMessage, MessageError> {
    if instructions.is_empty() {
        return Err(MessageError::Empty);
    }

    // Gather unique keys with union flags, preserving first-appearance order.
    // Payer is inserted first as writable signer.
    let mut keys: Vec<[u8; 32]> = Vec::new();
    let mut signer: Vec<bool> = Vec::new();
    let mut writable: Vec<bool> = Vec::new();

    let mut upsert = |pk: &[u8; 32], s: bool, w: bool| {
        for i in 0..keys.len() {
            if &keys[i] == pk {
                signer[i] = signer[i] || s;
                writable[i] = writable[i] || w;
                return;
            }
        }
        keys.push(*pk);
        signer.push(s);
        writable.push(w);
    };

    upsert(payer, true, true);
    for ix in instructions {
        for m in &ix.accounts {
            upsert(&m.pubkey, m.is_signer, m.is_writable);
        }
        upsert(&ix.program_id, false, false);
    }

    if keys.len() > 256 {
        return Err(MessageError::TooManyAccounts);
    }

    // Stable four-class ordering; payer stays first because it is the first
    // writable signer by first-appearance order.
    let mut ordered: Vec<usize> = Vec::with_capacity(keys.len());
    for class in 0..4usize {
        for i in 0..keys.len() {
            let c = match (signer[i], writable[i]) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            if c == class {
                ordered.push(i);
            }
        }
    }

    let num_signers = signer.iter().filter(|&&s| s).count();
    if num_signers > 127 {
        return Err(MessageError::TooManySigners);
    }
    let num_ro_signed = ordered
        .iter()
        .filter(|&&i| signer[i] && !writable[i])
        .count();
    let num_ro_unsigned = ordered
        .iter()
        .filter(|&&i| !signer[i] && !writable[i])
        .count();

    // Position of each original key index in the ordered list.
    let index_of = |pk: &[u8; 32]| -> u8 {
        for (pos, &i) in ordered.iter().enumerate() {
            if &keys[i] == pk {
                return pos as u8;
            }
        }
        // Unreachable: every referenced key was upserted above. Returning the
        // payer index would silently corrupt the message, so map to 0 only
        // after the debug assertion that documents impossibility.
        debug_assert!(false, "key not found after upsert");
        0
    };

    let mut bytes = Vec::with_capacity(256);
    bytes.push(num_signers as u8);
    bytes.push(num_ro_signed as u8);
    bytes.push(num_ro_unsigned as u8);
    push_shortvec_len(&mut bytes, ordered.len());
    for &i in &ordered {
        bytes.extend_from_slice(&keys[i]);
    }
    bytes.extend_from_slice(recent_blockhash);
    push_shortvec_len(&mut bytes, instructions.len());
    for ix in instructions {
        bytes.push(index_of(&ix.program_id));
        push_shortvec_len(&mut bytes, ix.accounts.len());
        for m in &ix.accounts {
            bytes.push(index_of(&m.pubkey));
        }
        push_shortvec_len(&mut bytes, ix.data.len());
        bytes.extend_from_slice(&ix.data);
    }

    // Enforce the packet cap on the FINAL wire size: shortvec(sig count) +
    // 64 bytes per signature + message.
    let mut sig_prefix = Vec::with_capacity(3);
    push_shortvec_len(&mut sig_prefix, num_signers);
    let wire = sig_prefix.len() + num_signers * SIGNATURE_BYTES + bytes.len();
    if wire > MAX_TX_BYTES {
        return Err(MessageError::TooLarge);
    }

    let account_keys = ordered.iter().map(|&i| keys[i]).collect();
    Ok(CompiledMessage {
        bytes,
        account_keys,
        num_required_signatures: num_signers as u8,
        num_readonly_signed: num_ro_signed as u8,
        num_readonly_unsigned: num_ro_unsigned as u8,
    })
}

/// The canonical byte surface for fixture parity (criterion 77a): exactly the
/// bytes that would be signed.
pub fn canonical_message_bytes(msg: &CompiledMessage) -> &[u8] {
    &msg.bytes
}

/// Assemble the wire transaction: `shortvec(n_sigs) ‖ sigs ‖ message`.
///
/// Signature order must match the message's signer order (payer first).
pub fn assemble_transaction(
    msg: &CompiledMessage,
    signatures: &[[u8; SIGNATURE_BYTES]],
) -> Result<Vec<u8>, MessageError> {
    if signatures.len() != msg.num_required_signatures as usize {
        return Err(MessageError::SignatureCountMismatch);
    }
    let mut out = Vec::with_capacity(4 + signatures.len() * SIGNATURE_BYTES + msg.bytes.len());
    push_shortvec_len(&mut out, signatures.len());
    for sig in signatures {
        out.extend_from_slice(sig);
    }
    out.extend_from_slice(&msg.bytes);
    if out.len() > MAX_TX_BYTES {
        return Err(MessageError::TooLarge);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Standard helper instructions (compute budget, system transfer, SPL/ATA).
// ---------------------------------------------------------------------------

/// ComputeBudget `SetComputeUnitLimit` (tag 2, u32 LE).
pub fn set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(2u8);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: Vec::new(),
        data,
    }
}

/// ComputeBudget `SetComputeUnitPrice` (tag 3, u64 LE micro-lamports).
///
/// Every Sender transaction must carry this **and** a tip transfer — both,
/// not either (`SENDER-SUBMISSION-SPEC-V1.md` §8.3).
pub fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(3u8);
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: Vec::new(),
        data,
    }
}

/// System program `Transfer` (enum tag 2 as u32 LE, lamports u64 LE) — the
/// tip instruction, among other uses.
pub fn system_transfer(from: &[u8; 32], to: &[u8; 32], lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    let accounts = vec![AccountMeta::ws(*from), AccountMeta::w(*to)];
    Instruction {
        program_id: SYSTEM_PROGRAM_ID,
        accounts,
        data,
    }
}

/// Associated-token-program `CreateIdempotent` (tag 1): create the ATA if it
/// does not exist, succeed silently if it does. Used so a first-ever buy and
/// a repeat buy compile to the same instruction sequence (determinism over
/// conditional assembly).
pub fn create_ata_idempotent(
    payer: &[u8; 32],
    ata: &[u8; 32],
    owner: &[u8; 32],
    mint: &[u8; 32],
    token_program: &[u8; 32],
) -> Instruction {
    let accounts = vec![
        AccountMeta::ws(*payer),
        AccountMeta::w(*ata),
        AccountMeta::ro(*owner),
        AccountMeta::ro(*mint),
        AccountMeta::ro(SYSTEM_PROGRAM_ID),
        AccountMeta::ro(*token_program),
    ];
    Instruction {
        program_id: ATA_PROGRAM_ID,
        accounts,
        data: vec![1u8],
    }
}

/// SPL-token `SyncNative` (tag 17): reconcile a WSOL ATA's amount with its
/// lamports after a system transfer into it.
pub fn spl_sync_native(wsol_ata: &[u8; 32], token_program: &[u8; 32]) -> Instruction {
    let accounts = vec![AccountMeta::w(*wsol_ata)];
    Instruction {
        program_id: *token_program,
        accounts,
        data: vec![17u8],
    }
}

/// SPL-token `CloseAccount` (tag 9): close a token account, refunding its
/// lamports (WSOL unwrap / ATA rent reclaim) to `destination`.
pub fn spl_close_account(
    account: &[u8; 32],
    destination: &[u8; 32],
    owner: &[u8; 32],
    token_program: &[u8; 32],
) -> Instruction {
    let accounts = vec![
        AccountMeta::w(*account),
        AccountMeta::w(*destination),
        AccountMeta::ws(*owner),
    ];
    Instruction {
        program_id: *token_program,
        accounts,
        data: vec![9u8],
    }
}
