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
use crate::curve_state::{isqrt_u128, GRADUATION_VSOL_LAMPORTS};

/// Basis-point denominator (`10_000 == 100%`). Named const (§102).
const BPS_DENOM: u128 = 10_000;

/// **The fixed lamports one landed transaction costs: priority fee + Jito tip.**
///
/// Operator-set, deliberately conservative (2026-07-28 decision). It replaces BOTH
/// numbers the split cost model carried — the gate's `gate_base_fixed_lamports` of
/// 200_000 for a whole round trip (i.e. ~100_000 a leg) and the lifecycle's
/// `tip_lamports` of 10_000 a tranche, which were a 10× disagreement about the price
/// of the same signature.
///
/// This is a PER-LEG figure. A round trip pays it `1 + exit_tranches` times, and the
/// fail-rate multiplier inflates it further, because a transaction that does not land
/// still paid its priority fee and its tip.
pub const FIXED_LAMPORTS_PER_LEG: u64 = 150_000;

/// pump.fun's per-trade fee **on the bonding curve**: 1.25% = 125 bps, charged on
/// EACH leg.
///
/// The schedule is tiered on SOL-denominated market cap and the first tier break sits
/// at 420 SOL of market cap. See [`venue_fee_bps_per_leg`] for why that fact means
/// this rate is the only one a pre-graduation strategy can ever pay.
pub const VENUE_FEE_BPS_CURVE: u32 = 125;

/// The per-leg venue fee **after graduation**, on the migrated PumpSwap pool: 30 bps.
///
/// Reachable only by a position that survives the migration at
/// [`crate::curve_state::GRADUATION_VSOL_LAMPORTS`]. No admission decision this bot
/// makes inside the operator's $9k–$20k band can be priced at this rate.
pub const VENUE_FEE_BPS_POST_GRADUATION: u32 = 30;

/// The market cap (lamports) at which pump.fun's fee schedule first steps down.
///
/// Stated here rather than inferred, because the ENTIRE argument of
/// [`venue_fee_bps_per_leg`] is the 9-SOL gap between this number and the market cap
/// at graduation.
pub const FIRST_FEE_TIER_BREAK_MCAP_LAMPORTS: u128 = 420_000_000_000;

/// **The venue fee in bps charged on ONE leg, from the pool's SOL-side reserve.**
///
/// pump.fun charges **1.25% per trade** on the bonding curve, tiered on
/// SOL-denominated market cap, and the first tier break is at **420 SOL of market
/// cap**. Graduation — the point at which the curve is exhausted and liquidity
/// migrates — happens at **410.88 SOL** of market cap
/// ([`crate::curve_state::mcap_lamports`] of
/// [`crate::curve_state::GRADUATION_VSOL_LAMPORTS`]).
///
/// **The tier break sits 9 SOL of market cap ABOVE the end of the curve.** That
/// single fact is why no pre-graduation band can buy fee relief: every reserve a
/// bonding-curve strategy can ever hold — launch, the operator's $9k–$20k target
/// band, the last lamport before migration — pays the top 125 bps a leg. A band
/// choice buys own-impact (a deeper pool is a smaller participation rate) and buys
/// nothing at all on fee. Any proposal that justifies a market-cap band by "we move
/// into a cheaper fee tier" is arithmetically impossible on this venue, and
/// `the_fee_tier_break_is_unreachable_before_graduation` pins it so the claim cannot
/// be made twice.
///
/// A zero reserve returns the curve rate rather than refusing: this is a fee
/// SCHEDULE lookup, not a price, and the caller that supplied a zero reserve is
/// refused by [`round_trip_lamports`] on the same input anyway. Returning the more
/// expensive rate is the conservative direction (§54).
#[inline]
#[must_use]
pub fn venue_fee_bps_per_leg(vsol_lamports: u64) -> u32 {
    if vsol_lamports >= GRADUATION_VSOL_LAMPORTS {
        VENUE_FEE_BPS_POST_GRADUATION
    } else {
        VENUE_FEE_BPS_CURVE
    }
}

