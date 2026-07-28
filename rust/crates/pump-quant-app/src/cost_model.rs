//! The SINGLE authority on what one round trip costs — admission and P&L priced by
//! the same arithmetic.
//!
//! ## The defect this module exists to close
//!
//! Until this module the engine carried TWO independent, disagreeing round-trip cost
//! models, and used one to decide and the other to book:
//!
//! * **The gate** ([`crate::gate::decide`] → `pump_quant_strategy::economic_gate::size_band`)
//!   priced a round trip as `gate_base_fixed_lamports` (200_000 on the golden tape),
//!   fail-inflated to 210_526 — 21 bps on the 0.1 SOL floor clip — plus a flat
//!   `gate_protocol_bps` of 450, plus its own linear impact model. Roughly **538 bps**.
//! * **The lifecycle** ([`crate::position`], `HeldPosition::realize`) priced the same
//!   round trip as `exit_fee_bps` of 100 charged on the EXIT GROSS ONLY,
//!   `first_sell_penalty_bps` of 150 of notional charged once, `tip_lamports` of
//!   10_000 charged PER TRANCHE, plus exact curve impact — and, from OUTSIDE
//!   `realize`, `entry_fee_bps` of 100 plus `entry_tip_lamports`, which
//!   [`crate::engine`] folds into the position's `cost_lamports` basis at `open()`
//!   and `realize` then nets out pro-rata. Roughly **420 bps**.
//!
//! Around 120 bps of disagreement between what the engine must beat to be allowed to
//! trade and what it charges itself when it does. Every number the backtest produces
//! sits on top of that gap, and no amount of care in either model fixes it while
//! there are two of them. There is now one.
//!
//! That the entry fee is charged in the COST BASIS rather than in `realize` is
//! precisely why the split survived so long: reading `realize` alone shows an entry
//! leg that pays no fee, and reading the gate alone shows a round trip that pays no
//! rent. Neither file is wrong on its own terms; the system is wrong between them.
//! A cost model that lives in one struct cannot hide a term in another file.
//!
//! ## The two errors that were nearly equal and opposite
//!
//! Fixing the split surfaced a second defect, and the two had been silently cancelling:
//!
//! 1. **Associated Token Account rent was priced NOWHERE in the workspace.** Holding
//!    an SPL token requires an Associated Token Account, and an ATA must be
//!    rent-exempt: `(128 + 165) · 3_480 · 2 = 2_039_280` lamports must be deposited
//!    before the first buy can land. On a 0.1 SOL clip that is **203 bps** — larger
//!    than every fee on the trade combined. A grep of the workspace for `rent`, `ATA`,
//!    `AssociatedToken`, `2_039_280` and `close_account` returned nothing.
//! 2. **`gate_protocol_bps = 450` contains a phantom 200 bps of "bid/ask spread".**
//!    The golden tape's own cost commentary itemises the 450 as ~200 bps swap fee,
//!    ~55 bps LP/protocol/creator fee, and "~200 bps bid/ask spread on a thin
//!    low-cap". **A constant-product AMM has no bid/ask spread.** There is one
//!    reserve ratio and one price; the cost of crossing size is own-impact, which
//!    [`crate::curve_fill::own_impact_bps`] already charges separately. That 200 bps
//!    is double-counted impact wearing an order-book's clothes.
//!
//! `the_phantom_spread_and_the_missing_rent_are_nearly_equal` pins the coincidence:
//! 203 bps of unpriced rent against 200 bps of phantom spread. The gate has been
//! approximately right for entirely the wrong reasons. **Whoever wires this module
//! must remove the phantom spread and add the rent in the SAME change** — fixing
//! either alone moves the gate 200 bps in the wrong direction, and removing only the
//! spread would make it 200 bps too permissive on a defect that was previously
//! masked.
//!
//! ## Rent is a DEPOSIT, not a fee
//!
//! [`ATA_RENT_LAMPORTS`] is refundable in full. Closing an emptied token account
//! returns all 2_039_280 lamports and costs one signature — [`ATA_CLOSE_LAMPORTS`],
//! 5_000 lamports. That is a **408:1 return** on the only action required to collect
//! it, which makes closing an emptied ATA the highest-return operation available
//! anywhere in this system, and makes the difference between `reclaims_ata` true and
//! false worth 203 bps on a floor clip — larger than the entire modelled edge of most
//! admitted trades. A round trip that leaves its ATA open has not finished.
//!
//! ## What scales with tranche count, and what does not
//!
//! Splitting an exit into `N` tranches does NOT multiply the cost of the exit:
//!
//! * **Fee is tranche-invariant.** `fee_bps_per_leg` is charged on TWO legs — entry
//!   notional and exit gross — however many transactions the exit is split across,
//!   because selling the same tokens in three pieces sells the same tokens.
//! * **Own-impact is tranche-invariant in lamports.** Curve impact is exactly linear
//!   in size (`notional · 10_000 / vsol`), so `N` tranches of `notional/N` sum to
//!   precisely the impact of one tranche of `notional`. Splitting an exit buys
//!   nothing back on impact against a constant product.
//! * **Only the FIXED per-signature cost scales**, at `1 + exit_tranches` legs. This
//!   is exactly the third disagreement between the old two models: the gate priced
//!   ONE round trip's fixed cost while the lifecycle charged a tip PER TRANCHE.
//!
//! ## Rounding is conservative, always against us (§22 / §54)
//!
//! Every rounding choice in this module rounds the COST UP: the fail-inflated fixed
//! term, the per-leg fee, the impact lamports, and the reported bps. A cost model may
//! only ever err in the direction that makes us poorer, so that no admission can be
//! an artifact of a favourable rounding choice. The one place this module is
//! knowingly generous is inherited: [`crate::curve_fill::own_impact_bps`] floors its
//! bps, understating each leg by under one bp. That is deliberate — this module does
//! not reimplement curve math, because a second implementation of the impact identity
//! would recreate exactly the class of defect this module exists to abolish.
//!
//! ## Determinism (§22, §99, §102)
//!
//! Integer only: no float, no RNG, no wall-clock, no heap. Every intermediate is
//! `u128` with checked or saturating arithmetic, every function is total, and every
//! degenerate input returns `None` — a REFUSAL, never a fabricated zero. A zero cost
//! would be admitted by every gate on earth, so an unpriceable round trip must be
//! UNKNOWN, not free (§18.2 / §6.4). This module holds no state, so there is nothing
//! to bound (§99), and it is pure, so it is safe on the decision path.

