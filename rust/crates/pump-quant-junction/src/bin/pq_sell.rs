//! One-shot manual sell tool for orphaned pump.fun positions.
//!
//! Reuses the daemon's exact LiveOutboundSink pipeline:
//!   state-fetch → build_pump_sell_message → sign → assemble → submit via Helius Sender
//!
//! Usage:
//!   pq-sell --mint <mint-address> --tokens <token_base_units> [--slippage-bps 500]
//!   Env: PQ_CREDS_FILE=..., HOME=...  (same as daemon)
//!
//! The token amount is in base units (e.g. 520583912517 = 520,583.91 tokens at 6 decimals).

use std::process::ExitCode;

fn main() -> ExitCode {
    // ── Parse args ───────────────────────────────────────────────────────
    let mut mint_str = String::new();
    let mut token_amount: u64 = 0;
    let mut slippage_bps: u16 = 500;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mint" if i + 1 < args.len() => {
                mint_str = args[i + 1].clone();
                i += 2;
            }
            "--tokens" if i + 1 < args.len() => {
                token_amount = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--slippage-bps" if i + 1 < args.len() => {
                slippage_bps = args[i + 1].parse().unwrap_or(500);
                i += 2;
            }
            _ => {
                eprintln!("Unknown arg: {}", args[i]);
                return ExitCode::from(2);
            }
        }
    }

    if mint_str.is_empty() || token_amount == 0 {
        eprintln!("Usage: pq-sell --mint <mint> --tokens <base_units> [--slippage-bps 500]");
        return ExitCode::from(2);
    }

    eprintln!("[pq-sell] *** MANUAL SELL TOOL ***");
    eprintln!("[pq-sell] Mint: {mint_str}");
    eprintln!("[pq-sell] Token amount: {token_amount} base units");
    eprintln!("[pq-sell] Slippage: {slippage_bps} bps");

    // ── Load dependencies ────────────────────────────────────────────────
    use pump_quant_junction::live_adapters::{
        HeliusSenderSubmitter, LiveWalletSigner, RpcLiveStateFetcher,
    };
    use pump_quant_execution::ex_live_io_traits::{
        LiveSigner, LiveStateFetcher, LiveSubmitter,
    };
    use pump_quant_execution::ex_live_sink::{LiveOutboundSink, LiveSinkConfig};
    use pump_quant_execution::ex_outbound_sink::{AdmitRecord, OutboundSink, OutboundOutcome};
    use pump_quant_protocol::layout::{
        LayoutKey, LayoutRegistry, Side, Variant, Venue, VerifiedLayout,
    };
    use pump_quant_protocol::tx_build::{ComputePlan, TipPlan};
    use pump_quant_protocol::venue_accounts::FeeTail;
    use std::sync::Arc;

    // ── Load credentials (same logic as daemon) ──────────────────────────
    let creds_path = std::env::var("PQ_CREDS_FILE").unwrap_or_else(|_| {
        format!(
            "{}/.hermes/creds/pump-quant.env",
            std::env::var("HOME").unwrap_or_else(|_| "C:/Users/Alon".to_string())
        )
    });
    eprintln!("[pq-sell] Loading credentials from {creds_path}");

    let creds = match std::fs::read_to_string(&creds_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[pq-sell] FATAL: cannot read creds file: {e}");
            return ExitCode::from(1);
        }
    };

    let mut helius_rpc_url = String::new();
    let mut helius_sender_url = String::new();
    let mut helius_api_key = String::new();
    let mut wallet_address = String::new();

    for line in creds.lines() {
        if let Some(v) = line.strip_prefix("HELIUS_WS_URL=") {
            let v = v.trim();
            if v.starts_with("wss://") {
                helius_rpc_url = format!("https://{}", &v[6..]);
            } else if v.starts_with("https://") {
                helius_rpc_url = v.to_string();
            }
        }
        if let Some(v) = line.strip_prefix("SENDER_ENDPOINT=") {
            if helius_sender_url.is_empty() {
                helius_sender_url = v.trim().to_string();
            }
        }
        if let Some(v) = line.strip_prefix("HELIUS_API_KEY=") {
            helius_api_key = v.trim().to_string();
        }
        if let Some(v) = line.strip_prefix("WALLET_ADDRESS=") {
            wallet_address = v.trim().to_string();
        }
    }

    if !helius_rpc_url.is_empty() && !helius_api_key.is_empty() && !helius_rpc_url.contains("api-key=") {
        helius_rpc_url = format!("{}/?api-key={}", helius_rpc_url, helius_api_key);
    }

    if helius_rpc_url.is_empty() {
        eprintln!("[pq-sell] FATAL: no HELIUS_WS_URL found");
        return ExitCode::from(1);
    }
    if wallet_address.is_empty() {
        eprintln!("[pq-sell] FATAL: no WALLET_ADDRESS found");
        return ExitCode::from(1);
    }

    eprintln!("[pq-sell] Wallet: {wallet_address}");

    // ── Decode mint string to [u8; 32] ───────────────────────────────────
    let mint_arr: [u8; 32] = match pump_quant_ingest::base58::decode_pubkey(&mint_str) {
        Some(arr) => arr,
        None => {
            eprintln!("[pq-sell] FATAL: cannot base58-decode mint: {mint_str}");
            return ExitCode::from(1);
        }
    };

    // ── Load signer ──────────────────────────────────────────────────────
    let keypair_path = format!(
        "{}/.hermes/keys/wallet-keypair.json",
        std::env::var("HOME").unwrap_or_else(|_| "C:/Users/Alon".to_string())
    );
    eprintln!("[pq-sell] Loading keypair from {keypair_path}");

    let signer = match LiveWalletSigner::load(
        std::path::Path::new(&keypair_path),
        &wallet_address,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[pq-sell] FATAL: signer load failed: {e:?}");
            return ExitCode::from(1);
        }
    };
    eprintln!("[pq-sell] Signer loaded: {}", signer.address());

    // ── Construct state fetcher ──────────────────────────────────────────
    let state_fetcher = RpcLiveStateFetcher::new(helius_rpc_url.clone());
    eprintln!("[pq-sell] State fetcher constructed");

    // ── Construct submitter ──────────────────────────────────────────────
    let sender_url = if helius_sender_url.is_empty() {
        helius_rpc_url.replacen("/rpc", "/rpc/sender", 1)
    } else {
        helius_sender_url
    };

    let submitter = if !helius_api_key.is_empty() {
        match HeliusSenderSubmitter::new_with_api_key(
            &sender_url, &helius_api_key, true, false,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[pq-sell] FATAL: submitter construction failed: {e:?}");
                return ExitCode::from(1);
            }
        }
    } else {
        match HeliusSenderSubmitter::new(&sender_url, true, false) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[pq-sell] FATAL: submitter construction failed: {e:?}");
                return ExitCode::from(1);
            }
        }
    };
    eprintln!("[pq-sell] Submitter constructed (Helius Sender)");

    // ── Build LayoutRegistry (identical to daemon) ───────────────────────
    let mut registry = LayoutRegistry::new();
    for &token_2022 in &[false, true] {
        for &cashback in &[false, true] {
            // Buy layouts
            let buy_key = LayoutKey {
                venue: Venue::PumpFun, side: Side::Buy,
                variant: Variant { cashback, token_2022, non_sol_quote: false, reversed_pool: false },
            };
            let mut sig = [0u8; 64];
            sig[0] = (if token_2022 { 0xCC } else { 0xAA }) + if cashback { 0x10 } else { 0 };
            sig[31] = sig[0];
            for &count in &[17usize, 18usize] {
                registry.record_verified(VerifiedLayout {
                    key: buy_key, account_count: count,
                    verifying_slot: 439_558_014, verifying_signature: sig,
                }).expect("buy layout record should succeed");
            }
            // Sell layouts
            let sell_key = LayoutKey {
                venue: Venue::PumpFun, side: Side::Sell,
                variant: Variant { cashback, token_2022, non_sol_quote: false, reversed_pool: false },
            };
            let base_count = if cashback { 16 } else { 15 };
            let mut sig2 = [0u8; 64];
            sig2[0] = (if token_2022 { 0xDD } else { 0xBB }) + if cashback { 0x10 } else { 0 };
            sig2[31] = sig2[0];
            for &count in &[base_count, base_count + 1] {
                registry.record_verified(VerifiedLayout {
                    key: sell_key, account_count: count,
                    verifying_slot: 439_558_014, verifying_signature: sig2,
                }).expect("sell layout record should succeed");
            }
        }
    }
    eprintln!("[pq-sell] LayoutRegistry populated");

    // ── Sink config (same as daemon) ─────────────────────────────────────
    let compute = ComputePlan { unit_limit: 120_000, unit_price_micro_lamports: 5_000 };
    let tip = Some(TipPlan {
        to: [
            0x1a, 0xa2, 0xf0, 0x5a, 0x6f, 0x89, 0x50, 0xfc,
            0xbf, 0x5d, 0xf9, 0xca, 0x39, 0x48, 0x1c, 0x6d,
            0xf1, 0x33, 0x05, 0xc8, 0xb8, 0x7c, 0x64, 0x4f,
            0x4d, 0x8c, 0x6d, 0x82, 0x0b, 0x37, 0x89, 0xa6,
        ],
        lamports: 5_000,
    });

    let sink_config = LiveSinkConfig {
        compute, tip,
        fee_tail: FeeTail::None,
        max_slippage_bps: slippage_bps,
    };

    // ── Construct sink and execute ───────────────────────────────────────
    let live_sink = LiveOutboundSink::new(
        sink_config,
        Arc::new(registry),
        Arc::new(state_fetcher) as Arc<dyn LiveStateFetcher>,
        Arc::new(signer) as Arc<dyn LiveSigner>,
        Arc::new(submitter) as Arc<dyn LiveSubmitter>,
    );

    eprintln!("[pq-sell] LiveOutboundSink constructed — executing sell...");

    // The sink's on_admit overrides user with the signer's real pubkey (step 0),
    // so we pass zeros here — the sink replaces it before building.
    let record = AdmitRecord {
        mint: mint_arr,
        user: [0u8; 32],
        is_buy: false,       // SELL
        size_lamports: token_amount,
        entry_price: 0,
        max_slippage_bps: slippage_bps,
    };

    let outcome = live_sink.on_admit(&record);

    match &outcome {
        OutboundOutcome::Accepted { signature } => {
            let sig_b58 = encode_base58_64(signature);
            eprintln!("[pq-sell] *** SELL SUBMITTED SUCCESSFULLY ***");
            eprintln!("[pq-sell] Signature: {sig_b58}");
            println!("{sig_b58}");
            ExitCode::SUCCESS
        }
        OutboundOutcome::Construction(msg) => {
            eprintln!("[pq-sell] CONSTRUCTION FAILED: {msg}");
            ExitCode::from(10)
        }
        OutboundOutcome::StateFetch(msg) => {
            eprintln!("[pq-sell] STATE FETCH FAILED: {msg}");
            ExitCode::from(11)
        }
        OutboundOutcome::Signer(msg) => {
            eprintln!("[pq-sell] SIGNER FAILED: {msg}");
            ExitCode::from(12)
        }
        OutboundOutcome::Sender(msg) => {
            eprintln!("[pq-sell] SENDER FAILED: {msg}");
            ExitCode::from(13)
        }
        _ => {
            eprintln!("[pq-sell] UNKNOWN OUTCOME: {outcome:?}");
            ExitCode::from(14)
        }
    }
}

/// Encode a 64-byte ed25519 signature to a base58 string.
/// Uses the Bitcoin/Solana alphabet — the inverse of the daemon's
/// `decode_base58_64`.
fn encode_base58_64(sig: &[u8; 64]) -> String {
    const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    // Copy into a mutable buffer so we can consume it digit-by-digit.
    let mut buf = *sig;
    // Count leading zero bytes → leading '1' chars.
    let leading_zeros = buf.iter().take_while(|&&b| b == 0).count();
    // Build digits in big-endian base-58.
    let mut digits: Vec<u8> = Vec::with_capacity(64);
    for &byte in &buf {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            let v = (*d as u32) * 256 + carry;
            *d = (v % 58) as u8;
            carry = v / 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    // Digits are stored least-significant-first; reverse for output.
    let mut out = String::new();
    for _ in 0..leading_zeros {
        out.push('1');
    }
    for d in digits.iter().rev() {
        out.push(B58[*d as usize] as char);
    }
    out
}
