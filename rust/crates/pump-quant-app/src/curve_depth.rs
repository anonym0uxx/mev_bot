//! **CURVE DEPTH — one type carrying a market's SOL-side depth AND where that depth
//! came from, so the two reserves a pump.fun market has can never be confused for
//! each other again.**
//!
//! # The defect this module exists to close
//!
//! A pump.fun bonding curve has **two** SOL-side numbers, and until this module the
//! engine had one field for both:
//!
//! * `virtual_sol` — the **price** reserve. Seeded at 30 SOL. It sets the curve, and
//!   therefore sets market cap ([`crate::curve_state::mcap_lamports`]), own-order
//!   impact ([`crate::curve_fill::own_impact_bps`]) and the fee tier
//!   ([`crate::cost_model::venue_fee_bps_per_leg`]).
//! * `real_sol` — the **payout** reserve. Seeded at 0. It is the only SOL escrowed by
//!   the program and therefore the only SOL a seller can ever receive.
//!
//! `Confirmation::sellable_depth_lamports` — the number that caps `size_band`'s
//! `x_max` — had **three producers with three different meanings**: an externally
//! supplied `OnchainConfirm` assertion, a straight copy of `Features::
//! liquidity_lamports` (i.e. *the virtual reserve*) on the EntryMode paths, and a
//! hardcoded `200_000_000` in two report harnesses. Nothing reconciled them and
//! nothing could, because the value travelled as a bare `u64` with no record of which
//! of those three things it was.
//!
//! The standing law `sellable_depth_never_exceeds_the_reserve_it_sells_into` asserted
//! `sellable <= virtual_sol` and every fixture passed it — while declaring **29 SOL of
//! sellable depth on a 30 SOL curve, whose true payout capacity is exactly zero.**
//! The assertion was not wrong so much as measured against the wrong reserve, which is
//! the failure mode a provenance type removes rather than re-argues.
//!
//! # The pattern, copied rather than invented
//!
//! `BankrollOrigin` already solved this defect class in this codebase: the sizing base
//! is either `PaperSeed(cfg.bankroll_initial_lamports)` or a live reconciled balance,
//! the distinction rides in the TYPE, and the operator's live-bankroll rule is
//! therefore enforced by the compiler rather than by a comment. Bankroll came back
//! clean from the silo audit for exactly that reason while cost, impact, fees and
//! depth all came back dirty.
//!
//! **Principle: when one quantity can come from more than one place, the value and its
//! provenance travel together in one type, and consumers receive the type — never a
//! bare integer.** [`CurveDepth`] is that type for depth; [`crate::priced_move::
//! PricedMove`] is it for the expected move.
//!
//! # Fail-closed, never clamped
//!
//! [`DepthBasis::Unknown`] answers `None` from **both** accessors. It is deliberately
//! not a zero: a zero is a number, and a number sizes. A decode that reports an
//! impossible reserve is a broken decode, and clamping it to something plausible would
//! hide the fault permanently (§18.2, §6.4). The refusal costs one rejected trade; the
//! clamp costs every future audit.
//!
//! # What this does NOT claim
//!
//! `payout_reserve()` is the SOL the pool holds **before our entry**. Our own buy adds
//! its full notional to `real_sol`, so on a bonding curve a round trip can always fund
//! its own exit and this cap is a **risk policy** — "never take more size than the
//! outside money already committed to this market" — rather than a physical
//! impossibility bound. It is stated that way deliberately: the previous bound was
//! neither, and calling this one what it is stops the next reader from mistaking a
//! policy for a law of the venue. `docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md`
//! records the full argument.

use crate::curve_state::{real_sol_for, GRADUATION_VSOL_LAMPORTS};

/// The absolute floor of the decoded-vs-derived cross-check tolerance: one operator
/// floor clip (0.1 SOL, `config::MIN_TRADE_SIZE_LAMPORTS_DEFAULT`).
///
/// Below this the 1% relative band is smaller than the smallest trade the engine may
/// place, so a disagreement inside it cannot change a single sizing decision and
/// refusing on it would only convert protocol-fee dust into rejected trades.
pub const CROSS_CHECK_FLOOR_LAMPORTS: u64 = 100_000_000;