use crate::curve_fill::own_impact_bps;

/// Basis-point denominator (`10_000 == 100%`). Named const (§102).
const BPS_DENOM: u128 = 10_000;

/// The rent-exempt minimum balance of an Associated Token Account: **2_039_280
/// lamports**, derived as `(ACCOUNT_STORAGE_OVERHEAD + SPL_TOKEN_ACCOUNT_LEN) ·
/// LAMPORTS_PER_BYTE_YEAR · EXEMPTION_THRESHOLD_YEARS` = `(128 + 165) · 3_480 · 2`.
///
/// **This is a REFUNDABLE DEPOSIT, not a fee.** It is posted before the first buy in
/// a mint can land and is returned in full by [`ATA_CLOSE_LAMPORTS`]-worth of
/// signature once the account is emptied. It is modelled here — rather than ignored,
/// as the whole workspace ignored it until now — because on the engine's 0.1 SOL floor
/// clip it is 203 bps of working capital: unrecovered, it is the single largest cost
/// on the trade; recovered, it is 0.5 bps. A cost model that cannot tell those two
/// cases apart cannot price a scalp.
pub const ATA_RENT_LAMPORTS: u64 = 2_039_280;

/// The cost of reclaiming [`ATA_RENT_LAMPORTS`]: one signature, 5_000 lamports.
///
/// `close_account` on an emptied token account returns the full rent deposit to the
/// owner, so the round trip's true ATA cost when the account is closed is this
/// signature alone — a 408:1 return (`2_039_280 / 5_000`) on the cheapest instruction
/// in the system.
pub const ATA_CLOSE_LAMPORTS: u64 = 5_000;

