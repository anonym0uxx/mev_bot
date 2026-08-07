//! `pq-laserstream-grpc` — Helius LaserStream (Yellowstone gRPC) capture tap.
//!
//! SERVER-BUILD-ONLY (see README): the official `helius-laserstream` SDK is
//! used precisely because it owns reconnect + `from_slot` replay internally —
//! the failure mode the WS lane can only log, this lane heals. Kept minimal
//! and obviously-correct-by-reading; it cannot be compiled in the authoring
//! environment (crates.io unreachable), so nothing clever lives here.
//!
//! Output: NDJSON lines on stdout, one per gRPC update. The paper_session
//! (and live trading) binary spawns this as a subprocess and reads stdout
//! line-by-line. Each line is a JSON object with the schema:
//!
//! Transaction:  {"lane":"laserstream","kind":"transaction","slot":N,"recv_unix_ms":N,
//!                 "signature_b58":"...","account_keys":["b58",...],
//!                 "instructions":[{"program_b58":"...","data_b64":"...","accounts":[0,1,2]}]}
//! Account:      {"lane":"laserstream","kind":"account","slot":N,"recv_unix_ms":N,
//!                 "pubkey_b58":"...","data_b64":"...","owner_b58":"..."}
//! Slot:         {"lane":"laserstream","kind":"slot","slot":N,"recv_unix_ms":N}

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use helius_laserstream::grpc::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions,
};
use helius_laserstream::{subscribe, LaserstreamConfig};

