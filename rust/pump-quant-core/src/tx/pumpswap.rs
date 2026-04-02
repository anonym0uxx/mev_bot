//! PumpSwap AMM swap instruction builder.
//!
//! Builds complete signed `VersionedTransaction`s for buy (SOL → Token) and
//! sell (Token → SOL) swaps through PumpSwap's AMM program.
//!
//! **Pool ordering.** PumpSwap sorts mints by raw byte comparison.
//! WSOL (0x069b…) sorts before most pump.fun tokens, so ~81% of pools have
//! WSOL as base_mint and the token as quote_mint ("reversed" ordering).
//! This module handles both orderings correctly:
//! - Accounts [3]-[8] are placed in the pool's actual on-chain base/quote order
//! - The instruction discriminator flips: BUY uses `sell` disc for reversed pools
//!   (selling WSOL-base to get token-quote) and vice versa
//!
//! No dependency on `spl-token` or `spl-associated-token-account` crates —
//! all SPL instructions and ATA derivation are built manually to avoid the
//! zeroize version conflict with rustls.
//!
//! ## TODO for eng1 (pool.rs owner)
//! Consider adding `token_is_base: bool` to `momentum::pool::PumpSwapPoolAccounts`
//! so the From conversion doesn't need to re-derive it from mint bytes.
//! Currently the From impl computes it as `base_mint < WSOL_MINT_BYTES`.
//! If pool.rs already knows the ordering at resolution time, passing it through
//! avoids redundant work and makes the contract explicit.

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

/// PumpSwap `buy` instruction discriminator = sha256("global:buy")[..8].
/// buy(base_out: u64, max_quote_in: u64) — buy base tokens by paying quote tokens.
pub const PUMPSWAP_BUY_DISCRIMINATOR: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];

/// PumpSwap `sell` instruction discriminator = sha256("global:sell")[..8].
/// sell(base_in: u64, min_quote_out: u64) — sell base tokens for quote tokens.
pub const PUMPSWAP_SELL_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

/// Legacy alias — kept for existing tests only.
#[deprecated(note = "Use PUMPSWAP_BUY_DISCRIMINATOR or PUMPSWAP_SELL_DISCRIMINATOR")]
pub const PUMPSWAP_SWAP_DISCRIMINATOR: [u8; 8] = PUMPSWAP_SELL_DISCRIMINATOR;

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

/// SPL Token program ID (classic).
pub const SPL_TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Token-2022 (Token Extensions) program ID.
/// Pump.fun graduated tokens use this program, NOT classic SPL Token.
pub const SPL_TOKEN_2022_PROGRAM_STR: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// SPL Associated Token Account program ID.
pub const SPL_ATA_PROGRAM_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

// ── Pool accounts ────────────────────────────────────────────────────────────

/// Pool accounts required for a PumpSwap swap.
/// Populated from PoolResolution at graduation time, stored in pumpswap_pools DashMap.
///
/// **Pool ordering matters.** PumpSwap sorts mints by raw byte comparison.
/// When WSOL < token (the majority ~81% case), WSOL is base_mint on-chain.
/// The TX builder must pass accounts and instruction args in the pool's actual
/// on-chain ordering (base/quote), not our normalized coin/pc ordering.
///
/// Fields `pool_base_token_account` and `pool_quote_token_account` always store
/// the pool's on-chain base and quote vaults respectively. The `token_is_base`
/// flag tells the TX builder which vault holds the token vs WSOL.
#[derive(Debug, Clone)]
pub struct PumpSwapPoolAccounts {
    /// Pool PDA address (PoolResolution.pool_address)
    pub pool: [u8; 32],
    /// The traded token mint (always the non-WSOL token, regardless of pool ordering)
    pub base_mint: [u8; 32],
    /// Pool's on-chain base vault (pool_base_token_account at offset 139).
    /// If token_is_base: this is the TOKEN vault.
    /// If !token_is_base: this is the WSOL vault.
    pub pool_base_token_account: [u8; 32],
    /// Pool's on-chain quote vault (pool_quote_token_account at offset 171).
    /// If token_is_base: this is the WSOL vault.
    /// If !token_is_base: this is the TOKEN vault.
    pub pool_quote_token_account: [u8; 32],
    /// Coin creator vault ATA (may be zeroed if not applicable; always include in ix)
    pub coin_creator_vault_ata: [u8; 32],
    /// Coin creator vault authority (may be zeroed; always include in ix)
    pub coin_creator_vault_authority: [u8; 32],
    /// Whether the token is the pool's base_mint (true) or quote_mint (false).
    /// When false, WSOL is base and the token is quote (the "reversed" ~81% case).
    /// Determines instruction discriminator choice and arg ordering.
    pub token_is_base: bool,
    /// Token program that owns the traded token mint (SPL Token or Token-2022).
    /// Resolved at graduation time from Helius notification or RPC.
    /// Defaults to [0u8; 32] (unresolved) — TX builder must check and resolve.
    pub token_mint_program: [u8; 32],
}