/// Everything a round trip's cost depends on, in one struct so that the gate and the
/// lifecycle cannot drift apart by passing different arguments.
///
/// All bps fields are basis points of `10_000`. `Copy` and scalar-only: constructing
/// one allocates nothing and it is free to pass on the decision path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct CostInputs {
    /// The clip size in lamports — the SOL committed on the entry leg, and the basis
    /// the exit is priced against on a flat round trip. Zero is a REFUSAL.
    pub notional_lamports: u64,
    /// The venue's SOL-side reserve in lamports (the engine's `liquidity_lamports`).
    /// The token side cancels out of the impact identity, so this alone prices own
    /// impact exactly. Zero is a REFUSAL.
    pub vsol_lamports: u64,
    /// Swap/protocol/creator fee in bps charged on EACH of the two legs. ONE number
    /// for both, replacing the old `entry_fee_bps` / `exit_fee_bps` pair that let the
    /// entry leg's fee live in the engine's cost basis while the exit leg's lived in
    /// `realize` — two files, two terms, no one place that summed them.
    pub fee_bps_per_leg: u32,
    /// Size-invariant lamports per landed transaction: priority fee plus tip plus
    /// base gas. Multiplied by `1 + exit_tranches`, not by two.
    pub fixed_lamports_per_leg: u64,
    /// Probability in bps that a submitted transaction does not land. Inflates the
    /// fixed term by `10_000 / (10_000 − fail_rate_bps)`, the expected attempts per
    /// landing. `>= 10_000` (certain failure) is a REFUSAL: no finite number of
    /// attempts lands.
    pub fail_rate_bps: u32,
    /// How many transactions the exit is split across. Must be `>= 1`; `0` is clamped
    /// to `1`, since a position that is never exited is not a round trip. Scales the
    /// fixed term ONLY — fee and impact are tranche-invariant (see the module note).
    pub exit_tranches: u32,
    /// Whether this round trip must post [`ATA_RENT_LAMPORTS`] to open a token
    /// account for the mint. False when an account for this mint already exists.
    pub needs_ata: bool,
    /// Whether this round trip closes the emptied token account and reclaims the
    /// deposit, paying [`ATA_CLOSE_LAMPORTS`] to do so. Worth 203 bps on a floor clip.
    pub reclaims_ata: bool,
}

/// `ceil(n / d)` on `u128`, total: `None` on a zero denominator or on an overflow of
/// the rounded-up quotient. Used everywhere rounding up is the conservative direction
/// — which, in a cost model, is everywhere.
///
/// Byte-for-byte [`crate::curve_fill`]'s own private helper, deliberately: this
/// commit adds a module and touches nothing that is already proven, and that file's
/// copy is private. It is a rounding primitive, not a cost or curve identity, so
/// duplicating it does not recreate the two-models defect this module abolishes.
/// Promoting one copy to `pub(crate)` belongs in the wiring commit.
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

