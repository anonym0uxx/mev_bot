//! pump.fun bonding-curve SELL quote from PRE-TRADE chain state: the curve account plus the pump-fees
//! `FeeConfig` (`["fee_config", PUMP_PROGRAM_ID]` under the fee program). Size-specific: the gross is
//! the constant-product walk for exactly `tokens_in`, never spot x quantity.
//!
//! SOURCES: official `@pump-fun/pump-sdk` 2.0.0 (`bondingCurve.ts` `getSellSolAmountFromTokenAmount`,
//! `bondingCurveMarketCap`; `fees.ts` `getFee` / `computeFeesBps` / `calculateFeeTier` / `fee`), and the
//! in-repo pump IDL `BondingCurve` layout. SDK source, not the deployed program.
//!
//! Arithmetic (SDK): `gross = floor(tokens * vsol / (vtok + tokens))`; tier by
//! `mcap = floor(vsol * 1e15 / vtok)` (non-mayhem basis); `protocol = ceil(gross * protocol_bps / 1e4)`;
//! `creator = ceil(gross * creator_bps / 1e4)` iff the curve creator is set; `net = gross - both`.
//! On cashback coins the creator share is paid as a claimable seller rebate instead of to the
//! creator; it is still withheld from the immediate proceeds, so `net` is unchanged.
//!
//! REFUSALS (named, never priced): complete/migrated curve, mayhem mode, non-SOL quote, a configured
//! per-curve creator rate, holder-reward coins, a size the curve's REAL SOL cannot pay. The last is
//! this module's fail-closed reading of `real_sol_reserves` (= `vsol - initial virtual`); the SDK does
//! not check it and the program's behaviour for such a sell is not established here.

use crate::decode::{decode_pump_curve, decode_pump_curve_tail};
use crate::pumpswap_fees::{fee_ceil, tier_for, FeeConfig, Fees};

/// pump.fun non-mayhem market-cap supply basis (`ONE_BILLION_SUPPLY`, raw units).
pub const CURVE_MCAP_SUPPLY: u128 = 1_000_000_000_000_000;

/// Decoded fields a curve sell quote needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveSellState {
    pub virtual_sol: u64,
    pub virtual_token: u64,
    pub real_sol: u64,
    pub complete: bool,
    pub creator_set: bool,
    pub mayhem: bool,
    pub cashback: bool,
    pub quote_is_sol: bool,
    pub creator_fee_bps: u64,
    pub holder_reward: bool,
}

