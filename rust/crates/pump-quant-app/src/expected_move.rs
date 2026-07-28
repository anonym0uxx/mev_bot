//! **EXPECTED MOVE — the per-candidate estimate that replaces the constant, and the
//! path by which every signal the engine computes finally reaches the money decision.**
//!
//! # The defect this exists to close
//!
//! `gate::decide` sizes and admits from `size_band(cfg.gate_expected_move_bps, …)`.
//! Every *other* input to that call — `liquidity_lamports`, `sellable_depth_lamports`,
//! the impact curve, the fixed fee — is a **measured, per-candidate cost or capacity**
//! term. The **benefit** term was a single global constant.
//!
//! So the engine's admission rule was, in full: *"assume every token in the universe has
//! the same expected favourable move, then check whether that assumption beats this
//! token's measured cost."* Meanwhile the system computes buy pressure, unique buyers,
//! market age, holder concentration, narrative velocity, creator state, whale flow and
//! episodic recall — **and none of it could reach the number that decides whether a
//! trade is worth taking.** `docs/EDGE_PROVENANCE_2026-07-27.md §4` establishes this is
//! the largest unpriced assumption in the codebase.
//!
//! **It should reach it.** A desk that priced every trade off one assumed move would not
//! survive a quarter. This module is the path — but the path has a mandatory station,
//! and the whole design turns on what that station is.
//!
//! # The station: signals reach money only through CALIBRATION
//!
//! A raw signal cannot be subtracted from a cost. `Candidate::discovery_score` is, by its
//! own doc, "raw discovery score in caller-defined fixed-point units" — an **ordinal
//! salience rank**. The gate's arithmetic is in **basis points of expected return**.
//! Feeding one into the other is not aggressive, it is dimensionally meaningless: it
//! would produce a number with no interpretation that nonetheless moves real size. That
//! is why `engine.rs` carries the bar *"expected net SOL, never raw discovery score"*,
//! and the bar is right.
//!
//! What a desk actually does is two steps, never one:
//!
//! 1. signals → a **calibrated forecast of expected return**, fitted on realized
//!    outcomes, each coefficient earned;
//! 2. forecast → the economic decision.
//!
//! This module is step 1, with the coefficients deliberately absent until data supplies
//! them. **A hand-weighted composite score would collapse the two steps into one and
//! smuggle the arbitrariness into the weights** — still a constant, but wearing enough
//! machinery to look principled and therefore much harder to retire.
//!
//! # The structure: ADDITIVE MARGINAL EFFECTS, not a joint table
//!
//! The obvious way to let many signals condition an estimate is to stratify on their
//! cross-product. It is also unaffordable, and the arithmetic says so precisely.
//!
//! With 9 curve strata, 4 signals and 5 bands each, at a 30-observation floor:
//!
//! | scheme | cells | episodes needed |
//! |---|---|---|
//! | joint (full cross-product) | 5,625 | **168,750** |
//! | **marginal (this module)** | **29** | **870** |
//!
//! **194× fewer.** A replay corpus of ~50,000 launches yields on the order of a few
//! thousand tradeable episodes — comfortably enough for the marginal model, nowhere near
//! enough for the joint one. A joint table fed that corpus would not fail loudly; it
//! would answer confidently from four observations a cell, which is worse than the
//! constant it replaced.
//!
//! So the estimate decomposes:
//!
//! ```text
//!   expected_move_bps = base(curve_progress)            <- must be calibrated, or refuse
//!                     + Σ lift(signal_b)                <- each earns its place separately
//! ```
//!
//! where `lift` is a signal band's realized mean **minus the global realized mean** —
//! the marginal excess return associated with being in that band, which is the only
//! quantity it is legitimate to add.
//!
//! ## The asymmetry that makes this safe
//!
//! * The **base** must clear its own sample floor or the whole estimate refuses and the
//!   gate falls back to the configured constant. No base, no opinion.
//! * Each **lift** contributes only if *its own* band clears the floor. An uncalibrated
//!   signal contributes exactly **zero** — never a guess, never a default. **Adding more
//!   signals therefore cannot add more fabricated edge**, which is what makes it safe to
//!   wire everything the engine knows.
//!
//! ## The known flaw, named rather than hidden
//!
//! Marginal effects **double-count correlated signals**. Buy pressure and unique buyers
//! are surely correlated; if each marginally carries +200 bps, adding both claims +400
//! where the true joint lift might be +250. Three guards, in increasing bluntness:
//! each single lift is clamped to [`MAX_SINGLE_LIFT_BPS`]; their sum is clamped to
//! [`MAX_TOTAL_LIFT_BPS`]; and the total may never exceed the base itself, so signals
//! can modulate the estimate but never manufacture one.
//!
//! The principled fix is **sequential residual calibration** — fit signal *k* against
//! what signals *1…k−1* left unexplained — which removes the double-count exactly but
//! needs both an ordering and more data than a first corpus will provide. It is the
//! documented upgrade path, not a substitute for the caps.
//!
//! # Status: DISARMED, empty, byte-identical
//!
//! Shipped with `expected_move_model_enable = false` and zeroed tables. Every call
//! returns `Unknown`, the gate uses `gate_expected_move_bps` exactly as before, and no
//! decision number moves. Arming requires filling the tables from a real replay corpus
//! and clearing the full Amendment A-11 leg set.
//!
//! **This module contains no alpha. It is the correctly-shaped, correctly-guarded place
//! that alpha goes, and the discipline that stops us pretending we have some.**

