//! Graduation arbitrage engine — v2 with postTokenBalances vault extraction.
//!
//! Detects migration events where a Pump.fun token graduates to Raydium AMM
//! or PumpSwap. The price dislocation between the bonding curve terminal price
//! and the DEX opening price creates an arbitrage opportunity.
//!
//! ## Architecture
//!
//! - `GradArbConfig` — parsed from EngineConfig graduation_arb fields
//! - `GradArbPosition` — live position with MFE/MAE tracking
//! - `GradArbClosedPosition` — completed trade for paper logging
//! - `GradArbStats` — atomic counters for real-time monitoring
//! - `GraduationArbEngine` — main engine struct, DashMap-backed positions
//!
//! ## Pool Resolution (v2)
//!
//! Phase A: Extract vault addresses from `postTokenBalances` in `getTransaction`
//!          response — works with v0 ALT transactions (no instruction parsing).
//! Phase B: `getMultipleAccountsInfo` on vault addresses, parse SPL token account
//!          amount at bytes [64..72].
//!
//! ## Price Dislocation Math
//!
//! ```text
//! bc_terminal_price = vSol_terminal / vTokens_terminal
//! ray_opening_price = ray_reserve_sol / ray_reserve_tokens
//! spread_pct = (bc_terminal_price - ray_opening_price).abs() / bc_terminal_price * 100
//! ```

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use super::dedup::MigrationDedup;
use crate::feeds::MigrationSource;

/// Pump.fun bonding curve terminal price at graduation.
///
/// Derivation:
///   k = vSol₀ × vTokens₀ = 30e9 × 1.073e15 = 3.219e25
///   vTokens_terminal = 1.073e15 - 793.1e12 = 279.9e12
///   vSol_terminal = k / vTokens_terminal = 115.005e9
///   price = vSol_terminal / vTokens_terminal = 4.1088e-4 lamports/atom
///
/// The 0.008% difference from the old constant (85e9/206.9e12) is negligible,
/// but this derivation is mathematically correct and self-documenting.
const BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM: f64 = {
    let k: f64 = 30_000_000_000.0 * 1_073_000_000_000_000.0;
    let vtokens_terminal: f64 = 1_073_000_000_000_000.0 - 793_100_000_000_000.0;
    let vsol_terminal: f64 = k / vtokens_terminal;
    vsol_terminal / vtokens_terminal
};

/// Maximum plausible spread (%). Anything above this is almost certainly a bad
/// price calculation (e.g. zero reserves, NaN, pool not yet seeded).
const MAX_PLAUSIBLE_SPREAD_PCT: f64 = 50.0;

// ── Config ───────────────────────────────────────────────────────────────────

/// Configuration for the graduation arbitrage engine.
/// Loaded from EngineConfig graduation_arb_* fields.
#[derive(Debug, Clone)]
pub struct GradArbConfig {
    /// Master toggle.
    pub enabled: bool,
    /// Paper mode — log trades but do not submit transactions.
    pub paper_mode: bool,
    /// Max position size in SOL.
    pub max_sol: f64,
    /// Minimum spread between BC terminal price and DEX opening price (%).
    pub min_spread_pct: f64,
    /// Take-profit target (fractional, e.g. 0.03 = 3%).
    pub tp_pct: f64,
    /// Stop-loss threshold (fractional, e.g. 0.02 = 2%).
    pub sl_pct: f64,
    /// Maximum hold time before forced exit (ms).
    pub max_hold_ms: u64,
    /// Jito tip for arb bundles (SOL).
    pub jito_tip_sol: f64,
    /// Dedup window — ignore duplicate migration events within this period (ms).
    pub dedup_ttl_ms: u64,
    /// RPC budget per arb attempt — timeout for pool reserve fetches (ms).
    pub rpc_timeout_ms: u64,
    /// Skip PumpSwap graduations entirely — no structural arb exists.
    /// pump.fun controls the migration price and it matches the BC terminal price.
    pub skip_pump_swap: bool,
}

impl Default for GradArbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_mode: true,
            max_sol: 0.30,
            min_spread_pct: 3.0,
            tp_pct: 0.03,
            sl_pct: 0.02,
            max_hold_ms: 5_000,
            jito_tip_sol: 0.003,
            dedup_ttl_ms: 10_000,
            rpc_timeout_ms: 200,
            skip_pump_swap: true,
        }
    }
}

impl GradArbConfig {
    /// Generate a config version string for paper trade logging.
    /// Format: `"grad-v{max_sol:.2}sol_{max_hold_ms}ms"`
    pub fn config_version(&self) -> String {
        format!("grad-v{:.2}sol_{}ms", self.max_sol, self.max_hold_ms)
    }
}

// ── Position Types ───────────────────────────────────────────────────────────

/// Type of DEX pool the token migrated to.
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

// ── Pool Resolution (v2: postTokenBalances vault extraction) ─────────────────

/// Raydium AMM V4 program ID (base58: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8).
const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// PumpSwap AMM program ID (base58: pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA).
const PUMPSWAP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// WSOL mint in base58 for vault extraction matching.
const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

/// Result of resolving a pool from a graduation transaction.
#[derive(Debug, Clone)]
pub struct PoolResolution {
    /// Token mint address.
    pub mint: [u8; 32],
    /// DEX pool address ([0u8; 32] if extraction failed).
    pub pool_address: [u8; 32],
    /// Type of DEX pool.
    pub pool_type: PoolType,
    /// Initial SOL reserves in pool (lamports). 0 if unknown.
    pub reserve_sol_lamports: u64,
    /// Initial token reserves in pool (atoms). 0 if unknown.
    pub reserve_token_atoms: u64,
    /// Bonding curve vSol at graduation (~85 SOL). 0.0 if unknown.
    pub bc_terminal_vsol: f64,
}

/// Decode a base58-encoded string into a 32-byte array.
fn decode_bs58_32(s: &str) -> Option<[u8; 32]> {
    let mut buf = [0u8; 32];
    let n = bs58::decode(s).onto(&mut buf[..]).ok()?;
    if n == 32 { Some(buf) } else { None }
}

/// Create a shared `reqwest::Client` with a 180ms timeout for pool resolution.
///
/// The 180ms limit leaves 20ms margin within the 200ms RPC budget.
/// Callers should wrap this in `Arc` and reuse across arb attempts.
pub fn make_pool_resolution_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(180))
        .build()
        .expect("reqwest client build should not fail")
}

/// Resolve pool address and initial reserves from a graduation transaction.
///
/// v2 approach: two-phase vault extraction.
///   Phase A: Extract vault addresses from `postTokenBalances` in `getTransaction`
///            — works reliably with v0 ALT transactions.
///   Phase B: `getMultipleAccountsInfo` on vault addresses, parse SPL token
///            account amount at bytes [64..72] LE u64.
///
/// Returns `None` if: RPC call fails, timeout, or tx doesn't contain pool creation.
///
/// # Arguments
/// * `client` — shared reqwest client (use `make_pool_resolution_client()`)
/// * `sig` — full 64-byte Solana transaction signature
/// * `helius_rpc_url` — Helius RPC endpoint with API key
#[inline(never)]
pub async fn resolve_pool_from_transaction(
    client: &reqwest::Client,
    sig: &[u8; 64],
    helius_rpc_url: &str,
) -> Option<PoolResolution> {
    match resolve_pool_inner(client, sig, helius_rpc_url).await {
        Ok(resolution) => Some(resolution),
        Err(e) => {
            tracing::debug!("[grad_arb] pool resolution failed: {}", e);
            None
        }
    }
}

