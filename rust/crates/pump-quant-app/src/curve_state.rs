//! **BONDING-CURVE STATE — market cap, curve progress, and distance to graduation,
//! all derived from the ONE number the engine already has on every candidate.**
//!
//! # The identity that makes this free
//!
//! pump.fun's curve is constant-product with *virtual* reserves:
//!
//! ```text
//!   k        = vsol · vtokens                    (invariant)
//!   mcap     = vsol · SUPPLY / vtokens           (platform's own definition)
//! ```
//!
//! Substituting `vtokens = k / vsol` collapses the token side out entirely:
//!
//! ```text
//!   mcap = vsol² · SUPPLY / k = vsol² / (k / SUPPLY)
//! ```
//!
//! So **market cap is a pure function of the SOL-side reserve**, exactly as own-curve
//! impact is (`curve_fill::own_impact_bps`). The engine has carried
//! `Features::liquidity_lamports` — which *is* `vsol` — on every candidate since the
//! first commit. Market-cap banding, curve progress and distance-to-graduation
//! therefore require **no new ingestion, no oracle, and no extra decode**. They were
//! always computable and were never computed.
//!
//! # Constants, and where they come from
//!
//! Verified against the pump.fun program's own initialisation (`pump-fun-sdk`
//! bonding-curve math, and the `decode.rs` account layout this repo already parses):
//!
//! | quantity | value |
//! |---|---|
//! | initial virtual SOL reserves | 30 SOL |
//! | initial virtual token reserves | 1,073,000,000,000,000 raw |
//! | real token reserves on the curve | 793,100,000,000,000 raw |
//! | total supply | 1,000,000,000,000,000 raw |
//!
//! Two anchors fall out and are asserted in the tests below, because they are the
//! cheap end-to-end proof that the arithmetic is right:
//!
//! * launch (`vsol = 30 SOL`) → mcap **27.96 SOL**
//! * graduation (all real tokens sold) → `vsol` **115.01 SOL**, i.e. **85.01 SOL
//!   raised**, at mcap **410.88 SOL**
//!
//! The 85-SOL raise is the number the whole ecosystem quotes, and reproducing it from
//! first principles is what says these constants are the real ones.
//!
//! # Why the bands are denominated in SOL, not USD
//!
//! The operator's target is "$9k–$20k market cap". That band is implemented here in
//! **SOL**, and the USD figure is an operator-supplied conversion recorded in the
//! study, not a live oracle read. Three reasons, in order of weight:
//!
//! 1. **The objective is net SOL** (§1) and every cost on this venue — the 1.25% fee,
//!    the curve, the tip — is SOL-denominated. A USD band would make the bot's
//!    behaviour a function of a price it does not trade.
//! 2. **An oracle is a new failure mode.** A stale or missing SOL/USD print would
//!    have to fail closed, which means a price feed outage stops trading for a reason
//!    that has nothing to do with the trade (§18.2).
//! 3. **Determinism (§22).** A USD band makes the golden digest a function of an
//!    external time-varying quantity. It would never replay.
//!
//! At the conversion recorded in `docs/BAND_THESIS_2026-07-28.md` (SOL ≈ $76), the
//! operator's $9k–$20k maps to **≈118–263 SOL market cap**, i.e. `vsol` ≈ **61.7–92.0
//! SOL**, which is **37%–73% of the way along the bonding curve**. If SOL moves
//! materially the operator re-pins the SOL band; the bot never guesses.

/// Initial virtual SOL reserves the pump.fun program seeds a curve with, lamports.
pub const LAUNCH_VSOL_LAMPORTS: u64 = 30_000_000_000;

/// Initial virtual token reserves, raw units (6 decimals).
pub const INITIAL_VIRTUAL_TOKENS: u128 = 1_073_000_000_000_000;

/// Real (purchasable) token reserves seeded on the curve, raw units.
pub const INITIAL_REAL_TOKENS: u128 = 793_100_000_000_000;

/// Total token supply, raw units.
pub const TOTAL_SUPPLY: u128 = 1_000_000_000_000_000;

/// `k / SUPPLY`, the single divisor that turns `vsol²` into a market cap in lamports.
///
/// `k = 30e9 · 1.073e15`, `SUPPLY = 1e15` ⇒ divisor = `30e9 · 1.073` = 32,190,000,000.
pub const MCAP_DIVISOR_LAMPORTS: u128 = 32_190_000_000;

