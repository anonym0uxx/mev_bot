//! FEES R10 F8: the paper BUY's account deposit, from the SAME plan the live builder uses
//! ([`pump_quant_protocol::tx_build::plan_buy_deposits`]), never from a universal constant.
//!
//! Lead decisions (FEES_F78_PACKET, recorded here and in the handoff):
//! * **Account state** is the PAPER wallet's: the user ATA is `Absent` unless our paper wallet already opened it
//!   for the mint, else `Present { owner = token program, size = ata_account_size, mint, lamports = what paper
//!   actually deposited }`. The UVA is modelled `Present` and valid: a declared PAPER ASSUMPTION (the paper
//!   wallet is not the live wallet; the live builder observes the real UVA and refuses an absent one).
//! * **Token program** is the mint's, where the event/ctx carries it. Unknown -> `deposit_unknown`, no default.
//! * **Rent** is a VERSIONED paper basis: config `paper_rent_basis = rent_v1_s455400973` holds the minimums read
//!   at slot 455,400,973 (proc/CHAIN_READONLY_EVIDENCE.md §2: 165 B -> 1,488,440; 170 B -> 1,513,840). Paper
//!   evaluates the plan with `obs.slot` = the snapshot slot: a declared paper assumption (rent at the pinned
//!   snapshot, NOT at the decision slot). The basis id is recorded on every settlement.
//! * An absent key is basis v0 (`rent_v0_legacy_constant`): the historical 2,039,280 constant for every entry,
//!   no token-program read. Historical replays only, labelled; it is NOT the chain's current rent.
//! * Refund on close = the paper account's ACTUAL deposited balance ([`refund_on_close`]), never today's minimum.
//! * A refundable deposit is committed cash, not an expense; it is released to cash only when the close is booked.
//! * The live builder keeps its strict rule (rent slot == observation slot) and `deposit_unknown`: unchanged.
#![forbid(unsafe_code)]

use pump_quant_protocol::tx_build::{
    ata_account_size, plan_buy_deposits, refund_on_close, AccountState, BuyAccountObservation,
    DepositPlan, DepositRefusal, RentSnapshot, UVA_ACCOUNT_SIZE,
};
use pump_quant_protocol::venue_accounts::{PumpCurveCtx, PUMP_PROGRAM_ID, WSOL_MINT};

/// A versioned paper rent basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperRentBasis {
    /// Basis id recorded on every settlement.
    pub id: &'static str,
    /// Slot the minimums were read at (0 for the v0 legacy constant: not a read).
    pub slot: u64,
    /// Rent-exempt minimum for a 165 B SPL token account.
    pub spl_165: u64,
    /// Rent-exempt minimum for a 170 B Token-2022 (+ImmutableOwner) account.
    pub t22_170: u64,
    /// Rent-exempt minimum for the 137 B pump UVA (only used to give the modelled UVA a balance).
    pub uva_137: u64,
}

/// v0: the historical constant for every entry (absent `paper_rent_basis` key). Historical replays only.
pub const RENT_V0_LEGACY: PaperRentBasis = PaperRentBasis {
    id: "rent_v0_legacy_constant",
    slot: 0,
    spl_165: crate::cost_model::ATA_RENT_LAMPORTS,
    t22_170: crate::cost_model::ATA_RENT_LAMPORTS,
    uva_137: 0,
};

/// v1: getMinimumBalanceForRentExemption at slot 455,400,973 (proc/CHAIN_READONLY_EVIDENCE.md §2).
pub const RENT_V1_S455400973: PaperRentBasis = PaperRentBasis {
    id: "rent_v1_s455400973",
    slot: 455_400_973,
    spl_165: 1_488_440,
    t22_170: 1_513_840,
    uva_137: 1_346_200,
};

impl PaperRentBasis {
    /// The basis a config's `paper_rent_basis` selects (0 = v0 legacy, 1 = v1).
    #[must_use]
    pub const fn by_version(v: u32) -> Self {
        match v {
            1 => RENT_V1_S455400973,
            _ => RENT_V0_LEGACY,
        }
    }

    /// The config value naming a version (`paper_rent_basis = <id>`).
    #[must_use]
    pub fn version_of_id(id: &str) -> Option<u32> {
        match id {
            "rent_v0_legacy_constant" => Some(0),
            "rent_v1_s455400973" => Some(1),
            _ => None,
        }
    }

    /// Whether this is the v0 legacy constant (no plan, no token-program read).
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.slot == 0
    }

    /// The snapshot handed to [`plan_buy_deposits`].
    #[must_use]
    pub fn snapshot(&self) -> RentSnapshot {
        RentSnapshot {
            slot: self.slot,
            minimums: vec![
                (165, self.spl_165),
                (170, self.t22_170),
                (137, self.uva_137),
            ],
        }
    }
}

