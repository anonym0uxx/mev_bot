//! Lossless raw recorder — writes NDJSON lines compressed with zstd.
//!
//! Each line is a JSON object capturing the FULL protobuf-derived truth for a
//! LaserStream update (transaction, account, slot, block-meta). No fields are
//! dropped or reduced — the goal is lossless provenance for future Qwen training.
//!
//! The recorder rotates files at a configurable line count so that individual
//! .ndjson.zst parts stay manageable (`part0000`, `part0001`, …). Each part
//! is independently decompressible.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use zstd::stream::Encoder;

use crate::encoding::{b58_encode, b64_encode, sha256_hex};

/// Lines per raw file part (before rotation).
const RAW_LINES_PER_PART: u32 = 50_000;

/// A serializable raw record envelope. The `record_type` discriminates the
/// payload; `payload` is a serde_json::Value carrying the lossless fields.
#[derive(Serialize)]
pub struct RawRecord {
    pub record_type: String,
    pub slot: u64,
    pub recv_unix_ms: u64,
    pub record_index: u64,
    pub payload: serde_json::Value,
}

/// The raw recorder — owns a zstd encoder writing NDJSON lines to a rotating
/// set of .ndjson.zst files. Thread-safe via Mutex.
pub struct RawRecorder {
    inner: Mutex<RawRecorderInner>,
    base_dir: PathBuf,
    session: String,
}

struct RawRecorderInner {
    current_part: u32,
    lines_in_part: u32,
    total_lines: u64,
    /// BufWriter wrapping a zstd Encoder<File>. On rotation/finalize, we
    /// flush the BufWriter, extract the Encoder via into_inner, and call
    /// .finish() to write the zstd end frame.
    writer: BufWriter<Encoder<'static, File>>,
}

/// Extract the zstd Encoder from a BufWriter, flushing first, and finalize
/// it (writes the zstd end frame). Returns the underlying File.
fn finalize_writer(writer: BufWriter<Encoder<'static, File>>) -> std::io::Result<File> {
    // Flush the BufWriter buffer into the Encoder, then consume the Encoder.
    let encoder = writer
        .into_inner()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("into_inner: {e}")))?;
    // finish() writes the zstd end frame and returns the underlying File.
    encoder.finish()
}

/// Create a dummy writer for mem::replace purposes. On Windows, use NUL; on
/// Unix, use /dev/null. This writer is immediately discarded.
fn make_dummy_writer() -> std::io::Result<BufWriter<Encoder<'static, File>>> {
    let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let file = File::create(null_path)?;
    let encoder = zstd::stream::Encoder::new(file, 3)?;
    Ok(BufWriter::new(encoder))
}

impl RawRecorder {
    /// Create a new raw recorder. Opens the first part file immediately.
    pub fn new(base_dir: PathBuf, session: &str) -> std::io::Result<Self> {
        let part0 = part_path(&base_dir, session, 0);
        let file = File::create(&part0)?;
        let encoder = zstd::stream::Encoder::new(file, 3)?; // level 3: good ratio + fast
        let writer = BufWriter::new(encoder);

        Ok(Self {
            inner: Mutex::new(RawRecorderInner {
                current_part: 0,
                lines_in_part: 0,
                total_lines: 0,
                writer,
            }),
            base_dir,
            session: session.to_string(),
        })
    }

    /// Write one raw record. Serializes to JSON, appends a newline, and
    /// rotates the part file if needed.
    pub fn write(&self, record_type: &str, slot: u64, payload: serde_json::Value) -> std::io::Result<u64> {
        let mut inner = self.inner.lock().unwrap();
        let recv_unix_ms = crate::encoding::now_unix_ms();
        let record_index = inner.total_lines;
        let record = RawRecord {
            record_type: record_type.to_string(),
            slot,
            recv_unix_ms,
            record_index,
            payload,
        };
        let json = serde_json::to_string(&record)?;
        writeln!(inner.writer, "{json}")?;
        inner.total_lines += 1;
        inner.lines_in_part += 1;

        if inner.lines_in_part >= RAW_LINES_PER_PART {
            Self::rotate(&self.base_dir, &self.session, &mut inner)?;
        }
        Ok(record_index)
    }

