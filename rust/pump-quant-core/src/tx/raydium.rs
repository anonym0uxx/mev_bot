//! Raydium AMM V4 swap instruction builder.
//!
//! Builds complete signed `VersionedTransaction`s for buy (SOL → Token) and
//! sell (Token → SOL) swaps through Raydium's AMM V4 program.
//!
//! No dependency on `spl-token` or `spl-associated-token-account` crates —
//! all SPL instructions and ATA derivation are built manually to avoid the
//! zeroize version conflict with rustls.

use std::str::FromStr;

use base64::Engine;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    system_program,
    transaction::VersionedTransaction,
};

// ── Raydium / Serum constants ────────────────────────────────────────────────

/// Raydium AMM V4 program ID.
pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Jito tip accounts — rotate to distribute tips.
pub const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt13Gb16aj",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// OpenBook (Serum) DEX V3 program ID.
pub const SERUM_DEX_PROGRAM: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";

/// Wrapped SOL mint.
pub const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

/// SPL Token program ID.
pub const SPL_TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Associated Token Account program ID.
pub const SPL_ATA_PROGRAM_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe8bXh";

// ── Pool accounts ────────────────────────────────────────────────────────────

/// All accounts needed to interact with a Raydium AMM V4 pool.
///
/// Resolved externally (E1: pool resolution) and passed into the tx builders.
/// `coin_vault` and `pc_vault` are the Raydium pool's own token vaults
/// (distinct from serum_coin_vault / serum_pc_vault which are on the OpenBook market).
#[derive(Debug, Clone)]
pub struct RaydiumPoolAccounts {
    pub amm_id: [u8; 32],
    pub amm_authority: [u8; 32],
    pub amm_open_orders: [u8; 32],
    pub amm_target_orders: [u8; 32],
    pub serum_program_id: [u8; 32],
    pub serum_market: [u8; 32],
    pub serum_bids: [u8; 32],
    pub serum_asks: [u8; 32],
    pub serum_event_queue: [u8; 32],
    pub serum_coin_vault: [u8; 32],
    pub serum_pc_vault: [u8; 32],
    pub serum_vault_signer: [u8; 32],
    pub coin_vault: [u8; 32],  // Raydium pool coin vault (= PoolResolution.coin_vault)
    pub pc_vault: [u8; 32],    // Raydium pool pc vault (= PoolResolution.pc_vault)
}

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum RaydiumTxError {
    MissingAccount(&'static str),
    InvalidPubkey(String),
    SignError(String),
}

impl std::fmt::Display for RaydiumTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAccount(name) => write!(f, "missing required account: {name}"),
            Self::InvalidPubkey(s) => write!(f, "invalid pubkey: {s}"),
            Self::SignError(s) => write!(f, "transaction signing error: {s}"),
        }
    }
}

impl std::error::Error for RaydiumTxError {}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Derive the associated token address for `wallet` + `mint`.
/// Equivalent to `spl_associated_token_account::get_associated_token_address`.
///
/// PDA seeds: [wallet, token_program, mint] under the ATA program.
pub fn token_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let (addr, _bump) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );
    addr
}

/// Build the 17-byte Raydium swapBaseIn instruction data.
/// Format: [9u8] + amount_in.to_le_bytes() + min_out.to_le_bytes()
pub fn build_swap_base_in_data(amount_in: u64, min_out: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(17);
    data.push(9u8); // swapBaseIn discriminator
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());
    data
}

/// Build a create_associated_token_account_idempotent instruction manually.
///
/// This instruction creates the ATA if it doesn't exist, or is a no-op if it does.
/// Avoids dependency on spl-associated-token-account crate.
///
/// Accounts:
///   0. [signer, writable] funding_account (payer)
///   1. [writable]         associated_token_account
///   2. []                 wallet_address
///   3. []                 token_mint
///   4. []                 system_program
///   5. []                 token_program
fn build_create_ata_idempotent_ix(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
) -> Instruction {
    let ata = token_ata(wallet, mint);
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();

    Instruction {
        program_id: ata_program,
        accounts: vec![
            AccountMeta::new(*payer, true),           // 0. funding account (signer)
            AccountMeta::new(ata, false),              // 1. associated token account
            AccountMeta::new_readonly(*wallet, false), // 2. wallet address
            AccountMeta::new_readonly(*mint, false),   // 3. token mint
            AccountMeta::new_readonly(system_program::id(), false), // 4. system_program
            AccountMeta::new_readonly(token_program, false),        // 5. token_program
        ],
        data: vec![1], // 1 = CreateIdempotent instruction discriminator
    }
}