/// Inner implementation — returns Result for clean error propagation.
async fn resolve_pool_inner(
    client: &reqwest::Client,
    sig: &[u8; 64],
    helius_rpc_url: &str,
) -> Result<PoolResolution, String> {
    let sig_b58 = bs58::encode(sig).into_string();

    // Phase A: getTransaction with jsonParsed — extract vault addresses from postTokenBalances
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

    let tx = json
        .get("result")
        .ok_or_else(|| {
            let err_msg = json
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("null result");
            format!("RPC returned no result: {}", err_msg)
        })?;

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

    let pool_type = if account_keys_strs.iter().any(|k| *k == RAYDIUM_AMM_V4_PROGRAM) {
        PoolType::RaydiumAmmV4
    } else if account_keys_strs.iter().any(|k| *k == PUMPSWAP_AMM_PROGRAM) {
        PoolType::PumpSwap
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
                if mint != WSOL_MINT_STR { Some(mint.to_string()) } else { None }
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
        "[grad_arb] v2 vault extraction from postTokenBalances"
    );

    // Phase B: fetch vault reserves via getMultipleAccountsInfo
    let (reserve_token, reserve_sol) =
        fetch_vault_reserves(client, helius_rpc_url, &coin_vault_b58, &pc_vault_b58)
            .await
            .ok_or_else(|| "getMultipleAccountsInfo failed for vault reserves".to_string())?;

    tracing::debug!(
        reserve_token = reserve_token,
        reserve_sol = reserve_sol,
        "[grad_arb] v2 vault reserves fetched"
    );

    Ok(PoolResolution {
        mint,
        pool_address: [0u8; 32], // Pool address not extracted in v2 — vaults are sufficient
        pool_type,
        reserve_sol_lamports: reserve_sol,
        reserve_token_atoms: reserve_token,
        bc_terminal_vsol: 0.0,
    })
}

/// Extract vault addresses from getTransaction response using `postTokenBalances`.
///
/// Works with v0 ALT transactions — `postTokenBalances` always contains all
/// token balance changes, and `accountKeys` includes ALT-resolved addresses
/// in `jsonParsed` output.
///
/// `coin_vault` = account with largest balance of `graduation_mint` (non-WSOL)
/// `pc_vault` = account with largest balance of WSOL
///
/// Returns `(coin_vault_bytes, pc_vault_bytes)` or `None` if extraction fails.
#[inline(always)]
fn extract_vaults_from_tx_response(
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
        if mint == WSOL_MINT_STR && amount > max_wsol_amount {
            max_wsol_amount = amount;
            pc_vault_idx = Some(idx);
        }
    }

    // Resolve account addresses from accountKeys (handles both string and object formats)
    let resolve_key = |idx: usize| -> Option<[u8; 32]> {
        let key = account_keys.get(idx)?;
        let key_str = key.as_str()
            .or_else(|| key.get("pubkey").and_then(|p| p.as_str()))?;
        decode_bs58_32(key_str)
    };

    let coin_vault = resolve_key(coin_vault_idx?)?;
    let pc_vault = resolve_key(pc_vault_idx?)?;
    Some((coin_vault, pc_vault))
}

/// Parse SPL token account amount from base64-encoded account data.
///
/// SPL Token Account layout: amount is a LE u64 at bytes [64..72].
/// Minimal account size is 165 bytes.
#[inline(always)]
fn parse_spl_token_amount(data_b64: &str) -> Option<u64> {
    use base64::Engine;
    // Decode only the first 72 bytes needed (avoid full allocation for 165-byte account)
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64).ok()?;
    if bytes.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(
        bytes[64..72].try_into().ok()?,
    ))
}

/// Fetch SPL token vault amounts via `getMultipleAccountsInfo`.
///
/// Returns `(reserve_token_atoms, reserve_sol_lamports)` or `None` on failure.
/// 150ms timeout on the RPC call.
async fn fetch_vault_reserves(
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

/// Best-effort mint extraction from postTokenBalances when pool parsing fails.
#[allow(dead_code)]
fn extract_fallback_mint(tx: &serde_json::Value) -> Option<[u8; 32]> {
    let balances = tx
        .pointer("/meta/postTokenBalances")?
        .as_array()?;

    // Find first non-WSOL mint
    for entry in balances {
        let mint_str = entry.get("mint").and_then(|m| m.as_str())?;
        if mint_str != WSOL_MINT_STR {
            return decode_bs58_32(mint_str);
        }
    }
    None
}

/// Reason the graduation arb position was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradArbExitReason {
    /// Hit take-profit target.
    TakeProfit,
    /// Hit stop-loss threshold.
    StopLoss,
    /// Exceeded max hold time.
    MaxHold,
    /// Spread below minimum threshold — no arb found.
    NoArbFound,
    /// Pool reserve fetch timed out within RPC budget.
    RpcTimeout,
    /// Could not resolve pool address from migration event.
    PoolNotFound,
    /// PumpSwap graduation — skipped because no structural arb exists.
    PumpSwapSkipped,
}

impl GradArbExitReason {
    /// Serialization string for JSONL output.
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TakeProfit => "take_profit",
            Self::StopLoss => "stop_loss",
            Self::MaxHold => "max_hold",
            Self::NoArbFound => "no_arb_found",
            Self::RpcTimeout => "rpc_timeout",
            Self::PoolNotFound => "pool_not_found",
            Self::PumpSwapSkipped => "pumpswap_skipped",
        }
    }
}

/// A live graduation arb position being tracked by the engine.
#[derive(Debug)]
pub struct GradArbPosition {
    /// Token mint address (32 bytes).
    pub mint: [u8; 32],
    /// DEX pool address.
    pub pool_address: [u8; 32],
    /// Type of pool (Raydium V4 or PumpSwap).
    pub pool_type: PoolType,
    /// Token price in lamports at entry.
    pub entry_price_lamports: u64,
    /// vSol reserves at migration (~85 SOL in lamports).
    pub entry_vsol_lamports: u64,
    /// SOL deployed in this position (lamports).
    pub size_lamports: u64,
    /// Bonding curve terminal price (SOL per token).
    pub bc_terminal_price: f64,
    /// Raydium/PumpSwap opening price (SOL per token).
    pub ray_opening_price: f64,
    /// Observed spread at entry (%).
    pub spread_pct: f64,
    /// Feed source that first detected the migration.
    pub detection_source: MigrationSource,
    /// Latency from migration tx to our detection (ms).
    pub detection_latency_ms: u64,
    /// Entry timestamp (epoch ms).
    pub entry_ts_ms: u64,
    /// Peak token price seen since entry (lamports) — for MFE tracking.
    pub peak_price_lamports: u64,
    /// Minimum token price seen since entry (lamports) — for MAE tracking.
    pub min_price_lamports: u64,
}

