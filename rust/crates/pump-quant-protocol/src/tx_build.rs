//! End-to-end unsigned-message assembly for pump.fun bonding-curve trades:
//! decoded state in, signable bytes out.
//!
//! ## Responsibility
//! Compose [`crate::venue_accounts`] (the account lists), [`crate::ix`] (the
//! data blobs) and [`crate::message`] (the wire compiler) into the one call
//! the live path needs: *given decoded venue state and a trade, produce the
//! exact bytes for the signer*. The signer (`stream-capture-rs::signer`) signs
//! them; the sender (`stream-capture-rs::sender`) submits the assembled
//! transaction; this module is the junction between them.
//!
//! ## Instruction sequences (fixed, deterministic)
//! * **buy**: `SetComputeUnitLimit`, `SetComputeUnitPrice`,
//!   `CreateIdempotent(user ATA)`, pump `buy`, then the tip transfer when a
//!   [`TipPlan`] is supplied. `CreateIdempotent` is unconditional so a
//!   first-ever buy and a repeat buy compile to identical shapes (§22
//!   determinism over conditional assembly).
//! * **sell**: `SetComputeUnitLimit`, `SetComputeUnitPrice`, pump `sell`,
//!   optionally `CloseAccount(user ATA)` on a full exit (reclaims the ATA
//!   rent the NET-SOL audit priced at 203 bps on a 0.1 SOL position), then
//!   the tip transfer when supplied.
//!
//! ## The `track_volume` byte
//! The current IDL (2026-08-02) adds `track_volume: OptionBool` to the
//! bonding-curve `buy` (it was previously AMM-only). This module appends an
//! explicit `0x00` (false) — 25-byte data — deliberately declining the volume
//! credit so an uninitialised `user_volume_accumulator` cannot produce
//! `AccountNotInitialized`, the exact defense legacy shipped on the AMM side
//! (`VENUE_TX_LAYOUTS.md` §4.2). [`crate::ix::build_buy_ix`] stays
//! byte-identical (24 bytes) for its existing decoder/fixture consumers.
//! If the volume credit is ever wanted, the correct move is
//! `init_user_volume_accumulator` first — not flipping this byte (§4.2).
//!
//! ## What this module refuses to do
//! * Substitute any placeholder account ([`crate::venue_accounts`] refuses
//!   first, §18.2).
//! * Accept a zero-lamport tip: a [`TipPlan`] models a real Sender tip, and a
//!   zero tip is below every documented tier floor — it would pay a signature
//!   fee to guarantee a rejection. `ex_sender_route::decide()` answering NO is
//!   expressed as `tip: None`, never as `tip_lamports: 0`.
//! * Sign or submit. No key material and no I/O exist in this crate.
//!
//! ## Constitution
//! * §22 — integer only, deterministic, identical inputs → identical bytes.
//! * §18.2 — fail closed everywhere; every account decoded or derived.
//! * criterion 77/113 — the output is the byte surface the construction gate
//!   fixtures; nothing here can reach a chain without passing it.

use crate::ix::{self, BuyParams, SellParams};
use crate::layout::{LayoutError, LayoutKey, LayoutRegistry, Side, Variant, Venue};
use crate::message::{self, compile_message, CompiledMessage, Instruction, MessageError};
use crate::venue_accounts::{
    pump_buy_accounts_with_tail, pump_sell_accounts_with_tail, AccountBuildError, FeeTail,
    PumpCurveCtx,
};

/// A Sender tip: destination (one of the ten committed tip accounts, selected
/// by deterministic seed upstream) and a non-zero lamport amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TipPlan {
    /// Tip destination account.
    pub to: [u8; 32],
    /// Tip amount in lamports; must be non-zero.
    pub lamports: u64,
}

/// Compute-budget envelope every live transaction carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputePlan {
    /// `SetComputeUnitLimit` units.
    pub unit_limit: u32,
    /// `SetComputeUnitPrice` in micro-lamports per CU.
    pub unit_price_micro_lamports: u64,
}

/// Why an end-to-end build was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxBuildError {
    /// Account-list construction refused (§18.2 fail-closed).
    Accounts(AccountBuildError),
    /// Message compilation refused (bounds, size).
    Message(MessageError),
    /// A [`TipPlan`] carried zero lamports.
    ZeroTip,
    /// The tip destination was all-zero.
    ZeroTipAccount,
    /// The recent blockhash was all-zero - a default value, not a decoded one.
    ZeroBlockhash,
    /// The layout this build needs has no verified on-chain fixture, or the
    /// builder disagrees with the fixture it has. Propagated from
    /// [`crate::layout::LayoutRegistry`].
    ///
    /// This is the control added after the 2026-08-02 falsification: the
    /// builder shipped a 17-account bonding-curve buy that the chain answered
    /// with 18, and nothing in the type system objected. Now it does.
    Layout(LayoutError),
}

impl From<LayoutError> for TxBuildError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// Derive the [`LayoutKey`] a bonding-curve build must have verified.
///
/// `reversed_pool` is always false here - it is a PumpSwap-only dimension.
fn pump_layout_key(ctx: &PumpCurveCtx, side: Side) -> LayoutKey {
    LayoutKey {
        venue: Venue::PumpFun,
        side,
        variant: Variant {
            cashback: ctx.is_cashback_coin,
            token_2022: ctx.token_program == crate::venue_accounts::TOKEN_2022_PROGRAM_ID,
            non_sol_quote: ctx.quote_mint != crate::venue_accounts::WSOL_MINT,
            reversed_pool: false,
        },
    }
}

impl From<AccountBuildError> for TxBuildError {
    fn from(e: AccountBuildError) -> Self {
        Self::Accounts(e)
    }
}
impl From<MessageError> for TxBuildError {
    fn from(e: MessageError) -> Self {
        Self::Message(e)
    }
}

