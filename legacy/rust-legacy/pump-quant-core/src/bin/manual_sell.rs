//! manual_sell — One-shot tool to sell stuck PumpSwap position.
//!
//! Usage: WALLET_KEYPAIR_PATH=... ./manual-sell
//!
//! Hardcoded for the stuck 58WSMR position. Remove after use.

use pump_quant_core::tx::pumpswap::{PumpSwapPoolAccounts, build_pumpswap_sell_tx};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use std::str::FromStr;

fn b58_to_bytes(s: &str) -> [u8; 32] {
    let bytes = bs58::decode(s).into_vec().expect("invalid base58");
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    arr
}

#[tokio::main]
async fn main() {
    // ── Pool accounts for 58WSMR ──────────────────────────────────────────────
    // Pool: 4qAX2HgJxFAbbtawb3knNZZSr7BVsT8tdAe1bUhaLRSu
    // base_mint (token) at offset 43: 58WSMRURYYN4DYknoGm4TzWiFrbo8EEJHD9cN5C1pump
    // quote_mint (WSOL)  at offset 75: So11111111111111111111111111111111111111112
    // base_vault (token) at offset 139: 6MW8R2tnvQ5McBBUmceK4cKy2bwrp5QMMtGiRvmUmMBc
    // quote_vault (WSOL) at offset 171: 4HYsDsWr5B5CN1FrSCJkkdwYDPLhGqnoNfQUH9gBruXQ
    // token_mint_program: Token-2022 (TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb)
    // token_is_base: true

    // coin_creator_vault accounts verified from on-chain sell TX:
    // 3PgwvdMyPBtbL5qBq2wSm6UpPsFo35bXZzzFqYs2H5zN5CdfkyYeyusbbjFkqLbkAG7UtRz2wBFNQngXRv1Q3Zea
    // [17] coin_creator_vault_ata = Evh7VzspLYQLjqHPhywBmGrJuhqxtpYUFCcqXZTesEct
    // [18] coin_creator_vault_authority = Asgd37waV834yEtFznKEc9WktEHVBsEaD4QZnzXXMrjg
    // pool creator (offset 11): HQwt5P3QnyAVba4ZasD9SKUEEJr8tYCkiB6JYkfhoSEn
    let pool_accounts = PumpSwapPoolAccounts {
        pool:                          b58_to_bytes("4qAX2HgJxFAbbtawb3knNZZSr7BVsT8tdAe1bUhaLRSu"),
        base_mint:                     b58_to_bytes("58WSMRURYYN4DYknoGm4TzWiFrbo8EEJHD9cN5C1pump"),
        pool_base_token_account:       b58_to_bytes("6MW8R2tnvQ5McBBUmceK4cKy2bwrp5QMMtGiRvmUmMBc"),
        pool_quote_token_account:      b58_to_bytes("4HYsDsWr5B5CN1FrSCJkkdwYDPLhGqnoNfQUH9gBruXQ"),
        coin_creator_vault_ata:        b58_to_bytes("Evh7VzspLYQLjqHPhywBmGrJuhqxtpYUFCcqXZTesEct"),
        coin_creator_vault_authority:  b58_to_bytes("Asgd37waV834yEtFznKEc9WktEHVBsEaD4QZnzXXMrjg"),
        token_is_base: true,
        token_mint_program:            b58_to_bytes("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
        is_cashback_coin: false,
    };

    // ── Load keypair ──────────────────────────────────────────────────────────
    let kp_path = std::env::var("WALLET_KEYPAIR_PATH")
        .unwrap_or_else(|_| "/data/.openclaw/workspace/projects/pump-quant/config/keys/wallet-keypair.json".to_string());
    let kp_bytes = std::fs::read(&kp_path).expect("read keypair");
    let kp_arr: Vec<u8> = serde_json::from_slice(&kp_bytes).expect("parse keypair");
    let mut kb = [0u8; 64];
    kb.copy_from_slice(&kp_arr);
    let keypair = Keypair::from_bytes(&kb).expect("keypair from bytes");
    let wallet = keypair.pubkey();
    println!("Wallet: {}", wallet);

    // ── RPC setup ─────────────────────────────────────────────────────────────
    let rpc_url = "https://marielle-qe2lvr-fast-mainnet.helius-rpc.com";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build().unwrap();

    // ── Get actual on-chain token balance ─────────────────────────────────────
    let ata_str = "2yoddfgrTzNqsUnS3YWrY7QyutEchkMTsxwN4NrSG39A";
    let bal_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getTokenAccountBalance",
            "params": [ata_str]
        }))
        .send().await.unwrap()
        .json().await.unwrap();
    
    if bal_resp.get("error").is_some() {
        eprintln!("ATA not found: {:?}", bal_resp["error"]);
        std::process::exit(1);
    }
    
    let tokens_to_sell: u64 = bal_resp["result"]["value"]["amount"]
        .as_str().unwrap().parse().unwrap();
    println!("Tokens to sell: {} (raw)", tokens_to_sell);
    
    if tokens_to_sell == 0 {
        println!("No tokens to sell — already clear!");
        return;
    }

    // ── Get pool balances for price estimate ──────────────────────────────────
    let base_vault_str = "6MW8R2tnvQ5McBBUmceK4cKy2bwrp5QMMtGiRvmUmMBc";
    let quote_vault_str = "4HYsDsWr5B5CN1FrSCJkkdwYDPLhGqnoNfQUH9gBruXQ";
    
    let bv_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getTokenAccountBalance",
            "params": [base_vault_str]
        }))
        .send().await.unwrap().json().await.unwrap();
    let qv_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "getTokenAccountBalance",
            "params": [quote_vault_str]
        }))
        .send().await.unwrap().json().await.unwrap();
    
    let pool_tokens: u128 = bv_resp["result"]["value"]["amount"].as_str().unwrap().parse().unwrap();
    let pool_wsol: u128 = qv_resp["result"]["value"]["amount"].as_str().unwrap().parse().unwrap();
    
    // AMM out: dy = y * dx / (x + dx)
    let dx = tokens_to_sell as u128;
    let dy = pool_wsol * dx / (pool_tokens + dx);
    let min_sol_out = dy * 95 / 100; // 5% slippage
    
    println!("Pool: {} tokens, {} lamports WSOL", pool_tokens, pool_wsol);
    println!("Expected SOL: {:.6} ({} lamports)", dy as f64 / 1e9, dy);
    println!("Min SOL out (5% slip): {:.6} ({} lamports)", min_sol_out as f64 / 1e9, min_sol_out);
    println!("Entry cost: 0.03 SOL");
    println!("PnL: {:+.6} SOL", (dy as f64 / 1e9) - 0.03);

    // ── Get recent blockhash ──────────────────────────────────────────────────
    let bh_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "confirmed"}]
        }))
        .send().await.unwrap().json().await.unwrap();
    
    let bh_b58 = bh_resp["result"]["value"]["blockhash"].as_str().unwrap();
    let bh_bytes: Vec<u8> = bs58::decode(bh_b58).into_vec().unwrap();
    let mut bh = [0u8; 32];
    bh.copy_from_slice(&bh_bytes);
    println!("Blockhash: {}", bh_b58);

    // ── Build sell TX ─────────────────────────────────────────────────────────
    let jito_tip_account = Pubkey::from_str("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5").unwrap();
    let tip_lamports = 500_000u64; // 0.0005 SOL
    let fee_idx = 0usize;

    let tx_bytes = match build_pumpswap_sell_tx(
        &pool_accounts,
        &keypair,
        tokens_to_sell,
        min_sol_out as u64,
        tip_lamports,
        jito_tip_account,
        bh,
        fee_idx,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to build sell TX: {:?}", e);
            std::process::exit(1);
        }
    };

    println!("Sell TX built ({} bytes)", tx_bytes.len());

    // ── Submit via Jito ───────────────────────────────────────────────────────
    let tx_b64 = base64::encode(&tx_bytes);
    
    // Try Jito block engine first
    let jito_url = "https://ny.mainnet.block-engine.jito.wtf/api/v1/transactions";
    let submit_resp = client.post(jito_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendTransaction",
            "params": [tx_b64, {"encoding": "base64", "skipPreflight": true}]
        }))
        .send().await;

    match submit_resp {
        Ok(r) => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(sig) = body["result"].as_str() {
                println!("✅ TX submitted! Signature: {}", sig);
                println!("Solscan: https://solscan.io/tx/{}", sig);
                
                // Poll for confirmation
                println!("Polling for confirmation...");
                for _ in 0..20 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let conf: serde_json::Value = client.post(rpc_url)
                        .json(&serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "method": "getSignatureStatuses",
                            "params": [[sig]]
                        }))
                        .send().await.unwrap().json().await.unwrap();
                    
                    if let Some(status) = conf["result"]["value"][0].as_object() {
                        if status.contains_key("confirmationStatus") {
                            let cs = status["confirmationStatus"].as_str().unwrap_or("?");
                            let err = status.get("err").and_then(|e| if e.is_null() { None } else { Some(e) });
                            if let Some(e) = err {
                                println!("❌ TX FAILED: {:?}", e);
                                break;
                            }
                            println!("Status: {} — confirmed!", cs);
                            if cs == "finalized" || cs == "confirmed" {
                                println!("✅ Sell confirmed! SOL recovered.");
                                break;
                            }
                        }
                    }
                }
            } else {
                eprintln!("Jito response: {:?}", body);
                // Fallback to regular RPC
                let rpc_resp: serde_json::Value = client.post(rpc_url)
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "method": "sendTransaction",
                        "params": [tx_b64, {"encoding": "base64", "skipPreflight": false}]
                    }))
                    .send().await.unwrap().json().await.unwrap();
                println!("RPC response: {:?}", rpc_resp);
            }
        }
        Err(e) => eprintln!("Submit error: {:?}", e),
    }
}