/// The `ImpactCurve::linear_test` denominator that makes the strategy gate's linear
/// impact model EXACTLY this curve's constant-product impact: `vsol / 10_000`.
///
/// `pump_quant_strategy::economic_gate::ImpactCurve::linear_test(den)` computes
/// `impact_bps = size / den`; [`crate::curve_fill::own_impact_bps`] computes
/// `size · 10_000 / vsol`. The two are identical exactly when `den = vsol / 10_000`.
///
/// **This is why the denominator must be derived per candidate and can never be a
/// config constant.** A static `gate_impact_den` is right for exactly one pool depth
/// and wrong — silently, and by an unbounded factor — for every other. The golden
/// tape carried 250_000 (a 0.025 SOL pool) and then 3_000_000 (a 30 SOL pool) while
/// pricing markets from 30 to 67 SOL; deriving it removes the choice.
///
/// Clamped to `1` so a zero reserve cannot produce a zero denominator; `linear_test`
/// clamps identically, so this is documentation of an existing floor rather than a
/// new one.
#[inline]
#[must_use]
pub fn impact_den_for(vsol_lamports: u64) -> u64 {
    (vsol_lamports / 10_000).max(1)
}

/// The strategy gate's size-invariant `protocol_bps` for a market: **two legs of
/// venue fee, and nothing else**.
///
/// This is the number that removes the phantom 200 bps of "bid/ask spread" the golden
/// tape's `gate_protocol_bps = 450` carried. A constant-product AMM has one reserve
/// ratio and one price; there is no spread to cross. The cost of crossing size is own
/// impact, which the gate charges separately through [`impact_den_for`] and which
/// [`round_trip_lamports`] charges on both legs. Leaving the 200 bps in would be
/// double-counting impact under an order book's name.
#[inline]
#[must_use]
pub fn gate_protocol_bps(vsol_lamports: u64) -> u32 {
    2 * venue_fee_bps_per_leg(vsol_lamports)
}

/// The strategy gate's `base_fixed_lamports`: `FIXED_LAMPORTS_PER_LEG ·
/// (1 + exit_tranches)`, plus the ATA term.
///
/// The ATA term is [`ATA_CLOSE_LAMPORTS`], not [`ATA_RENT_LAMPORTS`], because the
/// engine runs the **lazy-hold, close-on-full-exit** policy: the rent deposit is
/// posted at admit and reclaimed in full when the position fully exits, so the cash
/// cost of the token account across a COMPLETED round trip is the one closing
/// signature. A gate that charged the full deposit would be pricing a round trip
/// nobody intends to leave unfinished (203 bps on a floor clip), and a gate that
/// charged nothing would be pricing an account that closes itself.
///
/// `exit_tranches` is clamped to `>= 1`: a position that is never exited is not a
/// round trip.
#[inline]
#[must_use]
pub fn gate_base_fixed_lamports(exit_tranches: u32) -> u64 {
    FIXED_LAMPORTS_PER_LEG
        .saturating_mul(u64::from(exit_tranches.max(1)).saturating_add(1))
        .saturating_add(ATA_CLOSE_LAMPORTS)
}

