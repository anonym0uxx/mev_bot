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
//! ## Operator
//! * §22 — integer only, deterministic, identical inputs → identical bytes.
//! * §18.2 — fail closed everywhere; every account decoded or derived.
//! * criterion 77/113 — the output is the byte surface the construction gate
//!   fixtures; nothing here can reach a chain without passing it.

use crate::ix::{self, BuyExactSolInParams, BuyParams, SellParams};
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

/// `buy_exact_sol_in` data blob with the explicit `track_volume = false` byte (25 bytes), the same
/// OptionBool convention as [`build_buy_data_with_volume_flag`]. The pinned IDL (pump-sdk 2.0.0) has
/// exactly three args; the newer public IDL appends an optional `partial_fill` (mayhem coins only)
/// that "may be left off the end", so it is not emitted.
pub fn build_buy_exact_sol_in_data_with_volume_flag(params: BuyExactSolInParams) -> Vec<u8> {
    let mut data = ix::build_buy_exact_sol_in_ix(params);
    data.push(BUY_TRACK_VOLUME_BYTE); // OptionBool(false): decline volume tracking, fail-closed.
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
    build_curve_buy_message(ctx, build_buy_data_with_volume_flag(params), env)
}

/// Build the unsigned pump.fun **`buy_exact_sol_in`** message: spend exactly
/// `params.spendable_sol_in` (fees inside) and accept no fewer than `params.min_tokens_out`.
///
/// Same instruction sequence and the same account list as [`build_pump_buy_message`]: the IDL gives
/// both instructions the identical 16 named accounts, and mainnet shows both at 18 with the
/// remaining accounts (`tools/layout_probe/layout_matrix.json`: `buy_exact_sol_in|18|2022`, 63 txs).
/// The layout gate is therefore the same `Side::Buy` key. Only the data differs.
pub fn build_pump_buy_exact_sol_in_message(
    ctx: &PumpCurveCtx,
    params: BuyExactSolInParams,
    env: &BuildEnv<'_>,
) -> Result<CompiledMessage, TxBuildError> {
    build_curve_buy_message(
        ctx,
        build_buy_exact_sol_in_data_with_volume_flag(params),
        env,
    )
}

fn build_curve_buy_message(
    ctx: &PumpCurveCtx,
    data: Vec<u8>,
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
        data,
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

// ---------------------------------------------------------------------------
// SCOPE_TXCOST_R9 T2/T3: accounts a BUY creates, deposits, the cash commitment M and the
// failure-cost structure. Pure functions over OBSERVED state; nothing here reads a chain.
// ---------------------------------------------------------------------------

/// The `track_volume` byte every curve buy this module builds carries (`0x00`, OptionBool false).
///
/// Pinned as a constant so a test can prove it is never flipped as a side effect of a deposit or
/// refusal decision. With the `user_volume_accumulator` ABSENT this byte's on-chain effect is not
/// evidenced for the current deployment (CHAIN_READONLY_EVIDENCE §3.2 item 3), so
/// [`plan_buy_deposits`] refuses that case by name instead of enabling the flag.
pub const BUY_TRACK_VOLUME_BYTE: u8 = 0x00;

/// The pump `UserVolumeAccumulator` account size observed on every sampled account under the
/// current deployment (137 B, owner pump.fun; CHAIN_READONLY_EVIDENCE §2, §3.4).
pub const UVA_ACCOUNT_SIZE: u64 = 137;

/// The planned base fee per required signature.
///
/// This is the ONE accessor the plim slice reads the planned per-signature fee through. The
/// protocol crate cannot depend on `pump-quant-app`, so the value is pinned EQUAL to
/// `pump_quant_app::txcost::TXCOST_V1_20261010.lamports_per_signature` by the integration test
/// `pump-quant-app/tests/txcost_plim_parity.rs`; change both in one versioned step.
#[must_use]
pub const fn planned_lamports_per_signature() -> u64 {
    5_000
}

/// The token-account size the ATA program allocates for `token_program`, per its
/// `GetAccountDataSize` semantics: SPL Token 165 B; Token-2022 with the ImmutableOwner extension
/// the ATA program initialises, 170 B. Any other program is not a supported layout (`None`), and
/// the caller must refuse (`deposit_unknown`), never assume a size.
#[must_use]
pub fn ata_account_size(token_program: &[u8; 32]) -> Option<u64> {
    if token_program == &crate::venue_accounts::TOKEN_PROGRAM_ID {
        Some(165)
    } else if token_program == &crate::venue_accounts::TOKEN_2022_PROGRAM_ID {
        Some(170)
    } else {
        None
    }
}

/// Rent-exempt minimum balances read at ONE slot (`getMinimumBalanceForRentExemption` for each
/// size). There is no built-in table: a size the snapshot does not carry is `deposit_unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RentSnapshot {
    /// Slot the minimums were read at.
    pub slot: u64,
    /// `(data size in bytes, rent-exempt lamports)`.
    pub minimums: Vec<(u64, u64)>,
}

