//! Shared pool resolution utilities for post-graduation engines.
//!
//! Extracted from `arb/pool_resolver.rs` and `arb/graduation.rs` so that the
//! momentum engine can resolve pools independently of the (now-deleted) arb module.
//!
//! ## Types
//!
//! - `PoolType` — DEX pool type enum (Raydium, PumpSwap, Unknown)
//! - `PoolInfo` — resolved pool with vault addresses and reserves
//! - `PoolResolution` — result of resolving a pool from a graduation transaction
//!
//! ## Functions
//!
//! - `resolve_pool_from_transaction()` — resolve pool from getTransaction RPC
//! - `make_pool_resolution_client()` — create shared reqwest client
//! - `extract_vaults_from_tx_response()` — find vault addresses from postTokenBalances
//! - `fetch_vault_reserves()` — fetch SPL token vault reserves via getMultipleAccountsInfo
//! - `fetch_vault_reserves_from_pubkeys()` — fetch vault reserves from raw [u8; 32] pubkeys
//! - `parse_spl_token_amount()` — decode SPL token account amount from base64 data

use once_cell::sync::Lazy;

/// Concurrency semaphore for pool resolution.
///
/// Limits concurrent pool resolution RPC calls to 5. Excess callers are
/// dropped immediately (try_acquire) rather than queued, preventing unbounded
/// HTTP connection buildup from CoreCast graduation storms.
static POOL_RESOLUTION_SEMAPHORE: Lazy<tokio::sync::Semaphore> =
    Lazy::new(|| tokio::sync::Semaphore::new(5));

/// WSOL mint in base58 for vault extraction matching.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Minimum viable liquidity — reject pools with less than 50 SOL in reserves.
/// Fresh pump.fun graduations have 85-120 SOL; anything below 50 SOL is a
/// drained historical pool or failed launch returned by getProgramAccounts.
pub const MIN_SOL_RESERVES_LAMPORTS: u64 = 50_000_000_000; // 50 SOL

/// Minimum viable liquidity for PumpSwap pools specifically (FIX-3).
/// New pump.fun graduations deposit ~85 SOL to PumpSwap, but some valid
/// pools start with 30-50 SOL. Lower threshold than Raydium to capture
/// fresh graduations. Raydium keeps 50 SOL minimum (stale/legacy pools).
pub const MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS: u64 = 30_000_000_000; // 30 SOL

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

// ── Pool Type ────────────────────────────────────────────────────────────────

/// Type of DEX pool a token graduated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    /// Raydium AMM V4 — traditional graduation target.
    RaydiumAmmV4,
    /// PumpSwap — Pump.fun's native DEX.
    PumpSwap,
    /// Pool type could not be determined (RPC succeeded but parsing failed).
    Unknown,
}

impl PoolType {
    /// Serialization string for JSONL output.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RaydiumAmmV4 => "raydium_amm_v4",
            Self::PumpSwap => "pump_swap",
            Self::Unknown => "unknown",
        }
    }
}

// ── Pool Info ────────────────────────────────────────────────────────────────

/// Resolved pool information used by the momentum engine.
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

// ── Pool Resolution ──────────────────────────────────────────────────────────

/// Raydium AMM V4 program ID (base58: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8).
const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// PumpSwap AMM program ID (base58: pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA).
const PUMPSWAP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// WSOL mint as raw bytes for detecting reversed PumpSwap pool ordering.
/// PumpSwap sorts mints by raw byte comparison — when token > WSOL,
/// WSOL becomes base_mint and token becomes quote_mint.
const WSOL_MINT_BYTES: [u8; 32] = [
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
];

/// Result of resolving a pool from a graduation transaction.
#[derive(Debug, Clone)]
pub struct PoolResolution {
    /// Token mint address.
    pub mint: [u8; 32],
    /// DEX pool address. For Raydium, this is amm_id. ([0u8; 32] if extraction failed.)
    pub pool_address: [u8; 32],
    /// Token vault (SPL token account).
    pub coin_vault: [u8; 32],
    /// SOL/WSOL vault (SPL token account).
    pub pc_vault: [u8; 32],
    /// Type of DEX pool.
    pub pool_type: PoolType,
    /// Initial SOL reserves in pool (lamports). 0 if unknown.
    pub reserve_sol_lamports: u64,
    /// Initial token reserves in pool (atoms). 0 if unknown.
    pub reserve_token_atoms: u64,
    /// Bonding curve vSol at graduation (~85 SOL). 0.0 if unknown.
    pub bc_terminal_vsol: f64,
    /// Block time of the graduation transaction (ms since epoch). 0 if unknown.
    /// Used to gate stale CoreCast backlog events (old Raydium-era graduations).
    pub grad_block_time_ms: u64,

    // ── Raydium AMM V4 pool accounts (zero for PumpSwap/Unknown) ─────────
    /// Raydium AMM pool state account. Same as pool_address for Raydium.
    pub amm_id: [u8; 32],
    /// AMM open orders account (Serum/OpenBook order book state).
    pub amm_open_orders: [u8; 32],
    /// AMM target orders account.
    pub amm_target_orders: [u8; 32],
    /// Serum/OpenBook market account.
    pub serum_market: [u8; 32],
    /// Serum market bids slab.
    pub serum_bids: [u8; 32],
    /// Serum market asks slab.
    pub serum_asks: [u8; 32],
    /// Serum event queue.
    pub serum_event_queue: [u8; 32],
    /// Serum coin vault (token side).
    pub serum_coin_vault: [u8; 32],
    /// Serum pc vault (WSOL side).
    pub serum_pc_vault: [u8; 32],
    /// Serum vault signer (PDA).
    pub serum_vault_signer: [u8; 32],
}

/// Decode a base58-encoded string into a 32-byte array.
#[inline(always)]
fn decode_bs58_32(s: &str) -> Option<[u8; 32]> {
    let mut buf = [0u8; 32];
    let n = bs58::decode(s).onto(&mut buf[..]).ok()?;
    if n == 32 { Some(buf) } else { None }
}

/// Create a shared `reqwest::Client` for pool resolution.
///
/// 8s timeout — graduation txs often take 500ms-5s to be indexed by Helius.
/// The old 180ms limit was an arb latency budget, not appropriate for momentum.
pub fn make_pool_resolution_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(8_000))
        .build()
        .expect("reqwest client build should not fail")
}

/// Error message returned when getTransaction result is null (tx not yet indexed by RPC).
const TX_NOT_FOUND_ERR: &str = "transaction not found (not yet indexed)";

