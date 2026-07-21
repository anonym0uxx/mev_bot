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
/// Returns `None` when `account` is shorter than [`CURVE_MIN_LEN`] or when the
/// `complete` byte is not a canonical boolean (`0` or `1`).
///
/// # Constitution
/// §22 — pure integer decode; bounds checked on every field.
pub fn decode_pump_curve(account: &[u8]) -> Option<PumpCurve> {
    if account.len() < CURVE_MIN_LEN {
        return None;
    }
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
/// Returns `None` when `account` is shorter than [`POOL_MIN_LEN`].
///
/// # Constitution
/// §22 — pure integer decode; bounds checked on every field.
pub fn decode_pumpswap_pool(account: &[u8]) -> Option<PumpSwapPool> {
    if account.len() < POOL_MIN_LEN {
        return None;
    }
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