/// Virtual SOL reserve at the moment the last real token is sold — graduation.
///
/// `vtokens_grad = INITIAL_VIRTUAL_TOKENS − INITIAL_REAL_TOKENS = 279,900,000,000,000`,
/// so `vsol_grad = k / vtokens_grad = 115,005,359,056` lamports ≈ 115.01 SOL, i.e.
/// **85.01 SOL raised** — the figure the ecosystem quotes.
pub const GRADUATION_VSOL_LAMPORTS: u64 = 115_005_359_056;

/// Market capitalisation in lamports implied by a SOL-side reserve.
///
/// `mcap = vsol² / MCAP_DIVISOR`. Returns `None` only on a zero reserve, which is a
/// refusal rather than a zero: an undecoded pool must never be priced (§18.2).
#[must_use]
pub fn mcap_lamports(vsol_lamports: u64) -> Option<u128> {
    if vsol_lamports == 0 {
        return None;
    }
    let v = u128::from(vsol_lamports);
    Some(v.saturating_mul(v) / MCAP_DIVISOR_LAMPORTS)
}

/// The SOL-side reserve at which a given market cap is **first reached** — the
/// inverse of [`mcap_lamports`], for expressing a band the operator states in
/// market-cap terms.
///
/// `vsol = ⌈√(mcap · MCAP_DIVISOR)⌉`, by integer square root (§22: no floats anywhere
/// on a decision path).
///
/// **CEILING, deliberately.** A flooring inverse returns a reserve whose market cap is
/// fractionally *below* the target, so the band's own floor would fail
/// [`mcap_in_band`] — an off-by-one that silently rejects every candidate sitting
/// exactly at the band edge, and that no aggregate number would ever reveal. Ceiling
/// semantics make "the reserve at which this cap is reached" self-consistent with an
/// inclusive band. `the_band_edges_are_self_consistent` pins it.
#[must_use]
pub fn vsol_for_mcap(mcap_lamports: u128) -> Option<u64> {
    if mcap_lamports == 0 {
        return None;
    }
    let prod = mcap_lamports.checked_mul(MCAP_DIVISOR_LAMPORTS)?;
    let root = isqrt_u128(prod);
    // Round up unless the product was already a perfect square.
    let root = if root * root == prod { root } else { root + 1 };
    u64::try_from(root).ok()
}

/// Progress along the bonding curve in basis points: `0` at launch, `10_000` at
/// graduation. Saturates at `10_000` for a post-graduation pool.
///
/// Measured in SOL RAISED (`vsol − launch`), not in market cap, because the raise is
/// what the program actually meters to decide migration.
#[must_use]
pub fn curve_progress_bps(vsol_lamports: u64) -> u32 {
    if vsol_lamports <= LAUNCH_VSOL_LAMPORTS {
        return 0;
    }
    let raised = u128::from(vsol_lamports - LAUNCH_VSOL_LAMPORTS);
    let total = u128::from(GRADUATION_VSOL_LAMPORTS - LAUNCH_VSOL_LAMPORTS);
    u32::try_from(raised.saturating_mul(10_000) / total)
        .unwrap_or(10_000)
        .min(10_000)
}

/// SOL still to be raised before this curve graduates, lamports. `0` once reached.
///
/// This is the one **structurally defined** upside milestone on the venue: it is not a
/// fitted level, it is where liquidity migrates and the fee schedule changes.
#[must_use]
pub const fn lamports_to_graduation(vsol_lamports: u64) -> u64 {
    GRADUATION_VSOL_LAMPORTS.saturating_sub(vsol_lamports)
}

/// Whether a curve's SOL-side reserve sits inside `[lo, hi]` **market cap** band,
/// inclusive. Both bounds are lamports of market cap.
///
/// Comparing in mcap space rather than converting the band to `vsol` keeps the
/// operator's stated units and avoids a rounding seam at the band edge.
#[must_use]
pub fn mcap_in_band(vsol_lamports: u64, lo_lamports: u128, hi_lamports: u128) -> bool {
    match mcap_lamports(vsol_lamports) {
        Some(m) => m >= lo_lamports && m <= hi_lamports,
        None => false,
    }
}

