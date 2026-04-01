//! PumpSwap AMM swap instruction builder.
//!
//! Builds complete signed `VersionedTransaction`s for buy (SOL → Token) and
//! sell (Token → SOL) swaps through PumpSwap's AMM program.
//!
//! No dependency on `spl-token` or `spl-associated-token-account` crates —
//! all SPL instructions and ATA derivation are built manually to avoid the
//! zeroize version conflict with rustls.

use std::str::FromStr;

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

// ── PumpSwap constants ───────────────────────────────────────────────────────

/// PumpSwap AMM program ID.
pub const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// PumpSwap global config PDA.
pub const PUMPSWAP_GLOBAL_CONFIG: &str = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw";

/// PumpSwap event authority PDA.
pub const PUMPSWAP_EVENT_AUTHORITY: &str = "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR";

/// Coin fee program.
pub const PUMPSWAP_FEE_PROGRAM: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";

/// Coin fee config account.
pub const PUMPSWAP_FEE_PROG_STATE: &str = "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx";

/// Coin fee program state account.
pub const PUMPSWAP_FEE_PROG_STATE2: &str = "4Jjna3h73QbgmdqwnV5NJxjCidKWB7Q26jeuj9jtFetC";

/// Discriminator for both buy and sell (same Anchor discriminator, different arg semantics).
pub const PUMPSWAP_SWAP_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

/// 8 protocol fee recipients — rotate randomly per tx.
pub const PUMPSWAP_FEE_RECIPIENTS: [&str; 8] = [
    "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV",
    "7VtfL8fvgNfhz17qKRMjzQEXgbdpnHHHQRh54R9jP2RJ",
    "7hTckgnGnLQR6sdH7YkqFTAA7VwTfYFaZ6EhEsU3saCX",
    "9rPYyANsfQZw3DnDmKE3YCQF5E8oD89UXoHn9JFEhJUz",
    "AVmoTthdrX6tKt4nDjco2D775W2YK3sDhxPcMmzUAmTY",
    "FWsW1xNtWscwNmKv6wVsU1iTzRN6wmmk3MjxRP5tT7hz",
    "G5UZAVbAf46s7cKWoyKu8kYTip9DGTpbLZ2qa9Aq69dP",
    "JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU",
];

/// Wrapped SOL mint.
pub const WSOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

/// SPL Token program ID.
pub const SPL_TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Associated Token Account program ID.
pub const SPL_ATA_PROGRAM_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

// ── Pool accounts ────────────────────────────────────────────────────────────

/// Pool accounts required for a PumpSwap swap.
/// Populated from PoolResolution at graduation time, stored in pumpswap_pools DashMap.
///
/// Matches `momentum::pool::PumpSwapPoolAccounts` structurally so the two types
/// are interchangeable. Fixed-address accounts (coin_fee_config, coin_fee_program_state)
/// are resolved from constants at instruction build time.
#[derive(Debug, Clone)]
pub struct PumpSwapPoolAccounts {
    /// Pool PDA address (PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// Token mint (PoolResolution.mint) = base_mint in PumpSwap terms
    pub base_mint: [u8; 32],
    /// Pool token vault = PoolResolution.coin_vault = pool_base_token_account
    pub pool_base_token_account: [u8; 32],
    /// Pool WSOL vault = PoolResolution.pc_vault = pool_quote_token_account
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA (may be zeroed if not applicable; always include in ix)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority (may be zeroed; always include in ix)
    pub coin_creator_vault_authority: [u8; 32],
}

/// Convert from `momentum::pool::PumpSwapPoolAccounts` to `tx::pumpswap::PumpSwapPoolAccounts`.
/// Both structs have identical fields; this bridges the module boundary.
impl From<crate::momentum::pool::PumpSwapPoolAccounts> for PumpSwapPoolAccounts {
    fn from(p: crate::momentum::pool::PumpSwapPoolAccounts) -> Self {
        Self {
            pool: p.pool,
            base_mint: p.base_mint,
            pool_base_token_account: p.pool_base_token_account,
            pool_quote_token_account: p.pool_quote_token_account,
            coin_creator_vault_ata: p.coin_creator_vault_ata,
            coin_creator_vault_authority: p.coin_creator_vault_authority,
        }
    }
}

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum PumpSwapTxError {
    InvalidPubkey(String),
    SignError(String),
}

