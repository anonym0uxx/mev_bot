//! FEES R10 F8: the engine side of the paper BUY deposit (see [`crate::paper_deposit`] for the lead decisions).
//! The paper wallet's account state per mint (`ata_open` + `paper_ata_deposit`), the mint's token program where an
//! event carried it, and the versioned rent basis feed `plan_buy_deposits`; the close refund is the deposit
//! actually made.
#![forbid(unsafe_code)]

use super::{Engine, ATA_OPEN_CAP};
use crate::paper_deposit::{PaperRentBasis, RENT_V0_LEGACY};
use pump_quant_protocol::tx_build::DepositRefusal;

impl Engine {
    /// The paper rent basis this engine's config selects.
    #[must_use]
    pub fn paper_rent_basis(&self) -> PaperRentBasis {
        PaperRentBasis::by_version(self.cfg.paper_rent_basis)
    }

    /// Record the mint's owning token program (decoded from the mint account owner / a builder ctx). Bounded.
    pub fn model_observe_token_program(&mut self, mint: [u8; 32], program: [u8; 32]) {
        if self.model_token_program.contains_key(&mint)
            || self.model_token_program.len() < ATA_OPEN_CAP
        {
            self.model_token_program.insert(mint, program);
        } else {
            self.mrep("deposit:token_program_cap_unknown");
        }
    }

    /// What the paper wallet deposited into `mint`'s ATA: `None` = not open. An open ATA with no recorded deposit
    /// was opened under the v0 legacy constant (or by a pre-F8 ledger) and holds that constant.
    pub(super) fn paper_ata_deposited(&self, mint: &[u8; 32]) -> Option<u64> {
        if !self.ata_open.contains(mint) {
            return None;
        }
        Some(
            self.paper_ata_deposit
                .get(mint)
                .copied()
                .unwrap_or(RENT_V0_LEGACY.spl_165),
        )
    }

    /// The paper BUY deposit for `mint` now (0 if the paper ATA is open), or the named refusal.
    pub(super) fn paper_entry_deposit(&self, mint: &[u8; 32]) -> Result<u64, DepositRefusal> {
        crate::paper_deposit::paper_entry_deposit(
            &self.paper_rent_basis(),
            *mint,
            self.model_token_program.get(mint).copied(),
            self.paper_ata_deposited(mint),
        )
    }

    /// Record that the paper entry deposited `lamports` into `mint`'s (now open) ATA.
    pub(super) fn paper_ata_record_deposit(&mut self, mint: [u8; 32], lamports: u64) {
        if lamports > 0 && self.ata_open.contains(&mint) {
            self.paper_ata_deposit.insert(mint, lamports);
        }
    }

    /// The refund closing `mint`'s paper ATA returns: its actual deposited balance (0 when not open).
    pub(super) fn paper_ata_close_refund(&self, mint: &[u8; 32]) -> u64 {
        self.paper_ata_deposited(mint)
            .map_or(0, crate::paper_deposit::paper_close_refund)
    }

    /// Read-only (tests, status): the paper ATA deposit for `mint` (`None` = not open).
    #[must_use]
    pub fn model_paper_ata_deposit(&self, mint: &[u8; 32]) -> Option<u64> {
        self.paper_ata_deposited(mint)
    }
}
