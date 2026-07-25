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
}
