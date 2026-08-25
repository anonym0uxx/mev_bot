//! Helius RPC client: blockhash fetch + transaction submission.
//!
//! Uses `ureq` (blocking, no async runtime) for minimal footprint.
//! The RPC URL and API key are provided by the daemon's environment —
//! this crate never reads credential files directly.



/// Helius RPC client for Solana mainnet.
pub struct HeliusRpc {
    rpc_url: String,
    client: ureq::Agent,
}

impl HeliusRpc {
    /// Create a new RPC client. The URL should include the API key:
    /// `https://mainnet.helius-solana.com/x=YOUR_KEY`
    #[must_use]
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            client: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        }
    }

    /// Fetch a recent blockhash via `getLatestBlockhash`.
    ///
    /// Returns the 32-byte blockhash on success.
    pub fn get_recent_blockhash(&self) -> Result<[u8; 32], String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": []
        }).to_string();

        let req = self.client
            .post(&self.rpc_url)
            .set("Content-Type", "application/json");

        let resp = req.send_string(&body)
            .map_err(|e| format!("getLatestBlockhash: {e}"))?;

        let text = resp.into_string()
            .map_err(|e| format!("blockhash response read: {e}"))?;

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("blockhash response parse: {e}"))?;

        let blockhash_b58 = json["result"]["value"]["blockhash"]
            .as_str()
            .ok_or("blockhash field missing in response")?;

        // Decode base58 blockhash to 32 bytes
        let bytes = decode_base58(blockhash_b58)
            .ok_or(format!("invalid base58 blockhash: {blockhash_b58}"))?;

        let mut out = [0u8; 32];
        if bytes.len() != 32 {
            return Err(format!("blockhash is {} bytes, expected 32", bytes.len()));
        }
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// Submit a base64-encoded transaction via `sendTransaction`.
    ///
    /// Returns the transaction signature on success.
    pub fn send_transaction(&self, base64_tx: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                base64_tx,
                {
                    "encoding": "base64",
                    "skipPreflight": false,
                    "preflightCommitment": "confirmed",
                    "maxRetries": 3
                }
            ]
        }).to_string();

        let req = self.client
            .post(&self.rpc_url)
            .set("Content-Type", "application/json");

        let resp = req.send_string(&body)
            .map_err(|e| format!("sendTransaction: {e}"))?;

        let text = resp.into_string()
            .map_err(|e| format!("sendTransaction response read: {e}"))?;

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("sendTransaction response parse: {e}"))?;

        // Check for RPC error
        if let Some(err) = json.get("error") {
            let msg = err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown RPC error");
            return Err(msg.to_string());
        }

        let sig = json["result"]
            .as_str()
            .ok_or("result field missing in sendTransaction response")?;

        Ok(sig.to_string())
    }

    /// Get the SOL balance of an address (in lamports).
    pub fn get_balance(&self, address: &str) -> Result<u64, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBalance",
            "params": [address, {"commitment": "confirmed"}]
        }).to_string();

        let req = self.client
            .post(&self.rpc_url)
            .set("Content-Type", "application/json");

        let resp = req.send_string(&body)
            .map_err(|e| format!("getBalance: {e}"))?;

        let text = resp.into_string()
            .map_err(|e| format!("getBalance response read: {e}"))?;

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("getBalance response parse: {e}"))?;

        if let Some(err) = json.get("error") {
            let msg = err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown RPC error");
            return Err(msg.to_string());
        }

        let lamports = json["result"]["value"]
            .as_u64()
            .ok_or("balance value missing")?;

        Ok(lamports)
    }
}

/// Decode base58 to raw bytes (variable length for blockhashes).
fn decode_base58(s: &str) -> Option<Vec<u8>> {
    const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    if s.is_empty() {
        return Some(Vec::new());
    }

    let mut out = vec![0u8; 0];
    for c in s.bytes() {
        let digit = B58.iter().position(|&a| a == c)? as u32;
        let mut carry = digit;
        for byte in out.iter_mut().rev() {
            let v = (u32::from(*byte) * 58) + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }

    // Reverse to big-endian
    out.reverse();

    // Handle leading zeros (encoded as leading '1' chars)
    let leading_ones = s.bytes().take_while(|&c| c == b'1').count();
    let mut result = vec![0u8; leading_ones];
    result.extend(out);
    Some(result)
}

// Re-export for transfer.rs (decode_base58_32 is imported directly in transfer.rs)
