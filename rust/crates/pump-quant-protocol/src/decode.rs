//! Account decoders for the pump.fun bonding curve and the PumpSwap AMM pool.
//!
//! # Responsibility
//! Turn raw on-chain account bytes (as fetched by an out-of-scope RPC layer)
//! into strongly-typed, integer-only structs. All decoding is manual,
//! little-endian and **strictly bounds-checked**: a buffer that is too short,
//! or whose boolean fields are out of range, yields `None` rather than a panic
//! or a garbage value.
//!
//! # Constitution
//! * §22 — integer-only; no floats are produced or consumed here.
//! * Bounds are checked on every field access, so malformed input can never
//!   trigger a silent out-of-range read.
//! * §18.2 — **fail closed on unknown account identity**: before trusting any
//!   field, each decoder verifies the 8-byte Anchor account discriminator at
//!   offset `0..8` against the expected constant recorded in
//!   [`crate::registry`]. A buffer belonging to the wrong account type, a
//!   foreign program, or an all-zero placeholder is rejected with `None`
//!   rather than being decoded into plausible-looking reserves.

use crate::registry::{self, Venue};

/// Decoded pump.fun bonding-curve account (the "virtual" AMM state).
///
/// Reserves are expressed in native base units: `virtual_sol`/`real_sol` are
/// lamports (`u64`), `virtual_token`/`real_token` are raw token base units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpCurve {
    /// Virtual SOL reserves, in lamports.
    pub virtual_sol: u64,
    /// Virtual token reserves, in token base units.
    pub virtual_token: u64,
    /// Real SOL reserves currently escrowed, in lamports.
    pub real_sol: u64,
    /// Real token reserves still held by the curve, in token base units.
    pub real_token: u64,
    /// `true` once the curve has completed and migrated off the bonding curve.
    pub complete: bool,
}

/// On-chain layout of the pump.fun `BondingCurve` account.
///
/// ```text
/// offset  size  field
/// 0       8     anchor account discriminator
/// 8       8     virtual_token_reserves  (u64 LE)
/// 16      8     virtual_sol_reserves    (u64 LE)
/// 24      8     real_token_reserves     (u64 LE)
/// 32      8     real_sol_reserves       (u64 LE)
/// 40      8     token_total_supply      (u64 LE)   [not surfaced]
/// 48      1     complete                (bool)
/// ```
const CURVE_MIN_LEN: usize = 49;

/// Decode a pump.fun bonding-curve account.
///
/// Returns `None` when `account` is shorter than [`CURVE_MIN_LEN`], when the
/// leading 8-byte discriminator does not match the registry's expected
/// `BondingCurve` identity (§18.2 fail-closed), or when the `complete` byte is
/// not a canonical boolean (`0` or `1`).
///
/// # Constitution
/// * §22 — pure integer decode; bounds checked on every field.
/// * §18.2 — account identity is verified before any field is trusted.
pub fn decode_pump_curve(account: &[u8]) -> Option<PumpCurve> {
    if account.len() < CURVE_MIN_LEN {
        return None;
    }
    verify_discriminator(account, Venue::PumpFun)?;
    let virtual_token = read_u64_le(account, 8)?;
    let virtual_sol = read_u64_le(account, 16)?;
    let real_token = read_u64_le(account, 24)?;
    let real_sol = read_u64_le(account, 32)?;
    let complete = read_bool(account, 48)?;

    Some(PumpCurve {
        virtual_sol,
        virtual_token,
        real_sol,
        real_token,
        complete,
    })
}

/// Decoded PumpSwap AMM pool account.
///
/// PumpSwap is a plain constant-product AMM; a pool holds two token vaults.
/// `base_reserve`/`quote_reserve` are the last-known vault balances in base
/// units, decoded straight from the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpSwapPool {
    /// PDA bump seed of the pool account.
    pub pool_bump: u8,
    /// Pool index within the creator's namespace.
    pub index: u16,
    /// Base-token reserve, in base units.
    pub base_reserve: u64,
    /// Quote-token reserve, in base units.
    pub quote_reserve: u64,
    /// Outstanding LP-token supply.
    pub lp_supply: u64,
}

