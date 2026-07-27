//! Exact constant-product (bonding-curve / AMM) execution math — own-impact pricing.
//!
//! ## The defect this module exists to price
//!
//! The engine currently opens and closes positions at the LAST OBSERVED PRINT
//! ([`crate::engine`], `self.numeric.latest_price_fp(..)`). On a pump.fun bonding
//! curve — and on the PumpSwap constant-product pool it migrates into — that is two
//! separate lies about a real fill:
//!
//! 1. **No own-impact.** Executing size `S` does not fill at the marginal (spot)
//!    price implied by the reserves. It fills at the AVERAGE price traced along the
//!    curve as our own order walks it, which is strictly worse than spot on both
//!    sides. Because the venue is a closed-form constant product and we hold the
//!    reserves, this is not something that has to be approximated by an impact
//!    coefficient — it is exactly computable, so approximating it would be a choice
//!    to be wrong.
//! 2. **Same-slot fill is look-ahead.** Observing a swap and filling at that same
//!    print prices our order at OBSERVATION state. Real landing is at least one slot
//!    (~400 ms) later. Criterion 103 requires every fill be evaluated at the
//!    expected LANDING state, never the observation state.
//!
//! This module supplies (1): the exact curve math. It is pure, stateless, and is
//! deliberately NOT wired into any decision path yet — wiring is a separate gated
//! change (see `curve_exact_fill_enable` / `fill_landing_slots` in
//! [`crate::config::Config`], both default-off / zero so today's behaviour is
//! reproduced byte-for-byte).
//!
//! ## The math
//!
//! A constant-product venue holds reserves `(vsol, vtok)` and preserves
//! `k = vsol · vtok` across a swap.
//!
//! * BUY of `sol_in` lamports: `tokens_out = vtok − k/(vsol + sol_in)`.
//! * SELL of `tokens_in` base units: `sol_out = vsol − k/(vtok + tokens_in)`.
//!
//! The AVERAGE execution price of each side then collapses to a closed form with no
//! intermediate quantity at all — in exact arithmetic:
//!
//! ```text
//!   buy_avg  = sol_in / tokens_out  = (vsol + sol_in) / vtok
//!   sell_avg = sol_out / tokens_in  =  vsol / (vtok + tokens_in)
//!   spot     =                         vsol / vtok
//! ```
//!
//! Both closed forms are used here rather than dividing through the integer
//! `tokens_out` / `sol_out`, for three reasons that all matter for a backtest:
//! they avoid a second truncation stacked on the first; they make
//! `buy_avg ≥ spot ≥ sell_avg` and monotonicity-in-size PROVABLE from the
//! expressions rather than merely observed on sampled inputs; and they keep the
//! rounding direction honest instead of letting a division artifact in `tokens_out`
//! flatter the price. `closed_form_agrees_with_quantity_division` pins that the two
//! definitions are the same number to within the value of one token base unit.
//!
//! ## Rounding is conservative, always against us (§22 / §54)
//!
//! A buy's average price rounds UP; a sell's average price rounds DOWN; spot rounds
//! DOWN; and the post-trade reserve left behind by [`buy_tokens_out`] /
//! [`sell_sol_out`] rounds UP, so the executed quantity rounds DOWN. A fill model
//! may only ever err in the direction that makes us poorer, so that no backtest
//! number can be an artifact of a favourable rounding choice.
//!
//! **That reserve rounding is load-bearing, not cosmetic.** The naive reading of the
//! reserve formula — plain floor division of `k/(vsol + sol_in)` — hands out up to
//! one extra token base unit per fill. At pump.fun launch reserves a base unit is
//! worth ~3·10⁻⁵ lamports and the artifact is invisible; but on a venue where a base
//! unit is worth more than a lamport it is a real leak, and it composes: with floor
//! division on both legs, buying with 1 lamport and immediately selling the tokens
//! back returns MORE than 1 lamport on such reserves (measured at 1001, i.e. free
//! money manufactured by a rounding choice). Rounding the post-trade reserve up
//! closes it: `k` is then preserved or grown, never leaked, and
//! `round_trip_returns_no_more_than_it_costs` pins that a flat round trip can never
//! be profitable.
//!
//! ## Determinism (§22, §99, §102)
//!
//! Integer only: no float, no RNG, no wall-clock. Every intermediate is `u128` with
//! `checked_*` arithmetic, and every function is total — a degenerate input (zero
//! reserve, zero size, a fill too small to move one whole unit, a result that does
//! not fit `u64`) returns `None` rather than saturating, wrapping, or panicking.
//! Fail-closed: an unpriceable fill is `None`, never a fabricated number
//! (§18.2/§6.4). No state is held, so there is nothing to bound (§99).