/// Build an SPL Token `CloseAccount` instruction manually.
///
/// SPL Token instruction index: 9 (CloseAccount).
/// Accounts:
///   0. [writable] account to close
///   1. [writable] destination for remaining SOL
///   2. [signer]   owner of the account
fn build_close_account_ix(
    account_to_close: &Pubkey,
    destination: &Pubkey,
    owner: &Pubkey,
) -> Instruction {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    Instruction {
        program_id: token_program,
        accounts: vec![
            AccountMeta::new(*account_to_close, false), // 0. account
            AccountMeta::new(*destination, false),       // 1. destination
            AccountMeta::new_readonly(*owner, true),     // 2. owner (signer)
        ],
        data: vec![9], // CloseAccount = instruction index 9
    }
}

/// Build an SPL Token `SyncNative` instruction manually.
///
/// SPL Token instruction index: 17 (SyncNative).
/// Accounts:
///   0. [writable] native token account to sync
fn build_sync_native_ix(native_account: &Pubkey) -> Instruction {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    Instruction {
        program_id: token_program,
        accounts: vec![
            AccountMeta::new(*native_account, false), // 0. token account
        ],
        data: vec![17], // SyncNative = instruction index 17
    }
}

/// Build the Raydium swapBaseIn instruction with the 18-account layout.
///
/// Account order:
///   0:  SPL_TOKEN_PROGRAM (readonly)
///   1:  amm_id (writable)
///   2:  amm_authority (readonly)
///   3:  amm_open_orders (writable)
///   4:  amm_target_orders (writable)
///   5:  pool.coin_vault (writable)
///   6:  pool.pc_vault (writable)
///   7:  serum_program_id (readonly)
///   8:  serum_market (writable)
///   9:  serum_bids (writable)
///   10: serum_asks (writable)
///   11: serum_event_queue (writable)
///   12: serum_coin_vault (writable)
///   13: serum_pc_vault (writable)
///   14: serum_vault_signer (readonly)
///   15: user_source_token_account (writable)
///   16: user_destination_token_account (writable)
///   17: user_owner (signer, writable)
fn build_raydium_swap_ix(
    pool: &RaydiumPoolAccounts,
    user_source: &Pubkey,
    user_destination: &Pubkey,
    user_owner: &Pubkey,
    amount_in: u64,
    min_out: u64,
) -> Instruction {
    let raydium_program = Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM).unwrap();
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();

    let accounts = vec![
        AccountMeta::new_readonly(token_program, false),                          // 0
        AccountMeta::new(Pubkey::new_from_array(pool.amm_id), false),             // 1
        AccountMeta::new_readonly(Pubkey::new_from_array(pool.amm_authority), false), // 2
        AccountMeta::new(Pubkey::new_from_array(pool.amm_open_orders), false),    // 3
        AccountMeta::new(Pubkey::new_from_array(pool.amm_target_orders), false),  // 4
        AccountMeta::new(Pubkey::new_from_array(pool.coin_vault), false),         // 5
        AccountMeta::new(Pubkey::new_from_array(pool.pc_vault), false),           // 6
        AccountMeta::new_readonly(Pubkey::new_from_array(pool.serum_program_id), false), // 7
        AccountMeta::new(Pubkey::new_from_array(pool.serum_market), false),       // 8
        AccountMeta::new(Pubkey::new_from_array(pool.serum_bids), false),         // 9
        AccountMeta::new(Pubkey::new_from_array(pool.serum_asks), false),         // 10
        AccountMeta::new(Pubkey::new_from_array(pool.serum_event_queue), false),  // 11
        AccountMeta::new(Pubkey::new_from_array(pool.serum_coin_vault), false),   // 12
        AccountMeta::new(Pubkey::new_from_array(pool.serum_pc_vault), false),     // 13
        AccountMeta::new_readonly(Pubkey::new_from_array(pool.serum_vault_signer), false), // 14
        AccountMeta::new(*user_source, false),                                    // 15
        AccountMeta::new(*user_destination, false),                               // 16
        AccountMeta::new(*user_owner, true),                                      // 17
    ];

    Instruction {
        program_id: raydium_program,
        accounts,
        data: build_swap_base_in_data(amount_in, min_out),
    }
}