/// **The clip size that minimises round-trip cost as a fraction of notional:**
/// `S* = isqrt(fixed_total · vsol / 2)`.
///
/// Round-trip cost in bps is `fixed_total · 10_000 / S + 2 · S · 10_000 / vsol`
/// (fixed amortised over the clip, plus two legs of constant-product impact); the
/// venue fee is size-invariant and drops out of the derivative. Setting the
/// derivative to zero gives `S*² = fixed_total · vsol / 2`.
///
/// # Why this function exists
///
/// It is the shortest statement of what the ATA deposit actually does to strategy.
/// The deposit is not a 203 bps haircut you pay and forget — it is a **fixed cost**,
/// and a fixed cost moves the cost-minimising trade size by its square root. At a
/// mid-band curve, reclaiming the deposit puts the optimum near the operator's 0.1
/// SOL floor clip; abandoning it moves the optimum to roughly 0.26 SOL, a **3.3×
/// shift in the right size to trade**. Two strategies that disagree only about
/// whether they close an emptied token account should be trading different size, and
/// `the_reclaimed_deposit_moves_the_optimal_clip_by_three_times` pins exactly that.
///
/// `None` on a zero reserve, a zero fixed cost (no fixed cost means no interior
/// optimum — cost falls monotonically toward dust), or a result that does not fit
/// `u64`. Integer only (§22): the square root is
/// [`crate::curve_state::isqrt_u128`], the one implementation in this crate.
#[inline]
#[must_use]
pub fn optimal_clip_lamports(vsol_lamports: u64, fixed_total_lamports: u64) -> Option<u64> {
    if vsol_lamports == 0 || fixed_total_lamports == 0 {
        return None;
    }
    let prod = u128::from(fixed_total_lamports).checked_mul(u128::from(vsol_lamports))? / 2;
    u64::try_from(isqrt_u128(prod)).ok()
}

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
    // Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85 (the
    // helper stabilised in 1.87) — the same choice `engine.rs` documents.
    #[allow(clippy::manual_is_multiple_of)]
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
    let fee_leg = ceil_div(
        notional.checked_mul(u128::from(i.fee_bps_per_leg))?,
        BPS_DENOM,
    )?;
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
    let bps = ceil_div(
        lamports.checked_mul(BPS_DENOM)?,
        u128::from(i.notional_lamports),
    )?;
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
        let impact =
            2 * u128::from(CLIP) * u128::from(own_impact_bps(BAND_VSOL, CLIP).unwrap()) / BPS_DENOM;
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
        const {
            assert!(ANCHOR_RECLAIMED_BPS < 420);
            assert!(ANCHOR_ABANDONED_BPS > 420 && ANCHOR_ABANDONED_BPS < 538);
        }
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
        assert_eq!(
            round_trip_lamports(&zero),
            round_trip_lamports(&anchor(true))
        );
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
        assert!(
            round_trip_lamports(&worse).unwrap() > base,
            "thinner curve costs more"
        );
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
        let per_leg =
            u128::from(CLIP) * u128::from(own_impact_bps(BAND_VSOL, CLIP).unwrap()) / BPS_DENOM;
        assert_eq!(
            round_trip_lamports(&free),
            Some(u64::try_from(2 * per_leg).unwrap())
        );
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

    /// **THE FEE TIER BREAK IS UNREACHABLE.** pump.fun's first fee-tier boundary is
    /// at 420 SOL of market cap; the curve is exhausted at 410.88. The whole bonding
    /// curve — launch, the operator's band, the last lamport before migration — pays
    /// 125 bps a leg, and no pre-graduation band selection can change that.
    #[test]
    fn the_fee_tier_break_is_unreachable_before_graduation() {
        let grad_mcap = crate::curve_state::mcap_lamports(GRADUATION_VSOL_LAMPORTS).unwrap();
        assert!(grad_mcap < FIRST_FEE_TIER_BREAK_MCAP_LAMPORTS);
        // The gap, in SOL of market cap. Small, and decisive.
        assert_eq!(
            (FIRST_FEE_TIER_BREAK_MCAP_LAMPORTS - grad_mcap) / 1_000_000_000,
            9,
        );
        // Every reserve on the curve pays the top rate…
        for vsol in [
            crate::curve_state::LAUNCH_VSOL_LAMPORTS,
            BAND_VSOL,
            92_040_000_000,
            GRADUATION_VSOL_LAMPORTS - 1,
        ] {
            assert_eq!(venue_fee_bps_per_leg(vsol), VENUE_FEE_BPS_CURVE, "{vsol}");
            assert_eq!(gate_protocol_bps(vsol), 250, "two legs of curve fee");
        }
        // …and only the migrated pool pays less.
        assert_eq!(
            venue_fee_bps_per_leg(GRADUATION_VSOL_LAMPORTS),
            VENUE_FEE_BPS_POST_GRADUATION,
        );
        assert_eq!(gate_protocol_bps(GRADUATION_VSOL_LAMPORTS), 60);
        // A zero reserve is not a refusal here — it is the EXPENSIVE rate (§54).
        assert_eq!(venue_fee_bps_per_leg(0), VENUE_FEE_BPS_CURVE);
    }

    /// **THE IDENTITY THE GATE'S IMPACT MODEL NOW SATISFIES BY CONSTRUCTION.**
    /// `linear_test(vsol / 10_000).impact_bps(size)` is `own_impact_bps(vsol, size)`
    /// for every market — which a static `gate_impact_den` could only ever be for one.
    #[test]
    fn the_derived_impact_denominator_reproduces_the_constant_product_curve() {
        use pump_quant_strategy::economic_gate::ImpactCurve;
        for &vsol in &[
            crate::curve_state::LAUNCH_VSOL_LAMPORTS,
            BAND_VSOL,
            67_000_000_000,
            GRADUATION_VSOL_LAMPORTS,
            500_000_000_000,
        ] {
            let curve = ImpactCurve::linear_test(impact_den_for(vsol));
            for &size in &[10_000_000u64, CLIP, 250_000_000, 1_000_000_000] {
                let gate = u64::from(curve.impact_bps(size));
                let exact = own_impact_bps(vsol, size).unwrap();
                // Identical up to the one floor-division step each performs on a
                // slightly different grouping of the same ratio — never more than a
                // single bp apart, and never in a direction that flatters us by more.
                assert!(
                    gate.abs_diff(exact) <= 1,
                    "vsol={vsol} size={size}: gate {gate} vs curve {exact}",
                );
            }
        }
        // The anchor, exactly: a floor clip into a mid-band curve is 16 bps a leg on
        // both models.
        assert_eq!(
            ImpactCurve::linear_test(impact_den_for(BAND_VSOL)).impact_bps(CLIP),
            16,
        );
        assert_eq!(own_impact_bps(BAND_VSOL, CLIP), Some(16));
    }

    /// The gate's fixed term scales with exit legs and carries the CLOSE signature,
    /// not the deposit — the lazy-hold, close-on-full-exit policy priced.
    #[test]
    fn the_gate_fixed_term_is_legs_of_tip_plus_one_closing_signature() {
        assert_eq!(gate_base_fixed_lamports(1), 2 * 150_000 + 5_000);
        assert_eq!(gate_base_fixed_lamports(3), 4 * 150_000 + 5_000);
        // A zero-tranche exit is not free; it is one tranche.
        assert_eq!(gate_base_fixed_lamports(0), gate_base_fixed_lamports(1));
        // It is nowhere near the abandoned-deposit figure, and that is the point.
        assert!(gate_base_fixed_lamports(3) < ATA_RENT_LAMPORTS);
    }

    /// **THE STRATEGIC POINT OF THE WHOLE EXERCISE.** The ATA deposit is a FIXED
    /// cost, and a fixed cost moves the cost-minimising clip by its square root.
    /// Reclaiming the deposit puts the optimum at the operator's floor clip;
    /// abandoning it moves the optimum 3.3× higher. Two strategies that differ only
    /// in whether they close an emptied token account should not trade the same size.
    #[test]
    fn the_reclaimed_deposit_moves_the_optimal_clip_by_three_times() {
        // The anchor's own fixed terms: two legs at 100_000 plus the close signature
        // when the deposit is reclaimed, and the same plus the whole unrecovered
        // deposit when it is not.
        const RECLAIMED_FIXED: u64 = 2 * 100_000 + ATA_CLOSE_LAMPORTS;
        const ABANDONED_FIXED: u64 = 2 * 100_000 + ATA_RENT_LAMPORTS;
        let reclaimed = optimal_clip_lamports(BAND_VSOL, RECLAIMED_FIXED).unwrap();
        let abandoned = optimal_clip_lamports(BAND_VSOL, ABANDONED_FIXED).unwrap();
        println!("MEASURE optimal reclaimed={reclaimed} abandoned={abandoned}");
        assert_eq!(reclaimed, MEASURED_OPTIMAL_RECLAIMED);
        assert_eq!(abandoned, MEASURED_OPTIMAL_ABANDONED);
        // ~0.079 SOL against ~0.263 SOL.
        assert_eq!(reclaimed / 1_000_000, 79);
        assert_eq!(abandoned / 1_000_000, 262);
        // A 3.3× shift in the right size to trade, from one closing signature.
        assert_eq!(abandoned * 100 / reclaimed, 330);

        // The SHIPPED per-leg fixed cost (150_000) is larger, so both optima move up
        // together and the ratio compresses — stated so the shipped figure is on the
        // record next to the anchor's, not inferred from it.
        let ship_reclaimed =
            optimal_clip_lamports(BAND_VSOL, 2 * FIXED_LAMPORTS_PER_LEG + ATA_CLOSE_LAMPORTS)
                .unwrap();
        let ship_abandoned =
            optimal_clip_lamports(BAND_VSOL, 2 * FIXED_LAMPORTS_PER_LEG + ATA_RENT_LAMPORTS)
                .unwrap();
        println!("MEASURE ship optimal reclaimed={ship_reclaimed} abandoned={ship_abandoned}");
        assert_eq!(ship_reclaimed, MEASURED_SHIP_OPTIMAL_RECLAIMED);
        assert_eq!(ship_abandoned, MEASURED_SHIP_OPTIMAL_ABANDONED);
    }

    /// Measured, not computed: pinned from the first run of
    /// `the_reclaimed_deposit_moves_the_optimal_clip_by_three_times`.
    const MEASURED_OPTIMAL_RECLAIMED: u64 = 79_551_512;
    const MEASURED_OPTIMAL_ABANDONED: u64 = 262_921_263;
    const MEASURED_SHIP_OPTIMAL_RECLAIMED: u64 = 97_033_440;
    const MEASURED_SHIP_OPTIMAL_ABANDONED: u64 = 268_727_810;

    /// `optimal_clip_lamports` really is the minimiser: the cost at `S*` is no worse
    /// than the cost anywhere near it. Verified against the SAME arithmetic
    /// [`round_trip_bps`] uses, so the optimum is the optimum of the shipped model
    /// and not of a private restatement of it.
    #[test]
    fn the_optimal_clip_actually_minimises_the_shipped_round_trip_cost() {
        let fixed_total = 2 * FIXED_LAMPORTS_PER_LEG + ATA_CLOSE_LAMPORTS;
        let star = optimal_clip_lamports(BAND_VSOL, fixed_total).unwrap();
        let cost_at = |n: u64| -> u32 {
            round_trip_bps(&CostInputs {
                notional_lamports: n,
                vsol_lamports: BAND_VSOL,
                fee_bps_per_leg: VENUE_FEE_BPS_CURVE,
                fixed_lamports_per_leg: FIXED_LAMPORTS_PER_LEG,
                fail_rate_bps: 0,
                exit_tranches: 1,
                needs_ata: true,
                reclaims_ata: true,
            })
            .unwrap()
        };
        let at_star = cost_at(star);
        for &other in &[
            star / 4,
            star / 2,
            star * 2,
            star * 4,
            10_000_000,
            1_000_000_000,
        ] {
            assert!(
                cost_at(other) >= at_star,
                "S*={star} costs {at_star} bps; {other} costs {} bps",
                cost_at(other),
            );
        }
        // Degenerate inputs refuse rather than returning a fabricated size.
        assert_eq!(optimal_clip_lamports(0, fixed_total), None);
        assert_eq!(optimal_clip_lamports(BAND_VSOL, 0), None);
        // Totality at the extreme: it must not wrap or panic, whatever it returns.
        let _ = optimal_clip_lamports(u64::MAX, u64::MAX);
    }
}
