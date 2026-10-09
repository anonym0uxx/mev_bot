//! M3 — the paper executor's ONE source of size-specific execution economics.
//!
//! Every model-path fill (BUY entry, ADD, REDUCE, EXIT, protective sell) is priced here, against the
//! causal LANDING state the caller selected, for the exact quantity the order carries. Nothing here
//! chooses a size: a quote that cannot be honoured at the requested size is a NAMED refusal, never a
//! resized fill.
//!
//! COSTS ARE BOOKED ONCE, EACH IN ITS OWN BUCKET:
//! * venue fees — taken by the program inside the swap: curve protocol + creator/cashback, pool
//!   lp + protocol + creator (+ cashback). Rates come from the applicable fee configuration
//!   (curve: the pump-fees `FeeConfig` account; pool: the landing swap event's own fee fields).
//!   For a BUY they are INSIDE the all-in spend (exact-SOL-in), for a SELL they come out of gross.
//! * network fee — the landed transaction's base + priority fee: [`NETWORK_FEE_P50_LAMPORTS`],
//!   the measured p50 of `meta.fee` over 91,735 of our own fills (`FEE_QUANTILES_C16.json`). That
//!   measurement is the CHARGED transaction fee; it does not contain a tip (a tip is a separate
//!   transfer instruction), so the tip is booked separately. p50 is a typical leg, NOT a ceiling
//!   (p90 45,000; p99 1,005,000 are the scenario values).
//! * tip — the configured per-leg tip (`entry_tip_lamports` / `exit_tip_lamports`).
//!
//! Cashback (curve or pool) is withheld from the trade and credited to a claimable account: it is
//! a cost of the trade and is NOT spendable proceeds until a separate claim reconciles it.
#![forbid(unsafe_code)]

use pump_quant_protocol::curve_sell_quote::{
    curve_buy_exact_in, curve_sell_from_reserves, CurveSellRefusal,
};
use pump_quant_protocol::pumpswap_fees::{decode_fee_config, sell_net_quote, Fees};

/// Measured p50 network+priority fee of one landed leg (`FEE_QUANTILES_C16.json`, `meta.fee`).
pub const NETWORK_FEE_P50_LAMPORTS: u64 = 10_000;

/// The pump-fees program `FeeConfig` (`8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt`), captured
/// read-only at slot 454,754,100 (`proc/m3_raw_accounts.json`). One tier: protocol 95 bp,
/// creator 30 bp. The same 95 + 30 (creator OR cashback) was charged on 100 of 100 independent curve
/// trades in the bF2 tape window (slots 445,637,000..445,638,600, `m3_hist_curve_fee_evidence.json`)
/// and reproduced exactly (ceil per component) on the 2026-10 vectors. Between those two evidence
/// points the schedule is ASSUMED unchanged; a paper run outside them must re-read the account.
pub const CURVE_FEE_CONFIG_BYTES: &[u8] = include_bytes!("curve_fee_config_454754100.bin");
/// Slot the pinned curve `FeeConfig` was read at.
pub const CURVE_FEE_CONFIG_SLOT: u64 = 454_754_100;

/// Curve venue fee rates (protocol, creator-or-cashback) in bp, from the pinned `FeeConfig`.
#[must_use]
pub fn curve_fees() -> Option<(u64, u64)> {
    let cfg = decode_fee_config(CURVE_FEE_CONFIG_BYTES)?;
    // A single-tier schedule: the rate does not depend on market cap. A multi-tier account would
    // need the tier at the landing reserves; refuse rather than guess.
    if cfg.fee_tiers.len() != 1 {
        return None;
    }
    let f = cfg.fee_tiers[0].fees;
    if f.lp_bps != 0 {
        return None;
    }
    Some((f.protocol_bps, f.creator_bps))
}

/// One executed BUY leg: all-in venue spend (fees inside), the fee components, tokens delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyQuote {
    /// All-in venue spend (what leaves the wallet for the swap), lamports.
    pub spend: u64,
    /// The part of `spend` that bought tokens.
    pub net_in: u64,
    /// Venue fees inside `spend` (protocol + creator/cashback, or lp + protocol + creator).
    pub venue_fees: u64,
    /// Tokens delivered.
    pub tokens: u64,
}

/// One executed SELL leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellQuote {
    /// Gross quote out before venue fees.
    pub gross: u64,
    /// Venue fees taken out of gross (incl. any cashback withheld).
    pub venue_fees: u64,
    /// Of `venue_fees`, the cashback credited to a claimable account (not spendable).
    pub cashback_withheld: u64,
    /// Venue-net proceeds credited by the swap.
    pub net: u64,
}

