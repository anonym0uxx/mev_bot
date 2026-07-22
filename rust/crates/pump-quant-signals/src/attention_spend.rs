//! Paid-attention-spend intelligence + Tier-0 no-self-promotion guard
//! (constitution §29.10, criterion 110).
//!
//! Two halves:
//!
//! (a) **Versioned attention-spend computation** behind a neutral
//! [`AttentionSpendSource`] contract: platform-sold boost events multiplied by a
//! **versioned price/package table** yield an operator's marketing spend.
//! Epistemic class D (§6.6): never authoritative, never hot-path,
//! **Missing-on-stale** — a stale/unknown table yields a Missing estimate, never
//! a fabricated number ("an unversioned spend figure is not a number"). The
//! computation is deterministic and replayable; live polling of the source is
//! OUT OF SCOPE and modelled only behind the trait, never called here.
//!
//! (b) **Absolute Tier-0-severity no-self-promotion prohibition**, the direct
//! analogue of `si_no_copy_trade`: proven **by construction** that no code path
//! can purchase any paid promotion for a token the system holds, trades, or
//! researches (indeed for any token at all). [`authorize_paid_promotion`]
//! returns a [`PromotionAuthorization`] whose only variant is `Refused`, so an
//! approval is *unrepresentable* — the guard cannot be bypassed.
//!
//! # Constitution constraints (§22)
//!
//! Deterministic, integer-only. Spend is lamports (`u128`, since cumulative
//! boost spend can be large); timestamps are milliseconds. No floats, no I/O.

/// Opaque token identity (e.g. a mint-address hash). Kept abstract so this
/// module has no chain dependency.
pub type TokenId = u64;

/// A single observed paid-attention (boost-class) event.
///
/// Responsibility: journaled boost observation (§29.10). `observed_ts_ms` is the
/// local-arrival timestamp used for staleness. `package_id` indexes the
/// versioned price table. Constitution §22: integer count/timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoostEvent {
    /// Which package/tier was purchased (indexes [`PriceTable`]).
    pub package_id: u16,
    /// Number of units of that package purchased in this event.
    pub count: u32,
    /// Local-arrival observation timestamp (milliseconds).
    pub observed_ts_ms: u64,
}

/// One entry of a versioned price/package table.
///
/// Responsibility: the platform-published unit price of a boost package.
/// Constitution §22: integer lamports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricePackage {
    /// Package identifier matched against [`BoostEvent::package_id`].
    pub package_id: u16,
    /// Unit price of the package in lamports.
    pub unit_price_lamports: u64,
}

/// A versioned price/package table. **Unversioned spend is not a number**, so
/// every estimate is tagged with `version`; `valid_until_ts_ms` bounds the
/// table's freshness for Missing-on-stale semantics.
///
/// Responsibility: the versioned pricing basis of a spend estimate (§29.10).
/// Constitution §22: integer version / timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceTable {
    /// Monotonic table version pinned into every computed estimate.
    pub version: u32,
    /// Package rows (looked up by `package_id`).
    pub packages: Vec<PricePackage>,
    /// Inclusive validity horizon (milliseconds); an `as_of` beyond this is stale.
    pub valid_until_ts_ms: u64,
}

impl PriceTable {
    /// Look up a package's unit price, if present.
    ///
    /// Responsibility: deterministic package price resolution (§29.10).
    #[inline]
    pub fn unit_price(&self, package_id: u16) -> Option<u64> {
        self.packages
            .iter()
            .find(|p| p.package_id == package_id)
            .map(|p| p.unit_price_lamports)
    }
}

/// The result of a spend computation. Carries the pinned table version on
/// success; Missing variants preserve *why* the value is absent, never a zero
/// masquerading as a real spend.
///
/// Responsibility: Missing-on-stale spend estimate (§29.10, §6.6 D-class).
/// Constitution §22: integer lamports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendEstimate {
    /// A real, versioned spend figure.
    Amount {
        /// Total spend in lamports (widened; cumulative boosts can be large).
        lamports: u128,
        /// The price-table version this figure was computed from.
        table_version: u32,
    },
    /// The price table is stale relative to `as_of` — no number is produced.
    MissingStale,
    /// A boost referenced a package absent from the table — no number.
    MissingUnknownPackage,
}

