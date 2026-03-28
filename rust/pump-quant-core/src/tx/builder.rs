use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
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
    sysvar,
    transaction::VersionedTransaction,
};

// ── Pump.fun program constants ───────────────────────────────────────────────

/// pump.fun program ID
pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Anchor discriminator for `global:buy`
pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

/// Anchor discriminator for `global:sell`
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Jito tip accounts — rotate through these to distribute tips.
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

/// SPL Token program ID
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Associated Token Account program ID
const SPL_ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// pump.fun global PDA — derived from ["global"] seed under the pump program.
const PUMP_GLOBAL: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";

/// pump.fun fee recipient
const PUMP_FEE_RECIPIENT: &str = "CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ55NWRMLoAS";

/// pump.fun event authority PDA
const PUMP_EVENT_AUTHORITY: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";

// ── ATA derivation (avoids spl-associated-token-account dep) ─────────────────

/// Derive the associated token address for `wallet` + `mint`.
/// Equivalent to `spl_associated_token_account::get_associated_token_address`.
///
/// PDA seeds: [wallet, token_program, mint] under the ATA program.
fn get_associated_token_address(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM).unwrap();
    let ata_program = Pubkey::from_str(SPL_ATA_PROGRAM).unwrap();
    let (addr, _bump) = Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );
    addr
}

// ── Request structs ──────────────────────────────────────────────────────────

pub struct BuyTxRequest {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub vsol_lamports: u64,
    pub vtokens: u64,
    pub size_lamports: u64,
    pub slippage_bps: u32,
    pub priority_fee_microlamports: u64,
    pub jito_tip_lamports: u64,
    pub recent_blockhash: [u8; 32],
}

pub struct SellTxRequest {
    pub mint: [u8; 32],
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
    pub tokens_to_sell: u64,
    pub vsol_lamports: u64,
    pub vtokens: u64,
    pub min_sol_out_lamports: u64,
    pub priority_fee_microlamports: u64,
    pub jito_tip_lamports: u64,
    pub recent_blockhash: [u8; 32],
}

// ── TxBuilder ────────────────────────────────────────────────────────────────

pub struct TxBuilder {
    wallet: Keypair,
    pump_program: Pubkey,
    global: Pubkey,
    fee_recipient: Pubkey,
    event_authority: Pubkey,
    token_program: Pubkey,
    tip_account_idx: AtomicUsize,
}

impl TxBuilder {
    pub fn new(wallet: Keypair) -> Self {
        Self {
            wallet,
            pump_program: Pubkey::from_str(PUMP_FUN_PROGRAM).unwrap(),
            global: Pubkey::from_str(PUMP_GLOBAL).unwrap(),
            fee_recipient: Pubkey::from_str(PUMP_FEE_RECIPIENT).unwrap(),
            event_authority: Pubkey::from_str(PUMP_EVENT_AUTHORITY).unwrap(),
            token_program: Pubkey::from_str(SPL_TOKEN_PROGRAM).unwrap(),
            tip_account_idx: AtomicUsize::new(0),
        }
    }

    /// Build a buy VersionedTransaction (V0 message).
    ///
    /// Instructions:
    /// 1. SetComputeUnitLimit(200_000)
    /// 2. SetComputeUnitPrice(priority_fee)
    /// 3. pump.fun buy instruction
    /// 4. Jito tip transfer
    pub fn build_buy_tx(&self, req: &BuyTxRequest) -> Result<VersionedTransaction> {
        let mint = Pubkey::new_from_array(req.mint);
        let bonding_curve = Pubkey::new_from_array(req.bonding_curve);
        let assoc_bonding_curve = Pubkey::new_from_array(req.assoc_bonding_curve);
        let blockhash = Hash::new_from_array(req.recent_blockhash);
        let wallet_pubkey = self.wallet.pubkey();

        // Constant-product AMM: tokens_out = (size_lamports * vtokens) / (vsol + size_lamports)
        let tokens_out = Self::calc_tokens_out(req.size_lamports, req.vsol_lamports, req.vtokens);

        // max_sol_cost = size_lamports * (1 + slippage_bps / 10_000)
        let max_sol_cost = req
            .size_lamports
            .checked_mul(10_000u64 + req.slippage_bps as u64)
            .context("slippage overflow")?
            / 10_000u64;

        // Instruction data: [discriminator][tokens_out: u64 LE][max_sol_cost: u64 LE]
        let mut buy_data = Vec::with_capacity(24);
        buy_data.extend_from_slice(&BUY_DISCRIMINATOR);
        buy_data.extend_from_slice(&tokens_out.to_le_bytes());
        buy_data.extend_from_slice(&max_sol_cost.to_le_bytes());

        // Our ATA for this mint
        let associated_user = get_associated_token_address(&wallet_pubkey, &mint);

        // Account metas per pump.fun IDL buy instruction
        let accounts = vec![
            AccountMeta::new_readonly(self.global, false),         // 0. global
            AccountMeta::new(self.fee_recipient, false),           // 1. fee_recipient
            AccountMeta::new_readonly(mint, false),                // 2. mint
            AccountMeta::new(bonding_curve, false),                // 3. bonding_curve
            AccountMeta::new(assoc_bonding_curve, false),          // 4. associated_bonding_curve
            AccountMeta::new(associated_user, false),              // 5. associated_user (our ATA)
            AccountMeta::new(wallet_pubkey, true),                 // 6. user (signer)
            AccountMeta::new_readonly(system_program::id(), false),// 7. system_program
            AccountMeta::new_readonly(self.token_program, false),  // 8. token_program
            AccountMeta::new_readonly(sysvar::rent::id(), false),  // 9. rent
            AccountMeta::new_readonly(self.event_authority, false),// 10. event_authority
            AccountMeta::new_readonly(self.pump_program, false),   // 11. program
        ];

        let buy_ix = Instruction {
            program_id: self.pump_program,
            accounts,
            data: buy_data,
        };

        let ixs = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(200_000),
            ComputeBudgetInstruction::set_compute_unit_price(req.priority_fee_microlamports),
            buy_ix,
            system_instruction::transfer(
                &wallet_pubkey,
                &self.next_tip_account(),
                req.jito_tip_lamports,
            ),
        ];

