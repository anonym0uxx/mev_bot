//! §21.7/§70.1 **holder distribution SHAPE** — concentration, early-buyer
//! capture, bundle/sniper presence and flip (bump/wash) behaviour.
//!
//! # Why shape and not size
//!
//! The prior wave built the continuous holder ledger ([`crate::holder_flow`]) and
//! then fed holder-growth ACCELERATION into the §70.1 money proxy, where it
//! measured exactly zero. That zero was correctly diagnosed as an
//! **unreachability** result: `money_of` only reaches an outcome through
//! `nv_candidate_score`, and there attention level and the §29 fade cap dominate
//! rank, so no value the money proxy can produce reorders the promotion set.
//!
//! The external evidence says the reachable, predictive dimension was never the
//! count in the first place:
//!
//! * **MemeTrans (arXiv 2602.13480)**, high-risk Solana memecoin launches, ranks
//!   *Holding Concentration* the **second most important feature group** (removing
//!   it degrades AUPRC materially). Its named members are `dev_hold_pct`,
//!   `top10_hold_pct`, **`early_top10_hold_pct`**, `bundle_hold_pct`, `bundle_num`
//!   — every one a distribution statistic, not a count. Headline effect: **the
//!   first 10 buyers held ~17 percentage points MORE supply in high-risk than in
//!   low-risk tokens.** The same paper measures 28% of holders as bundled accounts
//!   concealing 36.5% of supply, wash trades at 21.4% of pre-migration
//!   transactions, and 73% of memecoins below 40% of migration price within 20
//!   minutes post-migration.
//! * **Memecoin fragility (arXiv 2512.00377)** finds ownership concentration the
//!   strongest fragility signal and proposes a **Whale Dominance Score =
//!   cumulative top-N share × normalized internal inequality (Herfindahl-style)**
//!   — concentration AND its internal skew, because a top-10 that is ten equal
//!   holders is a different market from a top-10 that is one holder and nine
//!   rounding errors. That paper is **cross-sectional, not forecasting**, so it is
//!   taken here as motivation for the FUNCTIONAL FORM and not as a validated
//!   predictor.
//! * **Manipulative bots in copy trading (arXiv 2601.08641)** defines **bundle
//!   bots** as non-creator purchases inside the *creation block*, **sniper bots**
//!   as purchases within the **first 1–5 blocks** (~0.4–2 s, below human reaction
//!   time), and **bump bots** by a "flip ratio" — repeated buy/sell of
//!   near-identical size with minimal net position change. Bundle-bot presence is
//!   associated with reduced returns and shorter dump duration.
//! * **Pump.fun graduation prediction (arXiv 2602.14860)** finds the dominant
//!   predictor to be the SPEED of capital deployment (reaching a SOL level in
//!   fewer trades; median 4.4 min / 457 trades) and — notably — **does not examine
//!   holder features at all**. Concentration is therefore treated here as
//!   COMPLEMENTARY to the capital-velocity signal the engine already has, never as
//!   a substitute for it.
//!
//! Synthesis: this is a **rug/fragility risk** family. It maps to a **reduce-only**
//! lever — a size haircut, and (conjunctively, see below) a pre-entry refusal —
//! and never to a size-up, a boost, or a relaxation of any existing veto.
//!
//! # THE BASIS DISCIPLINE IS THE LOAD-BEARING PART
//!
//! Concentration is a **level** quantity. Every share here has the tracked supply
//! `Σ net` in its denominator, and that denominator is only the TRUE supply when
//! the ledger saw the mint from an empty holder set — i.e. under
//! [`HolderCountBasis::Exact`].
//!
//! * Under [`HolderCountBasis::DeltaOnly`] an unknown number of pre-window holders
//!   exist whose positions are absent from the denominator. Every share is
//!   therefore a share of a **subset**, and is systematically **OVERSTATED** — by
//!   an amount we cannot bound, because the missing mass is exactly the thing we
//!   could not observe. A "top-10 share" computed on a delta-only ledger of six
//!   entities reads 100% on a market with four thousand holders.
//! * Under [`HolderCountBasis::Incomplete`] the ledger is entity-cap truncated, so
//!   the observed set is a subset for a second, independent reason.
//!
//! An overstated concentration reaching a veto would refuse **healthy** markets,
//! which is the single most expensive failure mode this module could ship (and
//! which §21.7(e) names outright: "over-rejection is a defect, not discipline").
//! So the refusal is structural, not a policy:
//! [`ConcentrationVerdict::Unknown`] **carries no estimate field at all** — the
//! same shape as `pump_quant_brain::recall::RecallVerdict` — and
//! [`concentration_of`] returns it for anything short of `Exact` with a
//! non-degenerate ledger. There is no accessor, anywhere, that yields a
//! concentration number from a delta-only basis.
//!
//! # NEVER A STANDALONE VETO (constitution §21.7)
//!
//! The constitution is explicit about this exact feature: bundle-adjusted top-N
//! holding concentration is *"a feature family and prior, **never a standalone
//! veto**, with its veto/downweight effects audited in the
//! ConvexityPreservationLedger like every other rule"*, and separately *"only
//! extreme fabrication signatures may hard-reject"*.
//!
//! [`ConcentrationMetrics::risk`] therefore takes a `corroborated` flag and can
//! only return [`ConcentrationRisk::Veto`] when it is set. The engine supplies it
//! from the §21.7 flow-authenticity screen — an **independent measurement**:
//! authenticity is computed over per-entity *quote-lamport gross flow*, this
//! module over per-entity *base-token net positions*; different quantity,
//! different denominator, different failure mode. Without that second, independent
//! signature an extreme distribution degrades to a haircut, never a refusal.
//!
//! # Purity
//!
//! Integer/fixed-point only (§22), every threshold a named const carrying its
//! citation (§102), no float, no wall clock, no RNG, no allocation (the top-N
//! selection runs in a fixed-size array over a borrowed slice), and bounded by
//! construction — the input is [`crate::holder_flow`]'s already-bounded ledger
//! (§99/§57).

use std::collections::BTreeMap;

use pump_quant_brain::concentration::{
    ConcentrationReading as BrainReading, ConcentrationShape as BrainShape,
    ConcentrationTrajectory as BrainTrajectory, ConcentrationUnknown as BrainUnknown,
    TrajectoryShape as BrainTrajectoryShape, TrajectoryUnknown as BrainTrajectoryUnknown,
};
use pump_quant_features::holder_growth::{
    HOLDER_GROWTH_NORM_NS, HOLDER_MAX_INTERVAL_NS, HOLDER_MIN_INTERVAL_NS,
};

use crate::holder_flow::{HolderCountBasis, HolderFlow, HolderShapeRef, EARLY_ROSTER_CAP};

/// Basis-point scale (100% == 10 000 bps), shared by every ratio here (§22).
const BPS: u128 = 10_000;

/// Cohort size for the cumulative top-N share (§102).
///
/// Ten, matching MemeTrans' `top10_hold_pct` and the fragility paper's top-N
/// cumulative share, and matching [`EARLY_ROSTER_CAP`] so the "all-time top ten"
/// and the "first ten buyers" statistics are the same cohort size and can be
/// compared directly.
pub const TOP_N: usize = 10;

/// Compile-time proof that the two cohorts are the same size (§102: a
/// relationship between named constants is checked, not remembered).
const _: () = assert!(
    TOP_N == EARLY_ROSTER_CAP,
    "the all-time top-N cohort and the early-buyer roster must be the same size \
     for their shares to be comparable"
);

/// Minimum tracked entities before a distribution SHAPE is a measurement rather
/// than an artefact of a short ledger (§6.4).
///
/// Twice [`TOP_N`]. Below `2 · TOP_N` the "top-10 share" cannot discriminate: with
/// ten or fewer tracked holders it is identically 10 000 bps on every market,
/// healthy or not, so it carries zero information while looking like a maximal
/// reading. Twenty is the smallest ledger in which the statistic has any dynamic
/// range at all, and it is a floor on the shape family only — the holder COUNT
/// remains readable below it through [`crate::holder_flow::HolderReading`].
pub const MIN_ENTITIES_FOR_SHAPE: u32 = 2 * TOP_N as u32;

