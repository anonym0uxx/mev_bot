//! Full PumpSwap (Pump AMM) **account** decoders + venue constants.
//!
//! # Responsibility
//! Byte-offset, length-tolerant decoders for the complete on-chain layouts of
//! the PumpSwap program (`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`):
//! [`PoolAccount`], [`GlobalConfigAccount`], the SPL token-account amount read
//! (pool reserves live in the pool's two token vaults, **not** in the `Pool`
//! account), and the appended-tail fields of the pump.fun `BondingCurve`
//! account that [`crate::decode::decode_pump_curve`] predates.
//!
//! Layout source of truth: the official `pump-fun/pump-public-docs` IDL,
//! cross-checked against the `carbon-pump-swap-decoder` source.
//!
//! # Length tolerance (fields are only ever appended)
//! On-chain accounts here are resized via `extend_account`; new fields are
//! appended, never inserted. Decoders therefore:
//! * decode every field by absolute byte offset;
//! * accept buffers **longer** than the known layout (a freshly-resized
//!   account is zero-padded; trailing unknown bytes are ignored);
//! * accept **shorter historical** buffers by returning `None` for each absent
//!   optional tail field (tail parsing is sequential and stops at the first
//!   absent or malformed field, so a partially-present tail never yields a
//!   field read past its predecessor).
//!
//! # Constitution
//! * §22 — integer-only; no float is produced or consumed.
//! * §99 — no allocation or unbounded state: every decode is a fixed-size
//!   struct read out of a borrowed slice.
//! * §102 — magic numbers are named constants with layout citations.
//! * §18.2 — fail closed on identity: the 8-byte Anchor discriminator is
//!   verified before any field is trusted; every field access is
//!   bounds-checked, so malformed input yields `None`, never a panic.

use crate::registry;

// ---------------------------------------------------------------------------
// Venue constants (§102 — named, cited).
// ---------------------------------------------------------------------------

/// PumpSwap `GlobalConfig` PDA on mainnet, derived from seeds
/// `["global_config"]` under program `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`.
/// Matches [`crate::registry::entry`]`(Venue::PumpSwap).config_pda`.
pub const PUMPSWAP_GLOBAL_CONFIG_PDA: &str = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw";

/// Anchor account discriminator for `GlobalConfig`
/// (`sha256("account:GlobalConfig")[..8]`).
pub const GLOBAL_CONFIG_DISCRIMINATOR: [u8; 8] = [149, 8, 156, 202, 160, 252, 176, 217];

/// Pump fee program (`pfee…`), the 2026 external fee program the pump programs
/// route protocol fees through.
///
/// Since 2025-09-01 pump.fun / PumpSwap fees are **dynamic, market-cap-tiered**
/// and configured through this program — a compiled-in fee schedule would rot.
/// The per-trade `*_fee_basis_points` fields carried in the Anchor CPI events
/// ([`crate::pumpswap_event`]) are the ground truth for what a given trade
/// actually paid; never hardcode a schedule.
pub const PUMP_FEES_PROGRAM: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";

/// Canonical wrapped-SOL mint — the quote asset of pools created by pump.fun
/// graduation.
///
/// Decode MUST key off `PoolAccount::quote_mint` rather than assuming this
/// value: USDC-quoted PumpSwap pools exist since 2026-05.
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Byte offset of the `amount` field (u64 LE) inside an SPL token account.
///
/// SPL token-account layout: `mint` (32) ++ `owner` (32) ++ `amount` (u64 LE
/// at 64). PumpSwap pool reserves are **not** `Pool` fields — they are the
/// live balances of `pool_base_token_account` / `pool_quote_token_account`,
/// read with [`decode_spl_token_amount`].
pub const SPL_TOKEN_AMOUNT_OFFSET: usize = 64;

// ---------------------------------------------------------------------------
// Shared bounds-checked little-endian readers (used by the sibling
// `pumpswap_ix` / `pumpswap_event` modules as well).
// ---------------------------------------------------------------------------