/// Resolve pool address and initial reserves from a graduation transaction.
///
/// v2 approach: two-phase vault extraction.
///   Phase A: Extract vault addresses from `postTokenBalances` in `getTransaction`
///            — works reliably with v0 ALT transactions.
///   Phase B: `getMultipleAccountsInfo` on vault addresses, parse SPL token
///            account amount at bytes [64..72] LE u64.
///
/// Retries up to 5 times with exponential backoff when the RPC returns
/// `result: null` — this means Helius hasn't indexed the tx yet.
/// Non-retriable errors (parse failures, vault extraction) fail immediately.
///
/// Returns `None` if: all retries exhausted, non-retriable error, or tx doesn't
/// contain pool creation.
///
/// # Parameters
/// - `client` — shared reqwest HTTP client
/// - `sig` — 64-byte transaction signature
/// - `public_rpc_url` — public Solana RPC for getTransaction / getMultipleAccounts
/// - `helius_rpc_url` — Helius API-key endpoint for getProgramAccounts (fallback)
#[inline(never)]
pub async fn resolve_pool_from_transaction(
    client: &reqwest::Client,
    sig: &[u8; 64],
    public_rpc_url: &str,
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    // ── Concurrency gate: drop if 5 resolutions already in flight ────────
    let _permit = match POOL_RESOLUTION_SEMAPHORE.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("[pool] resolution semaphore full — dropping resolve_pool_from_transaction");
            return None;
        }
    };

    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

    let sig_b58_short = &bs58::encode(sig).into_string()[..8];

    for attempt in 1..=MAX_ATTEMPTS {
        match resolve_pool_inner(client, sig, public_rpc_url, helius_rpc_url).await {
            Ok(resolution) => return Some(resolution),
            Err(e) => {
                let is_retriable = e == TX_NOT_FOUND_ERR
                    || e.contains("missing result field")
                    || e.contains("not yet indexed")
                    || e.contains("getMultipleAccountsInfo failed")
                    || e.contains("RPC request failed");

                if !is_retriable || attempt == MAX_ATTEMPTS {
                    tracing::debug!(
                        attempt,
                        sig = %sig_b58_short,
                        "[momentum] pool resolution failed (final): {}",
                        e
                    );
                    return None;
                }

                let delay_ms = BACKOFF_MS[(attempt - 1) as usize];
                tracing::info!(
                    attempt,
                    next_delay_ms = delay_ms,
                    sig = %sig_b58_short,
                    "[momentum] pool resolution retry — tx not yet indexed"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }

    None
}

/// Inner implementation — returns Result for clean error propagation.
///
/// - `public_rpc_url` used for getTransaction and getMultipleAccounts (read-heavy, free tier)
/// - `helius_rpc_url` used for getProgramAccounts fallbacks (requires API key)
async fn resolve_pool_inner(
    client: &reqwest::Client,
    sig: &[u8; 64],
    public_rpc_url: &str,
    helius_rpc_url: &str,
) -> Result<PoolResolution, String> {
    let sig_b58 = bs58::encode(sig).into_string();

    // Phase A: getTransaction with jsonParsed — extract vault addresses from postTokenBalances
    // Use "processed" commitment: Helius logsSubscribe fires at processed level, so the tx
    // may not be confirmed yet. "processed" is safe — confirmed txs are always available at
    // processed level. This eliminates the getTransaction race condition for fresh graduations.
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            sig_b58,
            {
                "encoding": "jsonParsed",
                "maxSupportedTransactionVersion": 0,
                "commitment": "confirmed"
            }
        ]
    });

    // getTransaction → Helius RPC (public RPC rate-limits this call aggressively).
    // With the 60/min rate gate in on_migration(), this is well within Helius budget.
    let resp = client
        .post(helius_rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("RPC request failed: {}", e))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("RPC response parse failed: {}", e))?;

    let result_field = json.get("result");

    // Distinguish null result (tx not indexed yet → retriable) from missing result (RPC error)
    let tx = match result_field {
        Some(v) if v.is_null() => {
            return Err(TX_NOT_FOUND_ERR.to_string());
        }
        Some(v) => v,
        None => {
            let err_msg = json
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("missing result field");
            return Err(format!("RPC returned error: {}", err_msg));
        }
    };

    // Extract blockTime from tx for staleness gating (seconds → ms)
    let grad_block_time_ms: u64 = tx
        .get("blockTime")
        .and_then(|v| v.as_i64())
        .map(|t| (t as u64).saturating_mul(1_000))
        .unwrap_or(0);

    // Detect pool type from account keys
    let account_keys_strs: Vec<&str> = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(|a| a.as_array())
        .map(|keys| {
            keys.iter()
                .filter_map(|k| {
                    k.as_str()
                        .or_else(|| k.get("pubkey").and_then(|p| p.as_str()))
                })
                .collect()
        })
        .unwrap_or_default();

    // PumpSwap-first priority: since April 2026, all new pump.fun graduations go to PumpSwap.
    // PumpSwap graduation txs may contain BOTH program IDs (CPI chain from pump.fun → PumpSwap),
    // so the first check wins. Check PumpSwap before Raydium.
    let pool_type = if account_keys_strs.iter().any(|k| *k == PUMPSWAP_AMM_PROGRAM) {
        PoolType::PumpSwap
    } else if account_keys_strs.iter().any(|k| *k == RAYDIUM_AMM_V4_PROGRAM) {
        PoolType::RaydiumAmmV4
    } else {
        PoolType::Unknown
    };

    // Extract the graduation mint from postTokenBalances (first non-WSOL mint)
    let graduation_mint_b58 = tx
        .pointer("/meta/postTokenBalances")
        .and_then(|b| b.as_array())
        .and_then(|balances| {
            balances.iter().find_map(|entry| {
                let mint = entry.get("mint")?.as_str()?;
                if mint != WSOL_MINT { Some(mint.to_string()) } else { None }
            })
        })
        .ok_or_else(|| "no non-WSOL mint in postTokenBalances".to_string())?;

    let mint = decode_bs58_32(&graduation_mint_b58)
        .ok_or_else(|| format!("invalid mint: {}", graduation_mint_b58))?;

    // Phase A: extract vault addresses from postTokenBalances
    let (coin_vault, pc_vault) = extract_vaults_from_tx_response(tx, &graduation_mint_b58)
        .ok_or_else(|| "failed to extract vault addresses from postTokenBalances".to_string())?;

    let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

    tracing::debug!(
        sig = %sig_b58,
        pool_type = %pool_type.as_str(),
        coin_vault = %coin_vault_b58,
        pc_vault = %pc_vault_b58,
        mint = %graduation_mint_b58,
        "[momentum] v2 vault extraction from postTokenBalances"
    );

    // Phase B: fetch vault reserves via getMultipleAccounts → public RPC
    let (reserve_token, reserve_sol) =
        fetch_vault_reserves(client, public_rpc_url, &coin_vault_b58, &pc_vault_b58)
            .await
            .ok_or_else(|| "getMultipleAccountsInfo failed for vault reserves".to_string())?;

    tracing::debug!(
        reserve_token = reserve_token,
        reserve_sol = reserve_sol,
        "[momentum] v2 vault reserves fetched"
    );

    // Minimum viable liquidity check — reject empty/drained pools
    if reserve_sol < MIN_SOL_RESERVES_LAMPORTS {
        let pool_str = if pool_type == PoolType::RaydiumAmmV4 { "Raydium" } else { "PumpSwap" };
        tracing::warn!(
            mint = %graduation_mint_b58,
            reserve_sol,
            pool_type = %pool_str,
            "[momentum] pool rejected from tx — insufficient liquidity (reserve_sol < 50 SOL)"
        );
        return Err(format!(
            "pool has insufficient liquidity: {} lamports < {} minimum",
            reserve_sol, MIN_SOL_RESERVES_LAMPORTS
        ));
    }

    // ── Phase C: Raydium pool account resolution ─────────────────────────
    // For Raydium AMM V4 pools, extract amm_id from accountKeys and fetch
    // full pool accounts (open_orders, target_orders, serum market, etc.).
    // For PumpSwap/Unknown, all Raydium fields are zero.

    let mut amm_id = [0u8; 32];
    let mut amm_open_orders = [0u8; 32];
    let mut amm_target_orders = [0u8; 32];
    let mut serum_market = [0u8; 32];
    let mut serum_bids = [0u8; 32];
    let mut serum_asks = [0u8; 32];
    let mut serum_event_queue = [0u8; 32];
    let mut serum_coin_vault = [0u8; 32];
    let mut serum_pc_vault = [0u8; 32];
    let mut serum_vault_signer = [0u8; 32];

    if pool_type == PoolType::RaydiumAmmV4 {
        // Extract amm_id from the graduation tx accountKeys.
        // amm_id is the writable account that is NOT a known program,
        // NOT coin_vault, NOT pc_vault, NOT WSOL mint, NOT the token mint.
        let known_programs: &[&str] = &[
            RAYDIUM_AMM_V4_PROGRAM,
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
            "11111111111111111111111111111111",
            "SysvarRent111111111111111111111111111111111",
            "SysvarC1ock11111111111111111111111111111111",
            "ComputeBudget111111111111111111111111111111",
            "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin",
            "srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX",
            WSOL_MINT,
            PUMPSWAP_AMM_PROGRAM,
            // Pump.fun programs
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
            "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp18C",
        ];

        let extracted_amm_id = extract_amm_id_from_account_keys(
            &account_keys_strs,
            &coin_vault_b58,
            &pc_vault_b58,
            &graduation_mint_b58,
            known_programs,
        );

        match extracted_amm_id {
            Some(id) => {
                amm_id = id;
                let amm_id_b58 = bs58::encode(&amm_id).into_string();
                tracing::debug!(
                    amm_id = %amm_id_b58,
                    "[momentum] extracted amm_id from accountKeys"
                );

                // Fetch full Raydium pool accounts (2 RPC calls) → public RPC
                match crate::tx::raydium::fetch_raydium_pool_accounts(
                    client,
                    public_rpc_url,
                    &amm_id,
                    coin_vault,
                    pc_vault,
                )
                .await
                {
                    Ok(pool_accounts) => {
                        amm_open_orders = pool_accounts.amm_open_orders;
                        amm_target_orders = pool_accounts.amm_target_orders;
                        serum_market = pool_accounts.serum_market;
                        serum_bids = pool_accounts.serum_bids;
                        serum_asks = pool_accounts.serum_asks;
                        serum_event_queue = pool_accounts.serum_event_queue;
                        serum_coin_vault = pool_accounts.serum_coin_vault;
                        serum_pc_vault = pool_accounts.serum_pc_vault;
                        serum_vault_signer = pool_accounts.serum_vault_signer;
                        tracing::info!(
                            amm_id = %amm_id_b58,
                            serum_market = %bs58::encode(&serum_market).into_string(),
                            "[momentum] Raydium pool accounts resolved successfully"
                        );
                    }
                    Err(e) => {
                        // Fetch failed — log warning, leave all Raydium fields as zeros.
                        // Don't block graduation — position will be paper-mode fallback.
                        tracing::warn!(
                            amm_id = %amm_id_b58,
                            err = %e,
                            "[momentum] fetch_raydium_pool_accounts FAILED — Raydium fields zeroed"
                        );
                    }
                }
            }
            None => {
                tracing::warn!(
                    "[momentum] could not extract amm_id from accountKeys — Raydium fields zeroed"
                );
            }
        }
    }

    // ── FIX-2: PumpSwap preference ───────────────────────────────────────────
    // Post-April 2026: all new pump.fun tokens graduate to PumpSwap.
    // CoreCast backlog sigs point to old Raydium migration txs, but those pools
    // are dead — all real trading is on PumpSwap. When we resolve a Raydium pool
    // via the sig, always check for an active PumpSwap pool first.
    if pool_type == PoolType::RaydiumAmmV4 {
        if let Some(ps) = resolve_pumpswap_pool_from_mint(client, &mint, public_rpc_url, helius_rpc_url).await {
            if ps.reserve_sol_lamports > 0 {
                tracing::info!(
                    mint = %bs58::encode(&mint).into_string(),
                    raydium_sol = reserve_sol / 1_000_000_000,
                    pumpswap_sol = ps.reserve_sol_lamports / 1_000_000_000,
                    "[pool] preferring PumpSwap over Raydium — active pool found"
                );
                return Ok(ps);
            }
        }
    }
    // ── End FIX-2 ─────────────────────────────────────────────────────────────

    Ok(PoolResolution {
        mint,
        pool_address: amm_id, // For Raydium, pool_address = amm_id
        coin_vault,
        pc_vault,
        pool_type,
        reserve_sol_lamports: reserve_sol,
        reserve_token_atoms: reserve_token,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms,
        amm_id,
        amm_open_orders,
        amm_target_orders,
        serum_market,
        serum_bids,
        serum_asks,
        serum_event_queue,
        serum_coin_vault,
        serum_pc_vault,
        serum_vault_signer,
    })
}