/// Equal-weight reference for the first-ten-buyer share (bps).
///
/// With a [`TOP_N`]-sized early cohort inside a ledger at the
/// [`MIN_ENTITIES_FOR_SHAPE`] floor, a perfectly equal distribution puts the first
/// ten buyers at exactly `TOP_N / MIN_ENTITIES_FOR_SHAPE` = 50% of supply. That is
/// the NEUTRAL point the published effect size is measured against, not a
/// threshold.
pub const EARLY_TOP10_EQUAL_SHARE_BPS: u32 = 10_000 * TOP_N as u32 / MIN_ENTITIES_FOR_SHAPE;

/// The MemeTrans (arXiv 2602.13480) headline effect size, verbatim: the first ten
/// buyers held **~17 percentage points more** supply in high-risk launches than in
/// low-risk ones. 17 pp == 1 700 bps.
///
/// The paper publishes the GAP, not the absolute levels, so the bars below are
/// built as `equal-weight reference + k · gap` rather than lifted from a table
/// that does not exist.
pub const MEMETRANS_EARLY_TOP10_EXCESS_BPS: u32 = 1_700;

/// Early-top-10 share (bps) at which size is cut: the equal-weight reference plus
/// the ENTIRE published high-risk excess. A launch here looks exactly like the
/// high-risk side of the MemeTrans split.
pub const EARLY_TOP10_HAIRCUT_BPS: u32 =
    EARLY_TOP10_EQUAL_SHARE_BPS + MEMETRANS_EARLY_TOP10_EXCESS_BPS;

/// Early-top-10 share (bps) that contributes a veto leg: the reference plus TWICE
/// the published excess — double the measured high-risk separation.
pub const EARLY_TOP10_VETO_BPS: u32 =
    EARLY_TOP10_EQUAL_SHARE_BPS + 2 * MEMETRANS_EARLY_TOP10_EXCESS_BPS;

/// Cumulative top-10 share (bps) at which size is cut.
///
/// 5 000 is not invented here: it is the `max_concentration_bps` the §21.5
/// active-market-universe dossiers in `pump_quant_signals` already use as the
/// server-side selector's concentration bar. Reusing it keeps the app's gate and
/// the selector from disagreeing about what "too concentrated" means.
pub const TOP10_HAIRCUT_BPS: u32 = 5_000;

/// Cumulative top-10 share (bps) that contributes a veto leg.
///
/// **Stated convention, not a fitted value.** arXiv 2512.00377 is cross-sectional
/// and publishes no threshold, so this is the midpoint between the existing §21.5
/// screen bar ([`TOP10_HAIRCUT_BPS`]) and total capture (10 000). A skeptic should
/// attack this number first; it is the least evidence-backed constant in the
/// module and it is labelled as such rather than dressed up.
pub const TOP10_VETO_BPS: u32 = (TOP10_HAIRCUT_BPS + 10_000) / 2;

/// Whale-dominance score (bps) at which size is cut.
///
/// The score is `top-N share × normalized HHI`, a product of two 0..10 000 terms,
/// so its scale is quadratically compressed relative to a share: an equal-weight
/// ledger scores 0 by construction (normalized HHI is 0 at perfect equality). 2 500
/// is the level a single entity holding ~60% inside an otherwise-broad ledger of
/// [`MIN_ENTITIES_FOR_SHAPE`] entities reaches. Also a stated convention.
pub const WHALE_DOMINANCE_HAIRCUT_BPS: u32 = 2_500;

/// Whale-dominance score (bps) that contributes a veto leg — roughly a single
/// entity holding ~80% inside an otherwise-broad ledger. Stated convention.
pub const WHALE_DOMINANCE_VETO_BPS: u32 = 4_500;

/// Reduce-only size multiplier (bps) applied at [`ConcentrationRisk::Haircut`].
///
/// A 40% cut. Sized against the outcome the family predicts rather than the
/// signal's strength: MemeTrans measures 73% of memecoins below 40% of migration
/// price within 20 minutes post-migration, so a concentrated launch is being
/// priced as a materially worse right-tail, not merely a noisier one. It is a
/// multiplier ≤ 10 000 by construction, so it can only ever shrink.
pub const CONCENTRATION_HAIRCUT_MULT_BPS: u32 = 6_000;

/// Cap on the reported flip ratio (bps). 1 000 000 bps == 100× — a hundred units
/// of gross traded base per unit of net position retained. Past this the ratio has
/// no additional decision content and an uncapped value would only invite overflow
/// thinking (§99).
pub const FLIP_RATIO_CAP_BPS: u64 = 1_000_000;

/// The neutral flip ratio (bps): pure accumulation, where every unit bought is
/// still held, gives `gross == net` == 10 000 bps. Values above it are round-trip
/// churn; the ratio cannot go below it.
pub const FLIP_RATIO_NEUTRAL_BPS: u64 = 10_000;

/// Flip-ratio tolerance (bps) before the bump/wash penalty starts.
///
/// 3× the neutral ratio. MemeTrans measures **21.4% of pre-migration transactions
/// as wash trades** as the venue BASELINE, and ordinary scalping (this system
/// included) round-trips positions by design, so a market must be churning well
/// past normal turnover before the ratio is evidence of anything.
pub const FLIP_TOLERANCE_BPS: u64 = 3 * FLIP_RATIO_NEUTRAL_BPS;

/// Bundle+sniper suspect count tolerated before the authenticity penalty starts.
///
/// MemeTrans measures **28% of holders as bundled accounts** as the baseline
/// condition of this venue, so a nonzero bundle cohort is normal and convicts
/// nobody. At the [`MIN_ENTITIES_FOR_SHAPE`] floor, 28% is between 5 and 6
/// entities; the tolerance is set at the published baseline rounded up, so only the
/// excess over the venue's own norm is charged.
pub const BUNDLE_TOLERANCE_COUNT: u32 = 6;

/// Compile-time proof that every veto bar sits strictly above its haircut bar and
/// that the haircut multiplier is reduce-only (§102 — the ordering between named
/// constants is checked at build time, not remembered in a comment).
const _: () = assert!(
    TOP10_VETO_BPS > TOP10_HAIRCUT_BPS
        && EARLY_TOP10_VETO_BPS > EARLY_TOP10_HAIRCUT_BPS
        && WHALE_DOMINANCE_VETO_BPS > WHALE_DOMINANCE_HAIRCUT_BPS
        && CONCENTRATION_HAIRCUT_MULT_BPS < 10_000,
    "concentration bars must be ordered haircut < veto, and the haircut must reduce"
);

/// Why a concentration verdict carries no numbers (§6.4 UNKNOWN discipline).
///
/// Every arm is a REASON, and none of them carries an estimate — see the module
/// docs for why a delta-only concentration must never exist as a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConcentrationUnknown {
    /// The law is not armed, so no reading was taken. Distinguished from every
    /// other arm because it is a CONFIGURATION fact, not an evidence fact — a
    /// consumer inspecting the reason must be able to tell "we looked and could
    /// not tell" apart from "we did not look" (§6.4).
    Disarmed,
    /// The mint has no holder ledger at all (no folded swap yet).
    Untracked,
    /// [`HolderCountBasis::DeltaOnly`]: an unknown number of pre-window holders
    /// are missing from the denominator, so every share would be overstated by an
    /// unbounded amount.
    DeltaOnlyBasis,
    /// [`HolderCountBasis::Incomplete`]: the entity ledger is cap-truncated, so
    /// the observed set is a subset for a second, independent reason.
    IncompleteBasis,
    /// Fewer than [`MIN_ENTITIES_FOR_SHAPE`] tracked entities: the top-N share has
    /// no dynamic range on a ledger this short.
    ThinLedger,
    /// Tracked supply is zero (every observed position has been fully exited), so
    /// there are no shares to take.
    NoTrackedSupply,
}