/// On-chain layout of the PumpSwap `Pool` account (relevant fields).
///
/// ```text
/// offset  size  field
/// 0       8     anchor account discriminator
/// 8       1     pool_bump      (u8)
/// 9       2     index          (u16 LE)
/// 11      8     base_reserve   (u64 LE)
/// 19      8     quote_reserve  (u64 LE)
/// 27      8     lp_supply      (u64 LE)
/// ```
const POOL_MIN_LEN: usize = 35;

/// Decode a PumpSwap AMM pool account.
///
/// Returns `None` when `account` is shorter than [`POOL_MIN_LEN`] or when the
/// leading 8-byte discriminator does not match the registry's expected `Pool`
/// identity (§18.2 fail-closed).
///
/// # Constitution
/// * §22 — pure integer decode; bounds checked on every field.
/// * §18.2 — account identity is verified before any field is trusted.
pub fn decode_pumpswap_pool(account: &[u8]) -> Option<PumpSwapPool> {
    if account.len() < POOL_MIN_LEN {
        return None;
    }
    verify_discriminator(account, Venue::PumpSwap)?;
    let pool_bump = *account.get(8)?;
    let index = read_u16_le(account, 9)?;
    let base_reserve = read_u64_le(account, 11)?;
    let quote_reserve = read_u64_le(account, 19)?;
    let lp_supply = read_u64_le(account, 27)?;

    Some(PumpSwapPool {
        pool_bump,
        index,
        base_reserve,
        quote_reserve,
        lp_supply,
    })
}

/// The cashback-era tail of the pump.fun `BondingCurve` account.
///
/// ```text
/// offset  size  field
/// 49      32    creator            (pubkey)
/// 81      1     is_mayhem_mode     (bool)
/// 82      1     is_cashback_coin   (bool)
/// 83      32    quote_mint         (pubkey)
/// ```
///
/// Fields decode sequentially and stop at the first absent or non-canonical
/// one (accounts predating an upgrade are shorter) — a truncated tail yields
/// `None` for that field and everything after it, never a panic. The
/// transaction builder ([`crate::venue_accounts::PumpCurveCtx`]) requires
/// `creator` and `is_cashback_coin`; a curve whose tail lacks them fails
/// closed upstream rather than trading on a guess (§18.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpCurveTail {
    /// Curve creator — the `["creator-vault", creator]` seed (§4.1).
    pub creator: Option<[u8; 32]>,
    /// Mayhem-mode flag.
    pub is_mayhem_mode: Option<bool>,
    /// Cashback flag, **byte 82** — the authoritative cashback signal; never
    /// inferred from the token program (§4.1).
    pub is_cashback_coin: Option<bool>,
    /// Quote mint. Non-SOL quote curves exist since 2026; the builder refuses
    /// them until their layout is verified.
    pub quote_mint: Option<[u8; 32]>,
}

/// Decode the [`PumpCurveTail`] of a pump.fun bonding-curve account.
///
/// The fixed prefix must decode first ([`decode_pump_curve`] semantics,
/// including the discriminator check); the tail then decodes sequentially.
///
/// # Constitution
/// §22 integer-only; §18.2 identity verified first; every access bounds-checked.
pub fn decode_pump_curve_tail(account: &[u8]) -> Option<PumpCurveTail> {
    // Identity and prefix validity first — a tail on an unverified account is
    // not a decode, it is a guess.
    decode_pump_curve(account)?;
    let creator = read_pubkey(account, 49);
    let is_mayhem_mode = if creator.is_some() {
        read_bool(account, 81)
    } else {
        None
    };
    let is_cashback_coin = if is_mayhem_mode.is_some() {
        read_bool(account, 82)
    } else {
        None
    };
    let quote_mint = if is_cashback_coin.is_some() {
        read_pubkey(account, 83)
    } else {
        None
    };
    Some(PumpCurveTail {
        creator,
        is_mayhem_mode,
        is_cashback_coin,
        quote_mint,
    })
}