/// Fixed-point scale for every price this module returns: `price_fp = price · 1e9`,
/// where `price` is SOL-lamports per token base unit.
///
/// This is deliberately the SAME scale the engine's `price_fp` already carries —
/// [`pump_quant_features::types::PRICE_SCALE`], the scale stamped on
/// [`crate::event::AppEvent::MarketTrade`]'s `price_fp` and therefore on everything
/// `crate::lane::NumericLane::latest_price_fp` hands the position lifecycle. A fill
/// price that is not directly comparable to the mark price it replaces would be a
/// wiring bug waiting to happen, so there is exactly one price scale in the system
/// and `price_scale_matches_engine` below pins that agreement as a test.
///
/// 1e9 also carries the resolution this math needs. At pump.fun-typical reserves a
/// token prices near `2.8e4` in these units; at a 100× coarser scale it would price
/// near `2.8e2`, and the quantisation error alone would misreport the worked
/// example's own-impact as 71 bps instead of its true 33 bps.
pub const PRICE_SCALE: u128 = 1_000_000_000;

/// Basis-point denominator (`10_000 == 100%`). Named const (§102).
pub const BPS_DENOM: u128 = 10_000;

/// `ceil(n / d)` on `u128`, total: `None` on a zero denominator or on an overflow
/// of the rounded-up quotient. Used wherever rounding up is the conservative
/// direction (buy average price; both post-trade reserves).
#[inline]
fn ceil_div(n: u128, d: u128) -> Option<u128> {
    if d == 0 {
        return None;
    }
    // Rounded up without a `+ d - 1` numerator that could itself overflow.
    let q = n / d;
    if n % d == 0 {
        Some(q)
    } else {
        q.checked_add(1)
    }
}

/// Narrow a `u128` result to `u64`, or `None` if it does not fit. Never saturates —
/// a price or quantity that overflows the engine's carrier is UNKNOWN, and UNKNOWN
/// fails closed (§18.2).
#[inline]
fn fit_u64(v: u128) -> Option<u64> {
    u64::try_from(v).ok()
}

/// `k = vsol · vtok` for non-degenerate reserves, or `None`.
///
/// `u64::MAX · u64::MAX < u128::MAX`, so the product itself never overflows; the
/// `checked_mul` is kept so the totality is structural rather than an argument.
#[inline]
fn invariant_k(vsol: u64, vtok: u64) -> Option<u128> {
    if vsol == 0 || vtok == 0 {
        return None;
    }
    u128::from(vsol).checked_mul(u128::from(vtok))
}

/// Exact constant-product execution for a BUY of `sol_in` lamports against reserves
/// (`vsol`, `vtok`). Returns the token base units received.
///
/// `k = vsol · vtok` is preserved: `tokens_out = vtok − (vsol·vtok)/(vsol + sol_in)`,
/// with the post-trade token reserve rounded UP so the venue never hands out a
/// fractional base unit it does not owe (see the module note on why that direction
/// is load-bearing).
///
/// Returns `None` on degenerate reserves, a zero size, a size too small to move a
/// whole base unit, or a result that does not fit `u64`.
#[inline]
#[must_use]
pub fn buy_tokens_out(vsol: u64, vtok: u64, sol_in: u64) -> Option<u64> {
    if sol_in == 0 {
        return None;
    }
    let k = invariant_k(vsol, vtok)?;
    let new_vsol = u128::from(vsol).checked_add(u128::from(sol_in))?;
    let new_vtok = ceil_div(k, new_vsol)?;
    let out = u128::from(vtok).checked_sub(new_vtok)?;
    // A size too small to move a whole base unit is not a fill (§18.2: an
    // unpriceable fill is UNKNOWN, not a free one).
    if out == 0 {
        return None;
    }
    fit_u64(out)
}

