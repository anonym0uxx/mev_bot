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
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions,
};
use helius_laserstream::{subscribe, LaserstreamConfig};

/// Build the production subscribe request. `pools` = the SELECTIVE PumpSwap transaction lane: only
/// transactions touching one of these pool accounts (Yellowstone `account_include` is OR over the
/// listed accounts; separate filter-map entries are OR'd and tagged by name). The PumpSwap PROGRAM id
/// is never placed in any `account_include`, so the AMM firehose cannot be selected by accident; the
/// program-wide opt-in (`PQ_LS_INCLUDE_PUMPSWAP=1`) remains a separate, explicit switch.
fn production_request(pools: &std::collections::BTreeSet<String>) -> SubscribeRequest {
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
    if !pools.is_empty() {
        let mut pf = SubscribeRequestFilterTransactions::default();
        pf.vote = Some(false);
        pf.failed = Some(false);
        pf.account_include = pools.iter().cloned().collect();
        request.transactions.insert("pq_pools".to_string(), pf);
    }
    // Rev-30 account lane (restored): bonding-curve PDAs (owner = pump.fun program) and PumpSwap
    // pools. The daemon's ONLY price producer is the reserve-delta print derived from these account
    // snapshots; when LS is active the daemon skips its WS accountSubscribe fallback, so without
    // this subscription every candidate is refused `no_price`/`join_no_mint`. Slots feed the
    // daemon's slot clock. (The training-capture rewrite had dropped both from production mode.)
    // PQ_LS_ACCOUNT_POOLS=0 drops the program-wide PumpSwap POOL account owner from this filter.
    // Dependency trace (2026-10-23): the daemon resolves account updates only through its
    // bonding-curve PDA map; pool account updates were 89 % of `ls_account_received` and were
    // never consumed (`ls_account_unresolved`). AMM reserves come from swap EVENTS on the
    // selective `pq_pools` transaction lane. Default keeps the historical behaviour.
    let include_pool_accounts = std::env::var("PQ_LS_ACCOUNT_POOLS").map(|v| v != "0").unwrap_or(true);
    let mut owners = vec!["6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string()];
    if include_pool_accounts {
        owners.push("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA".to_string());
    }
    request.accounts = HashMap::from([(
        "pq_pools_and_curves".to_string(),
        SubscribeRequestFilterAccounts { owner: owners, ..Default::default() },
    )]);
    request.slots = HashMap::from([(
        "pq_slots".to_string(),
        SubscribeRequestFilterSlots { filter_by_commitment: Some(true), ..Default::default() },
    )]);
    request.commitment = Some(CommitmentLevel::Processed as i32);
    request
}

/// Per-filter meter: update counts and uncompressed protobuf bytes (`encoded_len` of the update),
/// attributed to every filter tag the server reports for the update (an update matched by two
/// filters is counted under both — the `shared` counter says how often that happened so credits
/// are not double-booked). Written as one JSON line per interval to `PQ_LS_METER_FILE` (append) and
/// echoed on stdout as kind `ls_meter` (the daemon ignores unknown kinds).
#[derive(Default)]
struct Meter {
    counts: std::collections::BTreeMap<String, u64>,
    bytes: std::collections::BTreeMap<String, u64>,
    shared: u64,
    total_updates: u64,
    total_bytes: u64,
    started_unix_ms: i64,
}

impl Meter {
    fn record(&mut self, filters: &[String], len: usize) {
        self.total_updates += 1;
        self.total_bytes += len as u64;
        if filters.len() > 1 {
            self.shared += 1;
        }
        let tags: Vec<&str> = if filters.is_empty() { vec!["(untagged)"] } else { filters.iter().map(String::as_str).collect() };
        for t in tags {
            *self.counts.entry(t.to_string()).or_default() += 1;
            *self.bytes.entry(t.to_string()).or_default() += len as u64;
        }
    }
    fn line(&self, pools: usize, interval_s: u64) -> String {
        serde_json::json!({
            "lane": "laserstream", "kind": "ls_meter", "recv_unix_ms": encoding::now_unix_ms(),
            "since_unix_ms": self.started_unix_ms, "interval_s": interval_s, "pools_subscribed": pools,
            "updates": self.counts, "bytes_uncompressed": self.bytes, "shared_updates": self.shared,
            "total_updates": self.total_updates, "total_bytes_uncompressed": self.total_bytes,
        })
        .to_string()
    }
}

/// Run production mode — low-latency PROCESSED transaction subscribe, plus the selective PumpSwap
/// pool lane controlled over STDIN: one JSON line per command,
///   {"cmd":"pools","set":["<pool_b58>", ...]}      replace the active pool set (bounded by cap)
///   {"cmd":"pools","add":[...],"remove":[...]}     incremental
/// Each change re-sends the FULL request through the SDK handle (Yellowstone replaces all filters on
/// every write, and the SDK persists the merged request across reconnects). Activation is reported
/// on stdout as kind `pools_subscribed` with the sidecar clock so the daemon can measure
/// migration → subscription gaps. Cap: PQ_LS_POOL_CAP (default 256).
async fn run_production(config: LaserstreamConfig) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("pq-laserstream-grpc: production mode (PROCESSED, low-latency; selective pool lane on stdin)");
    let cap: usize = env::var("PQ_LS_POOL_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let meter_file = env::var("PQ_LS_METER_FILE").ok();
    let meter_interval_s: u64 = env::var("PQ_LS_METER_INTERVAL_S").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let pools: std::sync::Arc<tokio::sync::Mutex<std::collections::BTreeSet<String>>> = Default::default();

    let (stream, handle) = subscribe(config, production_request(&std::collections::BTreeSet::new()));
    tokio::pin!(stream);

    // stdin control task
    {
        let pools = pools.clone();
        let handle = handle.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if v.get("cmd").and_then(|c| c.as_str()) != Some("pools") {
                    continue;
                }
                let mut set = pools.lock().await;
                if let Some(arr) = v.get("set").and_then(|a| a.as_array()) {
                    set.clear();
                    for s in arr.iter().filter_map(|x| x.as_str()) {
                        if set.len() < cap { set.insert(s.to_string()); }
                    }
                }
                let mut rejected_cap = 0usize;
                for s in v.get("add").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()).unwrap_or_default() {
                    if set.len() < cap || set.contains(s) { set.insert(s.to_string()); } else { rejected_cap += 1; }
                }
                for s in v.get("remove").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()).unwrap_or_default() {
                    set.remove(s);
                }
                let n = set.len();
                let req = production_request(&set);
                let snapshot: Vec<String> = set.iter().cloned().collect();
                drop(set);
                let ok = handle.write(req).await.is_ok();
                println!("{}", serde_json::json!({"lane": "laserstream", "kind": "pools_subscribed", "recv_unix_ms": encoding::now_unix_ms(), "n": n, "write_ok": ok, "rejected_cap": rejected_cap, "pools_b58": snapshot}));
            }
        });
    }

    let mut meter = Meter { started_unix_ms: encoding::now_unix_ms() as i64, ..Default::default() };
    let mut last_meter = std::time::Instant::now();
    while let Some(result) = stream.next().await {
        match result {
            Ok(update) => {
                use helius_laserstream::grpc::subscribe_update::UpdateOneof;
                use prost::Message;
                meter.record(&update.filters, update.encoded_len());
                match update.update_oneof {
                    Some(UpdateOneof::Transaction(tx_update)) => {
                        if let Some(tx_info) = tx_update.transaction {
                            // The daemon's line contract (`parse_ndjson_line`). Emitted on STDOUT only;
                            // diagnostics stay on stderr so they can never corrupt the NDJSON stream.
                            println!("{}", daemon_tx_line(tx_update.slot, &tx_info));
                        }
                    }
                    Some(UpdateOneof::Account(acct)) => {
                        if let Some(a) = acct.account.as_ref() {
                            println!("{}", daemon_account_line(acct.slot, &a.pubkey, &a.owner, &a.data, a.txn_signature.as_deref(), a.write_version));
                        }
                    }
                    Some(UpdateOneof::Slot(s)) => {
                        println!("{}", serde_json::json!({"lane": "laserstream", "kind": "slot", "slot": s.slot}));
                    }
                    _ => {}
                }
                if last_meter.elapsed().as_secs() >= meter_interval_s {
                    let n = pools.lock().await.len();
                    let line = meter.line(n, meter_interval_s);
                    println!("{line}");
                    if let Some(f) = &meter_file {
                        use std::io::Write;
                        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                            let _ = writeln!(fh, "{line}");
                        }
                    }
                    last_meter = std::time::Instant::now();
                }
            }
            Err(e) => {
                eprintln!("stream error: {e}");
            }
        }
    }
    Ok(())
}