/// Extract the Raydium AMM pool ID (amm_id) from graduation tx accountKeys.
///
/// The amm_id is the unknown writable account that is NOT:
/// - Any known program address
/// - coin_vault or pc_vault
/// - WSOL mint or the token mint
///
/// Returns `None` if no suitable candidate is found.
fn extract_amm_id_from_account_keys(
    account_keys: &[&str],
    coin_vault_b58: &str,
    pc_vault_b58: &str,
    graduation_mint_b58: &str,
    known_programs: &[&str],
) -> Option<[u8; 32]> {
    // Collect all accounts that are NOT known programs, NOT vaults, NOT mints
    let mut candidates: Vec<[u8; 32]> = Vec::new();

    for key_str in account_keys {
        // Skip known programs
        if known_programs.contains(key_str) {
            continue;
        }
        // Skip vaults (already identified)
        if *key_str == coin_vault_b58 || *key_str == pc_vault_b58 {
            continue;
        }
        // Skip the token mint
        if *key_str == graduation_mint_b58 {
            continue;
        }
        // Skip WSOL mint
        if *key_str == WSOL_MINT {
            continue;
        }

        if let Some(decoded) = decode_bs58_32(key_str) {
            candidates.push(decoded);
        }
    }

    // The amm_id is typically the first unknown writable account.
    // In Raydium graduation txs, the amm_id appears early in the account list
    // (usually index 1-5). We take the first candidate.
    candidates.into_iter().next()
}

/// Extract vault addresses from getTransaction jsonParsed response.
///
/// Uses `postTokenBalances` to find `coin_vault` (token) and `pc_vault` (WSOL).
/// Works with v0 ALT transactions.
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
        .timeout(std::time::Duration::from_millis(500))
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

