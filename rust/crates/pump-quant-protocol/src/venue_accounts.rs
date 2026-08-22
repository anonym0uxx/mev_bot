//! Venue account-list builders: the real `AccountMeta` lists behind
//! `docs/VENUE_TX_LAYOUTS.md` §4.
//!
//! ## Responsibility
//! Produce the ordered, flagged account list for pump.fun bonding-curve
//! `buy`/`sell` (§4.1: 17 / 15–16 accounts) and PumpSwap AMM `buy`/`sell`
//! (§4.2: 23 / 21 accounts before the remaining-accounts tail), deriving every
//! derivable address via [`crate::pda`] and **refusing to build** when any
//! non-derivable input is absent (§18.2: `Pubkey::default()` is the System
//! Program and every venue validates it as something else — refusal, never a
//! placeholder).
//!
//! ## Provenance and status
//! Three independent corroborations back these tables:
//! 1. `VENUE_TX_LAYOUTS.md` §4 (legacy-derived, PDAs re-derived 2026-07-29);
//! 2. the official `pump-fun/pump-public-docs` IDLs (`pump.json`,
//!    `pump_amm.json`, read 2026-08-02) — account order and flags match §4
//!    exactly for the named lists;
//! 3. every derivable constant below re-derives by [`crate::pda`] at test time.
//!
//! Two findings from the 2026-08-02 IDL read, recorded here because they moved:
//! * **`fee_config` is venue-specific.** The seeds are
//!   `["fee_config", <consuming program id>]` under the fee program, so the
//!   bonding curve's is `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt`
//!   (bump 253) and PumpSwap's is
//!   `5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx` (bump 255). The latter was
//!   carried in `VENUE_TX_LAYOUTS.md` §2 as "legacy, **unverified**" — it now
//!   derives, which both verifies it and pins it to PumpSwap *only*. Using it
//!   in a bonding-curve instruction is a cross-venue constant mixup this
//!   module makes unrepresentable by deriving per-venue.
//! * **PumpSwap `sell` takes no `track_volume` arg** (IDL: `base_amount_in`,
//!   `min_quote_amount_out` and nothing else), settling §4.2's open question:
//!   legacy's buy-only `0x00` append was correct.
//!
//! > **STATUS: UNVERIFIED ON-CHAIN.** Per §4.1's gate, one real successful
//! > `buy` and one `sell` must be decoded off the chain and diffed against
//! > these lists before the first live entry. IDL corroboration narrows the
//! > risk; it does not discharge the gate.
//!
//! ## Constitution
//! * §18.2 — fail closed on every unknown; account identity from decoded
//!   state, never assumption.
//! * §22 — integer only, deterministic.
//! * §102 — every constant carries its base58 citation.

// The account-list builders below use explicit `Vec::with_capacity` + per-line
// `v.push(...) // [n]` so every account carries its ordinal citation inline —
// a re-ordering is a construction defect (criterion 77a) and the annotation is
// the audit surface. This is deliberately kept over a `vec!` literal, so the
// `vec_init_then_push` lint is allowed module-wide with that rationale.
#![allow(clippy::vec_init_then_push)]

use crate::pda::{self, PdaError};

// ---------------------------------------------------------------------------
// Program / fixed addresses (§102: base58 citation above each constant).
// Derivable PDAs among them are re-derived in `tests/venue_accounts.rs`.
// ---------------------------------------------------------------------------

/// `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` — pump.fun bonding curve.
pub const PUMP_PROGRAM_ID: [u8; 32] = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151, 245,
    210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176,
];

/// `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` — PumpSwap AMM.
pub const PUMPSWAP_PROGRAM_ID: [u8; 32] = [
    12, 20, 222, 252, 130, 94, 198, 118, 148, 37, 8, 24, 187, 101, 64, 101, 244, 41, 141, 49, 86,
    213, 113, 180, 212, 248, 9, 12, 24, 233, 168, 99,
];

/// `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` — pump fee program (both venues).
pub const FEE_PROGRAM_ID: [u8; 32] = [
    12, 53, 255, 169, 5, 90, 142, 86, 141, 168, 247, 188, 7, 86, 21, 39, 76, 241, 201, 44, 164, 31,
    64, 0, 156, 81, 106, 164, 20, 194, 124, 112,
];