/// Read a little-endian `u16` at `offset`; `None` if out of bounds.
pub(crate) fn read_u16_le(buf: &[u8], offset: usize) -> Option<u16> {
    let s = buf.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Read a little-endian `u64` at `offset`; `None` if out of bounds.
pub(crate) fn read_u64_le(buf: &[u8], offset: usize) -> Option<u64> {
    let s = buf.get(offset..offset.checked_add(8)?)?;
    let mut b = [0u8; 8];
    b.copy_from_slice(s);
    Some(u64::from_le_bytes(b))
}

/// Read a little-endian `i64` at `offset`; `None` if out of bounds.
pub(crate) fn read_i64_le(buf: &[u8], offset: usize) -> Option<i64> {
    let s = buf.get(offset..offset.checked_add(8)?)?;
    let mut b = [0u8; 8];
    b.copy_from_slice(s);
    Some(i64::from_le_bytes(b))
}

/// Read a 32-byte pubkey at `offset`; `None` if out of bounds.
pub(crate) fn read_pubkey(buf: &[u8], offset: usize) -> Option<[u8; 32]> {
    let s = buf.get(offset..offset.checked_add(32)?)?;
    let mut b = [0u8; 32];
    b.copy_from_slice(s);
    Some(b)
}

/// Read a canonical boolean (`0`/`1`) at `offset`; any other byte, or an
/// out-of-bounds offset, yields `None`.
pub(crate) fn read_bool(buf: &[u8], offset: usize) -> Option<bool> {
    match buf.get(offset)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pool account (full on-chain layout).
// ---------------------------------------------------------------------------

/// Fully-decoded PumpSwap `Pool` account (complete on-chain layout).
///
/// This is the *full* IDL layout of the account, distinct from the legacy
/// reduced [`crate::decode::PumpSwapPool`] view (which is pinned by its
/// dossier test and left untouched). Note that reserves are **not** stored in
/// the pool: read them from `pool_base_token_account` /
/// `pool_quote_token_account` via [`decode_spl_token_amount`].
///
/// ```text
/// offset  size  field
/// 0       8     anchor discriminator (sha256("account:Pool")[..8])
/// 8       1     pool_bump                 (u8)
/// 9       2     index                     (u16 LE)
/// 11      32    creator                   (Pubkey)
/// 43      32    base_mint                 (Pubkey)
/// 75      32    quote_mint                (Pubkey)
/// 107     32    lp_mint                   (Pubkey)
/// 139     32    pool_base_token_account   (Pubkey)
/// 171     32    pool_quote_token_account  (Pubkey)
/// 203     8     lp_supply                 (u64 LE)
/// ---- optional appended tail (extend_account resizes; §length tolerance) ----
/// 211     32    coin_creator              (Pubkey)
/// 243     1     is_mayhem_mode            (bool)
/// 244     1     is_cashback_coin          (bool)
/// ```
/// Current mainnet accounts run 245–300 bytes; pre-`coin_creator` historical
/// accounts are 211 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolAccount {
    /// PDA bump seed of the pool account.
    pub pool_bump: u8,
    /// Pool index within the creator's namespace.
    pub index: u16,
    /// Wallet that created the pool.
    pub creator: [u8; 32],
    /// Base-token mint (the traded memecoin).
    pub base_mint: [u8; 32],
    /// Quote-token mint. Usually [`WSOL_MINT`], but key off this field —
    /// USDC-quoted pools exist since 2026-05.
    pub quote_mint: [u8; 32],
    /// LP-token mint.
    pub lp_mint: [u8; 32],
    /// SPL token account holding the pool's base reserves.
    pub pool_base_token_account: [u8; 32],
    /// SPL token account holding the pool's quote reserves.
    pub pool_quote_token_account: [u8; 32],
    /// Outstanding LP-token supply.
    pub lp_supply: u64,
    /// Coin creator receiving creator fees (`None` on 211-byte historical
    /// accounts predating the field).
    pub coin_creator: Option<[u8; 32]>,
    /// Mayhem-mode flag (`None` when the tail byte is absent).
    pub is_mayhem_mode: Option<bool>,
    /// Cashback-coin flag (`None` when the tail byte is absent).
    pub is_cashback_coin: Option<bool>,
}

/// Length of the pre-`coin_creator` historical `Pool` account: 8-byte
/// discriminator + fixed fields through `lp_supply` (ends at byte 211).
pub const POOL_FIXED_LEN: usize = 211;

/// Decode a PumpSwap `Pool` account with the complete on-chain layout.
///
/// Returns `None` when the buffer is shorter than [`POOL_FIXED_LEN`] or the
/// discriminator does not match `sha256("account:Pool")[..8]` (§18.2
/// fail-closed). Optional tail fields decode sequentially: parsing stops at
/// the first absent or non-canonical field, so a truncated or corrupt tail
/// yields `None` for that field and everything after it — never a panic.
///
/// # Constitution
/// §22 integer-only; §18.2 identity verified first; every access bounds-checked.
pub fn decode_pool_account(account: &[u8]) -> Option<PoolAccount> {
    if account.get(0..8)? != registry::PUMPSWAP_ACCOUNT_DISCRIMINATOR {
        return None;
    }
    if account.len() < POOL_FIXED_LEN {
        return None;
    }
    let pool_bump = *account.get(8)?;
    let index = read_u16_le(account, 9)?;
    let creator = read_pubkey(account, 11)?;
    let base_mint = read_pubkey(account, 43)?;
    let quote_mint = read_pubkey(account, 75)?;
    let lp_mint = read_pubkey(account, 107)?;
    let pool_base_token_account = read_pubkey(account, 139)?;
    let pool_quote_token_account = read_pubkey(account, 171)?;
    let lp_supply = read_u64_le(account, 203)?;

    // Sequential optional tail (fields only ever appended).
    let coin_creator = read_pubkey(account, 211);
    let is_mayhem_mode = if coin_creator.is_some() {
        read_bool(account, 243)
    } else {
        None
    };
    let is_cashback_coin = if is_mayhem_mode.is_some() {
        read_bool(account, 244)
    } else {
        None
    };

    Some(PoolAccount {
        pool_bump,
        index,
        creator,
        base_mint,
        quote_mint,
        lp_mint,
        pool_base_token_account,
        pool_quote_token_account,
        lp_supply,
        coin_creator,
        is_mayhem_mode,
        is_cashback_coin,
    })
}

// ---------------------------------------------------------------------------
// GlobalConfig account.
// ---------------------------------------------------------------------------

/// Number of protocol-fee-recipient slots in the fixed `GlobalConfig` layout.
pub const PROTOCOL_FEE_RECIPIENT_SLOTS: usize = 8;

/// Number of reserved-fee-recipient slots in the appended `GlobalConfig` tail.
pub const RESERVED_FEE_RECIPIENT_SLOTS: usize = 7;

/// Decoded PumpSwap `GlobalConfig` account.
///
/// Known mainnet address: [`PUMPSWAP_GLOBAL_CONFIG_PDA`] (seeds
/// `["global_config"]`).
///
/// ```text
/// offset  size  field
/// 0       8     anchor discriminator (sha256("account:GlobalConfig")[..8])
/// 8       32    admin                            (Pubkey)
/// 40      8     lp_fee_basis_points              (u64 LE)
/// 48      8     protocol_fee_basis_points        (u64 LE)
/// 56      1     disable_flags                    (u8 bitfield)
/// 57      256   protocol_fee_recipients          ([Pubkey; 8])
/// ---- optional appended tail ----
/// 313     8     coin_creator_fee_basis_points    (u64 LE)
/// 321     32    admin_set_coin_creator_authority (Pubkey)
/// 353     32    whitelist_pda                    (Pubkey)
/// 385     32    reserved_fee_recipient           (Pubkey)
/// 417     1     mayhem_mode_enabled              (bool)
/// 418     224   reserved_fee_recipients          ([Pubkey; 7])
/// 642     1     is_cashback_enabled              (bool)
/// ```
///
/// The `*_fee_basis_points` fields here are the *config-level defaults*; the
/// authoritative per-trade rates are the ones echoed in each trade's CPI event
/// (dynamic market-cap-tiered fees since 2025-09-01 — see
/// [`PUMP_FEES_PROGRAM`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalConfigAccount {
    /// Admin authority.
    pub admin: [u8; 32],
    /// Default LP fee, basis points.
    pub lp_fee_basis_points: u64,
    /// Default protocol fee, basis points.
    pub protocol_fee_basis_points: u64,
    /// Bitfield disabling create/deposit/withdraw/buy/sell.
    pub disable_flags: u8,
    /// Protocol-fee recipient set.
    pub protocol_fee_recipients: [[u8; 32]; PROTOCOL_FEE_RECIPIENT_SLOTS],
    /// Default coin-creator fee, basis points (appended tail).
    pub coin_creator_fee_basis_points: Option<u64>,
    /// Authority allowed to set a coin creator by admin action (appended tail).
    pub admin_set_coin_creator_authority: Option<[u8; 32]>,
    /// Whitelist PDA (appended tail).
    pub whitelist_pda: Option<[u8; 32]>,
    /// Reserved fee recipient (appended tail).
    pub reserved_fee_recipient: Option<[u8; 32]>,
    /// Global mayhem-mode switch (appended tail).
    pub mayhem_mode_enabled: Option<bool>,
    /// Reserved fee-recipient set (appended tail).
    pub reserved_fee_recipients: Option<[[u8; 32]; RESERVED_FEE_RECIPIENT_SLOTS]>,
    /// Global cashback switch (appended tail).
    pub is_cashback_enabled: Option<bool>,
}

