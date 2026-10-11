//! SCOPE_TXCOST_R9 T2/T3 + PLIM_R8_P3 T-iii-1..4, 4b, 6, 7 (protocol side).
//!
//! Offline, bytes only: messages are compiled and decoded, nothing is signed or sent. Rent values are
//! EVIDENCE FIXTURES with slot provenance (getMinimumBalanceForRentExemption read at slots
//! 455,400,960-963, CHAIN_READONLY_EVIDENCE), not runtime constants: every expectation is derived
//! from the rent snapshot passed in, and the snapshot's slot must equal the observation slot.

use pump_quant_protocol::ix::{BuyExactSolInParams, SellParams, BUY_EXACT_SOL_IN_DISCRIMINATOR};
use pump_quant_protocol::layout::{
    LayoutKey, LayoutRegistry, Side, Variant, Venue, VerifiedLayout,
};
use pump_quant_protocol::message::CompiledMessage;
use pump_quant_protocol::tx_build::{
    ata_account_size, build_pump_buy_exact_sol_in_message, build_pump_sell_message,
    cash_commitment, decode_message, message_costs, plan_buy_deposits,
    planned_lamports_per_signature, predicted_failure_debit, refund_on_close, AccountState,
    BuildEnv, BuyAccountObservation, ComputePlan, DepositPlan, DepositRefusal, FailureCostRefusal,
    FailureDebit, MessageCosts, RentSnapshot, TipPlan, TxChainOutcome, BUY_TRACK_VOLUME_BYTE,
    UVA_ACCOUNT_SIZE,
};
use pump_quant_protocol::venue_accounts::{
    FeeTail, PumpCurveCtx, ATA_PROGRAM_ID, COMPUTE_BUDGET_PROGRAM_ID, PUMP_PROGRAM_ID,
    SYSTEM_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};

/// Observation slot of the rent fixtures (CHAIN_READONLY_EVIDENCE: reads at 455,400,960-963).
const OBS_SLOT: u64 = 455_400_960;
/// rent(165) at OBS_SLOT — SPL ATA.
const RENT_165: u64 = 1_488_440;
/// rent(170) at OBS_SLOT — Token-2022 ATA + ImmutableOwner.
const RENT_170: u64 = 1_513_840;
/// rent(137) at OBS_SLOT — UVA.
const RENT_137: u64 = 1_346_200;
/// What OLD SPL ATAs hold (historical observation only; never a deposit source).
const HISTORICAL_ATA_BALANCE: u64 = 2_039_280;

const CLIP: u64 = 250_000_000;

fn pk(tag: u8) -> [u8; 32] {
    let mut k = [5u8; 32];
    k[0] = tag;
    k
}

fn ctx(token_program: [u8; 32]) -> PumpCurveCtx {
    PumpCurveCtx {
        mint: pk(1),
        user: pk(2),
        fee_recipient: pk(3),
        creator: pk(4),
        token_program,
        is_cashback_coin: false,
        quote_mint: WSOL_MINT,
    }
}

fn sig(seed: u8) -> [u8; 64] {
    let mut s = [0u8; 64];
    for (i, b) in s.iter_mut().enumerate() {
        *b = ((i as u32 * 37 + seed as u32) % 256) as u8;
    }
    s
}

fn registry() -> LayoutRegistry {
    // Fixture registry (gate behaviour only; the slot tag is 3pP1's post-deploy slot, not a proof).
    let mut r = LayoutRegistry::new();
    let t22 = Variant {
        token_2022: true,
        ..Variant::plain()
    };
    for (side, variant, n, seed) in [
        (Side::Buy, Variant::plain(), 18, 1u8),
        (Side::Sell, Variant::plain(), 16, 2u8),
        (Side::Buy, t22, 18, 3u8),
    ] {
        r.record_verified(VerifiedLayout {
            key: LayoutKey {
                venue: Venue::PumpFun,
                side,
                variant,
            },
            account_count: n,
            verifying_slot: 454_779_709,
            verifying_signature: sig(seed),
        })
        .unwrap();
    }
    r
}

/// The pq_daemon live envelope: CU 120,000 @ 5,000 µlamports, 5,000-lamport Sender tip.
/// Compute/tip equal TXCOST_V1_20261010 (pinned by pump-quant-app/tests/txcost_plim_parity.rs).
fn daemon_compute() -> ComputePlan {
    ComputePlan {
        unit_limit: 120_000,
        unit_price_micro_lamports: 5_000,
    }
}

fn daemon_tip() -> TipPlan {
    TipPlan {
        to: pk(9),
        lamports: 5_000,
    }
}