use crate::curve_state;

/// Number of curve-progress strata for the BASE term. Eight pre-graduation bands of
/// 1,250 bps each, plus a ninth for anything at or past graduation.
pub const N_BANDS: usize = 9;

/// Bands per conditioning signal.
pub const N_SIGNAL_BANDS: usize = 5;

/// Cap on any single signal's lift, bps. A ~300 bps round trip in the operator band
/// means one signal may move the estimate by at most about two round trips — enough to
/// matter, not enough to dominate.
pub const MAX_SINGLE_LIFT_BPS: i64 = 600;

/// Cap on the summed lift, bps. Deliberately far below the sum of the individual caps
/// (`4 × 600 = 2_400`), because independent marginal effects over correlated signals
/// over-add. See "the known flaw", above.
pub const MAX_TOTAL_LIFT_BPS: i64 = 1_000;

/// The conditioning signals the admission economics may see. Every one of these is
/// already computed by the engine and already reaches discovery ranking or sizing;
/// what is new is that they can now reach the ADMISSION decision — once calibrated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalKind {
    /// Windowed buy pressure, bps (`Features::buy_pressure_bp`).
    BuyPressure,
    /// Entity-deduplicated distinct buyers (`Features::unique_buyers`).
    UniqueBuyers,
    /// Market age at decision, slots (`Features::age_slots`).
    AgeSlots,
    /// Holder-distribution concentration band. Supplied by the caller because
    /// `ConcentrationReading` distinguishes *measured-low* from *unknown* and that
    /// distinction must not be flattened into a numeric bucket here (see
    /// `pump_quant_brain::concentration`).
    Concentration,
}

/// Count of conditioning signals.
pub const N_SIGNALS: usize = 4;

impl SignalKind {
    /// Stable index for the table. Never reordered — it is part of the replay identity.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::BuyPressure => 0,
            Self::UniqueBuyers => 1,
            Self::AgeSlots => 2,
            Self::Concentration => 3,
        }
    }

    /// Every signal, in index order.
    pub const ALL: [Self; N_SIGNALS] = [
        Self::BuyPressure,
        Self::UniqueBuyers,
        Self::AgeSlots,
        Self::Concentration,
    ];
}

/// Band a buy-pressure reading (bps) into one of [`N_SIGNAL_BANDS`].
#[must_use]
pub const fn band_buy_pressure(bp: u32) -> usize {
    match bp {
        0..=1_999 => 0,
        2_000..=3_999 => 1,
        4_000..=5_999 => 2,
        6_000..=7_999 => 3,
        _ => 4,
    }
}

/// Band a distinct-buyer count.
#[must_use]
pub const fn band_unique_buyers(n: u32) -> usize {
    match n {
        0..=4 => 0,
        5..=14 => 1,
        15..=39 => 2,
        40..=99 => 3,
        _ => 4,
    }
}

/// Band a market age in slots. Roughly: seconds, a minute, ten minutes, an hour, older.
#[must_use]
pub const fn band_age_slots(slots: u32) -> usize {
    match slots {
        0..=49 => 0,
        50..=199 => 1,
        200..=999 => 2,
        1_000..=4_999 => 3,
        _ => 4,
    }
}