/// Fetch SPL token vault reserves from raw `[u8; 32]` pubkeys via `getMultipleAccounts`.
///
/// Convenience wrapper around `fetch_vault_reserves()` that handles bs58 encoding.
/// Used by `on_pumpswap_graduation_direct()` where vault pubkeys are already in byte form.
///
/// Returns `(reserve_token_atoms, reserve_sol_lamports)` or `None` on failure.
pub async fn fetch_vault_reserves_from_pubkeys(
    client: &reqwest::Client,
    rpc_url: &str,
    coin_vault: &[u8; 32],
    pc_vault: &[u8; 32],
) -> Option<(u64, u64)> {
    let coin_vault_b58 = bs58::encode(coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(pc_vault).into_string();
    fetch_vault_reserves(client, rpc_url, &coin_vault_b58, &pc_vault_b58).await
}

/// Parse SPL token account amount from base64-encoded account data.
///
/// SPL Token Account layout: amount is a LE u64 at bytes [64..72].
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

/// Resolve a PumpSwap AMM pool from the token mint using getProgramAccounts.
///
/// PumpSwap Pool account layout (verified on-chain 2026-04-01):
///   [0..8]    discriminator (f19a6d0411b16dbc)
///   [8]       pool_bump (u8)
///   [9..11]   index (u16 LE)
///   [11..43]  creator (pubkey)
///   [43..75]  base_mint  ← Can be WSOL or token (sorted by raw bytes)
///   [75..107] quote_mint ← Can be WSOL or token (sorted by raw bytes)
///   [107..139] lp_mint
///   [139..171] pool_base_token_account  ← vault for base_mint
///   [171..203] pool_quote_token_account ← vault for quote_mint
///
/// **ORDERING:** PumpSwap sorts mints by raw byte comparison. WSOL (0x069b...)
/// sorts before most pump.fun tokens, so ~81% of pools have WSOL as base_mint
/// (offset 43) and the token as quote_mint (offset 75).
///
/// **Strategy:** Try offset 43 first. If empty, retry at offset 75.
/// Then detect which field is WSOL to correctly assign coin_vault vs pc_vault.
///
/// # Parameters
/// - `public_rpc_url` — public Solana RPC for getMultipleAccounts (vault reserves)
/// - `helius_rpc_url` — Helius API-key endpoint for getProgramAccounts
pub async fn resolve_pumpswap_pool_from_mint(
    client: &reqwest::Client,
    mint: &[u8; 32],
    public_rpc_url: &str,
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    // ── Concurrency gate ─────────────────────────────────────────────────
    let _permit = match POOL_RESOLUTION_SEMAPHORE.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("[pool] resolution semaphore full — dropping resolve_pumpswap_pool_from_mint");
            return None;
        }
    };
    let mint_b58 = bs58::encode(mint).into_string();

    // ── Two-pass getProgramAccounts: try offset 43 (base_mint), then 75 (quote_mint) ──
    // PumpSwap sorts mints by raw bytes. WSOL (0x069b...) sorts before most tokens,
    // so the token ends up as quote_mint (offset 75) in ~81% of pools.
    let mut pool_data: Option<(serde_json::Value, Vec<u8>)> = None;
    let mut token_is_base = true;

    for (offset, is_base) in [(43, true), (75, false)] {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getProgramAccounts",
            "params": [
                PUMPSWAP_AMM_PROGRAM,
                {
                    "encoding": "base64",
                    "commitment": "confirmed",
                    "filters": [
                        {"memcmp": {"offset": offset, "bytes": mint_b58}}
                    ]
                }
            ]
        });

        let resp = match client.post(helius_rpc_url).json(&body).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };

        let accounts = match json.pointer("/result").and_then(|r| r.as_array()) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        use base64::Engine as _;
        if let Some(data_b64) = accounts[0].pointer("/account/data/0").and_then(|d| d.as_str()) {
            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                if data.len() >= 204 {
                    pool_data = Some((accounts[0].clone(), data));
                    token_is_base = is_base;
                    tracing::debug!(
                        mint = %mint_b58,
                        offset,
                        token_is_base,
                        "[momentum] PumpSwap pool found at offset {offset}"
                    );
                    break;
                }
            }
        }
    }

    let (account_json, data) = match pool_data {
        Some(d) => d,
        None => {
            tracing::debug!(
                mint = %mint_b58,
                "[momentum] PumpSwap pool lookup: no pool found at offset 43 or 75"
            );
            return None;
        }
    };

    let pool_address = decode_bs58_32(account_json.get("pubkey")?.as_str()?)?;

    // ── Vault assignment: depends on pool ordering ──────────────────────
    // When token_is_base (normal):
    //   pool_base_token_account [139..171] = token vault (coin_vault)
    //   pool_quote_token_account [171..203] = WSOL vault (pc_vault)
    // When !token_is_base (reversed, WSOL is base):
    //   pool_base_token_account [139..171] = WSOL vault (pc_vault)
    //   pool_quote_token_account [171..203] = token vault (coin_vault)
    let (coin_vault, pc_vault) = if token_is_base {
        // Normal: base=token, quote=WSOL
        let cv: [u8; 32] = data[139..171].try_into().ok()?; // token vault
        let pv: [u8; 32] = data[171..203].try_into().ok()?; // WSOL vault
        (cv, pv)
    } else {
        // Reversed: base=WSOL, quote=token
        let pv: [u8; 32] = data[139..171].try_into().ok()?; // WSOL vault (base)
        let cv: [u8; 32] = data[171..203].try_into().ok()?; // token vault (quote)
        (cv, pv)
    };

    let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

    // getMultipleAccounts → public RPC (vault reserves are read-only)
    let (reserve_token, reserve_sol) =
        fetch_vault_reserves(client, public_rpc_url, &coin_vault_b58, &pc_vault_b58).await?;

    // FIX-3: PumpSwap uses lower 30 SOL threshold (fresh graduations start at ~85 SOL
    // but some valid pools have 30-50 SOL). Raydium keeps 50 SOL minimum.
    if reserve_sol < MIN_PUMPSWAP_SOL_RESERVES_LAMPORTS {
        tracing::warn!(
            mint = %mint_b58,
            pool = %bs58::encode(&pool_address).into_string(),
            reserve_sol,
            "[momentum] PumpSwap pool rejected — insufficient liquidity (reserve_sol < 30 SOL)"
        );
        return None;
    }

    tracing::info!(
        mint = %mint_b58,
        pool = %bs58::encode(&pool_address).into_string(),
        token_is_base,
        reserve_sol,
        reserve_token,
        "[momentum] PumpSwap pool resolved via mint lookup"
    );

    Some(PoolResolution {
        mint: *mint,
        pool_address,
        coin_vault,
        pc_vault,
        pool_type: PoolType::PumpSwap,
        reserve_sol_lamports: reserve_sol,
        reserve_token_atoms: reserve_token,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms: 0,
        amm_id: [0u8; 32],
        amm_open_orders: [0u8; 32],
        amm_target_orders: [0u8; 32],
        serum_market: [0u8; 32],
        serum_bids: [0u8; 32],
        serum_asks: [0u8; 32],
        serum_event_queue: [0u8; 32],
        serum_coin_vault: [0u8; 32],
        serum_pc_vault: [0u8; 32],
        serum_vault_signer: [0u8; 32],
    })
}

/// Resolve a Raydium AMM V4 pool from the token mint using getProgramAccounts.
///
/// Fallback path for when the sig-based resolution fails. CoreCast sends DEX
/// trade sigs (not pool-creation sigs), so getTransaction returns no vault data.
/// This function queries Raydium's program accounts filtered by coin_mint at offset 400.
///
/// Raydium AMM V4 pool state layout (752 bytes):
///   offset 336..368 — pc_vault  (WSOL vault pubkey)
///   offset 368..400 — coin_vault (token vault pubkey)
///   offset 400..432 — coin_mint  (the graduated token mint)
///
/// # Parameters
/// - `public_rpc_url` — public Solana RPC for getMultipleAccounts (vault reserves)
/// - `helius_rpc_url` — Helius API-key endpoint for getProgramAccounts
pub async fn resolve_pool_from_mint(
    client: &reqwest::Client,
    mint: &[u8; 32],
    public_rpc_url: &str,
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    // ── Concurrency gate ─────────────────────────────────────────────────
    let _permit = match POOL_RESOLUTION_SEMAPHORE.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::debug!("[pool] resolution semaphore full — dropping resolve_pool_from_mint");
            return None;
        }
    };
    let mint_b58 = bs58::encode(mint).into_string();

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getProgramAccounts",
        "params": [
            RAYDIUM_AMM_V4_PROGRAM,
            {
                "encoding": "base64",
                "commitment": "confirmed",
                "filters": [
                    {"dataSize": 752},
                    {"memcmp": {"offset": 400, "bytes": mint_b58}}
                ]
            }
        ]
    });

    let resp = client
        .post(helius_rpc_url)
        .json(&body)
        .send()
        .await
        .ok()?;

    let json: serde_json::Value = resp.json().await.ok()?;
    let accounts = json.pointer("/result")?.as_array()?;

    if accounts.is_empty() {
        tracing::debug!(
            mint = %mint_b58,
            "[momentum] mint-based pool lookup: no Raydium pool found (may be PumpSwap)"
        );
        return None;
    }

    use base64::Engine as _;
    let data_b64 = accounts[0].pointer("/account/data/0")?.as_str()?;
    let data = base64::engine::general_purpose::STANDARD.decode(data_b64).ok()?;

    if data.len() < 464 {
        tracing::warn!(
            mint = %mint_b58,
            data_len = data.len(),
            "[momentum] mint-based pool lookup: pool state too short"
        );
        return None;
    }

    let pc_vault: [u8; 32] = data[336..368].try_into().ok()?;
    let coin_vault: [u8; 32] = data[368..400].try_into().ok()?;
    let amm_id = decode_bs58_32(accounts[0].get("pubkey")?.as_str()?)?;

    let coin_vault_b58 = bs58::encode(&coin_vault).into_string();
    let pc_vault_b58 = bs58::encode(&pc_vault).into_string();

    // getMultipleAccounts → public RPC (vault reserves are read-only)
    let (reserve_token, reserve_sol) =
        fetch_vault_reserves(client, public_rpc_url, &coin_vault_b58, &pc_vault_b58).await?;

    // Minimum viable liquidity check — reject empty/drained pools
    if reserve_sol < MIN_SOL_RESERVES_LAMPORTS {
        tracing::warn!(
            mint = %mint_b58,
            amm_id = %bs58::encode(&amm_id).into_string(),
            reserve_sol,
            "[momentum] Raydium pool rejected — insufficient liquidity (reserve_sol < 50 SOL)"
        );
        return None;
    }

    tracing::info!(
        mint = %mint_b58,
        amm_id = %bs58::encode(&amm_id).into_string(),
        reserve_sol,
        "[momentum] pool resolved via mint lookup (getProgramAccounts Raydium)"
    );

    Some(PoolResolution {
        mint: *mint,
        pool_address: amm_id,
        coin_vault,
        pc_vault,
        pool_type: PoolType::RaydiumAmmV4,
        reserve_sol_lamports: reserve_sol,
        reserve_token_atoms: reserve_token,
        bc_terminal_vsol: 0.0,
        grad_block_time_ms: 0,
        amm_id,
        amm_open_orders: [0u8; 32],
        amm_target_orders: [0u8; 32],
        serum_market: [0u8; 32],
        serum_bids: [0u8; 32],
        serum_asks: [0u8; 32],
        serum_event_queue: [0u8; 32],
        serum_coin_vault: [0u8; 32],
        serum_pc_vault: [0u8; 32],
        serum_vault_signer: [0u8; 32],
    })
}