impl RentSnapshot {
    /// The rent-exempt minimum for `size`, if read.
    #[must_use]
    pub fn minimum(&self, size: u64) -> Option<u64> {
        self.minimums
            .iter()
            .find(|(s, _)| *s == size)
            .map(|(_, l)| *l)
    }
}

/// One account's observed state at the observation slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    /// The account does not exist (null `getAccountInfo`).
    Absent,
    /// The account exists.
    Present {
        /// Owning program.
        owner: [u8; 32],
        /// Data length in bytes.
        data_len: u64,
        /// Actual lamport balance (the recoverable amount if it is closed).
        lamports: u64,
        /// For a token account: the mint at data[0..32]. `None` for non-token accounts.
        token_mint: Option<[u8; 32]>,
    },
}

/// The accounts a curve BUY may create, observed at one slot, plus rent at that slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuyAccountObservation {
    /// Slot the account states were observed at.
    pub slot: u64,
    /// The user's associated token account for the mint.
    pub user_ata: AccountState,
    /// The user's pump `user_volume_accumulator` PDA.
    pub user_volume_accumulator: AccountState,
    /// Rent minimums read at `slot`.
    pub rent: RentSnapshot,
}

/// The deposits a BUY posts, by account. A deposit is cash reserved, NOT an expense: it is
/// recoverable on close at the account's actual balance ([`refund_on_close`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositPlan {
    /// Rent deposit for the user ATA: rent(size) when absent (the message's `CreateIdempotent`
    /// creates it), 0 when it already exists and is valid.
    pub ata_lamports: u64,
    /// Rent deposit for the UVA. Always 0 in a plan that is not refused: the only creation case
    /// (absent) is refused until evidenced.
    pub uva_lamports: u64,
}

impl DepositPlan {
    /// Total deposit `D`.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.ata_lamports.checked_add(self.uva_lamports)
    }
}

/// Why a BUY's account/deposit plan was refused. Each has one stable name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositRefusal {
    /// The token program has no supported ATA size, or the rent snapshot lacks the size, or the
    /// rent was read at a different slot than the accounts.
    DepositUnknown,
    /// The user ATA exists but has the wrong owner program, size or mint.
    AtaIncompatible,
    /// The UVA exists but has the wrong owner or size.
    UvaIncompatible,
    /// `track_volume = 0x00` with the UVA absent: the behaviour is not evidenced for the current
    /// deployment (create, no-op or fail). Refused until a test establishes it.
    UvaAbsentTrackVolumeOffUnverified,
}

impl DepositRefusal {
    /// Stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DepositUnknown => "deposit_unknown",
            Self::AtaIncompatible => "ata_incompatible",
            Self::UvaIncompatible => "uva_incompatible",
            Self::UvaAbsentTrackVolumeOffUnverified => "uva_absent_track_volume_off_unverified",
        }
    }
}

