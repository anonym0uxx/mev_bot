//! Shared pool resolution utilities for graduation-related engines.
//!
//! Extracted from `graduation.rs` so that both `GraduationArbEngine` and
//! `MomentumEngine` can use the same vault extraction and reserve fetching logic.
//!
//! ## Functions
//!
//! - `extract_vaults_from_tx_response()` — find coin/pc vault addresses from `postTokenBalances`
//! - `fetch_vault_reserves()` — fetch SPL token vault reserves via `getMultipleAccountsInfo`
//! - `parse_spl_token_amount()` — decode SPL token account amount from base64 data

use super::graduation::PoolType;

/// WSOL mint in base58 for vault extraction matching.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Pump.fun bonding curve terminal price at graduation (lamports per token atom).
///
/// Derivation:
///   k = vSol₀ × vTokens₀ = 30e9 × 1.073e15 = 3.219e25
///   vTokens_terminal = 1.073e15 - 793.1e12 = 279.9e12
///   vSol_terminal = k / vTokens_terminal = 115.005e9
///   price = vSol_terminal / vTokens_terminal ≈ 4.1088e-4
pub const BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM: f64 = {
    let k: f64 = 30_000_000_000.0 * 1_073_000_000_000_000.0;
    let vtokens_terminal: f64 = 1_073_000_000_000_000.0 - 793_100_000_000_000.0;
    let vsol_terminal: f64 = k / vtokens_terminal;
    vsol_terminal / vtokens_terminal
};

/// Resolved pool information used by both arb and momentum engines.
#[derive(Debug, Clone, Copy)]
pub struct PoolInfo {
    /// Token vault (SPL token account for the base token).
    pub coin_vault: [u8; 32],
    /// SOL/WSOL vault (SPL token account for WSOL).
    pub pc_vault: [u8; 32],
    /// Token reserves in atoms.
    pub reserve_token: u64,
    /// SOL reserves in lamports.
    pub reserve_sol: u64,
    /// Type of DEX pool.
    pub pool_type: PoolType,
    /// Token mint address.
    pub mint: [u8; 32],
}

impl PoolInfo {
    /// Price in lamports per token atom (reserve_sol / reserve_token).
    #[inline(always)]
    pub fn price_lamports_per_atom(&self) -> f64 {
        self.reserve_sol as f64 / self.reserve_token as f64
    }

    /// Spread vs BC terminal price in percent.
    #[inline(always)]
    pub fn spread_vs_bc_pct(&self) -> f64 {
        let ray_price = self.price_lamports_per_atom();
        (BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM - ray_price).abs()
            / BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM
            * 100.0
    }
}

/// Decode a base58-encoded string into a 32-byte array.
#[inline(always)]
fn decode_bs58_32(s: &str) -> Option<[u8; 32]> {
    let mut buf = [0u8; 32];
    let n = bs58::decode(s).onto(&mut buf[..]).ok()?;
    if n == 32 { Some(buf) } else { None }
}

/// Extract vault addresses from getTransaction jsonParsed response.
///
/// Uses `postTokenBalances` to find `coin_vault` (token) and `pc_vault` (WSOL).
/// Works with v0 ALT transactions — `postTokenBalances` always contains all
/// token balance changes.
///
/// Returns `(coin_vault_bytes, pc_vault_bytes)` or `None` if extraction fails.
#[inline(always)]
pub fn extract_vaults_from_tx_response(
    tx_json: &serde_json::Value,
    graduation_mint: &str,
) -> Option<([u8; 32], [u8; 32])> {
    let account_keys = tx_json
        .pointer("/transaction/message/accountKeys")?
        .as_array()?;
    let post_token_balances = tx_json
        .pointer("/meta/postTokenBalances")?
        .as_array()?;

    let mut coin_vault_idx: Option<usize> = None;
    let mut pc_vault_idx: Option<usize> = None;
    let mut max_token_amount: u64 = 0;
    let mut max_wsol_amount: u64 = 0;

    for entry in post_token_balances {
        let mint = match entry.get("mint").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => continue,
        };
        let idx = match entry.get("accountIndex").and_then(|i| i.as_u64()) {
            Some(i) => i as usize,
            None => continue,
        };
        let amount: u64 = entry
            .pointer("/uiTokenAmount/amount")
            .and_then(|a| a.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if mint == graduation_mint && amount > max_token_amount {
            max_token_amount = amount;
            coin_vault_idx = Some(idx);
        }
        if mint == WSOL_MINT && amount > max_wsol_amount {
            max_wsol_amount = amount;
            pc_vault_idx = Some(idx);
        }
    }

    // Resolve account addresses from accountKeys (handles both string and object formats)
    let resolve_key = |idx: usize| -> Option<[u8; 32]> {
        let key = account_keys.get(idx)?;
        let key_str = key
            .as_str()
            .or_else(|| key.get("pubkey").and_then(|p| p.as_str()))?;
        decode_bs58_32(key_str)
    };

    let coin_vault = resolve_key(coin_vault_idx?)?;
    let pc_vault = resolve_key(pc_vault_idx?)?;
    Some((coin_vault, pc_vault))
}