/// Exact constant-product execution for a SELL of `tokens_in` base units against
/// reserves (`vsol`, `vtok`). Returns the SOL lamports received.
///
/// `sol_out = vsol − (vsol·vtok)/(vtok + tokens_in)`, `k` preserved, post-trade SOL
/// reserve rounded UP for the same conservative reason as the buy leg.
///
/// Returns `None` on degenerate reserves, a zero size, a size too small to realise a
/// whole lamport, or a result that does not fit `u64`.
#[inline]
#[must_use]
pub fn sell_sol_out(vsol: u64, vtok: u64, tokens_in: u64) -> Option<u64> {
    if tokens_in == 0 {
        return None;
    }
    let k = invariant_k(vsol, vtok)?;
    let new_vtok = u128::from(vtok).checked_add(u128::from(tokens_in))?;
    let new_vsol = ceil_div(k, new_vtok)?;
    let out = u128::from(vsol).checked_sub(new_vsol)?;
    if out == 0 {
        return None;
    }
    fit_u64(out)
}

/// The AVERAGE execution price of a BUY of `sol_in` lamports, in [`PRICE_SCALE`]
/// fixed-point units — the price the position ACTUALLY opens at once its own order
/// has walked the curve, as against the marginal [`spot_price_fp`] the engine reads
/// off the last print today.
///
/// In exact arithmetic `sol_in / tokens_out = (vsol + sol_in) / vtok`, so this is
/// `ceil((vsol + sol_in) · PRICE_SCALE / vtok)` — rounded UP, never in our favour.
/// It is `≥ spot_price_fp(vsol, vtok)` for every positive `sol_in`, and
/// non-decreasing in `sol_in`, both provable directly from that expression.
///
/// `None` when the reserves or the size are degenerate, when the size buys no whole
/// base unit (so there is no fill to price), or when the price does not fit `u64`.
#[inline]
#[must_use]
pub fn buy_avg_price_fp(vsol: u64, vtok: u64, sol_in: u64) -> Option<u64> {
    // Coherence with the executed quantity: a price is only defined for a fill that
    // is itself defined.
    buy_tokens_out(vsol, vtok, sol_in)?;
    let numer = u128::from(vsol)
        .checked_add(u128::from(sol_in))?
        .checked_mul(PRICE_SCALE)?;
    fit_u64(ceil_div(numer, u128::from(vtok))?)
}

/// The AVERAGE execution price of a SELL of `tokens_in` base units, in
/// [`PRICE_SCALE`] fixed-point units — the price the position ACTUALLY closes at.
///
/// In exact arithmetic `sol_out / tokens_in = vsol / (vtok + tokens_in)`, so this is
/// `floor(vsol · PRICE_SCALE / (vtok + tokens_in))` — rounded DOWN, never in our
/// favour. It is `≤ spot_price_fp(vsol, vtok)` for every positive `tokens_in`, and
/// non-increasing in `tokens_in`, both provable directly from that expression.
///
/// `None` on degenerate reserves/size, on a size that realises no whole lamport, or
/// on a price that does not fit `u64`.
#[inline]
#[must_use]
pub fn sell_avg_price_fp(vsol: u64, vtok: u64, tokens_in: u64) -> Option<u64> {
    sell_sol_out(vsol, vtok, tokens_in)?;
    let numer = u128::from(vsol).checked_mul(PRICE_SCALE)?;
    let denom = u128::from(vtok).checked_add(u128::from(tokens_in))?;
    fit_u64(numer.checked_div(denom)?)
}

/// The MARGINAL (spot) price implied by the reserves — `vsol · PRICE_SCALE / vtok`,
/// rounded DOWN.
///
/// This is the quantity the engine uses as its fill price today. It is the
/// zero-size limit of both average prices and is therefore reachable by NO order of
/// non-zero size: it is a bound, not an execution. `None` on degenerate reserves or
/// a price that does not fit `u64`.
#[inline]
#[must_use]
pub fn spot_price_fp(vsol: u64, vtok: u64) -> Option<u64> {
    if vsol == 0 || vtok == 0 {
        return None;
    }
    let numer = u128::from(vsol).checked_mul(PRICE_SCALE)?;
    fit_u64(numer / u128::from(vtok))
}