fn buy_msg(c: &PumpCurveCtx, compute: ComputePlan, tip: Option<TipPlan>) -> CompiledMessage {
    let reg = registry();
    let env = BuildEnv {
        compute,
        tip,
        recent_blockhash: [0xAB; 32],
        registry: &reg,
        fee_tail: FeeTail::FeeProgramGlobal,
    };
    build_pump_buy_exact_sol_in_message(
        c,
        BuyExactSolInParams {
            spendable_sol_in: CLIP,
            min_tokens_out: 1_000,
        },
        &env,
    )
    .unwrap()
}

fn rent() -> RentSnapshot {
    RentSnapshot {
        slot: OBS_SLOT,
        minimums: vec![(165, RENT_165), (170, RENT_170), (137, RENT_137)],
    }
}

fn ata_open(c: &PumpCurveCtx, lamports: u64) -> AccountState {
    AccountState::Present {
        owner: c.token_program,
        data_len: ata_account_size(&c.token_program).unwrap(),
        lamports,
        token_mint: Some(c.mint),
    }
}

fn uva_open() -> AccountState {
    AccountState::Present {
        owner: PUMP_PROGRAM_ID,
        data_len: UVA_ACCOUNT_SIZE,
        lamports: RENT_137,
        token_mint: None,
    }
}

fn obs(user_ata: AccountState, uva: AccountState) -> BuyAccountObservation {
    BuyAccountObservation {
        slot: OBS_SLOT,
        user_ata,
        user_volume_accumulator: uva,
        rent: rent(),
    }
}

// ---------------------------------------------------------------------------
// T-iii-1: M is read from the compiled message.
// ---------------------------------------------------------------------------

#[test]
fn m_is_read_from_the_message() {
    let c = ctx(TOKEN_2022_PROGRAM_ID);
    let msg = buy_msg(&c, daemon_compute(), Some(daemon_tip()));
    let costs = message_costs(&msg, planned_lamports_per_signature()).unwrap();
    assert_eq!(
        costs,
        MessageCosts {
            base_fee: 5_000,
            priority_fee: 600,
            tip: 5_000,
            tip_in_same_atomic_message: true,
        }
    );
    // meta.fee = base + priority (the tip is a transfer, NOT in meta.fee).
    assert_eq!(costs.network_fee(), Some(5_600));
    // pq_daemon: M = C + 10,600 + D. D for an absent Token-2022 ATA = rent(170) at the slot.
    let d = plan_buy_deposits(&c, &obs(AccountState::Absent, uva_open())).unwrap();
    assert_eq!(
        cash_commitment(CLIP, &costs, &d),
        Ok(CLIP + 10_600 + RENT_170)
    );
    // Open ATA: M = C + 10,600 (transaction costs still apply).
    let d0 = plan_buy_deposits(&c, &obs(ata_open(&c, RENT_170), uva_open())).unwrap();
    assert_eq!(cash_commitment(CLIP, &costs, &d0), Ok(CLIP + 10_600));

    // The priority fee is charged on the REQUESTED limit, ceiled over 1e6 (3pP1 consumed 88,946 of
    // 120,000 and paid on 120,000). A non-integer product pins the ceil.
    let odd = ComputePlan {
        unit_limit: 120_001,
        unit_price_micro_lamports: 5_000,
    };
    let c2 = message_costs(&buy_msg(&c, odd, None), 5_000).unwrap();
    assert_eq!(c2.priority_fee, 601, "ceil(120,001 x 5,000 / 1e6)");
    assert_eq!((c2.tip, c2.tip_in_same_atomic_message), (0, false));
    // base = lamports_per_signature x required signatures (1: payer only).
    assert_eq!(message_costs(&msg, 7_000).unwrap().base_fee, 7_000);
}

// ---------------------------------------------------------------------------
// T-iii-2 / T2 cases A and E: a deposit only when the ATA is absent.
// ---------------------------------------------------------------------------

#[test]
fn deposit_only_when_ata_not_open() {
    for tp in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let c = ctx(tp);
        let want = if tp == TOKEN_PROGRAM_ID {
            RENT_165
        } else {
            RENT_170
        };
        // (A) absent: due — the message is shown to create it (CreateIdempotent precedes the pump ix).
        let a = plan_buy_deposits(&c, &obs(AccountState::Absent, uva_open())).unwrap();
        assert_eq!(
            a,
            DepositPlan {
                ata_lamports: want,
                uva_lamports: 0
            }
        );
        let d = decode_message(&buy_msg(&c, daemon_compute(), Some(daemon_tip())).bytes).unwrap();
        let create = d
            .instructions
            .iter()
            .position(|ix| ix.program_id == ATA_PROGRAM_ID && ix.data == [1u8])
            .expect("CreateIdempotent creates the ATA");
        let pump = d
            .instructions
            .iter()
            .position(|ix| ix.program_id == PUMP_PROGRAM_ID)
            .unwrap();
        assert!(create < pump);
        // (E) existing + valid: no new deposit, whatever balance it holds (old ATAs hold 2,039,280).
        for bal in [want, HISTORICAL_ATA_BALANCE] {
            let e = plan_buy_deposits(&c, &obs(ata_open(&c, bal), uva_open())).unwrap();
            assert_eq!(e.total(), Some(0));
        }
    }
}