/// The deposits a curve BUY built by [`build_pump_buy_exact_sol_in_message`] posts, from the
/// accounts observed at `obs.slot` (SCOPE_TXCOST_R9 T2).
///
/// * ATA absent -> rent(actual size for the token program) at the observation slot. The message
///   is shown to create it: `build_curve_buy_message` emits `CreateIdempotent` unconditionally.
/// * ATA present and valid (owner = token program, size = the program's ATA size, mint = ctx.mint)
///   -> 0 deposit (transaction costs still apply; they are not in this plan).
/// * ATA present but incompatible -> `ata_incompatible`.
/// * UVA absent (with [`BUY_TRACK_VOLUME_BYTE`] = 0x00) -> `uva_absent_track_volume_off_unverified`.
/// * UVA present and valid (owner pump, 137 B) -> 0; otherwise `uva_incompatible`.
///
/// No universal constant is a deposit source here: the only lamport numbers come from `obs.rent`.
pub fn plan_buy_deposits(
    ctx: &PumpCurveCtx,
    obs: &BuyAccountObservation,
) -> Result<DepositPlan, DepositRefusal> {
    if obs.rent.slot != obs.slot {
        return Err(DepositRefusal::DepositUnknown);
    }
    let ata_size = ata_account_size(&ctx.token_program).ok_or(DepositRefusal::DepositUnknown)?;
    let ata_lamports = match obs.user_ata {
        AccountState::Absent => obs
            .rent
            .minimum(ata_size)
            .ok_or(DepositRefusal::DepositUnknown)?,
        AccountState::Present {
            owner,
            data_len,
            token_mint,
            ..
        } => {
            if owner != ctx.token_program || data_len != ata_size || token_mint != Some(ctx.mint) {
                return Err(DepositRefusal::AtaIncompatible);
            }
            0
        }
    };
    let uva_lamports = match obs.user_volume_accumulator {
        AccountState::Absent => {
            // Only 0x00 is ever built; with 0x00 + absent the effect is unverified. Do NOT flip the
            // byte to make the case "known" and do NOT reserve rent(137) as proof of success.
            debug_assert_eq!(BUY_TRACK_VOLUME_BYTE, 0x00);
            return Err(DepositRefusal::UvaAbsentTrackVolumeOffUnverified);
        }
        AccountState::Present {
            owner, data_len, ..
        } => {
            if owner != crate::venue_accounts::PUMP_PROGRAM_ID || data_len != UVA_ACCOUNT_SIZE {
                return Err(DepositRefusal::UvaIncompatible);
            }
            0
        }
    };
    Ok(DepositPlan {
        ata_lamports,
        uva_lamports,
    })
}

/// What closing an account returns: its ACTUAL lamport balance at close (old ATAs hold 2,039,280,
/// new ones today's minimum). An absent account returns nothing.
#[must_use]
pub fn refund_on_close(state: &AccountState) -> u64 {
    match state {
        AccountState::Absent => 0,
        AccountState::Present { lamports, .. } => *lamports,
    }
}

/// One instruction decoded back out of compiled message bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedIx {
    /// Program id.
    pub program_id: [u8; 32],
    /// Account keys in instruction order.
    pub accounts: Vec<[u8; 32]>,
    /// Raw data.
    pub data: Vec<u8>,
}

/// The parts of a compiled legacy message the fee and structure checks need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMessage {
    /// Header: required signatures.
    pub num_required_signatures: u8,
    /// Instructions in order.
    pub instructions: Vec<DecodedIx>,
}