/// The distribution-shape statistics for one mint. Every field is bps or a count;
/// all integer (§22).
///
/// Reachable **only** through [`ConcentrationVerdict::Known`], which
/// [`concentration_of`] issues only under [`HolderCountBasis::Exact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcentrationMetrics {
    /// Largest single entity's share of tracked supply (bps).
    pub top1_share_bps: u32,
    /// Cumulative share of the [`TOP_N`] largest tracked positions (bps).
    pub top10_share_bps: u32,
    /// Herfindahl index over tracked shares, `Σ share_bps² / 10 000` (bps).
    /// 10 000 = one holder owns everything; `10 000 / n` = perfect equality.
    pub hhi_bps: u32,
    /// HHI rescaled so perfect equality is 0 and total capture is 10 000:
    /// `(hhi − 10 000/n) · 10 000 / (10 000 − 10 000/n)`. This is the "internal
    /// inequality" term of the arXiv 2512.00377 whale-dominance form.
    pub hhi_normalized_bps: u32,
    /// **Whale Dominance Score** (arXiv 2512.00377):
    /// `top10_share_bps · hhi_normalized_bps / 10 000`.
    pub whale_dominance_bps: u32,
    /// Share of tracked supply held by the first [`EARLY_ROSTER_CAP`] DISTINCT
    /// entities ever observed buying this mint (MemeTrans `early_top10_hold_pct`).
    pub early_top10_share_bps: u32,
    /// Distinct entities whose first observed buy landed in the creation slot
    /// (bundle) or within [`crate::holder_flow::SNIPER_SLOT_WINDOW`] slots of it (sniper).
    pub bundle_suspect_count: u32,
    /// The creation-slot half of [`Self::bundle_suspect_count`].
    pub bundle_entities: u32,
    /// The 1..=[`crate::holder_flow::SNIPER_SLOT_WINDOW`] half of [`Self::bundle_suspect_count`].
    pub sniper_entities: u32,
    /// Entities whose first buy carried slot evidence at all — the honest
    /// denominator for the two counters above (§6.4).
    pub aged_first_buys: u32,
    /// Aggregate gross-traded-base over net-position ratio (bps), capped at
    /// [`FLIP_RATIO_CAP_BPS`]. [`FLIP_RATIO_NEUTRAL_BPS`] = pure accumulation;
    /// large values with near-zero net are the bump/wash signature.
    pub flip_ratio_bps: u64,
    /// Entities with a strictly positive tracked position.
    pub holders: u32,
    /// Distinct entities in the ledger (holders plus fully-exited ones).
    pub entities_tracked: u32,
    /// Σ net tracked base position — the denominator every share above was taken
    /// against, published so the reading can be audited rather than trusted.
    pub tracked_supply_base: u128,
}

/// Reduce-only risk tier implied by a distribution shape.
///
/// Ordered by severity. Never a boost, never a relaxation: the worst outcome for a
/// clean market is [`ConcentrationRisk::Clear`], which is the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConcentrationRisk {
    /// Nothing binds. Identity — the market is sized exactly as it would be
    /// without this family.
    Clear,
    /// Size is cut by [`CONCENTRATION_HAIRCUT_MULT_BPS`].
    Haircut,
    /// Pre-entry refusal. Reachable ONLY with independent corroboration (see the
    /// module docs on the constitution's never-a-standalone-veto rule).
    Veto,
}

impl ConcentrationRisk {
    /// Reduce-only size multiplier for this tier (bps, `<= 10_000`).
    ///
    /// [`ConcentrationRisk::Veto`] also returns the haircut multiplier rather than
    /// zero: a veto is expressed by REFUSING at the gate, never by silently sizing
    /// to nothing (§21.7 haircut-not-veto separation — a zero-size admit would be
    /// an unjournaled refusal).
    #[must_use]
    pub const fn size_mult_bp(self) -> u32 {
        match self {
            ConcentrationRisk::Clear => 10_000,
            ConcentrationRisk::Haircut | ConcentrationRisk::Veto => CONCENTRATION_HAIRCUT_MULT_BPS,
        }
    }
}

/// Additional §21.7 authenticity evidence carried out of the holder ledger.
///
/// Kept as its own type so the single-channel rule is visible at the call site:
/// these two quantities enter the sizing chain through the FLOW-AUTHENTICITY
/// multiplier and nowhere else, while the concentration shares enter through the
/// separate fragility haircut. See [`crate::screen::FlowScreen::authenticity_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HolderAuthEvidence {
    /// Bundle + sniper first-buy cohort size.
    pub bundle_suspect_count: u32,
    /// Aggregate flip ratio (bps).
    pub flip_ratio_bps: u64,
}

impl ConcentrationMetrics {
    /// The reduce-only risk tier this shape implies.
    ///
    /// `corroborated` is an INDEPENDENT extraction signature supplied by the
    /// caller. When it is false a veto-grade shape degrades to
    /// [`ConcentrationRisk::Haircut`] — the constitution forbids this family from
    /// vetoing alone, and this method is where that is enforced rather than
    /// remembered.
    #[must_use]
    pub const fn risk(&self, corroborated: bool) -> ConcentrationRisk {
        let veto_leg = self.top10_share_bps >= TOP10_VETO_BPS
            || self.early_top10_share_bps >= EARLY_TOP10_VETO_BPS
            || self.whale_dominance_bps >= WHALE_DOMINANCE_VETO_BPS;
        if veto_leg {
            if corroborated {
                return ConcentrationRisk::Veto;
            }
            return ConcentrationRisk::Haircut;
        }
        let haircut_leg = self.top10_share_bps >= TOP10_HAIRCUT_BPS
            || self.early_top10_share_bps >= EARLY_TOP10_HAIRCUT_BPS
            || self.whale_dominance_bps >= WHALE_DOMINANCE_HAIRCUT_BPS;
        if haircut_leg {
            ConcentrationRisk::Haircut
        } else {
            ConcentrationRisk::Clear
        }
    }

    /// The bundle/flip half of the reading, for the §21.7 authenticity channel.
    #[must_use]
    pub const fn auth_evidence(&self) -> HolderAuthEvidence {
        HolderAuthEvidence {
            bundle_suspect_count: self.bundle_suspect_count,
            flip_ratio_bps: self.flip_ratio_bps,
        }
    }
}

/// A concentration reading, or a labelled refusal to produce one.
///
/// Mirrors `pump_quant_brain::recall::RecallVerdict` deliberately: the `Unknown`
/// arm has **no estimate field**, so "we could not measure this" is not
/// representable as a number and cannot be accidentally consumed as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcentrationVerdict {
    /// The ledger supports a distribution reading.
    Known(ConcentrationMetrics),
    /// It does not, and here is why. No estimate exists, by construction.
    Unknown(ConcentrationUnknown),
}

impl ConcentrationVerdict {
    /// `true` when a reading is available.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The metrics, or `None`. This is the **only** way to reach a number.
    #[must_use]
    pub const fn metrics(&self) -> Option<&ConcentrationMetrics> {
        match self {
            Self::Known(m) => Some(m),
            Self::Unknown(_) => None,
        }
    }

    /// Why the reading was declined, or `None` if it was not.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<ConcentrationUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }

    /// The risk tier, with a `Clear` (identity) fallback for every refusal.
    ///
    /// **THE FAIL-OPEN LAW.** An `Unknown` verdict must leave behaviour exactly as
    /// it was before this family existed. A signal that starts vetoing when it
    /// LACKS data is strictly worse than no signal — it would refuse precisely the
    /// markets it knows least about — so the refusal path is the identity here and
    /// pinned as a test.
    #[must_use]
    pub const fn risk_or_clear(&self, corroborated: bool) -> ConcentrationRisk {
        match self {
            Self::Known(m) => m.risk(corroborated),
            Self::Unknown(_) => ConcentrationRisk::Clear,
        }
    }

    /// The §21.7 authenticity evidence, or the neutral default under `Unknown`.
    #[must_use]
    pub const fn auth_evidence_or_default(&self) -> HolderAuthEvidence {
        match self {
            Self::Known(m) => m.auth_evidence(),
            Self::Unknown(_) => HolderAuthEvidence {
                bundle_suspect_count: 0,
                flip_ratio_bps: 0,
            },
        }
    }

    /// The concentration number the §21.5 active-market screen consumes, or `0`
    /// under `Unknown`.
    ///
    /// Zero is the NEVER-BINDS value for that screen (it compares
    /// `top_holder_concentration_bps <= max_concentration_bps`), so an `Unknown`
    /// verdict reproduces the screen's pre-existing behaviour exactly. This is the
    /// same fail-open law as [`Self::risk_or_clear`], expressed in the screen's own
    /// units.
    #[must_use]
    pub const fn screen_concentration_bps(&self) -> u32 {
        match self {
            Self::Known(m) => m.top10_share_bps,
            Self::Unknown(_) => 0,
        }
    }
}

