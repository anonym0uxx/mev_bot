//! FEES R10 F8: paper/builder deposit parity. Identical inputs (same AccountState, same RentSnapshot incl. slot, same
//! token program, same TxCostConfig) -> paper planned cost == builder planned cost, deposits INCLUDED
//! (txcost leg 10,600 + deposit). The builder side is computed here independently: its own plans
//! (`builder_plans` -> `builder_planned_leg_lamports`) and its own `plan_buy_deposits` call on an observation the
//! test constructs, with the builder's strict rule rent slot == observation slot.
#![forbid(unsafe_code)]

use pump_quant_app::config::Config;
use pump_quant_app::paper_deposit::{paper_entry_deposit, PaperRentBasis, RENT_V1_S455400973};
use pump_quant_junction::builder_txcost::{
    builder_planned_leg_lamports, builder_plans, BUILDER_TXCOST,
};
use pump_quant_protocol::tx_build::{
    plan_buy_deposits, AccountState, BuyAccountObservation, DepositRefusal, RentSnapshot,
    UVA_ACCOUNT_SIZE,
};
use pump_quant_protocol::venue_accounts::{
    PumpCurveCtx, PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};

const MINT: [u8; 32] = [0x11; 32];
/// proc/CHAIN_READONLY_EVIDENCE.md §2 (slot ~455.40M): rent(165) / rent(170) / rent(137).
const SNAP_SLOT: u64 = 455_400_973;
const RENT_165: u64 = 1_488_440;
const RENT_170: u64 = 1_513_840;
const RENT_137: u64 = 1_346_200;

fn snapshot() -> RentSnapshot {
    RentSnapshot {
        slot: SNAP_SLOT,
        minimums: vec![(165, RENT_165), (170, RENT_170), (137, RENT_137)],
    }
}

/// The builder's planned cost on one observation: its leg from its own plans + its own deposit plan.
fn builder_cost(token_program: [u8; 32], user_ata: AccountState) -> Result<u64, DepositRefusal> {
    let ctx = PumpCurveCtx {
        mint: MINT,
        user: [2; 32],
        fee_recipient: [3; 32],
        creator: [4; 32],
        token_program,
        is_cashback_coin: false,
        quote_mint: WSOL_MINT,
    };
    let obs = BuyAccountObservation {
        slot: SNAP_SLOT,
        user_ata,
        user_volume_accumulator: AccountState::Present {
            owner: PUMP_PROGRAM_ID,
            data_len: UVA_ACCOUNT_SIZE,
            lamports: RENT_137,
            token_mint: None,
        },
        rent: snapshot(),
    };
    let (c, t) = builder_plans(&BUILDER_TXCOST);
    let leg = builder_planned_leg_lamports(BUILDER_TXCOST.signatures, c, t);
    let d = plan_buy_deposits(&ctx, &obs)?;
    Ok(leg + d.total().expect("deposit total"))
}

/// The paper's planned cost: the v1 paper leg + the paper deposit at the v1 rent basis.
fn paper_cost(
    token_program: Option<[u8; 32]>,
    deposited: Option<u64>,
) -> Result<u64, DepositRefusal> {
    let mut cfg = Config::dev_portable();
    cfg.apply("txcost_version", 1).unwrap();
    let basis = PaperRentBasis::by_version(1);
    assert_eq!(basis, RENT_V1_S455400973);
    assert_eq!(
        basis.snapshot(),
        snapshot(),
        "paper basis == the builder's snapshot, slot included"
    );
    Ok(cfg.txcost_for(false).planned_leg_lamports()
        + paper_entry_deposit(&basis, MINT, token_program, deposited)?)
}

#[test]
fn spl_absent_ata_paper_equals_builder_deposit_included() {
    let b = builder_cost(TOKEN_PROGRAM_ID, AccountState::Absent);
    assert_eq!(b, Ok(10_600 + RENT_165));
    assert_eq!(paper_cost(Some(TOKEN_PROGRAM_ID), None), b);
}

#[test]
fn t22_absent_ata_paper_equals_builder_deposit_included() {
    let b = builder_cost(TOKEN_2022_PROGRAM_ID, AccountState::Absent);
    assert_eq!(b, Ok(10_600 + RENT_170));
    assert_eq!(paper_cost(Some(TOKEN_2022_PROGRAM_ID), None), b);
}

#[test]
fn existing_valid_ata_paper_equals_builder_deposit_zero() {
    // The account holds what paper deposited (a historical 2,039,280 balance included): valid, deposit 0.
    for (tp, size, bal) in [
        (TOKEN_PROGRAM_ID, 165, RENT_165),
        (TOKEN_2022_PROGRAM_ID, 170, RENT_170),
        (TOKEN_PROGRAM_ID, 165, 2_039_280),
    ] {
        let st = AccountState::Present {
            owner: tp,
            data_len: size,
            lamports: bal,
            token_mint: Some(MINT),
        };
        let b = builder_cost(tp, st);
        assert_eq!(b, Ok(10_600));
        assert_eq!(paper_cost(Some(tp), Some(bal)), b);
    }
}

#[test]
fn unknown_token_program_both_refuse_deposit_unknown() {
    let unknown = [0x42; 32];
    let b = builder_cost(unknown, AccountState::Absent);
    assert_eq!(b, Err(DepositRefusal::DepositUnknown));
    assert_eq!(b.unwrap_err().label(), "deposit_unknown");
    assert_eq!(paper_cost(Some(unknown), None), b);
    // Paper with NO token program evidence: the same named refusal, no default program.
    assert_eq!(paper_cost(None, None), b);
}
