//! Legacy-message compiler: wire format, ordering classes, dedup-with-union,
//! shortvec encoding, size caps, and determinism.

use pump_quant_protocol::message::{
    assemble_transaction, compile_message, create_ata_idempotent, set_compute_unit_limit,
    set_compute_unit_price, spl_close_account, spl_sync_native, system_transfer, Instruction,
    MessageError, SIGNATURE_BYTES,
};
use pump_quant_protocol::venue_accounts::{
    AccountMeta, COMPUTE_BUDGET_PROGRAM_ID, SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID,
};

// ATA_PROGRAM_ID intentionally not imported; helper wiring is covered in
// tx_build.rs where the full instruction sequences are assembled.

fn pk(tag: u8) -> [u8; 32] {
    let mut k = [9u8; 32];
    k[0] = tag;
    k
}

fn bh() -> [u8; 32] {
    [0xBB; 32]
}

// -- wire format -------------------------------------------------------------

/// Hand-decode a minimal transfer message and verify every byte region.
#[test]
fn transfer_message_wire_format() {
    let payer = pk(1);
    let dest = pk(2);
    let ix = system_transfer(&payer, &dest, 42);
    let msg = compile_message(&payer, &bh(), &[ix]).unwrap();

    // Header: 1 signer, 0 ro-signed, 1 ro-unsigned (the system program).
    assert_eq!(msg.num_required_signatures, 1);
    assert_eq!(msg.num_readonly_signed, 0);
    assert_eq!(msg.num_readonly_unsigned, 1);
    assert_eq!(&msg.bytes[0..3], &[1, 0, 1]);
    // 3 keys: payer, dest, system program — in class order.
    assert_eq!(msg.bytes[3], 3);
    assert_eq!(&msg.bytes[4..36], &payer);
    assert_eq!(&msg.bytes[36..68], &dest);
    assert_eq!(&msg.bytes[68..100], &SYSTEM_PROGRAM_ID);
    // Blockhash.
    assert_eq!(&msg.bytes[100..132], &bh());
    // 1 instruction: program index 2, 2 account indices [0, 1], 12 data bytes.
    assert_eq!(msg.bytes[132], 1);
    assert_eq!(msg.bytes[133], 2);
    assert_eq!(msg.bytes[134], 2);
    assert_eq!(&msg.bytes[135..137], &[0, 1]);
    assert_eq!(msg.bytes[137], 12);
    assert_eq!(&msg.bytes[138..142], &2u32.to_le_bytes());
    assert_eq!(&msg.bytes[142..150], &42u64.to_le_bytes());
    assert_eq!(msg.bytes.len(), 150);
}

