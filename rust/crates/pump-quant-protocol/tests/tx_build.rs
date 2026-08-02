//! End-to-end unsigned-message assembly: instruction sequences, the
//! track_volume byte, ATA/close wiring, and the fail-closed refusals on tip
//! and blockhash. This is the junction under test.

use pump_quant_protocol::ix::{BuyParams, SellParams};
use pump_quant_protocol::layout::{
    LayoutError, LayoutKey, LayoutRegistry, Side, Variant, Venue, VerifiedLayout,
};
use pump_quant_protocol::message::assemble_transaction;
use pump_quant_protocol::tx_build::{
    build_buy_data_with_volume_flag, build_pump_buy_message, build_pump_sell_message, BuildEnv,
    ComputePlan, TipPlan, TxBuildError,
};
use pump_quant_protocol::venue_accounts::{
    FeeTail, PumpCurveCtx, SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};

/// The fee tail every test builds under. `FeeProgramGlobal` is one of the two
/// live hypotheses; which one is settled by the on-chain discriminating test.
/// The tests here assert the GATE's behaviour, which is tail-independent.
const TAIL: FeeTail = FeeTail::FeeProgramGlobal;

fn env<'a>(reg: &'a LayoutRegistry, tail: FeeTail, bhash: [u8; 32]) -> BuildEnv<'a> {
    BuildEnv {
        compute: compute(),
        tip: Some(tip()),
        recent_blockhash: bhash,
        registry: reg,
        fee_tail: tail,
    }
}

fn lkey(side: Side) -> LayoutKey {
    LayoutKey {
        venue: Venue::PumpFun,
        side,
        variant: Variant::plain(),
    }
}

/// A registry with both bonding-curve layouts verified at the post-finding
/// counts: 18 for buy, 16 for a non-cashback sell.
fn verified_registry() -> LayoutRegistry {
    let mut r = LayoutRegistry::new();
    r.record_verified(VerifiedLayout {
        key: lkey(Side::Buy),
        account_count: 18,
        verifying_slot: 436_828_370,
        verifying_signature: [1u8; 64],
    })
    .unwrap();
    r.record_verified(VerifiedLayout {
        key: lkey(Side::Sell),
        account_count: 16,
        verifying_slot: 436_828_370,
        verifying_signature: [2u8; 64],
    })
    .unwrap();
    r
}

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
    let msg = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    )
    .unwrap();
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
    let with = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    )
    .unwrap();
    let without = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: None,
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    )
    .unwrap();
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
    let full = build_pump_sell_message(
        &ctx(),
        sp,
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
        true,
    )
    .unwrap();
    let partial = build_pump_sell_message(
        &ctx(),
        sp,
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
        false,
    )
    .unwrap();
    // The close_account instruction references the token program; a partial
    // rung compiles a strictly shorter byte string (one fewer instruction).
    assert!(full.bytes.len() > partial.bytes.len());
    assert!(full.account_keys.contains(&TOKEN_PROGRAM_ID));
}

// -- fail-closed refusals ----------------------------------------------------

#[test]
fn negative_control_zero_blockhash_refuses() {
    let e = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: [0u8; 32],
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    );
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroBlockhash);
}

#[test]
fn negative_control_zero_tip_refuses() {
    let bad = TipPlan {
        to: pk(9),
        lamports: 0,
    };
    let e = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(bad),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    );
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroTip);
}

#[test]
fn negative_control_zero_tip_account_refuses() {
    let bad = TipPlan {
        to: [0u8; 32],
        lamports: 1_000,
    };
    let e = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(bad),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    );
    assert_eq!(e.unwrap_err(), TxBuildError::ZeroTipAccount);
}

#[test]
fn negative_control_account_refusal_propagates() {
    let mut c = ctx();
    c.creator = [0u8; 32];
    let e = build_pump_buy_message(
        &c,
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    );
    assert!(matches!(e.unwrap_err(), TxBuildError::Accounts(_)));
}

/// System program appears once, read-only, despite the tip transfer also
/// naming it — the compiler's dedup holds end-to-end.
#[test]
fn system_program_deduped_end_to_end() {
    let msg = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &verified_registry(),
            fee_tail: TAIL,
        },
    )
    .unwrap();
    assert_eq!(
        msg.account_keys
            .iter()
            .filter(|k| **k == SYSTEM_PROGRAM_ID)
            .count(),
        1
    );
}

// -- the layout gate, end to end ---------------------------------------------

/// THE CONTROL THE 2026-08-02 FALSIFICATION BOUGHT. With no verified fixture,
/// the builder refuses outright. Before this existed, an unverified layout
/// produced a perfectly well-formed transaction that the chain rejected.
#[test]
fn negative_control_unverified_layout_cannot_be_built() {
    let empty = LayoutRegistry::new();
    let e = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &empty,
            fee_tail: TAIL,
        },
    );
    assert_eq!(
        e.unwrap_err(),
        TxBuildError::Layout(LayoutError::Unverified(lkey(Side::Buy)))
    );

    let sp = SellParams {
        token_amount: 5,
        min_sol_out: 0,
    };
    let e2 = build_pump_sell_message(
        &ctx(),
        sp,
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &empty,
            fee_tail: TAIL,
        },
        false,
    );
    assert_eq!(
        e2.unwrap_err(),
        TxBuildError::Layout(LayoutError::Unverified(lkey(Side::Sell)))
    );
}

/// The falsified 17-account shape must not pass a fixture that proves 18. This
/// is the exact defect, encoded so it cannot return.
#[test]
fn negative_control_the_falsified_17_account_shape_is_rejected() {
    let reg = verified_registry(); // proves buy == 18
    let e = build_pump_buy_message(
        &ctx(),
        params(),
        &BuildEnv {
            compute: compute(),
            tip: Some(tip()),
            recent_blockhash: bh(),
            registry: &reg,
            fee_tail: FeeTail::None,
        },
    );
    assert_eq!(
        e.unwrap_err(),
        TxBuildError::Layout(LayoutError::CountDisagrees {
            key: lkey(Side::Buy),
            built: 17,
            verified: 18
        })
    );
}

/// A cashback mint is a DIFFERENT layout key, so verifying the plain sell does
/// not silently authorise the cashback one. Permutations are proven one by one.
#[test]
fn negative_control_cashback_is_a_separate_layout_that_must_be_verified() {
    let reg = verified_registry();
    let mut c = ctx();
    c.is_cashback_coin = true;
    let sp = SellParams {
        token_amount: 5,
        min_sol_out: 0,
    };
    let e = build_pump_sell_message(&c, sp, &env(&reg, TAIL, bh()), false);
    assert!(matches!(
        e.unwrap_err(),
        TxBuildError::Layout(LayoutError::Unverified(_))
    ));
}

/// Token-2022 is also a separate layout key: the token program is an ATA seed,
/// so every associated account moves even though the count does not.
#[test]
fn negative_control_token_2022_is_a_separate_layout() {
    let reg = verified_registry();
    let mut c = ctx();
    c.token_program = pump_quant_protocol::venue_accounts::TOKEN_2022_PROGRAM_ID;
    let e = build_pump_buy_message(&c, params(), &env(&reg, TAIL, bh()));
    assert!(matches!(
        e.unwrap_err(),
        TxBuildError::Layout(LayoutError::Unverified(_))
    ));
}
