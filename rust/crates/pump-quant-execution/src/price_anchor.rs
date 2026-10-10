//! Anchoring `min_tokens_out` to the price the MODEL said it would pay.
//!
//! WHY. The live sink computes `min_tokens_out` from the fresh curve reserves and a slippage
//! budget (`expected * (10_000 - bps) / 10_000`). That is a venue-protection number, not the
//! model's own risk bound: the completion's `PRICE LIMIT` — the worst price the trading brain is
//! willing to pay on this entry — was parsed and then read by nothing. Honouring it is the same
//! authority rule as sizing: the brain decides, Rust resolves.
//!
//! UNITS, established from the corpus rather than assumed (a 1e9 error here would be silent and
//! catastrophic). In a c12 row, `PRICE UNITS:` renders the same price three ways and the
//! assistant's own target is:
//!
//! ```text
//! PRICE UNITS: price_lamports_per_raw_token=0.02445740498  price_sol_per_raw_token=2.445740498e-11  ...
//! PRICE LIMIT: 0.02445740498411998
//! ```
//!
//! So `PRICE LIMIT` is **lamports per raw token**, the first of the three, and the anchor is
//! `size_lamports / limit` in raw tokens.
//!
//! ROUNDING DIRECTION. The count is CEILED. The model said "do not pay more than this price"; a
//! token count of `ceil(size/limit)` is the smallest count for which `size / count <= limit`. A
//! floored count would leave the achieved price a hair ABOVE the limit — exactly the direction a
//! limit must never be wrong in.

#![forbid(unsafe_code)]

/// Where the `min_tokens_out` that ships came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinTokensSource {
    /// Derived from the model's own `PRICE LIMIT` — the brain's bound, honoured.
    ModelPriceLimit,
    /// Derived from fresh reserves and the slippage budget, because the completion carried no
    /// usable limit (68% of trained BUYs carry none — it is optional, not required).
    SlippageFallback,
}

/// Why a model price limit could not be turned into a token count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceLimitError {
    /// Non-finite, zero or negative. The parser already refuses these on the live path; this is
    /// the same guard for any caller that did not.
    Unusable,
    /// `size / limit` exceeds `u64::MAX` tokens — a limit many orders of magnitude BELOW a
    /// lamport per raw token (for a ~0.67 SOL size, below ~3.6e-11 lamports/token). It is NOT
    /// "fractional": a merely sub-lamport limit like `0.5` (or the corpus's `0.02445740498`)
    /// yields a finite, ordinary count and is accepted. Refused rather than wrapped.
    Overflow,
    /// The count rounds to zero. A `min_tokens_out` of zero is not a bound at all.
    RoundsToZero,
}

/// The smallest raw-token count that guarantees the achieved price is at or below the limit.
pub fn min_tokens_from_price_limit(
    size_lamports: u64,
    price_limit_lamports_per_raw_token: f64,
) -> Result<u64, PriceLimitError> {
    if !price_limit_lamports_per_raw_token.is_finite() || price_limit_lamports_per_raw_token <= 0.0
    {
        return Err(PriceLimitError::Unusable);
    }
    if size_lamports == 0 {
        return Err(PriceLimitError::RoundsToZero);
    }
    let quotient = size_lamports as f64 / price_limit_lamports_per_raw_token;
    if !quotient.is_finite() || quotient > u64::MAX as f64 {
        return Err(PriceLimitError::Overflow);
    }
    // The float quotient is only a starting point: `size / limit` is rounded to nearest, and in ~45%
    // of realistic (clip, limit) pairs it rounds DOWN past an integer, so `quotient.ceil()` came out
    // one token short and admitted an average a hair ABOVE the limit (R5 finding, PLIM_CONTRACT.md
    // §S3.4). The count is settled in exact integer arithmetic on the limit's own binary value:
    // the smallest `t >= 1` with `t * limit >= size`.
    let mut t = (quotient.ceil() as u64).max(1);
    while !covers(t, size_lamports, price_limit_lamports_per_raw_token) {
        t = t.checked_add(1).ok_or(PriceLimitError::Overflow)?;
    }
    while t > 1 && covers(t - 1, size_lamports, price_limit_lamports_per_raw_token) {
        t -= 1;
    }
    Ok(t)
}

