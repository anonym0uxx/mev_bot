//! manual_sell_DtSQeRmkG9 — Emergency sell for stuck DtSQeRmkG9 position.
//! Mint: DtSQeRmkG9Xj1QNbcQevhRG1okBHJaZ6gMcvtuoFpump (Token-2022)
//! ATA:  J3GqN1FPiiV1P5DQG1fTQ1f8oGjHpra4FutjAi7RzTSe
//! Pool: E86gLHnH2rDgmTC17p4npfZRx65TwAZJf1NyFjRQeDSy

use pump_quant_core::tx::pumpswap::{PumpSwapPoolAccounts, build_pumpswap_sell_tx_with_ata};
use solana_sdk::{
    pubkey::Pubkey, signature::Keypair, signer::Signer,
    instruction::{AccountMeta, Instruction}, system_instruction,
};
use std::str::FromStr;

fn b58b(s: &str) -> [u8; 32] {
    let v = bs58::decode(s).into_vec().unwrap();
    let mut a = [0u8; 32]; a.copy_from_slice(&v); a
}

async fn get_blockhash(client: &reqwest::Client, url: &str) -> [u8; 32] {
    let r: serde_json::Value = client.post(url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getLatestBlockhash","params":[{"commitment":"confirmed"}]}))
        .send().await.unwrap().json().await.unwrap();
    let s = r["result"]["value"]["blockhash"].as_str().unwrap();
    let v = bs58::decode(s).into_vec().unwrap();
    let mut a = [0u8; 32]; a.copy_from_slice(&v); a
}

async fn send_and_confirm(client: &reqwest::Client, url: &str, tx_b64: &str, label: &str) -> bool {
    let resp: serde_json::Value = client.post(url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendTransaction","params":[tx_b64,{"encoding":"base64","skipPreflight":true}]}))
        .send().await.unwrap().json().await.unwrap();

    let sig = match resp["result"].as_str() {
        Some(s) => { println!("{label} TX: {s}"); s.to_string() }
        None => { eprintln!("{label} send failed: {:?}", resp); return false; }
    };

    for i in 0..20 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let conf: serde_json::Value = client.post(url)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getSignatureStatuses","params":[[sig]]}))
            .send().await.unwrap().json().await.unwrap();
        let status = &conf["result"]["value"][0];
        if !status.is_null() {
            let err = &status["err"];
            if err.is_null() {
                println!("✅ {label} confirmed in {}s", (i+1)*2);
                return true;
            } else {
                eprintln!("❌ {label} FAILED: {:?}", err);
                return false;
            }
        }
    }
    eprintln!("{label} timed out");
    false
}

#[tokio::main]
async fn main() {
    let rpc_url = "https://marielle-qe2lvr-fast-mainnet.helius-rpc.com";
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().unwrap();

    let kp_path = "/data/.openclaw/workspace/projects/pump-quant/config/keys/wallet-keypair.json";
    let kp_bytes = std::fs::read(kp_path).unwrap();
    let kp_arr: Vec<u8> = serde_json::from_slice(&kp_bytes).unwrap();
    let mut kb = [0u8; 64]; kb.copy_from_slice(&kp_arr);
    let keypair = Keypair::from_bytes(&kb).unwrap();
    let wallet = keypair.pubkey();
    println!("Wallet: {wallet}");

    // ── Step 1: ensure WSOL ATA exists ───────────────────────────────────────
    let wsol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
    let spl_token = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    let ata_prog  = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();
    let sys_prog  = Pubkey::from_str("11111111111111111111111111111111").unwrap();

    let (wsol_ata, _) = Pubkey::find_program_address(
        &[wallet.as_ref(), spl_token.as_ref(), wsol_mint.as_ref()], &ata_prog);
    println!("WSOL ATA: {wsol_ata}");

    let exists: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getAccountInfo","params":[wsol_ata.to_string(),{"encoding":"base64"}]}))
        .send().await.unwrap().json().await.unwrap();

    if exists["result"]["value"].is_null() {
        println!("WSOL ATA missing — creating...");
        let create_ix = Instruction {
            program_id: ata_prog,
            accounts: vec![
                AccountMeta::new(wallet, true),
                AccountMeta::new(wsol_ata, false),
                AccountMeta::new_readonly(wallet, false),
                AccountMeta::new_readonly(wsol_mint, false),
                AccountMeta::new_readonly(sys_prog, false),
                AccountMeta::new_readonly(spl_token, false),
            ],
            data: vec![1u8], // CreateIdempotent
        };
        let bh = get_blockhash(&client, rpc_url).await;
        let bh_hash = solana_sdk::hash::Hash::new_from_array(bh);
        let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
            &[create_ix], Some(&wallet), &[&keypair], bh_hash,
        );
        let b64 = base64::encode(&bincode::serialize(&tx).unwrap());
        if !send_and_confirm(&client, rpc_url, &b64, "create-wsol-ata").await {
            eprintln!("Failed to create WSOL ATA — aborting");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    } else {
        println!("WSOL ATA exists ✅");
    }

    // ── Step 2: get token balance ─────────────────────────────────────────────
    let real_ata_str = "J3GqN1FPiiV1P5DQG1fTQ1f8oGjHpra4FutjAi7RzTSe";
    let bal: serde_json::Value = client.post(rpc_url)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getTokenAccountBalance","params":[real_ata_str]}))
        .send().await.unwrap().json().await.unwrap();
    let tokens: u64 = bal["result"]["value"]["amount"].as_str().unwrap().parse().unwrap();
    println!("Tokens: {tokens}");
    if tokens == 0 { println!("Already zero — done"); return; }

    // ── Step 3: build and submit sell ────────────────────────────────────────
    let pool = PumpSwapPoolAccounts {
        pool:                         b58b("E86gLHnH2rDgmTC17p4npfZRx65TwAZJf1NyFjRQeDSy"),
        base_mint:                    b58b("DtSQeRmkG9Xj1QNbcQevhRG1okBHJaZ6gMcvtuoFpump"),
        pool_base_token_account:      b58b("E8NHBEZZmVeJFzBYkN9MyzYLHYPS1nB9oRy9a5hTcFSX"),
        pool_quote_token_account:     b58b("GHkmV4EYdgZV8Engw6RpkLgcopk7LhHN7fBu6rdyK5gH"),
        coin_creator_vault_ata:       b58b("4Pa9Z4qfAiruY3tAWp8ieJqEdVHDtVigSYQkXhkVcKdG"),
        coin_creator_vault_authority: b58b("11111111111111111111111111111111"),
        token_is_base: true,
        token_mint_program:           b58b("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
        is_cashback_coin: false,
    };

    let tip_acct = Pubkey::from_str("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5").unwrap();
    let real_ata = Pubkey::from_str(real_ata_str).unwrap();

    let bh = get_blockhash(&client, rpc_url).await;
    let tx_bytes = build_pumpswap_sell_tx_with_ata(
        &pool, &keypair, tokens, 0u64, 500_000u64, tip_acct, bh, 0, real_ata,
    ).expect("build sell TX");

    println!("Sell TX built ({} bytes)", tx_bytes.len());
    let b64 = base64::encode(&tx_bytes);
    send_and_confirm(&client, rpc_url, &b64, "sell").await;
}