/// The relative half of the cross-check tolerance, in basis points: 1%.
pub const CROSS_CHECK_TOLERANCE_BPS: u64 = 100;

/// How much a decoded `real_sol` may differ from the derived one before the pair is
/// refused: `max(1% of derived, one 0.1 SOL clip)`.
///
/// The relative term absorbs protocol-fee and rounding drift on a deep curve; the
/// absolute term keeps a near-launch curve — where 1% of a 0.2 SOL payout is 2M
/// lamports — from refusing on dust. Anything wider than this is not drift, it is a
/// decoder that disagrees with the venue's own arithmetic, and that must fail loudly.
#[inline]
#[must_use]
pub const fn cross_check_tolerance_lamports(derived_real_sol: u64) -> u64 {
    let relative = derived_real_sol / (10_000 / CROSS_CHECK_TOLERANCE_BPS);
    if relative > CROSS_CHECK_FLOOR_LAMPORTS {
        relative
    } else {
        CROSS_CHECK_FLOOR_LAMPORTS
    }
}

/// Where a market's SOL-side depth came from, and therefore what each reserve means.
///
/// The venue distinction is **load-bearing**: the `−30 SOL` offset is a bonding-curve
/// fact. Applying it to a migrated PumpSwap pool would understate payout depth by 30
/// SOL; failing to apply it on the curve overstates it by up to infinity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthBasis {
    /// Bonding curve, **both** reserves observed in one decode. `real_sol` is
    /// authoritative for payout and has been cross-checked against the identity.
    CurveDecoded {
        /// Virtual SOL reserve, lamports — the price curve.
        virtual_sol: u64,
        /// Real (escrowed) SOL reserve, lamports — the payout capacity.
        real_sol: u64,
    },
    /// Bonding curve, only `virtual_sol` observed. `real_sol` is DERIVED by
    /// [`crate::curve_state::real_sol_for`]. Exact, not approximate — the derivation
    /// is an identity, and it is the fallback only because a decoded value is a
    /// stronger statement about decoder health.
    CurveDerived {
        /// Virtual SOL reserve, lamports.
        virtual_sol: u64,
    },
    /// Post-graduation PumpSwap AMM: no virtual offset exists, so the reserve is the
    /// reserve on both sides.
    MigratedPool {
        /// The pool's SOL-side reserve, lamports.
        sol_reserve: u64,
    },
    /// Undecoded, impossible, or internally inconsistent. Prices nothing and sizes
    /// nothing (§18.2).
    Unknown,
}

/// A market's SOL-side depth together with its provenance. `Copy`, scalar-only, and
/// allocation-free — free to pass on the hot path (§24, §99).
///
/// Construct through [`CurveDepth::derived`], [`CurveDepth::decoded`] or
/// [`CurveDepth::migrated`]; the basis is private so no caller can assert a provenance
/// the numbers do not support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct CurveDepth {
    basis: DepthBasis,
}

impl CurveDepth {
    /// The refusal. Prices nothing, sizes nothing.
    pub const UNKNOWN: Self = Self {
        basis: DepthBasis::Unknown,
    };

    /// Depth from a SOL-side reserve alone — the shape the engine's numeric feature
    /// lane can always supply (`Features::liquidity_lamports` **is** `virtual_sol`).
    ///
    /// The venue is classified from the reserve itself, at the one boundary
    /// [`crate::curve_state::GRADUATION_VSOL_LAMPORTS`] draws:
    ///
    /// * `< LAUNCH_VSOL_LAMPORTS` → [`DepthBasis::Unknown`]. A curve cannot hold less
    ///   than its seed, so this is a broken decode and it refuses rather than clamps.
    /// * `>= GRADUATION_VSOL_LAMPORTS` → [`DepthBasis::MigratedPool`]. No curve can
    ///   reach that reserve without completing, so the market is not on the curve.
    /// * otherwise → [`DepthBasis::CurveDerived`].
    ///
    /// **Known limitation, stated rather than hidden:** the reserve alone is a
    /// *sufficient* test for "not a curve", not a *necessary* one. A migrated pool's
    /// SOL reserve (~79 SOL after the migration fee) sits BELOW the graduation vsol
    /// and is therefore read here as a curve, which understates its payout depth by 30
    /// SOL. That is the conservative direction, and it is the best a single reserve
    /// can do; the exact discriminator is the decoded `complete` flag, which
    /// [`CurveDepth::from_pump_curve`] uses when a real account decode is available.
    pub fn derived(virtual_sol_lamports: u64) -> Self {
        if virtual_sol_lamports >= GRADUATION_VSOL_LAMPORTS {
            return Self {
                basis: DepthBasis::MigratedPool {
                    sol_reserve: virtual_sol_lamports,
                },
            };
        }
        match real_sol_for(virtual_sol_lamports) {
            Some(_) => Self {
                basis: DepthBasis::CurveDerived {
                    virtual_sol: virtual_sol_lamports,
                },
            },
            None => Self::UNKNOWN,
        }
    }

