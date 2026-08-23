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
//! ## Constitution
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
    filter.account_include = vec![
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string(),
    ];
    request.transactions = HashMap::from([("pumpfun".to_string(), filter)]);
    request.commitment = Some(CommitmentLevel::Processed as i32);

    let (stream, _handle) = subscribe(config, request);
    tokio::pin!(stream);

    while let Some(result) = stream.next().await {
        match result {
            Ok(update) => {
                if let Some(helius_laserstream::grpc::subscribe_update::UpdateOneof::Transaction(tx_update)) = update.update_oneof {
                    if let Some(tx_info) = tx_update.transaction {
                        let sig = encoding::b58_encode(&tx_info.signature);
                        let slot = tx_update.slot;
                        eprintln!("tx slot={slot} sig={sig}");
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