/// `11111111111111111111111111111111` — System Program.
pub const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` — SPL Token.
pub const TOKEN_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237,
    95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];

/// `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` — Token-2022.
pub const TOKEN_2022_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77,
    131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252,
];

/// `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL` — Associated Token program.
pub const ATA_PROGRAM_ID: [u8; 32] = [
    140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131, 11, 90, 19, 153, 218,
    255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89,
];

/// `ComputeBudget111111111111111111111111111111` — Compute Budget program.
pub const COMPUTE_BUDGET_PROGRAM_ID: [u8; 32] = [
    3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 187,
    197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0,
];

/// `So11111111111111111111111111111111111111112` — wrapped SOL mint.
pub const WSOL_MINT: [u8; 32] = [
    6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26,
    235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1,
];

/// `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf` — pump.fun `["global"]`, bump 255.
pub const PUMP_GLOBAL: [u8; 32] = [
    58, 134, 94, 105, 238, 15, 84, 128, 202, 188, 246, 99, 87, 228, 220, 47, 24, 213, 141, 69, 193,
    234, 116, 137, 251, 55, 35, 217, 121, 60, 114, 166,
];

/// `Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1` — pump.fun `["__event_authority"]`, bump 255.
pub const PUMP_EVENT_AUTHORITY: [u8; 32] = [
    172, 241, 54, 235, 1, 252, 28, 78, 136, 61, 35, 200, 181, 132, 74, 181, 154, 55, 246, 106, 221,
    87, 197, 233, 172, 59, 83, 224, 89, 211, 92, 100,
];

/// `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y` — pump.fun
/// `["global_volume_accumulator"]`, bump 255.
pub const PUMP_GLOBAL_VOLUME_ACCUMULATOR: [u8; 32] = [
    250, 9, 17, 165, 72, 99, 65, 45, 99, 31, 78, 7, 135, 3, 41, 108, 3, 95, 13, 19, 51, 160, 217,
    200, 131, 141, 115, 183, 16, 254, 110, 45,
];

/// `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt` — pump.fun `fee_config`:
/// `["fee_config", PUMP_PROGRAM_ID]` under [`FEE_PROGRAM_ID`], **bump 253**.
/// Venue-specific — see the module doc; NOT interchangeable with PumpSwap's.
pub const PUMP_FEE_CONFIG: [u8; 32] = [
    111, 154, 180, 164, 241, 149, 141, 192, 169, 201, 76, 63, 183, 44, 7, 153, 88, 67, 237, 164,
    133, 227, 162, 79, 16, 198, 147, 153, 248, 25, 148, 15,
];

/// `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw` — PumpSwap
/// `["global_config"]`, bump 255.
pub const PUMPSWAP_GLOBAL_CONFIG: [u8; 32] = [
    137, 11, 166, 68, 254, 31, 85, 170, 25, 241, 28, 210, 210, 236, 20, 211, 35, 59, 110, 10, 75,
    234, 238, 247, 43, 105, 133, 142, 33, 225, 112, 214,
];

/// `GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR` — PumpSwap
/// `["__event_authority"]`, bump 255.
pub const PUMPSWAP_EVENT_AUTHORITY: [u8; 32] = [
    229, 74, 112, 149, 40, 131, 159, 97, 192, 185, 184, 96, 121, 137, 28, 19, 146, 22, 228, 122,
    113, 182, 47, 183, 59, 236, 114, 22, 148, 88, 116, 94,
];

/// `C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw` — PumpSwap
/// `["global_volume_accumulator"]`, bump 255.
pub const PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR: [u8; 32] = [
    163, 215, 187, 18, 126, 88, 173, 193, 44, 166, 143, 131, 67, 126, 194, 225, 195, 249, 130, 13,
    233, 62, 88, 249, 23, 138, 41, 24, 221, 170, 247, 180,
];

/// Seed prefix of the fee program's per-mint `SharingConfig` account:
/// `["sharing-config", mint]` under [`FEE_PROGRAM_ID`], writable.
///
/// # Provenance and status: HYPOTHESIS, NOT VERIFIED
/// The 2026-08-02 on-chain check found one **extra writable account owned by
/// the fee program**, positioned after `bonding_curve_v2`, in 100% of sampled
/// bonding-curve buys and sells. `pump_fees.json` defines exactly one writable
/// per-mint fee-program account that fits: `SharingConfig`, seeds
/// `["sharing-config", mint]` (see `create_fee_sharing_config`,
/// `update_fee_shares`, `create_donation_fee_pda`).
///
/// This is the leading candidate and it is NOT confirmed. Two other
/// fee-program accounts could occupy that slot, and they are distinguishable
/// by one observation:
///
/// * `fee_program_global` — seeds `["fee-program-global"]`, **no mint seed**,
///   so it is the SAME address in every transaction:
///   `CHqnuTkj6sXDFknM652aEFPECZh9qVsBXWkhPohmV9dA`.
/// * `SharingConfig` / `DonationFeePda` — per-mint, so the address VARIES with
///   the traded mint.
///
/// **The discriminating test:** compare the extra account across two
/// transactions on DIFFERENT mints. Same address across both ⇒
/// `fee_program_global`. Different ⇒ per-mint, and then diff against this
/// derivation to settle which. Until that observation exists,
/// [`crate::layout::LayoutRegistry`] refuses to build either layout.
pub const SHARING_CONFIG_SEED: &[u8] = b"sharing-config";

/// `CHqnuTkj6sXDFknM652aEFPECZh9qVsBXWkhPohmV9dA` — the fee program's
/// `["fee-program-global"]` PDA, bump 254. Constant across all mints; see
/// [`SHARING_CONFIG_SEED`] for why that property is the discriminating test.
pub const FEE_PROGRAM_GLOBAL: [u8; 32] = [
    167, 193, 4, 143, 73, 19, 57, 135, 189, 42, 3, 184, 87, 125, 86, 236, 42, 127, 234, 67, 216,
    71, 107, 70, 63, 192, 22, 198, 207, 56, 61, 241,
];

/// `5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx` — PumpSwap `fee_config`:
/// `["fee_config", PUMPSWAP_PROGRAM_ID]` under [`FEE_PROGRAM_ID`], bump 255.
/// Previously carried as "legacy, unverified"; now derives (module doc).
pub const PUMPSWAP_FEE_CONFIG: [u8; 32] = [
    65, 36, 110, 204, 125, 120, 254, 129, 228, 23, 115, 164, 105, 101, 65, 153, 55, 146, 58, 7,
    100, 71, 151, 223, 111, 62, 181, 20, 66, 96, 16, 203,
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An ordered account reference with its signer/writable flags.
///
/// Ordering is part of the instruction's meaning — a re-ordering is a
/// construction defect the fixture-parity gate (criterion 77a) must catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountMeta {
    /// 32-byte account public key.
    pub pubkey: [u8; 32],
    /// Whether this account must sign the transaction.
    pub is_signer: bool,
    /// Whether this account may be written.
    pub is_writable: bool,
}

impl AccountMeta {
    /// Read-only, non-signer.
    pub const fn ro(pubkey: [u8; 32]) -> Self {
        Self {
            pubkey,
            is_signer: false,
            is_writable: false,
        }
    }
    /// Writable, non-signer.
    pub const fn w(pubkey: [u8; 32]) -> Self {
        Self {
            pubkey,
            is_signer: false,
            is_writable: true,
        }
    }
    /// Writable signer.
    pub const fn ws(pubkey: [u8; 32]) -> Self {
        Self {
            pubkey,
            is_signer: true,
            is_writable: true,
        }
    }
}

/// Why an account list could not be built. Every variant is a refusal to
/// substitute a placeholder (§18.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBuildError {
    /// A required input account was all-zero. `Pubkey::default()` is the
    /// System Program address; every venue validates it as something else.
    ZeroedInput(&'static str),
    /// The supplied token program is neither spl-token nor Token-2022.
    UnknownTokenProgram,
    /// The bonding curve's quote mint is not native SOL. USDC-quoted curves
    /// exist (BondingCurve.quote_mint, IDL 2026-08-02); their account layout
    /// is unverified here, so building is refused until it is (§18.2).
    NonSolQuoteMint,
    /// PDA derivation failed (propagated from [`crate::pda`]).
    Pda(PdaError),
}

impl From<PdaError> for AccountBuildError {
    fn from(e: PdaError) -> Self {
        Self::Pda(e)
    }
}

fn require_nonzero(pk: &[u8; 32], name: &'static str) -> Result<(), AccountBuildError> {
    if pk == &[0u8; 32] {
        return Err(AccountBuildError::ZeroedInput(name));
    }
    Ok(())
}

fn require_known_token_program(tp: &[u8; 32]) -> Result<(), AccountBuildError> {
    if tp != &TOKEN_PROGRAM_ID && tp != &TOKEN_2022_PROGRAM_ID {
        return Err(AccountBuildError::UnknownTokenProgram);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// pump.fun bonding curve (§4.1)
// ---------------------------------------------------------------------------

/// Decoded inputs a bonding-curve instruction cannot proceed without.
///
/// Every field is a *decoded on-chain fact*, not a configuration value:
/// `fee_recipient` from the `Global` account ([`crate::decode::decode_global`]),
/// `creator` / `is_cashback_coin` / `quote_mint` from the bonding-curve account
/// ([`crate::decode::decode_pump_curve_extended`]), `token_program` from the
/// mint account's owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpCurveCtx {
    /// The traded token mint.
    pub mint: [u8; 32],
    /// The buying / selling wallet (fee payer and signer).
    pub user: [u8; 32],
    /// `Global.fee_recipient`, decoded — never hardcoded (§4.1 note).
    pub fee_recipient: [u8; 32],
    /// The curve's creator, from the bonding-curve account (offset 49).
    pub creator: [u8; 32],
    /// The mint's owning token program — spl-token or Token-2022, decoded.
    pub token_program: [u8; 32],
    /// `BondingCurve.is_cashback_coin` (byte 82) — decoded, never inferred
    /// from the token program.
    pub is_cashback_coin: bool,
    /// `BondingCurve.quote_mint` (offset 83). Native-SOL curves only for now;
    /// a non-SOL quote refuses the build ([`AccountBuildError::NonSolQuoteMint`]).
    pub quote_mint: [u8; 32],
}

impl PumpCurveCtx {
    fn validate(&self) -> Result<(), AccountBuildError> {
        require_nonzero(&self.mint, "mint")?;
        require_nonzero(&self.user, "user")?;
        require_nonzero(&self.fee_recipient, "fee_recipient")?;
        require_nonzero(&self.creator, "creator")?;
        require_known_token_program(&self.token_program)?;
        if self.quote_mint != WSOL_MINT {
            return Err(AccountBuildError::NonSolQuoteMint);
        }
        Ok(())
    }
}

/// Which trailing account the fee program requires after `bonding_curve_v2`.
///
/// The 2026-08-02 on-chain check proved one exists in 100% of sampled
/// transactions and that the builder omitted it. On 2026-08-03 the identity
/// was SETTLED via two RPC tests (Item 6):
///
/// **TEST 1** — `getAccountInfo` on the pump.fun Global account
/// (`4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf`) decoded three recipient
/// fields: `fee_recipient` (1, offset 41), `fee_recipients` (7, offset 162),
/// and `buyback_fee_recipients` (8, offset 741). The 8 addresses in the
/// `buyback_fee_recipients` list match the 8 observed tail addresses exactly.
///
/// **TEST 2** — `getAccountInfo` on all 8 observed tails: every one is owned
/// by `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` (fee program) with
/// discriminator `[153,166,71,144,179,189,137,251]` = **BuybackVault** and
/// 208-byte data. None is a wallet; none is `fee_program_global`; none is
/// `sharing-config`.
///
/// The trailing account is a **BuybackVault PDA**. The 8 distinct addresses
/// are the 8 entries of the Global account's `buyback_fee_recipients` list.
/// The per-mint selection logic (which of the 8 a given mint uses) is the
/// remaining unknown — the seeds are not yet reverse-engineered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeTail {
    /// Emit no trailing fee account. This is what shipped on 2026-08-02 and it
    /// is now known to be WRONG on mainnet. Kept only so the existing fixtures
    /// and the pre-finding behaviour remain expressible and diffable.
    None,
    /// Per-mint `["sharing-config", mint]` under the fee program. FALSIFIED
    /// on 2026-08-03 — observed tails are BuybackVault PDAs, not SharingConfig.
    /// Retained for diffability and to mark the hypothesis that was tested and
    /// eliminated.
    SharingConfig,
    /// Constant `["fee-program-global"]`. FALSIFIED on 2026-08-03 — observed
    /// tails vary per-mint (8 distinct addresses) and are not the single
    /// constant `fee_program_global` PDA. Retained for diffability.
    FeeProgramGlobal,
    /// A BuybackVault PDA whose address is one of the 8 entries in the Global
    /// account's `buyback_fee_recipients` list (offset 741, 8x32 bytes). The
    /// per-mint selection index is the remaining unknown. Use this variant
    /// when the caller knows which of the 8 addresses applies to this mint.
    BuybackVault([u8; 32]),
    /// An account observed on chain and supplied verbatim by the caller. The
    /// escape hatch that is NOT a guess: use this when the fixture extractor
    /// has read the real account but the derivation is still unknown, so the
    /// builder reproduces observed truth rather than a theory of it.
    Observed([u8; 32]),
}

impl FeeTail {
    /// Resolve to the trailing account meta, if any. Writable per the
    /// observation (the extra account was writable in every sample).
    fn resolve(self, mint: &[u8; 32]) -> Result<Option<AccountMeta>, AccountBuildError> {
        Ok(match self {
            FeeTail::None => None,
            FeeTail::SharingConfig => {
                let (a, _) =
                    pda::find_program_address(&[SHARING_CONFIG_SEED, mint], &FEE_PROGRAM_ID)?;
                Some(AccountMeta::w(a))
            }
            FeeTail::FeeProgramGlobal => Some(AccountMeta::w(FEE_PROGRAM_GLOBAL)),
            FeeTail::BuybackVault(pk) => {
                if pk == [0u8; 32] {
                    return Err(AccountBuildError::ZeroedInput("buyback_vault"));
                }
                Some(AccountMeta::w(pk))
            }
            FeeTail::Observed(pk) => {
                if pk == [0u8; 32] {
                    return Err(AccountBuildError::ZeroedInput("fee_tail_observed"));
                }
                Some(AccountMeta::w(pk))
            }
        })
    }
}

/// Build the pump.fun `buy` account list.
///
/// 17 accounts with [`FeeTail::None`] (the falsified 2026-08-02 shape), 18
/// with any other tail (the observed mainnet shape).
///
/// `[16]` `bonding_curve_v2` (`["bonding-curve-v2", mint]`) is the cashback
/// upgrade's trailing account and is not in the IDL's named list. It was
/// documented here as "must be last"; the chain disagrees — a fee-program
/// account follows it. That comment was wrong and is corrected.
pub fn pump_buy_accounts_with_tail(
    ctx: &PumpCurveCtx,
    tail: FeeTail,
) -> Result<Vec<AccountMeta>, AccountBuildError> {
    let mut v = pump_buy_accounts(ctx)?;
    if let Some(t) = tail.resolve(&ctx.mint)? {
        v.push(t);
    }
    Ok(v)
}

/// Build the pump.fun `sell` account list with the trailing fee account.
///
/// 15/16 with [`FeeTail::None`], 16/17 otherwise (non-cashback / cashback).
pub fn pump_sell_accounts_with_tail(
    ctx: &PumpCurveCtx,
    tail: FeeTail,
) -> Result<Vec<AccountMeta>, AccountBuildError> {
    let mut v = pump_sell_accounts(ctx)?;
    if let Some(t) = tail.resolve(&ctx.mint)? {
        v.push(t);
    }
    Ok(v)
}

/// Build the pump.fun `buy` account list — 17 accounts (§4.1).
///
/// # This shape is KNOWN WRONG on mainnet as of 2026-08-02
/// A live check found 18 accounts. Prefer [`pump_buy_accounts_with_tail`].
/// This function is retained because the existing fixtures encode this shape
/// and because `diff_layout` needs to express "what the builder used to do" to
/// produce a meaningful delta.
pub fn pump_buy_accounts(ctx: &PumpCurveCtx) -> Result<Vec<AccountMeta>, AccountBuildError> {
    ctx.validate()?;
    let (bonding_curve, _) =
        pda::find_program_address(&[b"bonding-curve", &ctx.mint], &PUMP_PROGRAM_ID)?;
    let associated_bonding_curve = pda::derive_ata(&bonding_curve, &ctx.token_program, &ctx.mint)?;
    let associated_user = pda::derive_ata(&ctx.user, &ctx.token_program, &ctx.mint)?;
    let (creator_vault, _) =
        pda::find_program_address(&[b"creator-vault", &ctx.creator], &PUMP_PROGRAM_ID)?;
    let (user_volume_accumulator, _) =
        pda::find_program_address(&[b"user_volume_accumulator", &ctx.user], &PUMP_PROGRAM_ID)?;
    let (bonding_curve_v2, _) =
        pda::find_program_address(&[b"bonding-curve-v2", &ctx.mint], &PUMP_PROGRAM_ID)?;

    let mut v = Vec::with_capacity(17);
    v.push(AccountMeta::ro(PUMP_GLOBAL)); // [0]
    v.push(AccountMeta::w(ctx.fee_recipient)); // [1]
    v.push(AccountMeta::ro(ctx.mint)); // [2]
    v.push(AccountMeta::w(bonding_curve)); // [3]
    v.push(AccountMeta::w(associated_bonding_curve)); // [4]
    v.push(AccountMeta::w(associated_user)); // [5]
    v.push(AccountMeta::ws(ctx.user)); // [6]
    v.push(AccountMeta::ro(SYSTEM_PROGRAM_ID)); // [7]
    v.push(AccountMeta::ro(ctx.token_program)); // [8]
    v.push(AccountMeta::w(creator_vault)); // [9]
    v.push(AccountMeta::ro(PUMP_EVENT_AUTHORITY)); // [10]
    v.push(AccountMeta::ro(PUMP_PROGRAM_ID)); // [11]
    v.push(AccountMeta::ro(PUMP_GLOBAL_VOLUME_ACCUMULATOR)); // [12]
    v.push(AccountMeta::w(user_volume_accumulator)); // [13]
    v.push(AccountMeta::ro(PUMP_FEE_CONFIG)); // [14]
    v.push(AccountMeta::ro(FEE_PROGRAM_ID)); // [15]
    v.push(AccountMeta::ro(bonding_curve_v2)); // [16] — must be last
    Ok(v)
}

/// Build the pump.fun `sell` account list.
///
/// The sell IDL (verified 2026-08-20 from @nirholas/pump-sdk 1.36.0 IDL) has
/// 14 named accounts — NOT 17. Crucially, sell does NOT include
/// `global_volume_accumulator` (that's buy-only). The `fee_config` and
/// `fee_program` are at [12]/[13], right after the 12 core accounts.
///
/// After the 14 named accounts, remaining accounts are appended:
/// - `user_volume_accumulator` [14] — ONLY if `is_cashback_coin` (writable)
/// - `bonding_curve_v2` [14 or 15] — always (read-only)
///
/// The `breaking_fee_recipient` (BuybackVault) is appended by
/// `pump_sell_accounts_with_tail` via the `FeeTail` parameter, producing:
/// - Non-cashback: 14 + 1 (bc_v2) + 1 (tail) = 16 accounts
/// - Cashback: 14 + 1 (uva) + 1 (bc_v2) + 1 (tail) = 17 accounts
///
/// Rev-29 (2026-08-20): COMPLETE REWRITE to match the verified SDK IDL.
/// Previous layout had global_volume_accumulator (buy-only, NOT in sell),
/// wrong fee_config/fee_program positions, and missing breaking_fee_recipient.
pub fn pump_sell_accounts(ctx: &PumpCurveCtx) -> Result<Vec<AccountMeta>, AccountBuildError> {
    ctx.validate()?;
    let (bonding_curve, _) =
        pda::find_program_address(&[b"bonding-curve", &ctx.mint], &PUMP_PROGRAM_ID)?;
    let associated_bonding_curve = pda::derive_ata(&bonding_curve, &ctx.token_program, &ctx.mint)?;
    let associated_user = pda::derive_ata(&ctx.user, &ctx.token_program, &ctx.mint)?;
    let (creator_vault, _) =
        pda::find_program_address(&[b"creator-vault", &ctx.creator], &PUMP_PROGRAM_ID)?;
    let (bonding_curve_v2, _) =
        pda::find_program_address(&[b"bonding-curve-v2", &ctx.mint], &PUMP_PROGRAM_ID)?;

    // Capacity: 14 IDL + up to 2 remaining (uva + bc_v2) = 16 max before tail.
    let mut v = Vec::with_capacity(16);

    // --- 14 IDL named accounts (sell instruction) ---
    v.push(AccountMeta::ro(PUMP_GLOBAL)); // [0]
    v.push(AccountMeta::w(ctx.fee_recipient)); // [1]
    v.push(AccountMeta::ro(ctx.mint)); // [2]
    v.push(AccountMeta::w(bonding_curve)); // [3]
    v.push(AccountMeta::w(associated_bonding_curve)); // [4]
    v.push(AccountMeta::w(associated_user)); // [5]
    v.push(AccountMeta::ws(ctx.user)); // [6]
    v.push(AccountMeta::ro(SYSTEM_PROGRAM_ID)); // [7]
    v.push(AccountMeta::w(creator_vault)); // [8] — sell has creator_vault before token_program
    v.push(AccountMeta::ro(ctx.token_program)); // [9] — sell has token_program after creator_vault
    v.push(AccountMeta::ro(PUMP_EVENT_AUTHORITY)); // [10]
    v.push(AccountMeta::ro(PUMP_PROGRAM_ID)); // [11]
    v.push(AccountMeta::ro(PUMP_FEE_CONFIG)); // [12] — fee_config (sell IDL position)
    v.push(AccountMeta::ro(FEE_PROGRAM_ID)); // [13] — fee_program (sell IDL position)

    // --- remaining accounts ---
    // user_volume_accumulator: ONLY for cashback coins (SDK: "For cashback coins,
    // optionally pass user_volume_accumulator as remaining_accounts[0]")
    if ctx.is_cashback_coin {
        let (user_volume_accumulator, _) =
            pda::find_program_address(&[b"user_volume_accumulator", &ctx.user], &PUMP_PROGRAM_ID)?;
        v.push(AccountMeta::w(user_volume_accumulator)); // [14] — cashback remaining[0]
    }
    v.push(AccountMeta::ro(bonding_curve_v2)); // [14 or 15] — always present

    Ok(v)
}

// ---------------------------------------------------------------------------
// PumpSwap AMM (§4.2)
// ---------------------------------------------------------------------------

/// Decoded inputs for a PumpSwap swap. Sourced from a decoded
/// [`crate::pumpswap::PoolAccount`] plus the mints' owner programs.
///
/// **Pool ordering is a decoded fact, not a convention** (§4.2): `base_mint` /
/// `quote_mint` here are the pool's own assignment, and the traded token is
/// the *quote* side in ~81% of pools. The caller decides which discriminator
/// expresses its trade; this struct only carries the pool as it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpSwapCtx {
    /// The pool account.
    pub pool: [u8; 32],
    /// The trading wallet (signer).
    pub user: [u8; 32],
    /// Pool's base mint, on-chain order (offset 43).
    pub base_mint: [u8; 32],
    /// Pool's quote mint, on-chain order (offset 75).
    pub quote_mint: [u8; 32],
    /// Vault holding base reserves (offset 139).
    pub pool_base_token_account: [u8; 32],
    /// Vault holding quote reserves (offset 171).
    pub pool_quote_token_account: [u8; 32],
    /// One of the `GlobalConfig` rotation set, selected by the caller with a
    /// deterministic seed (slot / blockhash — replay reproduces the choice).
    pub protocol_fee_recipient: [u8; 32],
    /// Base mint's owner program, decoded.
    pub base_token_program: [u8; 32],
    /// Quote mint's owner program, decoded.
    pub quote_token_program: [u8; 32],
    /// `Pool.coin_creator` (offset 211). Zeroed refuses the build — legacy
    /// returned an error when both creator-vault values were zeroed, and prod
    /// keeps that shape (§4.2).
    pub coin_creator: [u8; 32],
    /// `Pool.is_cashback_coin` (byte 244) — decoded, never inferred.
    pub is_cashback_coin: bool,
}

/// PumpSwap `["creator_vault", coin_creator]` authority PDA seed prefix.
///
/// Fail-closed note: the vault *authority* is a PumpSwap PDA and the vault ATA
/// is the quote-mint ATA owned by that authority — both derivable from
/// `coin_creator`, which is why a zeroed `coin_creator` refuses rather than
/// substitutes (deriving from zero would produce well-formed garbage).
const CREATOR_VAULT_SEED: &[u8] = b"creator_vault";

impl PumpSwapCtx {
    fn validate(&self) -> Result<(), AccountBuildError> {
        require_nonzero(&self.pool, "pool")?;
        require_nonzero(&self.user, "user")?;
        require_nonzero(&self.base_mint, "base_mint")?;
        require_nonzero(&self.quote_mint, "quote_mint")?;
        require_nonzero(&self.pool_base_token_account, "pool_base_token_account")?;
        require_nonzero(&self.pool_quote_token_account, "pool_quote_token_account")?;
        require_nonzero(&self.protocol_fee_recipient, "protocol_fee_recipient")?;
        require_nonzero(&self.coin_creator, "coin_creator")?;
        require_known_token_program(&self.base_token_program)?;
        require_known_token_program(&self.quote_token_program)?;
        Ok(())
    }

    /// The shared 19-account prefix `[0..18]` of buy and sell (§4.2).
    fn common_prefix(&self) -> Result<Vec<AccountMeta>, AccountBuildError> {
        self.validate()?;
        let user_base = pda::derive_ata(&self.user, &self.base_token_program, &self.base_mint)?;
        let user_quote = pda::derive_ata(&self.user, &self.quote_token_program, &self.quote_mint)?;
        // Protocol fees are collected in the pool's QUOTE mint (§4.2), so the
        // recipient's token account is their quote-mint ATA.
        let fee_recipient_ata = pda::derive_ata(
            &self.protocol_fee_recipient,
            &self.quote_token_program,
            &self.quote_mint,
        )?;
        let (creator_vault_authority, _) = pda::find_program_address(
            &[CREATOR_VAULT_SEED, &self.coin_creator],
            &PUMPSWAP_PROGRAM_ID,
        )?;
        // Creator fees are also quote-side; the vault ATA is the authority's
        // quote-mint ATA.
        let creator_vault_ata = pda::derive_ata(
            &creator_vault_authority,
            &self.quote_token_program,
            &self.quote_mint,
        )?;

        let mut v = Vec::with_capacity(23);
        v.push(AccountMeta::w(self.pool)); // [0]
        v.push(AccountMeta::ws(self.user)); // [1]
        v.push(AccountMeta::ro(PUMPSWAP_GLOBAL_CONFIG)); // [2]
        v.push(AccountMeta::ro(self.base_mint)); // [3]
        v.push(AccountMeta::ro(self.quote_mint)); // [4]
        v.push(AccountMeta::w(user_base)); // [5]
        v.push(AccountMeta::w(user_quote)); // [6]
        v.push(AccountMeta::w(self.pool_base_token_account)); // [7]
        v.push(AccountMeta::w(self.pool_quote_token_account)); // [8]
        v.push(AccountMeta::w(self.protocol_fee_recipient)); // [9]
        v.push(AccountMeta::w(fee_recipient_ata)); // [10]
        v.push(AccountMeta::ro(self.base_token_program)); // [11]
        v.push(AccountMeta::ro(self.quote_token_program)); // [12]
        v.push(AccountMeta::ro(SYSTEM_PROGRAM_ID)); // [13]
        v.push(AccountMeta::ro(ATA_PROGRAM_ID)); // [14]
        v.push(AccountMeta::ro(PUMPSWAP_EVENT_AUTHORITY)); // [15]
        v.push(AccountMeta::ro(PUMPSWAP_PROGRAM_ID)); // [16] self-CPI
        v.push(AccountMeta::w(creator_vault_ata)); // [17]
        v.push(AccountMeta::ro(creator_vault_authority)); // [18]
        Ok(v)
    }
}

/// Build the PumpSwap `buy` account list — 23 accounts (§4.2), before any
/// remaining-accounts tail.
pub fn pumpswap_buy_accounts(ctx: &PumpSwapCtx) -> Result<Vec<AccountMeta>, AccountBuildError> {
    let mut v = ctx.common_prefix()?;
    let (user_volume_accumulator, _) = pda::find_program_address(
        &[b"user_volume_accumulator", &ctx.user],
        &PUMPSWAP_PROGRAM_ID,
    )?;
    v.push(AccountMeta::ro(PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR)); // [19] buy only
    v.push(AccountMeta::w(user_volume_accumulator)); // [20] buy only
    v.push(AccountMeta::ro(PUMPSWAP_FEE_CONFIG)); // [21]
    v.push(AccountMeta::ro(FEE_PROGRAM_ID)); // [22]
    Ok(v)
}

/// Build the PumpSwap `sell` account list — 21 accounts (§4.2), before any
/// remaining-accounts tail.
pub fn pumpswap_sell_accounts(ctx: &PumpSwapCtx) -> Result<Vec<AccountMeta>, AccountBuildError> {
    let mut v = ctx.common_prefix()?;
    v.push(AccountMeta::ro(PUMPSWAP_FEE_CONFIG)); // [19]
    v.push(AccountMeta::ro(FEE_PROGRAM_ID)); // [20]
    Ok(v)
}
