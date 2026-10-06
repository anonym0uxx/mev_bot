//! pq-laserstream-grpc — Helius LaserStream gRPC client for Pump.fun data capture.
//!
//! ## Modes
//!
//! 1. **Production (default)**: Low-latency transaction subscribe with
//!    PROCESSED commitment. Unchanged from the original binary.
//!
//! 2. **`--training-capture`**: Broad capture mode for Qwen training data.
//!    Uses CONFIRMED commitment, no mayhem/cashback/complete/account-required
//!    filters. Captures ALL non-vote Pump.fun + PumpSwap transactions plus
//!    account updates and slot/block metadata. Writes 3 artifacts:
//!    - `pumpfun_laserstream_raw_v1_<SESSION>_partXXXX.ndjson.zst` (lossless)
//!    - `pumpfun_laserstream_events_v1_<SESSION>.ndjson` (causal events)
//!    - `pumpfun_laserstream_manifest_v1_<SESSION>.json` (metadata)
//!
//! ## Operator
//! * No secrets are ever logged. The endpoint host is recorded in the manifest
//!   but the API key is never written to any file.
//! * Training capture uses CONFIRMED commitment (not PROCESSED) to avoid
//!   capturing fork-rolled transactions.
//! * The production/low-latency path is completely unchanged.

mod encoding;
mod raw_recorder;
mod normalizer;
mod manifest;
mod events_writer;
mod capture;

use std::collections::HashMap;
use std::env;

use futures::StreamExt;
use helius_laserstream::grpc::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
};
use helius_laserstream::{subscribe, LaserstreamConfig};

/// Run production mode — low-latency PROCESSED transaction subscribe.
/// This is UNCHANGED from the original binary behavior.
async fn run_production(config: LaserstreamConfig) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("pq-laserstream-grpc: production mode (PROCESSED, low-latency)");

    let mut request = SubscribeRequest::default();
    let mut filter = SubscribeRequestFilterTransactions::default();
    filter.vote = Some(false);
    filter.failed = Some(false);
    // pump.fun curve, plus PumpSwap when opted in (AMM swap events arrive as CPIs inside PumpSwap transactions).
    // OPT-IN via `PQ_LS_INCLUDE_PUMPSWAP=1`: PumpSwap volume is billed (docs/HELIUS_BUDGET_2026-07-29.md),
    // so the production subscription is unchanged unless the operator asks for AMM discovery.
    filter.account_include = vec!["6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string()];
    if env::var("PQ_LS_INCLUDE_PUMPSWAP").map(|v| v == "1").unwrap_or(false) {
        filter
            .account_include
            .push("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".to_string());
    }
    request.transactions = HashMap::from([("pumpfun".to_string(), filter)]);
    request.commitment = Some(CommitmentLevel::Processed as i32);

    let (stream, _handle) = subscribe(config, request);
    tokio::pin!(stream);

    while let Some(result) = stream.next().await {
        match result {
            Ok(update) => {
                if let Some(helius_laserstream::grpc::subscribe_update::UpdateOneof::Transaction(tx_update)) = update.update_oneof {
                    if let Some(tx_info) = tx_update.transaction {
                        // The daemon's line contract (`parse_ndjson_line`). Emitted on STDOUT only;
                        // diagnostics stay on stderr so they can never corrupt the NDJSON stream.
                        println!("{}", daemon_tx_line(tx_update.slot, &tx_info));
                    }
                }
            }
            Err(e) => {
                eprintln!("stream error: {e}");
            }
        }
    }
    Ok(())
}


