//! §4.1 / §4.2 account-list construction: shapes, ordering, flags, and the
//! fail-closed refusals. The negative controls are the point — a builder that
//! cannot refuse is the defect class this commission has produced five times.

use pump_quant_protocol::venue_accounts::{
    pump_buy_accounts, pump_sell_accounts, pumpswap_buy_accounts, pumpswap_sell_accounts,
    AccountBuildError, PumpCurveCtx, PumpSwapCtx, FEE_PROGRAM_ID, PUMPSWAP_FEE_CONFIG,
    PUMPSWAP_GLOBAL_CONFIG, PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR, PUMPSWAP_PROGRAM_ID,
    PUMP_FEE_CONFIG, PUMP_GLOBAL, PUMP_PROGRAM_ID, SYSTEM_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};

fn pk(tag: u8) -> [u8; 32] {
    let mut k = [7u8; 32];
    k[0] = tag;
    k
}

fn curve_ctx() -> PumpCurveCtx {
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

fn swap_ctx() -> PumpSwapCtx {
    PumpSwapCtx {
        pool: pk(10),
        user: pk(11),
        base_mint: pk(12),
        quote_mint: WSOL_MINT,
        pool_base_token_account: pk(13),
        pool_quote_token_account: pk(14),
        protocol_fee_recipient: pk(15),
        base_token_program: TOKEN_PROGRAM_ID,
        quote_token_program: TOKEN_PROGRAM_ID,
        coin_creator: pk(16),
        is_cashback_coin: false,
    }
}

// -- pump.fun shapes ---------------------------------------------------------

#[test]
fn pump_buy_is_17_accounts_in_section_4_1_order() {
    let v = pump_buy_accounts(&curve_ctx()).unwrap();
    assert_eq!(v.len(), 17);
    assert_eq!(v[0].pubkey, PUMP_GLOBAL);
    assert!(!v[0].is_writable && !v[0].is_signer);
    assert!(v[1].is_writable, "fee_recipient is writable");
    assert_eq!(v[2].pubkey, curve_ctx().mint);
    assert!(v[3].is_writable, "bonding_curve is writable");
    assert!(v[6].is_signer && v[6].is_writable, "user signs");
    assert_eq!(v[7].pubkey, SYSTEM_PROGRAM_ID);
    assert_eq!(v[8].pubkey, TOKEN_PROGRAM_ID);
    assert!(v[9].is_writable, "creator_vault is writable");
    assert_eq!(v[11].pubkey, PUMP_PROGRAM_ID);
    assert!(v[13].is_writable, "user_volume_accumulator is writable");
    assert_eq!(v[14].pubkey, PUMP_FEE_CONFIG);
    assert_eq!(v[15].pubkey, FEE_PROGRAM_ID);
    // Exactly one signer in the whole list.
    assert_eq!(v.iter().filter(|m| m.is_signer).count(), 1);
}

#[test]
fn pump_buy_bonding_curve_v2_is_last() {
    let v = pump_buy_accounts(&curve_ctx()).unwrap();
    // [16] is the ["bonding-curve-v2", mint] PDA and must be last (§4.1).
    let bcv2 = v[16].pubkey;
    assert!(!v[16].is_writable);
    // It is a real derived PDA, distinct from every other account in the list.
    assert_eq!(v.iter().filter(|m| m.pubkey == bcv2).count(), 1);
}

#[test]
fn pump_sell_non_cashback_is_15_accounts() {
    let v = pump_sell_accounts(&curve_ctx()).unwrap();
    assert_eq!(v.len(), 15);
    // Sell order swaps creator_vault to [8] and token_program to [9] (§4.1).
    assert!(v[8].is_writable, "creator_vault at [8] on sell");
    assert_eq!(v[9].pubkey, TOKEN_PROGRAM_ID);
    assert_eq!(v[12].pubkey, PUMP_FEE_CONFIG);
    assert_eq!(v[13].pubkey, FEE_PROGRAM_ID);
}

#[test]
fn pump_sell_cashback_inserts_uva_before_bcv2() {
    let mut ctx = curve_ctx();
    let base = pump_sell_accounts(&ctx).unwrap();
    ctx.is_cashback_coin = true;
    let cash = pump_sell_accounts(&ctx).unwrap();
    assert_eq!(cash.len(), 16);
    // The trailing bonding_curve_v2 is identical; the inserted [14] is the
    // writable user_volume_accumulator.
    assert_eq!(cash[15].pubkey, base[14].pubkey);
    assert!(cash[14].is_writable);
    assert_ne!(cash[14].pubkey, base[14].pubkey);
}

#[test]
fn pump_buy_and_sell_share_the_first_eight_accounts() {
    let b = pump_buy_accounts(&curve_ctx()).unwrap();
    let s = pump_sell_accounts(&curve_ctx()).unwrap();
    for i in 0..8 {
        assert_eq!(b[i], s[i], "buy/sell diverge at [{i}]");
    }
}

// -- pump.fun refusals -------------------------------------------------------

#[test]
fn negative_control_zeroed_creator_refuses() {
    let mut ctx = curve_ctx();
    ctx.creator = [0u8; 32];
    assert_eq!(
        pump_buy_accounts(&ctx),
        Err(AccountBuildError::ZeroedInput("creator"))
    );
}

#[test]
fn negative_control_zeroed_fee_recipient_refuses() {
    let mut ctx = curve_ctx();
    ctx.fee_recipient = [0u8; 32];
    assert_eq!(
        pump_buy_accounts(&ctx),
        Err(AccountBuildError::ZeroedInput("fee_recipient"))
    );
}

#[test]
fn negative_control_unknown_token_program_refuses() {
    let mut ctx = curve_ctx();
    ctx.token_program = pk(99);
    assert_eq!(
        pump_buy_accounts(&ctx),
        Err(AccountBuildError::UnknownTokenProgram)
    );
    // Token-2022 is a live case and must be accepted.
    ctx.token_program = TOKEN_2022_PROGRAM_ID;
    assert!(pump_buy_accounts(&ctx).is_ok());
}

#[test]
fn negative_control_non_sol_quote_mint_refuses() {
    let mut ctx = curve_ctx();
    ctx.quote_mint = pk(50); // a USDC-quoted curve, layout unverified
    assert_eq!(
        pump_buy_accounts(&ctx),
        Err(AccountBuildError::NonSolQuoteMint)
    );
    assert_eq!(
        pump_sell_accounts(&ctx),
        Err(AccountBuildError::NonSolQuoteMint)
    );
}

/// Token program changes the ATA seeds, so the associated accounts must move.
#[test]
fn token_program_participates_in_ata_derivation() {
    let spl = pump_buy_accounts(&curve_ctx()).unwrap();
    let mut ctx = curve_ctx();
    ctx.token_program = TOKEN_2022_PROGRAM_ID;
    let t22 = pump_buy_accounts(&ctx).unwrap();
    assert_ne!(spl[4].pubkey, t22[4].pubkey, "associated_bonding_curve");
    assert_ne!(spl[5].pubkey, t22[5].pubkey, "associated_user");
}

// -- PumpSwap shapes ---------------------------------------------------------

#[test]
fn pumpswap_buy_is_23_accounts() {
    let v = pumpswap_buy_accounts(&swap_ctx()).unwrap();
    assert_eq!(v.len(), 23);
    assert_eq!(v[2].pubkey, PUMPSWAP_GLOBAL_CONFIG);
    assert_eq!(
        v[16].pubkey, PUMPSWAP_PROGRAM_ID,
        "self-CPI program at [16]"
    );
    assert!(v[17].is_writable, "coin_creator_vault_ata writable");
    assert!(!v[18].is_writable, "vault authority read-only");
    assert_eq!(v[19].pubkey, PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR);
    assert!(v[20].is_writable, "user_volume_accumulator writable");
    assert_eq!(v[21].pubkey, PUMPSWAP_FEE_CONFIG);
    assert_eq!(v[22].pubkey, FEE_PROGRAM_ID);
    assert_eq!(v.iter().filter(|m| m.is_signer).count(), 1);
}

#[test]
fn pumpswap_sell_is_21_accounts_without_volume_accumulators() {
    let v = pumpswap_sell_accounts(&swap_ctx()).unwrap();
    assert_eq!(v.len(), 21);
    assert_eq!(
        v[19].pubkey, PUMPSWAP_FEE_CONFIG,
        "fee_config at [19] on sell"
    );
    assert_eq!(v[20].pubkey, FEE_PROGRAM_ID);
    // No volume accumulator anywhere in the sell list (§4.2 / IDL).
    assert!(v
        .iter()
        .all(|m| m.pubkey != PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR));
}

#[test]
fn pumpswap_buy_and_sell_share_the_19_account_prefix() {
    let b = pumpswap_buy_accounts(&swap_ctx()).unwrap();
    let s = pumpswap_sell_accounts(&swap_ctx()).unwrap();
    for i in 0..19 {
        assert_eq!(b[i], s[i], "buy/sell diverge at [{i}]");
    }
}

/// Protocol fees collect in the pool's QUOTE mint: the fee-recipient token
/// account at [10] must move when the quote mint moves (reversed pool).
#[test]
fn fee_recipient_token_account_follows_quote_mint() {
    let normal = pumpswap_buy_accounts(&swap_ctx()).unwrap();
    let mut reversed = swap_ctx();
    reversed.base_mint = WSOL_MINT;
    reversed.quote_mint = pk(12); // the traded token on the quote side (~81% case)
    let rev = pumpswap_buy_accounts(&reversed).unwrap();
    assert_ne!(normal[10].pubkey, rev[10].pubkey);
}

// -- PumpSwap refusals -------------------------------------------------------

#[test]
fn negative_control_zeroed_coin_creator_refuses() {
    let mut ctx = swap_ctx();
    ctx.coin_creator = [0u8; 32];
    assert_eq!(
        pumpswap_buy_accounts(&ctx),
        Err(AccountBuildError::ZeroedInput("coin_creator"))
    );
    assert_eq!(
        pumpswap_sell_accounts(&ctx),
        Err(AccountBuildError::ZeroedInput("coin_creator"))
    );
}

#[test]
fn negative_control_zeroed_pool_refuses() {
    let mut ctx = swap_ctx();
    ctx.pool = [0u8; 32];
    assert_eq!(
        pumpswap_buy_accounts(&ctx),
        Err(AccountBuildError::ZeroedInput("pool"))
    );
}

#[test]
fn negative_control_unknown_quote_token_program_refuses() {
    let mut ctx = swap_ctx();
    ctx.quote_token_program = pk(77);
    assert_eq!(
        pumpswap_buy_accounts(&ctx),
        Err(AccountBuildError::UnknownTokenProgram)
    );
}