    /// Depth from a decoded curve account: **both** reserves, from one snapshot.
    ///
    /// The decoded `real_sol` is cross-checked against
    /// [`crate::curve_state::real_sol_for`] and the pair is REFUSED
    /// ([`DepthBasis::Unknown`]) when they disagree by more than
    /// [`cross_check_tolerance_lamports`]. Decoded truth beats derived truth when both
    /// exist, but a decoded value that contradicts the venue's own arithmetic is not
    /// truth — it is a decoder-health alarm, and the only safe response to an alarm on
    /// a money path is to stop.
    ///
    /// Both reserves must come from the **same snapshot**. Comparing a decoded
    /// `real_sol` read at slot T against a `virtual_sol` read at slot T′ is not a
    /// decoder check, it is a staleness check, and staleness is governed by the §34.3
    /// TTL laws instead. [`crate::engine`] enforces the same-snapshot requirement at
    /// the call site.
    pub fn decoded(virtual_sol_lamports: u64, real_sol_lamports: u64) -> Self {
        let Some(derived) = real_sol_for(virtual_sol_lamports) else {
            // Not a curve (or an impossible reserve): the decoded `real_sol` describes
            // nothing this branch can price. Fall back to the reserve-only
            // classification, which refuses or reclassifies as the venue requires.
            return Self::derived(virtual_sol_lamports);
        };
        if real_sol_lamports.abs_diff(derived) > cross_check_tolerance_lamports(derived) {
            return Self::UNKNOWN;
        }
        Self {
            basis: DepthBasis::CurveDecoded {
                virtual_sol: virtual_sol_lamports,
                real_sol: real_sol_lamports,
            },
        }
    }

    /// Depth from a post-graduation PumpSwap pool, whose SOL-side vault balance is
    /// both the price reserve and the payout reserve — an AMM has no virtual offset.
    /// A zero reserve is a refusal, not an empty pool.
    pub fn migrated(sol_reserve_lamports: u64) -> Self {
        if sol_reserve_lamports == 0 {
            return Self::UNKNOWN;
        }
        Self {
            basis: DepthBasis::MigratedPool {
                sol_reserve: sol_reserve_lamports,
            },
        }
    }

    /// Depth straight from a decoded pump.fun bonding-curve account.
    ///
    /// This is the one construction that can tell a completed curve from a live one
    /// **without inferring it from the reserve**: `PumpCurve::complete` is the
    /// program's own flag. `real_sol` is already decoded by
    /// [`pump_quant_protocol::decode::decode_pump_curve`] and, until this module, was
    /// read by nothing outside that crate's own tests.
    pub fn from_pump_curve(curve: &pump_quant_protocol::decode::PumpCurve) -> Self {
        if curve.complete {
            // Migrated: the curve's escrow has been handed to the AMM, so the SOL that
            // can still be paid out is the real balance, with no virtual offset.
            return Self::migrated(curve.real_sol);
        }
        Self::decoded(curve.virtual_sol, curve.real_sol)
    }

    /// The provenance, for journalling and for tests. Read-only by construction.
    #[must_use]
    pub const fn basis(&self) -> DepthBasis {
        self.basis
    }