/// Derive `mint`'s distribution shape from the continuous holder ledger.
///
/// Returns [`ConcentrationVerdict::Known`] **only** when all of the following
/// hold, and [`ConcentrationVerdict::Unknown`] with the binding reason otherwise:
///
/// 1. the mint has a ledger at all;
/// 2. its basis is [`HolderCountBasis::Exact`] — see the module docs for why
///    anything weaker cannot produce a share;
/// 3. at least [`MIN_ENTITIES_FOR_SHAPE`] entities are tracked;
/// 4. tracked supply is strictly positive.
///
/// `O(n · TOP_N)` over the mint's bounded entity ledger, allocation-free.
#[must_use]
pub fn concentration_of(flow: &HolderFlow, mint: &[u8; 32]) -> ConcentrationVerdict {
    let Some(shape) = flow.shape(mint) else {
        return ConcentrationVerdict::Unknown(ConcentrationUnknown::Untracked);
    };
    concentration_of_shape(&shape)
}

/// [`concentration_of`] over an already-borrowed shape view.
#[must_use]
pub fn concentration_of_shape(shape: &HolderShapeRef<'_>) -> ConcentrationVerdict {
    // ---- THE BASIS GATE. Nothing below this point can run on a subset ledger.
    match shape.basis {
        HolderCountBasis::Exact => {}
        HolderCountBasis::DeltaOnly => {
            return ConcentrationVerdict::Unknown(ConcentrationUnknown::DeltaOnlyBasis)
        }
        HolderCountBasis::Incomplete => {
            return ConcentrationVerdict::Unknown(ConcentrationUnknown::IncompleteBasis)
        }
    }
    let entities_tracked = u32::try_from(shape.positions.len()).unwrap_or(u32::MAX);
    if entities_tracked < MIN_ENTITIES_FOR_SHAPE {
        return ConcentrationVerdict::Unknown(ConcentrationUnknown::ThinLedger);
    }

    // ---- Pass 1: supply, holder count, gross, and the top-N selection.
    let mut supply: u128 = 0;
    let mut gross_total: u128 = 0;
    let mut holders: u32 = 0;
    // Descending fixed-size top-N buffer — no allocation, no sort of the ledger.
    let mut top: [u64; TOP_N] = [0; TOP_N];
    for p in shape.positions {
        gross_total = gross_total.saturating_add(u128::from(p.gross()));
        let net = p.net();
        if net == 0 {
            continue;
        }
        holders = holders.saturating_add(1);
        supply = supply.saturating_add(u128::from(net));
        if net > top[TOP_N - 1] {
            let mut i = TOP_N - 1;
            while i > 0 && top[i - 1] < net {
                top[i] = top[i - 1];
                i -= 1;
            }
            top[i] = net;
        }
    }
    if supply == 0 {
        return ConcentrationVerdict::Unknown(ConcentrationUnknown::NoTrackedSupply);
    }

    let share_bps = |v: u128| -> u32 {
        u32::try_from((v.saturating_mul(BPS) / supply).min(BPS)).unwrap_or(10_000)
    };
    let top1_share_bps = share_bps(u128::from(top[0]));
    let top_sum: u128 = top.iter().map(|&v| u128::from(v)).sum();
    let top10_share_bps = share_bps(top_sum);

    // ---- Pass 2: the Herfindahl over tracked shares.
    let mut hhi_acc: u128 = 0;
    for p in shape.positions {
        if p.net() == 0 {
            continue;
        }
        let s = u128::from(share_bps(u128::from(p.net())));
        hhi_acc = hhi_acc.saturating_add(s.saturating_mul(s));
    }
    let hhi_bps = u32::try_from((hhi_acc / BPS).min(BPS)).unwrap_or(10_000);

    // Normalized HHI (arXiv 2512.00377 internal-inequality term): rescale so
    // perfect equality (`hhi == 10_000/n`) is 0 and total capture is 10 000. With
    // a single holder there is no internal distribution to speak of, and the
    // honest reading of "one entity is the entire float" is maximal inequality.
    let hhi_normalized_bps = if holders <= 1 {
        10_000
    } else {
        let min_hhi = 10_000u128 / u128::from(holders);
        let den = 10_000u128.saturating_sub(min_hhi);
        let num = u128::from(hhi_bps)
            .saturating_sub(min_hhi)
            .saturating_mul(BPS);
        // `den == 0` is unreachable for `holders >= 2` (min_hhi <= 5_000), but the
        // division is guarded rather than argued: a zero denominator falls back to
        // maximal inequality, which is the conservative (reduce-only) reading.
        match num.checked_div(den) {
            Some(v) => u32::try_from(v.min(BPS)).unwrap_or(10_000),
            None => 10_000,
        }
    };
    // The whale-dominance functional form: cumulative top-N share × normalized
    // internal inequality. Both terms are 0..10 000, so the product divided by the
    // scale is again 0..10 000.
    let whale_dominance_bps = u32::try_from(
        (u128::from(top10_share_bps).saturating_mul(u128::from(hhi_normalized_bps)) / BPS).min(BPS),
    )
    .unwrap_or(10_000);

    // ---- The MemeTrans early cohort: supply held by the first-ten buyers.
    // The roster holds entity IDS in arrival order; the ledger is sorted by id, so
    // each lookup is a binary search. `EARLY_ROSTER_CAP` of them ⇒ O(10 log n).
    let mut early_sum: u128 = 0;
    for &e in shape.early {
        if let Ok(idx) = shape.positions.binary_search_by_key(&e, |p| p.entity()) {
            if let Some(p) = shape.positions.get(idx) {
                early_sum = early_sum.saturating_add(u128::from(p.net()));
            }
        }
    }
    let early_top10_share_bps = share_bps(early_sum);

    // ---- The bump/wash flip ratio: gross traded base per unit of net retained.
    let flip_ratio_bps = u64::try_from(
        (gross_total.saturating_mul(BPS) / supply).min(u128::from(FLIP_RATIO_CAP_BPS)),
    )
    .unwrap_or(FLIP_RATIO_CAP_BPS);

    ConcentrationVerdict::Known(ConcentrationMetrics {
        top1_share_bps,
        top10_share_bps,
        hhi_bps,
        hhi_normalized_bps,
        whale_dominance_bps,
        early_top10_share_bps,
        bundle_suspect_count: shape.bundle_entities.saturating_add(shape.sniper_entities),
        bundle_entities: shape.bundle_entities,
        sniper_entities: shape.sniper_entities,
        aged_first_buys: shape.aged_first_buys,
        flip_ratio_bps,
        holders,
        entities_tracked,
        tracked_supply_base: supply,
    })
}

// ===========================================================================
// THE PARALLEL STREAM: internal concentration, sampled continuously
// ===========================================================================
//
// Everything above this line is a LEVEL: a share of the true float, and therefore
// gated on `HolderCountBasis::Exact`. Everything below it is a DERIVATIVE of the
// TRACKED COHORT'S OWN internal distribution, which is a different quantity with a
// different basis requirement, and it is kept in different types so the two can
// never be confused for one another.

/// Compile-time proof that this module's bars and the brain's conditioning bands
/// agree (§102). If either side moves, the build breaks rather than the recall
/// conditioner and the sizing law quietly disagreeing about "concentrated".
const _: () = assert!(
    pump_quant_brain::concentration::TOP10_BAND_EDGES_BPS[1] == TOP10_HAIRCUT_BPS
        && pump_quant_brain::concentration::TOP10_BAND_EDGES_BPS[2] == TOP10_VETO_BPS
        && pump_quant_brain::concentration::WHALE_DOMINANCE_BAND_EDGES_BPS[1]
            == WHALE_DOMINANCE_HAIRCUT_BPS
        && pump_quant_brain::concentration::WHALE_DOMINANCE_BAND_EDGES_BPS[2]
            == WHALE_DOMINANCE_VETO_BPS
        && pump_quant_brain::concentration::EARLY_TOP10_BAND_EDGES_BPS[0]
            == EARLY_TOP10_EQUAL_SHARE_BPS
        && pump_quant_brain::concentration::EARLY_TOP10_BAND_EDGES_BPS[1]
            == EARLY_TOP10_HAIRCUT_BPS
        && pump_quant_brain::concentration::EARLY_TOP10_BAND_EDGES_BPS[2] == EARLY_TOP10_VETO_BPS,
    "the brain's concentration bands must be the app's own published bars"
);