/// One daemon-facing account line (`parse_ndjson_line` "account" contract): bonding-curve / pool
/// snapshot with the sidecar's own receive clock.
fn daemon_account_line(
    slot: u64,
    pubkey: &[u8],
    owner: &[u8],
    data: &[u8],
    txn_signature: Option<&[u8]>,
    write_version: u64,
) -> String {
    let mut v = serde_json::json!({
        "lane": "laserstream",
        "kind": "account",
        "slot": slot,
        "recv_unix_ms": encoding::now_unix_ms(),
        "pubkey_b58": encoding::b58_encode(pubkey),
        "owner_b58": encoding::b58_encode(owner),
        "data_b64": encoding::b64_encode(data),
        "write_version": write_version,
    });
    // The transaction that wrote this account state (Yellowstone `txn_signature`): lets the daemon
    // join a reserve print to its instruction EXACTLY instead of by (mint, slot, side).
    if let Some(sig) = txn_signature {
        v["txn_signature_b58"] = serde_json::Value::String(encoding::b58_encode(sig));
    }
    v.to_string()
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
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, Transaction, TransactionStatusMeta,
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
            for k in ["lane", "kind", "slot", "signature_b58", "account_keys", "instructions", "meta"] {
                assert_eq!(got[k], line[k], "field {k} diverged");
            }
            assert!(got["recv_unix_ms"].as_u64().unwrap() > 1_700_000_000_000, "the emitter stamps its own receive clock");
        }
    }
}