    /// Whether this depth can price or size anything at all.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self.basis, DepthBasis::Unknown)
    }

    /// **The reserve the PRICE model must use** — market cap, own-order impact, and
    /// the venue fee tier. `virtual_sol` on the curve; the pool reserve after
    /// migration. `None` on [`DepthBasis::Unknown`], never zero.
    #[must_use]
    pub const fn price_reserve(&self) -> Option<u64> {
        match self.basis {
            DepthBasis::CurveDecoded { virtual_sol, .. }
            | DepthBasis::CurveDerived { virtual_sol } => Some(virtual_sol),
            DepthBasis::MigratedPool { sol_reserve } => Some(sol_reserve),
            DepthBasis::Unknown => None,
        }
    }

    /// **The reserve a SELLER can actually receive from** — the `x_max` capacity cap.
    /// The decoded `real_sol` when one exists, the identity's derivation when it does
    /// not, and the pool reserve after migration. `None` on [`DepthBasis::Unknown`],
    /// never zero.
    ///
    /// A `Some(0)` IS reachable and IS meaningful: a curve at exactly its seed reserve
    /// has been bought into by nobody and can pay out nothing. That is a market with
    /// no capacity, which the gate refuses — distinct from a market whose capacity is
    /// unknown, which it also refuses, but for a reason the journal records
    /// differently.
    #[must_use]
    pub fn payout_reserve(&self) -> Option<u64> {
        match self.basis {
            DepthBasis::CurveDecoded { real_sol, .. } => Some(real_sol),
            DepthBasis::CurveDerived { virtual_sol } => real_sol_for(virtual_sol),
            DepthBasis::MigratedPool { sol_reserve } => Some(sol_reserve),
            DepthBasis::Unknown => None,
        }
    }

    /// A stable small code for the journal / diagnostics. Never reordered — it is part
    /// of the replay identity.
    #[must_use]
    pub const fn basis_code(&self) -> u8 {
        match self.basis {
            DepthBasis::Unknown => 0,
            DepthBasis::CurveDerived { .. } => 1,
            DepthBasis::CurveDecoded { .. } => 2,
            DepthBasis::MigratedPool { .. } => 3,
        }
    }
}