fn read_shortvec(b: &[u8], at: &mut usize) -> Option<usize> {
    let mut n = 0usize;
    for shift in [0u32, 7, 14] {
        let byte = *b.get(*at)?;
        *at += 1;
        n |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

/// Decode compiled legacy message bytes (the exact bytes the signer signs). `None` if malformed.
#[must_use]
pub fn decode_message(b: &[u8]) -> Option<DecodedMessage> {
    let num_required_signatures = *b.first()?;
    let mut at = 3usize;
    let nkeys = read_shortvec(b, &mut at)?;
    let mut keys = Vec::with_capacity(nkeys);
    for _ in 0..nkeys {
        keys.push(<[u8; 32]>::try_from(b.get(at..at + 32)?).ok()?);
        at += 32;
    }
    at += 32; // recent blockhash
    let nix = read_shortvec(b, &mut at)?;
    let mut instructions = Vec::with_capacity(nix);
    for _ in 0..nix {
        let program_id = *keys.get(usize::from(*b.get(at)?))?;
        at += 1;
        let na = read_shortvec(b, &mut at)?;
        let mut accounts = Vec::with_capacity(na);
        for _ in 0..na {
            accounts.push(*keys.get(usize::from(*b.get(at)?))?);
            at += 1;
        }
        let nd = read_shortvec(b, &mut at)?;
        let data = b.get(at..at + nd)?.to_vec();
        at += nd;
        instructions.push(DecodedIx {
            program_id,
            accounts,
            data,
        });
    }
    if at != b.len() {
        return None;
    }
    Some(DecodedMessage {
        num_required_signatures,
        instructions,
    })
}

/// Planned fee components read FROM THE COMPILED MESSAGE (never from CU consumed or a p50).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageCosts {
    /// `lamports_per_signature * num_required_signatures` (in `meta.fee`).
    pub base_fee: u64,
    /// `ceil(cu_limit * cu_price_micro / 1e6)` on the REQUESTED limit (in `meta.fee`).
    pub priority_fee: u64,
    /// Sum of system transfers from the payer to a non-instruction-account destination: the tip.
    /// A system transfer, NOT in `meta.fee`.
    pub tip: u64,
    /// Whether the tip transfer is an instruction of the SAME message as the pump instruction
    /// (so it reverts with it on failure).
    pub tip_in_same_atomic_message: bool,
}

impl MessageCosts {
    /// `meta.fee` predicted for this message: base + priority.
    #[must_use]
    pub fn network_fee(&self) -> Option<u64> {
        self.base_fee.checked_add(self.priority_fee)
    }
}

/// Read the planned costs from a compiled message (SCOPE_TXCOST_R9 T3, P3 T-iii-1).
///
/// The tip is the payer's System `Transfer`(s) in the message. `None` when the message is
/// malformed, carries more than one CU limit/price, or the arithmetic overflows.
#[must_use]
pub fn message_costs(msg: &CompiledMessage, lamports_per_signature: u64) -> Option<MessageCosts> {
    let d = decode_message(&msg.bytes)?;
    let (mut limit, mut price) = (None::<u32>, None::<u64>);
    let (mut tip, mut has_tip, mut has_pump) = (0u64, false, false);
    for ix in &d.instructions {
        if ix.program_id == crate::venue_accounts::COMPUTE_BUDGET_PROGRAM_ID {
            match ix.data.first() {
                Some(2) if ix.data.len() == 5 && limit.is_none() => {
                    limit = Some(u32::from_le_bytes(ix.data[1..5].try_into().ok()?));
                }
                Some(3) if ix.data.len() == 9 && price.is_none() => {
                    price = Some(u64::from_le_bytes(ix.data[1..9].try_into().ok()?));
                }
                _ => return None,
            }
        } else if ix.program_id == crate::venue_accounts::SYSTEM_PROGRAM_ID
            && ix.data.len() == 12
            && ix.data[..4] == 2u32.to_le_bytes()
        {
            tip = tip.checked_add(u64::from_le_bytes(ix.data[4..12].try_into().ok()?))?;
            has_tip = true;
        } else if ix.program_id == crate::venue_accounts::PUMP_PROGRAM_ID {
            has_pump = true;
        }
    }
    let base_fee = lamports_per_signature.checked_mul(u64::from(d.num_required_signatures))?;
    let priority_fee = u64::try_from(
        (u128::from(limit.unwrap_or(0)) * u128::from(price.unwrap_or(0))).div_ceil(1_000_000),
    )
    .ok()?;
    Some(MessageCosts {
        base_fee,
        priority_fee,
        tip,
        tip_in_same_atomic_message: has_tip && has_pump,
    })
}

/// `cash_commitment_overflow`: M is not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashCommitmentOverflow;

impl CashCommitmentOverflow {
    /// Stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        "cash_commitment_overflow"
    }
}

