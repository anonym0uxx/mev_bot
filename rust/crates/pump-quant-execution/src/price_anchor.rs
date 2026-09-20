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
    /// `size / limit` is larger than a `u64` token count can express — an absurdly small limit
    /// (a price of a fraction of a lamport per raw token). Refused rather than wrapped.
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
    let tokens = quotient.ceil();
    if tokens < 1.0 {
        return Err(PriceLimitError::RoundsToZero);
    }
    Ok(tokens as u64)
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
        assert_eq!(tokens, 22222222200);
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