impl std::fmt::Display for PumpSwapTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPubkey(s) => write!(f, "invalid pubkey: {s}"),
            Self::SignError(s) => write!(f, "transaction signing error: {s}"),
        }
    }
}

impl std::error::Error for PumpSwapTxError {}

// ── Helpers (copied from raydium.rs — NOT imported) ──────────────────────────

/// Derive the associated token address for `wallet` + `mint`.
/// Equivalent to `spl_associated_token_account::get_associated_token_address`.
///
/// PDA seeds: [wallet, token_program, mint] under the ATA program.
fn token_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let (addr, _bump) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );
    addr
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

// ── Instruction data ─────────────────────────────────────────────────────────

/// Build 24-byte PumpSwap swap instruction data.
/// buy:  [discriminator(8)] + base_out(u64 LE) + max_quote_in(u64 LE)
/// sell: [discriminator(8)] + base_in(u64 LE) + min_quote_out(u64 LE)
/// Both use the same discriminator and same arg layout.
fn build_swap_data(arg1: u64, arg2: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&PUMPSWAP_SWAP_DISCRIMINATOR);
    data.extend_from_slice(&arg1.to_le_bytes());
    data.extend_from_slice(&arg2.to_le_bytes());
    data
}

// ── Swap instruction builder ─────────────────────────────────────────────────

