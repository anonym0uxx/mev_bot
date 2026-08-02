//! `pq-laserstream-grpc` — Helius LaserStream (Yellowstone gRPC) capture tap.
//!
//! SERVER-BUILD-ONLY (see README): the official `helius-laserstream` SDK is
//! used precisely because it owns reconnect + `from_slot` replay internally —
//! the failure mode the WS lane can only log, this lane heals. Kept minimal
//! and obviously-correct-by-reading; it cannot be compiled in the authoring
//! environment (crates.io unreachable), so nothing clever lives here.

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

/// JSON string escaping (same minimal set as the sibling crate's emit.rs).
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

fn emit(kind: &str, slot: u64, id_b58: &str, debug_payload: &str) {
    println!(
        "{{\"lane\":\"laserstream\",\"recv_unix_ms\":{},\"kind\":\"{}\",\"slot\":{},\
         \"raw_b58_or_json\":{{\"id_b58\":\"{}\",\"debug\":\"{}\"}}}}",
        now_ms(),
        kind,
        slot,
        esc(id_b58),
        esc(debug_payload)
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
                    let sig = tx
                        .transaction
                        .as_ref()
                        .map(|t| b58(&t.signature))
                        .unwrap_or_default();
                    let dbg = format!("{:?}", tx.transaction);
                    emit("transaction", tx.slot, &sig, &dbg);
                }
                Some(UpdateOneof::Account(acct)) => {
                    let key = acct
                        .account
                        .as_ref()
                        .map(|a| b58(&a.pubkey))
                        .unwrap_or_default();
                    let dbg = format!("{:?}", acct.account);
                    emit("account", acct.slot, &key, &dbg);
                }
                Some(UpdateOneof::Slot(slot)) => {
                    emit("slot", slot.slot, "", &format!("{slot:?}"));
                }
                Some(UpdateOneof::BlockMeta(meta)) => {
                    emit("block_meta", meta.slot, "", &format!("{meta:?}"));
                }
                other => {
                    emit("other", 0, "", &format!("{other:?}"));
                }
            },
        }
    }
    eprintln!("[pq-laserstream-grpc] stream ended");
}