// ── Sell: Token → SOL ────────────────────────────────────────────────────────

/// Build a complete signed Raydium sell transaction (Token → SOL).
///
/// Instruction sequence:
///   1. ComputeBudgetInstruction::set_compute_unit_limit(300_000)
///   2. ComputeBudgetInstruction::set_compute_unit_price(5000)
///   3. Raydium swapBaseIn (token ATA → WSOL ATA, 18 accounts)
///   4. SPL Token close_account(wsol_ata → wallet) — unwrap WSOL → SOL
///   5. system_instruction::transfer(wallet → jito_tip_account, tip)
///
/// Returns: serialized transaction bytes (bincode-encoded VersionedTransaction).
pub fn build_raydium_sell_tx(
    pool: &RaydiumPoolAccounts,
    mint: &[u8; 32],
    wallet_keypair: &Keypair,
    tokens_to_sell: u64,
    min_sol_out: u64,
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
) -> Result<Vec<u8>, RaydiumTxError> {
    let wallet_pubkey = wallet_keypair.pubkey();
    let mint_pubkey = Pubkey::new_from_array(*mint);
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let blockhash = Hash::new_from_array(recent_blockhash);

    // Derive ATAs
    let token_ata_addr = token_ata(&wallet_pubkey, &mint_pubkey);
    let wsol_ata_addr = token_ata(&wallet_pubkey, &wsol_mint);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(300_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Raydium swapBaseIn: token ATA → WSOL ATA
    let ix_swap = build_raydium_swap_ix(
        pool,
        &token_ata_addr,   // user_source: token ATA
        &wsol_ata_addr,    // user_destination: WSOL ATA
        &wallet_pubkey,
        tokens_to_sell,
        min_sol_out,
    );

    // 4. Close WSOL ATA → wallet (unwrap WSOL to SOL)
    let ix_close = build_close_account_ix(&wsol_ata_addr, &wallet_pubkey, &wallet_pubkey);

    // 5. Jito tip
    let ix_tip = system_instruction::transfer(&wallet_pubkey, &jito_tip_account, jito_tip_lamports);

    let ixs = vec![ix_cu_limit, ix_cu_price, ix_swap, ix_close, ix_tip];

    // Compile V0 message (no address lookup tables)
    let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
        .map_err(|e| RaydiumTxError::SignError(format!("failed to compile V0 message: {e}")))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[wallet_keypair])
        .map_err(|e| RaydiumTxError::SignError(format!("failed to sign transaction: {e}")))?;

    bincode::serialize(&tx)
        .map_err(|e| RaydiumTxError::SignError(format!("failed to serialize transaction: {e}")))
}

// ── Buy: SOL → Token ─────────────────────────────────────────────────────────

