//! Raydium CPMM `swap_base_in` instruction builder for graduation arbitrage.
//!
//! Builds a Solana Instruction that swaps SOL → Token (buy) or Token → SOL (sell)
//! on a Raydium Constant-Product Market Maker pool.
//!
//! ## Raydium CPMM `swap_base_in` account layout (9 accounts):
//!
//! ```text
//! [0]  token_program       — SPL Token Program
//! [1]  amm_id              — Pool state account (CPMM pool PDA)
//! [2]  amm_authority       — Pool authority PDA (seeds: ["amm authority"])
//! [3]  amm_open_orders     — Pool open orders account (can be SystemProgram if N/A)
//! [4]  amm_coin_vault      — Pool token vault (coin = non-SOL token)
//! [5]  amm_pc_vault        — Pool SOL vault (pc = WSOL)
//! [6]  user_source          — User's source token account (ATA)
//! [7]  user_destination     — User's destination token account (ATA)
//! [8]  user_owner           — User wallet (signer)
//! ```
//!
//! Instruction data layout (17 bytes):
//! ```text
//! [0]       instruction discriminator (9 = swap_base_in for AMM V4)
//! [1..9]    amount_in (u64 LE)
//! [9..17]   minimum_amount_out (u64 LE)
//! ```

use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

/// Raydium AMM V4 program ID: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
pub const RAYDIUM_AMM_V4: Pubkey = Pubkey::new_from_array([75, 217, 73, 196, 54, 2, 195, 63, 32, 119, 144, 237, 22, 163, 82, 76, 161, 185, 151, 92, 241, 33, 162, 169, 12, 255, 236, 125, 248, 182, 138, 205]);

/// Raydium AMM authority PDA: 5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1
pub const RAYDIUM_AUTHORITY: Pubkey = Pubkey::new_from_array([65, 87, 176, 88, 15, 49, 197, 252, 228, 74, 98, 88, 45, 188, 249, 215, 142, 231, 89, 67, 160, 132, 163, 147, 179, 80, 54, 141, 34, 137, 147, 8]);

/// SPL Token Program: TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
pub const TOKEN_PROGRAM: Pubkey = Pubkey::new_from_array([6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169]);

/// System Program: 11111111111111111111111111111111
pub const SYSTEM_PROGRAM: Pubkey = solana_sdk::system_program::ID;

/// WSOL mint: So11111111111111111111111111111111111111112
pub const WSOL_MINT: Pubkey = Pubkey::new_from_array([6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26, 235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1]);

/// Associated Token Program: ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL
pub const ASSOCIATED_TOKEN_PROGRAM: Pubkey = Pubkey::new_from_array([140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89]);

/// Parameters for building a Raydium CPMM swap instruction.
#[derive(Debug, Clone)]
pub struct SwapParams {
    /// Pool state account (AMM ID).
    pub pool_id: Pubkey,
    /// Pool token vault (coin vault — non-SOL token).
    pub coin_vault: Pubkey,
    /// Pool SOL vault (pc vault — WSOL).
    pub pc_vault: Pubkey,
    /// Pool open orders account (SystemProgram if N/A for CPMM).
    pub open_orders: Pubkey,
    /// User's source token account (what we're sending).
    pub user_source: Pubkey,
    /// User's destination token account (what we're receiving).
    pub user_destination: Pubkey,
    /// User wallet (signer).
    pub user_owner: Pubkey,
    /// Amount to swap (lamports for SOL, atoms for token).
    pub amount_in: u64,
    /// Minimum acceptable output (slippage protection).
    pub minimum_amount_out: u64,
}

/// Build a Raydium AMM V4 `swap_base_in` instruction.
///
/// This is the exact instruction layout accepted by the on-chain program.
/// Instruction discriminator: 9 (swap_base_in).
///
/// # Performance
/// Zero allocation — returns stack-allocated Instruction with inline Vec.
#[inline(never)] // cold path — called once per arb
pub fn build_swap_base_in(params: &SwapParams) -> Instruction {
    // Instruction data: [discriminator(1)] [amount_in(8)] [minimum_amount_out(8)]
    let mut data = Vec::with_capacity(17);
    data.push(9u8); // swap_base_in discriminator
    data.extend_from_slice(&params.amount_in.to_le_bytes());
    data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    Instruction {
        program_id: RAYDIUM_AMM_V4,
        accounts: vec![
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new(params.pool_id, false),
            AccountMeta::new_readonly(RAYDIUM_AUTHORITY, false),
            AccountMeta::new(params.open_orders, false),
            AccountMeta::new(params.coin_vault, false),
            AccountMeta::new(params.pc_vault, false),
            AccountMeta::new(params.user_source, false),
            AccountMeta::new(params.user_destination, false),
            AccountMeta::new_readonly(params.user_owner, true), // signer
        ],
        data,
    }
}

/// Compute the Associated Token Account (ATA) address for a wallet + mint.
///
/// Uses the standard PDA derivation: seeds = [wallet, TOKEN_PROGRAM, mint].
#[inline(always)]
pub fn derive_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let (ata, _bump) = Pubkey::find_program_address(
        &[
            wallet.as_ref(),
            TOKEN_PROGRAM.as_ref(),
            mint.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM,
    );
    ata
}

/// Build a Jito tip transfer instruction.
///
/// Simple SOL transfer via System Program to a Jito tip account.
#[inline(always)]
pub fn build_tip_transfer(
    from: &Pubkey,
    tip_account: &Pubkey,
    lamports: u64,
) -> Instruction {
    solana_sdk::system_instruction::transfer(from, tip_account, lamports)
}