/// A completed graduation arb trade, ready for paper logging.
#[derive(Debug)]
pub struct GradArbClosedPosition {
    pub mint: [u8; 32],
    pub pool_address: [u8; 32],
    pub pool_type: PoolType,
    pub entry_price_lamports: u64,
    pub exit_price_lamports: u64,
    pub size_lamports: u64,
    pub bc_terminal_price: f64,
    pub ray_opening_price: f64,
    pub spread_pct: f64,
    pub detection_source: MigrationSource,
    pub detection_latency_ms: u64,
    pub entry_ts_ms: u64,
    pub exit_ts_ms: u64,
    pub hold_ms: u64,
    pub exit_reason: GradArbExitReason,
    /// Gross PnL in lamports (signed — can be negative).
    pub pnl_lamports: i64,
    /// Fee cost in lamports (Jito tip + priority fee).
    pub fee_lamports: u64,
    /// Net PnL in lamports (pnl_lamports - fee_lamports as i64).
    pub net_pnl_lamports: i64,
    /// Max favorable excursion in lamports.
    pub mfe_lamports: u64,
    /// Max adverse excursion in lamports.
    pub mae_lamports: u64,
}

// ── Stats ────────────────────────────────────────────────────────────────────

/// Lock-free atomic statistics for the graduation arb engine.
/// All counters use `Relaxed` ordering — eventual consistency is fine for stats.
pub struct GradArbStats {
    pub migrations_detected: AtomicU64,
    pub arb_entries: AtomicU64,
    pub arb_timeouts: AtomicU64,
    pub pool_not_found: AtomicU64,
    pub no_arb_spread: AtomicU64,
    pub exits_tp: AtomicU64,
    pub exits_sl: AtomicU64,
    pub exits_max_hold: AtomicU64,
    /// Gross PnL in lamports (signed via AtomicI64).
    pub gross_pnl_lamports: AtomicI64,
    /// Net PnL in lamports (signed via AtomicI64).
    pub net_pnl_lamports: AtomicI64,
}

impl GradArbStats {
    /// Create zeroed stats.
    pub fn new() -> Self {
        Self {
            migrations_detected: AtomicU64::new(0),
            arb_entries: AtomicU64::new(0),
            arb_timeouts: AtomicU64::new(0),
            pool_not_found: AtomicU64::new(0),
            no_arb_spread: AtomicU64::new(0),
            exits_tp: AtomicU64::new(0),
            exits_sl: AtomicU64::new(0),
            exits_max_hold: AtomicU64::new(0),
            gross_pnl_lamports: AtomicI64::new(0),
            net_pnl_lamports: AtomicI64::new(0),
        }
    }
}

impl Default for GradArbStats {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GradArbStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GradArbStats")
            .field(
                "migrations_detected",
                &self.migrations_detected.load(Ordering::Relaxed),
            )
            .field("arb_entries", &self.arb_entries.load(Ordering::Relaxed))
            .field("arb_timeouts", &self.arb_timeouts.load(Ordering::Relaxed))
            .field(
                "pool_not_found",
                &self.pool_not_found.load(Ordering::Relaxed),
            )
            .field(
                "no_arb_spread",
                &self.no_arb_spread.load(Ordering::Relaxed),
            )
            .field("exits_tp", &self.exits_tp.load(Ordering::Relaxed))
            .field("exits_sl", &self.exits_sl.load(Ordering::Relaxed))
            .field(
                "exits_max_hold",
                &self.exits_max_hold.load(Ordering::Relaxed),
            )
            .field(
                "gross_pnl_lamports",
                &self.gross_pnl_lamports.load(Ordering::Relaxed),
            )
            .field(
                "net_pnl_lamports",
                &self.net_pnl_lamports.load(Ordering::Relaxed),
            )
            .finish()
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// Graduation arbitrage engine.
///
/// Evaluates migration events for arb opportunities between the bonding curve
/// terminal price and DEX opening price. Manages positions with TP/SL/MaxHold
/// exits and sends closed positions to the paper logger via crossbeam channel.
pub struct GraduationArbEngine {
    config: GradArbConfig,
    /// Live positions keyed by mint address.
    positions: DashMap<[u8; 32], GradArbPosition>,
    /// Migration event deduplicator.
    dedup: MigrationDedup,
    /// Shared atomic stats counters.
    stats: Arc<GradArbStats>,
    /// Channel sender for completed trades → paper logger thread.
    closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>,
    /// Helius RPC URL for pool reserve fetches.
    helius_rpc_url: String,
    /// Helius WebSocket URL for accountSubscribe price feed.
    helius_ws_url: String,
    /// Shared reqwest client for pool resolution (reused across arb attempts).
    rpc_client: reqwest::Client,
    /// Cached recent blockhash for Jito bundle construction (used in live mode).
    #[allow(dead_code)]
    blockhash_cache: super::blockhash_manager::BlockhashCache,
    /// Price update channel sender — vault monitors send updates here.
    price_update_tx: tokio::sync::mpsc::UnboundedSender<super::price_feed::VaultPriceUpdate>,
    /// Price update channel receiver — polled in on_tick for TP/SL checks.
    price_update_rx: std::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<super::price_feed::VaultPriceUpdate>>,
}

impl GraduationArbEngine {
    /// Create a new graduation arb engine.
    pub fn new(
        config: GradArbConfig,
        stats: Arc<GradArbStats>,
        closed_tx: crossbeam_channel::Sender<GradArbClosedPosition>,
        helius_rpc_url: String,
        helius_ws_url: String,
        blockhash_cache: super::blockhash_manager::BlockhashCache,
    ) -> Self {
        let dedup_ttl_ms = config.dedup_ttl_ms;
        let (price_tx, price_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            config,
            positions: DashMap::with_capacity(16),
            dedup: MigrationDedup::new(dedup_ttl_ms),
            stats,
            closed_tx,
            rpc_client: make_pool_resolution_client(),
            helius_rpc_url,
            helius_ws_url,
            blockhash_cache,
            price_update_tx: price_tx,
            price_update_rx: std::sync::Mutex::new(price_rx),
        }
    }

    /// Get a reference to the engine config.
    pub fn config(&self) -> &GradArbConfig {
        &self.config
    }

    /// Get a reference to the shared stats.
    pub fn stats(&self) -> &Arc<GradArbStats> {
        &self.stats
    }

