//! PumpSwap PRE-TRADE fee schedule: the `FeeConfig` account of the pump fee program and the tier rule the
//! program applies to it. This is the fee source available BEFORE our order — read from chain state, not
//! recovered from a trade's realised fee amounts.
//!
//! SOURCES (and nothing else):
//! * Layout: in-repo `pump_amm` IDL (`docs/missing_history_causal/milestone4/idl/pump_amm.json`):
//!   `FeeConfig { bump u8, admin pubkey, flat_fees Fees, fee_tiers Vec<FeeTier>, stable_fee_tiers
//!   Vec<FeeTier>, exotic_flat_fees Fees }`, `FeeTier { market_cap_lamports_threshold u128, fees Fees }`,
//!   `Fees { lp_fee_bps u64, protocol_fee_bps u64, creator_fee_bps u64 }`, discriminator
//!   `sha256("account:FeeConfig")[..8]`.
//! * Selection and rounding: official `@pump-fun/pump-swap-sdk` 1.20.0 (`sdk/fees.ts` `computeFeesBps` /
//!   `feesForQuoteMint` / `calculateFeeTier`, `sdk/util.ts` `poolMarketCap` / `fee` / `isPumpPool`,
//!   `sdk/sell.ts` `sellBaseInput`). Those files cite the Rust references (`pump-fees-math::
//!   calculate_fee_tier`, `pump-amm Pool::market_cap`). This is SDK source, not the deployed program.
//!
//! SCOPE (refusals are named, never priced):
//! * canonical pump pools (`pool.creator == pump_pool_authority(base_mint)`) quoted in a SOL-like mint
//!   -> `fee_tiers`; anything else -> [`FeeRefusal`].
//! * mayhem-mode pools, a configured per-pool creator rate, and a missing / zero coin_creator are refused:
//!   each changes the rate and none is decoded here.
//! * the market-cap basis is the base mint's LIVE supply, which the caller must read from the mint
//!   account; it is not assumed to be 1e15.

/// `sha256("account:FeeConfig")[..8]`.
pub const FEE_CONFIG_DISCRIMINATOR: [u8; 8] = [143, 52, 146, 187, 219, 123, 76, 155];

/// One leg's three rates in basis points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fees {
    pub lp_bps: u64,
    pub protocol_bps: u64,
    pub creator_bps: u64,
}

/// A tier: applies from `threshold` (SOL-denominated market cap, lamports) upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeTier {
    pub threshold: u128,
    pub fees: Fees,
}

/// Decoded `FeeConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeConfig {
    pub flat_fees: Fees,
    pub fee_tiers: Vec<FeeTier>,
    pub stable_fee_tiers: Vec<FeeTier>,
    /// `None` when the account predates the field (the SDK reads it as all-zero).
    pub exotic_flat_fees: Option<Fees>,
}

fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        d.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}
fn fees_at(d: &[u8], o: usize) -> Option<Fees> {
    Some(Fees {
        lp_bps: u64_at(d, o)?,
        protocol_bps: u64_at(d, o.checked_add(8)?)?,
        creator_bps: u64_at(d, o.checked_add(16)?)?,
    })
}
fn tiers_at(d: &[u8], mut o: usize) -> Option<(Vec<FeeTier>, usize)> {
    let n = u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?);
    o = o.checked_add(4)?;
    // A tier is 40 bytes; refuse a length the account cannot hold.
    if usize::try_from(n).ok()?.checked_mul(40)? > d.len().saturating_sub(o) {
        return None;
    }
    let mut v = Vec::new();
    for _ in 0..n {
        let lo = u64_at(d, o)?;
        let hi = u64_at(d, o.checked_add(8)?)?;
        v.push(FeeTier {
            threshold: u128::from(lo) | (u128::from(hi) << 64),
            fees: fees_at(d, o.checked_add(16)?)?,
        });
        o = o.checked_add(40)?;
    }
    Some((v, o))
}

/// Decode a `FeeConfig` account. `None` = wrong discriminator, truncated, or an unsound tier table
/// (empty, or thresholds not strictly increasing from 0).
#[must_use]
pub fn decode_fee_config(d: &[u8]) -> Option<FeeConfig> {
    if d.get(..8)? != FEE_CONFIG_DISCRIMINATOR {
        return None;
    }
    // 8 disc + 1 bump + 32 admin
    let flat_fees = fees_at(d, 41)?;
    let (fee_tiers, o) = tiers_at(d, 65)?;
    let (stable_fee_tiers, o) = tiers_at(d, o)?;
    let exotic = fees_at(d, o);
    let exotic_flat_fees = match exotic {
        Some(f) if f.lp_bps | f.protocol_bps | f.creator_bps != 0 => Some(f),
        _ => None,
    };
    let sound = fee_tiers.first().is_some_and(|t| t.threshold == 0)
        && fee_tiers
            .windows(2)
            .all(|w| w[0].threshold < w[1].threshold);
    sound.then_some(FeeConfig {
        flat_fees,
        fee_tiers,
        stable_fee_tiers,
        exotic_flat_fees,
    })
}

