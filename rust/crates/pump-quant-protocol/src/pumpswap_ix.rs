//! PumpSwap (Pump AMM) **instruction** decoders + account-index map.
//!
//! # Responsibility
//! Decode the `data` blob of outer *or inner* instructions on program
//! `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` — `buy`, `sell`,
//! `create_pool`, `deposit`, `withdraw` — plus detection of the pump.fun
//! `migrate` instruction (program `6EF8rrec…`) that creates the PumpSwap pool
//! at graduation. Discriminators are `sha256("global:<name>")[..8]`, verified
//! against the IDL.
//!
//! # Prefix tolerance
//! Instruction *account lists* only ever grow — new accounts are appended over
//! time (coin-creator vault, fee-program accounts, …). [`SwapAccounts`] and
//! the migrate accessors therefore NEVER require an exact account count: every
//! typed accessor bounds-checks its index and returns `None` beyond the
//! provided list. Likewise, trailing optional *args* (e.g. `buy.track_volume`)
//! decode as `None` when absent and unknown trailing bytes are ignored.
//!
//! # Constitution
//! * §22 — integer-only.
//! * §99 — borrowed slices, fixed-size outputs, zero allocation.
//! * §102 — discriminators and account indices are named constants.
//! * §18.2 — fail closed: a wrong discriminator or a truncated required arg
//!   yields `None`, never a panic or garbage.

use crate::ix::{BUY_DISCRIMINATOR, SELL_DISCRIMINATOR};
use crate::pumpswap::{read_bool, read_pubkey, read_u16_le, read_u64_le};

/// `global:create_pool` discriminator (`sha256("global:create_pool")[..8]`).
pub const CREATE_POOL_DISCRIMINATOR: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];

/// `global:deposit` discriminator (`sha256("global:deposit")[..8]`).
pub const DEPOSIT_DISCRIMINATOR: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];

/// `global:withdraw` discriminator (`sha256("global:withdraw")[..8]`).
pub const WITHDRAW_DISCRIMINATOR: [u8; 8] = [183, 18, 70, 156, 148, 109, 161, 34];

/// pump.fun `global:migrate` discriminator (`sha256("global:migrate")[..8]`),
/// on program `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`.
pub const PUMP_MIGRATE_DISCRIMINATOR: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];

// ---------------------------------------------------------------------------
// Decoded argument structs.
// ---------------------------------------------------------------------------

/// Decoded PumpSwap `buy` args: exact-out swap (`base_amount_out` requested,
/// quote spend capped by `max_quote_amount_in`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyIxArgs {
    /// Exact base tokens requested out, base units.
    pub base_amount_out: u64,
    /// Maximum quote (slippage cap), base units.
    pub max_quote_amount_in: u64,
    /// Trailing optional volume-tracking flag (1-byte option tag + bool).
    /// `None` when the tail is absent, an explicit `None` tag, or malformed
    /// (older payloads predate the field — tolerated by design).
    pub track_volume: Option<bool>,
}

/// Decoded PumpSwap `sell` args: exact-in swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellIxArgs {
    /// Exact base tokens sold in, base units.
    pub base_amount_in: u64,
    /// Minimum quote out (slippage guard), base units.
    pub min_quote_amount_out: u64,
}

/// Decoded PumpSwap `create_pool` args.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatePoolIxArgs {
    /// Pool index within the creator's namespace.
    pub index: u16,
    /// Initial base deposit, base units.
    pub base_amount_in: u64,
    /// Initial quote deposit, base units.
    pub quote_amount_in: u64,
    /// Appended optional coin creator.
    pub coin_creator: Option<[u8; 32]>,
    /// Appended optional mayhem-mode flag.
    pub is_mayhem_mode: Option<bool>,
    /// Appended optional cashback flag (wire: 1-byte option tag + bool; an
    /// explicit `None` tag is flattened to `None`).
    pub is_cashback_coin: Option<bool>,
}