/// Length of the fixed (pre-tail) `GlobalConfig` layout: discriminator through
/// the eighth protocol-fee recipient (ends at byte 313).
pub const GLOBAL_CONFIG_FIXED_LEN: usize = 313;

/// Byte offset of the appended `GlobalConfig` tail (`coin_creator_fee_basis_points`).
const GLOBAL_CONFIG_TAIL_OFFSET: usize = 313;

/// Decode a PumpSwap `GlobalConfig` account.
///
/// Returns `None` when the buffer is shorter than
/// [`GLOBAL_CONFIG_FIXED_LEN`] or the discriminator mismatches
/// [`GLOBAL_CONFIG_DISCRIMINATOR`] (§18.2 fail-closed). The appended tail
/// decodes sequentially and stops at the first absent/malformed field.
///
/// # Constitution
/// §22 integer-only; §18.2 identity verified first; every access bounds-checked.
pub fn decode_global_config(account: &[u8]) -> Option<GlobalConfigAccount> {
    if account.get(0..8)? != GLOBAL_CONFIG_DISCRIMINATOR {
        return None;
    }
    if account.len() < GLOBAL_CONFIG_FIXED_LEN {
        return None;
    }
    let admin = read_pubkey(account, 8)?;
    let lp_fee_basis_points = read_u64_le(account, 40)?;
    let protocol_fee_basis_points = read_u64_le(account, 48)?;
    let disable_flags = *account.get(56)?;
    let mut protocol_fee_recipients = [[0u8; 32]; PROTOCOL_FEE_RECIPIENT_SLOTS];
    for (i, slot) in protocol_fee_recipients.iter_mut().enumerate() {
        *slot = read_pubkey(account, 57 + 32 * i)?;
    }

    // Sequential optional tail.
    let coin_creator_fee_basis_points = read_u64_le(account, GLOBAL_CONFIG_TAIL_OFFSET);
    let admin_set_coin_creator_authority = if coin_creator_fee_basis_points.is_some() {
        read_pubkey(account, 321)
    } else {
        None
    };
    let whitelist_pda = if admin_set_coin_creator_authority.is_some() {
        read_pubkey(account, 353)
    } else {
        None
    };
    let reserved_fee_recipient = if whitelist_pda.is_some() {
        read_pubkey(account, 385)
    } else {
        None
    };
    let mayhem_mode_enabled = if reserved_fee_recipient.is_some() {
        read_bool(account, 417)
    } else {
        None
    };
    let reserved_fee_recipients = if mayhem_mode_enabled.is_some() {
        let mut set = [[0u8; 32]; RESERVED_FEE_RECIPIENT_SLOTS];
        let mut complete = true;
        for (i, slot) in set.iter_mut().enumerate() {
            match read_pubkey(account, 418 + 32 * i) {
                Some(k) => *slot = k,
                None => {
                    complete = false;
                    break;
                }
            }
        }
        if complete {
            Some(set)
        } else {
            None
        }
    } else {
        None
    };
    let is_cashback_enabled = if reserved_fee_recipients.is_some() {
        read_bool(account, 642)
    } else {
        None
    };

    Some(GlobalConfigAccount {
        admin,
        lp_fee_basis_points,
        protocol_fee_basis_points,
        disable_flags,
        protocol_fee_recipients,
        coin_creator_fee_basis_points,
        admin_set_coin_creator_authority,
        whitelist_pda,
        reserved_fee_recipient,
        mayhem_mode_enabled,
        reserved_fee_recipients,
        is_cashback_enabled,
    })
}