/// Integer square root (Newton), `u128`. Deterministic, no floats (§22).
#[must_use]
pub fn isqrt_u128(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    // Seed from the bit length so Newton converges in a bounded number of steps.
    let mut x = 1u128 << ((128 - n.leading_zeros()).div_ceil(2));
    loop {
        let y = (x + n / x) / 2;
        if y >= x {
            break;
        }
        x = y;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE TWO ANCHORS.** If these hold, the constants are the real program's.
    #[test]
    fn the_curve_reproduces_its_own_published_anchors() {
        // Launch: 30 SOL of virtual reserve implies a ~27.96 SOL market cap.
        let launch = mcap_lamports(LAUNCH_VSOL_LAMPORTS).unwrap();
        assert_eq!(launch, 27_958_993_476, "launch mcap drifted");
        assert_eq!(launch / 1_000_000_000, 27, "launch mcap is ~27.96 SOL");

        // Graduation: derived independently from the token side, and it must land on
        // the 85-SOL raise the whole ecosystem quotes.
        let vtok_grad = INITIAL_VIRTUAL_TOKENS - INITIAL_REAL_TOKENS;
        assert_eq!(vtok_grad, 279_900_000_000_000);
        let k = u128::from(LAUNCH_VSOL_LAMPORTS) * INITIAL_VIRTUAL_TOKENS;
        let vsol_grad = u64::try_from(k / vtok_grad).unwrap();
        assert_eq!(
            vsol_grad, GRADUATION_VSOL_LAMPORTS,
            "graduation vsol drifted"
        );
        let raised = vsol_grad - LAUNCH_VSOL_LAMPORTS;
        assert_eq!(
            raised / 10_000_000,
            8_500,
            "the curve must raise 85.01 SOL to graduate — the published figure"
        );
        assert_eq!(mcap_lamports(vsol_grad).unwrap(), 410_880_168_114);
    }

    /// The mcap↔vsol map must round-trip to within one lamport of reserve.
    #[test]
    fn mcap_and_vsol_are_inverses() {
        for vsol in [
            LAUNCH_VSOL_LAMPORTS,
            45_000_000_000,
            61_740_000_000,
            92_040_000_000,
            GRADUATION_VSOL_LAMPORTS,
        ] {
            let m = mcap_lamports(vsol).unwrap();
            let back = vsol_for_mcap(m).unwrap();
            assert!(
                vsol.abs_diff(back) <= 1,
                "round trip drifted: {vsol} -> {m} -> {back}"
            );
        }
    }

    /// **THE OPERATOR'S TARGET BAND, pinned in curve coordinates.** At the recorded
    /// conversion (SOL ≈ $76), $9k–$20k is 118.42–263.16 SOL of market cap. This test
    /// states where that actually sits on the curve, because "low market cap" is
    /// misleading: the band is the MIDDLE-to-LATE curve, not the launch.
    #[test]
    fn the_target_band_is_the_middle_of_the_curve_not_the_launch() {
        const LO: u128 = 118_420_000_000; // $9k  @ $76/SOL
        const HI: u128 = 263_160_000_000; // $20k @ $76/SOL

        let lo_vsol = vsol_for_mcap(LO).unwrap();
        let hi_vsol = vsol_for_mcap(HI).unwrap();
        assert_eq!(lo_vsol / 10_000_000, 6_174, "band floor is vsol 61.74 SOL");
        assert_eq!(
            hi_vsol / 10_000_000,
            9_203,
            "band ceiling is vsol 92.03 SOL"
        );

        // 37% -> 73% of the way to graduation. Not "early".
        assert_eq!(curve_progress_bps(lo_vsol) / 100, 37);
        assert_eq!(
            curve_progress_bps(hi_vsol) / 100,
            72,
            "integer truncation: 72.9% floors to 72"
        );

        // And it is entirely PRE-graduation, so no migration event can fire mid-hold.
        assert!(hi_vsol < GRADUATION_VSOL_LAMPORTS);
        assert!(lamports_to_graduation(hi_vsol) > 0);

        // BAND EDGES, and the asymmetry that comes with ceiling semantics.
        // `vsol_for_mcap` answers "the FIRST reserve at or above this cap". For the
        // band FLOOR that is the first reserve in band; for the band CEILING it is the
        // first reserve just OUT of band, so the last in-band reserve is one lamport
        // below it. Stating this here is the point of the test: a caller that treats
        // both edges the same way silently loses or gains a candidate at the boundary.
        assert!(
            mcap_in_band(lo_vsol, LO, HI),
            "the floor reserve must be IN band"
        );
        assert!(
            !mcap_in_band(lo_vsol - 1, LO, HI),
            "one lamport below the floor is out"
        );
        assert!(
            !mcap_in_band(hi_vsol, LO, HI),
            "the ceiling reserve is the first one OUT"
        );
        assert!(
            mcap_in_band(hi_vsol - 1, LO, HI),
            "one lamport below it is the last one IN"
        );
        assert!(
            !mcap_in_band(LAUNCH_VSOL_LAMPORTS, LO, HI),
            "launch is below the band"
        );
        assert!(
            !mcap_in_band(GRADUATION_VSOL_LAMPORTS, LO, HI),
            "graduation is above it"
        );
    }

    /// **WHY THE BAND HELPS, AND BY EXACTLY HOW MUCH.** Deeper reserve means our own
    /// clip is a smaller fraction of the pool. This is the whole arithmetic benefit of
    /// the band, and it is modest — worth stating precisely so nobody oversells it.
    #[test]
    fn the_band_buys_impact_not_fees() {
        const CLIP: u64 = 100_000_000; // 0.1 SOL operator floor
        let imp = |vsol: u64| crate::curve_fill::own_impact_bps(vsol, CLIP).unwrap();

        assert_eq!(imp(LAUNCH_VSOL_LAMPORTS), 33, "launch depth: 33 bps a leg");
        assert_eq!(
            imp(vsol_for_mcap(118_420_000_000).unwrap()),
            16,
            "$9k: 16 bps"
        );
        assert_eq!(
            imp(vsol_for_mcap(263_160_000_000).unwrap()),
            10,
            "$20k: 10 bps"
        );

        // Round trip: the band saves ~35 bps against launch depth. Real, and small
        // against a ~250 bps fee that the band does NOT reduce (see below).
        let saved = 2 * (imp(LAUNCH_VSOL_LAMPORTS) - imp(vsol_for_mcap(263_160_000_000).unwrap()));
        assert_eq!(saved, 46, "the band is worth ~46 bps of round-trip impact");
    }

    /// **THE UNWELCOME FACT ABOUT FEES, pinned so it is not re-discovered the hard
    /// way.** pump.fun's fee is tiered on SOL-denominated market cap, and the first
    /// tier boundary is at **420 SOL**. Graduation happens at **410.88 SOL**.
    /// Therefore the ENTIRE bonding curve pays the top 1.25%-per-trade rate, and no
    /// choice of pre-graduation band buys any fee relief whatsoever.
    #[test]
    fn no_pre_graduation_band_can_reduce_the_fee() {
        const FIRST_FEE_TIER_BREAK_SOL_MCAP: u128 = 420_000_000_000;
        let grad_mcap = mcap_lamports(GRADUATION_VSOL_LAMPORTS).unwrap();
        assert!(
            grad_mcap < FIRST_FEE_TIER_BREAK_SOL_MCAP,
            "graduation ({grad_mcap}) sits BELOW the first fee-tier break \
             ({FIRST_FEE_TIER_BREAK_SOL_MCAP}) — every point on the curve pays 1.25%"
        );
        // The gap is small but decisive: ~9 SOL of market cap.
        assert_eq!(
            (FIRST_FEE_TIER_BREAK_SOL_MCAP - grad_mcap) / 1_000_000_000,
            9
        );
    }

    #[test]
    fn isqrt_is_exact_on_perfect_squares_and_never_overshoots() {
        for n in [
            0u128,
            1,
            2,
            3,
            4,
            99,
            100,
            101,
            1 << 60,
            u128::from(u64::MAX),
        ] {
            let r = isqrt_u128(n);
            assert!(r * r <= n, "isqrt({n}) = {r} overshot");
            assert!(
                (r + 1).checked_mul(r + 1).is_none_or(|s| s > n),
                "isqrt({n}) = {r} undershot"
            );
        }
    }

    #[test]
    fn zero_reserve_refuses_rather_than_returning_zero() {
        assert!(mcap_lamports(0).is_none());
        assert!(vsol_for_mcap(0).is_none());
        assert!(
            !mcap_in_band(0, 1, u128::MAX),
            "an undecoded pool is never in band"
        );
        assert_eq!(curve_progress_bps(0), 0);
    }
}
