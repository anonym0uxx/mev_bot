//! Jito Bundle + Nozomi dual-submit for graduation arbitrage.
//!
//! Submits Raydium swap transactions as Jito bundles via the Block Engine
//! REST API, with simultaneous Nozomi fast-lane submission for higher
//! landing rate.
//!
//! ## Architecture
//!
//! ```text
//! build_swap_tx() → serialize → dual_submit()
//!                                   ├── Jito Block Engine (bundle)
//!                                   └── Nozomi fast-lane (raw TX)
//! ```
//!
//! ## Adaptive Tipping
//!
//! Tip is computed from expected profit and estimated competition:
//! - Never tips > 10% of expected profit
//! - Scales with slots since graduation (more slots = more competitors)
//! - Minimum tip: 10,000 lamports (Jito minimum)

use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

// ── Constants ────────────────────────────────────────────────────────────────

/// Jito Block Engine mainnet URL.
pub const JITO_BLOCK_ENGINE_URL: &str = "https://mainnet.block-engine.jito.wtf";

/// Jito bundle submission endpoint.
pub const JITO_BUNDLE_ENDPOINT: &str = "/api/v1/bundles";

/// Minimum Jito tip (lamports).
pub const MIN_JITO_TIP: u64 = 10_000;

/// Maximum fraction of expected profit to tip (basis points out of 10000).
/// 1000 = 10%.
pub const MAX_TIP_FRACTION_BPS: u64 = 1_000;

/// Jito tip accounts — random selection distributes load.
pub const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
];

/// Nozomi tip accounts for fast-lane submission.
pub const NOZOMI_TIP_ACCOUNTS: [&str; 17] = [
    "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq",
    "noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4",
    "noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE",
    "noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo",
    "noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ",
    "nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L",
    "nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z",
    "nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu",
    "noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7",
    "nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP",
    "nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P",
    "nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge",
    "nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3",
    "nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ",
    "nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk",
    "nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne",
    "nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb",
];

/// Raydium CPMM program ID.
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";

/// Raydium AMM V4 program ID.
pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

// ── Adaptive Tipping ─────────────────────────────────────────────────────────

/// Compute Jito tip based on expected profit and competition estimate.
///
/// Strategy (from Yang et al. 2507.08302 competition model):
/// - slots_since_graduation == 0: we're likely first → base tip
/// - slots_since_graduation == 1: others may see → 2× base
/// - slots_since_graduation >= 2: competitive → 5× base
/// - Never tip > 10% of expected profit
/// - Minimum: 10,000 lamports (Jito minimum)
#[inline(always)]
pub fn compute_adaptive_tip(
    expected_profit_lamports: u64,
    slots_since_graduation: u8,
) -> u64 {
    let base_tip: u64 = 500_000; // 0.0005 SOL

    let competition_mult: u64 = match slots_since_graduation {
        0 => 1,
        1 => 2,
        _ => 5,
    };

    let scaled_tip = base_tip.saturating_mul(competition_mult);
    let max_tip = expected_profit_lamports * MAX_TIP_FRACTION_BPS / 10_000;

    scaled_tip.min(max_tip).max(MIN_JITO_TIP)
}

/// Select a random Jito tip account.
///
/// Uses a simple counter-based rotation (not cryptographic randomness —
/// we just need load distribution, not unpredictability).
#[inline(always)]
pub fn select_jito_tip_account(counter: u64) -> Pubkey {
    let idx = (counter as usize) % JITO_TIP_ACCOUNTS.len();
    Pubkey::from_str(JITO_TIP_ACCOUNTS[idx]).unwrap()
}

/// Select a random Nozomi tip account.
#[inline(always)]
pub fn select_nozomi_tip_account(counter: u64) -> Pubkey {
    let idx = (counter as usize) % NOZOMI_TIP_ACCOUNTS.len();
    Pubkey::from_str(NOZOMI_TIP_ACCOUNTS[idx]).unwrap()
}

// ── Bundle Submission ────────────────────────────────────────────────────────

/// Error from bundle submission.
#[derive(Debug)]
pub enum BundleError {
    /// Network error during submission.
    Network(String),
    /// Bundle rejected by block engine.
    Rejected(String),
    /// Serialization error.
    Serialize(String),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network: {}", e),
            Self::Rejected(e) => write!(f, "rejected: {}", e),
            Self::Serialize(e) => write!(f, "serialize: {}", e),
        }
    }
}