/// Build a complete signed Raydium buy transaction (SOL → Token).
///
/// Instruction sequence:
///   1. ComputeBudgetInstruction::set_compute_unit_limit(400_000)
///   2. ComputeBudgetInstruction::set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(wallet, WSOL_MINT)
///   4. create_associated_token_account_idempotent(wallet, token_mint)
///   5. system_instruction::transfer(wallet → wsol_ata, sol_lamports)
///   6. spl_token::sync_native(wsol_ata)
///   7. Raydium swapBaseIn (WSOL ATA → token ATA, 18 accounts)
///   8. spl_token::close_account(wsol_ata → wallet) — reclaim leftover WSOL
///   9. system_instruction::transfer(wallet → jito_tip_account, tip)
///
/// Returns: serialized transaction bytes (bincode-encoded VersionedTransaction).
pub fn build_raydium_buy_tx(
    pool: &RaydiumPoolAccounts,
    mint: &[u8; 32],
    wallet_keypair: &Keypair,
    sol_lamports: u64,
    min_tokens_out: u64,
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
) -> Result<Vec<u8>, RaydiumTxError> {
    let wallet_pubkey = wallet_keypair.pubkey();
    let mint_pubkey = Pubkey::new_from_array(*mint);
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let blockhash = Hash::new_from_array(recent_blockhash);

    // Derive ATAs
    let wsol_ata_addr = token_ata(&wallet_pubkey, &wsol_mint);
    let token_ata_addr = token_ata(&wallet_pubkey, &mint_pubkey);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(400_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Create WSOL ATA (idempotent)
    let ix_create_wsol_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &wsol_mint,
    );

    // 4. Create token ATA (idempotent)
    let ix_create_token_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &mint_pubkey,
    );

    // 5. Fund WSOL ATA with SOL
    let ix_fund_wsol = system_instruction::transfer(&wallet_pubkey, &wsol_ata_addr, sol_lamports);

    // 6. Sync native to wrap deposited SOL → WSOL
    let ix_sync = build_sync_native_ix(&wsol_ata_addr);

    // 7. Raydium swapBaseIn: WSOL ATA → token ATA
    let ix_swap = build_raydium_swap_ix(
        pool,
        &wsol_ata_addr,    // user_source: WSOL ATA
        &token_ata_addr,   // user_destination: token ATA
        &wallet_pubkey,
        sol_lamports,
        min_tokens_out,
    );

    // 8. Close WSOL ATA → wallet (reclaim leftover WSOL)
    let ix_close = build_close_account_ix(&wsol_ata_addr, &wallet_pubkey, &wallet_pubkey);

    // 9. Jito tip
    let ix_tip = system_instruction::transfer(&wallet_pubkey, &jito_tip_account, jito_tip_lamports);

    let ixs = vec![
        ix_cu_limit,
        ix_cu_price,
        ix_create_wsol_ata,
        ix_create_token_ata,
        ix_fund_wsol,
        ix_sync,
        ix_swap,
        ix_close,
        ix_tip,
    ];

    // Compile V0 message (no address lookup tables)
    let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
        .map_err(|e| RaydiumTxError::SignError(format!("failed to compile V0 message: {e}")))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[wallet_keypair])
        .map_err(|e| RaydiumTxError::SignError(format!("failed to sign transaction: {e}")))?;

    bincode::serialize(&tx)
        .map_err(|e| RaydiumTxError::SignError(format!("failed to serialize transaction: {e}")))
}

// ── Pool account fetching (E1's Task C — stub for compilation) ───────────────

/// Derive the Raydium AMM authority PDA.
///
/// Seeds: \[b"amm authority"\] under the Raydium AMM V4 program, using the nonce
/// from the AMM account data. For the default nonce (usually 254 or 253),
/// `create_program_address` is used directly.
pub fn derive_amm_authority(nonce: u8) -> Result<Pubkey, RaydiumTxError> {
    let raydium_program = Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM).unwrap();
    Pubkey::create_program_address(
        &[
            b"amm authority",
            &[nonce],
        ],
        &raydium_program,
    )
    .map_err(|_| RaydiumTxError::InvalidPubkey("failed to derive amm_authority PDA".into()))
}