    /// Flush + close the current part, open the next.
    fn rotate(base_dir: &Path, session: &str, inner: &mut RawRecorderInner) -> std::io::Result<()> {
        // Take the old writer out via mem::replace, finalize it (zstd end frame).
        let dummy = make_dummy_writer()?;
        let old_writer = std::mem::replace(&mut inner.writer, dummy);
        let _file = finalize_writer(old_writer)?;

        // Open next part.
        inner.current_part += 1;
        inner.lines_in_part = 0;
        let next_path = part_path(base_dir, session, inner.current_part);
        let file = File::create(&next_path)?;
        let encoder = zstd::stream::Encoder::new(file, 3)?;
        inner.writer = BufWriter::new(encoder);
        Ok(())
    }

    /// Finalize: flush + close the current part. Called on shutdown.
    pub fn finalize(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let dummy = make_dummy_writer()?;
        let old_writer = std::mem::replace(&mut inner.writer, dummy);
        drop(finalize_writer(old_writer)?);
        Ok(())
    }

    /// Total records written so far.
    pub fn total_records(&self) -> u64 {
        self.inner.lock().unwrap().total_lines
    }
}

/// Compute the path for part file number `part`.
fn part_path(base_dir: &Path, session: &str, part: u32) -> PathBuf {
    base_dir.join(format!(
        "pumpfun_laserstream_raw_v1_{session}_part{part:04}.ndjson.zst"
    ))
}

/// ─── Payload builders ──────────────────────────────────────────────────