/// One daemon-facing transaction line. Carries what the engine's decoders need and the old emitter
/// dropped: INNER (CPI) instructions (PumpSwap swap events live there), loaded ALT addresses (so
/// instruction account indices resolve), and `meta.fee` / `meta.compute_units_consumed`.
/// `recv_unix_ms` is the sidecar's own receive clock, stamped here and never re-derived downstream.
/// Account-key order is the protocol's: static keys, then loaded-writable, then loaded-readonly.
fn daemon_tx_line(
    slot: u64,
    tx_info: &helius_laserstream::grpc::SubscribeUpdateTransactionInfo,
) -> String {
    let recv_unix_ms = encoding::now_unix_ms();
    let msg = tx_info.transaction.as_ref().and_then(|t| t.message.as_ref());
    let meta = tx_info.meta.as_ref();
    let mut keys: Vec<String> = msg
        .map(|m| m.account_keys.iter().map(|k| encoding::b58_encode(k)).collect())
        .unwrap_or_default();
    if let Some(m) = meta {
        keys.extend(m.loaded_writable_addresses.iter().map(|k| encoding::b58_encode(k)));
        keys.extend(m.loaded_readonly_addresses.iter().map(|k| encoding::b58_encode(k)));
    }
    let ix_json = |program_id_index: u32, data: &[u8], accounts: &[u8]| -> Option<serde_json::Value> {
        let prog = keys.get(program_id_index as usize)?;
        Some(serde_json::json!({
            "program_b58": prog,
            "data_b64": encoding::b64_encode(data),
            "accounts": accounts.iter().map(|a| *a as u32).collect::<Vec<u32>>(),
        }))
    };
    let mut instructions: Vec<serde_json::Value> = Vec::new();
    if let Some(m) = msg {
        for ix in &m.instructions {
            if let Some(v) = ix_json(ix.program_id_index, &ix.data, &ix.accounts) {
                instructions.push(v);
            }
        }
    }
    if let Some(m) = meta {
        for group in &m.inner_instructions {
            for ii in &group.instructions {
                if let Some(v) = ix_json(ii.program_id_index, &ii.data, &ii.accounts) {
                    instructions.push(v);
                }
            }
        }
    }
    serde_json::json!({
        "lane": "laserstream",
        "kind": "transaction",
        "slot": slot,
        "recv_unix_ms": recv_unix_ms,
        "signature_b58": encoding::b58_encode(&tx_info.signature),
        "account_keys": keys,
        "instructions": instructions,
        "meta": {
            "fee": meta.map(|m| m.fee),
            "compute_units_consumed": meta.and_then(|m| m.compute_units_consumed),
            // Per-line success evidence: `meta.err` absent. Absent meta => field omitted (unknown).
            "tx_ok": meta.map(|m| u64::from(m.err.is_none())),
        },
    })
    .to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let training_mode = args.iter().any(|a| a == "--training-capture");
    let smoke_mode = args.iter().any(|a| a == "--smoke");
    let duration_minutes: u64 = if smoke_mode {
        1 // ~1 minute for smoke test
    } else if let Some(idx) = args.iter().position(|a| a == "--duration") {
        args.get(idx + 1).and_then(|s| s.parse().ok()).unwrap_or(300)
    } else {
        300 // default 300 minutes (~5 hours)
    };

    // Read endpoint + API key from env (PQ_CREDS_FILE / dotenvy or direct env).
    let endpoint = env::var("LASERSTREAM_ENDPOINT").unwrap_or_else(|_| {
        eprintln!("ERROR: LASERSTREAM_ENDPOINT not set");
        std::process::exit(1);
    });
    let api_key = env::var("HELIUS_API_KEY").unwrap_or_else(|_| {
        eprintln!("ERROR: HELIUS_API_KEY not set");
        std::process::exit(1);
    });

    let endpoint_host = endpoint.split('?').next().unwrap_or("unknown").to_string();
    eprintln!("Connecting to LaserStream: {endpoint_host}");

    let config = LaserstreamConfig::new(endpoint, api_key);

    if !training_mode {
        return run_production(config).await;
    }

    // ─── Training capture mode ──
    eprintln!("=== TRAINING CAPTURE MODE ===");
    eprintln!("Duration: {duration_minutes} minutes");
    eprintln!("Commitment: CONFIRMED (training mode)");
    eprintln!("Filters: BROAD (no mayhem/cashback/complete/account-required/data-slice optimizations)");

    // Determine output directory — local ignored dir, never committed.
    let data_dir = env::var("TRAINING_CAPTURE_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        // Default: training-data/ next to the binary / repo.
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(manifest_dir).join("training-data")
    });
    std::fs::create_dir_all(&data_dir)?;
    eprintln!("Output dir: {}", data_dir.display());

    // Generate session ID: timestamp + PID for uniqueness.
    let session_id = format!(
        "{}_{:06}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        std::process::id() % 1_000_000
    );

    // Get repo SHA (from git).
    let repo_sha = get_repo_sha();

    // Our wallet public key (if set, for is_our_wallet flagging — no secret).
    let our_wallet = env::var("WALLET_ADDRESS").ok();

    let capture = capture::TrainingCapture::new(
        config,
        data_dir,
        session_id.clone(),
        repo_sha,
        endpoint_host.to_string(),
        duration_minutes,
        our_wallet,
    );

    capture.run(smoke_mode).await
}