/// §99/§57 bound on mints carrying a concentration-trajectory series.
///
/// Matches [`crate::holder_flow::HOLDER_FLOW_MINT_CAP`] so the two planes cannot
/// disagree about which mints exist.
pub const TRAJECTORY_MINT_CAP: usize = crate::holder_flow::HOLDER_FLOW_MINT_CAP;

/// §99 ring capacity of one mint's internal-concentration series.
///
/// Eight samples at the [`crate::holder_flow::HOLDER_SAMPLE_INTERVAL_TICKS`]
/// cadence (1.2 s) reaches back ~9.6 s of information time — comfortably past the
/// estimator's one-second minimum spacing while keeping the per-mint footprint at
/// a fixed 96 bytes. Oldest-evicted.
pub const TRAJECTORY_SERIES_CAP: usize = 8;

/// Minimum samples before a trajectory is a measurement rather than a single
/// reading wearing a direction (§6.4). Two: a change needs two points.
pub const TRAJECTORY_MIN_SAMPLES: usize = 2;

/// The tracked cohort's INTERNAL distribution statistic at one instant.
///
/// Every number here has the tracked supply — *our own* ledger's `Σ net`, which we
/// know exactly — as its denominator. That is what makes it computable on a
/// delta-only basis: we are not claiming a share of the float, we are describing
/// the shape of the positions we actually watched being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternalConcentration {
    /// Herfindahl over tracked shares, rescaled so perfect equality is `0` and
    /// total capture is `10_000` — the same normalization
    /// [`ConcentrationMetrics::hhi_normalized_bps`] uses.
    ///
    /// The SIZE-NORMALIZED form is used on purpose. A raw top-N share falls
    /// mechanically every time a new entity joins the tracked cohort, so its
    /// trajectory on any live market would measure arrival rather than
    /// distribution. Normalizing by `1/n` is the standard adjustment for exactly
    /// that confound — it does not abolish it, and that limitation is stated here
    /// rather than buried.
    pub hhi_normalized_bps: u32,
    /// Entities with a strictly positive tracked position at this instant.
    pub holders: u32,
}

/// Why an internal-concentration reading could not be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalUnknown {
    /// The mint has no holder ledger at all.
    Untracked,
    /// [`HolderCountBasis::Incomplete`]: the ledger is cap-truncated, so even the
    /// TRACKED cohort is not fully observed and its internal distribution is
    /// biased in an unbounded direction. This is the one basis the trajectory
    /// refuses — note it does **not** refuse `DeltaOnly`.
    IncompleteBasis,
    /// Fewer than [`MIN_ENTITIES_FOR_SHAPE`] tracked entities.
    ThinLedger,
    /// Tracked supply is zero: every observed position has been fully exited.
    NoTrackedSupply,
}

/// The tracked cohort's internal concentration, or a labelled refusal.
///
/// Deliberately NOT [`ConcentrationVerdict`]: that type carries shares of the
/// float and is `Exact`-only. Keeping them as separate types is what makes it
/// impossible to feed a delta-only internal statistic into a consumer that asked
/// for a float share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalVerdict {
    /// A reading over the tracked cohort.
    Known(InternalConcentration),
    /// No reading, and why.
    Unknown(InternalUnknown),
}

impl InternalVerdict {
    /// The reading, or `None`. The only way to reach a number.
    #[must_use]
    pub const fn reading(&self) -> Option<InternalConcentration> {
        match self {
            Self::Known(m) => Some(*m),
            Self::Unknown(_) => None,
        }
    }

    /// Why the reading was declined, or `None`.
    #[must_use]
    pub const fn unknown_reason(&self) -> Option<InternalUnknown> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(u) => Some(*u),
        }
    }
}

/// The tracked cohort's internal concentration for `mint`.
///
/// **The basis gate here is `admits_growth`, not `admits_level`** — the load-bearing
/// difference from [`concentration_of`]. The reasoning, stated so it can be
/// attacked:
///
/// * the denominator is our own tracked supply, which we know exactly, so there is
///   no unknown-denominator problem the way there is for a float share;
/// * every BUY enters this ledger by construction (a buyer becomes tracked at the
///   moment they buy), so supply moving INTO an entity is always observed;
/// * the unobserved mass — pre-window holders' pre-existing stacks — is an
///   *unchanging* omission from the denominator, so it biases the LEVEL without
///   driving the CHANGE.
///
/// What it therefore does **not** license: reading the number as the float's
/// concentration. It is the tracked cohort's shape, and it says so in its type.
/// Under `Incomplete` even that fails, because arrivals past the entity cap are
/// dropped entirely, so the cohort itself is a biased sample of the cohort.
#[must_use]
pub fn internal_concentration_of(flow: &HolderFlow, mint: &[u8; 32]) -> InternalVerdict {
    let Some(shape) = flow.shape(mint) else {
        return InternalVerdict::Unknown(InternalUnknown::Untracked);
    };
    internal_concentration_of_shape(&shape)
}

/// [`internal_concentration_of`] over an already-borrowed shape view.
///
/// `O(n)` over the mint's bounded entity ledger, allocation-free.
#[must_use]
pub fn internal_concentration_of_shape(shape: &HolderShapeRef<'_>) -> InternalVerdict {
    if !shape.basis.admits_growth() {
        return InternalVerdict::Unknown(InternalUnknown::IncompleteBasis);
    }
    let entities_tracked = u32::try_from(shape.positions.len()).unwrap_or(u32::MAX);
    if entities_tracked < MIN_ENTITIES_FOR_SHAPE {
        return InternalVerdict::Unknown(InternalUnknown::ThinLedger);
    }

    let mut supply: u128 = 0;
    let mut holders: u32 = 0;
    for p in shape.positions {
        let net = p.net();
        if net == 0 {
            continue;
        }
        holders = holders.saturating_add(1);
        supply = supply.saturating_add(u128::from(net));
    }
    if supply == 0 {
        return InternalVerdict::Unknown(InternalUnknown::NoTrackedSupply);
    }

    let share_bps = |v: u128| -> u128 { (v.saturating_mul(BPS) / supply).min(BPS) };
    let mut hhi_acc: u128 = 0;
    for p in shape.positions {
        if p.net() == 0 {
            continue;
        }
        let s = share_bps(u128::from(p.net()));
        hhi_acc = hhi_acc.saturating_add(s.saturating_mul(s));
    }
    let hhi_bps = (hhi_acc / BPS).min(BPS);

    // Same normalization as `ConcentrationMetrics::hhi_normalized_bps`: perfect
    // equality is 0, total capture is 10 000.
    let hhi_normalized_bps = if holders <= 1 {
        10_000
    } else {
        let min_hhi = 10_000u128 / u128::from(holders);
        let den = 10_000u128.saturating_sub(min_hhi);
        let num = hhi_bps.saturating_sub(min_hhi).saturating_mul(BPS);
        match num.checked_div(den) {
            Some(v) => u32::try_from(v.min(BPS)).unwrap_or(10_000),
            None => 10_000,
        }
    };

    InternalVerdict::Known(InternalConcentration {
        hhi_normalized_bps,
        holders,
    })
}

/// One sampled point of a mint's internal-concentration series (§20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TrajectorySample {
    ts_ns: u64,
    hhi_normalized_bps: u32,
}

/// One mint's bounded internal-concentration series.
#[derive(Debug, Clone)]
struct TrajectorySeries {
    buf: [TrajectorySample; TRAJECTORY_SERIES_CAP],
    start: usize,
    len: usize,
    last_ts_ns: u64,
    last_tick: u64,
}

impl TrajectorySeries {
    const fn new(now: u64) -> Self {
        Self {
            buf: [TrajectorySample {
                ts_ns: 0,
                hhi_normalized_bps: 0,
            }; TRAJECTORY_SERIES_CAP],
            start: 0,
            len: 0,
            last_ts_ns: 0,
            last_tick: now,
        }
    }

