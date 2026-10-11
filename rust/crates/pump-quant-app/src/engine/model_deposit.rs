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

    /// Record the mint's owning token program (decoded from the mint account owner / a builder ctx / a
    /// derivation-proven stream observation). Bounded by `ATA_OPEN_CAP`: when full, the OLDEST-observed mint whose
    /// paper ATA is not open is evicted (a stream sees far more mints than it trades; never evicting would turn
    /// every later mint into `deposit_unknown`). Only if every entry is an open ATA is the insert refused (counted).
    pub fn model_observe_token_program(&mut self, mint: [u8; 32], program: [u8; 32]) {
        if let Some(p) = self.model_token_program.get_mut(&mint) {
            *p = program;
            return;
        }
        if self.model_token_program.len() >= ATA_OPEN_CAP {
            let victim = self
                .model_token_program_order
                .iter()
                .position(|m| !self.ata_open.contains(m));
            match victim.and_then(|i| self.model_token_program_order.remove(i)) {
                Some(v) => {
                    self.model_token_program.remove(&v);
                }
                None => {
                    self.mrep("deposit:token_program_cap_unknown");
                    return;
                }
            }
        }
        self.model_token_program.insert(mint, program);
        self.model_token_program_order.push_back(mint);
    }

    /// Whether `mint`'s token program is already known (lets the daemon skip re-deriving it).
    #[must_use]
    pub fn model_token_program_known(&self, mint: &[u8; 32]) -> bool {
        self.model_token_program.contains_key(mint)
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

#[cfg(test)]
mod tests {
    use super::super::{Engine, ATA_OPEN_CAP};
    use crate::config::Config;
    use crate::engine::RunMode;

    fn key(i: usize) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[..8].copy_from_slice(&(i as u64).to_le_bytes());
        k
    }

    #[test]
    fn a_full_token_program_map_evicts_the_oldest_non_open_mint_and_never_an_open_ata() {
        let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
        let tp = [7u8; 32];
        e.ata_open.insert(key(0)); // the oldest entry is a held (open) ATA: it must survive
        for i in 0..ATA_OPEN_CAP {
            e.model_observe_token_program(key(i), tp);
        }
        assert_eq!(e.model_token_program.len(), ATA_OPEN_CAP);
        e.model_observe_token_program(key(ATA_OPEN_CAP), tp);
        assert_eq!(e.model_token_program.len(), ATA_OPEN_CAP, "bounded");
        assert!(
            e.model_token_program_known(&key(0)),
            "open ATA never evicted"
        );
        assert!(
            !e.model_token_program_known(&key(1)),
            "oldest non-open evicted"
        );
        assert!(
            e.model_token_program_known(&key(ATA_OPEN_CAP)),
            "newest admitted"
        );
        // Re-observing a known mint is an update, not growth.
        e.model_observe_token_program(key(5), [8u8; 32]);
        assert_eq!(e.model_token_program.get(&key(5)), Some(&[8u8; 32]));
        assert_eq!(e.model_token_program.len(), ATA_OPEN_CAP);
        assert_eq!(e.model_token_program_order.len(), ATA_OPEN_CAP);
    }

    #[test]
    fn when_every_entry_is_an_open_ata_the_insert_is_refused_not_evicted() {
        let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
        for i in 0..ATA_OPEN_CAP {
            e.ata_open.insert(key(i));
            e.model_observe_token_program(key(i), [7u8; 32]);
        }
        e.model_observe_token_program(key(ATA_OPEN_CAP), [7u8; 32]);
        assert!(!e.model_token_program_known(&key(ATA_OPEN_CAP)));
        assert!((0..ATA_OPEN_CAP).all(|i| e.model_token_program_known(&key(i))));
    }
}