/// Submit a serialized transaction as a Jito bundle.
///
/// # Arguments
/// * `client` — shared reqwest client
/// * `tx_base64` — base64-encoded serialized transaction
///
/// # Returns
/// Bundle ID on success.
pub async fn submit_jito_bundle(
    client: &reqwest::Client,
    tx_base64: &str,
) -> Result<String, BundleError> {
    let url = format!("{}{}", JITO_BLOCK_ENGINE_URL, JITO_BUNDLE_ENDPOINT);

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [[tx_base64]]
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BundleError::Network(e.to_string()))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BundleError::Network(format!("parse: {}", e)))?;

    if let Some(error) = json.get("error") {
        return Err(BundleError::Rejected(error.to_string()));
    }

    json.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| BundleError::Rejected("no result in response".to_string()))
}

/// Submit a serialized transaction via Nozomi fast-lane.
///
/// Nozomi accepts standard `sendTransaction` RPC calls with a tip
/// transfer instruction included in the transaction.
pub async fn submit_nozomi(
    client: &reqwest::Client,
    nozomi_rpc_url: &str,
    tx_base64: &str,
) -> Result<String, BundleError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [
            tx_base64,
            {
                "encoding": "base64",
                "skipPreflight": true,
                "preflightCommitment": "confirmed",
                "maxRetries": 0
            }
        ]
    });

    let resp = client
        .post(nozomi_rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BundleError::Network(format!("nozomi: {}", e)))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BundleError::Network(format!("nozomi parse: {}", e)))?;

    if let Some(error) = json.get("error") {
        return Err(BundleError::Rejected(format!("nozomi: {}", error)));
    }

    json.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| BundleError::Rejected("nozomi: no result".to_string()))
}

/// Dual-submit: Jito bundle + Nozomi fast-lane simultaneously.
///
/// Returns the first successful result. If both fail, returns the Jito error.
pub async fn dual_submit(
    client: &reqwest::Client,
    nozomi_rpc_url: &str,
    tx_base64: &str,
) -> Result<String, BundleError> {
    let (jito_result, nozomi_result) = tokio::join!(
        submit_jito_bundle(client, tx_base64),
        submit_nozomi(client, nozomi_rpc_url, tx_base64),
    );

    // Prefer Jito result, fall back to Nozomi
    match (jito_result, nozomi_result) {
        (Ok(id), _) => Ok(format!("jito:{}", id)),
        (_, Ok(id)) => Ok(format!("nozomi:{}", id)),
        (Err(e), _) => Err(e),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_tip_first_slot() {
        // First slot, 0.01 SOL expected profit
        let tip = compute_adaptive_tip(10_000_000, 0);
        assert_eq!(tip, 500_000); // base tip
    }

    #[test]
    fn test_adaptive_tip_second_slot() {
        let tip = compute_adaptive_tip(10_000_000, 1);
        assert_eq!(tip, 1_000_000); // 2× base
    }

    #[test]
    fn test_adaptive_tip_competitive() {
        let tip = compute_adaptive_tip(10_000_000, 3);
        // 5× base = 2_500_000, but max 10% of profit = 1_000_000
        assert_eq!(tip, 1_000_000);
    }

    #[test]
    fn test_adaptive_tip_minimum() {
        // Very small profit → tip should be minimum
        let tip = compute_adaptive_tip(5_000, 0);
        assert_eq!(tip, MIN_JITO_TIP); // 10K lamports minimum
    }

    #[test]
    fn test_adaptive_tip_zero_profit() {
        let tip = compute_adaptive_tip(0, 0);
        assert_eq!(tip, MIN_JITO_TIP);
    }

    #[test]
    fn test_tip_account_rotation() {
        let a0 = select_jito_tip_account(0);
        let a1 = select_jito_tip_account(1);
        let a8 = select_jito_tip_account(8); // wraps to index 0
        assert_ne!(a0, a1);
        assert_eq!(a0, a8); // 8 % 8 == 0
    }

    #[test]
    fn test_nozomi_account_rotation() {
        let a0 = select_nozomi_tip_account(0);
        let a17 = select_nozomi_tip_account(17); // wraps to index 0
        assert_eq!(a0, a17);
    }

    #[test]
    fn test_all_jito_accounts_valid() {
        for (i, acc) in JITO_TIP_ACCOUNTS.iter().enumerate() {
            assert!(Pubkey::from_str(acc).is_ok(), "invalid Jito tip account at index {}", i);
        }
    }

    #[test]
    fn test_all_nozomi_accounts_valid() {
        for (i, acc) in NOZOMI_TIP_ACCOUNTS.iter().enumerate() {
            assert!(Pubkey::from_str(acc).is_ok(), "invalid Nozomi tip account at index {}", i);
        }
    }
}
