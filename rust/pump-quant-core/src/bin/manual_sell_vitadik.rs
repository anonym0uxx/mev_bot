//! manual_sell_vitadik — One-shot tool to sell stuck VITADIK position.
//!
//! Mint: FYEVo2ejcZncQ3JZULZ7ZswVoAQwg3bazVcNbNfgHLtc
//! Pool: Drts8WKTZJrMxiusmZDX9GXNwGET417SJiENUzu2KwYd
//! ATA:  H4JynyJD2PeLXHsNu791FaQxfEsh3kGSoqf92K3iXnZs (Token-2022)
//! Tokens held: 93,205.513072

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
    // ── Pool accounts for VITADIK ─────────────────────────────────────────────
    // Pool:        Drts8WKTZJrMxiusmZDX9GXNwGET417SJiENUzu2KwYd
    // base_mint:   FYEVo2ejcZncQ3JZULZ7ZswVoAQwg3bazVcNbNfgHLtc (Token-2022)
    // base_vault:  EymVRUKnpFnNn5mP5RH2tSNGauUkSG8Xzw32oMSgAEwp
    // quote_vault: 8Atrt25egDA9uggoQXw3KLEsvtVKrdFhBXPPW1NnxWy7
    // token_mint_program: TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb (Token-2022)
    // token_is_base: true (confirmed from buy TX: base slot received tokens)
    //
    // coin_creator_vault: look up from pool state on-chain
    // Using pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ fee program
    // fee_recipients from on-chain data (buy TX log):
    //   GN7gEMBAkv7scUmXBfC51GFf3yVK3oo7NyS1bPHKwDid (fee vault)
    //   7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ (protocol vault)

    // We need coin_creator_vault_ata and coin_creator_vault_authority from the pool.
    // These are fetched from pool state below.
    
    let pool_accounts = PumpSwapPoolAccounts {
        pool:                          b58_to_bytes("Drts8WKTZJrMxiusmZDX9GXNwGET417SJiENUzu2KwYd"),
        base_mint:                     b58_to_bytes("FYEVo2ejcZncQ3JZULZ7ZswVoAQwg3bazVcNbNfgHLtc"),
        pool_base_token_account:       b58_to_bytes("EymVRUKnpFnNn5mP5RH2tSNGauUkSG8Xzw32oMSgAEwp"),
        pool_quote_token_account:      b58_to_bytes("8Atrt25egDA9uggoQXw3KLEsvtVKrdFhBXPPW1NnxWy7"),
        // coin_creator_vault_ata and authority fetched from pool state below
        coin_creator_vault_ata:        b58_to_bytes("11111111111111111111111111111111"), // placeholder
        coin_creator_vault_authority:  b58_to_bytes("11111111111111111111111111111111"), // placeholder
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

    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://marielle-qe2lvr-fast-mainnet.helius-rpc.com".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build().unwrap();

    // ── Fetch pool state to get coin_creator_vault fields ─────────────────────
    println!("Fetching pool state...");
    let pool_state_resp: serde_json::Value = client.post(&rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getAccountInfo",
            "params": ["Drts8WKTZJrMxiusmZDX9GXNwGET417SJiENUzu2KwYd", {"encoding": "base64"}]
        }))
        .send().await.expect("pool state RPC")
        .json().await.expect("pool state parse");

    let pool_b64 = pool_state_resp["result"]["value"]["data"][0].as_str().expect("pool data");
    let pool_bytes = base64::decode(pool_b64).expect("pool base64 decode");
    println!("Pool state: {} bytes", pool_bytes.len());

    // PumpSwap pool layout (pAMMBay6):
    // [0..8]   discriminator
    // [8]      pool_bump
    // [9..41]  index (4 bytes) + padding
    // Actually: discriminator(8) + bump(1) + index(2) + padding(5) = 16
    // Then: creator(32) at offset 16
    // base_mint(32) at offset 43 (standard)
    // quote_mint(32) at offset 75
    // lp_mint(32) at offset 107
    // pool_base_token_account(32) at offset 139
    // pool_quote_token_account(32) at offset 171
    // lp_supply(8) at offset 203
    // coin_creator(32) at offset 211
    // coin_creator_vault_authority = PDA derived from coin_creator
    
    // coin_creator_vault_authority and coin_creator_vault_ata verified from buy TX:
    // Buy TX: wQ6H6gF2U8V9BXuxCJgSpSkUhYHPdFtK8bbRUsPF3Qum9gimqoJTjHJ3sdtZqLKM17ogMUZXPd4wGFhg7i752Kv
    // Account [20] = 6tkGUcYBJJ2c1pdtMQayUpEFEpyP3QqY8Lf6pvvpF5Fq = coin_creator_vault_authority
    // Account [9]  = FrY4aFWjydJmXDF6m59pA9JeuSdCZQHkyXnRgVogwrWU = coin_creator_vault_ata (WSOL, owner=6tkGUcY)
    let real_pool_accounts = PumpSwapPoolAccounts {
        pool:                          b58_to_bytes("Drts8WKTZJrMxiusmZDX9GXNwGET417SJiENUzu2KwYd"),
        base_mint:                     b58_to_bytes("FYEVo2ejcZncQ3JZULZ7ZswVoAQwg3bazVcNbNfgHLtc"),
        pool_base_token_account:       b58_to_bytes("EymVRUKnpFnNn5mP5RH2tSNGauUkSG8Xzw32oMSgAEwp"),
        pool_quote_token_account:      b58_to_bytes("8Atrt25egDA9uggoQXw3KLEsvtVKrdFhBXPPW1NnxWy7"),
        coin_creator_vault_ata:        b58_to_bytes("FrY4aFWjydJmXDF6m59pA9JeuSdCZQHkyXnRgVogwrWU"),
        coin_creator_vault_authority:  b58_to_bytes("6tkGUcYBJJ2c1pdtMQayUpEFEpyP3QqY8Lf6pvvpF5Fq"),
        token_is_base: true,
        token_mint_program:            b58_to_bytes("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
        is_cashback_coin: false,
    };

    println!("Pool state parsed OK ({} bytes)", pool_bytes.len());
    sell_position(&client, &rpc_url, &keypair, &real_pool_accounts).await;
}