// ---------------------------------------------------------------------------
// SPL token-account amount (pool reserves).
// ---------------------------------------------------------------------------

/// Read the `amount` (u64 LE at [`SPL_TOKEN_AMOUNT_OFFSET`]) out of a raw SPL
/// token account buffer.
///
/// This is how PumpSwap pool reserves are observed at rest: the pool's base /
/// quote reserves are the balances of `pool_base_token_account` /
/// `pool_quote_token_account` — they are **not** fields of the `Pool` account.
/// (Per-trade, prefer the reserve snapshots carried in the CPI events, which
/// are synchronous with the trade — see [`crate::pumpswap_event`].)
///
/// Returns `None` when the buffer is too short to contain the field. No
/// discriminator exists on SPL token accounts; the caller is responsible for
/// having fetched the right address.
pub fn decode_spl_token_amount(account: &[u8]) -> Option<u64> {
    read_u64_le(account, SPL_TOKEN_AMOUNT_OFFSET)
}

// ---------------------------------------------------------------------------
// pump.fun BondingCurve appended tail (creator / mayhem).
// ---------------------------------------------------------------------------

/// Appended-tail fields of the pump.fun `BondingCurve` account that postdate
/// the 49-byte layout decoded by [`crate::decode::decode_pump_curve`] (which
/// is pinned by its dossier test and deliberately untouched).
///
/// ```text
/// offset  size  field
/// 49      32    creator         (Pubkey)   [optional tail]
/// 81      1     is_mayhem_mode  (bool)     [optional tail]
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpCurveTail {
    /// Coin creator recorded on the curve (`None` on historical accounts).
    pub creator: Option<[u8; 32]>,
    /// Mayhem-mode flag (`None` when absent).
    pub is_mayhem_mode: Option<bool>,
}

