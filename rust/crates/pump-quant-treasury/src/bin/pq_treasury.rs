#!/usr/bin/env false
//! pq_treasury: CLI for the treasury module.
//!
//! Usage:
//!   pq_treasury transfer <destination> <lamports> "<purpose>"
//!   pq_treasury balance
//!   pq_treasury policy-check
//!
//! Environment variables (set by the daemon or launch script):
//!   PQ_KEYPAIR_PATH      — path to the Solana CLI keypair JSON
//!   PQ_WALLET_ADDRESS    — the expected base58 wallet address
//!   PQ_TREASURY_POLICY   — path to the treasury policy TOML
//!   PQ_TREASURY_AUDIT    — path to the audit log JSONL
//!   PQ_RPC_URL           — Helius RPC URL (with API key)
//!
//! This binary is run BY ALON, not by the agent. The agent does not have
//! access to PQ_KEYPAIR_PATH or the keypair file.

use std::path::PathBuf;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        std::process::exit(1);
    }

    let keypair_path = env::var("PQ_KEYPAIR_PATH").expect("PQ_KEYPAIR_PATH not set");
    let wallet_address = env::var("PQ_WALLET_ADDRESS").expect("PQ_WALLET_ADDRESS not set");
    let policy_path = env::var("PQ_TREASURY_POLICY").expect("PQ_TREASURY_POLICY not set");
    let audit_path = env::var("PQ_TREASURY_AUDIT").expect("PQ_TREASURY_AUDIT not set");
    let rpc_url = env::var("PQ_RPC_URL").expect("PQ_RPC_URL not set");

    let treasury = pump_quant_treasury::Treasury::load(
        &PathBuf::from(&keypair_path),
        &wallet_address,
        &PathBuf::from(&policy_path),
        &PathBuf::from(&audit_path),
    ).unwrap_or_else(|e| {
        eprintln!("FATAL: failed to load treasury: {e}");
        std::process::exit(1);
    });

    let rpc = pump_quant_treasury::HeliusRpc::new(&rpc_url);

    match args[1].as_str() {
        "transfer" => {
            if args.len() < 5 {
                eprintln!("Usage: pq_treasury transfer <destination> <lamports> \"<purpose>\"");
                std::process::exit(1);
            }
            let destination = &args[2];
            let lamports: u64 = args[3].parse().unwrap_or_else(|_| {
                eprintln!("Invalid lamports value: {}", args[3]);
                std::process::exit(1);
            });
            let purpose = &args[4];

            println!("Wallet: {}", treasury.wallet_address());
            println!("Destination: {destination}");
            println!("Amount: {lamports} lamports ({} SOL)", lamports as f64 / 1e9);
            println!("Purpose: {purpose}");
            println!();

            // Codeword gate: if the policy has a codeword configured, prompt
            // for it on stdin (never as a CLI arg — would be visible in
            // process lists and shell history).
            let codeword = if treasury.policy().has_codeword() {
                println!("Codeword required by policy. Enter codeword: ");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).unwrap_or_else(|e| {
                    eprintln!("Failed to read codeword from stdin: {e}");
                    std::process::exit(1);
                });
                let cw = input.trim();
                if cw.is_empty() {
                    eprintln!("Codeword is required but was empty.");
                    std::process::exit(2);
                }
                Some(cw.to_string())
            } else {
                None
            };

            let outcome = treasury.request_transfer(
                destination,
                lamports,
                purpose,
                &rpc,
                codeword.as_deref(),
            );
            match &outcome {
                pump_quant_treasury::TransferOutcome::Confirmed { tx_signature, .. } => {
                    println!("✅ CONFIRMED — tx signature: {tx_signature}");
                }
                pump_quant_treasury::TransferOutcome::TimeLocked { reason, .. } => {
                    println!("⏳ TIME-LOCKED — {reason}");
                    println!("   Confirm after the time-lock expires with:");
                    println!("   pq_treasury confirm <queue_id>");
                }
                pump_quant_treasury::TransferOutcome::Rejected { reason, .. } => {
                    println!("❌ REJECTED — {reason}");
                    std::process::exit(2);
                }
                pump_quant_treasury::TransferOutcome::Failed { reason, .. } => {
                    println!("💥 FAILED — {reason}");
                    std::process::exit(3);
                }
            }
        }

        "balance" => {
            let addr = treasury.wallet_address();
            match rpc.get_balance(addr) {
                Ok(lamports) => {
                    println!("Wallet: {addr}");
                    println!("Balance: {lamports} lamports ({} SOL)", lamports as f64 / 1e9);
                }
                Err(e) => {
                    eprintln!("Failed to get balance: {e}");
                    std::process::exit(1);
                }
            }
        }

        "policy-check" => {
            println!("Wallet: {}", treasury.wallet_address());
            println!("Policy loaded successfully.");
            println!("  Auto-max: {} lamports", treasury.policy().limits.auto_max_lamports);
            println!("  Approval threshold: {} lamports", treasury.policy().limits.approval_threshold_lamports);
            println!("  Time-lock: {}s", treasury.policy().limits.time_lock_seconds);
            println!("  Daily cap: {} lamports", treasury.policy().limits.daily_cap_lamports);
            println!("  Codeword gate: {}", if treasury.policy().has_codeword() { "ENABLED" } else { "disabled" });
            println!("  Whitelisted addresses:");
            for entry in &treasury.policy().whitelist {
                println!("    {} ({})", entry.address, entry.label);
                println!("      per-tx: {} lamports, daily: {} lamports",
                         entry.max_per_tx_lamports, entry.max_daily_lamports);
            }
        }

        _ => {
            usage();
            std::process::exit(1);
        }
    }
}

fn usage() {
    eprintln!("pq_treasury — policy-gated SOL transfers");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  pq_treasury transfer <destination> <lamports> \"<purpose>\"");
    eprintln!("  pq_treasury balance");
    eprintln!("  pq_treasury policy-check");
    eprintln!();
    eprintln!("Environment:");
    eprintln!("  PQ_KEYPAIR_PATH    — path to Solana CLI keypair JSON");
    eprintln!("  PQ_WALLET_ADDRESS  — expected base58 wallet address");
    eprintln!("  PQ_TREASURY_POLICY — path to treasury policy TOML");
    eprintln!("  PQ_TREASURY_AUDIT  — path to audit log JSONL");
    eprintln!("  PQ_RPC_URL         — Helius RPC URL");
}