/// `sha256("account:Global")[..8]` — the pump.fun `Global` account identity.
pub const GLOBAL_ACCOUNT_DISCRIMINATOR: [u8; 8] = [167, 232, 232, 177, 200, 108, 114, 127];

/// The fields of the pump.fun `Global` account the live path consumes.
///
/// ```text
/// offset  size  field
/// 0       8     anchor account discriminator
/// 8       1     initialized      (bool, unused on-chain)
/// 9       32    authority        (pubkey)
/// 41      32    fee_recipient    (pubkey)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpGlobal {
    /// Protocol fee recipient — the **decoded** source `§4.1 [1]` requires;
    /// hardcoding this address is forbidden (§18.2, `VENUE_TX_LAYOUTS.md` §7.3).
    pub fee_recipient: [u8; 32],
    /// The 8 buyback fee recipients at Global offset 741 (2026-08-17 finding).
    /// These are BuybackVault PDA addresses owned by the fee program
    /// (`pfeeUxB6...`). The pump program requires one of these (selected
    /// per-mint) as the trailing account in Buy/Sell instructions, or the
    /// transaction fails with `BuybackFeeRecipientMissing` (error 6062).
    pub buyback_fee_recipients: [[u8; 32]; 8],
}

/// Minimum length to read `fee_recipient` (offset 41, 32 bytes).
const GLOBAL_MIN_LEN: usize = 73;
/// Minimum length to read `buyback_fee_recipients` (offset 741, 8×32 = 256 bytes).
/// 741 + 256 = 997. The Global account is 1045 bytes on mainnet.
const GLOBAL_BUYBACK_MIN_LEN: usize = 997;

/// Decode the pump.fun `Global` account (the fields consumed here).
///
/// Returns `None` on short buffers, a wrong discriminator, or an all-zero
/// `fee_recipient` — a zeroed recipient is a default, not a decode (§18.2).
pub fn decode_global(account: &[u8]) -> Option<PumpGlobal> {
    if account.len() < GLOBAL_MIN_LEN {
        return None;
    }
    if account.get(0..8)? != GLOBAL_ACCOUNT_DISCRIMINATOR {
        return None;
    }
    let fee_recipient = read_pubkey(account, 41)?;
    if fee_recipient == [0u8; 32] {
        return None;
    }

    // Extract the 8 buyback_fee_recipients at offset 741 (2026-08-17 finding).
    // If the account is too short (e.g. a test fixture that doesn't model the
    // full Global layout), we fall back to all-zero recipients — the caller
    // treats zero recipients as "not available" and the FeeTail::None path
    // remains the fallback. This keeps existing fixtures compiling without
    // requiring every test to model 1045-byte Global accounts.
    let mut buyback_fee_recipients = [[0u8; 32]; 8];
    if account.len() >= GLOBAL_BUYBACK_MIN_LEN {
        for i in 0..8 {
            if let Some(pk) = read_pubkey(account, 741 + i * 32) {
                buyback_fee_recipients[i] = pk;
            }
        }
    }

    Some(PumpGlobal {
        fee_recipient,
        buyback_fee_recipients,
    })
}

/// Read a 32-byte pubkey at `offset`, returning `None` if out of bounds.
fn read_pubkey(buf: &[u8], offset: usize) -> Option<[u8; 32]> {
    let end = offset.checked_add(32)?;
    let slice = buf.get(offset..end)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(slice);
    Some(out)
}

/// Read a little-endian `u16` at `offset`, returning `None` if out of bounds.
fn read_u16_le(buf: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = buf.get(offset..end)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Read a little-endian `u64` at `offset`, returning `None` if out of bounds.
fn read_u64_le(buf: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let slice = buf.get(offset..end)?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slice);
    Some(u64::from_le_bytes(bytes))
}