/// Own-impact of a BUY in basis points over spot: `(avg − spot) · 10_000 / spot`,
/// rounded DOWN (a conservative REPORT of a conservative price). `None` whenever
/// either price is undefined or spot is zero.
///
/// For a constant product this is `sol_in · 10_000 / vsol` in exact arithmetic — the
/// own-impact of a buy is the size as a fraction of the SOL reserve, nothing else.
#[inline]
#[must_use]
pub fn buy_impact_bps(vsol: u64, vtok: u64, sol_in: u64) -> Option<u64> {
    let spot = u128::from(spot_price_fp(vsol, vtok)?);
    let avg = u128::from(buy_avg_price_fp(vsol, vtok, sol_in)?);
    if spot == 0 {
        return None;
    }
    fit_u64(avg.checked_sub(spot)?.checked_mul(BPS_DENOM)? / spot)
}

/// Own-impact of a SELL in basis points below spot: `(spot − avg) · 10_000 / spot`,
/// rounded DOWN. `None` whenever either price is undefined or spot is zero.
///
/// For a constant product this is `tokens_in · 10_000 / (vtok + tokens_in)` in exact
/// arithmetic — the mirror of the buy leg, so a round trip is charged impact twice.
#[inline]
#[must_use]
pub fn sell_impact_bps(vsol: u64, vtok: u64, tokens_in: u64) -> Option<u64> {
    let spot = u128::from(spot_price_fp(vsol, vtok)?);
    let avg = u128::from(sell_avg_price_fp(vsol, vtok, tokens_in)?);
    if spot == 0 {
        return None;
    }
    fit_u64(spot.checked_sub(avg)?.checked_mul(BPS_DENOM)? / spot)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pump.fun's canonical launch virtual SOL reserve: 30 SOL in lamports.
    const PF_VSOL: u64 = 30_000_000_000;
    /// pump.fun's canonical launch virtual token reserve, in base units.
    const PF_VTOK: u64 = 1_073_000_000_000_000;
    /// A realistic single scalp bite: 0.1 SOL in lamports (the engine's
    /// `MIN_TRADE_SIZE_LAMPORTS_DEFAULT` floor).
    const PF_SOL_IN: u64 = 100_000_000;

    /// The exact `tokens_out` of `PF_SOL_IN` against the canonical launch reserves.
    /// `k = 3.219e25`; `ceil(k/(vsol+sol_in)) = 1_069_435_215_946_844`; the
    /// difference from `PF_VTOK` is this. Pinned: any change to the curve math or to
    /// the rounding direction must change this number.
    const PF_TOKENS_OUT: u64 = 3_564_784_053_156;
    /// Spot price of the canonical launch reserves in [`PRICE_SCALE`] units:
    /// `floor(30e9 · 1e9 / 1.073e15)`.
    const PF_SPOT_FP: u64 = 27_958;
    /// Average buy price of `PF_SOL_IN` in [`PRICE_SCALE`] units:
    /// `ceil(30.1e9 · 1e9 / 1.073e15)`.
    const PF_BUY_AVG_FP: u64 = 28_053;
    /// **The pinned own-impact figure.** A 0.1 SOL buy into a 30 SOL virtual reserve
    /// fills 33 bps worse than the spot price the engine uses today. Exactly
    /// `sol_in · 10_000 / vsol = 1e8 · 1e4 / 3e10 = 33.33…` bps, floored.
    const PF_BUY_IMPACT_BPS: u64 = 33;
    /// Selling that same position straight back fills 32 bps BELOW spot (exactly
    /// `tokens_in · 10_000 / (vtok + tokens_in) = 32.9…`, floored). The engine's
    /// current model charges neither leg, so a flat round trip that it scores at
    /// exactly zero really costs ~65 bps of notional.
    const PF_SELL_IMPACT_BPS: u64 = 32;

    /// The one price scale in the system: this module's fixed point is byte-for-byte
    /// the scale the engine's `price_fp` already carries, so a modelled fill price is
    /// directly comparable to (and substitutable for) the mark price it replaces.
    #[test]
    fn price_scale_matches_engine() {
        assert_eq!(
            i128::try_from(PRICE_SCALE).unwrap(),
            pump_quant_features::types::PRICE_SCALE,
            "curve_fill must price in the engine's own price_fp scale",
        );
    }

    // ---- the worked pump.fun example -------------------------------------------

    #[test]
    fn worked_pump_fun_example_is_exact() {
        assert_eq!(
            buy_tokens_out(PF_VSOL, PF_VTOK, PF_SOL_IN),
            Some(PF_TOKENS_OUT),
            "exact constant-product tokens_out",
        );
        assert_eq!(spot_price_fp(PF_VSOL, PF_VTOK), Some(PF_SPOT_FP));
        assert_eq!(
            buy_avg_price_fp(PF_VSOL, PF_VTOK, PF_SOL_IN),
            Some(PF_BUY_AVG_FP),
        );
        // The pinned own-impact: 33 bps worse than the price the engine fills at.
        assert_eq!(
            buy_impact_bps(PF_VSOL, PF_VTOK, PF_SOL_IN),
            Some(PF_BUY_IMPACT_BPS),
        );
        // ...and that bps figure is exactly `sol_in · 10_000 / vsol`, the
        // closed-form constant-product impact, not a coincidence of this fixed point.
        assert_eq!(
            PF_BUY_IMPACT_BPS,
            u64::try_from(u128::from(PF_SOL_IN) * BPS_DENOM / u128::from(PF_VSOL)).unwrap(),
        );
        // The engine's current model would have booked the entry at PF_SPOT_FP; the
        // difference is real lamports it never charged itself.
        assert!(PF_BUY_AVG_FP > PF_SPOT_FP);
    }

    /// The same example on the sell side: unwinding the position we just bought
    /// fills BELOW spot, so the round trip is charged own-impact twice.
    #[test]
    fn worked_example_sell_side_is_below_spot() {
        let spot = spot_price_fp(PF_VSOL, PF_VTOK).unwrap();
        let avg = sell_avg_price_fp(PF_VSOL, PF_VTOK, PF_TOKENS_OUT).unwrap();
        assert!(avg < spot, "sell fills below spot: {avg} < {spot}");
        assert_eq!(
            sell_impact_bps(PF_VSOL, PF_VTOK, PF_TOKENS_OUT),
            Some(PF_SELL_IMPACT_BPS),
        );
        // Closed form: the sell-side impact is size over the POST-trade token
        // reserve — `33.11…` bps here, so the two legs are near-mirrors at this
        // size. The reported figure is one bp lower because BOTH prices it is
        // derived from round conservatively (spot down, sell avg down), which is the
        // mandated direction; the reported impact is therefore itself a floor.
        let exact = u64::try_from(
            u128::from(PF_TOKENS_OUT) * BPS_DENOM
                / (u128::from(PF_VTOK) + u128::from(PF_TOKENS_OUT)),
        )
        .unwrap();
        assert_eq!(exact, 33);
        assert!(exact - PF_SELL_IMPACT_BPS <= 1);
    }

    // ---- the ordering laws ------------------------------------------------------

    /// A representative sweep of reserve shapes: fresh curve, mid curve, late curve,
    /// a small migrated pool, a deep pool, a thin pool, and a deliberately inverted
    /// shape where one token base unit is worth ~1000 lamports (the regime where a
    /// careless rounding direction manufactures free money).
    const RESERVES: &[(u64, u64)] = &[
        (30_000_000_000, 1_073_000_000_000_000),
        (45_000_000_000, 715_000_000_000_000),
        (85_000_000_000, 379_000_000_000_000),
        (1_000_000_000, 1_000_000_000_000),
        (500_000_000_000, 200_000_000_000_000),
        (7_000_000, 900_000_000_000),
        (1_000_000_000_000_000, 1_000_000_000_000),
    ];

    /// Sizes spanning eleven orders of magnitude, from one lamport / base unit up to
    /// a reserve-scale order.
    const SIZES: &[u64] = &[
        1,
        1_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
        10_000_000_000,
        100_000_000_000,
    ];

    /// LAW: you never buy better than spot. For EVERY positive size on every reserve
    /// shape, the average buy price is at least the marginal price.
    #[test]
    fn buy_avg_is_always_at_least_spot() {
        for &(vsol, vtok) in RESERVES {
            let Some(spot) = spot_price_fp(vsol, vtok) else {
                continue;
            };
            for &s in SIZES {
                if let Some(avg) = buy_avg_price_fp(vsol, vtok, s) {
                    assert!(
                        avg >= spot,
                        "buy avg {avg} < spot {spot} at vsol={vsol} vtok={vtok} size={s}",
                    );
                }
            }
        }
    }

    /// LAW: you never sell better than spot.
    #[test]
    fn sell_avg_is_always_at_most_spot() {
        for &(vsol, vtok) in RESERVES {
            let Some(spot) = spot_price_fp(vsol, vtok) else {
                continue;
            };
            for &s in SIZES {
                if let Some(avg) = sell_avg_price_fp(vsol, vtok, s) {
                    assert!(
                        avg <= spot,
                        "sell avg {avg} > spot {spot} at vsol={vsol} vtok={vtok} size={s}",
                    );
                }
            }
        }
    }

    /// LAW: impact is monotone in size. A larger buy never fills at a better average
    /// price, and a larger sell never realises a better one.
    #[test]
    fn impact_is_monotone_in_size() {
        for &(vsol, vtok) in RESERVES {
            let mut prev_buy: Option<u64> = None;
            let mut prev_sell: Option<u64> = None;
            for &s in SIZES {
                if let Some(avg) = buy_avg_price_fp(vsol, vtok, s) {
                    if let Some(p) = prev_buy {
                        assert!(
                            avg >= p,
                            "buy avg fell with size at vsol={vsol} vtok={vtok} size={s}",
                        );
                    }
                    prev_buy = Some(avg);
                }
                if let Some(avg) = sell_avg_price_fp(vsol, vtok, s) {
                    if let Some(p) = prev_sell {
                        assert!(
                            avg <= p,
                            "sell avg rose with size at vsol={vsol} vtok={vtok} size={s}",
                        );
                    }
                    prev_sell = Some(avg);
                }
            }
        }
    }

    /// And the monotonicity is STRICT once sizes are separated enough to clear the
    /// fixed point: on the canonical curve each 10× in size is strictly worse, and
    /// the impact scales linearly in size exactly as `sol_in·10_000/vsol` predicts.
    #[test]
    fn impact_is_strictly_worse_for_materially_larger_size() {
        let a = buy_avg_price_fp(PF_VSOL, PF_VTOK, 100_000_000).unwrap();
        let b = buy_avg_price_fp(PF_VSOL, PF_VTOK, 1_000_000_000).unwrap();
        let c = buy_avg_price_fp(PF_VSOL, PF_VTOK, 10_000_000_000).unwrap();
        assert!(a < b && b < c, "avg buy price strictly worsens: {a} {b} {c}");
        assert_eq!(buy_impact_bps(PF_VSOL, PF_VTOK, 1_000_000_000), Some(333));
        assert_eq!(buy_impact_bps(PF_VSOL, PF_VTOK, 10_000_000_000), Some(3_333));
    }

    /// LAW: as size shrinks toward zero, the average price converges to spot.
    ///
    /// The claim lives in EXACT arithmetic, and on the canonical pump.fun reserves it
    /// is asserted there: at `sol_in = vsol / 1_000_000` the unrounded gap
    /// `avg − spot = sol_in · PRICE_SCALE / vtok` is strictly less than ONE
    /// fixed-point unit.
    ///
    /// The two ROUNDED values can still differ by 2, and that is a property of the
    /// mandated conservatism, not of the convergence: `buy_avg` rounds UP by up to
    /// one unit and `spot` rounds DOWN by up to one unit, in opposite directions by
    /// design. Across the whole reserve sweep the general form of the same statement
    /// holds — the relative gap is exactly `sol_in/vsol = 1e-6` — so the rounded gap
    /// never exceeds `spot/1_000_000` plus those two half-steps.
    #[test]
    fn tiny_size_converges_to_spot() {
        // Exact-arithmetic form, on the reserves the worked example uses.
        let pf_tiny = PF_VSOL / 1_000_000;
        assert!(
            u128::from(pf_tiny) * PRICE_SCALE < u128::from(PF_VTOK),
            "exact avg-minus-spot gap must be under one fixed-point unit",
        );

        for &(vsol, vtok) in RESERVES {
            let tiny = vsol / 1_000_000;
            if tiny == 0 {
                continue;
            }
            let (Some(spot), Some(avg)) = (
                spot_price_fp(vsol, vtok),
                buy_avg_price_fp(vsol, vtok, tiny),
            ) else {
                continue;
            };
            assert!(
                avg - spot <= spot / 1_000_000 + 2,
                "rounded gap {} too wide at vsol={vsol} vtok={vtok}",
                avg - spot,
            );
        }
    }

    /// The closed-form average price and dividing straight through the executed
    /// `tokens_out` are the same quantity: they differ by at most the two rounding
    /// half-steps plus the value of one token base unit (`avg/tokens_out` in
    /// fixed-point units), which is the entire error budget of the integer fill.
    #[test]
    fn closed_form_agrees_with_quantity_division() {
        for &(vsol, vtok) in RESERVES {
            for &s in SIZES {
                let (Some(avg), Some(out)) = (
                    buy_avg_price_fp(vsol, vtok, s),
                    buy_tokens_out(vsol, vtok, s),
                ) else {
                    continue;
                };
                let avg = u128::from(avg);
                let via_qty = u128::from(s) * PRICE_SCALE / u128::from(out);
                let budget = 2 + avg / u128::from(out);
                assert!(
                    avg.abs_diff(via_qty) <= budget,
                    "closed form {avg} vs quantity division {via_qty} (budget {budget}) \
                     at vsol={vsol} vtok={vtok} size={s}",
                );
            }
        }
    }

    // ---- invariant preservation --------------------------------------------------

    /// `k` survives a buy followed by a sell of exactly the tokens that buy produced.
    ///
    /// The token reserve returns EXACTLY to its start, and the SOL reserve returns to
    /// at or ABOVE its start — never below. The residual is bounded by the value of
    /// one token base unit (the conservative rounding on each leg), and it is
    /// strictly one-directional: the pool can only end up richer, so no sequence of
    /// modelled fills can drain `k`.
    #[test]
    fn k_is_preserved_across_a_buy_then_an_equal_token_sell() {
        for &(vsol, vtok) in RESERVES {
            let unit_value_lamports = u64::try_from(
                u128::from(spot_price_fp(vsol, vtok).unwrap_or(0)) / PRICE_SCALE,
            )
            .unwrap_or(u64::MAX);
            for &s in SIZES {
                let Some(out) = buy_tokens_out(vsol, vtok, s) else {
                    continue;
                };
                let vsol1 = vsol + s;
                let vtok1 = vtok - out;
                // k never shrinks on the buy leg.
                assert!(
                    u128::from(vsol1) * u128::from(vtok1) >= u128::from(vsol) * u128::from(vtok),
                    "buy leg leaked k at vsol={vsol} vtok={vtok} size={s}",
                );
                let Some(back) = sell_sol_out(vsol1, vtok1, out) else {
                    continue;
                };
                let vsol2 = vsol1 - back;
                let vtok2 = vtok1 + out;
                assert_eq!(vtok2, vtok, "token reserve must return exactly");
                assert!(
                    vsol2 >= vsol,
                    "sol reserve fell below its start at vsol={vsol} vtok={vtok} size={s}",
                );
                assert!(
                    vsol2 - vsol <= unit_value_lamports + 1,
                    "residual {} exceeds one base unit of value ({unit_value_lamports}) \
                     at vsol={vsol} vtok={vtok} size={s}",
                    vsol2 - vsol,
                );
            }
        }
    }

    /// A round trip through the curve is never profitable in the absence of a price
    /// move: buying and immediately selling back returns no more than we put in. This
    /// is the check that the two legs cannot be composed into free money — the exact
    /// property plain floor division on the reserves destroys.
    #[test]
    fn round_trip_returns_no_more_than_it_costs() {
        for &(vsol, vtok) in RESERVES {
            for &s in SIZES {
                let Some(out) = buy_tokens_out(vsol, vtok, s) else {
                    continue;
                };
                let Some(back) = sell_sol_out(vsol + s, vtok - out, out) else {
                    continue;
                };
                assert!(
                    back <= s,
                    "round trip returned {back} for {s} at vsol={vsol} vtok={vtok}",
                );
            }
        }
    }

    // ---- degenerate inputs are total --------------------------------------------

    /// Every documented degenerate input yields `None` — never a panic, never a
    /// wrapped or saturated number that a decision could later be built on.
    #[test]
    fn degenerate_inputs_are_none_not_a_panic() {
        // Zero reserves.
        assert_eq!(buy_tokens_out(0, PF_VTOK, PF_SOL_IN), None);
        assert_eq!(buy_tokens_out(PF_VSOL, 0, PF_SOL_IN), None);
        assert_eq!(buy_tokens_out(0, 0, PF_SOL_IN), None);
        assert_eq!(sell_sol_out(0, PF_VTOK, 1), None);
        assert_eq!(sell_sol_out(PF_VSOL, 0, 1), None);
        assert_eq!(buy_avg_price_fp(0, PF_VTOK, PF_SOL_IN), None);
        assert_eq!(buy_avg_price_fp(PF_VSOL, 0, PF_SOL_IN), None);
        assert_eq!(sell_avg_price_fp(0, PF_VTOK, 1), None);
        assert_eq!(sell_avg_price_fp(PF_VSOL, 0, 1), None);
        assert_eq!(spot_price_fp(0, PF_VTOK), None);
        assert_eq!(spot_price_fp(PF_VSOL, 0), None);
        assert_eq!(spot_price_fp(0, 0), None);

        // Zero size.
        assert_eq!(buy_tokens_out(PF_VSOL, PF_VTOK, 0), None);
        assert_eq!(sell_sol_out(PF_VSOL, PF_VTOK, 0), None);
        assert_eq!(buy_avg_price_fp(PF_VSOL, PF_VTOK, 0), None);
        assert_eq!(sell_avg_price_fp(PF_VSOL, PF_VTOK, 0), None);

        // A size too small to move one whole base unit is not a fill, and therefore
        // has no average price either — it is UNKNOWN, not free.
        assert_eq!(buy_tokens_out(1_000_000_000_000_000, 1_000, 1), None);
        assert_eq!(buy_avg_price_fp(1_000_000_000_000_000, 1_000, 1), None);
        // ...and the mirror on the sell leg: a dust sale realises no whole lamport.
        assert_eq!(sell_sol_out(1_000, 1_000_000_000_000_000, 1), None);
        assert_eq!(sell_avg_price_fp(1_000, 1_000_000_000_000_000, 1), None);

        // A price that cannot fit the engine's u64 carrier fails closed rather than
        // saturating to a number a sizing decision would trust.
        assert_eq!(spot_price_fp(u64::MAX, 1), None);
        assert_eq!(buy_avg_price_fp(u64::MAX, 2, 1), None);
        assert_eq!(buy_impact_bps(u64::MAX, 1, 1), None);
        assert_eq!(sell_impact_bps(u64::MAX, 1, 1), None);
    }

    /// `u64::MAX` at every argument position: total, and any value that IS returned
    /// is arithmetically consistent (never a wrapped one).
    #[test]
    fn u64_max_inputs_do_not_wrap() {
        let m = u64::MAX;
        for &(vsol, vtok, s) in &[
            (m, m, m),
            (m, m, 1u64),
            (m, 1, m),
            (1, m, m),
            (1, 1, m),
            (m, 1, 1),
            (1, m, 1),
        ] {
            // Totality: none of these may panic.
            let out = buy_tokens_out(vsol, vtok, s);
            let sol = sell_sol_out(vsol, vtok, s);
            let bavg = buy_avg_price_fp(vsol, vtok, s);
            let savg = sell_avg_price_fp(vsol, vtok, s);
            let spot = spot_price_fp(vsol, vtok);

            // Consistency: a fill never exceeds the reserve it comes out of, and the
            // ordering laws still hold wherever both sides are defined.
            if let Some(o) = out {
                assert!(o < vtok, "tokens_out {o} must be < vtok {vtok}");
            }
            if let Some(o) = sol {
                assert!(o < vsol, "sol_out {o} must be < vsol {vsol}");
            }
            if let (Some(a), Some(sp)) = (bavg, spot) {
                assert!(a >= sp, "buy avg {a} < spot {sp} at u64::MAX edge");
            }
            if let (Some(a), Some(sp)) = (savg, spot) {
                assert!(a <= sp, "sell avg {a} > spot {sp} at u64::MAX edge");
            }
        }
    }

    /// `ceil_div` is total and correct at its edges.
    #[test]
    fn ceil_div_is_total() {
        assert_eq!(ceil_div(10, 0), None);
        assert_eq!(ceil_div(0, 5), Some(0));
        assert_eq!(ceil_div(10, 5), Some(2));
        assert_eq!(ceil_div(11, 5), Some(3));
        assert_eq!(ceil_div(u128::MAX, 1), Some(u128::MAX));
        // Would overflow under a naive `(n + d - 1) / d`.
        assert_eq!(ceil_div(u128::MAX - 1, 2), Some(u128::MAX / 2));
    }
}