    /// Append one sample, dropping any that would move information time backwards
    /// (§20) and evicting oldest-first at capacity (§99).
    fn push(&mut self, sample: TrajectorySample) {
        if self.len > 0 && sample.ts_ns < self.last_ts_ns {
            return;
        }
        if self.len < TRAJECTORY_SERIES_CAP {
            let idx = (self.start + self.len) % TRAJECTORY_SERIES_CAP;
            if let Some(slot) = self.buf.get_mut(idx) {
                *slot = sample;
                self.len += 1;
            }
        } else if let Some(slot) = self.buf.get_mut(self.start) {
            *slot = sample;
            self.start = (self.start + 1) % TRAJECTORY_SERIES_CAP;
        }
        self.last_ts_ns = sample.ts_ns;
    }

    fn at_rev(&self, i: usize) -> Option<TrajectorySample> {
        if i >= self.len {
            return None;
        }
        let idx = (self.start + self.len - 1 - i) % TRAJECTORY_SERIES_CAP;
        self.buf.get(idx).copied()
    }

    /// The signed rate of change of normalized internal concentration as known at
    /// `as_of_ns`, in bps per `norm_ns`.
    ///
    /// Returns `None` — never a fabricated zero — when fewer than
    /// [`TRAJECTORY_MIN_SAMPLES`] usable samples exist at or before the cutoff, or
    /// when no pair is spaced at least `min_interval_ns` apart, or when the pair
    /// spans more than `max_interval_ns` (an unobserved gap is not a measurement).
    fn rate_as_of(&self, as_of_ns: u64) -> Option<i64> {
        if self.len < TRAJECTORY_MIN_SAMPLES {
            return None;
        }
        // Newest sample at or before the cutoff.
        let mut i = 0usize;
        let newest = loop {
            let s = self.at_rev(i)?;
            if s.ts_ns <= as_of_ns {
                break s;
            }
            i += 1;
        };
        // Oldest sample at least `min_interval_ns` older, but not so old that the
        // gap exceeds the staleness ceiling.
        let cutoff = newest.ts_ns.checked_sub(HOLDER_MIN_INTERVAL_NS)?;
        let mut j = i + 1;
        let oldest = loop {
            let s = self.at_rev(j)?;
            if s.ts_ns <= cutoff {
                break s;
            }
            j += 1;
        };
        let dt = newest.ts_ns.checked_sub(oldest.ts_ns)?;
        if dt == 0 || dt > HOLDER_MAX_INTERVAL_NS {
            return None;
        }
        // (delta bps) * norm_ns / dt, entirely in i128 then clamped (§22).
        let delta = i128::from(newest.hhi_normalized_bps) - i128::from(oldest.hhi_normalized_bps);
        let num = delta.checked_mul(i128::from(HOLDER_GROWTH_NORM_NS))?;
        let v = num / i128::from(dt);
        Some(if v > i128::from(i64::MAX) {
            i64::MAX
        } else if v < i128::from(i64::MIN) {
            i64::MIN
        } else {
            v as i64
        })
    }
}

/// The continuous concentration-TRAJECTORY plane (§21.7/§70.1).
///
/// Concentration used to be derived once, on demand, at admit — a point reading
/// with no history, which can answer "is this concentrated?" but not "is it
/// concentrATING?". Those are different questions and the second one is the one a
/// scalper actually has an edge on. This tracker turns the reading into a stream:
/// it is folded on the same bounded cadence as the holder-count sample, so a
/// trajectory exists by the time a decision needs one.
///
/// Bounded by construction: [`TRAJECTORY_MINT_CAP`] mints of
/// [`TRAJECTORY_SERIES_CAP`] samples each, with least-recently-updated eviction
/// (ties by the smaller mint key — a pure function of state, no clock, no
/// insertion-order dependence).
#[derive(Debug, Clone)]
pub struct ConcentrationTrajectoryPlane {
    mints: BTreeMap<[u8; 32], TrajectorySeries>,
    mint_cap: usize,
    evictions: u64,
}

impl Default for ConcentrationTrajectoryPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl ConcentrationTrajectoryPlane {
    /// An empty plane at the named-const bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(TRAJECTORY_MINT_CAP)
    }

    /// An empty plane with an explicit mint bound (clamped to at least 1).
    #[must_use]
    pub fn with_capacity(mint_cap: usize) -> Self {
        Self {
            mints: BTreeMap::new(),
            mint_cap: mint_cap.max(1),
            evictions: 0,
        }
    }

    /// Mints carrying a series.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mints.len()
    }

    /// Whether no mint carries a series.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mints.is_empty()
    }

    /// Series evicted by [`TRAJECTORY_MINT_CAP`].
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Fold one internal-concentration observation for `mint`.
    ///
    /// Called by the engine on the holder-sample cadence, so the `O(n)` derivation
    /// runs once per 1.2 s of information time per mint rather than once per swap.
    /// A refused reading pushes nothing — the series records measurements only, so
    /// a gap in it is a genuine gap and never an interpolated zero (§6.4).
    pub fn observe(&mut self, mint: &[u8; 32], verdict: InternalVerdict, now: u64, ns: u64) {
        let Some(reading) = verdict.reading() else {
            return;
        };
        if !self.mints.contains_key(mint) {
            if self.mints.len() >= self.mint_cap {
                if let Some(victim) = self.evict_key() {
                    self.mints.remove(&victim);
                    self.evictions = self.evictions.saturating_add(1);
                }
            }
            if self.mints.len() >= self.mint_cap {
                return;
            }
            self.mints.insert(*mint, TrajectorySeries::new(now));
        }
        let Some(s) = self.mints.get_mut(mint) else {
            return;
        };
        s.last_tick = now;
        s.push(TrajectorySample {
            ts_ns: ns,
            hhi_normalized_bps: reading.hhi_normalized_bps,
        });
    }

    /// The point-in-time concentration trajectory for `mint` as known at
    /// `as_of_ns`, in the brain's parallel-stream type (§20).
    ///
    /// `basis` is the mint's current holder basis; `Incomplete` refuses here for
    /// the same reason [`internal_concentration_of_shape`] refuses.
    #[must_use]
    pub fn trajectory_as_of(
        &self,
        mint: &[u8; 32],
        basis: Option<HolderCountBasis>,
        as_of_ns: u64,
    ) -> BrainTrajectory {
        let Some(basis) = basis else {
            return BrainTrajectory::Unknown(BrainTrajectoryUnknown::Untracked);
        };
        if !basis.admits_growth() {
            return BrainTrajectory::Unknown(BrainTrajectoryUnknown::IncompleteBasis);
        }
        let Some(series) = self.mints.get(mint) else {
            return BrainTrajectory::Unknown(BrainTrajectoryUnknown::ThinLedger);
        };
        match series.rate_as_of(as_of_ns) {
            Some(rate) => BrainTrajectory::Known(BrainTrajectoryShape::from_rate_bps(rate)),
            None => BrainTrajectory::Unknown(BrainTrajectoryUnknown::InsufficientHistory),
        }
    }

    /// The raw signed rate for `mint` (bps of normalized internal concentration
    /// per minute), for the REPORT plane. `None` when no rate is measurable.
    #[must_use]
    pub fn rate_as_of(&self, mint: &[u8; 32], as_of_ns: u64) -> Option<i64> {
        self.mints.get(mint)?.rate_as_of(as_of_ns)
    }

    /// The eviction victim: least-recently-updated series, ties by smaller mint
    /// key. A pure function of state (§22 determinism).
    fn evict_key(&self) -> Option<[u8; 32]> {
        let mut best: Option<([u8; 32], u64)> = None;
        for (k, s) in &self.mints {
            let replace = match best {
                None => true,
                Some((bk, bt)) => s.last_tick < bt || (s.last_tick == bt && *k < bk),
            };
            if replace {
                best = Some((*k, s.last_tick));
            }
        }
        best.map(|(k, _)| k)
    }
}