/// Compute versioned attention spend from boost events and a price table.
///
/// Missing-on-stale semantics (§29.10):
/// - if `as_of_ts_ms > table.valid_until_ts_ms` => [`SpendEstimate::MissingStale`];
/// - if any event references an unknown package => [`SpendEstimate::MissingUnknownPackage`];
/// - otherwise => `Amount { lamports = Σ count * unit_price, table_version }`.
///
/// Empty events with a fresh table yield `Amount { 0, version }` (a real zero:
/// the operator verifiably spent nothing), distinct from the Missing variants.
///
/// Responsibility: the deterministic, replayable half of §29.10.
/// Constitution §22: `u128` accumulation, `saturating_mul`/`saturating_add`,
/// staleness compared as integers, no float.
pub fn compute_spend(events: &[BoostEvent], table: &PriceTable, as_of_ts_ms: u64) -> SpendEstimate {
    if as_of_ts_ms > table.valid_until_ts_ms {
        return SpendEstimate::MissingStale;
    }
    let mut total: u128 = 0;
    for e in events {
        match table.unit_price(e.package_id) {
            None => return SpendEstimate::MissingUnknownPackage,
            Some(unit) => {
                let line = (unit as u128).saturating_mul(e.count as u128);
                total = total.saturating_add(line);
            }
        }
    }
    SpendEstimate::Amount {
        lamports: total,
        table_version: table.version,
    }
}

/// Neutral live-source contract for paid attention (§29.10). Any venue selling
/// externally-observable promotion feeds the same code through this trait.
///
/// **Live I/O is OUT OF SCOPE.** This trait is declared so the pure computation
/// ([`compute_spend`]) has a boundary to sit behind; no implementation is
/// provided and nothing in this crate calls it (deterministic core, §22).
///
/// Responsibility: provider-neutral boundary for boost observations (§29.10,
/// §6.6 D-class — never authoritative, never hot-path).
pub trait AttentionSpendSource {
    /// Return the journaled boost events observed for `token` no later than
    /// `as_of_ts_ms`. Implementations must be point-in-time-safe and must never
    /// be consulted on the hot path.
    fn boost_events(&self, token: TokenId, as_of_ts_ms: u64) -> Vec<BoostEvent>;
}

// -- Tier-0 no-self-promotion guard -------------------------------------------

/// The system's relationship to a token, for the self-promotion prohibition.
///
/// Responsibility: enumerate every relationship the prohibition must cover
/// (§29.10(d)). Constitution: exhaustive so the guard's proof covers all cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRelationship {
    /// The system currently holds the token.
    Holds,
    /// The system trades (or intends to trade) the token.
    Trades,
    /// The system is researching the token.
    Researches,
    /// The system has no relationship to the token.
    Unrelated,
}

/// A request to purchase paid promotion for a token. Constructing one is
/// harmless: it can never be *authorized* (see [`authorize_paid_promotion`]).
///
/// Responsibility: model an attempted self-promotion so the guard can refuse it
/// (§29.10(d)). Constitution §22: plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaidPromotionRequest {
    /// Token the promotion would target.
    pub token: TokenId,
    /// The system's relationship to `token`.
    pub relationship: SystemRelationship,
    /// The promotion package that would be purchased.
    pub package_id: u16,
}

/// Why a self-promotion purchase was refused. Tier-0 severity is unwaivable and
/// cannot be overridden from chat (§29.10(d)).
///
/// Responsibility: carry the refusal reason for audit. Constitution: data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfPromotionRefusal {
    /// The relationship that triggered the refusal.
    pub relationship: SystemRelationship,
}

/// Authorization outcome for a paid-promotion purchase.
///
/// **By construction there is exactly one variant — `Refused`.** No `Approved`
/// variant exists, so no caller can obtain permission to purchase paid
/// promotion; the prohibition is enforced by the type system, not by a runtime
/// branch that could be mis-configured. This is the §29.10(d) Tier-0
/// no-self-promotion guard, the direct analogue of `si_no_copy_trade`.
///
/// Responsibility: make self-promotion approval *unrepresentable*.
/// Constitution §22 / §29.10(d): compile-enforced prohibition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionAuthorization {
    /// The only possible outcome: the purchase is refused at Tier-0 severity.
    Refused(SelfPromotionRefusal),
}

/// The single, total entry point for "may the system buy paid promotion for
/// this token?" It **always** returns [`PromotionAuthorization::Refused`], for
/// every relationship including `Unrelated` — the system purchases no paid
/// promotion for any token, ever.
///
/// Because [`PromotionAuthorization`] has no approving variant, this function
/// cannot be written to approve, and no caller can extract an approval from its
/// result. That is the by-construction proof of the Tier-0 prohibition.
///
/// Responsibility: refuse all self-promotion purchases (§29.10(d)).
/// Constitution §22: deterministic, total, no I/O.
#[inline]
pub fn authorize_paid_promotion(req: PaidPromotionRequest) -> PromotionAuthorization {
    PromotionAuthorization::Refused(SelfPromotionRefusal {
        relationship: req.relationship,
    })
}

/// Convenience predicate: is any paid-promotion purchase permitted? Always
/// `false`. Useful as a compile/test-enforced invariant in higher layers.
///
/// Responsibility: expose the prohibition as a boolean for guard sites
/// (§29.10(d)). Constitution §22: total, constant-false by construction.
#[inline]
pub fn paid_promotion_permitted(_req: PaidPromotionRequest) -> bool {
    match authorize_paid_promotion(_req) {
        PromotionAuthorization::Refused(_) => false,
    }
}