// ---------------------------------------------------------------------------
// T2 case X: existing but incompatible -> named refusal.
// ---------------------------------------------------------------------------

#[test]
fn incompatible_existing_accounts_are_refused_by_name() {
    let c = ctx(TOKEN_2022_PROGRAM_ID);
    let bad = [
        // wrong owner (SPL program for a Token-2022 mint)
        AccountState::Present {
            owner: TOKEN_PROGRAM_ID,
            data_len: 170,
            lamports: RENT_170,
            token_mint: Some(c.mint),
        },
        // wrong size
        AccountState::Present {
            owner: TOKEN_2022_PROGRAM_ID,
            data_len: 165,
            lamports: RENT_170,
            token_mint: Some(c.mint),
        },
        // wrong mint
        AccountState::Present {
            owner: TOKEN_2022_PROGRAM_ID,
            data_len: 170,
            lamports: RENT_170,
            token_mint: Some(pk(77)),
        },
        // not a token account
        AccountState::Present {
            owner: TOKEN_2022_PROGRAM_ID,
            data_len: 170,
            lamports: RENT_170,
            token_mint: None,
        },
    ];
    for ata in bad {
        let r = plan_buy_deposits(&c, &obs(ata, uva_open())).unwrap_err();
        assert_eq!(r, DepositRefusal::AtaIncompatible, "{ata:?}");
        assert_eq!(r.label(), "ata_incompatible");
    }
    for uva in [
        AccountState::Present {
            owner: SYSTEM_PROGRAM_ID,
            data_len: 137,
            lamports: RENT_137,
            token_mint: None,
        },
        AccountState::Present {
            owner: PUMP_PROGRAM_ID,
            data_len: 136,
            lamports: RENT_137,
            token_mint: None,
        },
    ] {
        let r = plan_buy_deposits(&c, &obs(AccountState::Absent, uva)).unwrap_err();
        assert_eq!(r.label(), "uva_incompatible");
    }
}

// ---------------------------------------------------------------------------
// T-iii-3: the deposit is recoverable at the account's ACTUAL balance.
// ---------------------------------------------------------------------------

#[test]
fn deposit_is_recoverable_on_full_exit() {
    let c = ctx(TOKEN_PROGRAM_ID);
    // Refund = actual lamports, not today's minimum: an old ATA returns 2,039,280, a new one rent(165).
    assert_eq!(
        refund_on_close(&ata_open(&c, HISTORICAL_ATA_BALANCE)),
        HISTORICAL_ATA_BALANCE
    );
    assert_eq!(refund_on_close(&ata_open(&c, RENT_165)), RENT_165);
    assert_eq!(refund_on_close(&ata_open(&c, RENT_165 + 1)), RENT_165 + 1);
    assert_eq!(refund_on_close(&AccountState::Absent), 0);
    // The close rides the full-exit sell; its cost is that message's network fee (one signature +
    // priority), counted as a network fee — the deposit itself is never a fee.
    let reg = registry();
    let env = BuildEnv {
        compute: daemon_compute(),
        tip: Some(daemon_tip()),
        recent_blockhash: [0xAB; 32],
        registry: &reg,
        fee_tail: FeeTail::FeeProgramGlobal,
    };
    let sp = SellParams {
        token_amount: 1_000,
        min_sol_out: 1,
    };
    let with_close = build_pump_sell_message(&c, sp, &env, true).unwrap();
    let without = build_pump_sell_message(&c, sp, &env, false).unwrap();
    let a = message_costs(&with_close, 5_000).unwrap();
    let b = message_costs(&without, 5_000).unwrap();
    assert_eq!(
        a, b,
        "closing adds no fee beyond the sell's own signature + priority"
    );
    assert_eq!(a.network_fee(), Some(5_600));
    let d = decode_message(&with_close.bytes).unwrap();
    assert!(d
        .instructions
        .iter()
        .any(|ix| ix.program_id == TOKEN_PROGRAM_ID && ix.data == [9u8]));
}