/// Build a lossless transaction payload from the raw protobuf fields.
/// This captures EVERYTHING LaserStream exposes: full message, meta, inner
/// instructions, logs, balances, fees, CU, errors, loaded addresses.
pub fn build_tx_payload(
    tx_info: &helius_laserstream::grpc::SubscribeUpdateTransactionInfo,
) -> serde_json::Value {
    let signature_b58 = b58_encode(&tx_info.signature);
    let is_vote = tx_info.is_vote;
    let tx_index = tx_info.index;

    // ── Transaction message ──
    let msg_json = tx_info
        .transaction
        .as_ref()
        .and_then(|t| t.message.as_ref())
        .map(|msg| {
            let account_keys_b58: Vec<String> =
                msg.account_keys.iter().map(|k| b58_encode(k)).collect();
            let instructions_json: Vec<serde_json::Value> = msg
                .instructions
                .iter()
                .map(|ix| {
                    serde_json::json!({
                        "program_id_index": ix.program_id_index,
                        "accounts_b64": b64_encode(&ix.accounts),
                        "data_b64": b64_encode(&ix.data),
                    })
                })
                .collect();
            let header = msg.header.as_ref().map(|h| {
                serde_json::json!({
                    "num_required_signatures": h.num_required_signatures,
                    "num_readonly_signed_accounts": h.num_readonly_signed_accounts,
                    "num_readonly_unsigned_accounts": h.num_readonly_unsigned_accounts,
                })
            });
            let lookups: Vec<serde_json::Value> = msg
                .address_table_lookups
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "account_key_b58": b58_encode(&l.account_key),
                        "writable_indexes": l.writable_indexes,
                        "readonly_indexes": l.readonly_indexes,
                    })
                })
                .collect();
            serde_json::json!({
                "header": header,
                "account_keys_b58": account_keys_b58,
                "recent_blockhash_b58": b58_encode(&msg.recent_blockhash),
                "instructions": instructions_json,
                "versioned": msg.versioned,
                "address_table_lookups": lookups,
            })
        });

    let signatures_b58: Vec<String> = tx_info
        .transaction
        .as_ref()
        .map(|t| t.signatures.iter().map(|s| b58_encode(s)).collect())
        .unwrap_or_default();

    // ── Transaction status meta ──
    let meta_json = tx_info.meta.as_ref().map(|meta| {
        let inner_instructions: Vec<serde_json::Value> = meta
            .inner_instructions
            .iter()
            .map(|group| {
                let insts: Vec<serde_json::Value> = group
                    .instructions
                    .iter()
                    .map(|ii| {
                        serde_json::json!({
                            "program_id_index": ii.program_id_index,
                            "accounts_b64": b64_encode(&ii.accounts),
                            "data_b64": b64_encode(&ii.data),
                            "stack_height": ii.stack_height,
                        })
                    })
                    .collect();
                serde_json::json!({"index": group.index, "instructions": insts})
            })
            .collect();

        let pre_token_balances: Vec<serde_json::Value> = meta
            .pre_token_balances
            .iter()
            .map(|tb| {
                let ui = tb.ui_token_amount.as_ref().map(|u| {
                    serde_json::json!({
                        "ui_amount": u.ui_amount,
                        "decimals": u.decimals,
                        "amount": u.amount,
                        "ui_amount_string": u.ui_amount_string,
                    })
                });
                serde_json::json!({
                    "account_index": tb.account_index,
                    "mint": tb.mint,
                    "owner": tb.owner,
                    "program_id": tb.program_id,
                    "ui_token_amount": ui,
                })
            })
            .collect();

        let post_token_balances: Vec<serde_json::Value> = meta
            .post_token_balances
            .iter()
            .map(|tb| {
                let ui = tb.ui_token_amount.as_ref().map(|u| {
                    serde_json::json!({
                        "ui_amount": u.ui_amount,
                        "decimals": u.decimals,
                        "amount": u.amount,
                        "ui_amount_string": u.ui_amount_string,
                    })
                });
                serde_json::json!({
                    "account_index": tb.account_index,
                    "mint": tb.mint,
                    "owner": tb.owner,
                    "program_id": tb.program_id,
                    "ui_token_amount": ui,
                })
            })
            .collect();

        let err_hex = meta
            .err
            .as_ref()
            .map(|e| crate::encoding::hex_encode(&e.err));
        let return_data = meta.return_data.as_ref().map(|rd| {
            serde_json::json!({
                "program_id_b58": b58_encode(&rd.program_id),
                "data_b64": b64_encode(&rd.data),
            })
        });

        let loaded_writable: Vec<String> = meta
            .loaded_writable_addresses
            .iter()
            .map(|a| b58_encode(a))
            .collect();
        let loaded_readonly: Vec<String> = meta
            .loaded_readonly_addresses
            .iter()
            .map(|a| b58_encode(a))
            .collect();
        let log_messages: Vec<String> = meta.log_messages.clone();

        serde_json::json!({
            "err_hex": err_hex,
            "err_is_none": meta.err.is_none(),
            "fee": meta.fee,
            "pre_balances": meta.pre_balances,
            "post_balances": meta.post_balances,
            "inner_instructions": inner_instructions,
            "inner_instructions_none": meta.inner_instructions_none,
            "log_messages": log_messages,
            "log_messages_none": meta.log_messages_none,
            "pre_token_balances": pre_token_balances,
            "post_token_balances": post_token_balances,
            "loaded_writable_addresses_b58": loaded_writable,
            "loaded_readonly_addresses_b58": loaded_readonly,
            "return_data": return_data,
            "return_data_none": meta.return_data_none,
            "compute_units_consumed": meta.compute_units_consumed,
            "cost_units": meta.cost_units,
        })
    });

    // ── Raw-record hash (for dedupe + cross-ref) ──
    let raw_hash = sha256_hex(&tx_info.signature);

    serde_json::json!({
        "signature_b58": signature_b58,
        "raw_hash": raw_hash,
        "is_vote": is_vote,
        "tx_index": tx_index,
        "message": msg_json,
        "signatures_b58": signatures_b58,
        "meta": meta_json,
    })
}

/// Build a lossless account-update payload.
pub fn build_account_payload(
    acct_info: &helius_laserstream::grpc::SubscribeUpdateAccountInfo,
) -> serde_json::Value {
    let pubkey_b58 = b58_encode(&acct_info.pubkey);
    let owner_b58 = b58_encode(&acct_info.owner);
    let data_b64 = b64_encode(&acct_info.data);
    let txn_sig_b58 = acct_info.txn_signature.as_ref().map(|s| b58_encode(s));
    let raw_hash = sha256_hex(&acct_info.data);

    serde_json::json!({
        "pubkey_b58": pubkey_b58,
        "lamports": acct_info.lamports,
        "owner_b58": owner_b58,
        "executable": acct_info.executable,
        "rent_epoch": acct_info.rent_epoch,
        "data_b64": data_b64,
        "data_len": acct_info.data.len(),
        "write_version": acct_info.write_version,
        "txn_signature_b58": txn_sig_b58,
        "raw_hash": raw_hash,
    })
}

/// Build a slot-update payload.
pub fn build_slot_payload(
    slot: &helius_laserstream::grpc::SubscribeUpdateSlot,
) -> serde_json::Value {
    serde_json::json!({
        "slot": slot.slot,
        "parent": slot.parent,
        "status": slot_status_to_str(slot.status),
    })
}

