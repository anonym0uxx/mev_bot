//! **EXPECTED MOVE — the per-candidate estimate that replaces the constant.**
//!
//! # The defect this exists to close
//!
//! `gate::decide` sizes and admits from `size_band(cfg.gate_expected_move_bps, …)`.
//! Every *other* input to that call — `liquidity_lamports`, `sellable_depth_lamports`,
//! the impact curve, the fixed fee — is a **measured, per-candidate cost or capacity**
//! term. The **benefit** term is a single global constant.
//!
//! So the engine's admission rule is, in full: *"assume every token in the universe
//! has the same expected favourable move, then check whether that assumption beats
//! this token's measured cost."* Everything the system computes — holder
//! concentration, narrative velocity, creator state, whale flow, alpha calls, the
//! episodic brain — reaches discovery ranking, slot arbitration and sizing. **None of
//! it reaches the number that decides whether a trade is worth taking.**
//!
//! `docs/EDGE_PROVENANCE_2026-07-27.md §4` establishes this is the largest unpriced
//! assumption in the codebase, and the structural reason no parameter search has ever
//! found anything: every search ranged over the cost side of an inequality whose
//! benefit side was fixed.
//!
//! # What this module is NOT
//!
//! **It is not a hand-weighted composite score, and that refusal is the whole point.**
//!
//! The tempting move is to take the eight signals we already compute, assign them
//! weights — 0.25 concentration, 0.20 narrative, 0.15 creator, … — and call the sum an
//! expected move. That is strictly worse than the constant it replaces. A hand-weighted
//! score is *still* a constant (the weights are), but it is a constant wearing enough
//! machinery to look principled, it adds a large overfitting surface, and it is far
//! harder to falsify because every disappointing result invites a re-weighting rather
//! than a retirement. `docs/STRATEGY_PERMUTATION_STUDY_2026-07-25.md` is a record of
//! what happens when you tune a shape nobody calibrated.
//!
//! # What it is: a stratified empirical estimator with an EMPTY table
//!
//! The functional form is code. **Every value in it must come from realized outcomes.**
//! Until they do, the estimator *refuses* — it returns [`MoveVerdict::Unknown`] and the
//! gate falls back to the constant, byte-for-byte. This is the same discipline the
//! episodic brain already enforces for recall (§46: below the sample floor, say
//! `Unknown`, never interpolate), and it is house style for a reason: a small-sample
//! estimate that answers confidently is precisely how a quant fools himself.
//!
//! ## Why stratify on curve progress, and only on curve progress
//!
//! This is a **sample-size argument, not a modelling preference.**
//!
//! Graduation on pump.fun runs at ~0.198% (arXiv:2607.02823, 832,941 launches). Any
//! outcome worth conditioning on is rare. A replay corpus that yields, optimistically,
//! a few thousand clean episodes supports estimating on the order of ten cells with
//! ~30+ observations each — not seventy-two. Stratifying on
//! `curve_progress × flow × age` gives 72 cells and no power in any of them; the model
//! would report confident numbers assembled from four observations, which is worse than
//! the constant because it is confidently wrong instead of honestly arbitrary.
//!
//! Curve progress earns the single slot on structural grounds. On a bonding curve it is
//! (a) *exactly observable* from `vsol` alone with no oracle and no extra decode
//! (`curve_state`), (b) *monotone in the token's own life* rather than a market
//! covariate, (c) the thing that determines our own execution cost, since own-impact is
//! `notional·10_000/vsol`, and (d) bounded by a **structurally defined** terminal event
//! — graduation at 410.88 SOL of market cap — rather than by a fitted level. No other
//! available feature has all four properties.
//!
//! Additional strata are added **only** when the corpus demonstrably supports them, and
//! [`MoveParams::min_sample`] is what enforces that: adding a dimension the data cannot
//! fill makes cells refuse, which degrades to the constant, rather than makes them lie.
//!
//! # Status: DISARMED, empty, and byte-identical
//!
//! Shipped with `expected_move_model_enable = false` and a zeroed table. Every call
//! returns `Unknown`, the gate uses `gate_expected_move_bps` exactly as before, and no
//! decision number moves. Arming it requires filling the table from a real replay
//! corpus and clearing the full Amendment A-11 leg set — pre-registered rule, two-sided
//! test, pre-existing corpora as arbiter, materiality, no hazard harm.
//!
//! **This module contains no alpha. It is the correctly-shaped, correctly-guarded place
//! that alpha would go, and the discipline that stops us pretending we have some.**