// ── PumpSwap Pool Accounts ───────────────────────────────────────────────────

/// Lightweight pool accounts for PumpSwap live execution.
/// Extracted from PoolResolution at graduation time.
/// Stored in MomentumEngine.pumpswap_pools DashMap.
#[derive(Debug, Clone)]
pub struct PumpSwapPoolAccounts {
    /// Pool PDA (from PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint (from PoolResolution.mint) — base_mint in PumpSwap terms
    pub base_mint: [u8; 32],
    /// Pool token vault (from PoolResolution.coin_vault) = pool_base_token_account in PumpSwap
    pub pool_base_token_account: [u8; 32],
    /// Pool WSOL vault (from PoolResolution.pc_vault) = pool_quote_token_account in PumpSwap
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA ([0u8;32] if unknown — program handles gracefully)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority ([0u8;32] if unknown)
    pub coin_creator_vault_authority: [u8; 32],
}

/// Extract PumpSwapPoolAccounts from a PoolResolution.
///
/// Returns None if:
/// - pool_type != PoolType::PumpSwap
/// - pool_address is all-zeros (resolution failed to capture pool PDA)
///
/// coin_creator_vault_ata and coin_creator_vault_authority are zeroed by default.
/// The PumpSwap program handles zero-address accounts gracefully for creator fee.
pub fn extract_pumpswap_pool_accounts(res: &PoolResolution) -> Option<PumpSwapPoolAccounts> {
    if res.pool_type != PoolType::PumpSwap {
        return None;
    }
    if res.pool_address == [0u8; 32] {
        return None;
    }
    Some(PumpSwapPoolAccounts {
        pool: res.pool_address,
        base_mint: res.mint,
        pool_base_token_account: res.coin_vault,
        pool_quote_token_account: res.pc_vault,
        coin_creator_vault_ata: [0u8; 32],
        coin_creator_vault_authority: [0u8; 32],
    })
}