/// Inputs that select the schedule, each read from chain state BEFORE the order.
#[derive(Debug, Clone, Copy)]
pub struct PoolFeeInputs {
    /// `pool.creator == pump_pool_authority_pda(base_mint)` (caller derives the PDA).
    pub is_canonical_pump_pool: bool,
    /// Quote mint is WSOL / Token-2022 native / zero key.
    pub quote_is_sol_like: bool,
    pub is_mayhem_mode: bool,
    /// `Pool.creatorFeeBps` (0 = not configured). Nonzero is refused: the gate that applies it lives
    /// in GlobalConfig and is not decoded here.
    pub pool_creator_fee_bps: u64,
    /// `pool.coin_creator` is set (non-default). If unset the SDK charges no creator fee.
    pub coin_creator_set: bool,
    /// Live supply of the base mint (raw units), from the mint account.
    pub base_mint_supply: u128,
    pub base_reserve: u128,
    /// `quote_vault + virtual_quote_reserves`.
    pub effective_quote_reserve: u128,
}

/// Why no schedule is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeRefusal {
    NonCanonicalPool,
    NonSolQuote,
    MayhemMode,
    PoolCreatorFeeConfigured,
    ZeroBaseReserve,
    Overflow,
}

/// `Pool::market_cap` = `effective_quote * supply / base_reserve` (floor).
pub fn pool_market_cap(i: &PoolFeeInputs) -> Result<u128, FeeRefusal> {
    if i.base_reserve == 0 {
        return Err(FeeRefusal::ZeroBaseReserve);
    }
    i.effective_quote_reserve
        .checked_mul(i.base_mint_supply)
        .map(|x| x / i.base_reserve)
        .ok_or(FeeRefusal::Overflow)
}

/// `calculate_fee_tier`: below the first threshold -> first tier; else the highest tier whose threshold
/// is <= market cap.
#[must_use]
pub fn tier_for(tiers: &[FeeTier], mcap: u128) -> Option<Fees> {
    let first = tiers.first()?;
    if mcap < first.threshold {
        return Some(first.fees);
    }
    tiers
        .iter()
        .rev()
        .find(|t| mcap >= t.threshold)
        .map(|t| t.fees)
}

/// The rates this pool charges on a trade priced from `i`'s reserves. Creator rate is zeroed when the
/// coin creator is unset (SDK `sellBaseInput` / `buyQuoteInput`).
pub fn fees_for(cfg: &FeeConfig, i: &PoolFeeInputs) -> Result<Fees, FeeRefusal> {
    if !i.is_canonical_pump_pool {
        return Err(FeeRefusal::NonCanonicalPool);
    }
    if !i.quote_is_sol_like {
        return Err(FeeRefusal::NonSolQuote);
    }
    if i.is_mayhem_mode {
        return Err(FeeRefusal::MayhemMode);
    }
    if i.pool_creator_fee_bps != 0 {
        return Err(FeeRefusal::PoolCreatorFeeConfigured);
    }
    let mcap = pool_market_cap(i)?;
    let mut f = tier_for(&cfg.fee_tiers, mcap).ok_or(FeeRefusal::Overflow)?;
    if !i.coin_creator_set {
        f.creator_bps = 0;
    }
    Ok(f)
}

/// `ceil(amount * bps / 10_000)` (SDK `fee`).
#[must_use]
pub fn fee_ceil(amount: u128, bps: u64) -> Option<u128> {
    amount
        .checked_mul(u128::from(bps))
        .map(|x| x.div_ceil(10_000))
}

/// Net SOL to the seller of `base_in` (SDK `sellBaseInput.uiQuote`), plus the gross leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellQuote {
    pub gross: u128,
    pub lp_fee: u128,
    pub protocol_fee: u128,
    pub creator_fee: u128,
    pub net: u128,
}

/// Exact-in SELL: `gross = floor(eff * base_in / (base_reserve + base_in))`, each fee rounded up, net =
/// gross - fees. Refuses when the real vault cannot cover `gross - lp_fee` (SDK check).
#[must_use]
pub fn sell_net_quote(
    base_reserve: u128,
    quote_vault: u128,
    virtual_quote: u128,
    base_in: u128,
    f: Fees,
) -> Option<SellQuote> {
    let eff = quote_vault.checked_add(virtual_quote)?;
    let gross = eff
        .checked_mul(base_in)?
        .checked_div(base_reserve.checked_add(base_in)?)?;
    let lp_fee = fee_ceil(gross, f.lp_bps)?;
    let protocol_fee = fee_ceil(gross, f.protocol_bps)?;
    let creator_fee = fee_ceil(gross, f.creator_bps)?;
    if quote_vault < gross.checked_sub(lp_fee)? {
        return None;
    }
    let net = gross
        .checked_sub(lp_fee)?
        .checked_sub(protocol_fee)?
        .checked_sub(creator_fee)?;
    Some(SellQuote {
        gross,
        lp_fee,
        protocol_fee,
        creator_fee,
        net,
    })
}

/// SELL with the cashback component the current program withholds on cashback coins (`SellEvent.cashback`,
/// `cashback_fee_basis_points`): `net = gross - lp - protocol - creator - cashback`, each ceil-rounded, the
/// vault check as [`sell_net_quote`]. Exact on every captured sell layout (409/425/433, cashback and not):
/// `tests/pumpswap_event_layouts.rs`. The cashback is credited to the seller's claimable account, NOT paid
/// with the swap: it is not spendable proceeds until a separate claim reconciles.
#[must_use]
pub fn sell_net_quote_cb(
    base_reserve: u128,
    quote_vault: u128,
    virtual_quote: u128,
    base_in: u128,
    f: Fees,
    cashback_bps: u64,
) -> Option<(SellQuote, u128)> {
    let q = sell_net_quote(base_reserve, quote_vault, virtual_quote, base_in, f)?;
    let cb = fee_ceil(q.gross, cashback_bps)?;
    let net = q.net.checked_sub(cb)?;
    Some((SellQuote { net, ..q }, cb))
}