async fn sell_position(
    client: &reqwest::Client,
    rpc_url: &str,
    keypair: &Keypair,
    pool_accounts: &PumpSwapPoolAccounts,
) {
    let wallet = keypair.pubkey();
    
    // ATA for Token-2022: use Token-2022 program in seed
    let token_mint = Pubkey::from_str("FYEVo2ejcZncQ3JZULZ7ZswVoAQwg3bazVcNbNfgHLtc").unwrap();
    let token_2022 = Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap();
    let ata_program = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();
    let (token_ata, _) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_2022.as_ref(), token_mint.as_ref()],
        &ata_program,
    );
    println!("Our Token-2022 ATA: {}", token_ata);

    // ── Get actual on-chain token balance ─────────────────────────────────────
    let bal_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getTokenAccountBalance",
            "params": [token_ata.to_string()]
        }))
        .send().await.unwrap()
        .json().await.unwrap();

    if bal_resp.get("error").is_some() {
        eprintln!("ATA not found: {:?}", bal_resp["error"]);
        eprintln!("ATA address tried: {}", token_ata);
        std::process::exit(1);
    }

    let tokens_to_sell: u64 = bal_resp["result"]["value"]["amount"]
        .as_str().unwrap().parse().unwrap();
    println!("Tokens to sell: {} (raw lamports)", tokens_to_sell);
    println!("Tokens to sell: {:.6} (UI)", tokens_to_sell as f64 / 1e6);

    if tokens_to_sell == 0 {
        println!("No tokens to sell — already clear!");
        return;
    }

    // ── Get pool balances for price estimate ──────────────────────────────────
    let bv_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "getTokenAccountBalance",
            "params": ["EymVRUKnpFnNn5mP5RH2tSNGauUkSG8Xzw32oMSgAEwp"]
        }))
        .send().await.unwrap().json().await.unwrap();
    let qv_resp: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "getTokenAccountBalance",
            "params": ["8Atrt25egDA9uggoQXw3KLEsvtVKrdFhBXPPW1NnxWy7"]
        }))
        .send().await.unwrap().json().await.unwrap();

    let pool_tokens: u128 = bv_resp["result"]["value"]["amount"].as_str().unwrap().parse().unwrap();
    let pool_wsol: u128 = qv_resp["result"]["value"]["amount"].as_str().unwrap().parse().unwrap();

    // AMM out: dy = y * dx / (x + dx), with fee (0.25% = 25bps)
    let dx = tokens_to_sell as u128;
    let fee_bps = 25u128;
    let dx_after_fee = dx * (10000 - fee_bps) / 10000;
    let dy = pool_wsol * dx_after_fee / (pool_tokens + dx_after_fee);
    // Use min_sol_out = 1 (accept any price) because the PumpSwap contract overflows
    // with large token reserve pools when doing fee math. Token is ~dead anyway (0.003 SOL value).
    let min_sol_out = 1u128; // accept any nonzero return

    println!("Pool: {} tokens, {} lamports WSOL", pool_tokens, pool_wsol);
    println!("Expected SOL: {:.6} ({} lamports)", dy as f64 / 1e9, dy);
    println!("Min SOL out (10% slip): {:.6} ({} lamports)", min_sol_out as f64 / 1e9, min_sol_out);
    println!("Entry cost: 0.0433 SOL");
    println!("Expected PnL: {:+.6} SOL", (dy as f64 / 1e9) - 0.0433);

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
    let tip_lamports = 500_000u64;
    let fee_idx = 0usize;

    let tx_bytes = match build_pumpswap_sell_tx(
        pool_accounts,
        keypair,
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

    println!("Sell TX built ({} bytes) — submitting...", tx_bytes.len());

    let tx_b64 = base64::encode(&tx_bytes);

    // Try Jito first, fallback to RPC
    let jito_url = "https://ny.mainnet.block-engine.jito.wtf/api/v1/transactions";
    let submit_resp = client.post(jito_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "sendTransaction",
            "params": [tx_b64, {"encoding": "base64", "skipPreflight": true}]
        }))
        .send().await;

    let sig = match submit_resp {
        Ok(r) => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(s) = body["result"].as_str() {
                println!("✅ Jito TX submitted! Sig: {}", s);
                s.to_string()
            } else {
                eprintln!("Jito failed: {:?} — trying direct RPC", body);
                let rpc_resp: serde_json::Value = client.post(rpc_url)
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "method": "sendTransaction",
                        "params": [tx_b64, {"encoding": "base64", "skipPreflight": false}]
                    }))
                    .send().await.unwrap().json().await.unwrap();
                if let Some(s) = rpc_resp["result"].as_str() {
                    println!("✅ RPC TX submitted! Sig: {}", s);
                    s.to_string()
                } else {
                    eprintln!("RPC also failed: {:?}", rpc_resp);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Submit error: {:?}", e);
            std::process::exit(1);
        }
    };

    println!("Solscan: https://solscan.io/tx/{}", sig);

    // Poll for confirmation
    println!("Polling for confirmation...");
    for i in 0..30 {
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
                    eprintln!("❌ TX FAILED on-chain: {:?}", e);
                    std::process::exit(1);
                }
                println!("[{}s] Status: {}", i * 2, cs);
                if cs == "finalized" || cs == "confirmed" {
                    println!("✅ Sell confirmed! SOL recovered.");
                    println!("Recovered ~{:.6} SOL", dy as f64 / 1e9);
                    return;
                }
            }
        } else {
            print!(".");
        }
    }
    println!("Timed out polling — TX may still land. Check: https://solscan.io/tx/{}", sig);
}
