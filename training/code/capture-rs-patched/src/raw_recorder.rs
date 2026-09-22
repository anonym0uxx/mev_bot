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