/// `tokens * limit >= size`, EXACTLY, for a finite positive `limit` (no float rounding).
///
/// A finite positive f64 is `m * 2^e` with integer `m < 2^53`; `tokens * m < 2^117` fits a u128, so the
/// comparison is exact integer arithmetic on both sides.
fn covers(tokens: u64, size: u64, limit: f64) -> bool {
    let bits = limit.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if exp == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp - 1075)
    };
    let lhs = u128::from(tokens) * u128::from(m);
    if lhs == 0 {
        return size == 0;
    }
    if e >= 0 {
        // lhs * 2^e >= size. lhs >= 1 and size < 2^64, so a shift of 64+ always covers.
        let e = e as u32;
        if e >= 64 || lhs > (u128::MAX >> e) {
            return true;
        }
        (lhs << e) >= u128::from(size)
    } else {
        // lhs >= size * 2^s. lhs < 2^117: if size * 2^s needs more than 117 bits it cannot be covered.
        let s = (-e) as u32;
        if size == 0 {
            return true;
        }
        let size_bits = 64 - size.leading_zeros();
        if size_bits + s > 127 {
            return false;
        }
        lhs >= (u128::from(size) << s)
    }
}

/// The ONE curve-buy plan the paper fill and the live builder both use (W3/R5, `PLIM_CONTRACT.md`).
///
/// * The spend is the clip, always (`spendable_sol_in = clip`). Nothing here can shrink or grow it:
///   the PRICE LIMIT is a constraint on the clip the authority chose, never a size.
/// * With a limit: `min_tokens_out` is the exact smallest count with `clip / count <= limit`.
/// * Without one (68% of trained BUYs): the caller's slippage floor (`fallback_min_tokens`) stands.
/// * When the caller has a quote for THIS clip on THIS state (`quoted_tokens`), a quote below the floor
///   is a named refusal here — never a smaller clip, a widened limit, the slippage count, or the
///   exact-out instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveBuyPlan {
    /// `buy_exact_sol_in.spendable_sol_in`: the whole clip, venue fees inside.
    pub spendable_sol_in: u64,
    /// `buy_exact_sol_in.min_tokens_out`: a floor, never a size.
    pub min_tokens_out: u64,
    /// Which bound produced the floor.
    pub source: MinTokensSource,
}

/// Why a curve buy plan was refused. Each maps to one named outcome on both paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuyPlanRefusal {
    /// A PRESENT limit that cannot become a token count (paper `fill_none:limit_unusable`,
    /// live `price_limit_unusable`).
    LimitUnusable(PriceLimitError),
    /// The clip's quote delivers fewer tokens than the limit's floor (paper `fill_none:limit_not_met`,
    /// live `price_limit_not_met`).
    LimitNotMet {
        /// Tokens the quote delivers for the full clip.
        quoted_tokens: u64,
        /// The floor the limit requires.
        min_tokens_out: u64,
    },
    /// No limit, and the clip's quote is below the slippage floor (live `slippage_floor_not_met`).
    SlippageFloorNotMet {
        /// Tokens the quote delivers for the full clip.
        quoted_tokens: u64,
        /// The slippage floor.
        min_tokens_out: u64,
    },
}

impl BuyPlanRefusal {
    /// The stable name both paths report.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::LimitUnusable(_) => "limit_unusable",
            Self::LimitNotMet { .. } => "limit_not_met",
            Self::SlippageFloorNotMet { .. } => "slippage_floor_not_met",
        }
    }
}