        let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
            .context("failed to compile V0 message for buy tx")?;

        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[&self.wallet])
            .context("failed to sign buy tx")?;

        Ok(tx)
    }

    /// Build a sell VersionedTransaction (V0 message).
    ///
    /// Instructions:
    /// 1. SetComputeUnitLimit(200_000)
    /// 2. SetComputeUnitPrice(priority_fee)
    /// 3. pump.fun sell instruction
    /// 4. Jito tip transfer
    pub fn build_sell_tx(&self, req: &SellTxRequest) -> Result<VersionedTransaction> {
        let mint = Pubkey::new_from_array(req.mint);
        let bonding_curve = Pubkey::new_from_array(req.bonding_curve);
        let assoc_bonding_curve = Pubkey::new_from_array(req.assoc_bonding_curve);
        let blockhash = Hash::new_from_array(req.recent_blockhash);
        let wallet_pubkey = self.wallet.pubkey();

        // Instruction data: [discriminator][tokens_to_sell: u64 LE][min_sol_out: u64 LE]
        let mut sell_data = Vec::with_capacity(24);
        sell_data.extend_from_slice(&SELL_DISCRIMINATOR);
        sell_data.extend_from_slice(&req.tokens_to_sell.to_le_bytes());
        sell_data.extend_from_slice(&req.min_sol_out_lamports.to_le_bytes());

        // Our ATA for this mint
        let associated_user = get_associated_token_address(&wallet_pubkey, &mint);

        // Account metas for sell (same layout as buy)
        let accounts = vec![
            AccountMeta::new_readonly(self.global, false),         // 0. global
            AccountMeta::new(self.fee_recipient, false),           // 1. fee_recipient
            AccountMeta::new_readonly(mint, false),                // 2. mint
            AccountMeta::new(bonding_curve, false),                // 3. bonding_curve
            AccountMeta::new(assoc_bonding_curve, false),          // 4. associated_bonding_curve
            AccountMeta::new(associated_user, false),              // 5. associated_user (our ATA)
            AccountMeta::new(wallet_pubkey, true),                 // 6. user (signer)
            AccountMeta::new_readonly(system_program::id(), false),// 7. system_program
            AccountMeta::new_readonly(self.token_program, false),  // 8. token_program
            AccountMeta::new_readonly(sysvar::rent::id(), false),  // 9. rent
            AccountMeta::new_readonly(self.event_authority, false),// 10. event_authority
            AccountMeta::new_readonly(self.pump_program, false),   // 11. program
        ];

        let sell_ix = Instruction {
            program_id: self.pump_program,
            accounts,
            data: sell_data,
        };

        let ixs = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(200_000),
            ComputeBudgetInstruction::set_compute_unit_price(req.priority_fee_microlamports),
            sell_ix,
            system_instruction::transfer(
                &wallet_pubkey,
                &self.next_tip_account(),
                req.jito_tip_lamports,
            ),
        ];

        let msg = v0::Message::try_compile(&wallet_pubkey, &ixs, &[], blockhash)
            .context("failed to compile V0 message for sell tx")?;

        let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[&self.wallet])
            .context("failed to sign sell tx")?;

        Ok(tx)
    }

    /// Serialize a signed VersionedTransaction to base64 for RPC / Jito submission.
    pub fn serialize_tx(tx: &VersionedTransaction) -> Result<String> {
        let bytes =
            bincode::serialize(tx).context("failed to bincode-serialize VersionedTransaction")?;
        Ok(BASE64.encode(bytes))
    }

    /// Rotate through Jito tip accounts (round-robin, atomic).
    fn next_tip_account(&self) -> Pubkey {
        let idx = self.tip_account_idx.fetch_add(1, Ordering::Relaxed) % JITO_TIP_ACCOUNTS.len();
        Pubkey::from_str(JITO_TIP_ACCOUNTS[idx]).unwrap()
    }

    /// Constant-product AMM: tokens_out = (sol_in * vtokens) / (vsol + sol_in)
    /// Matches pump.fun's bonding curve formula. Uses u128 to avoid overflow.
    fn calc_tokens_out(sol_in: u64, vsol: u64, vtokens: u64) -> u64 {
        let numerator = (sol_in as u128) * (vtokens as u128);
        let denominator = (vsol as u128) + (sol_in as u128);
        (numerator / denominator) as u64
    }
}
