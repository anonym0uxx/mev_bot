use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use solana_sdk::{signature::Keypair, transaction::VersionedTransaction};

use super::builder::TxBuilder;

// ── Config ───────────────────────────────────────────────────────────────────

pub struct JitoConfig {
    /// Jito block engine URL, e.g. "https://frankfurt.mainnet.block-engine.jito.wtf"
    pub block_engine_url: String,
    /// Auth keypair — used to sign the x-jito-auth header (reserved for future gRPC auth).
    pub auth_keypair: Keypair,
    /// Maximum transactions per bundle (Jito hard limit = 5).
    pub max_bundle_size: usize,
    /// HTTP timeout in milliseconds.
    pub timeout_ms: u64,
}

// ── Client ───────────────────────────────────────────────────────────────────

pub struct JitoClient {
    config: JitoConfig,
    http: HttpClient,
}

#[derive(Serialize)]
struct BundleRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: Vec<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct BundleResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

impl JitoClient {
    pub async fn new(config: JitoConfig) -> Result<Self> {
        let http = HttpClient::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .context("failed to build reqwest client for Jito")?;

        Ok(Self { config, http })
    }

    /// Submit a single-transaction bundle via Jito REST JSON-RPC.
    ///
    /// Serializes the tx to base64 and POSTs to `{block_engine_url}/api/v1/bundles`.
    /// Returns the bundle ID on success.
    pub async fn submit_bundle(&self, tx: &VersionedTransaction) -> Result<String> {
        let tx_base64 = TxBuilder::serialize_tx(tx)?;
        self.submit_bundle_rest(&tx_base64).await
    }

    /// Submit a pre-serialized base64 transaction as a single-element bundle.
    ///
    /// POST to `{url}/api/v1/bundles` with:
    /// ```json
    /// {"jsonrpc":"2.0","id":1,"method":"sendBundle","params":[[base64_tx]]}
    /// ```
    pub async fn submit_bundle_rest(&self, tx_base64: &str) -> Result<String> {
        let url = format!(
            "{}/api/v1/bundles",
            self.config.block_engine_url.trim_end_matches('/')
        );

        let body = BundleRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "sendBundle",
            params: vec![vec![tx_base64.to_string()]],
        };

        let resp: reqwest::Response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Jito bundle REST request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Jito REST returned HTTP {status}: {text}");
        }

        let parsed: BundleResponse = resp
            .json::<BundleResponse>()
            .await
            .context("failed to parse Jito bundle response")?;

        if let Some(err) = parsed.error {
            bail!("Jito RPC error: {err}");
        }

        parsed
            .result
            .context("Jito bundle response missing 'result' field")
    }

    /// Submit a multi-transaction bundle (up to `max_bundle_size` txs).
    pub async fn submit_multi_bundle(&self, txs_base64: &[String]) -> Result<String> {
        if txs_base64.len() > self.config.max_bundle_size {
            bail!(
                "bundle size {} exceeds Jito limit {}",
                txs_base64.len(),
                self.config.max_bundle_size
            );
        }

        let url = format!(
            "{}/api/v1/bundles",
            self.config.block_engine_url.trim_end_matches('/')
        );

        let body = BundleRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "sendBundle",
            params: vec![txs_base64.to_vec()],
        };

        let resp: reqwest::Response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Jito multi-bundle REST request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Jito REST returned HTTP {status}: {text}");
        }

        let parsed: BundleResponse = resp
            .json::<BundleResponse>()
            .await
            .context("failed to parse Jito bundle response")?;

        if let Some(err) = parsed.error {
            bail!("Jito RPC error: {err}");
        }

        parsed
            .result
            .context("Jito multi-bundle response missing 'result' field")
    }
}