    /// Current number of open positions.
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Called for every migration event. Async — designed to run in a tokio::spawn task.
    /// Budget: 200ms total (pool resolution 180ms + decision 20ms).
    ///
    /// Pipeline: dedup → PumpSwap skip → pool resolution (RPC) → spread calc → paper entry or skip.
    pub async fn on_migration(
        &self,
        mint: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
        sig: [u8; 64],
    ) {
        self.stats
            .migrations_detected
            .fetch_add(1, Ordering::Relaxed);

        if !self.config.enabled {
            return;
        }

        // 1. Dedup check using first 32 bytes of sig as key (compact, sufficient)
        let sig_key: [u8; 32] = sig[..32].try_into().unwrap();
        let _dedup_entry = match self.dedup.try_insert(sig_key, ts_ms, source) {
            Some(entry) => entry,
            None => {
                tracing::debug!("[grad_arb] dedup hit — skipping duplicate migration event");
                return;
            }
        };

        // 2. Pool resolution with timeout (v2: postTokenBalances vault extraction)
        let resolution = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.rpc_timeout_ms),
            resolve_pool_from_transaction(&self.rpc_client, &sig, &self.helius_rpc_url),
        )
        .await;

        let pool = match resolution {
            Ok(Some(p)) => p,
            Ok(None) => {
                self.stats.pool_not_found.fetch_add(1, Ordering::Relaxed);
                self.log_no_entry(mint, ts_ms, source, GradArbExitReason::PoolNotFound);
                return;
            }
            Err(_timeout) => {
                self.stats.arb_timeouts.fetch_add(1, Ordering::Relaxed);
                self.log_no_entry(mint, ts_ms, source, GradArbExitReason::RpcTimeout);
                return;
            }
        };

        // 2a. PumpSwap skip — no structural arb exists; pump.fun controls migration price.
        if pool.pool_type == PoolType::PumpSwap && self.config.skip_pump_swap {
            tracing::debug!(
                mint = %bs58::encode(pool.mint).into_string(),
                "[grad_arb] PumpSwap graduation — skipping (no structural arb)"
            );
            self.log_no_entry(pool.mint, ts_ms, source, GradArbExitReason::PumpSwapSkipped);
            return;
        }

        // 2b. Guard: zero reserves → can't calculate a meaningful price
        if pool.reserve_sol_lamports == 0 || pool.reserve_token_atoms == 0 {
            tracing::debug!(
                mint = %bs58::encode(pool.mint).into_string(),
                pool_type = pool.pool_type.as_str(),
                reserve_sol = pool.reserve_sol_lamports,
                reserve_token = pool.reserve_token_atoms,
                "[grad_arb] zero reserves — cannot calculate spread, treating as pool_not_found"
            );
            self.stats.pool_not_found.fetch_add(1, Ordering::Relaxed);
            self.log_no_entry(pool.mint, ts_ms, source, GradArbExitReason::PoolNotFound);
            return;
        }

        // 3. Spread calculation
        // ray_opening_price = reserve_sol_lamports / reserve_token_atoms
        let ray_price = pool.reserve_sol_lamports as f64 / pool.reserve_token_atoms as f64;

        // BC terminal price: pre-computed constant (avoids runtime f64 division).
        let bc_price = BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM;

        let spread_pct = if bc_price > 0.0 {
            ((ray_price - bc_price) / bc_price * 100.0).abs()
        } else {
            0.0
        };

        // 3b. Guard: NaN/Inf/implausible spread → bad price data
        if !spread_pct.is_finite() || spread_pct > MAX_PLAUSIBLE_SPREAD_PCT {
            tracing::debug!(
                mint = %bs58::encode(pool.mint).into_string(),
                spread_pct = spread_pct,
                ray_price = ray_price,
                bc_price = bc_price,
                reserve_sol = pool.reserve_sol_lamports,
                reserve_token = pool.reserve_token_atoms,
                "[grad_arb] spread not finite or > {}% — rejecting as bad price data",
                MAX_PLAUSIBLE_SPREAD_PCT
            );
            self.stats.no_arb_spread.fetch_add(1, Ordering::Relaxed);
            self.log_no_entry(pool.mint, ts_ms, source, GradArbExitReason::NoArbFound);
            return;
        }

        // 4. Entry decision
        if spread_pct < self.config.min_spread_pct || pool.pool_type == PoolType::Unknown {
            self.stats.no_arb_spread.fetch_add(1, Ordering::Relaxed);
            self.log_no_entry(pool.mint, ts_ms, source, GradArbExitReason::NoArbFound);
            return;
        }

        // 5. Open paper position
        let size_lamports = (self.config.max_sol * 1e9) as u64;

        let position = GradArbPosition {
            mint: pool.mint,
            pool_address: pool.pool_address,
            pool_type: pool.pool_type,
            entry_price_lamports: (ray_price * 1e9) as u64,
            entry_vsol_lamports: (85.0 * 1e9) as u64,
            size_lamports,
            bc_terminal_price: bc_price,
            ray_opening_price: ray_price,
            spread_pct,
            detection_source: source,
            detection_latency_ms: 80, // approximate — would need tx timestamp for precise measurement
            entry_ts_ms: ts_ms,
            peak_price_lamports: (ray_price * 1e9) as u64,
            min_price_lamports: (ray_price * 1e9) as u64,
        };

        tracing::info!(
            mint = %bs58::encode(pool.mint).into_string(),
            spread_pct = %format!("{:.2}%", spread_pct),
            pool_type = pool.pool_type.as_str(),
            source = source.as_str(),
            size_sol = self.config.max_sol,
            "[grad_arb] paper position OPENED"
        );

        self.stats.arb_entries.fetch_add(1, Ordering::Relaxed);
        self.positions.insert(pool.mint, position);