/// Why a quote is unavailable. Every variant is a named outcome; none is a zero or a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteRefusal {
    /// The pinned curve fee configuration could not be decoded / is not single-tier.
    CurveFeeConfigUnsupported,
    /// The curve's real SOL cannot pay the gross for this size (the program refuses it).
    CurveRealSolInsufficient { max_tokens: u64 },
    /// The curve is complete (migrated) — no curve route.
    CurveComplete,
    /// Pool event carries no coin-creator fee: the coin may be a cashback coin whose cashback rate
    /// the engine does not receive. Refused rather than assumed zero.
    AmmCashbackUnknown,
    /// Pool economics (fee parts / virtual quote) missing on the landing event.
    AmmEconomicsMissing,
    /// The pool's real quote vault cannot cover the sell.
    AmmVaultInsufficient,
    /// Size too small to move a whole unit, or arithmetic out of range.
    Unpriceable,
}

impl QuoteRefusal {
    /// Stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CurveFeeConfigUnsupported => "quote_unavailable:curve_fee_config_unsupported",
            Self::CurveRealSolInsufficient { .. } => {
                "quote_unavailable:curve_real_sol_insufficient"
            }
            Self::CurveComplete => "quote_unavailable:curve_complete",
            Self::AmmCashbackUnknown => "quote_unavailable:amm_cashback_unknown",
            Self::AmmEconomicsMissing => "quote_unavailable:amm_economics_missing",
            Self::AmmVaultInsufficient => "quote_unavailable:amm_vault_insufficient",
            Self::Unpriceable => "quote_unavailable:unpriceable",
        }
    }
}

/// Curve BUY with an all-in spend of `spend` lamports (`buy_exact_sol_in` arithmetic).
pub fn curve_buy(
    vsol: u64,
    vtok: u64,
    real_tok: u64,
    spend: u64,
) -> Result<BuyQuote, QuoteRefusal> {
    if vtok == 0 {
        return Err(QuoteRefusal::CurveComplete);
    }
    let (p, c) = curve_fees().ok_or(QuoteRefusal::CurveFeeConfigUnsupported)?;
    let q =
        curve_buy_exact_in(vsol, vtok, real_tok, spend, p, c).ok_or(QuoteRefusal::Unpriceable)?;
    let fees = q.protocol_fee + q.creator_fee;
    let used = q.net_in + fees;
    Ok(BuyQuote {
        spend: u64::try_from(used).map_err(|_| QuoteRefusal::Unpriceable)?,
        net_in: u64::try_from(q.net_in).map_err(|_| QuoteRefusal::Unpriceable)?,
        venue_fees: u64::try_from(fees).map_err(|_| QuoteRefusal::Unpriceable)?,
        tokens: u64::try_from(q.tokens_out).map_err(|_| QuoteRefusal::Unpriceable)?,
    })
}

/// Curve SELL of exactly `tokens` against landing reserves incl. the curve's REAL SOL.
pub fn curve_sell(
    vsol: u64,
    vtok: u64,
    real_sol: u64,
    tokens: u64,
) -> Result<SellQuote, QuoteRefusal> {
    let (p, c) = curve_fees().ok_or(QuoteRefusal::CurveFeeConfigUnsupported)?;
    let f = Fees {
        lp_bps: 0,
        protocol_bps: p,
        creator_bps: c,
    };
    match curve_sell_from_reserves(vsol, vtok, real_sol, tokens, f) {
        Ok(q) => {
            let fees = q.protocol_fee + q.creator_fee;
            Ok(SellQuote {
                gross: u64::try_from(q.gross).map_err(|_| QuoteRefusal::Unpriceable)?,
                venue_fees: u64::try_from(fees).map_err(|_| QuoteRefusal::Unpriceable)?,
                // Creator vs cashback recipient is not observable from reserves; the 30 bp leaves the
                // seller either way. A cashback credit is never counted as proceeds here.
                cashback_withheld: 0,
                net: u64::try_from(q.net).map_err(|_| QuoteRefusal::Unpriceable)?,
            })
        }
        Err(CurveSellRefusal::RealSolInsufficient { max_tokens }) => {
            Err(QuoteRefusal::CurveRealSolInsufficient { max_tokens })
        }
        Err(CurveSellRefusal::CurveComplete) => Err(QuoteRefusal::CurveComplete),
        Err(_) => Err(QuoteRefusal::Unpriceable),
    }
}

/// Pool fee parts from the landing event. A zero coin-creator fee is refused: on every captured
/// current event (27/27) creator fee and cashback are mutually exclusive, so `cr == 0` means the
/// cashback rate is unknown to the engine (the event format does not carry it through).
fn amm_parts(parts: Option<(u32, u32, u32)>) -> Result<Fees, QuoteRefusal> {
    let (lp, pr, cr) = parts.ok_or(QuoteRefusal::AmmEconomicsMissing)?;
    if cr == 0 {
        return Err(QuoteRefusal::AmmCashbackUnknown);
    }
    Ok(Fees {
        lp_bps: u64::from(lp),
        protocol_bps: u64::from(pr),
        creator_bps: u64::from(cr),
    })
}