/// The paper wallet's ATA state for `mint`: `Absent` unless paper already deposited `deposited` into it.
#[must_use]
pub fn paper_ata_state(
    mint: [u8; 32],
    token_program: [u8; 32],
    deposited: Option<u64>,
) -> AccountState {
    match deposited {
        None => AccountState::Absent,
        Some(lamports) => AccountState::Present {
            owner: token_program,
            data_len: ata_account_size(&token_program).unwrap_or(0),
            lamports,
            token_mint: Some(mint),
        },
    }
}

/// PAPER ASSUMPTION: the paper wallet's UVA exists and is valid.
#[must_use]
pub fn paper_uva_state(rent: &RentSnapshot) -> AccountState {
    AccountState::Present {
        owner: PUMP_PROGRAM_ID,
        data_len: UVA_ACCOUNT_SIZE,
        lamports: rent.minimum(UVA_ACCOUNT_SIZE).unwrap_or(0),
        token_mint: None,
    }
}

/// The paper BUY deposit plan: `plan_buy_deposits` on the paper account state, with the observation slot set to
/// the rent snapshot's slot (declared paper assumption). Identical inputs give the builder's plan exactly.
pub fn paper_buy_deposit(
    mint: [u8; 32],
    token_program: [u8; 32],
    user_ata: AccountState,
    rent: &RentSnapshot,
) -> Result<DepositPlan, DepositRefusal> {
    // Only `mint` and `token_program` are read by the plan; the rest is a placeholder identity.
    let ctx = PumpCurveCtx {
        mint,
        user: [1; 32],
        fee_recipient: [1; 32],
        creator: [1; 32],
        token_program,
        is_cashback_coin: false,
        quote_mint: WSOL_MINT,
    };
    let obs = BuyAccountObservation {
        slot: rent.slot,
        user_ata,
        user_volume_accumulator: paper_uva_state(rent),
        rent: rent.clone(),
    };
    plan_buy_deposits(&ctx, &obs)
}

/// The engine's paper entry deposit under `basis`: v0 = the legacy constant iff the ATA is not yet open; v1 = the
/// plan. `token_program` `None` under v1 = `deposit_unknown`.
pub fn paper_entry_deposit(
    basis: &PaperRentBasis,
    mint: [u8; 32],
    token_program: Option<[u8; 32]>,
    deposited: Option<u64>,
) -> Result<u64, DepositRefusal> {
    if basis.is_legacy() {
        return Ok(if deposited.is_some() {
            0
        } else {
            basis.spl_165
        });
    }
    let tp = token_program.ok_or(DepositRefusal::DepositUnknown)?;
    let plan = paper_buy_deposit(
        mint,
        tp,
        paper_ata_state(mint, tp, deposited),
        &basis.snapshot(),
    )?;
    plan.total().ok_or(DepositRefusal::DepositUnknown)
}

/// What closing the paper ATA returns: its actual deposited balance.
#[must_use]
pub fn paper_close_refund(deposited: u64) -> u64 {
    refund_on_close(&AccountState::Present {
        owner: [0; 32],
        data_len: 0,
        lamports: deposited,
        token_mint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_protocol::venue_accounts::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};

    #[test]
    fn v1_absent_is_rent_at_the_snapshot_open_is_zero_unknown_refuses_and_v0_is_the_constant() {
        let b = RENT_V1_S455400973;
        let m = [7; 32];
        assert_eq!(
            paper_entry_deposit(&b, m, Some(TOKEN_PROGRAM_ID), None),
            Ok(1_488_440)
        );
        assert_eq!(
            paper_entry_deposit(&b, m, Some(TOKEN_2022_PROGRAM_ID), None),
            Ok(1_513_840)
        );
        assert_eq!(
            paper_entry_deposit(&b, m, Some(TOKEN_PROGRAM_ID), Some(1_488_440)),
            Ok(0)
        );
        assert_eq!(
            paper_entry_deposit(&b, m, None, None),
            Err(DepositRefusal::DepositUnknown)
        );
        assert_eq!(
            paper_entry_deposit(&b, m, Some([9; 32]), None),
            Err(DepositRefusal::DepositUnknown)
        );
        assert_eq!(
            paper_entry_deposit(&RENT_V0_LEGACY, m, None, None),
            Ok(2_039_280)
        );
        assert_eq!(
            paper_entry_deposit(&RENT_V0_LEGACY, m, None, Some(2_039_280)),
            Ok(0)
        );
        assert_eq!(paper_close_refund(2_039_280), 2_039_280);
        assert_eq!(paper_close_refund(1_513_840), 1_513_840);
        assert_eq!(PaperRentBasis::version_of_id("rent_v1_s455400973"), Some(1));
        assert_eq!(PaperRentBasis::version_of_id("rent_v2"), None);
    }
}