/// Fetches full Raydium AMM V4 pool accounts given an amm_id.
///
/// Makes 2 RPC calls: getAccountInfo(amm_id), getAccountInfo(serum_market).
/// Called once at graduation time — cold path, ~20ms total.
///
/// `coin_vault` and `pc_vault` are passed in from the graduation tx parsing
/// (they are verified against the AMM account data).
///
/// **NOTE:** This is E1's Task C. Full implementation will be provided by E1.
/// This stub returns an error to indicate it's not yet implemented.
pub async fn fetch_raydium_pool_accounts(
    client: &reqwest::Client,
    rpc_url: &str,
    amm_id: &[u8; 32],
    coin_vault: [u8; 32],
    pc_vault: [u8; 32],
) -> Result<RaydiumPoolAccounts, String> {
    let amm_id_b58 = bs58::encode(amm_id).into_string();

    // ── Step 1: Fetch AMM account data ───────────────────────────────────
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            amm_id_b58,
            { "encoding": "base64" }
        ]
    });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("RPC request failed for amm_id {amm_id_b58}: {e}"))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse RPC response for amm_id: {e}"))?;

    let data_b64 = json["result"]["value"]["data"][0]
        .as_str()
        .ok_or_else(|| format!("no account data for amm_id {amm_id_b58}"))?;

    let amm_data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("base64 decode error for amm_id: {e}"))?;

    if amm_data.len() < 560 {
        return Err(format!(
            "AMM account data too short: {} bytes (expected >= 560)",
            amm_data.len()
        ));
    }

    // Parse AMM layout offsets (little-endian)
    let nonce = u64::from_le_bytes(amm_data[8..16].try_into().unwrap()) as u8;
    let amm_open_orders: [u8; 32] = amm_data[432..464].try_into().unwrap();
    let serum_market: [u8; 32] = amm_data[464..496].try_into().unwrap();
    let serum_program_id: [u8; 32] = amm_data[496..528].try_into().unwrap();
    let amm_target_orders: [u8; 32] = amm_data[528..560].try_into().unwrap();

    // Derive amm_authority PDA
    let amm_authority = derive_amm_authority(nonce)
        .map_err(|e| format!("failed to derive amm_authority: {e}"))?;

    // ── Step 2: Fetch Serum market account data ──────────────────────────
    let serum_market_b58 = bs58::encode(&serum_market).into_string();
    let body2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "getAccountInfo",
        "params": [
            serum_market_b58,
            { "encoding": "base64" }
        ]
    });

    let resp2 = client
        .post(rpc_url)
        .json(&body2)
        .send()
        .await
        .map_err(|e| format!("RPC request failed for serum_market {serum_market_b58}: {e}"))?;

    let json2: serde_json::Value = resp2
        .json()
        .await
        .map_err(|e| format!("failed to parse RPC response for serum_market: {e}"))?;

    let data2_b64 = json2["result"]["value"]["data"][0]
        .as_str()
        .ok_or_else(|| format!("no account data for serum_market {serum_market_b58}"))?;

    let market_data = base64::engine::general_purpose::STANDARD
        .decode(data2_b64)
        .map_err(|e| format!("base64 decode error for serum_market: {e}"))?;

    if market_data.len() < 437 {
        return Err(format!(
            "Serum market data too short: {} bytes (expected >= 437)",
            market_data.len()
        ));
    }

    // Serum MarketState layout (after 5 bytes padding+flag)
    let serum_coin_vault: [u8; 32] = market_data[129..161].try_into().unwrap();
    let serum_pc_vault: [u8; 32] = market_data[201..233].try_into().unwrap();
    let serum_bids: [u8; 32] = market_data[261..293].try_into().unwrap();
    let serum_asks: [u8; 32] = market_data[293..325].try_into().unwrap();
    let serum_event_queue: [u8; 32] = market_data[357..389].try_into().unwrap();
    let vault_signer_nonce = u64::from_le_bytes(market_data[429..437].try_into().unwrap());

    // Derive serum vault signer PDA
    let serum_program_pubkey = Pubkey::new_from_array(serum_program_id);
    let serum_market_pubkey = Pubkey::new_from_array(serum_market);
    let serum_vault_signer = Pubkey::create_program_address(
        &[
            serum_market_pubkey.as_ref(),
            &vault_signer_nonce.to_le_bytes(),
        ],
        &serum_program_pubkey,
    )
    .map_err(|_| "failed to derive serum_vault_signer PDA".to_string())?;

    Ok(RaydiumPoolAccounts {
        amm_id: *amm_id,
        amm_authority: amm_authority.to_bytes(),
        amm_open_orders,
        amm_target_orders,
        serum_program_id,
        serum_market,
        serum_bids,
        serum_asks,
        serum_event_queue,
        serum_coin_vault,
        serum_pc_vault,
        serum_vault_signer: serum_vault_signer.to_bytes(),
        coin_vault,
        pc_vault,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;

    /// Create a dummy RaydiumPoolAccounts for testing.
    fn dummy_pool() -> RaydiumPoolAccounts {
        RaydiumPoolAccounts {
            amm_id: [1u8; 32],
            amm_authority: [2u8; 32],
            amm_open_orders: [3u8; 32],
            amm_target_orders: [4u8; 32],
            serum_program_id: [5u8; 32],
            serum_market: [6u8; 32],
            serum_bids: [7u8; 32],
            serum_asks: [8u8; 32],
            serum_event_queue: [9u8; 32],
            serum_coin_vault: [10u8; 32],
            serum_pc_vault: [11u8; 32],
            serum_vault_signer: [12u8; 32],
            coin_vault: [13u8; 32],
            pc_vault: [14u8; 32],
        }
    }

    fn dummy_mint() -> [u8; 32] {
        [42u8; 32]
    }

    fn dummy_blockhash() -> [u8; 32] {
        [0xAA; 32]
    }

    #[test]
    fn test_swap_base_in_data() {
        let amount_in = 1_000_000u64;
        let min_out = 500_000u64;
        let data = build_swap_base_in_data(amount_in, min_out);
        assert_eq!(data.len(), 17);
        assert_eq!(data[0], 9u8);
        assert_eq!(
            u64::from_le_bytes(data[1..9].try_into().unwrap()),
            amount_in
        );
        assert_eq!(
            u64::from_le_bytes(data[9..17].try_into().unwrap()),
            min_out
        );
    }

    #[test]
    fn test_token_ata_deterministic() {
        let wallet = Keypair::new();
        let mint = Pubkey::new_unique();
        let ata1 = token_ata(&wallet.pubkey(), &mint);
        let ata2 = token_ata(&wallet.pubkey(), &mint);
        assert_eq!(ata1, ata2, "ATA derivation must be deterministic");

        // Different mint → different ATA
        let mint2 = Pubkey::new_unique();
        let ata3 = token_ata(&wallet.pubkey(), &mint2);
        assert_ne!(ata1, ata3, "Different mints must produce different ATAs");
    }

    #[test]
    fn test_sell_tx_instruction_count() {
        // Build a sell tx and deserialize to verify 5 instructions
        let pool = dummy_pool();
        let mint = dummy_mint();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();

        let tx_bytes = build_raydium_sell_tx(
            &pool,
            &mint,
            &kp,
            1_000_000,   // tokens_to_sell
            500_000,     // min_sol_out
            10_000,      // jito_tip
            tip_account,
            dummy_blockhash(),
        )
        .expect("sell tx build should succeed");

        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        let msg = tx.message;
        match &msg {
            VersionedMessage::V0(m) => {
                assert_eq!(
                    m.instructions.len(),
                    5,
                    "sell tx should have 5 instructions (cu_limit, cu_price, swap, close, tip)"
                );
            }
            _ => panic!("expected V0 message"),
        }
    }

    #[test]
    fn test_buy_tx_instruction_count() {
        // Build a buy tx and deserialize to verify 9 instructions
        let pool = dummy_pool();
        let mint = dummy_mint();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();

        let tx_bytes = build_raydium_buy_tx(
            &pool,
            &mint,
            &kp,
            1_000_000_000,  // sol_lamports (1 SOL)
            500_000,        // min_tokens_out
            10_000,         // jito_tip
            tip_account,
            dummy_blockhash(),
        )
        .expect("buy tx build should succeed");

        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        let msg = tx.message;
        match &msg {
            VersionedMessage::V0(m) => {
                assert_eq!(
                    m.instructions.len(),
                    9,
                    "buy tx should have 9 instructions (cu_limit, cu_price, create_wsol_ata, create_token_ata, fund, sync, swap, close, tip)"
                );
            }
            _ => panic!("expected V0 message"),
        }
    }

    #[test]
    fn test_sell_tx_verifies_signature() {
        // A properly built tx should have exactly 1 valid signature
        let pool = dummy_pool();
        let mint = dummy_mint();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();

        let tx_bytes = build_raydium_sell_tx(
            &pool,
            &mint,
            &kp,
            1_000_000,
            500_000,
            10_000,
            tip_account,
            dummy_blockhash(),
        )
        .expect("sell tx build should succeed");

        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1, "should have exactly 1 signature");
        assert_ne!(
            tx.signatures[0],
            solana_sdk::signature::Signature::default(),
            "signature should not be zeroed"
        );
    }

    #[test]
    fn test_buy_tx_verifies_signature() {
        let pool = dummy_pool();
        let mint = dummy_mint();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();

        let tx_bytes = build_raydium_buy_tx(
            &pool,
            &mint,
            &kp,
            1_000_000_000,
            500_000,
            10_000,
            tip_account,
            dummy_blockhash(),
        )
        .expect("buy tx build should succeed");

        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1, "should have exactly 1 signature");
        assert_ne!(
            tx.signatures[0],
            solana_sdk::signature::Signature::default(),
            "signature should not be zeroed"
        );
    }
}