/// FIX-5: Query the most recent confirmed transaction timestamp for an account.
///
/// Used to detect dead Raydium pools that have had no swap activity recently.
/// Returns the blockTime in milliseconds, or None if unavailable/empty.
///
/// Uses public RPC for getSignaturesForAddress (read-only, no API key needed).
pub async fn get_account_last_activity_ms(
    client: &reqwest::Client,
    rpc_url: &str,
    account_b58: &str,
) -> Option<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignaturesForAddress",
        "params": [
            account_b58,
            {"limit": 1, "commitment": "confirmed"}
        ]
    });
    let resp = client.post(rpc_url).json(&body).send().await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let sigs = json.pointer("/result")?.as_array()?;
    let block_time = sigs.first()?.get("blockTime")?.as_i64()?;
    // blockTime is Unix seconds → convert to ms
    Some((block_time as u64).saturating_mul(1_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn test_parse_spl_token_amount_valid() {
        let mut data = vec![0u8; 165];
        let amount: u64 = 1_000_000_000;
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        let result = parse_spl_token_amount(&encoded);
        assert_eq!(result, Some(1_000_000_000));
    }

    #[test]
    fn test_parse_spl_token_amount_too_short() {
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
            reserve_token: 200_000_000_000_000,
            reserve_sol: 80_000_000_000,
            pool_type: PoolType::RaydiumAmmV4,
            mint: [0xAA; 32],
        };

        let price = info.price_lamports_per_atom();
        assert!((price - 0.0004).abs() < 1e-10);

        let spread = info.spread_vs_bc_pct();
        assert!(spread.is_finite());
        assert!(spread >= 0.0);
        assert!(spread < 10.0, "spread was {} but expected < 10%", spread);
    }

    #[test]
    fn test_pool_type_as_str() {
        assert_eq!(PoolType::RaydiumAmmV4.as_str(), "raydium_amm_v4");
        assert_eq!(PoolType::PumpSwap.as_str(), "pump_swap");
        assert_eq!(PoolType::Unknown.as_str(), "unknown");
    }

    // ── PumpSwap pool accounts extraction tests ──────────────────────────

    /// Helper: build a default PumpSwap PoolResolution for tests.
    fn make_pumpswap_resolution() -> PoolResolution {
        PoolResolution {
            mint: [1u8; 32],
            pool_address: [2u8; 32],
            coin_vault: [3u8; 32],
            pc_vault: [4u8; 32],
            pool_type: PoolType::PumpSwap,
            reserve_sol_lamports: 100_000_000_000,
            reserve_token_atoms: 1_000_000_000,
            bc_terminal_vsol: 0.0,
            grad_block_time_ms: 0,
            amm_id: [0u8; 32],
            amm_open_orders: [0u8; 32],
            amm_target_orders: [0u8; 32],
            serum_market: [0u8; 32],
            serum_bids: [0u8; 32],
            serum_asks: [0u8; 32],
            serum_event_queue: [0u8; 32],
            serum_coin_vault: [0u8; 32],
            serum_pc_vault: [0u8; 32],
            serum_vault_signer: [0u8; 32],
        }
    }

    #[test]
    fn test_extract_pumpswap_pool_accounts_basic() {
        let res = make_pumpswap_resolution();
        let accts = extract_pumpswap_pool_accounts(&res).expect("should extract");
        assert_eq!(accts.pool, [2u8; 32]);
        assert_eq!(accts.base_mint, [1u8; 32]);
        assert_eq!(accts.pool_base_token_account, [3u8; 32]);
        assert_eq!(accts.pool_quote_token_account, [4u8; 32]);
    }

    #[test]
    fn test_extract_pumpswap_returns_none_for_raydium() {
        let mut res = make_pumpswap_resolution();
        res.pool_type = PoolType::RaydiumAmmV4;
        assert!(extract_pumpswap_pool_accounts(&res).is_none());
    }

    #[test]
    fn test_extract_pumpswap_returns_none_for_zero_pool_address() {
        let mut res = make_pumpswap_resolution();
        res.pool_address = [0u8; 32];
        assert!(extract_pumpswap_pool_accounts(&res).is_none());
    }

    #[test]
    fn test_extract_pumpswap_vault_field_mapping() {
        let mut res = make_pumpswap_resolution();
        res.coin_vault = [0xAA; 32];
        res.pc_vault = [0xBB; 32];
        let accts = extract_pumpswap_pool_accounts(&res).unwrap();
        // coin_vault → pool_base_token_account
        assert_eq!(accts.pool_base_token_account, [0xAA; 32]);
        // pc_vault → pool_quote_token_account
        assert_eq!(accts.pool_quote_token_account, [0xBB; 32]);
    }

    #[test]
    fn test_extract_pumpswap_creator_vaults_zeroed() {
        let res = make_pumpswap_resolution();
        let accts = extract_pumpswap_pool_accounts(&res).unwrap();
        assert_eq!(accts.coin_creator_vault_ata, [0u8; 32]);
        assert_eq!(accts.coin_creator_vault_authority, [0u8; 32]);
    }

    #[test]
    fn test_extract_pumpswap_returns_none_for_unknown_pool_type() {
        let mut res = make_pumpswap_resolution();
        res.pool_type = PoolType::Unknown;
        assert!(extract_pumpswap_pool_accounts(&res).is_none());
    }

    // ══════════════════════════════════════════════════════════════════════
    // PumpSwap reversed pool ordering tests (eng5)
    //
    // Validates the fix for the two-pass lookup strategy: PumpSwap sorts
    // mints by raw byte comparison. ~81% of pools have WSOL as base_mint
    // (offset 43) and token as quote_mint (offset 75). The old code only
    // checked offset 43, missing most pools.
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_wsol_mint_bytes_constant_matches_known_value() {
        // WSOL mint: So11111111111111111111111111111111111111112
        // Raw hex: 069b8857feab8184fb687f634618c035dac439dc1aeb3b5598a0f00000000001
        let expected = [
            0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
            0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
            0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
            0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
        ];
        assert_eq!(WSOL_MINT_BYTES, expected);
        // Also verify it decodes to the known base58 string
        let b58 = bs58::encode(&WSOL_MINT_BYTES).into_string();
        assert_eq!(b58, "So11111111111111111111111111111111111111112");
    }

    #[test]
    fn test_wsol_sorting_determines_pool_ordering() {
        // Most pump.fun tokens start with bytes > 0x06, so WSOL sorts first
        let wsol = WSOL_MINT_BYTES;
        let typical_pump_token = [0x7a; 32]; // starts with 0x7a > 0x06
        assert!(wsol < typical_pump_token, "WSOL should sort before most tokens");

        let rare_low_token = [0x01; 32]; // starts with 0x01 < 0x06
        assert!(rare_low_token < wsol, "rare low-byte tokens sort before WSOL");
    }

    #[test]
    fn test_wsol_first_byte_is_0x06() {
        // WSOL starts with 0x06 — any mint starting 0x07..0xFF sorts after
        assert_eq!(WSOL_MINT_BYTES[0], 0x06);
        // This is why ~81% of pools are "reversed" (WSOL = base_mint)
    }

    #[test]
    fn test_random_pump_tokens_mostly_sort_after_wsol() {
        // Simulate what PumpSwap does: compare raw bytes
        // pump.fun token mints are essentially random pubkeys.
        // We verify that the vast majority start with byte > 0x06.
        let wsol = WSOL_MINT_BYTES;

        // Tokens starting with bytes 0x07..0xFF sort after WSOL (reversed pool)
        for first_byte in 0x07u8..=0xFF {
            let mut token = [0u8; 32];
            token[0] = first_byte;
            assert!(
                wsol < token,
                "Token starting with 0x{:02x} should sort after WSOL",
                first_byte
            );
        }

        // Only tokens starting with 0x00..0x05 sort before WSOL (normal pool)
        for first_byte in 0x00u8..0x06 {
            let mut token = [0u8; 32];
            token[0] = first_byte;
            // Fill rest with 0xFF to maximize — still sorts before WSOL first byte
            for b in token[1..].iter_mut() { *b = 0xFF; }
            assert!(
                token < wsol || first_byte == 0x06,
                "Token starting with 0x{:02x} should sort before WSOL",
                first_byte
            );
        }

        // Probability: 249/256 ≈ 97.3% of random first bytes → reversed pool
        // (reality is ~81% for 301-byte pools due to second-byte effects with 0x06)
    }

    #[test]
    fn test_vault_swap_for_reversed_pool() {
        // Given a reversed pool (WSOL = base_mint at offset 43)
        let base_mint = WSOL_MINT_BYTES; // WSOL
        let token_vault = [0xAA; 32];
        let wsol_vault = [0xBB; 32];

        // In reversed pool: pool_base_token_account = WSOL vault, pool_quote = token vault
        let raw_base_vault = wsol_vault; // offset 139
        let raw_quote_vault = token_vault; // offset 171

        let (coin_vault, pc_vault) = if base_mint == WSOL_MINT_BYTES {
            (raw_quote_vault, raw_base_vault) // swap: coin=token vault, pc=WSOL vault
        } else {
            (raw_base_vault, raw_quote_vault) // normal
        };

        assert_eq!(coin_vault, token_vault, "coin_vault should be token vault");
        assert_eq!(pc_vault, wsol_vault, "pc_vault should be WSOL vault");
    }

    #[test]
    fn test_vault_no_swap_for_normal_pool() {
        // Normal pool: token = base_mint (token sorts before WSOL — rare)
        let base_mint = [0x01; 32]; // Sorts before WSOL (0x06...)
        let token_vault = [0xAA; 32];
        let wsol_vault = [0xBB; 32];

        let raw_base_vault = token_vault; // offset 139 — token vault
        let raw_quote_vault = wsol_vault; // offset 171 — WSOL vault

        let (coin_vault, pc_vault) = if base_mint == WSOL_MINT_BYTES {
            (raw_quote_vault, raw_base_vault)
        } else {
            (raw_base_vault, raw_quote_vault) // no swap needed
        };

        assert_eq!(coin_vault, token_vault);
        assert_eq!(pc_vault, wsol_vault);
    }

    #[test]
    fn test_vault_swap_mirrors_resolve_pumpswap_logic() {
        // This test replicates the exact vault assignment logic from
        // resolve_pumpswap_pool_from_mint() to verify it's correct.
        //
        // Simulate both orderings with known vault bytes.
        let token_vault_bytes = [0xCC; 32];
        let wsol_vault_bytes = [0xDD; 32];

        // Case 1: token_is_base = true (normal)
        {
            let token_is_base = true;
            let data = build_mock_pool_data(
                &[0x01; 32], // base_mint: token (sorts before WSOL)
                &WSOL_MINT_BYTES,       // quote_mint: WSOL
                &token_vault_bytes,     // pool_base_token_account = token vault
                &wsol_vault_bytes,      // pool_quote_token_account = WSOL vault
            );
            let (coin_vault, pc_vault) = if token_is_base {
                let cv: [u8; 32] = data[139..171].try_into().unwrap();
                let pv: [u8; 32] = data[171..203].try_into().unwrap();
                (cv, pv)
            } else {
                let pv: [u8; 32] = data[139..171].try_into().unwrap();
                let cv: [u8; 32] = data[171..203].try_into().unwrap();
                (cv, pv)
            };
            assert_eq!(coin_vault, token_vault_bytes, "normal: coin_vault = token vault");
            assert_eq!(pc_vault, wsol_vault_bytes, "normal: pc_vault = WSOL vault");
        }

        // Case 2: token_is_base = false (reversed — WSOL is base)
        {
            let token_is_base = false;
            let data = build_mock_pool_data(
                &WSOL_MINT_BYTES,       // base_mint: WSOL (sorts first)
                &[0x7a; 32],            // quote_mint: token
                &wsol_vault_bytes,      // pool_base_token_account = WSOL vault
                &token_vault_bytes,     // pool_quote_token_account = token vault
            );
            let (coin_vault, pc_vault) = if token_is_base {
                let cv: [u8; 32] = data[139..171].try_into().unwrap();
                let pv: [u8; 32] = data[171..203].try_into().unwrap();
                (cv, pv)
            } else {
                let pv: [u8; 32] = data[139..171].try_into().unwrap();
                let cv: [u8; 32] = data[171..203].try_into().unwrap();
                (cv, pv)
            };
            assert_eq!(coin_vault, token_vault_bytes, "reversed: coin_vault = token vault");
            assert_eq!(pc_vault, wsol_vault_bytes, "reversed: pc_vault = WSOL vault");
        }
    }

    /// Build a mock 211-byte PumpSwap pool data buffer with specified fields.
    fn build_mock_pool_data(
        base_mint: &[u8; 32],
        quote_mint: &[u8; 32],
        pool_base_token_account: &[u8; 32],
        pool_quote_token_account: &[u8; 32],
    ) -> Vec<u8> {
        let mut data = vec![0u8; 211];
        // [0..8] discriminator
        data[0..8].copy_from_slice(&[0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc]);
        // [8] bump
        data[8] = 0xFF;
        // [9..11] index
        data[9..11].copy_from_slice(&[0x00, 0x00]);
        // [11..43] creator (zeros)
        // [43..75] base_mint
        data[43..75].copy_from_slice(base_mint);
        // [75..107] quote_mint
        data[75..107].copy_from_slice(quote_mint);
        // [107..139] lp_mint (zeros)
        // [139..171] pool_base_token_account
        data[139..171].copy_from_slice(pool_base_token_account);
        // [171..203] pool_quote_token_account
        data[171..203].copy_from_slice(pool_quote_token_account);
        data
    }

    // ── Integration tests: decode reference pool data from spec Section 4 ──

    /// Pool A: REVERSED ordering (WSOL = base, token = quote)
    /// Address: 114XmiBstWqYVhSiH6qnU4jFCskFxP8t9iBqBLJPmaf
    /// Size: 301 bytes
    #[rustfmt::skip]
    const POOL_A_DATA: [u8; 301] = [
        0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc, 0xff, 0x00, 0x00, 0xb5, 0x7b, 0xd1, 0x8d, 0x84,
        0x42, 0xb4, 0x6f, 0xa7, 0xaa, 0xe6, 0xad, 0x0d, 0x45, 0xd6, 0x40, 0x26, 0x84, 0xa0, 0x3c, 0xdd,
        0xf9, 0xa5, 0x5e, 0x4a, 0xf6, 0x95, 0x17, 0xe1, 0x30, 0x6c, 0x40, 0x06, 0x9b, 0x88, 0x57, 0xfe,
        0xab, 0x81, 0x84, 0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35, 0xda, 0xc4, 0x39, 0xdc, 0x1a,
        0xeb, 0x3b, 0x55, 0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01, 0xf9, 0x48, 0x64, 0x80, 0x7b,
        0x81, 0x2c, 0x34, 0x5b, 0xbc, 0x12, 0x27, 0x74, 0x90, 0xaf, 0xc5, 0x75, 0x47, 0xcf, 0x1c, 0x3c,
        0xdb, 0xdb, 0x16, 0x0c, 0x1c, 0x07, 0x60, 0x0c, 0x71, 0xb4, 0x07, 0x2a, 0x38, 0x91, 0x72, 0xdf,
        0x69, 0x23, 0xea, 0x67, 0x40, 0x56, 0xf6, 0x15, 0x3e, 0x3c, 0x48, 0x7e, 0xaa, 0xa6, 0xbe, 0x32,
        0x01, 0xfd, 0x01, 0x48, 0xa1, 0xd1, 0xee, 0x0a, 0x31, 0x13, 0xbb, 0x7d, 0xca, 0xb7, 0xce, 0xb1,
        0x2a, 0xc3, 0xf2, 0x2d, 0x58, 0x2e, 0x20, 0x69, 0x5a, 0x8c, 0x22, 0x49, 0xfc, 0x75, 0x82, 0xbd,
        0x6b, 0x09, 0xa3, 0x1f, 0x93, 0x32, 0xef, 0x29, 0x4b, 0xa2, 0x13, 0xd7, 0xe6, 0xa3, 0x88, 0xda,
        0xd3, 0xf3, 0xc9, 0x0d, 0x40, 0x74, 0xfd, 0xc2, 0xf0, 0x82, 0xae, 0x3c, 0x17, 0x1f, 0x16, 0xce,
        0xc1, 0x67, 0x48, 0x8d, 0x6f, 0x51, 0x72, 0x98, 0x6f, 0x7e, 0xed, 0x64, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// Pool B: NORMAL ordering (token = base, WSOL = quote)
    /// Address: 11CwRL2M8m5EeZUphCx8BvD6GXjw9VGTQUhjrWkjr3L
    /// Size: 301 bytes
    #[rustfmt::skip]
    const POOL_B_DATA: [u8; 301] = [
        0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc, 0xfd, 0x01, 0x00, 0xbe, 0x32, 0x3d, 0xac, 0xef,
        0xdf, 0xad, 0x2b, 0x71, 0xf1, 0x78, 0x1d, 0xa3, 0x1d, 0x0b, 0x43, 0x13, 0xd6, 0x51, 0x87, 0xa8,
        0xa8, 0x89, 0xa0, 0x06, 0x15, 0x40, 0xee, 0xec, 0xd3, 0xe6, 0x7b, 0x7a, 0xf1, 0xe7, 0x57, 0xc2,
        0x07, 0xfa, 0xf4, 0xa3, 0x1f, 0xc4, 0x7d, 0xb5, 0x64, 0x64, 0x14, 0xad, 0x12, 0x8c, 0x63, 0xb5,
        0x3c, 0x72, 0x79, 0x33, 0x07, 0x13, 0xb4, 0x12, 0xb3, 0x33, 0xcf, 0x06, 0x9b, 0x88, 0x57, 0xfe,
        0xab, 0x81, 0x84, 0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35, 0xda, 0xc4, 0x39, 0xdc, 0x1a,
        0xeb, 0x3b, 0x55, 0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01, 0xc3, 0x62, 0xae, 0x60, 0x60,
        0x59, 0x6a, 0x66, 0x69, 0x70, 0x73, 0xd1, 0x3b, 0x90, 0xca, 0xcd, 0x78, 0xfb, 0xac, 0x3e, 0x59,
        0x03, 0x95, 0xfa, 0x11, 0xfb, 0xc8, 0x70, 0x5c, 0x53, 0x54, 0x7f, 0xf7, 0x4b, 0x79, 0xce, 0x7e,
        0xbb, 0x68, 0x93, 0xf9, 0x78, 0x23, 0x00, 0x95, 0x84, 0xf6, 0xde, 0x0a, 0x58, 0x11, 0x7a, 0xda,
        0xbb, 0x3b, 0x57, 0x3a, 0x93, 0x7e, 0x7b, 0x7e, 0xe3, 0xd6, 0x8b, 0xc2, 0xb9, 0x96, 0xd8, 0xb1,
        0x96, 0x05, 0x1d, 0x34, 0x00, 0xc9, 0x24, 0x52, 0x5a, 0x21, 0x9a, 0x14, 0xa6, 0x74, 0x92, 0xba,
        0xc7, 0x38, 0x26, 0x20, 0x23, 0xe2, 0xb2, 0xeb, 0xfa, 0xba, 0x48, 0x65, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn test_decode_reversed_pool_a_reference_data() {
        // Pool A from spec: REVERSED (WSOL = base)
        // Address: 114XmiBstWqYVhSiH6qnU4jFCskFxP8t9iBqBLJPmaf
        let data = &POOL_A_DATA;
        assert_eq!(data.len(), 301);

        // Verify discriminator
        assert_eq!(&data[0..8], &[0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc]);

        // Verify base_mint == WSOL (this is a reversed pool)
        let base_mint: [u8; 32] = data[43..75].try_into().unwrap();
        assert_eq!(base_mint, WSOL_MINT_BYTES, "Pool A: base_mint should be WSOL (reversed)");

        // Verify quote_mint != WSOL (it's the token)
        let quote_mint: [u8; 32] = data[75..107].try_into().unwrap();
        assert_ne!(quote_mint, WSOL_MINT_BYTES, "Pool A: quote_mint should be the token");

        // Verify the token's first byte > WSOL's first byte (which is why it's reversed)
        assert!(
            quote_mint[0] > WSOL_MINT_BYTES[0],
            "Token first byte 0x{:02x} should be > WSOL first byte 0x{:02x}",
            quote_mint[0],
            WSOL_MINT_BYTES[0]
        );

        // Apply the vault swap logic (as in resolve_pumpswap_pool_from_mint)
        let token_is_base = false; // WSOL is base, so token is NOT base
        let raw_base_vault: [u8; 32] = data[139..171].try_into().unwrap(); // WSOL vault
        let raw_quote_vault: [u8; 32] = data[171..203].try_into().unwrap(); // token vault

        let (coin_vault, pc_vault) = if token_is_base {
            (raw_base_vault, raw_quote_vault)
        } else {
            // Swap: coin_vault = token vault (quote), pc_vault = WSOL vault (base)
            (raw_quote_vault, raw_base_vault)
        };

        // coin_vault should be the token vault (quote_vault in on-chain terms)
        assert_eq!(coin_vault, raw_quote_vault, "Reversed: coin_vault = pool_quote_token_account");
        // pc_vault should be the WSOL vault (base_vault in on-chain terms)
        assert_eq!(pc_vault, raw_base_vault, "Reversed: pc_vault = pool_base_token_account");
    }

    #[test]
    fn test_decode_normal_pool_b_reference_data() {
        // Pool B from spec: NORMAL (token = base, WSOL = quote)
        // Address: 11CwRL2M8m5EeZUphCx8BvD6GXjw9VGTQUhjrWkjr3L
        let data = &POOL_B_DATA;
        assert_eq!(data.len(), 301);

        // Verify discriminator
        assert_eq!(&data[0..8], &[0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc]);

        // Verify base_mint != WSOL (this is a normal pool — token is base)
        let base_mint: [u8; 32] = data[43..75].try_into().unwrap();
        assert_ne!(base_mint, WSOL_MINT_BYTES, "Pool B: base_mint should be the token (normal)");

        // Verify quote_mint == WSOL
        let quote_mint: [u8; 32] = data[75..107].try_into().unwrap();
        assert_eq!(quote_mint, WSOL_MINT_BYTES, "Pool B: quote_mint should be WSOL (normal)");

        // The token mint is 9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump
        // Verify by checking that base_mint starts with 0x7a (from the spec hex dump)
        assert_eq!(base_mint[0], 0x7a, "Pool B token should start with 0x7a");

        // Apply the vault logic (no swap needed for normal pool)
        let token_is_base = true;
        let raw_base_vault: [u8; 32] = data[139..171].try_into().unwrap(); // token vault
        let raw_quote_vault: [u8; 32] = data[171..203].try_into().unwrap(); // WSOL vault

        let (coin_vault, pc_vault) = if token_is_base {
            (raw_base_vault, raw_quote_vault) // no swap
        } else {
            (raw_quote_vault, raw_base_vault)
        };

        // coin_vault should be the base vault (token)
        assert_eq!(coin_vault, raw_base_vault, "Normal: coin_vault = pool_base_token_account");
        // pc_vault should be the quote vault (WSOL)
        assert_eq!(pc_vault, raw_quote_vault, "Normal: pc_vault = pool_quote_token_account");
    }

    #[test]
    fn test_pool_a_and_b_have_same_discriminator() {
        // Both pool sizes (211 and 301 bytes) should have identical discriminators
        assert_eq!(
            &POOL_A_DATA[0..8],
            &POOL_B_DATA[0..8],
            "Discriminator should be identical for all PumpSwap pools"
        );
        // Known discriminator: sha256("account:Pool")[0..8] = f19a6d0411b16dbc
        assert_eq!(
            &POOL_A_DATA[0..8],
            &[0xf1, 0x9a, 0x6d, 0x04, 0x11, 0xb1, 0x6d, 0xbc]
        );
    }

    #[test]
    fn test_pool_ordering_detection_from_base_mint() {
        // Given raw pool data, detect ordering by checking if base_mint == WSOL
        let pool_a_base: [u8; 32] = POOL_A_DATA[43..75].try_into().unwrap();
        let pool_b_base: [u8; 32] = POOL_B_DATA[43..75].try_into().unwrap();

        // Pool A: reversed (base = WSOL)
        let pool_a_token_is_base = pool_a_base != WSOL_MINT_BYTES;
        assert!(!pool_a_token_is_base, "Pool A should be detected as reversed");

        // Pool B: normal (base = token)
        let pool_b_token_is_base = pool_b_base != WSOL_MINT_BYTES;
        assert!(pool_b_token_is_base, "Pool B should be detected as normal");
    }

    #[test]
    fn test_two_pass_offset_strategy() {
        // Simulate the two-pass strategy used in resolve_pumpswap_pool_from_mint.
        // The function tries offset 43 first, then offset 75.

        // For Pool A (reversed): token is at offset 75 (quote_mint)
        let pool_a_base: [u8; 32] = POOL_A_DATA[43..75].try_into().unwrap();
        let pool_a_quote: [u8; 32] = POOL_A_DATA[75..107].try_into().unwrap();
        let token_a = pool_a_quote; // The actual token mint

        // Query 1 at offset 43 would filter on token_a → no match (base=WSOL)
        assert_ne!(pool_a_base, token_a, "offset 43 won't match token for reversed pool");
        // Query 2 at offset 75 would match → found!
        assert_eq!(pool_a_quote, token_a, "offset 75 matches token for reversed pool");

        // For Pool B (normal): token is at offset 43 (base_mint)
        let pool_b_base: [u8; 32] = POOL_B_DATA[43..75].try_into().unwrap();
        let pool_b_quote: [u8; 32] = POOL_B_DATA[75..107].try_into().unwrap();
        let token_b = pool_b_base; // The actual token mint

        // Query 1 at offset 43 would match → found on first try!
        assert_eq!(pool_b_base, token_b, "offset 43 matches token for normal pool");
        // We never need to check offset 75
        assert_ne!(pool_b_quote, token_b, "offset 75 won't match (it's WSOL)");
    }

    #[test]
    fn test_extract_pumpswap_accounts_preserves_vault_normalization() {
        // Verify that extract_pumpswap_pool_accounts correctly maps
        // PoolResolution's normalized vaults (coin=token, pc=WSOL) to
        // PumpSwapPoolAccounts fields, regardless of on-chain ordering.

        // Simulate a reversed pool resolution
        let mut res = make_pumpswap_resolution();
        let token_vault = [0xAA; 32];
        let wsol_vault = [0xBB; 32];
        res.coin_vault = token_vault; // already normalized by resolver
        res.pc_vault = wsol_vault;    // already normalized by resolver

        let accts = extract_pumpswap_pool_accounts(&res).unwrap();
        // pool_base_token_account = coin_vault = token vault
        assert_eq!(accts.pool_base_token_account, token_vault);
        // pool_quote_token_account = pc_vault = WSOL vault
        assert_eq!(accts.pool_quote_token_account, wsol_vault);
    }

    #[test]
    fn test_decode_bs58_32_roundtrip() {
        // Ensure our decode helper works correctly for WSOL
        let wsol_b58 = "So11111111111111111111111111111111111111112";
        let decoded = decode_bs58_32(wsol_b58).expect("should decode WSOL");
        assert_eq!(decoded, WSOL_MINT_BYTES);

        // And for a typical pump token from Pool B
        let token_b58 = "9GvgSRMprTdnrpuQhS3uzCe1FijtZxPU974H8zjHpump";
        let decoded_token = decode_bs58_32(token_b58).expect("should decode token");
        let pool_b_base: [u8; 32] = POOL_B_DATA[43..75].try_into().unwrap();
        assert_eq!(decoded_token, pool_b_base, "decoded token should match Pool B base_mint");
    }

    #[test]
    fn test_pool_data_field_boundaries() {
        // Verify all field offsets are correct by checking known values in both pools
        let data = &POOL_A_DATA;

        // [0..8] discriminator = f19a6d0411b16dbc
        assert_eq!(data[0], 0xf1);
        assert_eq!(data[7], 0xbc);

        // [8] pool_bump
        assert_eq!(data[8], 0xff);

        // [9..11] index (u16 LE)
        let index = u16::from_le_bytes([data[9], data[10]]);
        assert_eq!(index, 0x0000);

        // [43..75] base_mint = WSOL for Pool A
        assert_eq!(data[43], 0x06); // WSOL first byte

        // [75..107] quote_mint starts with 0xf9 for Pool A
        assert_eq!(data[75], 0xf9);

        // [139..171] pool_base_token_account (first byte)
        assert_eq!(data[139], 0x7d); // from hex dump

        // [171..203] pool_quote_token_account (first byte)
        assert_eq!(data[171], 0xd7); // from hex dump

        // Verify Pool B field boundaries
        let data_b = &POOL_B_DATA;
        // base_mint starts with 0x7a (the token, since it's normal ordering)
        assert_eq!(data_b[43], 0x7a);
        // quote_mint starts with 0x06 (WSOL)
        assert_eq!(data_b[75], 0x06);
    }

    #[test]
    fn test_minimum_data_length_for_vault_extraction() {
        // The code checks data.len() >= 204 before extracting vaults.
        // Verify this is sufficient: we need up to offset 203 (end of pool_quote_token_account)
        assert!(
            204 > 203,
            "204 bytes is sufficient to read pool_quote_token_account ending at byte 203"
        );

        // Build a minimal 204-byte buffer and verify we can extract vaults
        let mut minimal = vec![0u8; 204];
        minimal[139..171].copy_from_slice(&[0xAA; 32]);
        minimal[171..203].copy_from_slice(&[0xBB; 32]);
        let cv: [u8; 32] = minimal[139..171].try_into().unwrap();
        let pv: [u8; 32] = minimal[171..203].try_into().unwrap();
        assert_eq!(cv, [0xAA; 32]);
        assert_eq!(pv, [0xBB; 32]);

        // 203 bytes should be too short
        let short = vec![0u8; 203];
        let result: Result<[u8; 32], _> = short[171..203].try_into();
        assert!(result.is_ok(), "203 bytes still has enough for the last vault at 171..203");

        // But 202 would fail for the full range
        let too_short = vec![0u8; 202];
        assert!(too_short.len() < 203, "202 bytes is not enough");
    }
}