/// Build the PumpSwap swap instruction with the 22-account layout.
///
/// Account order matches on-chain verified layout:
///   [0]  pool                              (writable)
///   [1]  user                              (signer, writable)
///   [2]  global_config                     (readonly)
///   [3]  base_mint                         (readonly)
///   [4]  quote_mint (WSOL)                 (readonly)
///   [5]  user_base_token_account           (writable)
///   [6]  user_quote_token_account          (writable)
///   [7]  pool_base_token_account           (writable)
///   [8]  pool_quote_token_account          (writable)
///   [9]  protocol_fee_recipient            (writable)
///   [10] protocol_fee_recipient_token_acct (writable)
///   [11] base_token_program               (readonly)
///   [12] quote_token_program              (readonly)
///   [13] system_program                   (readonly)
///   [14] associated_token_program         (readonly)
///   [15] event_authority                  (readonly)
///   [16] pump_program                     (readonly)
///   [17] coin_creator_vault_ata           (writable)
///   [18] coin_creator_vault_authority     (writable)
///   [19] coin_fee_config                  (readonly)
///   [20] coin_fee_program                 (readonly)
///   [21] coin_fee_program_state           (readonly)
fn build_pumpswap_swap_ix(
    pool: &PumpSwapPoolAccounts,
    wallet_pubkey: &Pubkey,
    fee_recipient_idx: usize,
    arg1: u64,
    arg2: u64,
) -> Instruction {
    let pumpswap_program = Pubkey::from_str(PUMPSWAP_PROGRAM).unwrap();
    let global_config = Pubkey::from_str(PUMPSWAP_GLOBAL_CONFIG).unwrap();
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let event_authority = Pubkey::from_str(PUMPSWAP_EVENT_AUTHORITY).unwrap();
    let fee_program = Pubkey::from_str(PUMPSWAP_FEE_PROGRAM).unwrap();

    let base_mint = Pubkey::new_from_array(pool.base_mint);
    let user_base_ata = token_ata(wallet_pubkey, &base_mint);
    let user_quote_ata = token_ata(wallet_pubkey, &wsol_mint);

    // Fee recipient rotation
    let fee_recipient = Pubkey::from_str(
        PUMPSWAP_FEE_RECIPIENTS[fee_recipient_idx % 8],
    )
    .unwrap();
    let fee_recipient_token_account = token_ata(&fee_recipient, &wsol_mint);

    // coin_creator_vault_ata / authority: Pubkey::default() when zeroed — program handles it
    let coin_creator_vault_ata = Pubkey::new_from_array(pool.coin_creator_vault_ata);
    let coin_creator_vault_authority = Pubkey::new_from_array(pool.coin_creator_vault_authority);
    // Fixed-address accounts — always the same for all PumpSwap pools
    let coin_fee_config = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE).unwrap();
    let coin_fee_program_state = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE2).unwrap();

    let accounts = vec![
        AccountMeta::new(Pubkey::new_from_array(pool.pool), false),              // [0]  pool
        AccountMeta::new(*wallet_pubkey, true),                                   // [1]  user (signer)
        AccountMeta::new_readonly(global_config, false),                          // [2]  global_config
        AccountMeta::new_readonly(base_mint, false),                              // [3]  base_mint
        AccountMeta::new_readonly(wsol_mint, false),                              // [4]  quote_mint
        AccountMeta::new(user_base_ata, false),                                   // [5]  user_base_token_account
        AccountMeta::new(user_quote_ata, false),                                  // [6]  user_quote_token_account
        AccountMeta::new(Pubkey::new_from_array(pool.pool_base_token_account), false),  // [7]
        AccountMeta::new(Pubkey::new_from_array(pool.pool_quote_token_account), false), // [8]
        AccountMeta::new(fee_recipient, false),                                   // [9]  protocol_fee_recipient
        AccountMeta::new(fee_recipient_token_account, false),                     // [10] fee_recipient_token_acct
        AccountMeta::new_readonly(token_program, false),                          // [11] base_token_program
        AccountMeta::new_readonly(token_program, false),                          // [12] quote_token_program
        AccountMeta::new_readonly(system_program::id(), false),                   // [13] system_program
        AccountMeta::new_readonly(ata_program, false),                            // [14] associated_token_program
        AccountMeta::new_readonly(event_authority, false),                        // [15] event_authority
        AccountMeta::new_readonly(pumpswap_program, false),                       // [16] pump_program (self CPI)
        AccountMeta::new(coin_creator_vault_ata, false),                          // [17] coin_creator_vault_ata
        AccountMeta::new(coin_creator_vault_authority, false),                    // [18] coin_creator_vault_authority
        AccountMeta::new_readonly(coin_fee_config, false),                        // [19] coin_fee_config
        AccountMeta::new_readonly(fee_program, false),                            // [20] coin_fee_program
        AccountMeta::new_readonly(coin_fee_program_state, false),                 // [21] coin_fee_program_state
    ];

    Instruction {
        program_id: pumpswap_program,
        accounts,
        data: build_swap_data(arg1, arg2),
    }
}

// ── Buy: SOL → Token ─────────────────────────────────────────────────────────