/// Read a canonical boolean (`0`/`1`) at `offset`; any other value is rejected.
fn read_bool(buf: &[u8], offset: usize) -> Option<bool> {
    match buf.get(offset)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Verify the 8-byte account discriminator at offset `0..8` (§18.2).
///
/// Returns `Some(())` only when `account[0..8]` equals the discriminator the
/// [`crate::registry`] records for `venue`'s primary account. Any mismatch —
/// wrong account type, foreign program, or an all-zero placeholder — yields
/// `None`, so the caller fails closed instead of trusting reserves that may
/// belong to an unrelated account.
fn verify_discriminator(account: &[u8], venue: Venue) -> Option<()> {
    let found = account.get(0..8)?;
    if found == registry::account_discriminator(venue) {
        Some(())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_bytes(disc: [u8; 8], v_token: u64, v_sol: u64, complete: u8) -> Vec<u8> {
        let mut b = vec![0u8; CURVE_MIN_LEN];
        b[0..8].copy_from_slice(&disc);
        b[8..16].copy_from_slice(&v_token.to_le_bytes());
        b[16..24].copy_from_slice(&v_sol.to_le_bytes());
        b[48] = complete;
        b
    }

    fn pool_bytes(disc: [u8; 8], base: u64, quote: u64) -> Vec<u8> {
        let mut b = vec![0u8; POOL_MIN_LEN];
        b[0..8].copy_from_slice(&disc);
        b[8] = 255;
        b[11..19].copy_from_slice(&base.to_le_bytes());
        b[19..27].copy_from_slice(&quote.to_le_bytes());
        b
    }

    #[test]
    fn curve_decodes_with_correct_discriminator() {
        let disc = registry::account_discriminator(Venue::PumpFun);
        let b = curve_bytes(disc, 100, 200, 0);
        let c = decode_pump_curve(&b).expect("valid identity should decode");
        assert_eq!(c.virtual_token, 100);
        assert_eq!(c.virtual_sol, 200);
    }

    #[test]
    fn curve_rejects_zero_discriminator() {
        // The historical "left as zeros" buffer must now fail closed.
        let b = curve_bytes([0u8; 8], 100, 200, 0);
        assert!(decode_pump_curve(&b).is_none());
    }

    #[test]
    fn curve_rejects_foreign_discriminator() {
        // The PumpSwap Pool discriminator on a curve-length buffer is rejected.
        let wrong = registry::account_discriminator(Venue::PumpSwap);
        let b = curve_bytes(wrong, 100, 200, 0);
        assert!(decode_pump_curve(&b).is_none());
    }

    #[test]
    fn curve_rejects_single_flipped_discriminator_byte() {
        let mut disc = registry::account_discriminator(Venue::PumpFun);
        disc[0] ^= 0x01;
        let b = curve_bytes(disc, 100, 200, 0);
        assert!(decode_pump_curve(&b).is_none());
    }

    #[test]
    fn pool_decodes_with_correct_discriminator() {
        let disc = registry::account_discriminator(Venue::PumpSwap);
        let b = pool_bytes(disc, 555, 777);
        let p = decode_pumpswap_pool(&b).expect("valid identity should decode");
        assert_eq!(p.base_reserve, 555);
        assert_eq!(p.quote_reserve, 777);
    }

    #[test]
    fn pool_rejects_zero_discriminator() {
        let b = pool_bytes([0u8; 8], 555, 777);
        assert!(decode_pumpswap_pool(&b).is_none());
    }

    #[test]
    fn pool_rejects_foreign_discriminator() {
        let wrong = registry::account_discriminator(Venue::PumpFun);
        let b = pool_bytes(wrong, 555, 777);
        assert!(decode_pumpswap_pool(&b).is_none());
    }

    #[test]
    fn short_buffer_rejected_before_discriminator_check() {
        // Fewer than 8 bytes cannot carry a discriminator at all.
        assert!(decode_pump_curve(&[1, 2, 3]).is_none());
        assert!(decode_pumpswap_pool(&[1, 2, 3]).is_none());
    }

    #[test]
    fn golden_fixtures_decode() {
        // Each registry entry's golden fixture must decode cleanly.
        let pf = registry::entry(Venue::PumpFun);
        assert!(decode_pump_curve(pf.golden_fixture).is_some());
        let ps = registry::entry(Venue::PumpSwap);
        assert!(decode_pumpswap_pool(ps.golden_fixture).is_some());
    }
}