/// Get the current git SHA of the repo (for manifest provenance).
fn get_repo_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod emitted_line_acceptance {
    //! SERIALIZER acceptance: a Yellowstone-shaped `SubscribeUpdateTransactionInfo` is rebuilt from a
    //! real transaction's daemon line, pushed through the REAL `daemon_tx_line`, and compared with that
    //! line (everything except the receive clock, which the emitter stamps itself). It proves the
    //! emitter keeps inner instructions, ALT-loaded key order, `meta.fee` and `compute_units_consumed`.
    //! It does NOT prove a running LaserStream subscription produces such a message: that live capture
    //! is the named Windows acceptance step.
    use super::*;
    use helius_laserstream::grpc::SubscribeUpdateTransactionInfo;
    use helius_laserstream::solana::storage::confirmed_block::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, Transaction, TransactionError, TransactionStatusMeta,
    };

    fn b58_decode(s: &str) -> Vec<u8> {
        const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut out: Vec<u8> = Vec::new();
        for c in s.bytes() {
            let mut carry = A.iter().position(|&x| x == c).expect("b58 char") as u32;
            for b in out.iter_mut() {
                carry += u32::from(*b) * 58;
                *b = (carry & 0xff) as u8;
                carry >>= 8;
            }
            while carry > 0 {
                out.push((carry & 0xff) as u8);
                carry >>= 8;
            }
        }
        out.extend(std::iter::repeat(0u8).take(s.bytes().take_while(|&c| c == b'1').count()));
        out.reverse();
        out
    }
    fn b64_decode(s: &str) -> Vec<u8> {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
        for c in s.bytes().filter(|&c| c != b'=') {
            acc = (acc << 6) | A.iter().position(|&x| x == c).expect("b64 char") as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((acc >> bits) & 0xff) as u8);
            }
        }
        out
    }

    /// The real serializer's `tx_ok` for the three cases the daemon's parser must distinguish:
    /// success -> 1, failed (meta.err set) -> 0, no meta at all -> null (parsed as UNKNOWN, never success).
    #[test]
    fn tx_ok_serializes_success_failure_and_missing_meta() {
        let mut ok = SubscribeUpdateTransactionInfo { signature: vec![1; 64], ..Default::default() };
        ok.meta = Some(TransactionStatusMeta { fee: 5000, compute_units_consumed: Some(7), ..Default::default() });
        let mut bad = ok.clone();
        bad.meta = Some(TransactionStatusMeta {
            fee: 5000,
            compute_units_consumed: Some(7),
            err: Some(TransactionError { err: vec![1, 2, 3] }),
            ..Default::default()
        });
        let mut none = ok.clone();
        none.meta = None;
        let v = |i: &SubscribeUpdateTransactionInfo| -> serde_json::Value { serde_json::from_str(&daemon_tx_line(9, i)).unwrap() };
        assert_eq!(v(&ok)["meta"]["tx_ok"], 1);
        assert_eq!(v(&bad)["meta"]["tx_ok"], 0);
        assert!(v(&none)["meta"]["tx_ok"].is_null(), "no meta must serialize null, which the parser reads as unknown");
    }

    #[test]
    fn emitted_line_roundtrips_real_pumpswap_transactions() {
        let raw = include_str!("../tests_fixtures/pumpswap_rpc_txs.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for item in arr {
            let line = &item["line"];
            let keys: Vec<Vec<u8>> = line["account_keys"].as_array().unwrap().iter().map(|k| b58_decode(k.as_str().unwrap())).collect();
            // Split keys across static / loaded-writable / loaded-readonly to exercise the ordering.
            let n = keys.len();
            let (s_end, w_end) = (n - 6, n - 3);
            let all = line["instructions"].as_array().unwrap();
            let prog_idx = |p: &str| keys.iter().position(|k| *k == b58_decode(p)).unwrap() as u32;
            let mk = |i: &serde_json::Value| (prog_idx(i["program_b58"].as_str().unwrap()), b64_decode(i["data_b64"].as_str().unwrap()), i["accounts"].as_array().unwrap().iter().map(|a| a.as_u64().unwrap() as u8).collect::<Vec<u8>>());
            // Outer vs inner are not distinguishable in the flat line, so make the first 5 outer and the
            // rest one inner group; the emitter's output order (outer then inner) is what is checked.
            let split = 5.min(all.len());
            let outer: Vec<CompiledInstruction> = all[..split].iter().map(|i| { let (p, d, a) = mk(i); CompiledInstruction { program_id_index: p, accounts: a, data: d } }).collect();
            let inner: Vec<InnerInstruction> = all[split..].iter().map(|i| { let (p, d, a) = mk(i); InnerInstruction { program_id_index: p, accounts: a, data: d, stack_height: None } }).collect();
            let info = SubscribeUpdateTransactionInfo {
                signature: b58_decode(line["signature_b58"].as_str().unwrap()),
                transaction: Some(Transaction {
                    message: Some(Message { account_keys: keys[..s_end].to_vec(), instructions: outer, ..Default::default() }),
                    ..Default::default()
                }),
                meta: Some(TransactionStatusMeta {
                    fee: line["meta"]["fee"].as_u64().unwrap(),
                    compute_units_consumed: line["meta"]["compute_units_consumed"].as_u64(),
                    loaded_writable_addresses: keys[s_end..w_end].to_vec(),
                    loaded_readonly_addresses: keys[w_end..].to_vec(),
                    inner_instructions: vec![InnerInstructions { index: 0, instructions: inner }],
                    ..Default::default()
                }),
                ..Default::default()
            };
            let got: serde_json::Value = serde_json::from_str(&daemon_tx_line(line["slot"].as_u64().unwrap(), &info)).unwrap();
            for k in ["lane", "kind", "slot", "signature_b58", "account_keys", "instructions"] {
                assert_eq!(got[k], line[k], "field {k} diverged");
            }
            // `meta` = the fixture's fee/CU (frozen, compared exactly) PLUS `tx_ok` (added with the event
            // path). The fixture txs carry no error, so the serializer must emit success = 1.
            assert_eq!(got["meta"]["fee"], line["meta"]["fee"]);
            assert_eq!(got["meta"]["compute_units_consumed"], line["meta"]["compute_units_consumed"]);
            assert_eq!(got["meta"]["tx_ok"], 1, "no-error tx must serialize tx_ok=1");
            assert_eq!(got["meta"].as_object().unwrap().len(), 3, "meta must carry exactly fee, CU, tx_ok");
            assert!(got["recv_unix_ms"].as_u64().unwrap() > 1_700_000_000_000, "the emitter stamps its own receive clock");
        }
    }
}
