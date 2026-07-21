//! Nozomi/Harmonic transaction landing client.
//! Sends raw serialized transactions directly to validators for fast, flat-fee landing.
//! Endpoint: https://ewr1.nozomi.temporal.xyz (Newark, NJ — co-located with Jito NY)

use anyhow::{Context, Result};

pub struct NozomiClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl NozomiClient {
    pub fn new(endpoint: String, api_key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build Nozomi HTTP client");
        Self {
            http,
            endpoint,
            api_key,
        }
    }

    /// Send a base64-encoded serialized transaction.
    /// Returns the transaction signature or an error.
    pub async fn send_transaction(&self, tx_b64: &str) -> Result<String> {
        // Nozomi uses standard Solana JSON-RPC sendTransaction format
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                tx_b64,
                { "encoding": "base64", "skipPreflight": true, "maxRetries": 0 }
            ]
        });

        let resp = self
            .http
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Nozomi HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nozomi returned {}: {}", status, body);
        }

        let json: serde_json::Value =
            resp.json().await.context("Nozomi response parse failed")?;
        let sig = json["result"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        Ok(sig)
    }

    pub fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}