/// Decode a full-length (>= 125 byte) curve account; shorter layouts refuse (fields unknown).
#[must_use]
pub fn decode_curve_sell_state(acct: &[u8]) -> Option<CurveSellState> {
    let c = decode_pump_curve(acct)?;
    let t = decode_pump_curve_tail(acct)?;
    let creator = t.creator?;
    let quote = t.quote_mint?;
    let creator_fee_bps = u64::from_le_bytes(acct.get(115..123)?.try_into().ok()?);
    let holder_reward = match *acct.get(124)? {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some(CurveSellState {
        virtual_sol: c.virtual_sol,
        virtual_token: c.virtual_token,
        real_sol: c.real_sol,
        complete: c.complete,
        creator_set: creator != [0u8; 32],
        mayhem: t.is_mayhem_mode?,
        cashback: t.is_cashback_coin?,
        // SOL curves store the zero key (SDK `PublicKey.default`).
        quote_is_sol: quote == [0u8; 32],
        creator_fee_bps,
        holder_reward,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveSellRefusal {
    CurveComplete,
    MayhemMode,
    NonSolQuote,
    CurveCreatorFeeConfigured,
    HolderRewardCoin,
    ZeroQuantity,
    /// The gross exceeds the curve's real SOL. Carries the largest size it can pay.
    RealSolInsufficient {
        max_tokens: u64,
    },
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveSellQuote {
    pub gross: u128,
    pub fees: Fees,
    pub protocol_fee: u128,
    /// Creator share (paid to the cashback rebate on cashback coins); withheld either way.
    pub creator_fee: u128,
    pub net: u128,
    pub mcap_lamports: u128,
}

fn gross_out(s: &CurveSellState, tokens: u128) -> Option<u128> {
    tokens
        .checked_mul(u128::from(s.virtual_sol))?
        .checked_div(u128::from(s.virtual_token).checked_add(tokens)?)
}

/// Largest `tokens` whose (floored) gross the curve's real SOL can pay.
///
/// `floor(t*vs/(vt+t)) <= rs  <=>  t*vs < (rs+1)*(vt+t)  <=>  t*(vs-rs-1) < (rs+1)*vt`, so the
/// maximum is `ceil((rs+1)*vt / (vs-rs-1)) - 1`. Verified against the walk before returning.
#[must_use]
pub fn max_sellable_tokens(s: &CurveSellState) -> Option<u64> {
    let (vs, vt, rs) = (
        u128::from(s.virtual_sol),
        u128::from(s.virtual_token),
        u128::from(s.real_sol),
    );
    if rs.checked_add(1)? >= vs {
        return Some(u64::MAX);
    }
    let t = rs
        .checked_add(1)?
        .checked_mul(vt)?
        .div_ceil(vs - rs - 1)
        .checked_sub(1)?;
    let ok = gross_out(s, t)? <= rs && gross_out(s, t.checked_add(1)?)? > rs;
    if !ok {
        return None;
    }
    u64::try_from(t).ok()
}

/// Exact-in SELL of `tokens_in` against the decoded curve and pump-fees `FeeConfig`.
pub fn curve_sell_quote(
    s: &CurveSellState,
    cfg: &FeeConfig,
    tokens_in: u64,
) -> Result<CurveSellQuote, CurveSellRefusal> {
    if s.complete || s.virtual_token == 0 {
        return Err(CurveSellRefusal::CurveComplete);
    }
    if s.mayhem {
        return Err(CurveSellRefusal::MayhemMode);
    }
    if !s.quote_is_sol {
        return Err(CurveSellRefusal::NonSolQuote);
    }
    if s.creator_fee_bps != 0 {
        return Err(CurveSellRefusal::CurveCreatorFeeConfigured);
    }
    if s.holder_reward {
        return Err(CurveSellRefusal::HolderRewardCoin);
    }
    if tokens_in == 0 {
        return Err(CurveSellRefusal::ZeroQuantity);
    }
    let gross = gross_out(s, u128::from(tokens_in)).ok_or(CurveSellRefusal::Overflow)?;
    if gross > u128::from(s.real_sol) {
        return Err(CurveSellRefusal::RealSolInsufficient {
            max_tokens: max_sellable_tokens(s).ok_or(CurveSellRefusal::Overflow)?,
        });
    }
    let mcap = u128::from(s.virtual_sol)
        .checked_mul(CURVE_MCAP_SUPPLY)
        .and_then(|x| x.checked_div(u128::from(s.virtual_token)))
        .ok_or(CurveSellRefusal::Overflow)?;
    let fees = tier_for(&cfg.fee_tiers, mcap).ok_or(CurveSellRefusal::Overflow)?;
    let protocol_fee = fee_ceil(gross, fees.protocol_bps).ok_or(CurveSellRefusal::Overflow)?;
    let creator_fee = if s.creator_set {
        fee_ceil(gross, fees.creator_bps).ok_or(CurveSellRefusal::Overflow)?
    } else {
        0
    };
    let net = gross
        .checked_sub(protocol_fee)
        .and_then(|x| x.checked_sub(creator_fee))
        .ok_or(CurveSellRefusal::Overflow)?;
    Ok(CurveSellQuote {
        gross,
        fees,
        protocol_fee,
        creator_fee,
        net,
        mcap_lamports: mcap,
    })
}