use crate::curve_state;

/// Number of curve-progress strata. Eight pre-graduation bands of 1,250 bps each,
/// plus a ninth for anything at or past graduation.
pub const N_BANDS: usize = 9;

/// The band index a curve-progress reading falls in. `8` is post-graduation.
#[must_use]
pub const fn band_of(curve_progress_bps: u32) -> usize {
    if curve_progress_bps >= 10_000 {
        return N_BANDS - 1;
    }
    (curve_progress_bps / 1_250) as usize
}

/// The band a SOL-side reserve falls in, straight from the curve.
#[must_use]
pub fn band_of_vsol(vsol_lamports: u64) -> usize {
    band_of(curve_state::curve_progress_bps(vsol_lamports))
}

/// One stratum's accumulated evidence. Integer-only (§22), bounded (§99).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Episodes observed in this stratum.
    pub n: u32,
    /// Sum of realized forward returns, basis points. Signed: losses count.
    pub sum_bps: i64,
}

/// Estimator tuning. Mirrors the brain's recall parameters deliberately — the same
/// refusal discipline, the same shrinkage shape, so there is one idea to audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveParams {
    /// Minimum episodes in a stratum before it may answer at all (cf. §46).
    pub min_sample: u32,
    /// Pseudo-count for shrinkage toward the prior — the hierarchical partial-pooling
    /// weight, identical in form to `Engine::conditional_edge_bps`.
    pub prior_weight: u32,
    /// The cold-start prior itself, bps: `Config::gate_expected_move_bps`.
    pub prior_bps: u32,
}

/// Why an estimate could not be produced. Diagnostics only — never a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveUnknown {
    /// The law is off. The shipped state.
    Disarmed,
    /// No episode has ever been recorded anywhere. The shipped table.
    EmptyTable,
    /// This stratum exists but is under the sample floor.
    BelowSampleFloor { n: u32, need: u32 },
}

/// A produced estimate, with the evidence behind it attached so a caller can never
/// see the number without also seeing how thin it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveEstimate {
    /// Shrunk expected favourable move, bps. Never negative: a negative estimate
    /// means "do not trade", which the gate expresses as a refusal, not as a size.
    pub bps: u32,
    /// Episodes in the stratum this came from.
    pub n: u32,
    /// The stratum index, for journalling and attribution.
    pub band: usize,
}

/// The estimator's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveVerdict {
    Known(MoveEstimate),
    Unknown(MoveUnknown),
}

impl MoveVerdict {
    /// The bps a caller should use, or `None` to fall back to the configured constant.
    ///
    /// Deliberately not `unwrap_or(prior)`: the fallback must be visible at the call
    /// site so that "we used the constant" is a journalled fact, not a silent default.
    #[must_use]
    pub const fn known_bps(&self) -> Option<u32> {
        match self {
            Self::Known(e) => Some(e.bps),
            Self::Unknown(_) => None,
        }
    }
}

/// The stratified table. Fixed-size, so state is bounded by construction (§99).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveTable {
    cells: [Cell; N_BANDS],
}

impl MoveTable {
    /// An empty table — the shipped state. Every lookup refuses.
    #[must_use]
    pub const fn empty() -> Self {
        Self { cells: [Cell { n: 0, sum_bps: 0 }; N_BANDS] }
    }

