//! End-to-end unsigned-message assembly: instruction sequences, the
//! track_volume byte, ATA/close wiring, and the fail-closed refusals on tip
//! and blockhash. This is the junction under test.

use pump_quant_protocol::ix::{BuyParams, SellParams};
use pump_quant_protocol::message::assemble_transaction;
use pump_quant_protocol::tx_build::{
    build_buy_data_with_volume_flag, build_pump_buy_message, build_pump_sell_message, ComputePlan,
    TipPlan, TxBuildError,
};
use pump_quant_protocol::venue_accounts::{
    PumpCurveCtx, SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};

fn pk(tag: u8) -> [u8; 32] {
    let mut k = [5u8; 32];
    k[0] = tag;
    k
}

fn ctx() -> PumpCurveCtx {
    PumpCurveCtx {
        mint: pk(1),
        user: pk(2),
        fee_recipient: pk(3),
        creator: pk(4),
        token_program: TOKEN_PROGRAM_ID,
        is_cashback_coin: false,
        quote_mint: WSOL_MINT,
    }
}

fn compute() -> ComputePlan {
    ComputePlan {
        unit_limit: 120_000,
        unit_price_micro_lamports: 5_000,
    }
}

fn bh() -> [u8; 32] {
    [0xAB; 32]
}

fn tip() -> TipPlan {
    TipPlan {
        to: pk(9),
        lamports: 1_000,
    }
}

fn params() -> BuyParams {
    BuyParams {
        min_tokens_out: 1,
        max_sol_cost: 100_000_000,
    }
}

// -- track_volume byte -------------------------------------------------------

#[test]
fn buy_data_appends_false_volume_flag() {
    let d = build_buy_data_with_volume_flag(params());
    assert_eq!(d.len(), 25, "24-byte blob + one OptionBool byte");
    assert_eq!(d[24], 0, "track_volume = false, fail-closed");
    // The first 24 bytes are byte-identical to the pinned data blob.
    assert_eq!(
        &d[..24],
        &pump_quant_protocol::ix::build_buy_ix(params())[..]
    );
}

// -- buy sequence ------------------------------------------------------------

#[test]
fn buy_message_compiles_and_signs_payer_first() {
    let msg = build_pump_buy_message(&ctx(), params(), compute(), Some(tip()), &bh()).unwrap();
    // Payer (user) is the sole signer and the first account key.
    assert_eq!(msg.num_required_signatures, 1);
    assert_eq!(msg.account_keys[0], ctx().user);
    // Blockhash sits after the account-key block (offset varies with key
    // count); assert it is embedded exactly once.
    let bh = bh();
    let found = msg.bytes.windows(32).filter(|w| *w == bh).count();
    assert_eq!(found, 1, "blockhash embedded once");
    // Two signatures would be rejected; one assembles.
    let sig = [1u8; 64];
    assert!(assemble_transaction(&msg, &[sig]).is_ok());
}

#[test]
fn buy_without_tip_omits_the_transfer() {
    let with = build_pump_buy_message(&ctx(), params(), compute(), Some(tip()), &bh()).unwrap();
    let without = build_pump_buy_message(&ctx(), params(), compute(), None, &bh()).unwrap();
    // The tip adds the tip destination as a writable account; without it that
    // key is absent.
    assert!(with.account_keys.contains(&tip().to));
    assert!(!without.account_keys.contains(&tip().to));
}

// -- sell sequence -----------------------------------------------------------

#[test]
fn sell_full_exit_closes_ata_partial_does_not() {
    let sp = SellParams {
        token_amount: 500,
        min_sol_out: 0,
    };
    let full = build_pump_sell_message(&ctx(), sp, compute(), Some(tip()), &bh(), true).unwrap();
    let partial =
        build_pump_sell_message(&ctx(), sp, compute(), Some(tip()), &bh(), false).unwrap();
    // The close_account instruction references the token program; a partial
    // rung compiles a strictly shorter byte string (one fewer instruction).
    assert!(full.bytes.len() > partial.bytes.len());
    assert!(full.account_keys.contains(&TOKEN_PROGRAM_ID));
}

// -- fail-closed refusals ----------------------------------------------------

#[test]
fn negative_control_zero_blockhash_refuses() {
    let e = build_pump_buy_message(&ctx(), params(), compute(), Some(tip()), &[0u8; 32]);
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroBlockhash);
}

#[test]
fn negative_control_zero_tip_refuses() {
    let bad = TipPlan {
        to: pk(9),
        lamports: 0,
    };
    let e = build_pump_buy_message(&ctx(), params(), compute(), Some(bad), &bh());
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroTip);
}

#[test]
fn negative_control_zero_tip_account_refuses() {
    let bad = TipPlan {
        to: [0u8; 32],
        lamports: 1_000,
    };
    let e = build_pump_buy_message(&ctx(), params(), compute(), Some(bad), &bh());
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroTipAccount);
}

#[test]
fn negative_control_account_refusal_propagates() {
    let mut c = ctx();
    c.creator = [0u8; 32];
    let e = build_pump_buy_message(&c, params(), compute(), Some(tip()), &bh());
    assert!(matches!(e.unwrap_err(), TxBuildError::Accounts(_)));
}

/// System program appears once, read-only, despite the tip transfer also
/// naming it — the compiler's dedup holds end-to-end.
#[test]
fn system_program_deduped_end_to_end() {
    let msg = build_pump_buy_message(&ctx(), params(), compute(), Some(tip()), &bh()).unwrap();
    assert_eq!(
        msg.account_keys
            .iter()
            .filter(|k| **k == SYSTEM_PROGRAM_ID)
            .count(),
        1
    );
}