#[cfg(test)]
mod account_line_tests {
    #[test]
    fn account_line_matches_the_daemon_contract_fields() {
        let l = super::daemon_account_line(7, &[1u8; 32], &[2u8; 32], &[9, 8, 7], Some(&[5u8; 64]), 42);
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["kind"], "account");
        assert_eq!(v["slot"], 7);
        for k in ["lane", "recv_unix_ms", "pubkey_b58", "owner_b58", "data_b64"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["data_b64"], "CQgH");
    }
}

#[cfg(test)]
mod pool_lane_tests {
    use super::*;
    const PUMPSWAP: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

    #[test]
    fn empty_pool_set_is_the_unchanged_production_request() {
        std::env::remove_var("PQ_LS_INCLUDE_PUMPSWAP");
        let r = production_request(&Default::default());
        assert_eq!(r.transactions.len(), 1);
        let f = &r.transactions["pumpfun"];
        assert_eq!(f.account_include, vec!["6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string()]);
        assert!(r.accounts.contains_key("pq_pools_and_curves") && r.slots.contains_key("pq_slots"));
    }

    #[test]
    fn pools_become_a_separate_or_filter_without_the_program_id() {
        std::env::remove_var("PQ_LS_INCLUDE_PUMPSWAP");
        let pools: std::collections::BTreeSet<String> = ["PoolA111".to_string(), "PoolB222".to_string()].into_iter().collect();
        let r = production_request(&pools);
        assert_eq!(r.transactions.len(), 2);
        let pf = &r.transactions["pq_pools"];
        assert_eq!(pf.account_include, vec!["PoolA111".to_string(), "PoolB222".to_string()]);
        assert!(pf.account_required.is_empty() && pf.account_exclude.is_empty());
        // The PumpSwap PROGRAM never appears in any include list: the firehose cannot be selected by a pool set.
        for f in r.transactions.values() {
            assert!(!f.account_include.iter().any(|a| a == PUMPSWAP));
        }
        // pump.fun lane untouched
        assert_eq!(r.transactions["pumpfun"].account_include.len(), 1);
    }

    #[test]
    fn meter_attributes_to_every_filter_and_counts_shared() {
        let mut m = Meter::default();
        m.record(&["pumpfun".to_string()], 100);
        m.record(&["pumpfun".to_string(), "pq_pools".to_string()], 50);
        m.record(&[], 7);
        assert_eq!(m.total_updates, 3);
        assert_eq!(m.total_bytes, 157);
        assert_eq!(m.shared, 1);
        assert_eq!(m.bytes["pumpfun"], 150);
        assert_eq!(m.bytes["pq_pools"], 50);
        assert_eq!(m.counts["(untagged)"], 1);
    }
}