/// Minimum `BondingCurve` length (the classic 49-byte layout) before any tail.
const CURVE_FIXED_LEN: usize = 49;

/// Decode the appended tail of a pump.fun `BondingCurve` account.
///
/// Verifies the `BondingCurve` discriminator and minimum length first (§18.2
/// fail-closed), then reads the sequential optional tail: `creator` Pubkey at
/// 49, `is_mayhem_mode` bool at 81. A 49-byte historical account yields
/// `Some(PumpCurveTail { creator: None, is_mayhem_mode: None })`.
pub fn decode_pump_curve_tail(account: &[u8]) -> Option<PumpCurveTail> {
    if account.get(0..8)? != registry::PUMPFUN_ACCOUNT_DISCRIMINATOR {
        return None;
    }
    if account.len() < CURVE_FIXED_LEN {
        return None;
    }
    let creator = read_pubkey(account, 49);
    let is_mayhem_mode = if creator.is_some() {
        read_bool(account, 81)
    } else {
        None
    };
    Some(PumpCurveTail {
        creator,
        is_mayhem_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic filler pubkey: 32 copies of `tag`.
    fn pk(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    /// Build a full current-layout Pool account of `len` bytes (>= 245).
    fn pool_fixture(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..8].copy_from_slice(&registry::PUMPSWAP_ACCOUNT_DISCRIMINATOR);
        b[8] = 254; // pool_bump
        b[9..11].copy_from_slice(&7u16.to_le_bytes());
        b[11..43].copy_from_slice(&pk(1));
        b[43..75].copy_from_slice(&pk(2));
        b[75..107].copy_from_slice(&pk(3));
        b[107..139].copy_from_slice(&pk(4));
        b[139..171].copy_from_slice(&pk(5));
        b[171..203].copy_from_slice(&pk(6));
        b[203..211].copy_from_slice(&123_456_789u64.to_le_bytes());
        if len >= 243 {
            b[211..243].copy_from_slice(&pk(9));
        }
        if len >= 244 {
            b[243] = 1;
        }
        if len >= 245 {
            b[244] = 0;
        }
        b
    }

    #[test]
    fn pool_full_current_length_decodes_every_field() {
        let p = decode_pool_account(&pool_fixture(245)).expect("decodes");
        assert_eq!(p.pool_bump, 254);
        assert_eq!(p.index, 7);
        assert_eq!(p.creator, pk(1));
        assert_eq!(p.base_mint, pk(2));
        assert_eq!(p.quote_mint, pk(3));
        assert_eq!(p.lp_mint, pk(4));
        assert_eq!(p.pool_base_token_account, pk(5));
        assert_eq!(p.pool_quote_token_account, pk(6));
        assert_eq!(p.lp_supply, 123_456_789);
        assert_eq!(p.coin_creator, Some(pk(9)));
        assert_eq!(p.is_mayhem_mode, Some(true));
        assert_eq!(p.is_cashback_coin, Some(false));
    }

    #[test]
    fn pool_historical_211_bytes_decodes_with_absent_tail() {
        let p = decode_pool_account(&pool_fixture(211)).expect("decodes");
        assert_eq!(p.lp_supply, 123_456_789);
        assert_eq!(p.coin_creator, None);
        assert_eq!(p.is_mayhem_mode, None);
        assert_eq!(p.is_cashback_coin, None);
    }

    #[test]
    fn pool_partial_tail_decodes_prefix_only() {
        // 243 bytes: coin_creator present, both flag bytes absent.
        let p = decode_pool_account(&pool_fixture(243)).expect("decodes");
        assert_eq!(p.coin_creator, Some(pk(9)));
        assert_eq!(p.is_mayhem_mode, None);
        assert_eq!(p.is_cashback_coin, None);
        // 244 bytes: mayhem present, cashback absent.
        let p = decode_pool_account(&pool_fixture(244)).expect("decodes");
        assert_eq!(p.is_mayhem_mode, Some(true));
        assert_eq!(p.is_cashback_coin, None);
    }

    #[test]
    fn pool_resized_300_bytes_zero_padded_decodes() {
        // extend_account zero-pads; trailing zeros beyond 245 are ignored.
        let p = decode_pool_account(&pool_fixture(300)).expect("decodes");
        assert_eq!(p.coin_creator, Some(pk(9)));
        assert_eq!(p.is_mayhem_mode, Some(true));
        assert_eq!(p.is_cashback_coin, Some(false));
    }

    #[test]
    fn pool_corrupt_tail_bool_stops_tail_not_decode() {
        let mut b = pool_fixture(245);
        b[243] = 7; // non-canonical bool
        let p = decode_pool_account(&b).expect("fixed prefix still decodes");
        assert_eq!(p.coin_creator, Some(pk(9)));
        assert_eq!(p.is_mayhem_mode, None, "corrupt bool treated as absent");
        assert_eq!(p.is_cashback_coin, None, "tail parsing stops at corruption");
    }

    #[test]
    fn pool_wrong_discriminator_rejected() {
        let mut b = pool_fixture(245);
        b[0] ^= 0x01;
        assert!(decode_pool_account(&b).is_none());
        let zero = vec![0u8; 245];
        assert!(decode_pool_account(&zero).is_none());
    }

    #[test]
    fn pool_truncated_mid_field_rejected() {
        let b = pool_fixture(245);
        for len in [0, 7, 8, 100, 202, 210] {
            assert!(decode_pool_account(&b[..len]).is_none(), "len {len}");
        }
    }

    /// Build a GlobalConfig fixture of `len` bytes.
    fn config_fixture(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..8].copy_from_slice(&GLOBAL_CONFIG_DISCRIMINATOR);
        b[8..40].copy_from_slice(&pk(11));
        b[40..48].copy_from_slice(&20u64.to_le_bytes()); // lp fee 20 bps
        b[48..56].copy_from_slice(&5u64.to_le_bytes()); // protocol fee 5 bps
        b[56] = 0b0000_0011;
        for i in 0..PROTOCOL_FEE_RECIPIENT_SLOTS {
            let o = 57 + 32 * i;
            b[o..o + 32].copy_from_slice(&pk(0x20 + i as u8));
        }
        if len >= 321 {
            b[313..321].copy_from_slice(&5u64.to_le_bytes());
        }
        if len >= 353 {
            b[321..353].copy_from_slice(&pk(0x40));
        }
        if len >= 385 {
            b[353..385].copy_from_slice(&pk(0x41));
        }
        if len >= 417 {
            b[385..417].copy_from_slice(&pk(0x42));
        }
        if len >= 418 {
            b[417] = 1;
        }
        if len >= 642 {
            for i in 0..RESERVED_FEE_RECIPIENT_SLOTS {
                let o = 418 + 32 * i;
                b[o..o + 32].copy_from_slice(&pk(0x50 + i as u8));
            }
        }
        if len >= 643 {
            b[642] = 1;
        }
        b
    }

    #[test]
    fn global_config_fixed_layout_decodes() {
        let c = decode_global_config(&config_fixture(GLOBAL_CONFIG_FIXED_LEN)).expect("decodes");
        assert_eq!(c.admin, pk(11));
        assert_eq!(c.lp_fee_basis_points, 20);
        assert_eq!(c.protocol_fee_basis_points, 5);
        assert_eq!(c.disable_flags, 0b0000_0011);
        assert_eq!(c.protocol_fee_recipients[0], pk(0x20));
        assert_eq!(c.protocol_fee_recipients[7], pk(0x27));
        assert_eq!(c.coin_creator_fee_basis_points, None);
        assert_eq!(c.reserved_fee_recipients, None);
        assert_eq!(c.is_cashback_enabled, None);
    }

    #[test]
    fn global_config_full_tail_decodes() {
        let c = decode_global_config(&config_fixture(643)).expect("decodes");
        assert_eq!(c.coin_creator_fee_basis_points, Some(5));
        assert_eq!(c.admin_set_coin_creator_authority, Some(pk(0x40)));
        assert_eq!(c.whitelist_pda, Some(pk(0x41)));
        assert_eq!(c.reserved_fee_recipient, Some(pk(0x42)));
        assert_eq!(c.mayhem_mode_enabled, Some(true));
        let reserved = c.reserved_fee_recipients.expect("all 7 present");
        assert_eq!(reserved[0], pk(0x50));
        assert_eq!(reserved[6], pk(0x56));
        assert_eq!(c.is_cashback_enabled, Some(true));
    }

    #[test]
    fn global_config_partial_tail_stops_cleanly() {
        // Ends right after mayhem_mode_enabled: reserved set + cashback absent.
        let c = decode_global_config(&config_fixture(418)).expect("decodes");
        assert_eq!(c.mayhem_mode_enabled, Some(true));
        assert_eq!(c.reserved_fee_recipients, None);
        assert_eq!(c.is_cashback_enabled, None);
    }

    #[test]
    fn global_config_wrong_disc_and_truncation_rejected() {
        let mut b = config_fixture(643);
        b[7] ^= 0xff;
        assert!(decode_global_config(&b).is_none());
        let good = config_fixture(643);
        for len in [0, 8, 56, 312] {
            assert!(decode_global_config(&good[..len]).is_none(), "len {len}");
        }
    }

    #[test]
    fn spl_token_amount_reads_offset_64() {
        // Standard 165-byte SPL token account.
        let mut b = vec![0u8; 165];
        b[64..72].copy_from_slice(&987_654_321u64.to_le_bytes());
        assert_eq!(decode_spl_token_amount(&b), Some(987_654_321));
        // Too short to carry the field.
        assert_eq!(decode_spl_token_amount(&b[..71]), None);
        assert_eq!(decode_spl_token_amount(&[]), None);
    }

    /// Build a BondingCurve fixture of `len` bytes.
    fn curve_fixture(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..8].copy_from_slice(&registry::PUMPFUN_ACCOUNT_DISCRIMINATOR);
        if len >= 81 {
            b[49..81].copy_from_slice(&pk(0x77));
        }
        if len >= 82 {
            b[81] = 1;
        }
        b
    }

    #[test]
    fn curve_tail_full_decodes() {
        let t = decode_pump_curve_tail(&curve_fixture(82)).expect("decodes");
        assert_eq!(t.creator, Some(pk(0x77)));
        assert_eq!(t.is_mayhem_mode, Some(true));
    }

    #[test]
    fn curve_tail_historical_49_bytes_absent() {
        let t = decode_pump_curve_tail(&curve_fixture(49)).expect("decodes");
        assert_eq!(t.creator, None);
        assert_eq!(t.is_mayhem_mode, None);
        // Creator present, mayhem byte absent.
        let t = decode_pump_curve_tail(&curve_fixture(81)).expect("decodes");
        assert_eq!(t.creator, Some(pk(0x77)));
        assert_eq!(t.is_mayhem_mode, None);
    }

    #[test]
    fn curve_tail_identity_and_bounds_enforced() {
        let mut b = curve_fixture(82);
        b[0] ^= 0x01;
        assert!(decode_pump_curve_tail(&b).is_none());
        assert!(decode_pump_curve_tail(&curve_fixture(48)).is_none());
    }

    #[test]
    fn constants_are_pinned() {
        // The GlobalConfig PDA must agree with the registry entry (§18.2).
        assert_eq!(
            PUMPSWAP_GLOBAL_CONFIG_PDA,
            registry::entry(registry::Venue::PumpSwap).config_pda
        );
        assert_eq!(
            PUMP_FEES_PROGRAM,
            "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ"
        );
        assert_eq!(WSOL_MINT, "So11111111111111111111111111111111111111112");
    }
}