// ---------------------------------------------------------------------------
// T-iii-4: the deposit size is token-program specific and read from rent, never hardcoded.
// ---------------------------------------------------------------------------

#[test]
fn deposit_size_is_token_program_specific() {
    assert_eq!(ata_account_size(&TOKEN_PROGRAM_ID), Some(165));
    assert_eq!(ata_account_size(&TOKEN_2022_PROGRAM_ID), Some(170));
    assert_eq!(ata_account_size(&pk(42)), None);

    let spl = plan_buy_deposits(
        &ctx(TOKEN_PROGRAM_ID),
        &obs(AccountState::Absent, uva_open()),
    );
    let t22 = plan_buy_deposits(
        &ctx(TOKEN_2022_PROGRAM_ID),
        &obs(AccountState::Absent, uva_open()),
    );
    assert_eq!(spl.unwrap().ata_lamports, RENT_165);
    assert_eq!(t22.unwrap().ata_lamports, RENT_170);
    assert_ne!(
        RENT_165, HISTORICAL_ATA_BALANCE,
        "2,039,280 is not current rent"
    );

    // The value follows the rent INPUT at the slot: a different snapshot gives a different deposit.
    let mut o = obs(AccountState::Absent, uva_open());
    o.rent.minimums = vec![(165, 1_000_001), (170, 1_000_002)];
    assert_eq!(
        plan_buy_deposits(&ctx(TOKEN_PROGRAM_ID), &o)
            .unwrap()
            .ata_lamports,
        1_000_001
    );
    assert_eq!(
        plan_buy_deposits(&ctx(TOKEN_2022_PROGRAM_ID), &o)
            .unwrap()
            .ata_lamports,
        1_000_002
    );

    // Unknown token program -> deposit_unknown.
    let r = plan_buy_deposits(&ctx(pk(42)), &obs(AccountState::Absent, uva_open())).unwrap_err();
    assert_eq!(r.label(), "deposit_unknown");
    // Rent not read for the size -> deposit_unknown (never a default).
    let mut o = obs(AccountState::Absent, uva_open());
    o.rent.minimums = vec![(165, RENT_165)];
    assert_eq!(
        plan_buy_deposits(&ctx(TOKEN_2022_PROGRAM_ID), &o),
        Err(DepositRefusal::DepositUnknown)
    );
    // Rent read at a different slot than the accounts -> deposit_unknown.
    let mut o = obs(AccountState::Absent, uva_open());
    o.rent.slot = OBS_SLOT - 1;
    assert_eq!(
        plan_buy_deposits(&ctx(TOKEN_PROGRAM_ID), &o),
        Err(DepositRefusal::DepositUnknown)
    );
}

// ---------------------------------------------------------------------------
// T-iii-4b: UVA absent with track_volume = 0x00 -> named refusal; the byte is never flipped.
// ---------------------------------------------------------------------------

#[test]
fn uva_absent_with_track_volume_off_is_refused_by_name() {
    for tp in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
        let c = ctx(tp);
        for ata in [AccountState::Absent, ata_open(&c, RENT_170)] {
            let r = plan_buy_deposits(&c, &obs(ata, AccountState::Absent)).unwrap_err();
            assert_eq!(r, DepositRefusal::UvaAbsentTrackVolumeOffUnverified);
            assert_eq!(r.label(), "uva_absent_track_volume_off_unverified");
        }
        // Even with rent(137) in the snapshot, a reservation alone does not prove success: refused.
        assert!(plan_buy_deposits(&c, &obs(AccountState::Absent, AccountState::Absent)).is_err());
        // The built byte stays 0x00 (never silently set to Some(true) to make the case "known").
        assert_eq!(BUY_TRACK_VOLUME_BYTE, 0x00);
        let d = decode_message(&buy_msg(&c, daemon_compute(), None).bytes).unwrap();
        let pump = d
            .instructions
            .iter()
            .find(|ix| ix.program_id == PUMP_PROGRAM_ID)
            .unwrap();
        assert_eq!(pump.data[..8], BUY_EXACT_SOL_IN_DISCRIMINATOR);
        assert_eq!(pump.data.len(), 25);
        assert_eq!(pump.data[24], 0x00, "track_volume = OptionBool(false)");
    }
}

// ---------------------------------------------------------------------------
// T-iii-6: M overflow is named.
// ---------------------------------------------------------------------------