/// Decoded PumpSwap `deposit` args (add liquidity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositIxArgs {
    /// Exact LP tokens requested out.
    pub lp_token_amount_out: u64,
    /// Maximum base deposit (slippage cap), base units.
    pub max_base_amount_in: u64,
    /// Maximum quote deposit (slippage cap), base units.
    pub max_quote_amount_in: u64,
}

/// Decoded PumpSwap `withdraw` args (remove liquidity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawIxArgs {
    /// Exact LP tokens burned in.
    pub lp_token_amount_in: u64,
    /// Minimum base out (slippage guard), base units.
    pub min_base_amount_out: u64,
    /// Minimum quote out (slippage guard), base units.
    pub min_quote_amount_out: u64,
}

/// Any decoded PumpSwap instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpSwapIx {
    /// `buy` — exact-out swap, quote → base.
    Buy(BuyIxArgs),
    /// `sell` — exact-in swap, base → quote.
    Sell(SellIxArgs),
    /// `create_pool` — pool creation (incl. graduation CPI).
    CreatePool(CreatePoolIxArgs),
    /// `deposit` — add liquidity.
    Deposit(DepositIxArgs),
    /// `withdraw` — remove liquidity.
    Withdraw(WithdrawIxArgs),
}

/// Read a Borsh `Option<bool>` (1-byte tag, then 1-byte bool when tag = 1) at
/// `offset`, flattened: absent tail, explicit `None` tag, or a malformed
/// tag/value all yield `None` (length tolerance — the field is a late
/// addition and old payloads simply end before it).
fn read_option_bool_flat(buf: &[u8], offset: usize) -> Option<bool> {
    match buf.get(offset) {
        Some(1) => read_bool(buf, offset.checked_add(1)?),
        _ => None,
    }
}

/// Decode PumpSwap `buy` instruction data.
///
/// Layout: discriminator ++ `base_amount_out` u64 @8 ++ `max_quote_amount_in`
/// u64 @16 ++ optional `track_volume` (option tag @24, bool @25). Trailing
/// unknown bytes beyond the known args are ignored (args are only appended).
/// Returns `None` on a wrong discriminator or truncated required args.
pub fn decode_buy_ix(data: &[u8]) -> Option<BuyIxArgs> {
    if data.get(0..8)? != BUY_DISCRIMINATOR {
        return None;
    }
    Some(BuyIxArgs {
        base_amount_out: read_u64_le(data, 8)?,
        max_quote_amount_in: read_u64_le(data, 16)?,
        track_volume: read_option_bool_flat(data, 24),
    })
}

/// Decode PumpSwap `sell` instruction data.
///
/// Layout: discriminator ++ `base_amount_in` u64 @8 ++ `min_quote_amount_out`
/// u64 @16. Returns `None` on a wrong discriminator or truncated args.
pub fn decode_sell_ix(data: &[u8]) -> Option<SellIxArgs> {
    if data.get(0..8)? != SELL_DISCRIMINATOR {
        return None;
    }
    Some(SellIxArgs {
        base_amount_in: read_u64_le(data, 8)?,
        min_quote_amount_out: read_u64_le(data, 16)?,
    })
}

/// Decode PumpSwap `create_pool` instruction data.
///
/// Layout: discriminator ++ `index` u16 @8 ++ `base_amount_in` u64 @10 ++
/// `quote_amount_in` u64 @18, then optional appended tail: `coin_creator`
/// Pubkey @26, `is_mayhem_mode` bool @58, `is_cashback_coin` option-bool @59.
/// Tail parsing is sequential and stops at the first absent/malformed field.
pub fn decode_create_pool_ix(data: &[u8]) -> Option<CreatePoolIxArgs> {
    if data.get(0..8)? != CREATE_POOL_DISCRIMINATOR {
        return None;
    }
    let index = read_u16_le(data, 8)?;
    let base_amount_in = read_u64_le(data, 10)?;
    let quote_amount_in = read_u64_le(data, 18)?;
    let coin_creator = read_pubkey(data, 26);
    let is_mayhem_mode = if coin_creator.is_some() {
        read_bool(data, 58)
    } else {
        None
    };
    let is_cashback_coin = if is_mayhem_mode.is_some() {
        read_option_bool_flat(data, 59)
    } else {
        None
    };
    Some(CreatePoolIxArgs {
        index,
        base_amount_in,
        quote_amount_in,
        coin_creator,
        is_mayhem_mode,
        is_cashback_coin,
    })
}