/// The conditioning observation for one candidate. `None` on a signal means **not
/// observed**, which is distinct from a zero reading and contributes no lift — a
/// missing measurement must never look like a neutral one (§6.4/§18.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignalObs {
    bands: [Option<usize>; N_SIGNALS],
}

impl SignalObs {
    /// Nothing observed. The safe construction: every lift is zero.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            bands: [None; N_SIGNALS],
        }
    }

    /// Record one signal's band. Out-of-range band indices are dropped rather than
    /// clamped: a caller that computed a nonsense band must not silently get band 4.
    #[must_use]
    pub fn with(mut self, kind: SignalKind, band: usize) -> Self {
        if band < N_SIGNAL_BANDS {
            self.bands[kind.index()] = Some(band);
        }
        self
    }

    /// Build the three microstructure signals straight from a candidate's feature
    /// snapshot. Concentration is added separately by the caller that holds it.
    #[must_use]
    pub fn from_features(buy_pressure_bp: u32, unique_buyers: u32, age_slots: u32) -> Self {
        Self::none()
            .with(SignalKind::BuyPressure, band_buy_pressure(buy_pressure_bp))
            .with(SignalKind::UniqueBuyers, band_unique_buyers(unique_buyers))
            .with(SignalKind::AgeSlots, band_age_slots(age_slots))
    }

    /// The band recorded for a signal, if any.
    #[must_use]
    pub const fn band(&self, kind: SignalKind) -> Option<usize> {
        self.bands[kind.index()]
    }
}

/// One stratum's accumulated evidence. Integer-only (§22), bounded (§99).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Episodes observed in this stratum.
    pub n: u32,
    /// Sum of realized forward returns, basis points. Signed: losses count.
    pub sum_bps: i64,
}

impl Cell {
    /// Mean realized return, bps. `None` on an empty cell — never a zero, which would
    /// be indistinguishable from a genuinely break-even stratum.
    #[must_use]
    pub const fn mean_bps(&self) -> Option<i64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_bps / self.n as i64)
        }
    }
}

/// Estimator tuning. Mirrors the brain's recall parameters deliberately — the same
/// refusal discipline, the same shrinkage shape, so there is one idea to audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveParams {
    /// Minimum episodes in a stratum before it may answer at all (cf. §46). Applies
    /// independently to the base stratum and to each signal band.
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
    /// The BASE stratum exists but is under the sample floor. No base, no opinion.
    BelowSampleFloor { n: u32, need: u32 },
}

/// A produced estimate, with its full provenance attached so a caller can never see the
/// number without also seeing how it was assembled and how thin the evidence is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveEstimate {
    /// Final expected favourable move, bps. Never negative: a negative estimate means
    /// "do not trade", which the gate expresses as a refusal, not as a size.
    pub bps: u32,
    /// The base term before any signal lift, bps.
    pub base_bps: u32,
    /// Summed, capped signal lift actually applied, bps (signed).
    pub lift_bps: i64,
    /// How many of the conditioning signals were calibrated enough to contribute.
    pub signals_applied: u32,
    /// Episodes behind the base stratum.
    pub n: u32,
    /// The curve-progress stratum index, for journalling and attribution.
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

/// The curve-progress band a reading falls in. `8` is post-graduation.
#[must_use]
pub const fn band_of(curve_progress_bps: u32) -> usize {
    if curve_progress_bps >= 10_000 {
        return N_BANDS - 1;
    }
    (curve_progress_bps / 1_250) as usize
}

/// The curve-progress band a SOL-side reserve falls in, straight from the curve.
#[must_use]
pub fn band_of_vsol(vsol_lamports: u64) -> usize {
    band_of(curve_state::curve_progress_bps(vsol_lamports))
}

/// The stratified table: one base stratification over curve progress, plus one marginal
/// table per conditioning signal. Fixed-size, so state is bounded by construction (§99).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveTable {
    base: [Cell; N_BANDS],
    lift: [[Cell; N_SIGNAL_BANDS]; N_SIGNALS],
    /// Global pool across every episode — the reference the marginal lifts are measured
    /// against. Without it a "lift" would be an absolute level, not an excess.
    global: Cell,
}

impl Default for MoveTable {
    fn default() -> Self {
        Self::empty()
    }
}