/// PumpSwap AMM program (pool-account owner filter + default tx include).
const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
/// pump.fun bonding-curve program (default tx include).
const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Minimal base58 (Bitcoin alphabet) for signatures/pubkeys.
fn b58(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = vec![0];
    for &byte in bytes {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(std::iter::repeat('1').take(zeros));
    out.extend(digits.iter().rev().map(|&d| ALPHABET[d as usize] as char));
    out
}

/// Minimal base64 encoder (standard alphabet) for instruction data + account data.
fn b64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// JSON string escaping.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Emit a structured transaction NDJSON line.
/// Extracts account keys and instruction data from the protobuf transaction.
fn emit_transaction(slot: u64, tx_update: &helius_laserstream::grpc::SubscribeUpdateTransaction) {
    let tx_info = tx_update.transaction.as_ref();
    let sig_b58 = tx_info
        .and_then(|t| Some(b58(&t.signature)))
        .unwrap_or_default();

    // Extract account keys from the transaction message.
    let account_keys: Vec<String> = tx_info
        .and_then(|t| t.transaction.as_ref())
        .and_then(|t| t.message.as_ref())
        .map(|msg| {
            msg.account_keys
                .iter()
                .map(|k| b58(k))
                .collect()
        })
        .unwrap_or_default();

    // Extract instructions from the message.
    let instructions_json: Vec<String> = tx_info
        .and_then(|t| t.transaction.as_ref())
        .and_then(|t| t.message.as_ref())
        .map(|msg| {
            msg.instructions
                .iter()
                .map(|ix| {
                    // program_id_index is a 0-based index into account_keys
                    let prog_b58 = if (ix.program_id_index as usize) < msg.account_keys.len() {
                        b58(&msg.account_keys[ix.program_id_index as usize])
                    } else {
                        String::new()
                    };
                    let data_b64 = b64_encode(&ix.data);
                    let accounts_json: Vec<String> = ix
                        .accounts
                        .iter()
                        .map(|a| a.to_string())
                        .collect();
                    format!(
                        "{{\"program_b58\":\"{}\",\"data_b64\":\"{}\",\"accounts\":[{}]}}",
                        esc(&prog_b58),
                        esc(&data_b64),
                        accounts_json.join(",")
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    println!(
        "{{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":{},\"recv_unix_ms\":{},\
         \"signature_b58\":\"{}\",\"account_keys\":[{}],\"instructions\":[{}]}}",
        slot,
        now_ms(),
        esc(&sig_b58),
        account_keys.iter().map(|k| format!("\"{}\"", esc(k))).collect::<Vec<_>>().join(","),
        instructions_json.join(",")
    );
}

/// Emit a structured account NDJSON line.
fn emit_account(slot: u64, acct_update: &helius_laserstream::grpc::SubscribeUpdateAccount) {
    let acct_info = acct_update.account.as_ref();
    let key_b58 = acct_info
        .map(|a| b58(&a.pubkey))
        .unwrap_or_default();
    let owner_b58 = acct_info
        .map(|a| b58(&a.owner))
        .unwrap_or_default();
    let data_b64 = acct_info
        .map(|a| b64_encode(&a.data))
        .unwrap_or_default();

    println!(
        "{{\"lane\":\"laserstream\",\"kind\":\"account\",\"slot\":{},\"recv_unix_ms\":{},\
         \"pubkey_b58\":\"{}\",\"owner_b58\":\"{}\",\"data_b64\":\"{}\"}}",
        slot,
        now_ms(),
        esc(&key_b58),
        esc(&owner_b58),
        esc(&data_b64)
    );
}

/// Emit a slot NDJSON line.
fn emit_slot(slot_num: u64) {
    println!(
        "{{\"lane\":\"laserstream\",\"kind\":\"slot\",\"slot\":{},\"recv_unix_ms\":{}}}",
        slot_num,
        now_ms()
    );
}

fn read_list_file(path: &str) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
        Err(e) => {
            eprintln!("[pq-laserstream-grpc] cannot read {path}: {e}");
            std::process::exit(2);
        }
    }
}

#[tokio::main]
async fn main() {
    let api_key = match std::env::var("HELIUS_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("[pq-laserstream-grpc] ARMING_FAILED: HELIUS_API_KEY not set (exit 3)");
            std::process::exit(3);
        }
    };
    let endpoint = match std::env::var("LASERSTREAM_ENDPOINT") {
        Ok(e) if !e.trim().is_empty() => e,
        _ => {
            eprintln!("[pq-laserstream-grpc] ARMING_FAILED: LASERSTREAM_ENDPOINT not set (exit 3)\n  no silent default — the endpoint the Business key travels must be explicit");
            std::process::exit(3);
        }
    };

    // Flags: --accounts-file f, --programs p1,p2 (defaults: pump programs).
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut include: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match (flag.as_str(), it.next()) {
            ("--accounts-file", Some(path)) => include.extend(read_list_file(path)),
            ("--programs", Some(csv)) => {
                include.extend(csv.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from));
            }
            _ => {
                eprintln!("usage: pq-laserstream-grpc [--accounts-file f] [--programs p1,p2]");
                std::process::exit(2);
            }
        }
    }
    if include.is_empty() {
        include = vec![PUMPSWAP_PROGRAM.to_string(), PUMP_PROGRAM.to_string()];
    }

    let config = LaserstreamConfig::new(endpoint.clone(), api_key);
    let request = SubscribeRequest {
        transactions: HashMap::from([(
            "pq_tx".to_string(),
            SubscribeRequestFilterTransactions {
                vote: Some(false),
                failed: Some(false),
                account_include: include.clone(),
                ..Default::default()
            },
        )]),
        slots: HashMap::from([(
            "pq_slots".to_string(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: Some(true),
                ..Default::default()
            },
        )]),
        accounts: HashMap::from([(
            "pq_pools".to_string(),
            SubscribeRequestFilterAccounts {
                owner: vec![PUMPSWAP_PROGRAM.to_string()],
                ..Default::default()
            },
        )]),
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    };

    eprintln!(
        "[pq-laserstream-grpc] streaming from {endpoint} ({} include keys); \
         SDK owns reconnect + from_slot replay",
        include.len()
    );
    let (stream, _handle) = subscribe(config, request);
    tokio::pin!(stream);
    while let Some(item) = stream.next().await {
        match item {
            Err(e) => eprintln!("[pq-laserstream-grpc] stream error (SDK will resume): {e}"),
            Ok(update) => match update.update_oneof {
                Some(UpdateOneof::Transaction(tx)) => {
                    emit_transaction(tx.slot, &tx);
                }
                Some(UpdateOneof::Account(acct)) => {
                    emit_account(acct.slot, &acct);
                }
                Some(UpdateOneof::Slot(slot)) => {
                    emit_slot(slot.slot);
                }
                Some(UpdateOneof::BlockMeta(meta)) => {
                    // Block meta not needed for trade decode — skip silently.
                    let _ = meta;
                }
                other => {
                    // Unknown update types — log to stderr, don't pollute stdout.
                    eprintln!("[pq-laserstream-grpc] unhandled update: {other:?}");
                }
            },
        }
    }
    eprintln!("[pq-laserstream-grpc] stream ended");
}