    /// Whether any evidence at all has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|c| c.n == 0)
    }

    /// Total episodes across all strata.
    #[must_use]
    pub fn total_n(&self) -> u64 {
        self.cells.iter().map(|c| u64::from(c.n)).sum()
    }

    /// Read one stratum's raw evidence.
    #[must_use]
    pub fn cell(&self, band: usize) -> Cell {
        self.cells.get(band).copied().unwrap_or_default()
    }

    /// Record one realized outcome into its stratum.
    ///
    /// `realized_bps` is the forward return actually achieved from the decision point,
    /// signed. Saturating throughout: a corpus large enough to overflow an `i64` of
    /// basis points does not exist, but §22 does not permit the assumption.
    pub fn record(&mut self, curve_progress_bps: u32, realized_bps: i64) {
        let b = band_of(curve_progress_bps);
        if let Some(c) = self.cells.get_mut(b) {
            c.n = c.n.saturating_add(1);
            c.sum_bps = c.sum_bps.saturating_add(realized_bps);
        }
    }

    /// Estimate the expected favourable move for a candidate at `vsol_lamports`.
    ///
    /// Shrinkage: `(Σ realized + prior · k) / (n + k)`, the same hierarchical partial
    /// pooling `conditional_edge_bps` already uses, so there is one estimator shape in
    /// the codebase rather than two.
    ///
    /// A shrunk estimate at or below zero is reported as `0`, which the gate reads as
    /// "no priced move" and refuses on. That is the honest encoding: the estimator's
    /// job is to say how much move it expects, and the gate's job is to decide that
    /// zero is not enough to pay 300 bps of cost.
    #[must_use]
    pub fn estimate(&self, vsol_lamports: u64, p: MoveParams) -> MoveVerdict {
        if self.is_empty() {
            return MoveVerdict::Unknown(MoveUnknown::EmptyTable);
        }
        let band = band_of_vsol(vsol_lamports);
        let c = self.cell(band);
        if c.n < p.min_sample {
            return MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor {
                n: c.n,
                need: p.min_sample,
            });
        }
        let k = i64::from(p.prior_weight.max(1));
        let num = c.sum_bps.saturating_add(i64::from(p.prior_bps).saturating_mul(k));
        let den = i64::from(c.n).saturating_add(k);
        let shrunk = num / den.max(1);
        MoveVerdict::Known(MoveEstimate {
            bps: u32::try_from(shrunk.max(0)).unwrap_or(u32::MAX),
            n: c.n,
            band,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: MoveParams = MoveParams { min_sample: 30, prior_weight: 30, prior_bps: 1_800 };

    /// **THE SHIPPED STATE.** Empty table ⇒ every lookup refuses ⇒ the gate uses the
    /// constant ⇒ no decision number can move. This is what makes the wiring safe.
    #[test]
    fn the_shipped_table_refuses_everywhere() {
        let t = MoveTable::empty();
        assert!(t.is_empty());
        assert_eq!(t.total_n(), 0);
        for vsol in [30_000_000_000u64, 61_740_908_643, 92_038_689_690, 115_005_359_056] {
            assert_eq!(
                t.estimate(vsol, P),
                MoveVerdict::Unknown(MoveUnknown::EmptyTable),
                "an uncalibrated estimator must never produce a number"
            );
            assert_eq!(t.estimate(vsol, P).known_bps(), None);
        }
    }

    /// A stratum below the sample floor refuses, and says exactly how short it is.
    #[test]
    fn a_thin_stratum_refuses_and_reports_its_thinness() {
        let mut t = MoveTable::empty();
        // 29 observations in the $9k band — one short of the floor.
        for _ in 0..29 {
            t.record(curve_state::curve_progress_bps(61_740_908_643), 500);
        }
        match t.estimate(61_740_908_643, P) {
            MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor { n, need }) => {
                assert_eq!((n, need), (29, 30));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // One more crosses the floor.
        t.record(curve_state::curve_progress_bps(61_740_908_643), 500);
        assert!(t.estimate(61_740_908_643, P).known_bps().is_some());
    }

    /// Evidence pulls the estimate away from the prior, and shrinkage bounds how fast.
    /// With `n == prior_weight` the answer sits exactly halfway — the property that
    /// makes the pooling auditable by hand.
    #[test]
    fn shrinkage_is_hierarchical_and_exactly_halfway_at_n_equals_k() {
        let mut t = MoveTable::empty();
        let prog = curve_state::curve_progress_bps(61_740_908_643);
        for _ in 0..30 {
            t.record(prog, 200); // realized 200 bps, well under the 1_800 prior
        }
        let e = match t.estimate(61_740_908_643, P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(e.n, 30);
        assert_eq!(e.bps, (200 + 1_800) / 2, "n == k must land exactly halfway");
    }

    /// **A STRATUM THAT LOSES MONEY MUST BE ABLE TO SAY SO.** The estimator floors at
    /// zero rather than reporting a negative move, because a negative expected move is
    /// a refusal for the gate to make, not a size. Zero is the honest encoding: it
    /// cannot clear any positive cost, so the gate refuses.
    #[test]
    fn a_losing_stratum_estimates_zero_and_cannot_clear_any_cost() {
        let mut t = MoveTable::empty();
        let prog = curve_state::curve_progress_bps(45_000_000_000);
        for _ in 0..300 {
            t.record(prog, -4_000); // consistently -40%
        }
        let e = match t.estimate(45_000_000_000, P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(e.bps, 0, "a losing stratum must not report a positive move");
        // ~300 bps is the measured round-trip cost in the target band; zero cannot pay it.
        assert!(e.bps < 300);
    }

    /// Strata are independent: evidence in one band must not leak into another. This is
    /// the property that makes the stratification meaningful rather than decorative.
    #[test]
    fn strata_do_not_leak_into_one_another() {
        let mut t = MoveTable::empty();
        let early = curve_state::curve_progress_bps(35_000_000_000);
        for _ in 0..100 {
            t.record(early, 9_000);
        }
        assert!(t.estimate(35_000_000_000, P).known_bps().is_some());
        // A different band remains uncalibrated and still refuses.
        match t.estimate(92_038_689_690, P) {
            MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor { n, .. }) => assert_eq!(n, 0),
            other => panic!("a neighbouring band must not borrow evidence: {other:?}"),
        }
    }

    /// Band assignment covers the whole curve exactly once, and graduation lands in
    /// the dedicated post-graduation stratum rather than folding into band 7.
    #[test]
    fn bands_partition_the_curve_and_graduation_is_its_own_stratum() {
        assert_eq!(band_of(0), 0);
        assert_eq!(band_of(1_249), 0);
        assert_eq!(band_of(1_250), 1);
        assert_eq!(band_of(9_999), 7);
        assert_eq!(band_of(10_000), 8, "graduation is its own stratum");
        assert_eq!(band_of(u32::MAX), 8);
        assert_eq!(band_of_vsol(curve_state::LAUNCH_VSOL_LAMPORTS), 0);
        assert_eq!(band_of_vsol(curve_state::GRADUATION_VSOL_LAMPORTS), 8);
        // The operator's target band spans strata 2 and 5 — it is not one cell, which
        // is why it can be measured for internal structure rather than assumed uniform.
        assert_eq!(band_of_vsol(61_740_908_643), 2, "$9k sits in stratum 2 (37% of curve)");
        assert_eq!(band_of_vsol(92_038_689_690), 5, "$20k sits in stratum 5 (72% of curve)");
    }

    /// A table is bounded by construction: unbounded recording never grows state.
    #[test]
    fn state_is_bounded_under_unbounded_recording() {
        let mut t = MoveTable::empty();
        for i in 0..50_000u32 {
            t.record(i % 12_000, i64::from(i % 700) - 350);
        }
        assert_eq!(core::mem::size_of_val(&t), core::mem::size_of::<MoveTable>());
        assert_eq!(t.total_n(), 50_000);
    }
}