#[test]
fn identical_inputs_compile_to_identical_bytes() {
    let payer = pk(1);
    let ixs = [
        set_compute_unit_limit(120_000),
        set_compute_unit_price(5_000),
        system_transfer(&payer, &pk(2), 1),
    ];
    let a = compile_message(&payer, &bh(), &ixs).unwrap();
    let b = compile_message(&payer, &bh(), &ixs).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

/// A key referenced read-only in one instruction and writable in another
/// takes the union, and appears exactly once.
#[test]
fn dedup_takes_flag_union() {
    let payer = pk(1);
    let shared = pk(3);
    let ix_ro = Instruction {
        program_id: pk(8),
        accounts: [AccountMeta::ro(shared)].to_vec(),
        data: [1u8].to_vec(),
    };
    let ix_w = Instruction {
        program_id: pk(8),
        accounts: [AccountMeta::w(shared)].to_vec(),
        data: [2u8].to_vec(),
    };
    let msg = compile_message(&payer, &bh(), &[ix_ro, ix_w]).unwrap();
    let count = msg.account_keys.iter().filter(|k| **k == shared).count();
    assert_eq!(count, 1);
    // shared must sit in the writable non-signer region: after the payer,
    // before the read-only tail.
    let pos = msg.account_keys.iter().position(|k| *k == shared).unwrap();
    let ro_start = msg.account_keys.len() - msg.num_readonly_unsigned as usize;
    assert!(pos >= 1 && pos < ro_start);
}

/// Compute-budget instructions carry no accounts and the documented tags.
#[test]
fn compute_budget_encodings() {
    let l = set_compute_unit_limit(200_000);
    assert_eq!(l.program_id, COMPUTE_BUDGET_PROGRAM_ID);
    assert!(l.accounts.is_empty());
    assert_eq!(l.data[0], 2);
    assert_eq!(&l.data[1..5], &200_000u32.to_le_bytes());

    let p = set_compute_unit_price(7_777);
    assert_eq!(p.data[0], 3);
    assert_eq!(&p.data[1..9], &7_777u64.to_le_bytes());
}

#[test]
fn spl_helper_encodings() {
    let ata = create_ata_idempotent(&pk(1), &pk(2), &pk(1), &pk(3), &TOKEN_PROGRAM_ID);
    assert_eq!(ata.data, [1u8].to_vec());
    assert_eq!(ata.accounts.len(), 6);
    assert!(ata.accounts[0].is_signer && ata.accounts[0].is_writable);

    let sync = spl_sync_native(&pk(2), &TOKEN_PROGRAM_ID);
    assert_eq!(sync.data, [17u8].to_vec());
    assert_eq!(sync.program_id, TOKEN_PROGRAM_ID);

    let close = spl_close_account(&pk(2), &pk(1), &pk(1), &TOKEN_PROGRAM_ID);
    assert_eq!(close.data, [9u8].to_vec());
    assert_eq!(close.accounts.len(), 3);
    assert!(close.accounts[2].is_signer);
}

// -- assembly ----------------------------------------------------------------

#[test]
fn assemble_prepends_signatures() {
    let payer = pk(1);
    let msg = compile_message(&payer, &bh(), &[system_transfer(&payer, &pk(2), 1)]).unwrap();
    let sig = [0x5A; SIGNATURE_BYTES];
    let wire = assemble_transaction(&msg, &[sig]).unwrap();
    assert_eq!(wire[0], 1);
    assert_eq!(&wire[1..65], &sig);
    assert_eq!(&wire[65..], &msg.bytes[..]);
}

#[test]
fn negative_control_wrong_signature_count_refuses() {
    let payer = pk(1);
    let msg = compile_message(&payer, &bh(), &[system_transfer(&payer, &pk(2), 1)]).unwrap();
    assert_eq!(
        assemble_transaction(&msg, &[]),
        Err(MessageError::SignatureCountMismatch)
    );
    let sig = [0u8; SIGNATURE_BYTES];
    assert_eq!(
        assemble_transaction(&msg, &[sig, sig]),
        Err(MessageError::SignatureCountMismatch)
    );
}

// -- fail-closed bounds ------------------------------------------------------

#[test]
fn negative_control_empty_instruction_list_refuses() {
    assert_eq!(
        compile_message(&pk(1), &bh(), &[]).unwrap_err(),
        MessageError::Empty
    );
}

#[test]
fn negative_control_oversized_message_refuses() {
    // One instruction with data big enough to blow the 1232-byte packet cap.
    let ix = Instruction {
        program_id: pk(8),
        accounts: Vec::new(),
        data: [0u8; 1300].to_vec(),
    };
    assert_eq!(
        compile_message(&pk(1), &bh(), &[ix]).unwrap_err(),
        MessageError::TooLarge
    );
}

/// The cap is on the WIRE size (signatures included), not the message alone:
/// a message that fits alone but not with its signature must refuse.
#[test]
fn size_cap_includes_signature_envelope() {
    // Message overhead for this shape: header 3 + keys shortvec 1 + 3·32 keys
    // + blockhash 32 + ix count 1 + prog idx 1 + acct count 1 + 1 acct idx +
    // 2-byte data shortvec = 138 bytes. Wire = 65 (1 + one 64-byte sig) + msg.
    let ix = |n: usize| Instruction {
        program_id: pk(8),
        accounts: [AccountMeta::w(pk(2))].to_vec(),
        data: vec![0u8; n],
    };
    // data 1017: message 1155, wire 1220 <= 1232 — fits.
    assert!(compile_message(&pk(1), &bh(), &[ix(1017)]).is_ok());
    // data 1060: message 1198 <= 1232 alone, but wire 1263 > 1232 — must
    // refuse. A cap on the message alone would wrongly accept this.
    assert_eq!(
        compile_message(&pk(1), &bh(), &[ix(1060)]).unwrap_err(),
        MessageError::TooLarge
    );
}