/// Decode PumpSwap `deposit` instruction data (3×u64 @8/@16/@24).
pub fn decode_deposit_ix(data: &[u8]) -> Option<DepositIxArgs> {
    if data.get(0..8)? != DEPOSIT_DISCRIMINATOR {
        return None;
    }
    Some(DepositIxArgs {
        lp_token_amount_out: read_u64_le(data, 8)?,
        max_base_amount_in: read_u64_le(data, 16)?,
        max_quote_amount_in: read_u64_le(data, 24)?,
    })
}

/// Decode PumpSwap `withdraw` instruction data (3×u64 @8/@16/@24).
pub fn decode_withdraw_ix(data: &[u8]) -> Option<WithdrawIxArgs> {
    if data.get(0..8)? != WITHDRAW_DISCRIMINATOR {
        return None;
    }
    Some(WithdrawIxArgs {
        lp_token_amount_in: read_u64_le(data, 8)?,
        min_base_amount_out: read_u64_le(data, 16)?,
        min_quote_amount_out: read_u64_le(data, 24)?,
    })
}

/// Decode any PumpSwap instruction by dispatching on its 8-byte discriminator.
///
/// Returns `None` for unknown discriminators (fail closed — an unrecognized
/// instruction is never guessed at) and for truncated required args.
pub fn decode_pumpswap_ix(data: &[u8]) -> Option<PumpSwapIx> {
    let disc: [u8; 8] = data.get(0..8)?.try_into().ok()?;
    match disc {
        BUY_DISCRIMINATOR => decode_buy_ix(data).map(PumpSwapIx::Buy),
        SELL_DISCRIMINATOR => decode_sell_ix(data).map(PumpSwapIx::Sell),
        CREATE_POOL_DISCRIMINATOR => decode_create_pool_ix(data).map(PumpSwapIx::CreatePool),
        DEPOSIT_DISCRIMINATOR => decode_deposit_ix(data).map(PumpSwapIx::Deposit),
        WITHDRAW_DISCRIMINATOR => decode_withdraw_ix(data).map(PumpSwapIx::Withdraw),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// buy/sell account-index map (§102 named indices; prefix-tolerant accessors).
// ---------------------------------------------------------------------------

/// `buy`/`sell` account index: pool state account.
pub const SWAP_IDX_POOL: usize = 0;
/// `buy`/`sell` account index: user (signer).
pub const SWAP_IDX_USER: usize = 1;
/// `buy`/`sell` account index: global config PDA.
pub const SWAP_IDX_GLOBAL_CONFIG: usize = 2;
/// `buy`/`sell` account index: base mint.
pub const SWAP_IDX_BASE_MINT: usize = 3;
/// `buy`/`sell` account index: quote mint.
pub const SWAP_IDX_QUOTE_MINT: usize = 4;
/// `buy`/`sell` account index: user base-token ATA.
pub const SWAP_IDX_USER_BASE_ATA: usize = 5;
/// `buy`/`sell` account index: user quote-token ATA.
pub const SWAP_IDX_USER_QUOTE_ATA: usize = 6;
/// `buy`/`sell` account index: pool base-token vault.
pub const SWAP_IDX_POOL_BASE_ATA: usize = 7;
/// `buy`/`sell` account index: pool quote-token vault.
pub const SWAP_IDX_POOL_QUOTE_ATA: usize = 8;
/// `buy`/`sell` account index: protocol fee recipient.
pub const SWAP_IDX_PROTOCOL_FEE_RECIPIENT: usize = 9;
/// `buy`/`sell` account index: protocol fee recipient's token ATA.
pub const SWAP_IDX_PROTOCOL_FEE_RECIPIENT_ATA: usize = 10;
/// `buy`/`sell` account index: Anchor event authority PDA.
pub const SWAP_IDX_EVENT_AUTHORITY: usize = 16;
/// `buy`/`sell` account index: the PumpSwap program itself.
pub const SWAP_IDX_PROGRAM: usize = 17;
/// `buy`/`sell` account index: coin-creator vault ATA (appended later).
pub const SWAP_IDX_COIN_CREATOR_VAULT_ATA: usize = 18;
/// `buy`/`sell` account index: coin-creator vault authority (appended later).
pub const SWAP_IDX_COIN_CREATOR_VAULT_AUTHORITY: usize = 19;

/// Typed, prefix-tolerant view over the resolved account keys of a PumpSwap
/// `buy`/`sell` instruction.
///
/// The caller resolves the instruction's account *indices* against the
/// transaction message's key table and passes the resulting ordered pubkey
/// slice. Accessors NEVER require an exact account count — the program has
/// appended accounts over time (coin-creator vault, fee-program accounts) and
/// will again; any index beyond the provided list returns `None`.
#[derive(Debug, Clone, Copy)]
pub struct SwapAccounts<'a> {
    keys: &'a [[u8; 32]],
}

impl<'a> SwapAccounts<'a> {
    /// Wrap an ordered account-key slice (as listed by the instruction).
    pub const fn new(keys: &'a [[u8; 32]]) -> Self {
        Self { keys }
    }

    /// Key at raw index `idx`, `None` beyond the provided list.
    pub fn get(&self, idx: usize) -> Option<&'a [u8; 32]> {
        self.keys.get(idx)
    }

    /// Pool state account.
    pub fn pool(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_POOL)
    }
    /// User (signer).
    pub fn user(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_USER)
    }
    /// Global config PDA.
    pub fn global_config(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_GLOBAL_CONFIG)
    }
    /// Base mint.
    pub fn base_mint(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_BASE_MINT)
    }
    /// Quote mint.
    pub fn quote_mint(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_QUOTE_MINT)
    }
    /// User base-token ATA.
    pub fn user_base_token_account(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_USER_BASE_ATA)
    }
    /// User quote-token ATA.
    pub fn user_quote_token_account(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_USER_QUOTE_ATA)
    }
    /// Pool base-token vault (reserve account).
    pub fn pool_base_token_account(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_POOL_BASE_ATA)
    }
    /// Pool quote-token vault (reserve account).
    pub fn pool_quote_token_account(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_POOL_QUOTE_ATA)
    }
    /// Protocol fee recipient.
    pub fn protocol_fee_recipient(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_PROTOCOL_FEE_RECIPIENT)
    }
    /// Protocol fee recipient's token ATA.
    pub fn protocol_fee_recipient_token_account(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_PROTOCOL_FEE_RECIPIENT_ATA)
    }
    /// Anchor event authority PDA.
    pub fn event_authority(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_EVENT_AUTHORITY)
    }
    /// The PumpSwap program id itself.
    pub fn program(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_PROGRAM)
    }
    /// Coin-creator vault ATA (`None` on older transactions).
    pub fn coin_creator_vault_ata(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_COIN_CREATOR_VAULT_ATA)
    }
    /// Coin-creator vault authority (`None` on older transactions).
    pub fn coin_creator_vault_authority(&self) -> Option<&'a [u8; 32]> {
        self.get(SWAP_IDX_COIN_CREATOR_VAULT_AUTHORITY)
    }
}