/// Build a complete signed PumpSwap BUY transaction (SOL → Token).
///
/// Instruction sequence:
///   1. ComputeBudget::set_compute_unit_limit(400_000)
///   2. ComputeBudget::set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, base_mint) — ensure token ATA
///   4. create_associated_token_account_idempotent(user, WSOL) — ensure WSOL ATA
///   5. system_instruction::transfer(wallet → wsol_ata, sol_lamports) — fund WSOL ATA
///   6. spl_token::sync_native(wsol_ata) — wrap SOL → WSOL
///   7. pumpswap swap ix (22 accounts, discriminator + base_out + max_quote_in)
///   8. spl_token::close_account(wsol_ata → wallet) — reclaim leftover WSOL
///   9. system_instruction::transfer(wallet → jito_tip_account, tip_lamports)
///
/// Returns: serialized VersionedTransaction bytes (bincode).
pub fn build_pumpswap_buy_tx(
    pool: &PumpSwapPoolAccounts,
    wallet_keypair: &Keypair,
    sol_lamports: u64,
    min_tokens_out: u64,
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
    fee_recipient_idx: usize,
) -> Result<Vec<u8>, PumpSwapTxError> {
    let wallet_pubkey = wallet_keypair.pubkey();
    let base_mint = Pubkey::new_from_array(pool.base_mint);
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let blockhash = Hash::new_from_array(recent_blockhash);

    // Derive ATAs
    let wsol_ata_addr = token_ata(&wallet_pubkey, &wsol_mint);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(400_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Create token ATA (idempotent) — ensure base_mint ATA exists
    let ix_create_token_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &base_mint,
    );

    // 4. Create WSOL ATA (idempotent)
    let ix_create_wsol_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &wsol_mint,
    );

    // 5. Fund WSOL ATA with SOL
    let ix_fund_wsol = system_instruction::transfer(&wallet_pubkey, &wsol_ata_addr, sol_lamports);

    // 6. Sync native to wrap deposited SOL → WSOL
    let ix_sync = build_sync_native_ix(&wsol_ata_addr);

    // 7. PumpSwap swap: buy(base_out=min_tokens_out, max_quote_in=sol_lamports)
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        min_tokens_out,  // base_out
        sol_lamports,    // max_quote_in
    );

    // 8. Close WSOL ATA → wallet (reclaim leftover WSOL)
    let ix_close = build_close_account_ix(&wsol_ata_addr, &wallet_pubkey, &wallet_pubkey);

    // 9. Jito tip
    let ix_tip = system_instruction::transfer(&wallet_pubkey, &jito_tip_account, jito_tip_lamports);

    let ixs = vec![
        ix_cu_limit,
        ix_cu_price,
        ix_create_token_ata,
        ix_create_wsol_ata,
        ix_fund_wsol,
        ix_sync,
        ix_swap,
        ix_close,
        ix_tip,
    ];

    // Compile V0 message (no address lookup tables)
    let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to compile V0 message: {e}")))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[wallet_keypair])
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to sign transaction: {e}")))?;

    bincode::serialize(&tx)
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to serialize transaction: {e}")))
}

// ── Sell: Token → SOL ────────────────────────────────────────────────────────