/// The cash a BUY reserves before submit: `M = C + base + priority + tip + D`, checked.
///
/// `D` is reserved, not an expense. The price bound (present or absent) does not enter M: an
/// absent MAX PRICE never removes M.
pub fn cash_commitment(
    clip: u64,
    costs: &MessageCosts,
    deposits: &DepositPlan,
) -> Result<u64, CashCommitmentOverflow> {
    let e = CashCommitmentOverflow;
    clip.checked_add(costs.base_fee)
        .and_then(|v| v.checked_add(costs.priority_fee))
        .and_then(|v| v.checked_add(costs.tip))
        .and_then(|v| v.checked_add(deposits.total()?))
        .ok_or(e)
}

/// What happened to a submitted-or-not transaction, as far as the chain is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxChainOutcome {
    /// Processed and succeeded.
    Landed,
    /// Processed but an instruction failed: the fee payer pays `meta.fee`, every instruction
    /// effect (spend, deposits, the in-message tip transfer) reverts.
    ProcessedFailed,
    /// Never handed to any endpoint: zero chain cost, final.
    Unsubmitted,
    /// Submitted, not (yet) seen: zero chain cost so far, but UNCERTAIN until the blockhash
    /// expires or reconciliation settles it.
    NeverLanded,
}

/// The chain debit predicted for a NON-landed outcome, and whether it is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureDebit {
    /// Lamports the wallet loses.
    pub lamports: u64,
    /// `false` while the outcome can still change (never-landed before expiry).
    pub settled: bool,
}

/// Why a failure-cost prediction is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCostRefusal {
    /// A landed transaction's cost is its actual evidence (`meta.fee` + transfers), not a prediction.
    LandedUsesActuals,
    /// The tip is not an instruction of the same atomic message (a separate tip transaction or a
    /// bundle). The tip-rollback prediction is NOT generalised to other structures.
    UnsupportedSubmissionStructure,
    /// Arithmetic overflow.
    Overflow,
}

/// Predict the chain debit of a non-landed outcome for THIS message structure (SCOPE_TXCOST_R9 T3).
///
/// For the builder's structure (tip transfer inside the same message as the pump instruction),
/// processed-failed costs exactly `meta.fee` = base + priority (e.g. 5,000 + 600 = 5,600 under the
/// pq_daemon envelope). That number is a PREDICTION from Solana atomicity, not an observed failed
/// transaction. Unsubmitted and never-landed cost 0; never-landed stays unsettled.
pub fn predicted_failure_debit(
    outcome: TxChainOutcome,
    costs: &MessageCosts,
) -> Result<FailureDebit, FailureCostRefusal> {
    match outcome {
        TxChainOutcome::Landed => Err(FailureCostRefusal::LandedUsesActuals),
        TxChainOutcome::Unsubmitted => Ok(FailureDebit {
            lamports: 0,
            settled: true,
        }),
        TxChainOutcome::NeverLanded => Ok(FailureDebit {
            lamports: 0,
            settled: false,
        }),
        TxChainOutcome::ProcessedFailed => {
            if costs.tip > 0 && !costs.tip_in_same_atomic_message {
                return Err(FailureCostRefusal::UnsupportedSubmissionStructure);
            }
            Ok(FailureDebit {
                lamports: costs.network_fee().ok_or(FailureCostRefusal::Overflow)?,
                settled: true,
            })
        }
    }
}