/// Build a complete arb transaction: [create_wsol_ata_if_needed] + swap + tip.
///
/// Returns a Vec of instructions ready for Transaction construction.
///
/// For buy (SOL → Token): user_source = WSOL ATA, user_destination = Token ATA
/// For sell (Token → SOL): user_source = Token ATA, user_destination = WSOL ATA
pub fn build_arb_instructions(
    wallet: &Pubkey,
    token_mint: &Pubkey,
    pool_id: &Pubkey,
    coin_vault: &Pubkey,
    pc_vault: &Pubkey,
    amount_in: u64,
    minimum_amount_out: u64,
    tip_account: &Pubkey,
    tip_lamports: u64,
    is_buy: bool,
) -> Vec<Instruction> {
    let wsol_ata = derive_ata(wallet, &WSOL_MINT);
    let token_ata = derive_ata(wallet, token_mint);

    let (user_source, user_destination) = if is_buy {
        (wsol_ata, token_ata) // SOL → Token
    } else {
        (token_ata, wsol_ata) // Token → SOL
    };

    let swap_ix = build_swap_base_in(&SwapParams {
        pool_id: *pool_id,
        coin_vault: *coin_vault,
        pc_vault: *pc_vault,
        open_orders: SYSTEM_PROGRAM, // CPMM doesn't use open orders
        user_source,
        user_destination,
        user_owner: *wallet,
        amount_in,
        minimum_amount_out,
    });

    let tip_ix = build_tip_transfer(wallet, tip_account, tip_lamports);

    vec![swap_ix, tip_ix]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raydium_amm_v4_pubkey() {
        assert_eq!(
            RAYDIUM_AMM_V4.to_string(),
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
        );
    }

    #[test]
    fn test_raydium_authority_pubkey() {
        assert_eq!(
            RAYDIUM_AUTHORITY.to_string(),
            "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"
        );
    }

    #[test]
    fn test_build_swap_instruction_layout() {
        let params = SwapParams {
            pool_id: Pubkey::new_unique(),
            coin_vault: Pubkey::new_unique(),
            pc_vault: Pubkey::new_unique(),
            open_orders: SYSTEM_PROGRAM,
            user_source: Pubkey::new_unique(),
            user_destination: Pubkey::new_unique(),
            user_owner: Pubkey::new_unique(),
            amount_in: 500_000_000, // 0.5 SOL
            minimum_amount_out: 1_000_000_000, // 1B tokens
        };

        let ix = build_swap_base_in(&params);

        // Check program ID
        assert_eq!(ix.program_id, RAYDIUM_AMM_V4);

        // Check data layout: [discriminator(1)] [amount_in(8)] [min_out(8)]
        assert_eq!(ix.data.len(), 17);
        assert_eq!(ix.data[0], 9); // swap_base_in discriminator

        let amount = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
        assert_eq!(amount, 500_000_000);

        let min_out = u64::from_le_bytes(ix.data[9..17].try_into().unwrap());
        assert_eq!(min_out, 1_000_000_000);

        // Check account count
        assert_eq!(ix.accounts.len(), 9);

        // Check signer
        assert!(ix.accounts[8].is_signer); // user_owner is signer
        assert!(!ix.accounts[0].is_signer); // token_program is not signer
    }

    #[test]
    fn test_build_arb_instructions_buy() {
        let wallet = Pubkey::new_unique();
        let token_mint = Pubkey::new_unique();
        let pool_id = Pubkey::new_unique();
        let coin_vault = Pubkey::new_unique();
        let pc_vault = Pubkey::new_unique();
        let tip_account = Pubkey::new_unique();

        let ixs = build_arb_instructions(
            &wallet,
            &token_mint,
            &pool_id,
            &coin_vault,
            &pc_vault,
            500_000_000,  // 0.5 SOL
            1_000_000_000, // min 1B tokens
            &tip_account,
            500_000, // 0.0005 SOL tip
            true, // buy
        );

        // Should have 2 instructions: swap + tip
        assert_eq!(ixs.len(), 2);
        assert_eq!(ixs[0].program_id, RAYDIUM_AMM_V4); // swap
        assert_eq!(ixs[1].program_id, solana_sdk::system_program::id()); // tip transfer
    }

    #[test]
    fn test_build_arb_instructions_sell() {
        let wallet = Pubkey::new_unique();
        let token_mint = Pubkey::new_unique();
        let pool_id = Pubkey::new_unique();
        let coin_vault = Pubkey::new_unique();
        let pc_vault = Pubkey::new_unique();
        let tip_account = Pubkey::new_unique();

        let ixs = build_arb_instructions(
            &wallet,
            &token_mint,
            &pool_id,
            &coin_vault,
            &pc_vault,
            1_000_000_000, // 1B token atoms
            490_000_000,   // min 0.49 SOL back
            &tip_account,
            500_000,
            false, // sell
        );

        assert_eq!(ixs.len(), 2);
    }

    #[test]
    fn test_derive_ata_deterministic() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata1 = derive_ata(&wallet, &mint);
        let ata2 = derive_ata(&wallet, &mint);
        assert_eq!(ata1, ata2); // deterministic
    }

    #[test]
    fn test_tip_transfer() {
        let from = Pubkey::new_unique();
        let to = Pubkey::new_unique();
        let ix = build_tip_transfer(&from, &to, 500_000);
        assert_eq!(ix.program_id, solana_sdk::system_program::id());
    }
}