/// The total cost of one round trip in lamports, or `None` if it cannot be priced.
///
/// The sum of four terms, each rounded up:
///
/// ```text
///   fee    = ceil(notional · fee_bps_per_leg / 10_000) · 2
///   fixed  = ceil(fixed_lamports_per_leg · (1 + exit_tranches)
///                 · 10_000 / (10_000 − fail_rate_bps))
///   impact = ceil(notional · own_impact_bps(vsol, notional) / 10_000) · 2
///   ata    = (needs_ata ? +ATA_RENT : 0) − (reclaims_ata ? ATA_RENT − ATA_CLOSE : 0)
/// ```
///
/// `None` — a refusal, never a zero — when `vsol_lamports == 0`, when
/// `notional_lamports == 0`, when `fail_rate_bps >= 10_000`, or when any intermediate
/// or the total does not fit `u64`. An unpriceable round trip is UNKNOWN, and UNKNOWN
/// fails closed (§18.2).
///
/// A reclaim credit is applied with a saturating subtraction, so the cost of a round
/// trip is never negative even for the incoherent input `needs_ata == false` with
/// `reclaims_ata == true` (reclaiming a deposit that was never posted). This module
/// will not report that trading pays us.
#[inline]
#[must_use]
pub fn round_trip_lamports(i: &CostInputs) -> Option<u64> {
    if i.vsol_lamports == 0 || i.notional_lamports == 0 || i.fail_rate_bps >= 10_000 {
        return None;
    }
    let notional = u128::from(i.notional_lamports);

    // 1. Fee: BOTH legs, on the full notional, from ONE parameter. Tranche-invariant
    //    — splitting a sale sells the same tokens.
    let fee_leg = ceil_div(notional.checked_mul(u128::from(i.fee_bps_per_leg))?, BPS_DENOM)?;
    let fee = fee_leg.checked_mul(2)?;

    // 2. Fixed: one entry leg plus N exit tranches, then inflated by the expected
    //    number of attempts per landing. The gate priced one round trip here while
    //    the lifecycle charged a tip per tranche; this is the term that reconciles it.
    let legs = u128::from(i.exit_tranches.max(1)).checked_add(1)?;
    let fixed_raw = u128::from(i.fixed_lamports_per_leg).checked_mul(legs)?;
    let fixed = ceil_div(
        fixed_raw.checked_mul(BPS_DENOM)?,
        BPS_DENOM.checked_sub(u128::from(i.fail_rate_bps))?,
    )?;

    // 3. Own curve impact on BOTH legs, from the one authority on curve math. The
    //    token reserve cancels — that identity is proven in `curve_fill`, not here.
    //    Linear in size, hence also tranche-invariant in lamports.
    let impact_bps = u128::from(own_impact_bps(i.vsol_lamports, i.notional_lamports)?);
    let impact_leg = ceil_div(notional.checked_mul(impact_bps)?, BPS_DENOM)?;
    let impact = impact_leg.checked_mul(2)?;

    // 4. The refundable deposit, and the signature that refunds it.
    let mut total = fee.checked_add(fixed)?.checked_add(impact)?;
    if i.needs_ata {
        total = total.checked_add(u128::from(ATA_RENT_LAMPORTS))?;
    }
    if i.reclaims_ata {
        total = total.saturating_sub(u128::from(ATA_RENT_LAMPORTS - ATA_CLOSE_LAMPORTS));
    }
    u64::try_from(total).ok()
}