impl MoveTable {
    /// An empty table — the shipped state. Every lookup refuses.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            base: [Cell { n: 0, sum_bps: 0 }; N_BANDS],
            lift: [[Cell { n: 0, sum_bps: 0 }; N_SIGNAL_BANDS]; N_SIGNALS],
            global: Cell { n: 0, sum_bps: 0 },
        }
    }

    /// Whether any evidence at all has been recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.global.n == 0
    }

    /// Total episodes recorded.
    #[must_use]
    pub const fn total_n(&self) -> u32 {
        self.global.n
    }

    /// Read one curve-progress stratum's raw evidence.
    #[must_use]
    pub fn base_cell(&self, band: usize) -> Cell {
        self.base.get(band).copied().unwrap_or_default()
    }

    /// Read one signal band's raw evidence.
    #[must_use]
    pub fn lift_cell(&self, kind: SignalKind, band: usize) -> Cell {
        self.lift[kind.index()]
            .get(band)
            .copied()
            .unwrap_or_default()
    }

    /// Record one realized outcome into the base stratum, the global pool, and every
    /// signal band that was observed for it.
    ///
    /// `realized_bps` is the forward return actually achieved from the decision point,
    /// signed. Saturating throughout: a corpus large enough to overflow an `i64` of
    /// basis points does not exist, but §22 does not permit the assumption.
    pub fn record(&mut self, vsol_lamports: u64, obs: SignalObs, realized_bps: i64) {
        let bump = |c: &mut Cell| {
            c.n = c.n.saturating_add(1);
            c.sum_bps = c.sum_bps.saturating_add(realized_bps);
        };
        bump(&mut self.global);
        if let Some(c) = self.base.get_mut(band_of_vsol(vsol_lamports)) {
            bump(c);
        }
        for kind in SignalKind::ALL {
            if let Some(b) = obs.band(kind) {
                if let Some(c) = self.lift[kind.index()].get_mut(b) {
                    bump(c);
                }
            }
        }
    }

    /// Estimate the expected favourable move for a candidate.
    ///
    /// Base: `(Σ realized + prior · k) / (n + k)` over the curve-progress stratum — the
    /// same hierarchical partial pooling `conditional_edge_bps` uses, so there is one
    /// estimator shape in the codebase rather than two.
    ///
    /// Lift: for each signal whose band cleared the floor, its realized mean minus the
    /// global realized mean, clamped to [`MAX_SINGLE_LIFT_BPS`]; the sum is clamped to
    /// [`MAX_TOTAL_LIFT_BPS`] and then to the base itself, so signals can modulate the
    /// estimate but never manufacture one.
    ///
    /// A result at or below zero is reported as `0`, which the gate reads as "no priced
    /// move" and refuses on. That is the honest encoding: the estimator says how much
    /// move it expects, and the gate decides that zero cannot pay ~300 bps of cost.
    #[must_use]
    pub fn estimate(&self, vsol_lamports: u64, obs: SignalObs, p: MoveParams) -> MoveVerdict {
        if self.is_empty() {
            return MoveVerdict::Unknown(MoveUnknown::EmptyTable);
        }
        let band = band_of_vsol(vsol_lamports);
        let bc = self.base_cell(band);
        if bc.n < p.min_sample {
            return MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor {
                n: bc.n,
                need: p.min_sample,
            });
        }
        let k = i64::from(p.prior_weight.max(1));
        let base = (bc
            .sum_bps
            .saturating_add(i64::from(p.prior_bps).saturating_mul(k)))
            / i64::from(bc.n).saturating_add(k).max(1);

        // Marginal lifts, measured against the global mean so they are EXCESSES.
        let global_mean = self.global.mean_bps().unwrap_or(0);
        let mut lift_sum: i64 = 0;
        let mut applied: u32 = 0;
        for kind in SignalKind::ALL {
            let Some(b) = obs.band(kind) else { continue };
            let c = self.lift_cell(kind, b);
            if c.n < p.min_sample {
                continue; // uncalibrated signals contribute EXACTLY zero
            }
            let Some(m) = c.mean_bps() else { continue };
            lift_sum = lift_sum
                .saturating_add((m - global_mean).clamp(-MAX_SINGLE_LIFT_BPS, MAX_SINGLE_LIFT_BPS));
            applied += 1;
        }
        // Correlated marginals over-add: cap the total, then never let signals exceed
        // the base, so they modulate an estimate rather than create one.
        let lift = lift_sum
            .clamp(-MAX_TOTAL_LIFT_BPS, MAX_TOTAL_LIFT_BPS)
            .clamp(-base.max(0), base.max(0));

        let total = base.saturating_add(lift).max(0);
        MoveVerdict::Known(MoveEstimate {
            bps: u32::try_from(total).unwrap_or(u32::MAX),
            base_bps: u32::try_from(base.max(0)).unwrap_or(u32::MAX),
            lift_bps: lift,
            signals_applied: applied,
            n: bc.n,
            band,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: MoveParams = MoveParams {
        min_sample: 30,
        prior_weight: 30,
        prior_bps: 1_800,
    };
    /// A reserve inside the operator's $9k–$20k band.
    const IN_BAND_VSOL: u64 = 61_740_908_643;

    fn obs() -> SignalObs {
        SignalObs::from_features(6_500, 45, 300)
    }

    /// **THE SHIPPED STATE.** Empty table ⇒ every lookup refuses ⇒ the gate uses the
    /// constant ⇒ no decision number can move. This is what makes the wiring safe.
    #[test]
    fn the_shipped_table_refuses_everywhere() {
        let t = MoveTable::empty();
        assert!(t.is_empty());
        assert_eq!(t.total_n(), 0);
        for vsol in [
            30_000_000_000u64,
            IN_BAND_VSOL,
            92_038_689_690,
            115_005_359_056,
        ] {
            assert_eq!(
                t.estimate(vsol, obs(), P),
                MoveVerdict::Unknown(MoveUnknown::EmptyTable),
                "an uncalibrated estimator must never produce a number"
            );
            assert_eq!(t.estimate(vsol, obs(), P).known_bps(), None);
        }
    }

    /// **THE LOAD-BEARING SAFETY PROPERTY.** An uncalibrated signal contributes EXACTLY
    /// zero — not a guess, not a neutral default. This is what makes it safe to wire in
    /// every signal the engine knows: adding signals cannot add fabricated edge.
    #[test]
    fn uncalibrated_signals_contribute_exactly_zero() {
        let mut t = MoveTable::empty();
        // Calibrate ONLY the base stratum; leave every signal band empty by recording
        // with no observations attached.
        for _ in 0..60 {
            t.record(IN_BAND_VSOL, SignalObs::none(), 900);
        }
        let with_signals = match t.estimate(IN_BAND_VSOL, obs(), P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        let without = match t.estimate(IN_BAND_VSOL, SignalObs::none(), P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(
            with_signals.signals_applied, 0,
            "no signal band is calibrated"
        );
        assert_eq!(with_signals.lift_bps, 0);
        assert_eq!(
            with_signals.bps, without.bps,
            "presenting signals the table cannot price must change NOTHING"
        );
    }

    /// **A CALIBRATED SIGNAL REACHES THE MONEY.** This is the property the operator
    /// asked for: once a signal band has earned its coefficient from realized outcomes,
    /// it moves the admission estimate.
    #[test]
    fn a_calibrated_signal_moves_the_admission_estimate() {
        let mut t = MoveTable::empty();
        let strong = SignalObs::none().with(SignalKind::BuyPressure, band_buy_pressure(9_000));
        let weak = SignalObs::none().with(SignalKind::BuyPressure, band_buy_pressure(500));
        // Base stratum plus two buy-pressure bands with genuinely different outcomes.
        for _ in 0..40 {
            t.record(IN_BAND_VSOL, strong, 1_500);
            t.record(IN_BAND_VSOL, weak, -500);
        }
        let hi = t.estimate(IN_BAND_VSOL, strong, P).known_bps().unwrap();
        let lo = t.estimate(IN_BAND_VSOL, weak, P).known_bps().unwrap();
        assert!(
            hi > lo,
            "a signal band with better realized outcomes must price higher ({hi} vs {lo})"
        );
        // And the lift is an EXCESS over the global mean, so the two straddle it.
        let neutral = t
            .estimate(IN_BAND_VSOL, SignalObs::none(), P)
            .known_bps()
            .unwrap();
        assert!(lo < neutral && neutral < hi, "{lo} < {neutral} < {hi}");
    }

    /// **THE CAPS HOLD.** Correlated marginals over-add, so no single signal may run
    /// away and their sum is bounded — including by the base itself, so signals can
    /// modulate an estimate but never manufacture one.
    #[test]
    fn signal_lift_is_bounded_singly_and_in_total() {
        let mut t = MoveTable::empty();
        // Four signals all pointing the same way, hugely, on a modest base.
        let all = SignalObs::none()
            .with(SignalKind::BuyPressure, 4)
            .with(SignalKind::UniqueBuyers, 4)
            .with(SignalKind::AgeSlots, 4)
            .with(SignalKind::Concentration, 4);
        for _ in 0..50 {
            t.record(IN_BAND_VSOL, all, 50_000); // absurd +500% outcomes
            t.record(IN_BAND_VSOL, SignalObs::none(), -100);
        }
        let e = match t.estimate(IN_BAND_VSOL, all, P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(e.signals_applied, 4);
        assert!(
            e.lift_bps <= MAX_TOTAL_LIFT_BPS,
            "total lift {} must respect the cap {MAX_TOTAL_LIFT_BPS}",
            e.lift_bps
        );
        assert!(
            e.lift_bps <= i64::from(e.base_bps),
            "signals must never exceed the base ({} vs base {})",
            e.lift_bps,
            e.base_bps
        );
        assert!(
            e.bps <= e.base_bps * 2,
            "the estimate cannot more than double on signals"
        );
    }

    /// **NO BASE, NO OPINION.** A calibrated signal cannot rescue an uncalibrated base:
    /// the whole estimate refuses and the gate falls back to the constant.
    #[test]
    fn a_calibrated_signal_cannot_rescue_an_uncalibrated_base() {
        let mut t = MoveTable::empty();
        let strong = SignalObs::none().with(SignalKind::BuyPressure, 4);
        // Pile evidence into a DIFFERENT curve stratum, so the signal band is rich but
        // the queried base stratum is empty.
        for _ in 0..200 {
            t.record(35_000_000_000, strong, 2_000);
        }
        assert!(t.lift_cell(SignalKind::BuyPressure, 4).n >= 200);
        match t.estimate(92_038_689_690, strong, P) {
            MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor { n, need }) => {
                assert_eq!((n, need), (0, 30));
            }
            other => panic!("an uncalibrated base must refuse outright, got {other:?}"),
        }
    }

    /// A stratum below the sample floor refuses, and says exactly how short it is.
    #[test]
    fn a_thin_stratum_refuses_and_reports_its_thinness() {
        let mut t = MoveTable::empty();
        for _ in 0..29 {
            t.record(IN_BAND_VSOL, SignalObs::none(), 500);
        }
        match t.estimate(IN_BAND_VSOL, SignalObs::none(), P) {
            MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor { n, need }) => {
                assert_eq!((n, need), (29, 30));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        t.record(IN_BAND_VSOL, SignalObs::none(), 500);
        assert!(t
            .estimate(IN_BAND_VSOL, SignalObs::none(), P)
            .known_bps()
            .is_some());
    }

    /// Shrinkage is hierarchical: with `n == prior_weight` the base sits exactly halfway
    /// between the realized mean and the prior — the property that makes it hand-auditable.
    #[test]
    fn shrinkage_is_exactly_halfway_at_n_equals_k() {
        let mut t = MoveTable::empty();
        for _ in 0..30 {
            t.record(IN_BAND_VSOL, SignalObs::none(), 200);
        }
        let e = match t.estimate(IN_BAND_VSOL, SignalObs::none(), P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(e.n, 30);
        assert_eq!(
            e.base_bps,
            (200 + 1_800) / 2,
            "n == k must land exactly halfway"
        );
    }

    /// **A LOSING STRATUM MUST BE ABLE TO SAY SO.** Floored at zero rather than reported
    /// negative, because a negative expected move is a refusal for the gate to make.
    #[test]
    fn a_losing_stratum_estimates_zero_and_cannot_clear_any_cost() {
        let mut t = MoveTable::empty();
        for _ in 0..300 {
            t.record(45_000_000_000, SignalObs::none(), -4_000);
        }
        let e = match t.estimate(45_000_000_000, SignalObs::none(), P) {
            MoveVerdict::Known(e) => e,
            other => panic!("expected an estimate, got {other:?}"),
        };
        assert_eq!(e.bps, 0, "a losing stratum must not report a positive move");
        assert!(e.bps < 292, "and cannot clear the ~292 bps band cost floor");
    }

    /// An UNOBSERVED signal is not a zero-valued one. Missing measurement must never
    /// look like a neutral reading (§6.4/§18.2).
    #[test]
    fn an_unobserved_signal_is_not_a_neutral_reading() {
        let mut t = MoveTable::empty();
        let zero_band = SignalObs::none().with(SignalKind::BuyPressure, 0);
        for _ in 0..60 {
            t.record(IN_BAND_VSOL, zero_band, -2_000);
            t.record(IN_BAND_VSOL, SignalObs::none(), 2_000);
        }
        let observed = t.estimate(IN_BAND_VSOL, zero_band, P).known_bps().unwrap();
        let unobserved = t
            .estimate(IN_BAND_VSOL, SignalObs::none(), P)
            .known_bps()
            .unwrap();
        assert!(
            observed < unobserved,
            "an observed weak reading must price below an absent one ({observed} vs {unobserved})"
        );
        assert!(SignalObs::none().band(SignalKind::BuyPressure).is_none());
        assert_eq!(zero_band.band(SignalKind::BuyPressure), Some(0));
    }

    /// Strata are independent: evidence in one curve band must not leak into another.
    #[test]
    fn strata_do_not_leak_into_one_another() {
        let mut t = MoveTable::empty();
        for _ in 0..100 {
            t.record(35_000_000_000, SignalObs::none(), 9_000);
        }
        assert!(t
            .estimate(35_000_000_000, SignalObs::none(), P)
            .known_bps()
            .is_some());
        match t.estimate(92_038_689_690, SignalObs::none(), P) {
            MoveVerdict::Unknown(MoveUnknown::BelowSampleFloor { n, .. }) => assert_eq!(n, 0),
            other => panic!("a neighbouring band must not borrow evidence: {other:?}"),
        }
    }

    /// Band assignment covers the whole curve exactly once, graduation is its own
    /// stratum, and the operator's target band spans strata 2–5.
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
        assert_eq!(
            band_of_vsol(IN_BAND_VSOL),
            2,
            "$9k sits in stratum 2 (37% of curve)"
        );
        assert_eq!(
            band_of_vsol(92_038_689_690),
            5,
            "$20k sits in stratum 5 (72% of curve)"
        );
    }

    /// Signal banding is total, monotone, and never out of range.
    #[test]
    fn signal_banding_is_total_and_monotone() {
        let mut last = 0;
        for bp in (0..12_000).step_by(97) {
            let b = band_buy_pressure(bp);
            assert!(b < N_SIGNAL_BANDS);
            assert!(b >= last, "buy-pressure banding must be monotone");
            last = b;
        }
        assert_eq!(band_unique_buyers(0), 0);
        assert_eq!(band_unique_buyers(u32::MAX), 4);
        assert_eq!(band_age_slots(0), 0);
        assert_eq!(band_age_slots(u32::MAX), 4);
        // An out-of-range band is DROPPED, never clamped into a real one.
        assert!(SignalObs::none()
            .with(SignalKind::BuyPressure, 99)
            .band(SignalKind::BuyPressure)
            .is_none());
    }

    /// **THE DATA-COST ARGUMENT, pinned in arithmetic.** This is why the model is
    /// additive-marginal rather than a joint cross-product: 194x fewer episodes.
    #[test]
    fn the_marginal_decomposition_costs_194x_less_data_than_a_joint_table() {
        const FLOOR: usize = 30;
        let joint = N_BANDS * N_SIGNAL_BANDS.pow(N_SIGNALS as u32);
        let marginal = N_BANDS + N_SIGNALS * N_SIGNAL_BANDS;
        assert_eq!(joint, 5_625);
        assert_eq!(marginal, 29);
        assert_eq!(joint * FLOOR, 168_750, "a joint table needs ~169k episodes");
        assert_eq!(marginal * FLOOR, 870, "the marginal model needs ~870");
        assert!(joint / marginal > 190, "the saving is ~194x");
    }

    /// State is bounded by construction: unbounded recording never grows it.
    #[test]
    fn state_is_bounded_under_unbounded_recording() {
        let mut t = MoveTable::empty();
        for i in 0..50_000u32 {
            let o = SignalObs::from_features(i % 11_000, i % 300, i % 8_000);
            t.record(
                30_000_000_000 + u64::from(i % 90_000) * 1_000_000,
                o,
                i64::from(i % 700) - 350,
            );
        }
        assert_eq!(
            core::mem::size_of_val(&t),
            core::mem::size_of::<MoveTable>()
        );
        assert_eq!(t.total_n(), 50_000);
    }
}