/// Fetch SPL token vault reserves via `getMultipleAccountsInfo`.
///
/// Returns `(reserve_token_atoms, reserve_sol_lamports)` or `None` on failure.
/// Uses a 150ms timeout on the RPC call.
pub async fn fetch_vault_reserves(
    client: &reqwest::Client,
    rpc_url: &str,
    coin_vault_b58: &str,
    pc_vault_b58: &str,
) -> Option<(u64, u64)> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getMultipleAccounts",
        "params": [
            [coin_vault_b58, pc_vault_b58],
            {"encoding": "base64", "commitment": "confirmed"}
        ]
    });

    let resp = client
        .post(rpc_url)
        .timeout(std::time::Duration::from_millis(150))
        .json(&body)
        .send()
        .await
        .ok()?;

    let json: serde_json::Value = resp.json().await.ok()?;
    let accounts = json
        .pointer("/result/value")
        .and_then(|v| v.as_array())?;

    if accounts.len() < 2 {
        return None;
    }

    let parse_account = |v: &serde_json::Value| -> Option<u64> {
        let data_arr = v.get("data")?.as_array()?;
        let data_b64 = data_arr.first()?.as_str()?;
        parse_spl_token_amount(data_b64)
    };

    let reserve_token = parse_account(&accounts[0])?;
    let reserve_sol = parse_account(&accounts[1])?;
    Some((reserve_token, reserve_sol))
}

/// Parse SPL token account amount from base64-encoded account data.
///
/// SPL Token Account layout: amount is a LE u64 at bytes [64..72].
/// Minimal account size is 165 bytes.
#[inline(always)]
pub fn parse_spl_token_amount(data_b64: &str) -> Option<u64> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64).ok()?;
    if bytes.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(
        bytes[64..72].try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn test_parse_spl_token_amount_valid() {
        // Create a 165-byte buffer with a known u64 at bytes [64..72]
        let mut data = vec![0u8; 165];
        let amount: u64 = 1_000_000_000; // 1 SOL in lamports
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        let result = parse_spl_token_amount(&encoded);
        assert_eq!(result, Some(1_000_000_000));
    }

    #[test]
    fn test_parse_spl_token_amount_too_short() {
        // Only 64 bytes — should return None (needs at least 72)
        let data = vec![0u8; 64];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        let result = parse_spl_token_amount(&encoded);
        assert_eq!(result, None);
    }

    #[test]
    fn test_pool_info_price_calc() {
        let info = PoolInfo {
            coin_vault: [1u8; 32],
            pc_vault: [2u8; 32],
            reserve_token: 200_000_000_000_000, // 200T atoms
            reserve_sol: 80_000_000_000,        // 80 SOL in lamports
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xAA; 32],
        };

        // price = 80e9 / 200e12 = 0.0004 lamports per atom
        let price = info.price_lamports_per_atom();
        assert!((price - 0.0004).abs() < 1e-10);

        // spread vs BC terminal price should be a reasonable percentage
        let spread = info.spread_vs_bc_pct();
        assert!(spread.is_finite());
        assert!(spread >= 0.0);
        // BC terminal is ~4.1088e-4, our price is 4.0e-4, so spread ≈ 2.6%
        assert!(spread < 10.0, "spread was {} but expected < 10%", spread);
    }
}