/// The total cost of one round trip in basis points of notional, rounded UP, or
/// `None` under exactly the conditions [`round_trip_lamports`] refuses.
///
/// This is the number a gate compares against expected move, and the number a
/// realized-P&L check must reconcile to. There is one of it.
#[inline]
#[must_use]
pub fn round_trip_bps(i: &CostInputs) -> Option<u32> {
    let lamports = u128::from(round_trip_lamports(i)?);
    let bps = ceil_div(lamports.checked_mul(BPS_DENOM)?, u128::from(i.notional_lamports))?;
    u32::try_from(bps).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's floor clip: 0.1 SOL, `MIN_TRADE_SIZE_LAMPORTS_DEFAULT`.
    const CLIP: u64 = 100_000_000;
    /// The SOL-side reserve of a curve sitting in the middle of the operator's
    /// $9k–$20k target band — the same `IN_BAND_VSOL` [`crate::expected_move`] pins.
    const BAND_VSOL: u64 = 61_740_908_643;

    /// The anchor: a floor clip into a mid-band curve, with the golden tape's
    /// frictions restated leg-by-leg — 125 bps of swap/LP/creator fee per leg (the
    /// honest half of the tape's 450, with the phantom 200 bps of "spread" removed),
    /// 100_000 lamports of priority + tip per landed transaction, a 5% fail rate, a
    /// single-tranche exit, and an ATA that must be opened.
    fn anchor(reclaims_ata: bool) -> CostInputs {
        CostInputs {
            notional_lamports: CLIP,
            vsol_lamports: BAND_VSOL,
            fee_bps_per_leg: 125,
            fixed_lamports_per_leg: 100_000,
            fail_rate_bps: 500,
            exit_tranches: 1,
            needs_ata: true,
            reclaims_ata,
        }
    }

    /// **The pinned anchor, ATA reclaimed.** 304 bps: 250 of fee (both legs, from one
    /// parameter), 21 of fail-inflated fixed, 32 of curve
    /// impact (16 bps per leg, floored by `own_impact_bps`), and 0.5 of ATA close
    /// signature. This is what a floor clip into a mid-band curve actually costs when
    /// the round trip is finished properly.
    const ANCHOR_RECLAIMED_BPS: u32 = 304;
    /// **The pinned anchor, ATA abandoned.** The same trade, 203 bps more expensive,
    /// because 2_034_280 lamports of refundable deposit were left on the table.
    const ANCHOR_ABANDONED_BPS: u32 = 507;

    /// A floor clip into a mid-band curve, with its ATA reclaimed, costs exactly the
    /// pinned round-trip figure — and that figure sits between the two models it
    /// replaces, not outside them.
    #[test]
    fn the_reclaimed_anchor_round_trip_costs_exactly_the_pinned_bps() {
        assert_eq!(round_trip_bps(&anchor(true)), Some(ANCHOR_RECLAIMED_BPS));
        // Decomposition, so a change to any one term names itself in the diff.
        let fee = 2 * u128::from(CLIP) * 125 / BPS_DENOM;
        let fixed = (200_000u128 * BPS_DENOM).div_ceil(9_500);
        let impact = 2 * u128::from(CLIP) * u128::from(own_impact_bps(BAND_VSOL, CLIP).unwrap())
            / BPS_DENOM;
        let total = fee + fixed + impact + u128::from(ATA_CLOSE_LAMPORTS);
        assert_eq!(
            round_trip_lamports(&anchor(true)),
            Some(u64::try_from(total).unwrap()),
        );
        // It comes in BELOW both models it replaces — the gate's ~538 bps and the
        // lifecycle's ~420 bps — and that is the expected direction, not a discount:
        // the gate's figure carried 200 bps of spread a constant product cannot
        // charge, and the lifecycle's carried a 150 bps first-sell penalty that is
        // own-impact under another name. Remove the two fictions and the honest cost
        // of a properly-closed floor clip is lower than either fiction claimed, while
        // the ABANDONED case is worse than the lifecycle ever booked — which is the
        // whole point: the cost now moves with what the trade actually does, and the
        // 203 bps that separates the two cases is a real, recoverable deposit rather
        // than a constant either model could have absorbed.
        //
        // The abandoned figure lands just UNDER the gate's 538, which is the
        // cancellation this module exists to break apart: the gate's phantom spread
        // was very nearly pricing the rent it had never heard of.
        assert!(ANCHOR_RECLAIMED_BPS < 420);
        assert!(ANCHOR_ABANDONED_BPS > 420 && ANCHOR_ABANDONED_BPS < 538);
    }

    /// The same trade with the token account abandoned costs the pinned higher
    /// figure, and the gap is the unrecovered ATA deposit to within one bp of
    /// rounding — 203 bps of pure, refundable, previously unmodelled cost.
    #[test]
    fn abandoning_the_token_account_costs_the_pinned_two_hundred_and_three_bps_more() {
        assert_eq!(round_trip_bps(&anchor(false)), Some(ANCHOR_ABANDONED_BPS));
        let delta = ANCHOR_ABANDONED_BPS - ANCHOR_RECLAIMED_BPS;
        let deposit_bps = u32::try_from(
            u128::from(ATA_RENT_LAMPORTS - ATA_CLOSE_LAMPORTS) * BPS_DENOM / u128::from(CLIP),
        )
        .unwrap();
        assert_eq!(delta, 203);
        assert!(delta.abs_diff(deposit_bps) <= 1, "{delta} vs {deposit_bps}");
    }

    /// Reclaiming the deposit is STRICTLY cheaper than abandoning it, by exactly
    /// `ATA_RENT_LAMPORTS − ATA_CLOSE_LAMPORTS`, on every shape of round trip — the
    /// property that makes closing an emptied ATA unconditionally correct.
    #[test]
    fn reclaiming_the_deposit_is_strictly_cheaper_by_exactly_the_refund() {
        for &tranches in &[1u32, 2, 3, 8] {
            for &vsol in &[30_000_000_000u64, BAND_VSOL, 500_000_000_000] {
                for &notional in &[10_000_000u64, CLIP, 1_000_000_000] {
                    let mut i = anchor(false);
                    i.exit_tranches = tranches;
                    i.vsol_lamports = vsol;
                    i.notional_lamports = notional;
                    let abandoned = round_trip_lamports(&i).unwrap();
                    i.reclaims_ata = true;
                    let reclaimed = round_trip_lamports(&i).unwrap();
                    assert!(reclaimed < abandoned);
                    assert_eq!(
                        abandoned - reclaimed,
                        ATA_RENT_LAMPORTS - ATA_CLOSE_LAMPORTS,
                        "refund must be exact at tranches={tranches} vsol={vsol} n={notional}",
                    );
                }
            }
        }
    }

    /// A three-tranche exit costs strictly more than a one-tranche exit, and the
    /// entire difference is the fail-inflated fixed cost of the two extra
    /// signatures — fee and impact do not move, because neither depends on how many
    /// transactions the same sale is split across.
    #[test]
    fn three_exit_tranches_cost_more_than_one_by_exactly_the_fail_inflated_fixed_delta() {
        let mut one = anchor(true);
        one.exit_tranches = 1;
        let mut three = anchor(true);
        three.exit_tranches = 3;
        let a = round_trip_lamports(&one).unwrap();
        let b = round_trip_lamports(&three).unwrap();
        assert!(b > a, "more signatures must cost more: {b} vs {a}");
        // 4 legs' fixed cost minus 2 legs', each inflated by 10_000/9_500.
        let inflate = |legs: u128| (100_000u128 * legs * BPS_DENOM).div_ceil(9_500);
        assert_eq!(u128::from(b - a), inflate(4) - inflate(2));
        // And the delta really is only the fixed term: two extra signatures at a 5%
        // fail rate, which is 21 bps of the clip — the term the gate priced once and
        // the lifecycle charged per tranche.
        assert_eq!(b - a, 210_526);
    }

    /// A zero exit-tranche count is not a round trip; it is priced as one tranche
    /// rather than as a free exit.
    #[test]
    fn zero_exit_tranches_is_clamped_to_one_rather_than_priced_as_free() {
        let mut zero = anchor(true);
        zero.exit_tranches = 0;
        assert_eq!(round_trip_lamports(&zero), round_trip_lamports(&anchor(true)));
    }

    /// Every degenerate input REFUSES. Not one of them returns a zero cost, because a
    /// zero-cost round trip clears every gate ever written (§18.2 / §6.4).
    #[test]
    fn a_curve_with_no_depth_and_a_trade_with_no_size_refuse_rather_than_costing_zero() {
        let mut no_depth = anchor(true);
        no_depth.vsol_lamports = 0;
        assert_eq!(round_trip_lamports(&no_depth), None);
        assert_eq!(round_trip_bps(&no_depth), None);

        let mut no_size = anchor(true);
        no_size.notional_lamports = 0;
        assert_eq!(round_trip_lamports(&no_size), None);
        assert_eq!(round_trip_bps(&no_size), None);

        // Certain failure: no finite number of attempts lands the trade.
        let mut certain_failure = anchor(true);
        certain_failure.fail_rate_bps = 10_000;
        assert_eq!(round_trip_lamports(&certain_failure), None);
        assert_eq!(round_trip_bps(&certain_failure), None);
        certain_failure.fail_rate_bps = u32::MAX;
        assert_eq!(round_trip_lamports(&certain_failure), None);
    }

    /// A reclaim credit can never make a round trip pay us, even on the incoherent
    /// input that reclaims a deposit it never posted.
    #[test]
    fn a_reclaim_credit_never_drives_the_cost_of_a_round_trip_below_zero() {
        let i = CostInputs {
            notional_lamports: 1_000,
            vsol_lamports: BAND_VSOL,
            fee_bps_per_leg: 0,
            fixed_lamports_per_leg: 0,
            fail_rate_bps: 0,
            exit_tranches: 1,
            needs_ata: false,
            reclaims_ata: true,
        };
        assert_eq!(round_trip_lamports(&i), Some(0));
        assert_eq!(round_trip_bps(&i), Some(0));
    }

    /// Extreme inputs are total: `u64::MAX / 2` at every position wraps nothing,
    /// panics nowhere, and either fits `u64` honestly or refuses.
    #[test]
    fn extreme_inputs_do_not_wrap_they_refuse() {
        let h = u64::MAX / 2;
        for &(notional, vsol, fixed) in &[
            (h, h, h),
            (h, h, 0),
            (h, 1, h),
            (1, h, h),
            (h, 1, 1),
            (1, 1, h),
            (u64::MAX, u64::MAX, u64::MAX),
        ] {
            let i = CostInputs {
                notional_lamports: notional,
                vsol_lamports: vsol,
                fee_bps_per_leg: u32::MAX,
                fixed_lamports_per_leg: fixed,
                fail_rate_bps: 9_999,
                exit_tranches: u32::MAX,
                needs_ata: true,
                reclaims_ata: false,
            };
            // Totality: must not panic (this crate builds release with
            // overflow-checks ON, so a wrap here would abort).
            let lam = round_trip_lamports(&i);
            let bps = round_trip_bps(&i);
            // Consistency: any value returned is derivable from the other.
            if let (Some(l), Some(b)) = (lam, bps) {
                assert_eq!(
                    u128::from(b),
                    (u128::from(l) * BPS_DENOM).div_ceil(u128::from(notional)),
                );
            }
        }
    }

    /// Cost is monotone in every term that should raise it: more fee, more fixed, a
    /// higher fail rate, a thinner curve, and an abandoned deposit each cost more.
    #[test]
    fn cost_rises_with_every_friction_and_falls_with_none() {
        let base = round_trip_lamports(&anchor(true)).unwrap();
        let mut worse = anchor(true);
        worse.fee_bps_per_leg += 1;
        assert!(round_trip_lamports(&worse).unwrap() > base);
        let mut worse = anchor(true);
        worse.fixed_lamports_per_leg += 10_000;
        assert!(round_trip_lamports(&worse).unwrap() > base);
        let mut worse = anchor(true);
        worse.fail_rate_bps = 2_000;
        assert!(round_trip_lamports(&worse).unwrap() > base);
        let mut worse = anchor(true);
        worse.vsol_lamports = 30_000_000_000;
        assert!(round_trip_lamports(&worse).unwrap() > base, "thinner curve costs more");
        assert!(round_trip_lamports(&anchor(false)).unwrap() > base);
    }

    /// The impact term is this module's, but the curve math is not: both legs are
    /// priced by `curve_fill::own_impact_bps` and by nothing else, so there is
    /// exactly one implementation of the constant-product impact identity.
    #[test]
    fn own_impact_is_charged_on_both_legs_from_the_single_curve_authority() {
        let mut free = anchor(true);
        free.fee_bps_per_leg = 0;
        free.fixed_lamports_per_leg = 0;
        free.needs_ata = false;
        free.reclaims_ata = false;
        let per_leg = u128::from(CLIP) * u128::from(own_impact_bps(BAND_VSOL, CLIP).unwrap())
            / BPS_DENOM;
        assert_eq!(round_trip_lamports(&free), Some(u64::try_from(2 * per_leg).unwrap()));
        // A round trip is charged impact twice, never once.
        assert_eq!(own_impact_bps(BAND_VSOL, CLIP), Some(16));
        assert_eq!(round_trip_bps(&free), Some(32));
    }

    /// **The coincidence that made both defects invisible.** The ATA rent nobody
    /// priced is 203 bps of a floor clip; the "bid/ask spread" inside
    /// `gate_protocol_bps = 450` — which cannot exist on a constant-product AMM, and
    /// which the golden tape's own comment values at ~200 bps — is 200 bps. They are
    /// equal to within 3 bps and opposite in sign, so the gate has been arriving at
    /// roughly the right admission threshold by adding a cost that is not real to
    /// cancel a cost it never knew about.
    ///
    /// This is why the two MUST be fixed in the same change. Delete the phantom
    /// spread alone and the gate becomes 200 bps too permissive; add the rent alone
    /// and it becomes 200 bps too strict, on top of a fiction. Neither half is a
    /// safe commit.
    #[test]
    fn the_phantom_spread_and_the_missing_rent_are_nearly_equal() {
        let rent_bps_on_a_floor_clip = ATA_RENT_LAMPORTS * 10_000 / 100_000_000;
        let phantom_spread_bps = 200;
        assert!(rent_bps_on_a_floor_clip.abs_diff(phantom_spread_bps) <= 5);
        // Pinned exactly, so that a change to either number breaks this test rather
        // than quietly restoring the cancellation.
        assert_eq!(rent_bps_on_a_floor_clip, 203);
    }
}