/// Band a `Known` concentration reading into the brain's parallel-stream type,
/// mapping each refusal REASON across rather than collapsing them all into one.
///
/// The one-for-one arm correspondence is what
/// `tests::every_app_refusal_reason_maps_to_its_own_brain_arm` pins.
#[must_use]
pub fn brain_reading_of(verdict: &ConcentrationVerdict) -> BrainReading {
    match verdict {
        ConcentrationVerdict::Known(m) => BrainReading::Known(BrainShape::from_bps(
            m.top10_share_bps,
            m.whale_dominance_bps,
            m.early_top10_share_bps,
        )),
        ConcentrationVerdict::Unknown(u) => BrainReading::Unknown(match u {
            ConcentrationUnknown::Disarmed => BrainUnknown::Disarmed,
            ConcentrationUnknown::Untracked => BrainUnknown::Untracked,
            ConcentrationUnknown::DeltaOnlyBasis => BrainUnknown::DeltaOnlyBasis,
            ConcentrationUnknown::IncompleteBasis => BrainUnknown::IncompleteBasis,
            ConcentrationUnknown::ThinLedger => BrainUnknown::ThinLedger,
            ConcentrationUnknown::NoTrackedSupply => BrainUnknown::NoTrackedSupply,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holder_flow::SNIPER_SLOT_WINDOW;

    const M: [u8; 32] = [9u8; 32];

    /// Build an Exact-basis ledger with `n` equal holders.
    fn equal_ledger(n: u64) -> HolderFlow {
        let mut hf = HolderFlow::new();
        hf.note_creation(&M, 0);
        for e in 0..n {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        hf
    }

    #[test]
    fn equal_distribution_scores_zero_internal_inequality() {
        let hf = equal_ledger(40);
        let v = concentration_of(&hf, &M);
        let m = v.metrics().expect("Exact basis, 40 entities");
        assert_eq!(m.holders, 40);
        // Ten of forty equal holders ⇒ 25%.
        assert_eq!(m.top10_share_bps, 2_500);
        assert_eq!(m.hhi_normalized_bps, 0);
        assert_eq!(m.whale_dominance_bps, 0);
        // Pure accumulation: every unit bought is still held.
        assert_eq!(m.flip_ratio_bps, FLIP_RATIO_NEUTRAL_BPS);
        assert_eq!(m.risk(true), ConcentrationRisk::Clear);
    }

    #[test]
    fn a_whale_lifts_dominance_and_the_risk_tier() {
        let mut hf = equal_ledger(30);
        // One entity accumulates far past the rest.
        hf.observe_swap_aged(&M, 0, 200_000, 0, 0, Some(100));
        let v = concentration_of(&hf, &M);
        let m = v.metrics().expect("Exact basis");
        assert!(m.top1_share_bps > 8_000, "top1 = {}", m.top1_share_bps);
        assert!(m.whale_dominance_bps >= WHALE_DOMINANCE_VETO_BPS);
        // Veto-grade shape, but only WITH corroboration (§21.7 never-alone).
        assert_eq!(m.risk(true), ConcentrationRisk::Veto);
        assert_eq!(m.risk(false), ConcentrationRisk::Haircut);
    }

    #[test]
    fn delta_only_basis_refuses_with_no_estimate() {
        let mut hf = HolderFlow::new();
        // No creation sighting ⇒ DeltaOnly.
        for e in 0..40u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        let v = concentration_of(&hf, &M);
        assert!(!v.is_known());
        assert_eq!(v.metrics(), None);
        assert_eq!(
            v.unknown_reason(),
            Some(ConcentrationUnknown::DeltaOnlyBasis)
        );
        // And the fail-open law holds.
        assert_eq!(v.risk_or_clear(true), ConcentrationRisk::Clear);
        assert_eq!(v.screen_concentration_bps(), 0);
    }

    #[test]
    fn bundle_and_sniper_cohorts_are_classified_by_slot() {
        let mut hf = HolderFlow::new();
        hf.note_creation(&M, 0);
        for e in 0..5u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(0)); // creation slot
        }
        for e in 5..9u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(SNIPER_SLOT_WINDOW));
        }
        for e in 9..25u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(500)); // organic latecomers
        }
        let v = concentration_of(&hf, &M);
        let m = v.metrics().expect("Exact basis");
        assert_eq!(m.bundle_entities, 5);
        assert_eq!(m.sniper_entities, 4);
        assert_eq!(m.bundle_suspect_count, 9);
        assert_eq!(m.aged_first_buys, 25);
    }

    #[test]
    fn a_swap_with_no_slot_evidence_classifies_nobody() {
        let mut hf = HolderFlow::new();
        hf.note_creation(&M, 0);
        for e in 0..25u64 {
            hf.observe_swap(&M, e, 1_000, 0, 0); // no age evidence
        }
        let v = concentration_of(&hf, &M);
        let m = v.metrics().expect("Exact basis");
        assert_eq!(m.bundle_suspect_count, 0, "§6.4: unknown age is not slot 0");
        assert_eq!(m.aged_first_buys, 0);
    }

    #[test]
    fn flip_ratio_rises_with_round_trip_churn() {
        let mut hf = HolderFlow::new();
        hf.note_creation(&M, 0);
        for e in 0..25u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        let hold = concentration_of(&hf, &M)
            .metrics()
            .map(|m| m.flip_ratio_bps)
            .unwrap_or_default();
        // Now churn: each entity buys and sells repeatedly, ending flat-ish.
        for _ in 0..4 {
            for e in 0..25u64 {
                hf.observe_swap_aged(&M, e, 5_000, 0, 0, Some(100));
                hf.observe_swap_aged(&M, e, -5_000, 0, 0, Some(100));
            }
        }
        let churn = concentration_of(&hf, &M)
            .metrics()
            .map(|m| m.flip_ratio_bps)
            .unwrap_or_default();
        assert_eq!(hold, FLIP_RATIO_NEUTRAL_BPS);
        assert!(churn > FLIP_TOLERANCE_BPS, "churned flip ratio = {churn}");
    }

    #[test]
    fn thin_ledger_refuses_rather_than_reading_a_trivial_hundred_percent() {
        let hf = equal_ledger(u64::from(MIN_ENTITIES_FOR_SHAPE) - 1);
        let v = concentration_of(&hf, &M);
        assert_eq!(v.unknown_reason(), Some(ConcentrationUnknown::ThinLedger));
    }

    #[test]
    fn every_risk_tier_is_reduce_only() {
        assert_eq!(ConcentrationRisk::Clear.size_mult_bp(), 10_000);
        for tier in [ConcentrationRisk::Haircut, ConcentrationRisk::Veto] {
            assert!(tier.size_mult_bp() < 10_000, "{tier:?} must reduce");
        }
    }

    // -----------------------------------------------------------------------
    // THE PARALLEL STREAM
    // -----------------------------------------------------------------------

    /// An Exact-basis ledger and a delta-only one built from the SAME flow. The
    /// level refuses on the second; the internal statistic does not. This is the
    /// coverage asymmetry that motivates the whole design, as code.
    #[test]
    fn the_internal_statistic_reads_where_the_level_refuses() {
        let mut exact = HolderFlow::new();
        exact.note_creation(&M, 0);
        let mut delta = HolderFlow::new(); // no creation sighting ⇒ DeltaOnly
        for e in 0..40u64 {
            exact.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
            delta.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        assert!(concentration_of(&exact, &M).is_known());
        assert_eq!(
            concentration_of(&delta, &M).unknown_reason(),
            Some(ConcentrationUnknown::DeltaOnlyBasis),
            "a float share needs an Exact basis"
        );
        // …but the tracked cohort's own shape is observable on both, identically:
        // the two ledgers hold the same positions, so the same internal number.
        let a = internal_concentration_of(&exact, &M)
            .reading()
            .expect("exact");
        let b = internal_concentration_of(&delta, &M)
            .reading()
            .expect("delta-only is admitted for a derivative");
        assert_eq!(a, b);
        assert_eq!(a.hhi_normalized_bps, 0, "forty equal holders");
        assert_eq!(a.holders, 40);
    }

    /// The one basis the internal statistic still refuses: a truncated ledger, in
    /// which the tracked cohort is itself a biased sample of the tracked cohort.
    #[test]
    fn a_truncated_ledger_refuses_even_the_internal_statistic() {
        let mut hf = HolderFlow::with_caps(4, 25);
        hf.note_creation(&M, 0);
        for e in 0..40u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        assert_eq!(
            internal_concentration_of(&hf, &M).unknown_reason(),
            Some(InternalUnknown::IncompleteBasis)
        );
        assert_eq!(internal_concentration_of(&hf, &M).reading(), None);
    }

    /// A whale accumulating raises the internal inequality; the crowd arriving
    /// lowers it. Direction, on a delta-only ledger, is the product.
    #[test]
    fn the_internal_statistic_moves_with_the_shape_not_the_size() {
        let mut hf = HolderFlow::new(); // DeltaOnly throughout
        for e in 0..30u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        let flat = internal_concentration_of(&hf, &M)
            .reading()
            .expect("readable")
            .hhi_normalized_bps;
        // A whale accumulates hard.
        hf.observe_swap_aged(&M, 0, 500_000, 1, 1_000_000_000, Some(100));
        let whale = internal_concentration_of(&hf, &M)
            .reading()
            .expect("readable")
            .hhi_normalized_bps;
        assert!(whale > flat, "{whale} must exceed {flat}");
        // Then a broad crowd arrives at the whale's own size, diluting it.
        for e in 30..60u64 {
            hf.observe_swap_aged(&M, e, 20_000, 2, 2_000_000_000, Some(100));
        }
        let diluted = internal_concentration_of(&hf, &M)
            .reading()
            .expect("readable")
            .hhi_normalized_bps;
        assert!(diluted < whale, "{diluted} must fall back below {whale}");
    }

    /// A single sample is not a trajectory (§6.4). The plane says
    /// `InsufficientHistory` rather than `Flat`.
    #[test]
    fn one_sample_is_not_a_trajectory() {
        let mut hf = HolderFlow::new();
        for e in 0..30u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        let mut plane = ConcentrationTrajectoryPlane::new();
        plane.observe(&M, internal_concentration_of(&hf, &M), 0, 0);
        let t = plane.trajectory_as_of(&M, Some(HolderCountBasis::DeltaOnly), 0);
        assert_eq!(
            t.unknown_reason(),
            Some(BrainTrajectoryUnknown::InsufficientHistory)
        );
        assert_eq!(t.shape(), None, "a refusal carries no direction");
    }

    /// Two samples spanning the minimum interval, with the whale accumulating in
    /// between, must read `Concentrating` on a DELTA-ONLY ledger — which is the
    /// claim this whole sub-feature rests on.
    #[test]
    fn a_delta_only_ledger_yields_a_concentrating_trajectory() {
        use pump_quant_brain::concentration::TrajectoryDirection;
        let mut hf = HolderFlow::new(); // DeltaOnly
        for e in 0..30u64 {
            hf.observe_swap_aged(&M, e, 1_000, 0, 0, Some(100));
        }
        let mut plane = ConcentrationTrajectoryPlane::new();
        plane.observe(&M, internal_concentration_of(&hf, &M), 0, 0);
        // One minute later, one entity has taken over.
        let t1 = 60 * HOLDER_MIN_INTERVAL_NS;
        hf.observe_swap_aged(&M, 0, 500_000, 1, t1, Some(100));
        plane.observe(&M, internal_concentration_of(&hf, &M), 1, t1);

        assert_eq!(
            hf.reading(&M).map(|r| r.basis()),
            Some(HolderCountBasis::DeltaOnly),
            "the premise: this is the basis a float share refuses"
        );
        let t = plane.trajectory_as_of(&M, Some(HolderCountBasis::DeltaOnly), t1);
        assert_eq!(
            t.shape().map(BrainTrajectoryShape::direction),
            Some(TrajectoryDirection::Concentrating)
        );
        assert!(plane.rate_as_of(&M, t1).unwrap_or(0) > 0);
    }

    /// The dispersing direction is reachable too — otherwise the signal would be
    /// one-sided and its "direction" would be a constant wearing a name.
    #[test]
    fn the_dispersing_direction_is_reachable() {
        use pump_quant_brain::concentration::TrajectoryDirection;
        let mut hf = HolderFlow::new();
        // Start dominated by one entity.
        hf.observe_swap_aged(&M, 0, 1_000_000, 0, 0, Some(100));
        for e in 1..30u64 {
            hf.observe_swap_aged(&M, e, 100, 0, 0, Some(100));
        }
        let mut plane = ConcentrationTrajectoryPlane::new();
        plane.observe(&M, internal_concentration_of(&hf, &M), 0, 0);
        // A broad crowd arrives at real size.
        let t1 = 60 * HOLDER_MIN_INTERVAL_NS;
        for e in 30..120u64 {
            hf.observe_swap_aged(&M, e, 40_000, 1, t1, Some(100));
        }
        plane.observe(&M, internal_concentration_of(&hf, &M), 1, t1);
        let t = plane.trajectory_as_of(&M, Some(HolderCountBasis::DeltaOnly), t1);
        assert_eq!(
            t.shape().map(BrainTrajectoryShape::direction),
            Some(TrajectoryDirection::Dispersing)
        );
        assert!(plane.rate_as_of(&M, t1).unwrap_or(0) < 0);
    }

    /// The plane is bounded and its eviction is a pure function of state (§99/§22).
    #[test]
    fn the_trajectory_plane_is_bounded_and_evicts_deterministically() {
        let mut plane = ConcentrationTrajectoryPlane::with_capacity(3);
        let reading = InternalVerdict::Known(InternalConcentration {
            hhi_normalized_bps: 100,
            holders: 30,
        });
        for i in 0..10u8 {
            let mut m = [0u8; 32];
            m[0] = i;
            plane.observe(&m, reading, u64::from(i), u64::from(i) * 1_000);
        }
        assert_eq!(plane.len(), 3, "hard bound");
        assert_eq!(plane.evictions(), 7);
        // Deterministic: the same fold order reproduces the same survivor set.
        let mut again = ConcentrationTrajectoryPlane::with_capacity(3);
        for i in 0..10u8 {
            let mut m = [0u8; 32];
            m[0] = i;
            again.observe(&m, reading, u64::from(i), u64::from(i) * 1_000);
        }
        assert_eq!(
            plane.mints.keys().collect::<Vec<_>>(),
            again.mints.keys().collect::<Vec<_>>()
        );
    }

    /// A refused reading pushes NOTHING, so a gap in the series is a genuine gap
    /// and never an interpolated zero (§6.4).
    #[test]
    fn a_refused_reading_is_not_sampled() {
        let mut plane = ConcentrationTrajectoryPlane::new();
        plane.observe(
            &M,
            InternalVerdict::Unknown(InternalUnknown::ThinLedger),
            0,
            0,
        );
        assert!(plane.is_empty());
        assert_eq!(plane.rate_as_of(&M, 0), None);
    }

    /// Every app-side refusal reason maps to its OWN brain-side arm. Collapsing
    /// them would destroy the one thing an `Unknown` is for: saying why.
    #[test]
    fn every_app_refusal_reason_maps_to_its_own_brain_arm() {
        let arms = [
            ConcentrationUnknown::Disarmed,
            ConcentrationUnknown::Untracked,
            ConcentrationUnknown::DeltaOnlyBasis,
            ConcentrationUnknown::IncompleteBasis,
            ConcentrationUnknown::ThinLedger,
            ConcentrationUnknown::NoTrackedSupply,
        ];
        let mut seen = Vec::new();
        for a in arms {
            let mapped = brain_reading_of(&ConcentrationVerdict::Unknown(a))
                .unknown_reason()
                .expect("an Unknown maps to an Unknown");
            assert!(
                !seen.contains(&mapped),
                "two app reasons collapsed onto {mapped:?}"
            );
            seen.push(mapped);
        }
        assert_eq!(seen.len(), arms.len());
    }

    /// A `Known` verdict bands across without ever producing a number from an
    /// `Unknown`, and the bands honour the module's own published bars.
    #[test]
    fn a_known_verdict_bands_across_to_the_brain() {
        let hf = equal_ledger(40);
        let v = concentration_of(&hf, &M);
        let banded = brain_reading_of(&v);
        let shape = banded.shape().expect("Known bands across");
        // Ten of forty equal holders is 2 500 bp ⇒ top-10 band 1 (at the low edge).
        assert_eq!(shape.top10_band(), 1);
        assert_eq!(shape.whale_dominance_band(), 0, "equal weight ⇒ no whale");
        // And an Unknown yields nothing, by any route.
        let u = brain_reading_of(&ConcentrationVerdict::Unknown(
            ConcentrationUnknown::DeltaOnlyBasis,
        ));
        assert_eq!(u.shape(), None);
    }
}