fn validate_common(blockhash: &[u8; 32], tip: &Option<TipPlan>) -> Result<(), TxBuildError> {
    if blockhash == &[0u8; 32] {
        return Err(TxBuildError::ZeroBlockhash);
    }
    if let Some(t) = tip {
        if t.lamports == 0 {
            return Err(TxBuildError::ZeroTip);
        }
        if t.to == [0u8; 32] {
            return Err(TxBuildError::ZeroTipAccount);
        }
    }
    Ok(())
}

/// `buy` data blob with the explicit `track_volume = false` byte (25 bytes).
/// See the module doc; [`ix::build_buy_ix`] (24 bytes) is unchanged.
pub fn build_buy_data_with_volume_flag(params: BuyParams) -> Vec<u8> {
    let mut data = ix::build_buy_ix(params);
    data.push(0u8); // OptionBool(false): decline volume tracking, fail-closed.
    data
}

/// `sell` data blob — 24 bytes (discriminator + 2 u64 args).
///
/// Rev-29 (2026-08-20): The sell IDL has only 2 args (`amount`, `min_sol_output`)
/// and NO `track_volume` parameter. The previous code appended a spurious 25th
/// byte (OptionBool false), which is buy-only. Sell data is exactly 24 bytes.
pub fn build_sell_data_with_volume_flag(params: SellParams) -> Vec<u8> {
    ix::build_sell_ix(params)
}

/// Everything about *how* a transaction is built, as opposed to *what* it
/// trades. Grouping these is not cosmetic: the parameter list grew past seven
/// when the layout gate was added, and a long positional argument list is how
/// a `recent_blockhash` ends up where a `tip` was meant to go.
pub struct BuildEnv<'a> {
    /// Compute-budget envelope.
    pub compute: ComputePlan,
    /// Sender tip, or `None` when `ex_sender_route` answered no.
    pub tip: Option<TipPlan>,
    /// Decoded recent blockhash. All-zero is refused.
    pub recent_blockhash: [u8; 32],
    /// The verified-layout registry. An empty one builds nothing.
    pub registry: &'a LayoutRegistry,
    /// Which trailing fee-program account to emit (2026-08-02 finding).
    pub fee_tail: FeeTail,
}

/// Build the unsigned pump.fun **buy** message.
///
/// The caller signs `result.bytes` with the wallet signer and assembles the
/// wire transaction via [`message::assemble_transaction`].
pub fn build_pump_buy_message(
    ctx: &PumpCurveCtx,
    params: BuyParams,
    env: &BuildEnv<'_>,
) -> Result<CompiledMessage, TxBuildError> {
    let (compute, tip, recent_blockhash) = (env.compute, env.tip, &env.recent_blockhash);
    validate_common(recent_blockhash, &tip)?;
    let accounts = pump_buy_accounts_with_tail(ctx, env.fee_tail)?;
    // THE GATE. An unverified layout cannot be built, so an account list that
    // has never been diffed against a real transaction cannot reach a signer.
    env.registry
        .require(&pump_layout_key(ctx, Side::Buy), accounts.len())?;
    // associated_user is index [5] of the §4.1 buy list.
    let associated_user = accounts[5].pubkey;

    let mut ixs: Vec<Instruction> = Vec::with_capacity(5);
    ixs.push(message::set_compute_unit_limit(compute.unit_limit));
    ixs.push(message::set_compute_unit_price(
        compute.unit_price_micro_lamports,
    ));
    ixs.push(message::create_ata_idempotent(
        &ctx.user,
        &associated_user,
        &ctx.user,
        &ctx.mint,
        &ctx.token_program,
    ));
    ixs.push(Instruction {
        program_id: crate::venue_accounts::PUMP_PROGRAM_ID,
        accounts,
        data: build_buy_data_with_volume_flag(params),
    });
    if let Some(t) = tip {
        ixs.push(message::system_transfer(&ctx.user, &t.to, t.lamports));
    }
    Ok(compile_message(&ctx.user, recent_blockhash, &ixs)?)
}

/// Build the unsigned pump.fun **sell** message.
///
/// `close_token_account` reclaims the user ATA's rent on a full exit; a
/// partial ladder rung passes `false`. The close refund goes to the seller.
pub fn build_pump_sell_message(
    ctx: &PumpCurveCtx,
    params: SellParams,
    env: &BuildEnv<'_>,
    close_token_account: bool,
) -> Result<CompiledMessage, TxBuildError> {
    let (compute, tip, recent_blockhash) = (env.compute, env.tip, &env.recent_blockhash);
    validate_common(recent_blockhash, &tip)?;
    let accounts = pump_sell_accounts_with_tail(ctx, env.fee_tail)?;
    env.registry
        .require(&pump_layout_key(ctx, Side::Sell), accounts.len())?;
    // associated_user is index [5] of the §4.1 sell list.
    let associated_user = accounts[5].pubkey;

    let mut ixs: Vec<Instruction> = Vec::with_capacity(5);
    ixs.push(message::set_compute_unit_limit(compute.unit_limit));
    ixs.push(message::set_compute_unit_price(
        compute.unit_price_micro_lamports,
    ));
    ixs.push(Instruction {
        program_id: crate::venue_accounts::PUMP_PROGRAM_ID,
        accounts,
        data: build_sell_data_with_volume_flag(params),
    });
    if close_token_account {
        ixs.push(message::spl_close_account(
            &associated_user,
            &ctx.user,
            &ctx.user,
            &ctx.token_program,
        ));
    }
    if let Some(t) = tip {
        ixs.push(message::system_transfer(&ctx.user, &t.to, t.lamports));
    }
    Ok(compile_message(&ctx.user, recent_blockhash, &ixs)?)
}