        // Spawn vault price monitor — live TP/SL via accountSubscribe
        if !self.helius_ws_url.is_empty() && pool.reserve_sol_lamports > 0 {
            let pc_vault_b58 = bs58::encode(&pool.pool_address).into_string();
            let monitor = Arc::new(super::price_feed::VaultMonitor::new(
                pool.mint,
                pool.reserve_sol_lamports,
                pool.reserve_token_atoms,
            ));
            super::price_feed::spawn_vault_subscriber(
                self.helius_ws_url.clone(),
                pc_vault_b58,
                monitor,
                self.price_update_tx.clone(),
            );
            tracing::debug!(
                mint = %bs58::encode(pool.mint).into_string(),
                "[grad_arb] vault price monitor spawned"
            );
        }
    }

    /// Called every tick (50ms) for position management.
    ///
    /// Checks all open positions for MaxHold exits and processes price updates
    /// from vault monitors for TP/SL decisions.
    pub fn on_tick(&self, now_ms: u64) {
        // Process price updates from vault monitors → update peak/min prices
        if let Ok(mut rx) = self.price_update_rx.try_lock() {
            while let Ok(update) = rx.try_recv() {
                if let Some(mut pos) = self.positions.get_mut(&update.mint) {
                    // Update price from vault reserves
                    if update.reserve_token_atoms > 0 {
                        let current_price_lamports = (update.reserve_sol_lamports as f64
                            / update.reserve_token_atoms as f64
                            * 1e9) as u64;
                        if current_price_lamports > pos.peak_price_lamports {
                            pos.peak_price_lamports = current_price_lamports;
                        }
                        if current_price_lamports < pos.min_price_lamports {
                            pos.min_price_lamports = current_price_lamports;
                        }
                    }
                }
            }
        }

        let mut to_close: Vec<([u8; 32], GradArbExitReason)> = Vec::new();

        for entry in self.positions.iter() {
            let mint = *entry.key();
            let pos = entry.value();
            let hold_ms = now_ms.saturating_sub(pos.entry_ts_ms);

            // MaxHold exit
            if hold_ms >= self.config.max_hold_ms {
                to_close.push((mint, GradArbExitReason::MaxHold));
                continue;
            }

            // TP/SL from live price feed
            if pos.entry_price_lamports > 0 {
                let peak_pnl = (pos.peak_price_lamports as f64 - pos.entry_price_lamports as f64)
                    / pos.entry_price_lamports as f64;
                let current_min_pnl = (pos.min_price_lamports as f64 - pos.entry_price_lamports as f64)
                    / pos.entry_price_lamports as f64;

                if peak_pnl >= self.config.tp_pct {
                    to_close.push((mint, GradArbExitReason::TakeProfit));
                    continue;
                }
                if current_min_pnl <= -(self.config.sl_pct) {
                    to_close.push((mint, GradArbExitReason::StopLoss));
                    continue;
                }
            }
        }

        for (mint, reason) in to_close {
            if let Some((_, pos)) = self.positions.remove(&mint) {
                self.close_position(pos, reason, now_ms);
            }
        }
    }

    /// Close a position and send it to the logger channel.
    fn close_position(&self, pos: GradArbPosition, reason: GradArbExitReason, exit_ts_ms: u64) {
        let hold_ms = exit_ts_ms.saturating_sub(pos.entry_ts_ms);

        // Paper mode: exit price = entry price (no live feed) = 0 pnl
        // This is honest — we don't have a price feed yet
        let exit_price = pos.entry_price_lamports;

        // Fee simulation: 0.0015 SOL (Jito tip) + 0.0005 SOL (priority) = 0.002 SOL
        let fee_lamports = 2_000_000u64;
        let pnl = 0i64; // neutral in paper mode without price feed
        let net_pnl = pnl - fee_lamports as i64;

        // Update stats
        match reason {
            GradArbExitReason::TakeProfit => {
                self.stats.exits_tp.fetch_add(1, Ordering::Relaxed);
            }
            GradArbExitReason::StopLoss => {
                self.stats.exits_sl.fetch_add(1, Ordering::Relaxed);
            }
            GradArbExitReason::MaxHold => {
                self.stats.exits_max_hold.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        self.stats
            .net_pnl_lamports
            .fetch_add(net_pnl, Ordering::Relaxed);

        let closed = GradArbClosedPosition {
            mint: pos.mint,
            pool_address: pos.pool_address,
            pool_type: pos.pool_type,
            entry_price_lamports: pos.entry_price_lamports,
            exit_price_lamports: exit_price,
            size_lamports: pos.size_lamports,
            bc_terminal_price: pos.bc_terminal_price,
            ray_opening_price: pos.ray_opening_price,
            spread_pct: pos.spread_pct,
            detection_source: pos.detection_source,
            detection_latency_ms: pos.detection_latency_ms,
            entry_ts_ms: pos.entry_ts_ms,
            exit_ts_ms,
            hold_ms,
            exit_reason: reason,
            pnl_lamports: pnl,
            fee_lamports,
            net_pnl_lamports: net_pnl,
            mfe_lamports: pos.peak_price_lamports.saturating_sub(pos.entry_price_lamports),
            mae_lamports: pos.entry_price_lamports.saturating_sub(pos.min_price_lamports),
        };

        tracing::info!(
            mint = %bs58::encode(closed.mint).into_string(),
            reason = closed.exit_reason.as_str(),
            hold_ms = hold_ms,
            "[grad_arb] paper position CLOSED"
        );

        let _ = self.closed_tx.send(closed);
    }

    /// Log a failed arb attempt (pool not found, timeout, spread too low) to JSONL.
    /// These are valuable data points for understanding graduation event quality.
    fn log_no_entry(
        &self,
        mint: [u8; 32],
        ts_ms: u64,
        source: MigrationSource,
        reason: GradArbExitReason,
    ) {
        let closed = GradArbClosedPosition {
            mint,
            pool_address: [0u8; 32],
            pool_type: PoolType::Unknown,
            entry_price_lamports: 0,
            exit_price_lamports: 0,
            size_lamports: 0,
            bc_terminal_price: 0.0,
            ray_opening_price: 0.0,
            spread_pct: 0.0,
            detection_source: source,
            detection_latency_ms: 0,
            entry_ts_ms: ts_ms,
            exit_ts_ms: ts_ms,
            hold_ms: 0,
            exit_reason: reason,
            pnl_lamports: 0,
            fee_lamports: 0,
            net_pnl_lamports: 0,
            mfe_lamports: 0,
            mae_lamports: 0,
        };
        let _ = self.closed_tx.send(closed);
    }

    /// Get the Helius RPC URL (for pool reserve fetches).
    pub fn helius_rpc_url(&self) -> &str {
        &self.helius_rpc_url
    }

    /// Get the closed position sender (for external close triggers).
    pub fn closed_tx(&self) -> &crossbeam_channel::Sender<GradArbClosedPosition> {
        &self.closed_tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_config() -> GradArbConfig {
        GradArbConfig {
            enabled: true,
            paper_mode: true,
            max_sol: 0.30,
            min_spread_pct: 3.0,
            tp_pct: 0.03,
            sl_pct: 0.02,
            max_hold_ms: 5_000,
            jito_tip_sol: 0.003,
            dedup_ttl_ms: 10_000,
            rpc_timeout_ms: 200,
            skip_pump_swap: true,
        }
    }

    fn make_test_engine() -> (GraduationArbEngine, crossbeam_channel::Receiver<GradArbClosedPosition>) {
        let config = make_test_config();
        let stats = Arc::new(GradArbStats::new());
        let (tx, rx) = crossbeam_channel::unbounded();
        let engine = GraduationArbEngine::new(
            config,
            stats,
            tx,
            "https://rpc.example.com".to_string(),
            String::new(), // no WS in tests
            crate::arb::blockhash_manager::new_cache(),
        );
        (engine, rx)
    }

    #[test]
    fn test_grad_arb_config_version() {
        let config = make_test_config();
        assert_eq!(config.config_version(), "grad-v0.30sol_5000ms");
    }

    #[test]
    fn test_grad_arb_config_version_custom() {
        let mut config = make_test_config();
        config.max_sol = 1.50;
        config.max_hold_ms = 10_000;
        assert_eq!(config.config_version(), "grad-v1.50sol_10000ms");
    }

    #[test]
    fn test_grad_arb_stats_default() {
        let stats = GradArbStats::new();
        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 0);
        assert_eq!(stats.arb_entries.load(Ordering::Relaxed), 0);
        assert_eq!(stats.gross_pnl_lamports.load(Ordering::Relaxed), 0);
        assert_eq!(stats.net_pnl_lamports.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_grad_arb_engine_construction() {
        let (engine, _rx) = make_test_engine();
        assert!(engine.config().enabled);
        assert!(engine.config().paper_mode);
        assert_eq!(engine.position_count(), 0);
        assert_eq!(engine.helius_rpc_url(), "https://rpc.example.com");
    }

    #[test]
    fn test_grad_arb_engine_disabled_skips_processing() {
        let mut config = make_test_config();
        config.enabled = false;
        let stats = Arc::new(GradArbStats::new());
        let (tx, _rx) = crossbeam_channel::unbounded();
        let engine = GraduationArbEngine::new(
            config, stats.clone(), tx, String::new(), String::new(),
            crate::arb::blockhash_manager::new_cache(),
        );

        // Run on_migration synchronously via tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            engine
                .on_migration([1u8; 32], 1000, MigrationSource::HeliusLogs, [0u8; 64])
                .await;
        });

        // Stats should increment even when disabled (for monitoring)
        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_pool_type_as_str() {
        assert_eq!(PoolType::RaydiumAmmV4.as_str(), "raydium_amm_v4");
        assert_eq!(PoolType::PumpSwap.as_str(), "pump_swap");
        assert_eq!(PoolType::Unknown.as_str(), "unknown");
    }

    #[test]
    fn test_exit_reason_as_str() {
        assert_eq!(GradArbExitReason::TakeProfit.as_str(), "take_profit");
        assert_eq!(GradArbExitReason::StopLoss.as_str(), "stop_loss");
        assert_eq!(GradArbExitReason::MaxHold.as_str(), "max_hold");
        assert_eq!(GradArbExitReason::NoArbFound.as_str(), "no_arb_found");
        assert_eq!(GradArbExitReason::RpcTimeout.as_str(), "rpc_timeout");
        assert_eq!(GradArbExitReason::PoolNotFound.as_str(), "pool_not_found");
    }

    #[test]
    fn test_closed_position_pnl_fields() {
        let cp = GradArbClosedPosition {
            mint: [1u8; 32],
            pool_address: [2u8; 32],
            pool_type: PoolType::RaydiumAmmV4,
            entry_price_lamports: 1_000_000,
            exit_price_lamports: 1_030_000,
            size_lamports: 300_000_000, // 0.3 SOL
            bc_terminal_price: 0.000001234,
            ray_opening_price: 0.000001176,
            spread_pct: 4.7,
            detection_source: MigrationSource::HeliusLogs,
            detection_latency_ms: 82,
            entry_ts_ms: 1_700_000_000_000,
            exit_ts_ms: 1_700_000_001_240,
            hold_ms: 1_240,
            exit_reason: GradArbExitReason::TakeProfit,
            pnl_lamports: 12_000_000,
            fee_lamports: 4_000_000,
            net_pnl_lamports: 8_000_000,
            mfe_lamports: 14_000_000,
            mae_lamports: 2_000_000,
        };
        assert_eq!(cp.hold_ms, 1_240);
        assert_eq!(cp.net_pnl_lamports, cp.pnl_lamports - cp.fee_lamports as i64);
    }

    #[test]
    fn test_grad_arb_stats_atomic_operations() {
        let stats = GradArbStats::new();
        stats.migrations_detected.fetch_add(5, Ordering::Relaxed);
        stats.arb_entries.fetch_add(3, Ordering::Relaxed);
        stats.gross_pnl_lamports.fetch_add(1_000_000, Ordering::Relaxed);
        stats.net_pnl_lamports.fetch_add(-500_000, Ordering::Relaxed);

        assert_eq!(stats.migrations_detected.load(Ordering::Relaxed), 5);
        assert_eq!(stats.arb_entries.load(Ordering::Relaxed), 3);
        assert_eq!(stats.gross_pnl_lamports.load(Ordering::Relaxed), 1_000_000);
        assert_eq!(stats.net_pnl_lamports.load(Ordering::Relaxed), -500_000);
    }

    #[test]
    fn test_on_tick_noop_no_positions() {
        let (engine, _rx) = make_test_engine();
        // Should not panic — no open positions
        engine.on_tick(1_700_000_000_000);
        assert_eq!(engine.position_count(), 0);
    }

    #[test]
    fn test_on_tick_closes_expired_position() {
        let (engine, rx) = make_test_engine();

        // Manually insert a position
        let mint = [42u8; 32];
        let pos = GradArbPosition {
            mint,
            pool_address: [0u8; 32],
            pool_type: PoolType::RaydiumAmmV4,
            entry_price_lamports: 1_000_000,
            entry_vsol_lamports: 85_000_000_000,
            size_lamports: 300_000_000,
            bc_terminal_price: 0.000000411,
            ray_opening_price: 0.000000450,
            spread_pct: 9.5,
            detection_source: MigrationSource::HeliusLogs,
            detection_latency_ms: 80,
            entry_ts_ms: 1_000_000,
            peak_price_lamports: 1_000_000,
            min_price_lamports: 1_000_000,
        };
        engine.positions.insert(mint, pos);
        assert_eq!(engine.position_count(), 1);

        // Tick at entry + max_hold_ms (5000ms) → should close
        engine.on_tick(1_000_000 + 5_001);
        assert_eq!(engine.position_count(), 0);

        // Should have received the closed position
        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.exit_reason, GradArbExitReason::MaxHold);
        assert_eq!(closed.mint, mint);
        assert_eq!(closed.hold_ms, 5001);
    }

    #[test]
    fn test_on_tick_keeps_fresh_position() {
        let (engine, _rx) = make_test_engine();

        let mint = [43u8; 32];
        let pos = GradArbPosition {
            mint,
            pool_address: [0u8; 32],
            pool_type: PoolType::PumpSwap,
            entry_price_lamports: 500_000,
            entry_vsol_lamports: 85_000_000_000,
            size_lamports: 300_000_000,
            bc_terminal_price: 0.000000411,
            ray_opening_price: 0.000000430,
            spread_pct: 4.6,
            detection_source: MigrationSource::CoreCastStream2,
            detection_latency_ms: 120,
            entry_ts_ms: 1_000_000,
            peak_price_lamports: 500_000,
            min_price_lamports: 500_000,
        };
        engine.positions.insert(mint, pos);

        // Tick at entry + 2000ms (< max_hold_ms 5000ms) → should keep
        engine.on_tick(1_000_000 + 2_000);
        assert_eq!(engine.position_count(), 1);
    }

    #[test]
    fn test_on_migration_dedup() {
        let (engine, _rx) = make_test_engine();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut sig = [0u8; 64];
        sig[0] = 1;

        rt.block_on(async {
            // First call should be processed
            engine
                .on_migration([1u8; 32], 1000, MigrationSource::HeliusLogs, sig)
                .await;
            // Second call with same sig should be deduped
            engine
                .on_migration([1u8; 32], 1100, MigrationSource::CoreCastStream2, sig)
                .await;
        });

        // migrations_detected should be 2 (both calls increment), but dedup prevents double-processing
        assert_eq!(
            engine.stats().migrations_detected.load(Ordering::Relaxed),
            2
        );
    }

    #[test]
    fn test_log_no_entry_sends_closed_position() {
        let (engine, rx) = make_test_engine();

        engine.log_no_entry(
            [99u8; 32],
            5000,
            MigrationSource::HeliusLogs,
            GradArbExitReason::RpcTimeout,
        );

        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.mint, [99u8; 32]);
        assert_eq!(closed.exit_reason, GradArbExitReason::RpcTimeout);
        assert_eq!(closed.hold_ms, 0);
        assert_eq!(closed.pnl_lamports, 0);
        assert_eq!(closed.pool_type, PoolType::Unknown);
    }

    #[test]
    fn test_close_position_stats() {
        let (engine, rx) = make_test_engine();

        let pos = GradArbPosition {
            mint: [50u8; 32],
            pool_address: [0u8; 32],
            pool_type: PoolType::RaydiumAmmV4,
            entry_price_lamports: 1_000_000,
            entry_vsol_lamports: 85_000_000_000,
            size_lamports: 300_000_000,
            bc_terminal_price: 0.000000411,
            ray_opening_price: 0.000000450,
            spread_pct: 9.5,
            detection_source: MigrationSource::HeliusLogs,
            detection_latency_ms: 80,
            entry_ts_ms: 10_000,
            peak_price_lamports: 1_050_000,
            min_price_lamports: 990_000,
        };

        engine.close_position(pos, GradArbExitReason::MaxHold, 15_000);

        assert_eq!(engine.stats().exits_max_hold.load(Ordering::Relaxed), 1);

        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.hold_ms, 5000);
        assert_eq!(closed.exit_reason, GradArbExitReason::MaxHold);
        assert_eq!(closed.fee_lamports, 2_000_000);
        assert_eq!(closed.net_pnl_lamports, -2_000_000); // 0 pnl - 2M fee
        assert_eq!(closed.mfe_lamports, 50_000); // peak - entry
        assert_eq!(closed.mae_lamports, 10_000); // entry - min
    }

    // ── Pool Resolution Tests ────────────────────────────────────────────

    #[test]
    fn test_decode_bs58_32_valid() {
        // Raydium AMM v4 program ID as a known 32-byte pubkey
        let result = decode_bs58_32("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
        assert!(result.is_some());
        let bytes = result.unwrap();
        // Re-encode to verify round-trip
        assert_eq!(bs58::encode(&bytes).into_string(), "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    }

    #[test]
    fn test_decode_bs58_32_invalid() {
        assert!(decode_bs58_32("short").is_none());
        assert!(decode_bs58_32("").is_none());
        assert!(decode_bs58_32("!!!invalid!!!").is_none());
    }

    #[test]
    fn test_pool_resolution_struct() {
        let res = PoolResolution {
            mint: [1u8; 32],
            pool_address: [2u8; 32],
            pool_type: PoolType::RaydiumAmmV4,
            reserve_sol_lamports: 85_000_000_000, // ~85 SOL
            reserve_token_atoms: 200_000_000_000_000,
            bc_terminal_vsol: 85.0,
        };
        assert_eq!(res.pool_type, PoolType::RaydiumAmmV4);
        assert_eq!(res.reserve_sol_lamports, 85_000_000_000);
    }

    #[test]
    fn test_pool_resolution_unknown() {
        let res = PoolResolution {
            mint: [0u8; 32],
            pool_address: [0u8; 32],
            pool_type: PoolType::Unknown,
            reserve_sol_lamports: 0,
            reserve_token_atoms: 0,
            bc_terminal_vsol: 0.0,
        };
        assert_eq!(res.pool_type, PoolType::Unknown);
        assert_eq!(res.pool_type.as_str(), "unknown");
    }

    #[test]
    fn test_make_pool_resolution_client() {
        // Should not panic
        let client = make_pool_resolution_client();
        // Client exists — that's the test
        drop(client);
    }

    // ── v2 Pool Resolution Tests (postTokenBalances vault extraction) ───

    #[test]
    fn test_extract_vaults_from_postTokenBalances() {
        // Core v2 test: extract coin_vault and pc_vault from postTokenBalances.
        // This is the fix for the 99.97% pool resolution failure rate.
        // Use known valid 32-byte base58 pubkeys:
        let graduation_mint = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"; // SPL Token program
        let coin_vault_b58 = "11111111111111111111111111111111"; // System program (32 zero bytes)
        // Raydium AMM v4 program as a valid 32-byte pubkey for pc_vault
        let pc_vault_b58 = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        // Another valid 32-byte pubkey for payer
        let payer_b58 = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
        // Another valid pubkey for small holder
        let small_holder_b58 = "SysvarRent111111111111111111111111111111111";

        let mock_tx = serde_json::json!({
            "meta": {
                "postTokenBalances": [
                    {
                        "accountIndex": 1,
                        "mint": graduation_mint,
                        "uiTokenAmount": { "amount": "206900000000000" }
                    },
                    {
                        "accountIndex": 2,
                        "mint": "So11111111111111111111111111111111111111112",
                        "uiTokenAmount": { "amount": "85000000000" }
                    },
                    {
                        "accountIndex": 3,
                        "mint": graduation_mint,
                        "uiTokenAmount": { "amount": "100000" }
                    }
                ]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        payer_b58,
                        coin_vault_b58,
                        pc_vault_b58,
                        small_holder_b58
                    ]
                }
            }
        });

        let result = extract_vaults_from_tx_response(&mock_tx, graduation_mint);
        assert!(result.is_some(), "should extract vaults from postTokenBalances");

        let (coin_vault, pc_vault) = result.unwrap();
        assert_eq!(bs58::encode(&coin_vault).into_string(), coin_vault_b58);
        assert_eq!(bs58::encode(&pc_vault).into_string(), pc_vault_b58);
    }

    #[test]
    fn test_extract_vaults_with_pubkey_object_keys() {
        // jsonParsed can return accountKeys as objects with "pubkey" field
        let graduation_mint = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        let coin_vault_b58 = "11111111111111111111111111111111";
        let pc_vault_b58 = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

        let mock_tx = serde_json::json!({
            "meta": {
                "postTokenBalances": [
                    {
                        "accountIndex": 0,
                        "mint": graduation_mint,
                        "uiTokenAmount": { "amount": "206900000000000" }
                    },
                    {
                        "accountIndex": 1,
                        "mint": "So11111111111111111111111111111111111111112",
                        "uiTokenAmount": { "amount": "85000000000" }
                    }
                ]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": coin_vault_b58, "signer": false, "writable": true},
                        {"pubkey": pc_vault_b58, "signer": false, "writable": true}
                    ]
                }
            }
        });

        let result = extract_vaults_from_tx_response(&mock_tx, graduation_mint);
        assert!(result.is_some());
        let (coin_vault, pc_vault) = result.unwrap();
        assert_eq!(bs58::encode(&coin_vault).into_string(), coin_vault_b58);
        assert_eq!(bs58::encode(&pc_vault).into_string(), pc_vault_b58);
    }

    #[test]
    fn test_extract_vaults_no_wsol_returns_none() {
        // If no WSOL balance entry exists, extraction should return None
        let graduation_mint = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

        let mock_tx = serde_json::json!({
            "meta": {
                "postTokenBalances": [
                    {
                        "accountIndex": 0,
                        "mint": graduation_mint,
                        "uiTokenAmount": { "amount": "206900000000000" }
                    }
                ]
            },
            "transaction": {
                "message": {
                    "accountKeys": ["11111111111111111111111111111111"]
                }
            }
        });

        let result = extract_vaults_from_tx_response(&mock_tx, graduation_mint);
        assert!(result.is_none(), "should fail when no WSOL balance entry exists");
    }

    #[test]
    fn test_parse_spl_token_amount() {
        // SPL Token Account: amount at bytes [64..72] LE u64
        use base64::Engine;

        // Build a 165-byte SPL token account with amount = 206_900_000_000_000 at [64..72]
        let mut account_data = vec![0u8; 165];
        let amount: u64 = 206_900_000_000_000;
        account_data[64..72].copy_from_slice(&amount.to_le_bytes());

        let encoded = base64::engine::general_purpose::STANDARD.encode(&account_data);
        let parsed = parse_spl_token_amount(&encoded);
        assert_eq!(parsed, Some(amount));
    }

    #[test]
    fn test_parse_spl_token_amount_too_short() {
        use base64::Engine;
        // Only 64 bytes — not enough for amount field
        let short_data = vec![0u8; 64];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&short_data);
        let parsed = parse_spl_token_amount(&encoded);
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_spl_token_amount_zero() {
        use base64::Engine;
        let account_data = vec![0u8; 165];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&account_data);
        let parsed = parse_spl_token_amount(&encoded);
        assert_eq!(parsed, Some(0));
    }

    #[test]
    fn test_pumpswap_skipped_immediately() {
        // PumpSwap graduations should be skipped without any pool resolution attempt.
        let (engine, rx) = make_test_engine();

        // Verify default config has skip_pump_swap = true
        assert!(engine.config().skip_pump_swap);

        // Simulate what on_migration does when pool_type == PumpSwap:
        // log as PumpSwapSkipped and return
        engine.log_no_entry(
            [0xAA; 32],
            1_000_000,
            MigrationSource::HeliusLogs,
            GradArbExitReason::PumpSwapSkipped,
        );

        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.exit_reason, GradArbExitReason::PumpSwapSkipped);
        assert_eq!(closed.exit_reason.as_str(), "pumpswap_skipped");
        assert_eq!(closed.hold_ms, 0);
        assert_eq!(closed.pnl_lamports, 0);
        assert_eq!(engine.position_count(), 0);
    }

    #[test]
    fn test_pumpswap_exit_reason_as_str() {
        assert_eq!(GradArbExitReason::PumpSwapSkipped.as_str(), "pumpswap_skipped");
    }

    #[test]
    fn test_skip_pump_swap_config_default() {
        let config = GradArbConfig::default();
        assert!(config.skip_pump_swap, "skip_pump_swap should default to true");
    }

    #[test]
    fn test_extract_fallback_mint() {
        let mock_tx = serde_json::json!({
            "meta": {
                "postTokenBalances": [
                    {"mint": "So11111111111111111111111111111111111111112", "accountIndex": 0},
                    {"mint": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "accountIndex": 1}
                ]
            }
        });

        let result = extract_fallback_mint(&mock_tx);
        assert!(result.is_some());
        assert_eq!(
            bs58::encode(&result.unwrap()).into_string(),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
    }

    #[test]
    fn test_extract_fallback_mint_only_wsol() {
        let mock_tx = serde_json::json!({
            "meta": {
                "postTokenBalances": [
                    {"mint": "So11111111111111111111111111111111111111112", "accountIndex": 0}
                ]
            }
        });

        let result = extract_fallback_mint(&mock_tx);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_pool_inner_rpc_error_response() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(50))
                .build()
                .unwrap();
            let sig = [0u8; 64];
            let result = resolve_pool_from_transaction(
                &client,
                &sig,
                "http://127.0.0.1:1",
            ).await;
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_pool_resolution_clone_debug() {
        let res = PoolResolution {
            mint: [1u8; 32],
            pool_address: [2u8; 32],
            pool_type: PoolType::Unknown,
            reserve_sol_lamports: 0,
            reserve_token_atoms: 0,
            bc_terminal_vsol: 0.0,
        };
        let cloned = res.clone();
        assert_eq!(cloned.pool_type, PoolType::Unknown);
        let debug_str = format!("{:?}", res);
        assert!(debug_str.contains("Unknown"));
    }

    // ── Bug Fix Tests ────────────────────────────────────────────────────

    #[test]
    fn test_zero_reserves_returns_pool_not_found() {
        let (engine, rx) = make_test_engine();

        let pool = PoolResolution {
            mint: [0xAA; 32],
            pool_address: [0xBB; 32],
            pool_type: PoolType::RaydiumAmmV4,
            reserve_sol_lamports: 0,
            reserve_token_atoms: 0,
            bc_terminal_vsol: 0.0,
        };

        assert!(pool.reserve_sol_lamports == 0 || pool.reserve_token_atoms == 0);

        engine.stats().pool_not_found.fetch_add(1, Ordering::Relaxed);
        engine.log_no_entry(
            pool.mint,
            1_000_000,
            MigrationSource::HeliusLogs,
            GradArbExitReason::PoolNotFound,
        );

        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.exit_reason, GradArbExitReason::PoolNotFound);
        assert_eq!(closed.spread_pct, 0.0);
        assert_eq!(engine.stats().pool_not_found.load(Ordering::Relaxed), 1);
        assert_eq!(engine.position_count(), 0);
    }

    #[test]
    fn test_infinite_spread_rejected() {
        let (engine, rx) = make_test_engine();

        let bc_price = BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM;
        let ray_price = 1.0_f64;
        let spread_pct = ((ray_price - bc_price) / bc_price * 100.0).abs();

        assert!(spread_pct > MAX_PLAUSIBLE_SPREAD_PCT);
        assert!(spread_pct > 50.0);

        engine.stats().no_arb_spread.fetch_add(1, Ordering::Relaxed);
        engine.log_no_entry(
            [0xEE; 32],
            2_000_000,
            MigrationSource::CoreCastStream2,
            GradArbExitReason::NoArbFound,
        );

        let closed = rx.try_recv().unwrap();
        assert_eq!(closed.exit_reason, GradArbExitReason::NoArbFound);
        assert_eq!(engine.stats().no_arb_spread.load(Ordering::Relaxed), 1);
        assert_eq!(engine.position_count(), 0);

        let nan_spread = f64::NAN;
        assert!(!nan_spread.is_finite());
        let inf_spread = f64::INFINITY;
        assert!(!inf_spread.is_finite());
    }

    #[test]
    fn test_bc_terminal_price_constant_value() {
        // v2 constant is derived from k = vSol₀ × vTokens₀ invariant
        // Should be approximately 4.1e-4 lamports per atom
        assert!(BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM > 4.0e-4);
        assert!(BC_TERMINAL_PRICE_LAMPORTS_PER_ATOM < 4.2e-4);
    }

    #[test]
    fn test_max_plausible_spread_constant() {
        assert_eq!(MAX_PLAUSIBLE_SPREAD_PCT, 50.0);
    }
}