/// Plan a curve buy of exactly `clip` lamports. See [`CurveBuyPlan`].
pub fn plan_curve_buy(
    clip: u64,
    price_limit: Option<f64>,
    quoted_tokens: Option<u64>,
    fallback_min_tokens: u64,
) -> Result<CurveBuyPlan, BuyPlanRefusal> {
    let (min_tokens_out, source) = match price_limit {
        Some(limit) => (
            min_tokens_from_price_limit(clip, limit).map_err(BuyPlanRefusal::LimitUnusable)?,
            MinTokensSource::ModelPriceLimit,
        ),
        None => (fallback_min_tokens, MinTokensSource::SlippageFallback),
    };
    if let Some(q) = quoted_tokens {
        if q < min_tokens_out {
            return Err(match source {
                MinTokensSource::ModelPriceLimit => BuyPlanRefusal::LimitNotMet {
                    quoted_tokens: q,
                    min_tokens_out,
                },
                MinTokensSource::SlippageFallback => BuyPlanRefusal::SlippageFloorNotMet {
                    quoted_tokens: q,
                    min_tokens_out,
                },
            });
        }
    }
    Ok(CurveBuyPlan {
        spendable_sol_in: clip,
        min_tokens_out,
        source,
    })
}

/// Resolve the `min_tokens_out` that ships.
///
/// The model's limit governs when the completion carried a usable one; otherwise the
/// slippage-derived count stands. Note the direction of the fallback: it is not a second,
/// competing bound — it is what protects the order when the brain expressed no price at all.
pub fn resolve_min_tokens_out(
    size_lamports: u64,
    model_price_limit: Option<f64>,
    slippage_min_tokens_out: u64,
) -> (u64, MinTokensSource) {
    if let Some(limit) = model_price_limit {
        if let Ok(from_model) = min_tokens_from_price_limit(size_lamports, limit) {
            return (from_model, MinTokensSource::ModelPriceLimit);
        }
    }
    (slippage_min_tokens_out, MinTokensSource::SlippageFallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact row used to establish the units: a c12 BUY whose prompt rendered
    /// `price_lamports_per_raw_token=0.02445740498` and whose target was
    /// `PRICE LIMIT: 0.02445740498411998`. The anchor is a count, not a guess.
    #[test]
    fn the_anchor_is_the_count_that_makes_the_price_equal_the_limit() {
        let size = 666_666_666u64; // 0.667 SOL
        let limit = 0.02445740498411998f64;
        let tokens = min_tokens_from_price_limit(size, limit).expect("usable");
        assert_eq!(tokens, 27_258_274_802);
        // The bound holds in the direction it must: the achieved price is at or BELOW the limit.
        let achieved = size as f64 / tokens as f64;
        assert!(
            achieved <= limit,
            "achieved {achieved} must not exceed the limit {limit}"
        );
        // ...and it is the SMALLEST such count, so we never demand more tokens than the limit
        // requires (a stricter bound would refuse fills the brain had authorised).
        let one_fewer = size as f64 / (tokens - 1) as f64;
        assert!(
            one_fewer > limit,
            "one fewer token ({one_fewer}) would break the limit — the count is not minimal"
        );
    }

    /// Sub-lamport *fractional* limits are the NORMAL case on the bonding curve — not an
    /// overflow. Overflow is a distinct, far-away condition (`size/limit` exceeding `u64::MAX`,
    /// below ~3.6e-11 lamports/token for a 0.667-SOL size), many orders of magnitude under any
    /// realistic price.
    #[test]
    fn a_sub_lamport_fractional_limit_is_valid_the_overflow_is_far_lower() {
        let size = 666_666_666u64; // 0.667 SOL
                                   // 0.5 lamports/token -> 2x raw tokens. Accepted, exact.
        assert_eq!(
            min_tokens_from_price_limit(size, 0.5).unwrap(),
            1_333_333_332
        );
        // 0.25 -> 4x. Both are exact powers of two, so the division+ceil is deterministic.
        assert_eq!(
            min_tokens_from_price_limit(size, 0.25).unwrap(),
            2_666_666_664
        );
        // The corpus price itself (already pinned in the anchor test).
        assert_eq!(
            min_tokens_from_price_limit(size, 0.02445740498411998).unwrap(),
            27_258_274_802
        );
        // A tiny-but-valid limit still resolves to a finite u64 count near the boundary, and
        // the bound holds in the right direction.
        let v = min_tokens_from_price_limit(size, 1e-9).unwrap();
        assert!(
            v > 1_000_000_000_000,
            "1e-9 lamports/token for 0.667 SOL is ~6.7e17 tokens"
        );
        assert!(
            (size as f64 / v as f64) <= 1e-9,
            "achieved price must not exceed the limit"
        );
        // The overflow threshold is size/u64::MAX ≈ 3.6e-11, NOT "below one lamport".
        assert_eq!(
            min_tokens_from_price_limit(size, 1e-12),
            Err(PriceLimitError::Overflow)
        );
        assert_eq!(
            min_tokens_from_price_limit(size, 1e-20),
            Err(PriceLimitError::Overflow)
        );
    }

    #[test]
    fn an_unusable_limit_is_refused_not_coerced() {
        assert_eq!(
            min_tokens_from_price_limit(1_000_000, 0.0),
            Err(PriceLimitError::Unusable)
        );
        assert_eq!(
            min_tokens_from_price_limit(1_000_000, -1.0),
            Err(PriceLimitError::Unusable)
        );
        assert_eq!(
            min_tokens_from_price_limit(1_000_000, f64::NAN),
            Err(PriceLimitError::Unusable)
        );
        // A price of a fraction of a lamport per raw token: the count exceeds a u64.
        assert_eq!(
            min_tokens_from_price_limit(u64::MAX, 1e-30),
            Err(PriceLimitError::Overflow)
        );
    }

    /// R5 rounding: the float quotient rounds DOWN past an integer in ~45% of realistic pairs, so
    /// `quotient.ceil()` was one token short (achieved average above the limit). Three such pairs, found
    /// by an exact-rational sweep, where the limit is the f64 nearest `clip / tokens`.
    #[test]
    fn the_count_is_exact_where_the_float_quotient_rounds_down() {
        for (clip, tokens) in [
            (657_892_279u64, 3_401_114_873_424u64),
            (4_616_078_771, 6_626_141_166_123),
            (4_554_070_773, 15_108_579_399_925),
        ] {
            let limit = clip as f64 / tokens as f64;
            // The f64 limit sits just BELOW clip/tokens, so `tokens` tokens are an average above it.
            assert_eq!(
                (clip as f64 / limit).ceil() as u64,
                tokens,
                "the old float count"
            );
            let t = min_tokens_from_price_limit(clip, limit).unwrap();
            assert_eq!(t, tokens + 1, "{clip}/{tokens}");
            assert!(covers(t, clip, limit) && !covers(t - 1, clip, limit));
        }
        // A limit whose f64 equals clip/tokens exactly needs exactly `tokens`.
        assert_eq!(min_tokens_from_price_limit(1 << 20, 0.25).unwrap(), 1 << 22);
        assert_eq!(min_tokens_from_price_limit(3, 0.5).unwrap(), 6);
        // Limits above 1 lamport/raw token and sub-normal-free extremes stay exact.
        assert_eq!(min_tokens_from_price_limit(10, 3.0).unwrap(), 4);
        assert_eq!(min_tokens_from_price_limit(9, 3.0).unwrap(), 3);
        assert_eq!(min_tokens_from_price_limit(1, 1e300).unwrap(), 1);
    }

    /// The shared plan: the spend is ALWAYS the clip; a limit is a floor on tokens; a short quote is a
    /// named refusal; absent limits keep the caller's slippage floor.
    #[test]
    fn the_plan_spends_the_clip_and_refuses_a_short_quote_by_name() {
        let clip = 500_000_000u64;
        let limit = 198_819e-9;
        let min = min_tokens_from_price_limit(clip, limit).unwrap();
        assert_eq!(min, 2_514_850_190_375);
        let ok = plan_curve_buy(clip, Some(limit), Some(min), 7).unwrap();
        assert_eq!(
            ok,
            CurveBuyPlan {
                spendable_sol_in: clip,
                min_tokens_out: min,
                source: MinTokensSource::ModelPriceLimit
            }
        );
        // +187.5 bp fixture: the FULL clip's all-in quote on the 80-SOL pool.
        assert_eq!(
            plan_curve_buy(clip, Some(limit), Some(2_468_558_269_818), 7),
            Err(BuyPlanRefusal::LimitNotMet {
                quoted_tokens: 2_468_558_269_818,
                min_tokens_out: min
            })
        );
        assert_eq!(
            plan_curve_buy(clip, Some(limit), Some(min - 1), 7)
                .unwrap_err()
                .label(),
            "limit_not_met"
        );
        assert_eq!(
            plan_curve_buy(clip, Some(f64::NAN), Some(u64::MAX), 7),
            Err(BuyPlanRefusal::LimitUnusable(PriceLimitError::Unusable))
        );
        assert_eq!(
            plan_curve_buy(clip, Some(1e-20), None, 7),
            Err(BuyPlanRefusal::LimitUnusable(PriceLimitError::Overflow))
        );
        // No limit: the fallback floor, the clip unchanged, and a short quote refused by its own name.
        let f = plan_curve_buy(clip, None, Some(100), 100).unwrap();
        assert_eq!((f.spendable_sol_in, f.min_tokens_out), (clip, 100));
        assert_eq!(f.source, MinTokensSource::SlippageFallback);
        assert_eq!(
            plan_curve_buy(clip, None, Some(99), 100)
                .unwrap_err()
                .label(),
            "slippage_floor_not_met"
        );
        // Without a quote, the floor is still built (the chain enforces it).
        assert_eq!(
            plan_curve_buy(clip, Some(limit), None, 7)
                .unwrap()
                .min_tokens_out,
            min
        );
    }

    #[test]
    fn the_models_limit_governs_and_slippage_is_only_the_fallback() {
        let size = 666_666_666u64;
        let slippage = 26_000_000_000u64;
        // With a limit, the model's bound is what ships — even when it is more permissive than
        // the slippage count, because it is the brain's own bound and not Rust's.
        let (tokens, source) = resolve_min_tokens_out(size, Some(0.02445740498411998), slippage);
        assert_eq!(tokens, 27_258_274_802);
        assert_eq!(source, MinTokensSource::ModelPriceLimit);
        let (tokens, source) = resolve_min_tokens_out(size, Some(0.03), slippage);
        // R5: was pinned at 22_222_222_200 (float ceil). f64(0.03) = 0.029999999999999998889…, so
        // 22_222_222_200 tokens average a hair ABOVE the limit; the exact floor is one more.
        assert_eq!(tokens, 22_222_222_201);
        assert_eq!(source, MinTokensSource::ModelPriceLimit);
        assert!(
            tokens < slippage,
            "a looser limit yields a looser count, by design"
        );
        // With no limit (68% of trained BUYs), the slippage count protects the order.
        let (tokens, source) = resolve_min_tokens_out(size, None, slippage);
        assert_eq!(tokens, slippage);
        assert_eq!(source, MinTokensSource::SlippageFallback);
        // An unusable limit is not silently trusted: it falls back rather than shipping a
        // degenerate bound.
        let (tokens, source) = resolve_min_tokens_out(size, Some(0.0), slippage);
        assert_eq!(tokens, slippage);
        assert_eq!(source, MinTokensSource::SlippageFallback);
    }
}