/// WSOL mint as raw bytes for detecting reversed PumpSwap pool ordering.
const WSOL_MINT_BYTES: [u8; 32] = [
    0x06, 0x9b, 0x88, 0x57, 0xfe, 0xab, 0x81, 0x84,
    0xfb, 0x68, 0x7f, 0x63, 0x46, 0x18, 0xc0, 0x35,
    0xda, 0xc4, 0x39, 0xdc, 0x1a, 0xeb, 0x3b, 0x55,
    0x98, 0xa0, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x01,
];

/// Convert from `momentum::pool::PumpSwapPoolAccounts` to `tx::pumpswap::PumpSwapPoolAccounts`.
///
/// **Critical: vault re-ordering for reversed pools.**
///
/// The upstream pool.rs struct uses a NORMALIZED convention:
///   `pool_base_token_account` = always TOKEN vault (from PoolResolution.coin_vault)
///   `pool_quote_token_account` = always WSOL vault (from PoolResolution.pc_vault)
///
/// But PumpSwap's on-chain layout and instruction expect vaults in the pool's
/// actual base/quote order. For reversed pools (WSOL=base), the on-chain layout is:
///   pool_base_token_account [offset 139] = WSOL vault
///   pool_quote_token_account [offset 171] = TOKEN vault
///
/// So we swap the vaults when converting reversed pools to match on-chain order.
impl From<crate::momentum::pool::PumpSwapPoolAccounts> for PumpSwapPoolAccounts {
    fn from(p: crate::momentum::pool::PumpSwapPoolAccounts) -> Self {
        // Determine pool ordering: if token mint < WSOL bytes, token is base (normal).
        // Otherwise WSOL is base (reversed).
        let token_is_base = p.base_mint < WSOL_MINT_BYTES;

        // Swap vaults to match on-chain ordering for reversed pools.
        // p.pool_base_token_account = TOKEN vault (normalized)
        // p.pool_quote_token_account = WSOL vault (normalized)
        let (onchain_base_vault, onchain_quote_vault) = if token_is_base {
            // Normal: token=base on-chain → base_vault=token, quote_vault=WSOL
            (p.pool_base_token_account, p.pool_quote_token_account)
        } else {
            // Reversed: WSOL=base on-chain → base_vault=WSOL, quote_vault=token
            (p.pool_quote_token_account, p.pool_base_token_account)
        };

        Self {
            pool: p.pool,
            base_mint: p.base_mint,
            pool_base_token_account: onchain_base_vault,
            pool_quote_token_account: onchain_quote_vault,
            coin_creator_vault_ata: p.coin_creator_vault_ata,
            coin_creator_vault_authority: p.coin_creator_vault_authority,
            token_is_base,
            token_mint_program: p.token_mint_program,
        }
    }
}