/// Build a complete signed PumpSwap SELL transaction (Token → SOL).
///
/// Instruction sequence:
///   1. ComputeBudget::set_compute_unit_limit(300_000)
///   2. ComputeBudget::set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, WSOL) — ensure WSOL ATA
///   4. pumpswap swap ix (22 accounts, discriminator + base_in + min_quote_out)
///   5. spl_token::close_account(wsol_ata → wallet) — close WSOL ATA, SOL flows back
///   6. system_instruction::transfer(wallet → jito_tip_account, tip_lamports)
///
/// Returns: serialized VersionedTransaction bytes (bincode).
pub fn build_pumpswap_sell_tx(
    pool: &PumpSwapPoolAccounts,
    wallet_keypair: &Keypair,
    tokens_to_sell: u64,
    min_sol_out: u64,
    jito_tip_lamports: u64,
    jito_tip_account: Pubkey,
    recent_blockhash: [u8; 32],
    fee_recipient_idx: usize,
) -> Result<Vec<u8>, PumpSwapTxError> {
    let wallet_pubkey = wallet_keypair.pubkey();
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let blockhash = Hash::new_from_array(recent_blockhash);

    // Derive WSOL ATA
    let wsol_ata_addr = token_ata(&wallet_pubkey, &wsol_mint);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(300_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Create WSOL ATA (idempotent)
    let ix_create_wsol_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &wsol_mint,
    );

    // 4. PumpSwap swap: sell(base_in=tokens_to_sell, min_quote_out=min_sol_out)
    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        tokens_to_sell,  // base_in
        min_sol_out,     // min_quote_out
    );

    // 5. Close WSOL ATA → wallet (SOL flows back to wallet)
    let ix_close = build_close_account_ix(&wsol_ata_addr, &wallet_pubkey, &wallet_pubkey);

    // 6. Jito tip
    let ix_tip = system_instruction::transfer(&wallet_pubkey, &jito_tip_account, jito_tip_lamports);

    let ixs = vec![
        ix_cu_limit,
        ix_cu_price,
        ix_create_wsol_ata,
        ix_swap,
        ix_close,
        ix_tip,
    ];

    // Compile V0 message (no address lookup tables)
    let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to compile V0 message: {e}")))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[wallet_keypair])
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to sign transaction: {e}")))?;

    bincode::serialize(&tx)
        .map_err(|e| PumpSwapTxError::SignError(format!("failed to serialize transaction: {e}")))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;

    /// Create a dummy PumpSwapPoolAccounts for testing.
    fn dummy_pool() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            base_mint: [42u8; 32],
            pool_base_token_account: [3u8; 32],
            pool_quote_token_account: [4u8; 32],
            coin_creator_vault_ata: [0u8; 32],       // zeroed — program handles it
            coin_creator_vault_authority: [0u8; 32],  // zeroed — program handles it
        }
    }

    fn dummy_blockhash() -> [u8; 32] {
        [0xAA; 32]
    }

    fn build_buy_tx_helper(fee_idx: usize) -> Vec<u8> {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();
        build_pumpswap_buy_tx(
            &pool,
            &kp,
            1_000_000_000,  // sol_lamports (1 SOL)
            1,              // min_tokens_out
            10_000,         // jito_tip
            tip_account,
            dummy_blockhash(),
            fee_idx,
        )
        .expect("buy tx build should succeed")
    }

    fn build_sell_tx_helper(fee_idx: usize, min_sol_out: u64) -> Vec<u8> {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let tip_account = Pubkey::new_unique();
        build_pumpswap_sell_tx(
            &pool,
            &kp,
            1_000_000,    // tokens_to_sell
            min_sol_out,
            10_000,       // jito_tip
            tip_account,
            dummy_blockhash(),
            fee_idx,
        )
        .expect("sell tx build should succeed")
    }

    // ── 1. test_discriminator_correct ────────────────────────────────────

    #[test]
    fn test_discriminator_correct() {
        let data = build_swap_data(100, 200);
        assert_eq!(
            &data[..8],
            &[0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad],
            "discriminator must match PumpSwap swap discriminator"
        );
    }

    // ── 2. test_buy_data_length_24 ───────────────────────────────────────

    #[test]
    fn test_buy_data_length_24() {
        let data = build_swap_data(1, 1_000_000_000);
        assert_eq!(data.len(), 24, "buy swap data must be exactly 24 bytes");
    }

    // ── 3. test_sell_data_length_24 ──────────────────────────────────────

    #[test]
    fn test_sell_data_length_24() {
        let data = build_swap_data(1_000_000, 0);
        assert_eq!(data.len(), 24, "sell swap data must be exactly 24 bytes");
    }

    // ── 4. test_buy_tx_9_instructions ────────────────────────────────────

    #[test]
    fn test_buy_tx_9_instructions() {
        let tx_bytes = build_buy_tx_helper(0);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        match &tx.message {
            VersionedMessage::V0(m) => {
                assert_eq!(
                    m.instructions.len(),
                    9,
                    "buy tx should have 9 instructions"
                );
            }
            _ => panic!("expected V0 message"),
        }
    }

    // ── 5. test_sell_tx_6_instructions ───────────────────────────────────

    #[test]
    fn test_sell_tx_6_instructions() {
        let tx_bytes = build_sell_tx_helper(0, 500_000);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        match &tx.message {
            VersionedMessage::V0(m) => {
                assert_eq!(
                    m.instructions.len(),
                    6,
                    "sell tx should have 6 instructions"
                );
            }
            _ => panic!("expected V0 message"),
        }
    }

    // ── 6. test_buy_tx_signature_nonzero ─────────────────────────────────

    #[test]
    fn test_buy_tx_signature_nonzero() {
        let tx_bytes = build_buy_tx_helper(0);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1, "should have exactly 1 signature");
        assert_ne!(
            tx.signatures[0],
            solana_sdk::signature::Signature::default(),
            "signature should not be zeroed"
        );
    }

    // ── 7. test_sell_tx_signature_nonzero ────────────────────────────────

    #[test]
    fn test_sell_tx_signature_nonzero() {
        let tx_bytes = build_sell_tx_helper(0, 500_000);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1, "should have exactly 1 signature");
        assert_ne!(
            tx.signatures[0],
            solana_sdk::signature::Signature::default(),
            "signature should not be zeroed"
        );
    }

    // ── 8. test_buy_tx_v0_message_format ─────────────────────────────────

    #[test]
    fn test_buy_tx_v0_message_format() {
        let tx_bytes = build_buy_tx_helper(0);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert!(
            matches!(tx.message, VersionedMessage::V0(_)),
            "buy tx must use V0 message format"
        );
    }

    // ── 9. test_fee_recipient_idx0 ──────────────────────────────────────

    #[test]
    fn test_fee_recipient_idx0() {
        // Build swap ix with fee_idx=0 and verify account[9] = 62qc2CNX...
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, 1, 1_000_000);
        let expected = Pubkey::from_str("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV").unwrap();
        assert_eq!(
            ix.accounts[9].pubkey, expected,
            "fee_idx=0 should produce 62qc2CNX... as protocol_fee_recipient"
        );
    }

    // ── 10. test_fee_recipient_idx7 ─────────────────────────────────────

    #[test]
    fn test_fee_recipient_idx7() {
        // Build swap ix with fee_idx=7 and verify account[9] = JCRGumo...
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 7, 1, 1_000_000);
        let expected = Pubkey::from_str("JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU").unwrap();
        assert_eq!(
            ix.accounts[9].pubkey, expected,
            "fee_idx=7 should produce JCRGumo... as protocol_fee_recipient"
        );
    }

    // ── 11. test_sell_min_sol_zero_accepted ──────────────────────────────

    #[test]
    fn test_sell_min_sol_zero_accepted() {
        // min_sol_out=0 should build successfully (accept any price)
        let tx_bytes = build_sell_tx_helper(0, 0);
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1);
    }

    // ── 12. test_buy_min_tokens_one_accepted ────────────────────────────

    #[test]
    fn test_buy_min_tokens_one_accepted() {
        // min_tokens_out=1 should build successfully (accept any amount)
        let tx_bytes = build_buy_tx_helper(0);  // helper uses min_tokens_out=1
        let tx: VersionedTransaction =
            bincode::deserialize(&tx_bytes).expect("should deserialize");
        assert_eq!(tx.signatures.len(), 1);
    }

    // ── 13. test_swap_ix_has_22_accounts ─────────────────────────────────

    #[test]
    fn test_swap_ix_has_22_accounts() {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, 1, 1_000_000);
        assert_eq!(ix.accounts.len(), 22, "PumpSwap swap ix must have 22 accounts");
    }

    // ── 14. test_fee_recipient_wraps_around ──────────────────────────────

    #[test]
    fn test_fee_recipient_wraps_around() {
        // idx=8 should wrap to idx=0
        let pool = dummy_pool();
        let kp = Keypair::new();
        let pubkey = kp.pubkey();
        let ix0 = build_pumpswap_swap_ix(&pool, &pubkey, 0, 1, 1);
        let ix8 = build_pumpswap_swap_ix(&pool, &pubkey, 8, 1, 1);
        assert_eq!(
            ix0.accounts[9].pubkey, ix8.accounts[9].pubkey,
            "fee_idx=8 should wrap to same as fee_idx=0"
        );
    }

    // ── 15. test_swap_data_args_encoded_correctly ────────────────────────

    #[test]
    fn test_swap_data_args_encoded_correctly() {
        let arg1: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let arg2: u64 = 0x1234_5678_9ABC_DEF0;
        let data = build_swap_data(arg1, arg2);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            arg1,
            "arg1 should be LE-encoded at offset 8"
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            arg2,
            "arg2 should be LE-encoded at offset 16"
        );
    }
}