/// Build a block-meta payload (slot, blockhash, parent, height, time, tx count).
pub fn build_block_meta_payload(
    meta: &helius_laserstream::grpc::SubscribeUpdateBlockMeta,
) -> serde_json::Value {
    let block_time = meta.block_time.as_ref().map(|bt| bt.timestamp);
    let block_height = meta.block_height.as_ref().map(|bh| bh.block_height);
    serde_json::json!({
        "slot": meta.slot,
        "blockhash": meta.blockhash,
        "parent_slot": meta.parent_slot,
        "parent_blockhash": meta.parent_blockhash,
        "block_height": block_height,
        "block_time": block_time,
        "executed_transaction_count": meta.executed_transaction_count,
        "entries_count": meta.entries_count,
    })
}

/// Map the SlotStatus enum int to a string (for human readability).
fn slot_status_to_str(status: i32) -> &'static str {
    match status {
        0 => "Processed",
        1 => "Confirmed",
        2 => "Finalized",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod capture_serialization_tests {
    use super::*;
    use helius_laserstream::grpc::{
        SubscribeUpdateAccountInfo, SubscribeUpdateSlot, SubscribeUpdateTransactionInfo,
    };
    use helius_laserstream::solana::storage::confirmed_block::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message,
        MessageAddressTableLookup, MessageHeader, ReturnData, TokenBalance, Transaction,
        TransactionStatusMeta, UiTokenAmount,
    };
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Synthetic protobuf fixtures, NOT observed or signature-verified transactions.
    // These tests exercise the production builders AND NDJSON/zstd writer; no
    // serializer/encoder copies or new dependencies are used. Golden encodings
    // were calculated independently using integer divmod/base64/hashlib, not
    // crate::encoding. Ownership means only the provider's supplied field;
    // nothing here proves beneficial ownership, execution, settlement or finality.
    const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
    const KEY_ONE: &str = "4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi";
    const KEY_TWO: &str = "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR";
    const SIGNATURE_ONE: &str =
        "1GMkH3brNXiNNs1tiFZHu4yZSRrzJwxi5wB9bHFtMinfCXNnR1adh8Vo8NTheK4evneedH4qmvjeqcBBNAefgS";
    const SIGNATURE_TWO: &str =
        "67rpwLCuS5DGA8KGZXKsVQ7dnPb9goRLoKfgGbLfQg9WoLUgNY77E2jT11fem3coV9nAkguBACzrU1iyZM4B8roQ";
    const ABOVE_F64_EXACT_INTEGER: u64 = 9_007_199_254_740_993;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            // Atomic create_dir prevents sharing even with stale directories or
            // another process. Never remove a directory this test did not create.
            loop {
                let path = std::env::temp_dir().join(format!(
                    "pq-capture-serialization-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("create fixture directory: {e}"),
                }
            }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn recorded(record_type: &str, payload: Value) -> Value {
        let dir = ScratchDir::new();
        let recorder = RawRecorder::new(dir.0.clone(), "offline-fixture").unwrap();
        assert_eq!(recorder.write(record_type, 42, payload).unwrap(), 0);
        assert_eq!(recorder.total_records(), 1);
        recorder.finalize().unwrap();
        drop(recorder);
        let compressed = std::fs::read(part_path(&dir.0, "offline-fixture", 0)).unwrap();
        let bytes = zstd::stream::decode_all(compressed.as_slice()).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'), "NDJSON must end in a newline");
        let text = std::str::from_utf8(&bytes).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 1, "one complete JSON record, no split payload");
        let record: Value = serde_json::from_str(lines[0]).unwrap();
        let envelope = record.as_object().unwrap();
        assert_eq!(envelope.len(), 5);
        assert_eq!(record["record_type"], record_type);
        assert_eq!(record["slot"].as_u64(), Some(42));
        assert_eq!(record["record_index"].as_u64(), Some(0));
        assert!(record["recv_unix_ms"].as_u64().is_some());
        assert!(record["payload"].is_object());
        record["payload"].clone()
    }

    fn token_balance(amount: &str, ui_amount_string: &str, owner: &str) -> TokenBalance {
        TokenBalance {
            account_index: 3,
            mint: KEY_TWO.into(),
            owner: owner.into(),
            program_id: KEY_ONE.into(),
            ui_token_amount: Some(UiTokenAmount {
                // Deliberately rounded UI input must never replace raw amount.
                ui_amount: 9_007_199.254_740_992,
                decimals: 9,
                amount: amount.into(),
                ui_amount_string: ui_amount_string.into(),
            }),
        }
    }

    fn transaction_fixture() -> SubscribeUpdateTransactionInfo {
        SubscribeUpdateTransactionInfo {
            signature: (0u8..64).collect(),
            is_vote: false,
            index: ABOVE_F64_EXACT_INTEGER,
            transaction: Some(Transaction {
                signatures: vec![(0u8..64).collect(), vec![255; 64]],
                message: Some(Message {
                    header: Some(MessageHeader {
                        num_required_signatures: 2,
                        num_readonly_signed_accounts: 0,
                        num_readonly_unsigned_accounts: 1,
                    }),
                    account_keys: vec![vec![1; 32], vec![2; 32], vec![0; 32]],
                    recent_blockhash: vec![2; 32],
                    instructions: vec![
                        CompiledInstruction {
                            program_id_index: 2,
                            accounts: vec![0, 3, 4],
                            data: vec![0, 255, 128],
                        },
                        CompiledInstruction {
                            program_id_index: 2,
                            accounts: vec![4, 3, 0],
                            data: vec![1],
                        },
                        CompiledInstruction {
                            program_id_index: 2,
                            accounts: vec![],
                            data: vec![],
                        },
                    ],
                    versioned: true,
                    address_table_lookups: vec![MessageAddressTableLookup {
                        account_key: vec![1; 32],
                        writable_indexes: vec![7, 0, 255],
                        readonly_indexes: vec![9, 2],
                    }],
                    ..Default::default()
                }),
            }),
            meta: Some(TransactionStatusMeta {
                fee: ABOVE_F64_EXACT_INTEGER,
                pre_balances: vec![u64::MAX, ABOVE_F64_EXACT_INTEGER, 0],
                post_balances: vec![u64::MAX - 1, ABOVE_F64_EXACT_INTEGER - 1, 1],
                pre_token_balances: vec![token_balance(
                    "18446744073709551615",
                    "18446744073.709551615",
                    "provider-owner-before",
                )],
                post_token_balances: vec![token_balance(
                    "9007199254740993",
                    "9007199.254740993",
                    "provider-owner-after",
                )],
                // Preserve received order, not a sort by outer index or program.
                inner_instructions: vec![
                    InnerInstructions {
                        index: 2,
                        instructions: vec![
                            InnerInstruction {
                                program_id_index: 4,
                                accounts: vec![3, 0, 3, 4],
                                data: vec![255, 0],
                                stack_height: Some(3),
                            },
                            InnerInstruction {
                                program_id_index: 2,
                                accounts: vec![4, 0],
                                data: vec![0, 128, 255],
                                stack_height: None,
                            },
                        ],
                    },
                    InnerInstructions {
                        index: 0,
                        instructions: vec![InnerInstruction {
                            program_id_index: 3,
                            accounts: vec![],
                            data: vec![1],
                            stack_height: Some(2),
                        }],
                    },
                ],
                log_messages: vec!["fixture\nnot another NDJSON record".into()],
                loaded_writable_addresses: vec![vec![2; 32], vec![1; 32]],
                loaded_readonly_addresses: vec![vec![1; 32], vec![2; 32]],
                return_data: Some(ReturnData {
                    program_id: vec![0; 32],
                    data: vec![0, 255, 128],
                }),
                compute_units_consumed: Some(ABOVE_F64_EXACT_INTEGER),
                cost_units: Some(u64::MAX),
                ..Default::default()
            }),
        }
    }

    fn account_fixture() -> SubscribeUpdateAccountInfo {
        SubscribeUpdateAccountInfo {
            pubkey: vec![0; 32],
            lamports: ABOVE_F64_EXACT_INTEGER,
            owner: vec![0; 32],
            executable: false,
            rent_epoch: u64::MAX,
            data: vec![0, 255, 128, 0, 1, 2, 13, 10, 34, 92, 254, 0],
            write_version: ABOVE_F64_EXACT_INTEGER,
            txn_signature: Some((0u8..64).collect()),
        }
    }

    #[test]
    fn system_pubkey_is_exactly_32_ones_in_captured_payloads() {
        let mut tx = transaction_fixture();
        let message = tx.transaction.as_mut().unwrap().message.as_mut().unwrap();
        message.recent_blockhash = vec![0; 32];
        message.address_table_lookups[0].account_key = vec![0; 32];
        let meta = tx.meta.as_mut().unwrap();
        meta.loaded_writable_addresses = vec![vec![0; 32]];
        meta.loaded_readonly_addresses = vec![vec![0; 32]];
        let payload = recorded("transaction", build_tx_payload(&tx));
        for field in [
            &payload["message"]["account_keys_b58"][2],
            &payload["message"]["recent_blockhash_b58"],
            &payload["message"]["address_table_lookups"][0]["account_key_b58"],
            &payload["meta"]["loaded_writable_addresses_b58"][0],
            &payload["meta"]["loaded_readonly_addresses_b58"][0],
            &payload["meta"]["return_data"]["program_id_b58"],
        ] {
            assert_eq!(field.as_str(), Some(SYSTEM_PROGRAM));
            assert_eq!(field.as_str().unwrap().len(), 32);
        }
        let payload = recorded("account", build_account_payload(&account_fixture()));
        assert_eq!(payload["pubkey_b58"], SYSTEM_PROGRAM);
        assert_eq!(payload["owner_b58"], SYSTEM_PROGRAM);
    }

    #[test]
    fn nonzero_loaded_keys_and_signatures_match_independent_goldens() {
        let payload = recorded("transaction", build_tx_payload(&transaction_fixture()));
        assert_eq!(payload["signature_b58"], SIGNATURE_ONE);
        assert_eq!(
            payload["signatures_b58"],
            json!([SIGNATURE_ONE, SIGNATURE_TWO])
        );
        assert_eq!(
            payload["raw_hash"],
            "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108"
        );
        assert_eq!(
            payload["message"]["account_keys_b58"],
            json!([KEY_ONE, KEY_TWO, SYSTEM_PROGRAM])
        );
        assert_eq!(payload["message"]["recent_blockhash_b58"], KEY_TWO);
        assert_eq!(
            payload["message"]["address_table_lookups"],
            json!([{
                "account_key_b58": KEY_ONE,
                "writable_indexes": [7, 0, 255],
                "readonly_indexes": [9, 2]
            }])
        );
        assert_eq!(
            payload["meta"]["loaded_writable_addresses_b58"],
            json!([KEY_TWO, KEY_ONE])
        );
        assert_eq!(
            payload["meta"]["loaded_readonly_addresses_b58"],
            json!([KEY_ONE, KEY_TWO])
        );
        let mut account = account_fixture();
        account.pubkey = vec![1; 32];
        account.owner = vec![2; 32];
        let payload = recorded("account", build_account_payload(&account));
        assert_eq!(payload["pubkey_b58"], KEY_ONE);
        assert_eq!(payload["owner_b58"], KEY_TWO);
        assert_eq!(payload["txn_signature_b58"], SIGNATURE_ONE);
    }

    #[test]
    fn raw_integer_precision_survives_ndjson_zstd_roundtrip() {
        let payload = recorded("transaction", build_tx_payload(&transaction_fixture()));
        assert_eq!(payload["tx_index"].as_u64(), Some(ABOVE_F64_EXACT_INTEGER));
        let meta = &payload["meta"];
        assert_eq!(meta["fee"].as_u64(), Some(ABOVE_F64_EXACT_INTEGER));
        assert_eq!(
            meta["pre_balances"],
            json!([u64::MAX, ABOVE_F64_EXACT_INTEGER, 0])
        );
        assert_eq!(
            meta["post_balances"],
            json!([u64::MAX - 1, ABOVE_F64_EXACT_INTEGER - 1, 1])
        );
        assert_eq!(
            meta["compute_units_consumed"].as_u64(),
            Some(ABOVE_F64_EXACT_INTEGER)
        );
        assert_eq!(meta["cost_units"].as_u64(), Some(u64::MAX));
        for (field, raw, ui) in [
            (
                "pre_token_balances",
                "18446744073709551615",
                "18446744073.709551615",
            ),
            (
                "post_token_balances",
                "9007199254740993",
                "9007199.254740993",
            ),
        ] {
            let amount = &meta[field][0]["ui_token_amount"];
            assert_eq!(amount["amount"].as_str(), Some(raw));
            assert_eq!(amount["ui_amount_string"].as_str(), Some(ui));
            assert_eq!(amount["decimals"].as_u64(), Some(9));
        }
        let account = recorded("account", build_account_payload(&account_fixture()));
        assert_eq!(account["lamports"].as_u64(), Some(ABOVE_F64_EXACT_INTEGER));
        assert_eq!(account["rent_epoch"].as_u64(), Some(u64::MAX));
        assert_eq!(
            account["write_version"].as_u64(),
            Some(ABOVE_F64_EXACT_INTEGER)
        );
    }

    #[test]
    fn account_bytes_are_preserved_without_text_or_layout_decoding() {
        let payload = recorded("account", build_account_payload(&account_fixture()));
        assert_eq!(payload["data_b64"], "AP+AAAECDQoiXP4A");
        assert_eq!(payload["data_len"].as_u64(), Some(12));
        assert_eq!(
            payload["raw_hash"],
            "bab26b3c654947e947f27f44cc8d03baad4af2178fd148c4e6cdf20419d99374"
        );
        assert_eq!(payload["executable"], false);
        let mut empty = account_fixture();
        empty.data.clear();
        empty.txn_signature = None;
        let payload = recorded("account", build_account_payload(&empty));
        assert_eq!(payload["data_b64"], "");
        assert_eq!(payload["data_len"].as_u64(), Some(0));
        assert_eq!(
            payload["raw_hash"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(payload.get("txn_signature_b58"), Some(&Value::Null));
    }

    #[test]
    fn inner_instruction_group_order_and_raw_bytes_are_preserved() {
        let payload = recorded("transaction", build_tx_payload(&transaction_fixture()));
        assert_eq!(
            payload["message"]["instructions"],
            json!([
                {"program_id_index": 2, "accounts_b64": "AAME", "data_b64": "AP+A"},
                {"program_id_index": 2, "accounts_b64": "BAMA", "data_b64": "AQ=="},
                {"program_id_index": 2, "accounts_b64": "", "data_b64": ""}
            ])
        );
        assert_eq!(
            payload["meta"]["inner_instructions"],
            json!([
                {"index": 2, "instructions": [
                    {"program_id_index": 4, "accounts_b64": "AwADBA==", "data_b64": "/wA=", "stack_height": 3},
                    {"program_id_index": 2, "accounts_b64": "BAA=", "data_b64": "AID/", "stack_height": null}
                ]},
                {"index": 0, "instructions": [
                    {"program_id_index": 3, "accounts_b64": "", "data_b64": "AQ==", "stack_height": 2}
                ]}
            ])
        );
        assert_eq!(payload["meta"]["inner_instructions_none"], false);
        assert_eq!(
            payload["meta"]["return_data"],
            json!({"program_id_b58": SYSTEM_PROGRAM, "data_b64": "AP+A"})
        );
        assert_eq!(
            payload["meta"]["log_messages"],
            json!(["fixture\nnot another NDJSON record"])
        );
    }

    #[test]
    fn provider_owner_fields_and_processed_status_are_not_truth_claims() {
        let tx = recorded("transaction", build_tx_payload(&transaction_fixture()));
        // Different provider-supplied owner strings survive; no canonical owner
        // is resolved from keys, token deltas, instruction accounts, or fee payer.
        assert_eq!(
            tx["meta"]["pre_token_balances"][0]["owner"],
            "provider-owner-before"
        );
        assert_eq!(
            tx["meta"]["post_token_balances"][0]["owner"],
            "provider-owner-after"
        );
        let mut account = account_fixture();
        account.owner = vec![2; 32];
        let account = recorded("account", build_account_payload(&account));
        assert_eq!(account["owner_b58"], KEY_TWO);
        let slot = recorded(
            "slot",
            build_slot_payload(&SubscribeUpdateSlot {
                slot: 42,
                parent: Some(41),
                status: 0,
                ..Default::default()
            }),
        );
        assert_eq!(
            slot,
            json!({"slot": 42, "parent": 41, "status": "Processed"})
        );
        for payload in [&tx, &account] {
            for unsupported_claim in [
                "true_owner",
                "beneficial_owner",
                "resolved_owner",
                "finalized",
                "is_finalized",
                "finality",
                "commitment",
                "settled",
            ] {
                assert!(
                    payload.get(unsupported_claim).is_none(),
                    "invented {unsupported_claim}"
                );
            }
        }
        // These are serialization contracts only, not evidence that a supplied
        // owner controls an account or that a captured transaction was finalized.
    }
}