/// Build `PumpSwapPoolAccounts` deterministically from `CreatePoolExtracted`.
///
/// This is the zero-RPC path: all data comes from the create_pool instruction
/// accounts (available in the graduation transaction). No getProgramAccounts,
/// no getTransaction, no pool data fetching needed.
///
/// Handles pool ordering normalization and creator ATA derivation.
pub fn build_pool_accounts_from_create_pool(
    extracted: &crate::momentum::pool::CreatePoolExtracted,
) -> PumpSwapPoolAccounts {
    let upstream = crate::momentum::pool::build_pumpswap_pool_accounts_deterministic(extracted);
    PumpSwapPoolAccounts::from(upstream)
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
/// SPL Token program raw bytes (public for use in mint program resolution fallback).
pub const SPL_TOKEN_PROGRAM_BYTES: [u8; 32] = [
    6,221,246,225, 215,101,161,147,
    217,203,225, 70, 206, 235, 121, 172,
    28, 180,133, 237, 95,  91, 55,145,
    58,  140,245,133,126,255, 0, 169
];

/// SPL Token-2022 program raw bytes (public for pool resolution).
pub const SPL_TOKEN_2022_PROGRAM_BYTES: [u8; 32] = [
    6,221,246,225, 238,117,143,222,
    170, 44,170, 99, 234, 71, 245,  86,
    168, 167, 87, 215, 131, 140, 233,171,
    175, 191,  9, 87,  45, 17, 78,  52
];

/// Determine the owning token program for a mint.
///
/// Uses an explicit `token_mint_program` if provided (non-zero bytes from pool accounts).
/// Falls back to WSOL detection (classic SPL Token for WSOL).
/// For unresolved non-WSOL mints, defaults to classic SPL Token (safe default —
/// Token-2022 mints will fail and we'll detect at runtime).
fn token_program_for_mint_with_hint(mint: &Pubkey, hint: &[u8; 32]) -> Pubkey {
    // If we have a resolved program hint, use it
    if *hint != [0u8; 32] {
        return Pubkey::new_from_array(*hint);
    }
    // Fallback: WSOL → classic SPL Token, everything else → classic SPL Token (safe default)
    Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap()
}

/// Determine the owning token program for WSOL (always classic SPL Token).
fn wsol_token_program() -> Pubkey {
    Pubkey::from_str(SPL_TOKEN_PROGRAM_STR).unwrap()
}

/// Derive ATA address using the specified token program.
fn token_ata_with_program(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let (addr, _bump) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );
    addr
}