#[test]
fn m_overflow_is_named() {
    let costs = MessageCosts {
        base_fee: 5_000,
        priority_fee: 600,
        tip: 5_000,
        tip_in_same_atomic_message: true,
    };
    let d = DepositPlan {
        ata_lamports: RENT_170,
        uva_lamports: 0,
    };
    let e = cash_commitment(u64::MAX - 10_000, &costs, &d).unwrap_err();
    assert_eq!(e.label(), "cash_commitment_overflow");
    let big = DepositPlan {
        ata_lamports: u64::MAX,
        uva_lamports: 1,
    };
    assert!(cash_commitment(1, &costs, &big).is_err());
    // Exactly at the edge is representable.
    let exact = u64::MAX - 10_600 - RENT_170;
    assert_eq!(cash_commitment(exact, &costs, &d), Ok(u64::MAX));
}

// ---------------------------------------------------------------------------
// T3 / T-iii-7: failure costs are a PREDICTION for this message structure.
// ---------------------------------------------------------------------------

/// Pins the structure the 5,600 prediction depends on: ONE message, ONE signature, the tip transfer is
/// an instruction of the same message as the pump instruction (so it reverts with it).
#[test]
fn the_tip_is_inside_the_same_atomic_message() {
    let c = ctx(TOKEN_2022_PROGRAM_ID);
    let msg = buy_msg(&c, daemon_compute(), Some(daemon_tip()));
    let d = decode_message(&msg.bytes).unwrap();
    assert_eq!(d.num_required_signatures, 1);
    let progs: Vec<[u8; 32]> = d.instructions.iter().map(|ix| ix.program_id).collect();
    assert_eq!(
        progs,
        vec![
            COMPUTE_BUDGET_PROGRAM_ID,
            COMPUTE_BUDGET_PROGRAM_ID,
            ATA_PROGRAM_ID,
            PUMP_PROGRAM_ID,
            SYSTEM_PROGRAM_ID,
        ]
    );
    let tip = &d.instructions[4];
    assert_eq!(tip.accounts, vec![c.user, pk(9)]);
    assert_eq!(tip.data[4..12], 5_000u64.to_le_bytes());
    assert!(
        message_costs(&msg, 5_000)
            .unwrap()
            .tip_in_same_atomic_message
    );
    // A SEPARATE tip transaction (compute + transfer, no pump instruction) is a different structure:
    // its tip does not revert with the buy, so the processed-failed prediction is refused for it.
    let tip_only = pump_quant_protocol::message::compile_message(
        &c.user,
        &[0xAB; 32],
        &[
            pump_quant_protocol::message::set_compute_unit_limit(120_000),
            pump_quant_protocol::message::set_compute_unit_price(5_000),
            pump_quant_protocol::message::system_transfer(&c.user, &pk(9), 5_000),
        ],
    )
    .unwrap();
    let tc = message_costs(&tip_only, 5_000).unwrap();
    assert_eq!(tc.tip, 5_000);
    assert!(!tc.tip_in_same_atomic_message);
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::ProcessedFailed, &tc),
        Err(FailureCostRefusal::UnsupportedSubmissionStructure)
    );
}

#[test]
fn processed_failed_tx_costs_fee_not_tip_or_spend() {
    let c = ctx(TOKEN_2022_PROGRAM_ID);
    let costs = message_costs(&buy_msg(&c, daemon_compute(), Some(daemon_tip())), 5_000).unwrap();
    // Processed-failed: meta.fee only (5,000 + 600). Spend, deposit and the in-message tip revert.
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::ProcessedFailed, &costs),
        Ok(FailureDebit {
            lamports: 5_600,
            settled: true
        })
    );
    // Unsubmitted: zero chain cost, final.
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::Unsubmitted, &costs),
        Ok(FailureDebit {
            lamports: 0,
            settled: true
        })
    );
    // Never-landed: zero so far, but UNCERTAIN until expiry or reconciliation.
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::NeverLanded, &costs),
        Ok(FailureDebit {
            lamports: 0,
            settled: false
        })
    );
    // Landed: actual evidence governs, not a prediction.
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::Landed, &costs),
        Err(FailureCostRefusal::LandedUsesActuals)
    );
    // Not generalised: a tip outside the atomic message (separate tip tx / bundle) is refused.
    let separate = MessageCosts {
        tip_in_same_atomic_message: false,
        ..costs
    };
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::ProcessedFailed, &separate),
        Err(FailureCostRefusal::UnsupportedSubmissionStructure)
    );
    // No tip at all: the fee alone, no structure assumption needed.
    let no_tip = message_costs(&buy_msg(&c, daemon_compute(), None), 5_000).unwrap();
    assert_eq!(
        predicted_failure_debit(TxChainOutcome::ProcessedFailed, &no_tip)
            .unwrap()
            .lamports,
        5_600
    );
}