// ---------------------------------------------------------------------------
// pump.fun `migrate` detection (graduation → PumpSwap pool creation).
// ---------------------------------------------------------------------------

/// `migrate` account index: the graduating token mint.
pub const MIGRATE_IDX_MINT: usize = 2;
/// `migrate` account index: the PumpSwap pool being created.
pub const MIGRATE_IDX_POOL: usize = 9;

/// `true` iff `data` is pump.fun `migrate` instruction data (detection only —
/// the instruction carries no args worth decoding).
pub fn is_pump_migrate_ix(data: &[u8]) -> bool {
    data.get(0..8) == Some(&PUMP_MIGRATE_DISCRIMINATOR[..])
}

/// The graduating mint from a `migrate` instruction's resolved account keys
/// (index [`MIGRATE_IDX_MINT`]); `None` beyond the provided list.
pub fn migrate_mint(keys: &[[u8; 32]]) -> Option<&[u8; 32]> {
    keys.get(MIGRATE_IDX_MINT)
}

/// The created PumpSwap pool from a `migrate` instruction's resolved account
/// keys (index [`MIGRATE_IDX_POOL`]); `None` beyond the provided list.
pub fn migrate_pool(keys: &[[u8; 32]]) -> Option<&[u8; 32]> {
    keys.get(MIGRATE_IDX_POOL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    fn ix_data(disc: [u8; 8], args: &[u8]) -> Vec<u8> {
        let mut d = disc.to_vec();
        d.extend_from_slice(args);
        d
    }

    #[test]
    fn buy_decodes_without_track_volume() {
        let mut args = Vec::new();
        args.extend_from_slice(&1_000u64.to_le_bytes());
        args.extend_from_slice(&2_000u64.to_le_bytes());
        let b = decode_buy_ix(&ix_data(BUY_DISCRIMINATOR, &args)).expect("decodes");
        assert_eq!(b.base_amount_out, 1_000);
        assert_eq!(b.max_quote_amount_in, 2_000);
        assert_eq!(b.track_volume, None, "absent tail tolerated");
    }

    #[test]
    fn buy_decodes_track_volume_variants() {
        let base: Vec<u8> = [3u64.to_le_bytes(), 4u64.to_le_bytes()].concat();
        // Some(true): tag 1, value 1.
        let mut a = base.clone();
        a.extend_from_slice(&[1, 1]);
        let b = decode_buy_ix(&ix_data(BUY_DISCRIMINATOR, &a)).expect("decodes");
        assert_eq!(b.track_volume, Some(true));
        // Some(false): tag 1, value 0.
        let mut a = base.clone();
        a.extend_from_slice(&[1, 0]);
        let b = decode_buy_ix(&ix_data(BUY_DISCRIMINATOR, &a)).expect("decodes");
        assert_eq!(b.track_volume, Some(false));
        // Explicit None tag.
        let mut a = base.clone();
        a.push(0);
        let b = decode_buy_ix(&ix_data(BUY_DISCRIMINATOR, &a)).expect("decodes");
        assert_eq!(b.track_volume, None);
        // Garbage tag byte: tolerated as absent, fixed args still decoded.
        let mut a = base;
        a.extend_from_slice(&[0xEE, 0xEE, 0xEE, 0xEE]);
        let b = decode_buy_ix(&ix_data(BUY_DISCRIMINATOR, &a)).expect("decodes");
        assert_eq!(b.base_amount_out, 3);
        assert_eq!(b.track_volume, None);
    }

    #[test]
    fn buy_rejects_wrong_disc_and_truncation() {
        let args: Vec<u8> = [1u64.to_le_bytes(), 2u64.to_le_bytes()].concat();
        assert!(decode_buy_ix(&ix_data(SELL_DISCRIMINATOR, &args)).is_none());
        let good = ix_data(BUY_DISCRIMINATOR, &args);
        for len in [0, 7, 8, 15, 23] {
            assert!(decode_buy_ix(&good[..len]).is_none(), "len {len}");
        }
    }

    #[test]
    fn sell_decodes_and_rejects() {
        let args: Vec<u8> = [7u64.to_le_bytes(), 9u64.to_le_bytes()].concat();
        let s = decode_sell_ix(&ix_data(SELL_DISCRIMINATOR, &args)).expect("decodes");
        assert_eq!(s.base_amount_in, 7);
        assert_eq!(s.min_quote_amount_out, 9);
        let good = ix_data(SELL_DISCRIMINATOR, &args);
        assert!(decode_sell_ix(&good[..20]).is_none());
        assert!(decode_sell_ix(&ix_data(BUY_DISCRIMINATOR, &args)).is_none());
    }

    fn create_pool_args(tail: &[u8]) -> Vec<u8> {
        let mut a = Vec::new();
        a.extend_from_slice(&3u16.to_le_bytes());
        a.extend_from_slice(&11u64.to_le_bytes());
        a.extend_from_slice(&22u64.to_le_bytes());
        a.extend_from_slice(tail);
        a
    }

    #[test]
    fn create_pool_decodes_base_args() {
        let d = ix_data(CREATE_POOL_DISCRIMINATOR, &create_pool_args(&[]));
        let c = decode_create_pool_ix(&d).expect("decodes");
        assert_eq!(c.index, 3);
        assert_eq!(c.base_amount_in, 11);
        assert_eq!(c.quote_amount_in, 22);
        assert_eq!(c.coin_creator, None);
        assert_eq!(c.is_mayhem_mode, None);
        assert_eq!(c.is_cashback_coin, None);
    }

    #[test]
    fn create_pool_decodes_full_and_partial_tail() {
        // Full tail: creator + mayhem + Some(true) cashback.
        let mut tail = pk(0xAB).to_vec();
        tail.push(0);
        tail.extend_from_slice(&[1, 1]);
        let d = ix_data(CREATE_POOL_DISCRIMINATOR, &create_pool_args(&tail));
        let c = decode_create_pool_ix(&d).expect("decodes");
        assert_eq!(c.coin_creator, Some(pk(0xAB)));
        assert_eq!(c.is_mayhem_mode, Some(false));
        assert_eq!(c.is_cashback_coin, Some(true));
        // Partial tail: creator only.
        let d = ix_data(CREATE_POOL_DISCRIMINATOR, &create_pool_args(&pk(0xAB)));
        let c = decode_create_pool_ix(&d).expect("decodes");
        assert_eq!(c.coin_creator, Some(pk(0xAB)));
        assert_eq!(c.is_mayhem_mode, None);
        assert_eq!(c.is_cashback_coin, None);
    }

    #[test]
    fn create_pool_rejects_truncated_required_args() {
        let d = ix_data(CREATE_POOL_DISCRIMINATOR, &create_pool_args(&[]));
        for len in [9, 10, 17, 25] {
            assert!(decode_create_pool_ix(&d[..len]).is_none(), "len {len}");
        }
    }

    #[test]
    fn deposit_and_withdraw_decode() {
        let args: Vec<u8> = [5u64.to_le_bytes(), 6u64.to_le_bytes(), 7u64.to_le_bytes()].concat();
        let d = decode_deposit_ix(&ix_data(DEPOSIT_DISCRIMINATOR, &args)).expect("decodes");
        assert_eq!(
            (
                d.lp_token_amount_out,
                d.max_base_amount_in,
                d.max_quote_amount_in
            ),
            (5, 6, 7)
        );
        let w = decode_withdraw_ix(&ix_data(WITHDRAW_DISCRIMINATOR, &args)).expect("decodes");
        assert_eq!(
            (
                w.lp_token_amount_in,
                w.min_base_amount_out,
                w.min_quote_amount_out
            ),
            (5, 6, 7)
        );
        assert!(decode_deposit_ix(&ix_data(WITHDRAW_DISCRIMINATOR, &args)).is_none());
        assert!(decode_withdraw_ix(&ix_data(DEPOSIT_DISCRIMINATOR, &args)[..30]).is_none());
    }

    #[test]
    fn dispatch_decodes_every_kind_and_fails_closed() {
        let two: Vec<u8> = [1u64.to_le_bytes(), 2u64.to_le_bytes()].concat();
        let three: Vec<u8> = [1u64.to_le_bytes(), 2u64.to_le_bytes(), 3u64.to_le_bytes()].concat();
        assert!(matches!(
            decode_pumpswap_ix(&ix_data(BUY_DISCRIMINATOR, &two)),
            Some(PumpSwapIx::Buy(_))
        ));
        assert!(matches!(
            decode_pumpswap_ix(&ix_data(SELL_DISCRIMINATOR, &two)),
            Some(PumpSwapIx::Sell(_))
        ));
        assert!(matches!(
            decode_pumpswap_ix(&ix_data(CREATE_POOL_DISCRIMINATOR, &create_pool_args(&[]))),
            Some(PumpSwapIx::CreatePool(_))
        ));
        assert!(matches!(
            decode_pumpswap_ix(&ix_data(DEPOSIT_DISCRIMINATOR, &three)),
            Some(PumpSwapIx::Deposit(_))
        ));
        assert!(matches!(
            decode_pumpswap_ix(&ix_data(WITHDRAW_DISCRIMINATOR, &three)),
            Some(PumpSwapIx::Withdraw(_))
        ));
        // Unknown discriminator → None (never guessed).
        assert!(decode_pumpswap_ix(&ix_data([9u8; 8], &three)).is_none());
        assert!(decode_pumpswap_ix(&[]).is_none());
    }

    #[test]
    fn swap_accounts_full_list_resolves_every_accessor() {
        let keys: Vec<[u8; 32]> = (0u8..20).map(pk).collect();
        let a = SwapAccounts::new(&keys);
        assert_eq!(a.pool(), Some(&pk(0)));
        assert_eq!(a.user(), Some(&pk(1)));
        assert_eq!(a.global_config(), Some(&pk(2)));
        assert_eq!(a.base_mint(), Some(&pk(3)));
        assert_eq!(a.quote_mint(), Some(&pk(4)));
        assert_eq!(a.user_base_token_account(), Some(&pk(5)));
        assert_eq!(a.user_quote_token_account(), Some(&pk(6)));
        assert_eq!(a.pool_base_token_account(), Some(&pk(7)));
        assert_eq!(a.pool_quote_token_account(), Some(&pk(8)));
        assert_eq!(a.protocol_fee_recipient(), Some(&pk(9)));
        assert_eq!(a.protocol_fee_recipient_token_account(), Some(&pk(10)));
        assert_eq!(a.event_authority(), Some(&pk(16)));
        assert_eq!(a.program(), Some(&pk(17)));
        assert_eq!(a.coin_creator_vault_ata(), Some(&pk(18)));
        assert_eq!(a.coin_creator_vault_authority(), Some(&pk(19)));
    }

    #[test]
    fn swap_accounts_prefix_tolerant_beyond_len_is_none() {
        // Historical 11-account list: core accessors work, appended ones None.
        let keys: Vec<[u8; 32]> = (0u8..11).map(pk).collect();
        let a = SwapAccounts::new(&keys);
        assert_eq!(a.protocol_fee_recipient_token_account(), Some(&pk(10)));
        assert_eq!(a.event_authority(), None);
        assert_eq!(a.coin_creator_vault_ata(), None);
        assert_eq!(a.coin_creator_vault_authority(), None);
        // More accounts than we know about: still fine, extras ignored.
        let keys: Vec<[u8; 32]> = (0u8..30).map(pk).collect();
        let a = SwapAccounts::new(&keys);
        assert_eq!(a.coin_creator_vault_authority(), Some(&pk(19)));
        // Empty list: everything None, no panic.
        let a = SwapAccounts::new(&[]);
        assert_eq!(a.pool(), None);
        assert_eq!(a.user(), None);
    }

    #[test]
    fn migrate_detection_and_accounts() {
        let data = ix_data(PUMP_MIGRATE_DISCRIMINATOR, &[]);
        assert!(is_pump_migrate_ix(&data));
        assert!(!is_pump_migrate_ix(&ix_data(BUY_DISCRIMINATOR, &[])));
        assert!(!is_pump_migrate_ix(&data[..7]));

        let keys: Vec<[u8; 32]> = (0u8..12).map(pk).collect();
        assert_eq!(migrate_mint(&keys), Some(&pk(2)));
        assert_eq!(migrate_pool(&keys), Some(&pk(9)));
        // Short account list: beyond-len is None, never a panic.
        assert_eq!(migrate_mint(&keys[..2]), None);
        assert_eq!(migrate_pool(&keys[..9]), None);
    }
}