/// Derive ATA for WSOL (always classic SPL Token program).
fn wsol_ata(wallet: &Pubkey) -> Pubkey {
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let wsol_prog = wsol_token_program();
    token_ata_with_program(wallet, &wsol_mint, &wsol_prog)
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
    token_program: &Pubkey,
) -> Instruction {
    let ata = token_ata_with_program(wallet, mint, token_program);
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();

    Instruction {
        program_id: ata_program,
        accounts: vec![
            AccountMeta::new(*payer, true),           // 0. funding account (signer)
            AccountMeta::new(ata, false),              // 1. associated token account
            AccountMeta::new_readonly(*wallet, false), // 2. wallet address
            AccountMeta::new_readonly(*mint, false),   // 3. token mint
            AccountMeta::new_readonly(system_program::id(), false), // 4. system_program
            AccountMeta::new_readonly(*token_program, false),       // 5. token_program
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
///
/// PumpSwap has TWO separate instructions with different discriminators:
/// - `buy`  (0x66063d1201daebea): args = (base_out: u64, max_quote_in: u64)
///   → buy base tokens by paying quote tokens
/// - `sell` (0x33e685a4017f83ad): args = (base_in: u64, min_quote_out: u64)
///   → sell base tokens for quote tokens
///
/// "base" and "quote" refer to the pool's on-chain ordering, NOT our token/SOL convention.
fn build_swap_data(discriminator: &[u8; 8], arg1: u64, arg2: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(discriminator);
    data.extend_from_slice(&arg1.to_le_bytes());
    data.extend_from_slice(&arg2.to_le_bytes());
    data
}

// ── Swap instruction builder ─────────────────────────────────────────────────

/// Build the PumpSwap swap instruction with the 22-account layout.
///
/// **Pool ordering aware.** Accounts [3]-[8] must match the pool's actual
/// on-chain base/quote ordering, not our normalized token/SOL convention.
///
/// For a NORMAL pool (token=base, WSOL=quote):
///   [3] = token mint    [4] = WSOL mint
///   [5] = user token ATA  [6] = user WSOL ATA
///   [7] = pool token vault [8] = pool WSOL vault
///
/// For a REVERSED pool (WSOL=base, token=quote):
///   [3] = WSOL mint     [4] = token mint
///   [5] = user WSOL ATA  [6] = user token ATA
///   [7] = pool WSOL vault [8] = pool token vault
///
/// Fixed accounts [0]-[2], [9]-[21] are the same regardless of ordering.
fn build_pumpswap_swap_ix(
    pool: &PumpSwapPoolAccounts,
    wallet_pubkey: &Pubkey,
    fee_recipient_idx: usize,
    discriminator: &[u8; 8],
    arg1: u64,
    arg2: u64,
) -> Instruction {
    let pumpswap_program = Pubkey::from_str(PUMPSWAP_PROGRAM).unwrap();
    let global_config = Pubkey::from_str(PUMPSWAP_GLOBAL_CONFIG).unwrap();
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM_STR).unwrap();
    let event_authority = Pubkey::from_str(PUMPSWAP_EVENT_AUTHORITY).unwrap();
    let fee_program = Pubkey::from_str(PUMPSWAP_FEE_PROGRAM).unwrap();

    let token_mint = Pubkey::new_from_array(pool.base_mint);

    // Token program for each side: resolved from pool accounts (runtime-detected)
    let token_mint_program = token_program_for_mint_with_hint(&token_mint, &pool.token_mint_program);
    let wsol_prog = wsol_token_program();

    // Derive user ATAs (uses correct token program for PDA derivation)
    let user_token_ata = token_ata_with_program(wallet_pubkey, &token_mint, &token_mint_program);
    let user_wsol_ata = wsol_ata(wallet_pubkey);

    // Accounts [3]-[8] depend on pool ordering:
    // Pool's on-chain base_mint/quote_mint determine the account positions.
    // Accounts [11]/[12] = base/quote token programs — must match their respective mints.
    let (acct3_base_mint, acct4_quote_mint, acct5_user_base_ata, acct6_user_quote_ata,
         base_token_program, quote_token_program) =
        if pool.token_is_base {
            // Normal: token=base, WSOL=quote
            (token_mint, wsol_mint, user_token_ata, user_wsol_ata,
             token_mint_program, wsol_prog)
        } else {
            // Reversed: WSOL=base, token=quote
            (wsol_mint, token_mint, user_wsol_ata, user_token_ata,
             wsol_prog, token_mint_program)
        };
    // Pool vaults [7]/[8] are already stored in the pool's on-chain order
    let acct7_pool_base_vault = Pubkey::new_from_array(pool.pool_base_token_account);
    let acct8_pool_quote_vault = Pubkey::new_from_array(pool.pool_quote_token_account);

    // Fee recipient rotation
    let fee_recipient = Pubkey::from_str(
        PUMPSWAP_FEE_RECIPIENTS[fee_recipient_idx % 8],
    )
    .unwrap();
    let fee_recipient_token_account = wsol_ata(&fee_recipient);

    // coin_creator_vault_ata / authority: Pubkey::default() when zeroed — program handles it
    let coin_creator_vault_ata = Pubkey::new_from_array(pool.coin_creator_vault_ata);
    let coin_creator_vault_authority = Pubkey::new_from_array(pool.coin_creator_vault_authority);
    // Fixed-address accounts — always the same for all PumpSwap pools
    let coin_fee_config = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE).unwrap();
    let coin_fee_program_state = Pubkey::from_str(PUMPSWAP_FEE_PROG_STATE2).unwrap();

    let accounts = vec![
        AccountMeta::new(Pubkey::new_from_array(pool.pool), false),     // [0]  pool
        AccountMeta::new(*wallet_pubkey, true),                          // [1]  user (signer)
        AccountMeta::new_readonly(global_config, false),                 // [2]  global_config
        AccountMeta::new_readonly(acct3_base_mint, false),               // [3]  base_mint (on-chain)
        AccountMeta::new_readonly(acct4_quote_mint, false),              // [4]  quote_mint (on-chain)
        AccountMeta::new(acct5_user_base_ata, false),                    // [5]  user_base_token_account
        AccountMeta::new(acct6_user_quote_ata, false),                   // [6]  user_quote_token_account
        AccountMeta::new(acct7_pool_base_vault, false),                  // [7]  pool_base_token_account
        AccountMeta::new(acct8_pool_quote_vault, false),                 // [8]  pool_quote_token_account
        AccountMeta::new(fee_recipient, false),                          // [9]  protocol_fee_recipient
        AccountMeta::new(fee_recipient_token_account, false),            // [10] fee_recipient_token_acct
        AccountMeta::new_readonly(base_token_program, false),              // [11] base_token_program
        AccountMeta::new_readonly(quote_token_program, false),           // [12] quote_token_program
        AccountMeta::new_readonly(system_program::id(), false),          // [13] system_program
        AccountMeta::new_readonly(ata_program, false),                   // [14] associated_token_program
        AccountMeta::new_readonly(event_authority, false),               // [15] event_authority
        AccountMeta::new_readonly(pumpswap_program, false),              // [16] pump_program (self CPI)
        AccountMeta::new(coin_creator_vault_ata, false),                 // [17] coin_creator_vault_ata
        AccountMeta::new(coin_creator_vault_authority, false),           // [18] coin_creator_vault_authority
        AccountMeta::new_readonly(coin_fee_config, false),               // [19] coin_fee_config
        AccountMeta::new_readonly(fee_program, false),                   // [20] coin_fee_program
        AccountMeta::new_readonly(coin_fee_program_state, false),        // [21] coin_fee_program_state
    ];

    Instruction {
        program_id: pumpswap_program,
        accounts,
        data: build_swap_data(discriminator, arg1, arg2),
    }
}

// ── Buy: SOL → Token ─────────────────────────────────────────────────────────

/// Build a complete signed PumpSwap BUY transaction (SOL → Token).
///
/// **Pool ordering aware.** The PumpSwap instruction discriminator and arg
/// semantics depend on whether the token is base or quote in the pool:
///
/// NORMAL pool (token=base, WSOL=quote):
///   Use `buy` discriminator: buy(base_out=tokens, max_quote_in=sol)
///   → "buy base tokens by paying quote tokens"
///
/// REVERSED pool (WSOL=base, token=quote):
///   Use `sell` discriminator: sell(base_in=sol, min_quote_out=tokens)
///   → "sell base(WSOL) to get quote(tokens)" — semantically a "buy tokens"
///
/// Instruction sequence:
///   1. ComputeBudget::set_compute_unit_limit(400_000)
///   2. ComputeBudget::set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, token_mint)
///   4. create_associated_token_account_idempotent(user, WSOL)
///   5. system_instruction::transfer(wallet → wsol_ata, sol_lamports)
///   6. spl_token::sync_native(wsol_ata)
///   7. pumpswap swap ix (22 accounts)
///   8. spl_token::close_account(wsol_ata → wallet)
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
    let token_mint = Pubkey::new_from_array(pool.base_mint);
    let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
    let blockhash = Hash::new_from_array(recent_blockhash);

    // Resolve token programs
    let token_prog = token_program_for_mint_with_hint(&token_mint, &pool.token_mint_program);
    let wsol_prog = wsol_token_program();

    // Derive ATAs
    let wsol_ata_addr = wsol_ata(&wallet_pubkey);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(400_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Create token ATA (idempotent)
    let ix_create_token_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &token_mint,
        &token_prog,
    );

    // 4. Create WSOL ATA (idempotent)
    let ix_create_wsol_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &wsol_mint,
        &wsol_prog,
    );

    // 5. Fund WSOL ATA with SOL
    let ix_fund_wsol = system_instruction::transfer(&wallet_pubkey, &wsol_ata_addr, sol_lamports);

    // 6. Sync native to wrap deposited SOL → WSOL
    let ix_sync = build_sync_native_ix(&wsol_ata_addr);

    // 7. PumpSwap swap — discriminator + args depend on pool ordering
    let (discriminator, arg1, arg2) = if pool.token_is_base {
        // NORMAL: token=base, WSOL=quote → PumpSwap "buy" (buy base with quote)
        // buy(base_out=min_tokens_out, max_quote_in=sol_lamports)
        (&PUMPSWAP_BUY_DISCRIMINATOR, min_tokens_out, sol_lamports)
    } else {
        // REVERSED: WSOL=base, token=quote → PumpSwap "sell" (sell base for quote)
        // sell(base_in=sol_lamports, min_quote_out=min_tokens_out)
        (&PUMPSWAP_SELL_DISCRIMINATOR, sol_lamports, min_tokens_out)
    };

    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        discriminator,
        arg1,
        arg2,
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
/// **Pool ordering aware.** The PumpSwap instruction discriminator and arg
/// semantics depend on whether the token is base or quote in the pool:
///
/// NORMAL pool (token=base, WSOL=quote):
///   Use `sell` discriminator: sell(base_in=tokens, min_quote_out=sol)
///   → "sell base(tokens) to get quote(WSOL)"
///
/// REVERSED pool (WSOL=base, token=quote):
///   Use `buy` discriminator: buy(base_out=sol, max_quote_in=tokens)
///   → "buy base(WSOL) by paying quote(tokens)" — semantically a "sell tokens"
///
/// Instruction sequence:
///   1. ComputeBudget::set_compute_unit_limit(300_000)
///   2. ComputeBudget::set_compute_unit_price(5000)
///   3. create_associated_token_account_idempotent(user, WSOL)
///   4. pumpswap swap ix (22 accounts)
///   5. spl_token::close_account(wsol_ata → wallet)
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
    let wsol_prog = wsol_token_program();

    // Derive WSOL ATA
    let wsol_ata_addr = wsol_ata(&wallet_pubkey);

    // 1. Compute budget: limit
    let ix_cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(300_000);

    // 2. Compute budget: priority fee
    let ix_cu_price = ComputeBudgetInstruction::set_compute_unit_price(5000);

    // 3. Create WSOL ATA (idempotent)
    let ix_create_wsol_ata = build_create_ata_idempotent_ix(
        &wallet_pubkey,
        &wallet_pubkey,
        &wsol_mint,
        &wsol_prog,
    );

    // 4. PumpSwap swap — discriminator + args depend on pool ordering
    let (discriminator, arg1, arg2) = if pool.token_is_base {
        // NORMAL: token=base, WSOL=quote → PumpSwap "sell" (sell base for quote)
        // sell(base_in=tokens_to_sell, min_quote_out=min_sol_out)
        (&PUMPSWAP_SELL_DISCRIMINATOR, tokens_to_sell, min_sol_out)
    } else {
        // REVERSED: WSOL=base, token=quote → PumpSwap "buy" (buy base with quote)
        // buy(base_out=min_sol_out, max_quote_in=tokens_to_sell)
        (&PUMPSWAP_BUY_DISCRIMINATOR, min_sol_out, tokens_to_sell)
    };

    let ix_swap = build_pumpswap_swap_ix(
        pool,
        &wallet_pubkey,
        fee_recipient_idx,
        discriminator,
        arg1,
        arg2,
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

    /// Create a dummy PumpSwapPoolAccounts for testing (NORMAL ordering: token=base).
    fn dummy_pool() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            base_mint: [42u8; 32],
            pool_base_token_account: [3u8; 32],
            pool_quote_token_account: [4u8; 32],
            coin_creator_vault_ata: [0u8; 32],       // zeroed — program handles it
            coin_creator_vault_authority: [0u8; 32],  // zeroed — program handles it
            token_is_base: true,
            token_mint_program: SPL_TOKEN_PROGRAM_BYTES, // classic SPL Token for tests
        }
    }

    /// Create a dummy PumpSwapPoolAccounts with REVERSED ordering (WSOL=base, token=quote).
    fn dummy_reversed_pool() -> PumpSwapPoolAccounts {
        PumpSwapPoolAccounts {
            pool: [1u8; 32],
            base_mint: [42u8; 32],       // token mint (always the non-WSOL mint)
            pool_base_token_account: [5u8; 32],  // WSOL vault (base=WSOL for reversed)
            pool_quote_token_account: [6u8; 32], // token vault (quote=token for reversed)
            coin_creator_vault_ata: [0u8; 32],
            coin_creator_vault_authority: [0u8; 32],
            token_is_base: false,
            token_mint_program: SPL_TOKEN_PROGRAM_BYTES, // classic SPL Token for tests
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

    // ── 1. test_buy_discriminator_correct ────────────────────────────────

    #[test]
    fn test_buy_discriminator_correct() {
        let data = build_swap_data(&PUMPSWAP_BUY_DISCRIMINATOR, 100, 200);
        assert_eq!(
            &data[..8],
            &[0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea],
            "discriminator must match PumpSwap buy discriminator"
        );
    }

    // ── 1b. test_sell_discriminator_correct ───────────────────────────────

    #[test]
    fn test_sell_discriminator_correct() {
        let data = build_swap_data(&PUMPSWAP_SELL_DISCRIMINATOR, 100, 200);
        assert_eq!(
            &data[..8],
            &[0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad],
            "discriminator must match PumpSwap sell discriminator"
        );
    }

    // ── 2. test_buy_data_length_24 ───────────────────────────────────────

    #[test]
    fn test_buy_data_length_24() {
        let data = build_swap_data(&PUMPSWAP_BUY_DISCRIMINATOR, 1, 1_000_000_000);
        assert_eq!(data.len(), 24, "buy swap data must be exactly 24 bytes");
    }

    // ── 3. test_sell_data_length_24 ──────────────────────────────────────

    #[test]
    fn test_sell_data_length_24() {
        let data = build_swap_data(&PUMPSWAP_SELL_DISCRIMINATOR, 1_000_000, 0);
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
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, &PUMPSWAP_BUY_DISCRIMINATOR, 1, 1_000_000);
        let expected = Pubkey::from_str("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV").unwrap();
        assert_eq!(
            ix.accounts[9].pubkey, expected,
            "fee_idx=0 should produce 62qc2CNX... as protocol_fee_recipient"
        );
    }

    // ── 10. test_fee_recipient_idx7 ─────────────────────────────────────

    #[test]
    fn test_fee_recipient_idx7() {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 7, &PUMPSWAP_SELL_DISCRIMINATOR, 1, 1_000_000);
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
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, &PUMPSWAP_BUY_DISCRIMINATOR, 1, 1_000_000);
        assert_eq!(ix.accounts.len(), 22, "PumpSwap swap ix must have 22 accounts");
    }

    // ── 13b. test_reversed_pool_swap_ix_has_22_accounts ──────────────────

    #[test]
    fn test_reversed_pool_swap_ix_has_22_accounts() {
        let pool = dummy_reversed_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(&pool, &kp.pubkey(), 0, &PUMPSWAP_SELL_DISCRIMINATOR, 1, 1_000_000);
        assert_eq!(ix.accounts.len(), 22, "reversed pool swap ix must have 22 accounts");
    }

    // ── 14. test_fee_recipient_wraps_around ──────────────────────────────

    #[test]
    fn test_fee_recipient_wraps_around() {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let pubkey = kp.pubkey();
        let ix0 = build_pumpswap_swap_ix(&pool, &pubkey, 0, &PUMPSWAP_BUY_DISCRIMINATOR, 1, 1);
        let ix8 = build_pumpswap_swap_ix(&pool, &pubkey, 8, &PUMPSWAP_BUY_DISCRIMINATOR, 1, 1);
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
        let data = build_swap_data(&PUMPSWAP_BUY_DISCRIMINATOR, arg1, arg2);
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

    // ── 16. test_buy_normal_pool_uses_buy_discriminator ──────────────────

    #[test]
    fn test_buy_normal_pool_uses_buy_discriminator() {
        let pool = dummy_pool(); // token_is_base = true
        let kp = Keypair::new();
        let tx_bytes = build_pumpswap_buy_tx(
            &pool, &kp, 1_000_000_000, 1, 10_000,
            Pubkey::new_unique(), dummy_blockhash(), 0,
        ).unwrap();
        let tx: VersionedTransaction = bincode::deserialize(&tx_bytes).unwrap();
        // swap ix is the 7th instruction (index 6)
        if let VersionedMessage::V0(m) = &tx.message {
            let swap_ix = &m.instructions[6];
            let ix_data = &swap_ix.data;
            assert_eq!(&ix_data[..8], &PUMPSWAP_BUY_DISCRIMINATOR,
                "normal pool BUY should use buy discriminator");
        } else { panic!("expected V0"); }
    }

    // ── 17. test_buy_reversed_pool_uses_sell_discriminator ───────────────

    #[test]
    fn test_buy_reversed_pool_uses_sell_discriminator() {
        let pool = dummy_reversed_pool(); // token_is_base = false
        let kp = Keypair::new();
        let tx_bytes = build_pumpswap_buy_tx(
            &pool, &kp, 1_000_000_000, 1, 10_000,
            Pubkey::new_unique(), dummy_blockhash(), 0,
        ).unwrap();
        let tx: VersionedTransaction = bincode::deserialize(&tx_bytes).unwrap();
        if let VersionedMessage::V0(m) = &tx.message {
            let swap_ix = &m.instructions[6];
            let ix_data = &swap_ix.data;
            assert_eq!(&ix_data[..8], &PUMPSWAP_SELL_DISCRIMINATOR,
                "reversed pool BUY should use sell discriminator (selling WSOL base for token quote)");
        } else { panic!("expected V0"); }
    }

    // ── 18. test_sell_normal_pool_uses_sell_discriminator ─────────────────

    #[test]
    fn test_sell_normal_pool_uses_sell_discriminator() {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let tx_bytes = build_pumpswap_sell_tx(
            &pool, &kp, 1_000_000, 500_000, 10_000,
            Pubkey::new_unique(), dummy_blockhash(), 0,
        ).unwrap();
        let tx: VersionedTransaction = bincode::deserialize(&tx_bytes).unwrap();
        if let VersionedMessage::V0(m) = &tx.message {
            let swap_ix = &m.instructions[3]; // sell tx: swap is instruction index 3
            let ix_data = &swap_ix.data;
            assert_eq!(&ix_data[..8], &PUMPSWAP_SELL_DISCRIMINATOR,
                "normal pool SELL should use sell discriminator");
        } else { panic!("expected V0"); }
    }

    // ── 19. test_sell_reversed_pool_uses_buy_discriminator ───────────────

    #[test]
    fn test_sell_reversed_pool_uses_buy_discriminator() {
        let pool = dummy_reversed_pool();
        let kp = Keypair::new();
        let tx_bytes = build_pumpswap_sell_tx(
            &pool, &kp, 1_000_000, 500_000, 10_000,
            Pubkey::new_unique(), dummy_blockhash(), 0,
        ).unwrap();
        let tx: VersionedTransaction = bincode::deserialize(&tx_bytes).unwrap();
        if let VersionedMessage::V0(m) = &tx.message {
            let swap_ix = &m.instructions[3];
            let ix_data = &swap_ix.data;
            assert_eq!(&ix_data[..8], &PUMPSWAP_BUY_DISCRIMINATOR,
                "reversed pool SELL should use buy discriminator (buying WSOL base with token quote)");
        } else { panic!("expected V0"); }
    }

    // ── 20. test_reversed_pool_base_mint_account_is_wsol ────────────────

    #[test]
    fn test_reversed_pool_base_mint_account_is_wsol() {
        let pool = dummy_reversed_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(
            &pool, &kp.pubkey(), 0, &PUMPSWAP_SELL_DISCRIMINATOR, 1, 1,
        );
        // Account [3] should be WSOL mint for reversed pool
        let wsol_mint = Pubkey::from_str(WSOL_MINT_STR).unwrap();
        assert_eq!(ix.accounts[3].pubkey, wsol_mint,
            "reversed pool: account[3] (base_mint) must be WSOL");
    }

    // ── 21. test_normal_pool_base_mint_account_is_token ─────────────────

    #[test]
    fn test_normal_pool_base_mint_account_is_token() {
        let pool = dummy_pool();
        let kp = Keypair::new();
        let ix = build_pumpswap_swap_ix(
            &pool, &kp.pubkey(), 0, &PUMPSWAP_BUY_DISCRIMINATOR, 1, 1,
        );
        // Account [3] should be token mint for normal pool
        let token_mint = Pubkey::new_from_array(pool.base_mint);
        assert_eq!(ix.accounts[3].pubkey, token_mint,
            "normal pool: account[3] (base_mint) must be token mint");
    }

    // ── 22. test_token_is_base_detection ─────────────────────────────────

    #[test]
    fn test_token_is_base_detection() {
        // A token mint starting with 0x01 < WSOL (0x06...) → token_is_base = true
        let low_mint = [0x01; 32];
        assert!(low_mint < WSOL_MINT_BYTES, "mint 0x01.. should be < WSOL");

        // A token mint starting with 0xFF > WSOL (0x06...) → token_is_base = false
        let high_mint = [0xFF; 32];
        assert!(high_mint > WSOL_MINT_BYTES, "mint 0xFF.. should be > WSOL");
    }
}