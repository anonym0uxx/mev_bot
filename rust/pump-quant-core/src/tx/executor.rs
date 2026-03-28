//! Transaction executor: builds and submits buy/sell transactions via Jito,
//! or simulates them in paper mode.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use solana_sdk::hash::Hash;

use crate::engine::positions::ClosedPosition;
use super::builder::{BuyTxRequest, SellTxRequest, TxBuilder};
use super::jito::JitoClient;
use super::wallet::WalletManager;

// ── Config ───────────────────────────────────────────────────────────────────

pub struct ExecutorConfig {
    /// If true, skip building/submitting and return zeroed signatures.
    pub paper_mode: bool,
    /// Jito tip per bundle (lamports).
    pub jito_tip_lamports: u64,
    /// Priority fee in microlamports per CU.
    pub priority_fee_lamports: u64,
    /// Slippage tolerance in basis points (e.g. 300 = 3%).
    pub slippage_bps: u32,
    /// Solana RPC URL for blockhash fetching.
    pub rpc_url: String,
}

// ── Executor ─────────────────────────────────────────────────────────────────

pub struct TxExecutor {
    config: ExecutorConfig,
    wallet: Arc<WalletManager>,
    jito: Arc<JitoClient>,
    rpc: reqwest::Client,
}

/// JSON-RPC response structures for `getLatestBlockhash`.
#[derive(Deserialize)]
struct RpcResponse {
    result: Option<RpcResultValue>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RpcResultValue {
    value: BlockhashValue,
}

#[derive(Deserialize)]
struct BlockhashValue {
    blockhash: String,
}

impl TxExecutor {
    pub fn new(
        config: ExecutorConfig,
        wallet: Arc<WalletManager>,
        jito: Arc<JitoClient>,
    ) -> Self {
        let rpc = reqwest::Client::new();
        Self {
            config,
            wallet,
            jito,
            rpc,
        }
    }

    /// Execute a buy transaction.
    ///
    /// In paper mode: returns a zeroed `[u8; 64]` signature immediately.
    /// In live mode: fetches blockhash, builds the buy tx, submits via Jito.
    pub async fn execute_buy(
        &self,
        mint: [u8; 32],
        bonding_curve: [u8; 32],
        assoc_bonding_curve: [u8; 32],
        vsol_lamports: u64,
        vtokens: u64,
        size_lamports: u64,
    ) -> Result<[u8; 64]> {
        if self.config.paper_mode {
            tracing::debug!("paper mode: simulated buy for mint {}", bs58::encode(&mint).into_string());
            return Ok([0u8; 64]);
        }

        // Fetch latest blockhash
        let blockhash = self.fetch_latest_blockhash().await?;

        // Build the buy transaction
        let keypair = self.wallet.current_keypair();
        // We need to clone the keypair bytes to create a new Keypair for TxBuilder
        let builder = TxBuilder::new(
            solana_sdk::signature::Keypair::from_bytes(&keypair.to_bytes())
                .context("failed to clone keypair for TxBuilder")?,
        );

        let req = BuyTxRequest {
            mint,
            bonding_curve,
            assoc_bonding_curve,
            vsol_lamports,
            vtokens,
            size_lamports,
            slippage_bps: self.config.slippage_bps,
            priority_fee_microlamports: self.config.priority_fee_lamports,
            jito_tip_lamports: self.config.jito_tip_lamports,
            recent_blockhash: blockhash.to_bytes(),
        };

        let tx = builder.build_buy_tx(&req).context("failed to build buy tx")?;

        // Submit via Jito
        let bundle_id = self.jito.submit_bundle(&tx).await
            .context("failed to submit buy bundle to Jito")?;

        tracing::info!(
            "buy bundle submitted: {}, mint: {}",
            bundle_id,
            bs58::encode(&mint).into_string()
        );

        // Return the transaction signature
        Ok(tx.signatures[0].into())
    }

    /// Execute a sell transaction for a closed position.
    ///
    /// In paper mode: returns a zeroed `[u8; 64]` signature immediately.
    /// In live mode: fetches blockhash, builds the sell tx, submits via Jito.
    pub async fn execute_sell(
        &self,
        pos: &ClosedPosition,
        vtokens_current: u64,
    ) -> Result<[u8; 64]> {
        if self.config.paper_mode {
            tracing::debug!(
                "paper mode: simulated sell for mint {}",
                bs58::encode(&pos.mint).into_string()
            );
            return Ok([0u8; 64]);
        }

        // Fetch latest blockhash
        let blockhash = self.fetch_latest_blockhash().await?;

        // Build the sell transaction
        let keypair = self.wallet.current_keypair();
        let builder = TxBuilder::new(
            solana_sdk::signature::Keypair::from_bytes(&keypair.to_bytes())
                .context("failed to clone keypair for TxBuilder")?,
        );

        // Calculate minimum SOL out with slippage
        // Simulate what we'd get from selling, then apply slippage
        let sell_sim = crate::engine::bonding_curve::simulate_sell(
            pos.current_vsol,
            vtokens_current,
            pos.tokens_held,
        );
        let min_sol_out = if self.config.slippage_bps >= 10_000 {
            0
        } else {
            (sell_sim.sol_out as u128 * (10_000 - self.config.slippage_bps) as u128 / 10_000) as u64
        };

        let req = SellTxRequest {
            mint: pos.mint,
            bonding_curve: pos.bonding_curve,
            assoc_bonding_curve: pos.assoc_bonding_curve,
            tokens_to_sell: pos.tokens_held,
            vsol_lamports: pos.current_vsol,
            vtokens: vtokens_current,
            min_sol_out_lamports: min_sol_out,
            priority_fee_microlamports: self.config.priority_fee_lamports,
            jito_tip_lamports: self.config.jito_tip_lamports,
            recent_blockhash: blockhash.to_bytes(),
        };

        let tx = builder.build_sell_tx(&req).context("failed to build sell tx")?;

        // Submit via Jito
        let bundle_id = self.jito.submit_bundle(&tx).await
            .context("failed to submit sell bundle to Jito")?;

        tracing::info!(
            "sell bundle submitted: {}, mint: {}",
            bundle_id,
            bs58::encode(&pos.mint).into_string()
        );

        // Return the transaction signature
        Ok(tx.signatures[0].into())
    }

    /// Fetch the latest blockhash from the Solana RPC.
    async fn fetch_latest_blockhash(&self) -> Result<Hash> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": [{"commitment": "finalized"}]
        });

        let resp = self
            .rpc
            .post(&self.config.rpc_url)
            .json(&body)
            .send()
            .await
            .context("failed to send getLatestBlockhash RPC request")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("RPC returned HTTP {status}: {text}");
        }

        let parsed: RpcResponse = resp
            .json()
            .await
            .context("failed to parse getLatestBlockhash response")?;

        if let Some(err) = parsed.error {
            bail!("RPC error in getLatestBlockhash: {err}");
        }

        let blockhash_str = parsed
            .result
            .context("getLatestBlockhash response missing 'result'")?
            .value
            .blockhash;

        // Decode base58 blockhash string to Hash
        let hash_bytes = bs58::decode(&blockhash_str)
            .into_vec()
            .context("blockhash is not valid base58")?;

        if hash_bytes.len() != 32 {
            bail!(
                "blockhash decoded to {} bytes, expected 32",
                hash_bytes.len()
            );
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash_bytes);
        Ok(Hash::new_from_array(arr))
    }
}