impl Default for CurveDepth {
    /// The safe default is the refusal — an unpopulated depth must never size.
    fn default() -> Self {
        Self::UNKNOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve_state::{LAUNCH_VSOL_LAMPORTS, MAX_CURVE_REAL_SOL_LAMPORTS};

    /// The operator's floor clip.
    const CLIP: u64 = 100_000_000;

    /// **THE MAGNITUDE THE OLD BOUND MISSED.** Every row here PASSED the retired
    /// `sellable <= virtual_sol` assertion. Against the reserve that can actually pay,
    /// the golden fixtures were overstating capacity by 7.5x to unbounded.
    #[test]
    fn the_retired_bound_passed_while_the_fixtures_were_thirtyfold_wrong() {
        // (declared vsol, declared sellable) exactly as the golden tape presented them.
        const DECLARED: [(u64, u64); 4] = [
            (30_000_000_000, 29_000_000_000),
            (31_000_000_000, 30_000_000_000),
            (32_000_000_000, 30_000_000_000),
            (34_000_000_000, 30_000_000_000),
        ];
        let mut worst = 0u64;
        for (vsol, claimed) in DECLARED {
            assert!(claimed <= vsol, "the retired assertion passed on every row");
            let real = CurveDepth::derived(vsol).payout_reserve().unwrap();
            assert!(
                claimed > real,
                "every row claimed more than the curve escrows: {claimed} vs {real}"
            );
            if real > 0 {
                worst = worst.max(claimed / real);
            } else {
                // vsol == 30 SOL: a curve nobody has bought into, declaring 29 SOL of
                // sellable depth. The overstatement is not large, it is unbounded.
                assert_eq!(vsol, 30_000_000_000);
                assert_eq!(real, 0);
            }
        }
        assert_eq!(worst, 30, "31 SOL of price reserve escrows 1 SOL");
    }

    /// Both accessors refuse on `Unknown`. Neither may answer zero — a zero is a
    /// number and a number sizes.
    #[test]
    fn unknown_refuses_from_both_sides_and_never_answers_zero() {
        let u = CurveDepth::UNKNOWN;
        assert!(u.is_unknown());
        assert_eq!(u.price_reserve(), None);
        assert_eq!(u.payout_reserve(), None);
        assert_eq!(CurveDepth::default(), u);
        assert_eq!(u.basis_code(), 0);
        // An impossible reserve becomes the refusal, NOT a clamped small number.
        for bad in [0u64, 1, LAUNCH_VSOL_LAMPORTS - 1] {
            assert!(CurveDepth::derived(bad).is_unknown(), "{bad}");
            assert_eq!(CurveDepth::derived(bad).payout_reserve(), None);
        }
        assert!(CurveDepth::migrated(0).is_unknown());
    }

    /// The two reserves are DIFFERENT numbers with different jobs, and the type keeps
    /// them apart. This is the whole point of the module in four lines.
    #[test]
    fn price_and_payout_are_different_reserves_on_the_curve() {
        let d = CurveDepth::derived(61_740_908_643); // mid-band
        assert_eq!(d.price_reserve(), Some(61_740_908_643));
        assert_eq!(d.payout_reserve(), Some(31_740_908_643));
        assert_eq!(
            d.price_reserve().unwrap() - d.payout_reserve().unwrap(),
            LAUNCH_VSOL_LAMPORTS,
        );
        // …and IDENTICAL after migration, because an AMM has no virtual offset.
        let m = CurveDepth::migrated(79_000_000_000);
        assert_eq!(m.price_reserve(), m.payout_reserve());
        assert_eq!(m.basis_code(), 3);
    }

    /// A curve at exactly its seed reserve has capacity ZERO — known, not unknown.
    /// The distinction matters: one is a market with no depth, the other is a market
    /// we cannot see.
    #[test]
    fn a_curve_nobody_has_bought_into_has_zero_capacity_not_unknown_capacity() {
        let launch = CurveDepth::derived(LAUNCH_VSOL_LAMPORTS);
        assert!(!launch.is_unknown());
        assert_eq!(launch.price_reserve(), Some(LAUNCH_VSOL_LAMPORTS));
        assert_eq!(launch.payout_reserve(), Some(0));
        // One floor clip of raise is the first reserve that can fund a floor clip.
        let barely = CurveDepth::derived(LAUNCH_VSOL_LAMPORTS + CLIP);
        assert_eq!(barely.payout_reserve(), Some(CLIP));
    }

    /// **DECODED BEATS DERIVED — until it contradicts the venue's own arithmetic.**
    #[test]
    fn a_decoded_reserve_is_preferred_and_a_contradictory_one_is_refused() {
        let vsol = 40_000_000_000u64;
        let derived = real_sol_for(vsol).unwrap(); // 10 SOL
        let tol = cross_check_tolerance_lamports(derived);
        assert_eq!(tol, 100_000_000, "1% of 10 SOL is under the 0.1 SOL floor");

        // Exactly on the identity: decoded, and the decoded value is what is served.
        let ok = CurveDepth::decoded(vsol, derived);
        assert_eq!(ok.basis_code(), 2);
        assert_eq!(ok.payout_reserve(), Some(derived));

        // Inside tolerance (protocol-fee drift): still decoded, decoded value served.
        let drifted = CurveDepth::decoded(vsol, derived - tol);
        assert_eq!(drifted.basis_code(), 2);
        assert_eq!(drifted.payout_reserve(), Some(derived - tol));

        // One lamport beyond: REFUSE. Not clamp, not prefer-the-smaller.
        assert!(CurveDepth::decoded(vsol, derived - tol - 1).is_unknown());
        assert!(CurveDepth::decoded(vsol, derived + tol + 1).is_unknown());
        // The historic fixture claim — 30 SOL of payout on a 31 SOL curve — is
        // refused outright rather than silently min()'d down to something plausible.
        assert!(CurveDepth::decoded(31_000_000_000, 30_000_000_000).is_unknown());
    }

    /// The relative tolerance takes over on a deep curve, where 0.1 SOL would be
    /// tighter than protocol-fee dust.
    #[test]
    fn the_cross_check_tolerance_is_relative_once_the_curve_is_deep() {
        assert_eq!(cross_check_tolerance_lamports(0), CROSS_CHECK_FLOOR_LAMPORTS);
        assert_eq!(
            cross_check_tolerance_lamports(10_000_000_000),
            CROSS_CHECK_FLOOR_LAMPORTS,
        );
        assert_eq!(
            cross_check_tolerance_lamports(MAX_CURVE_REAL_SOL_LAMPORTS),
            850_053_590,
            "1% of the graduation raise",
        );
        // Monotone, and never below the floor.
        let mut last = 0;
        for r in (0..MAX_CURVE_REAL_SOL_LAMPORTS).step_by(3_000_000_000) {
            let t = cross_check_tolerance_lamports(r);
            assert!(t >= CROSS_CHECK_FLOOR_LAMPORTS && t >= last);
            last = t;
        }
    }

    /// The venue branch: past graduation the `−30 SOL` offset must NOT be applied, or
    /// a migrated pool's payout depth is understated by 30 SOL.
    #[test]
    fn the_graduation_boundary_switches_the_offset_off() {
        let grad = CurveDepth::derived(GRADUATION_VSOL_LAMPORTS);
        assert_eq!(grad.basis_code(), 3, "at graduation this is no longer a curve");
        assert_eq!(grad.payout_reserve(), Some(GRADUATION_VSOL_LAMPORTS));
        // One lamport earlier it is still a curve, and payout is 30 SOL smaller.
        let last_curve = CurveDepth::derived(GRADUATION_VSOL_LAMPORTS - 1);
        assert_eq!(last_curve.basis_code(), 1);
        assert_eq!(
            last_curve.payout_reserve(),
            Some(MAX_CURVE_REAL_SOL_LAMPORTS - 1)
        );
        assert_eq!(
            grad.payout_reserve().unwrap() - last_curve.payout_reserve().unwrap(),
            LAUNCH_VSOL_LAMPORTS + 1,
        );
    }

    /// The decoded `complete` flag is the EXACT venue discriminator, and it is the
    /// reason `real_sol` must keep being decoded rather than always derived.
    #[test]
    fn a_completed_curve_is_detected_by_its_own_flag_not_by_its_reserve() {
        use pump_quant_protocol::decode::PumpCurve;
        // A migrated pool holding 79 SOL — BELOW the graduation vsol, so the
        // reserve-only classifier would call it a curve and understate it by 30 SOL.
        let migrated = PumpCurve {
            virtual_sol: 0,
            virtual_token: 0,
            real_sol: 79_000_000_000,
            real_token: 0,
            complete: true,
        };
        assert_eq!(
            CurveDepth::from_pump_curve(&migrated).payout_reserve(),
            Some(79_000_000_000),
        );
        assert_eq!(
            CurveDepth::derived(79_000_000_000).payout_reserve(),
            Some(49_000_000_000),
            "the reserve-only classifier understates a migrated pool by exactly 30 SOL",
        );

        // A live curve decodes through the cross-checked pair.
        let live = PumpCurve {
            virtual_sol: 45_000_000_000,
            virtual_token: 0,
            real_sol: 15_000_000_000,
            real_token: 0,
            complete: false,
        };
        let d = CurveDepth::from_pump_curve(&live);
        assert_eq!(d.basis_code(), 2);
        assert_eq!(d.price_reserve(), Some(45_000_000_000));
        assert_eq!(d.payout_reserve(), Some(15_000_000_000));

        // A live curve whose decoded reserves contradict each other is refused.
        let broken = PumpCurve {
            real_sol: 44_000_000_000,
            ..live
        };
        assert!(CurveDepth::from_pump_curve(&broken).is_unknown());
    }

    /// Totality: no input panics, wraps, or produces a payout above the price reserve.
    #[test]
    fn depth_is_total_and_payout_never_exceeds_the_price_reserve() {
        for vsol in [
            0u64,
            1,
            LAUNCH_VSOL_LAMPORTS - 1,
            LAUNCH_VSOL_LAMPORTS,
            LAUNCH_VSOL_LAMPORTS + 1,
            61_740_908_643,
            GRADUATION_VSOL_LAMPORTS - 1,
            GRADUATION_VSOL_LAMPORTS,
            u64::MAX,
        ] {
            let d = CurveDepth::derived(vsol);
            if let (Some(p), Some(q)) = (d.price_reserve(), d.payout_reserve()) {
                assert!(q <= p, "payout {q} exceeds price reserve {p} at vsol {vsol}");
            }
            for real in [0u64, 1, vsol / 2, vsol, u64::MAX] {
                let dd = CurveDepth::decoded(vsol, real);
                if let (Some(p), Some(q)) = (dd.price_reserve(), dd.payout_reserve()) {
                    assert!(q <= p, "decoded payout {q} > price {p} at {vsol}/{real}");
                }
            }
        }
    }
}