/// Pool BUY with an all-in spend (`buy_exact_quote_in`, verified on independent transactions).
pub fn amm_buy(
    base: u64,
    quote: u64,
    vq: Option<u64>,
    parts: Option<(u32, u32, u32)>,
    spend: u64,
) -> Result<BuyQuote, QuoteRefusal> {
    let f = amm_parts(parts)?;
    let vq = vq.ok_or(QuoteRefusal::AmmEconomicsMissing)?;
    let q = pump_quant_protocol::pumpswap_event::buy_exact_quote_in(
        u128::from(base),
        u128::from(quote),
        u128::from(vq),
        u128::from(spend),
        u128::from(f.lp_bps),
        u128::from(f.protocol_bps),
        u128::from(f.creator_bps),
    )
    .ok_or(QuoteRefusal::Unpriceable)?;
    let net_in = u64::try_from(q.net_quote_in).map_err(|_| QuoteRefusal::Unpriceable)?;
    let fee = |bps: u64| (u128::from(net_in) * u128::from(bps)).div_ceil(10_000);
    let fees = fee(f.lp_bps) + fee(f.protocol_bps) + fee(f.creator_bps);
    let fees = u64::try_from(fees).map_err(|_| QuoteRefusal::Unpriceable)?;
    Ok(BuyQuote {
        spend: net_in.saturating_add(fees),
        net_in,
        venue_fees: fees,
        tokens: u64::try_from(q.base_out).map_err(|_| QuoteRefusal::Unpriceable)?,
    })
}

/// Pool SELL of exactly `tokens` (SDK `sellBaseInput`, verified on independent transactions).
pub fn amm_sell(
    base: u64,
    quote: u64,
    vq: Option<u64>,
    parts: Option<(u32, u32, u32)>,
    tokens: u64,
) -> Result<SellQuote, QuoteRefusal> {
    let f = amm_parts(parts)?;
    let vq = vq.ok_or(QuoteRefusal::AmmEconomicsMissing)?;
    if tokens == 0 {
        return Err(QuoteRefusal::Unpriceable);
    }
    let q = sell_net_quote(
        u128::from(base),
        u128::from(quote),
        u128::from(vq),
        u128::from(tokens),
        f,
    )
    .ok_or(QuoteRefusal::AmmVaultInsufficient)?;
    let fees = q.lp_fee + q.protocol_fee + q.creator_fee;
    Ok(SellQuote {
        gross: u64::try_from(q.gross).map_err(|_| QuoteRefusal::Unpriceable)?,
        venue_fees: u64::try_from(fees).map_err(|_| QuoteRefusal::Unpriceable)?,
        cashback_withheld: 0,
        net: u64::try_from(q.net).map_err(|_| QuoteRefusal::Unpriceable)?,
    })
}

/// Per-leg network + tip lamports for a landed transaction (booked once per landed leg).
#[must_use]
pub const fn landed_leg_cost(tip_lamports: u64) -> u64 {
    NETWORK_FEE_P50_LAMPORTS.saturating_add(tip_lamports)
}

/// All-in price of a fill, lamports per raw token in the engine's 1e9 fixed point (ceil).
#[must_use]
pub fn price_fp(lamports: u64, tokens: u64) -> Option<u64> {
    if tokens == 0 {
        return None;
    }
    u64::try_from((u128::from(lamports) * 1_000_000_000).div_ceil(u128::from(tokens))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_curve_fee_config_is_single_tier_95_30() {
        assert_eq!(curve_fees(), Some((95, 30)));
    }

    #[test]
    fn a_curve_sell_beyond_real_sol_is_refused_with_the_largest_sellable_size() {
        // 74721d at slot 454,754,100 (m3 item 1): real SOL 654,602.
        let r = curve_sell(
            30_000_654_602,
            1_072_976_591_886_115,
            654_602,
            4_015_177_314_798,
        );
        assert_eq!(
            r,
            Err(QuoteRefusal::CurveRealSolInsufficient {
                max_tokens: 23_412_456_533
            })
        );
        let ok = curve_sell(
            30_000_654_602,
            1_072_976_591_886_115,
            654_602,
            23_412_456_533,
        )
        .unwrap();
        assert_eq!(
            (ok.gross, ok.venue_fees, ok.net),
            (654_602, 6_219 + 1_964, 646_419)
        );
    }

    #[test]
    fn a_pool_event_without_creator_fee_is_cashback_unknown_not_zero() {
        assert_eq!(
            amm_sell(1_000_000, 1_000_000, Some(0), Some((2, 93, 0)), 10),
            Err(QuoteRefusal::AmmCashbackUnknown)
        );
    }
}
