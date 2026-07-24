//! `MetaRotationState` — market-level narrative-category intelligence
//! (on-chain factual measures) plus the deterministic, time-safe
//! category-assignment classifier v0.
//!
//! ## Responsibility
//! Memecoin flow rotates by narrative category (animals → political →
//! celebrity → AI, etc.); this layer sits between `MarketRegimeState` (too
//! macro) and per-token attention (too micro) (§21.4). This module implements
//! the two mandatory *deterministic* pieces of that subsystem:
//!
//! 1. [`classify_category`] — the **deterministic lexical/metadata
//!    category-assignment classifier v0** (§21.4 two-layer assignment, layer 1;
//!    the GLM layer is a ResearchArtifact and is out of scope here). It is a
//!    pure function of the token name/symbol as observed, stamped with the
//!    assignment slot — **timestamped and never retroactive** (criterion 81:
//!    "category assignments are timestamped and never retroactive").
//! 2. [`MetaRotationReducer`] / [`MetaRotationState`] — **per-category on-chain
//!    factual measures** (launches, flow, creators, graduations) and the
//!    integer rotation / emergence / saturation signals derived between two
//!    snapshots (§21.4).
//!
//! No float, no clock, no RNG (§22). GLM/social interpretation may never
//! populate this factual state (§ criterion 83); only decoded on-chain events
//! and the deterministic lexical assignment feed it.

use crate::common::{ratio_bps, signed_ratio_bps, BoundedMap, BoundedSet, Completeness, EntityId};

/// A versioned narrative category in the taxonomy.
///
/// ## Responsibility
/// A category is a stable numeric `id` plus a set of lowercase-ASCII keyword
/// needles. Ids are stable across taxonomy versions so historical category
/// assignments remain interpretable (criterion 81 non-retroactivity — old
/// assignments keep their meaning).
#[derive(Clone, Copy, Debug)]
pub struct CategoryDef {
    /// Stable category id (used as the [`EntityId`] key in the reducer).
    pub id: EntityId,
    /// Lowercase-ASCII needles, each carrying the mode it is allowed to match
    /// under; a case-insensitive hit of any needle in the name or symbol
    /// assigns the token to this category.
    pub needles: &'static [CategoryNeedle],
}

/// How a category needle is allowed to match a name/symbol field.
///
/// ## Responsibility
/// v0 matched every needle as a naive substring, which mis-categorizes ordinary
/// English: `"Fair Launch"` hits `ai`, `"Catalyst"` hits `cat`, `"Bottom
/// Signal"` hits `bot`, `"Bullish Chain"` hits `bull`, `"Starter Pack"` hits
/// `star`. Because [`CategoryAssignment::category_id`] is a *recall filter key*
/// downstream, a mis-assignment pools a token with the wrong meta's episodes
/// and silently corrupts every conditioned estimate keyed on it. This enum is
/// the fix, mirroring the word-boundary discipline already proven in
/// `pump_quant_narrative::narrative_family::MatchMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryMatchMode {
    /// Matches anywhere inside the field. Reserved for needles long and
    /// distinctive enough that an incidental hit is not a realistic concern.
    Substring,
    /// Must match at a word boundary: start/end of the field, or adjacent to a
    /// non-alphanumeric byte. Required for short or English-common needles.
    Word,
}

/// One category needle and the mode it is allowed to match under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategoryNeedle {
    /// Lowercase-ASCII needle text.
    pub text: &'static str,
    /// How it is allowed to match.
    pub mode: CategoryMatchMode,
}

/// Shorthand for a substring needle.
const fn sub(text: &'static str) -> CategoryNeedle {
    CategoryNeedle {
        text,
        mode: CategoryMatchMode::Substring,
    }
}

/// Shorthand for a word-boundary needle.
const fn word(text: &'static str) -> CategoryNeedle {
    CategoryNeedle {
        text,
        mode: CategoryMatchMode::Word,
    }
}

/// A versioned category taxonomy for the classifier.
///
/// ## Responsibility
/// Holds the ordered category list and the version stamp written onto every
/// [`CategoryAssignment`] (§21.4 "versioned dynamic category taxonomy").
/// Ordering is significant: the first matching category (by scan order) wins,
/// making assignment deterministic when a name matches multiple categories.
#[derive(Clone, Copy, Debug)]
pub struct CategoryTaxonomy {
    /// Taxonomy version.
    pub version: u32,
    /// Category definitions, scanned in order (first match wins).
    pub categories: &'static [CategoryDef],
}

/// Reserved category id meaning "no lexical category matched" (UNCLASSIFIED).
/// Kept distinct from a real category so unmatched tokens are inspectable, not
/// silently bucketed (§6.4 UNKNOWN discipline).
pub const CATEGORY_UNCLASSIFIED: EntityId = 0;

/// Version stamp of [`TAXONOMY_V0`] — the historical, naive-substring lexicon.
pub const TAXONOMY_VERSION_V0: u32 = 0;

/// Version stamp of [`TAXONOMY_V1`] — the word-boundary-disciplined lexicon.
pub const TAXONOMY_VERSION_V1: u32 = 1;

/// A deterministic v0 taxonomy covering the constitution's named rotation
/// examples (animals, political, celebrity, AI) plus a few common memecoin
/// tropes. Illustrative and versioned; production supplies the live taxonomy.
///
/// **Frozen historical record (criterion 81).** Every needle here matches as a
/// naive substring, which is exactly the behaviour that produced the known
/// mis-assignments enumerated on [`CategoryMatchMode`]. It is left bit-identical
/// on purpose: assignments already stamped `taxonomy_version = 0` keep their
/// meaning, and re-running v0 must reproduce them. New assignments use
/// [`TAXONOMY_V1`]; the fix is forward, never retroactive.
///
/// Constitution: §21.4 (documented rotation sequences), criterion 81.
pub const TAXONOMY_V0: CategoryTaxonomy = CategoryTaxonomy {
    version: TAXONOMY_VERSION_V0,
    categories: &[
        CategoryDef {
            id: 1,
            needles: &[
                sub("dog"),
                sub("doge"),
                sub("shib"),
                sub("inu"),
                sub("cat"),
                sub("pepe"),
                sub("frog"),
                sub("animal"),
                sub("bull"),
                sub("bear"),
            ],
        },
        CategoryDef {
            id: 2,
            needles: &[
                sub("trump"),
                sub("biden"),
                sub("maga"),
                sub("election"),
                sub("president"),
                sub("political"),
                sub("potus"),
            ],
        },
        CategoryDef {
            id: 3,
            needles: &[
                sub("musk"),
                sub("elon"),
                sub("taylor"),
                sub("celeb"),
                sub("kanye"),
                sub("star"),
            ],
        },
        CategoryDef {
            id: 4,
            needles: &[
                sub("ai"),
                sub("gpt"),
                sub("agent"),
                sub("neural"),
                sub("llm"),
                sub("bot"),
                sub("model"),
            ],
        },
    ],
};

/// The v1 taxonomy: the same four categories and the same **stable ids**, with
/// every short or English-common needle demoted to
/// [`CategoryMatchMode::Word`].
///
/// ## Responsibility
/// Fix-forward for the v0 substring defect (see [`CategoryMatchMode`]). Ids are
/// deliberately unchanged so a v0-stamped assignment and a v1-stamped assignment
/// of the *same* token remain directly comparable; only the version stamp and the
/// matching discipline move. The six proven v0 mis-classifications — `Fair
/// Launch`, `Catalyst`, `Bottom Signal`, `Bullish Chain`, `Starter Pack`, and
/// `Magazine` — all resolve to [`CATEGORY_UNCLASSIFIED`] here, which is the
/// honest answer: no lexical evidence, so no category (§6.4).
///
/// Word-boundary needles: `ai`, `cat`, `bot`, `star`, `bull`, `bear`, `inu`,
/// `llm`, `maga`, `elon`, `gpt`. Each is either two-to-four bytes or a common
/// English infix (`fair`/`chain`, `catalyst`, `bottom`/`robot`, `starter`,
/// `bullish`, `bearing`, `minute`, `magazine`, `melon`). Long, distinctive
/// needles (`doge`, `pepe`, `neural`, `president`, …) keep
/// [`CategoryMatchMode::Substring`] because an incidental hit is not a realistic
/// concern and substring matching preserves compound-word recall
/// (`pepekingdom`, `dogecoin`).
///
/// `dog` deliberately stays a substring: unlike `cat` it has essentially no
/// high-frequency English carrier word in ticker space, while compound animal
/// tickers (`dogwifhat`, `dogbrain`) are exactly the population the Animal
/// category exists to catch.
///
/// Constitution: §21.4, criterion 81 (non-retroactive versioned assignment),
/// §102 (every needle and its mode is a named, reviewable constant).
pub const TAXONOMY_V1: CategoryTaxonomy = CategoryTaxonomy {
    version: TAXONOMY_VERSION_V1,
    categories: &[
        CategoryDef {
            id: 1,
            needles: &[
                sub("dog"),
                sub("doge"),
                sub("shib"),
                word("inu"),
                word("cat"),
                sub("pepe"),
                sub("frog"),
                sub("animal"),
                word("bull"),
                word("bear"),
            ],
        },
        CategoryDef {
            id: 2,
            needles: &[
                sub("trump"),
                sub("biden"),
                word("maga"),
                sub("election"),
                sub("president"),
                sub("political"),
                sub("potus"),
            ],
        },
        CategoryDef {
            id: 3,
            needles: &[
                sub("musk"),
                word("elon"),
                sub("taylor"),
                sub("celeb"),
                sub("kanye"),
                word("star"),
            ],
        },
        CategoryDef {
            id: 4,
            needles: &[
                word("ai"),
                word("gpt"),
                sub("agent"),
                sub("neural"),
                word("llm"),
                word("bot"),
                sub("model"),
            ],
        },
    ],
};

/// The result of classifying a token's metadata into a category.
///
/// ## Responsibility
/// Carries the assigned category id **and** the slot at which the assignment
/// was made and the taxonomy version used, so the assignment is auditable and
/// **never retroactive** — a later re-classification is a new assignment at a
/// new slot, never a rewrite of this one (criterion 81).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategoryAssignment {
    /// Assigned category id, or [`CATEGORY_UNCLASSIFIED`].
    pub category_id: EntityId,
    /// Slot at which the classifying metadata was observed (caller-supplied
    /// time). The assignment is valid *from this slot onward*.
    pub assigned_at_slot: u64,
    /// Taxonomy version used for this assignment.
    pub taxonomy_version: u32,
}

/// Deterministically classify a token into a category from its name and symbol.
///
/// ## Responsibility
/// The **category-assignment classifier v0** (§21.4 layer 1). Pure function:
/// lowercases ASCII, scans the taxonomy in order, and returns the first
/// category with a keyword that is a substring of either the name or the
/// symbol. No float, no clock — the `slot` is supplied by the caller and only
/// stamped onto the result, guaranteeing time-safety and non-retroactivity
/// (criterion 81).
///
/// ASCII-only case folding is used deliberately: it is allocation-light,
/// deterministic across platforms, and sufficient for ticker/name matching;
/// non-ASCII bytes are compared as-is.
#[must_use]
pub fn classify_category(
    name: &str,
    symbol: &str,
    taxonomy: &CategoryTaxonomy,
    slot: u64,
) -> CategoryAssignment {
    for cat in taxonomy.categories {
        for needle in cat.needles {
            if category_needle_matches(name, needle) || category_needle_matches(symbol, needle) {
                return CategoryAssignment {
                    category_id: cat.id,
                    assigned_at_slot: slot,
                    taxonomy_version: taxonomy.version,
                };
            }
        }
    }

    CategoryAssignment {
        category_id: CATEGORY_UNCLASSIFIED,
        assigned_at_slot: slot,
        taxonomy_version: taxonomy.version,
    }
}

/// Whether `b` is an ASCII alphanumeric byte (a "word" byte for boundary
/// purposes). Non-ASCII bytes count as non-word, so a needle adjacent to
/// multi-byte text still matches at a boundary.
const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// ASCII-case-insensitive test of whether `hay[at..]` starts with `needle`.
fn starts_with_ci(hay: &[u8], at: usize, needle: &[u8]) -> bool {
    let Some(slice) = hay.get(at..at + needle.len()) else {
        return false;
    };
    for (h, n) in slice.iter().zip(needle.iter()) {
        if !h.eq_ignore_ascii_case(n) {
            return false;
        }
    }
    true
}

/// Whether `needle` occurs in `hay` under its [`CategoryMatchMode`],
/// ASCII-case-insensitively.
///
/// Allocation-free: the haystack is scanned in place with byte-wise case folding
/// rather than being lowercased into a new buffer, so the classifier no longer
/// allocates at all. An empty needle never matches (it would otherwise fire on
/// every token). Panic-free on any input, including non-ASCII/multi-byte text.
#[must_use]
pub fn category_needle_matches(hay: &str, needle: &CategoryNeedle) -> bool {
    let n = needle.text.as_bytes();
    let h = hay.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return false;
    }
    let last_start = h.len() - n.len();
    let mut i = 0usize;
    while i <= last_start {
        if starts_with_ci(h, i, n) {
            match needle.mode {
                CategoryMatchMode::Substring => return true,
                CategoryMatchMode::Word => {
                    let before_ok = match i.checked_sub(1).and_then(|p| h.get(p)) {
                        Some(b) => !is_word_byte(*b),
                        None => true,
                    };
                    let after_ok = match h.get(i + n.len()) {
                        Some(b) => !is_word_byte(*b),
                        None => true,
                    };
                    if before_ok && after_ok {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Per-category on-chain factual accumulator.
#[derive(Clone, Debug)]
struct CategoryAgg {
    launches: u64,
    graduations: u64,
    buy_quote: u128,
    sell_quote: u128,
    buy_count: u64,
    sell_count: u64,
    creators: BoundedSet,
}

impl CategoryAgg {
    /// Create an empty accumulator whose creator set holds at most `cap`
    /// distinct creators (§99 memory bound).
    fn new(cap: usize) -> Self {
        CategoryAgg {
            launches: 0,
            graduations: 0,
            buy_quote: 0,
            sell_quote: 0,
            buy_count: 0,
            sell_count: 0,
            creators: BoundedSet::with_capacity(cap),
        }
    }
}

/// A category on-chain factual event.
///
/// ## Responsibility
/// Feeds the reducer with decoded on-chain facts, already tagged with the
/// category id from [`classify_category`]. Slot is caller-supplied (time-safe).
#[derive(Clone, Copy, Debug)]
pub struct CategoryEvent {
    /// Category id this event belongs to.
    pub category_id: EntityId,
    /// The specific on-chain fact.
    pub kind: CategoryEventKind,
    /// Slot of the event.
    pub slot: u64,
}

/// The kind of on-chain fact in a [`CategoryEvent`].
#[derive(Clone, Copy, Debug)]
pub enum CategoryEventKind {
    /// A token launch attributed to this category, by `creator`.
    Launch {
        /// Creator entity id.
        creator: EntityId,
    },
    /// A graduation/migration of a token in this category.
    Graduation,
    /// A buy in this category's tokens.
    Buy {
        /// Quote lamports.
        quote_lamports: u64,
    },
    /// A sell in this category's tokens.
    Sell {
        /// Quote lamports.
        quote_lamports: u64,
    },
}

/// Per-category factual measures at a point in time.
///
/// ## Responsibility
/// The inspectable on-chain measures for one category (§21.4). Purely factual,
/// integer; no interpretation. Net flow is signed lamports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategoryMeasures {
    /// Category id.
    pub category_id: EntityId,
    /// Launches attributed to the category.
    pub launches: u64,
    /// Graduations in the category.
    pub graduations: u64,
    /// Total buy quote (lamports).
    pub buy_quote: u128,
    /// Total sell quote (lamports).
    pub sell_quote: u128,
    /// Net flow = buy_quote - sell_quote (signed lamports, saturating).
    pub net_flow: i128,
    /// Buy event count.
    pub buy_count: u64,
    /// Sell event count.
    pub sell_count: u64,
    /// Distinct creators launching in the category (lower bound if
    /// [`CategoryMeasures::completeness`] is Incomplete).
    pub unique_creators: u32,
    /// Completeness of the creator count.
    pub completeness: Completeness,
}

/// A full snapshot of every tracked category's measures plus the market total.
///
/// ## Responsibility
/// The `MetaRotationState` factual layer at one instant. Category *share* and
/// rotation signals are derived from this via [`rotation_between`].
#[derive(Clone, Debug)]
pub struct MetaRotationState {
    /// Taxonomy version in force.
    pub taxonomy_version: u32,
    /// Per-category measures, in ascending category-id order (deterministic).
    pub categories: Vec<CategoryMeasures>,
    /// Total launches across all tracked categories (excludes UNCLASSIFIED
    /// only if UNCLASSIFIED events were never ingested).
    pub total_launches: u64,
    /// Total buy quote across all categories (lamports).
    pub total_buy_quote: u128,
    /// Whether category tracking overflowed its capacity.
    pub completeness: Completeness,
}

impl MetaRotationState {
    /// Look up one category's measures by id.
    #[must_use]
    pub fn category(&self, id: EntityId) -> Option<&CategoryMeasures> {
        self.categories.iter().find(|c| c.category_id == id)
    }

    /// Launch share of a category in bps of total launches. `None` when there
    /// are no launches (UNKNOWN, §6.4).
    #[must_use]
    pub fn launch_share_bps(&self, id: EntityId) -> Option<u64> {
        let cat = self.category(id)?;
        ratio_bps(u128::from(cat.launches), u128::from(self.total_launches))
    }
}

/// Emergence / saturation / rotation signal for a single category, derived
/// between two [`MetaRotationState`] snapshots.
///
/// ## Responsibility
/// The rotation-detection layer (§21.4 "rotation/emergence/saturation
/// signals"). All integer/bps; a positive `launch_share_change_bps` with rising
/// net flow indicates emergence, while a high launch share with *negative* net
/// flow change indicates saturation/distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategoryRotation {
    /// Category id.
    pub category_id: EntityId,
    /// Change in launch share, later minus earlier, in bps (signed).
    pub launch_share_change_bps: i64,
    /// Change in net flow, later minus earlier, signed lamports (saturating).
    pub net_flow_change: i128,
    /// Launch-count growth ratio in bps (later*1e4/earlier). `None` when the
    /// earlier launch count was zero (growth from zero is UNKNOWN-scaled and
    /// flagged via [`CategoryRotation::emerging_from_zero`]).
    pub launch_growth_bps: Option<u64>,
    /// True when the category had zero launches earlier and positive launches
    /// later — fresh emergence.
    pub emerging_from_zero: bool,
    /// Heuristic-free classification flags derived purely from the deltas.
    pub emerging: bool,
    /// Saturation flag: category holds meaningful launch share but net-flow
    /// change is non-positive (inflow no longer keeping pace).
    pub saturating: bool,
}

/// Compute per-category rotation signals between an `earlier` and a `later`
/// snapshot.
///
/// ## Responsibility
/// Pure deterministic diff (§21.4, §22). `min_share_bps` is the explicit,
/// versioned threshold above which a category is considered to hold meaningful
/// share for saturation classification (§102 no silent magic numbers). The
/// result is ordered by category id and covers the union of category ids
/// present in either snapshot.
#[must_use]
pub fn rotation_between(
    earlier: &MetaRotationState,
    later: &MetaRotationState,
    min_share_bps: u64,
) -> Vec<CategoryRotation> {
    // Union of category ids, deterministic ascending order.
    let mut ids: Vec<EntityId> = earlier
        .categories
        .iter()
        .chain(later.categories.iter())
        .map(|c| c.category_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();

    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let e_share = earlier.launch_share_bps(id).unwrap_or(0);
        let l_share = later.launch_share_bps(id).unwrap_or(0);
        let share_change =
            i64::try_from(l_share).unwrap_or(i64::MAX) - i64::try_from(e_share).unwrap_or(i64::MAX);

        let e_flow = earlier.category(id).map(|c| c.net_flow).unwrap_or(0);
        let l_flow = later.category(id).map(|c| c.net_flow).unwrap_or(0);
        let flow_change = l_flow.saturating_sub(e_flow);

        let e_launches = earlier.category(id).map(|c| c.launches).unwrap_or(0);
        let l_launches = later.category(id).map(|c| c.launches).unwrap_or(0);

        let emerging_from_zero = e_launches == 0 && l_launches > 0;
        let launch_growth_bps = if e_launches == 0 {
            None
        } else {
            ratio_bps(u128::from(l_launches), u128::from(e_launches))
        };

        // Emerging: gaining launch share AND gaining net flow.
        let emerging = share_change > 0 && flow_change > 0;
        // Saturating: holds meaningful share but inflow is not growing.
        let saturating = l_share >= min_share_bps && flow_change <= 0;

        out.push(CategoryRotation {
            category_id: id,
            launch_share_change_bps: share_change,
            net_flow_change: flow_change,
            launch_growth_bps,
            emerging_from_zero,
            emerging,
            saturating,
        });
    }
    out
}

/// Streaming reducer building per-category on-chain measures.
///
/// ## Responsibility
/// Accumulates [`CategoryEvent`]s into bounded per-category state (§21.4, §99).
/// Category count and per-category creator sets are capacity-bounded; overflow
/// is reported as [`Completeness::Incomplete`].
#[derive(Clone, Debug)]
pub struct MetaRotationReducer {
    taxonomy_version: u32,
    categories: BoundedMap<CategoryAgg>,
    max_creators_per_category: usize,
    total_launches: u64,
    total_buy_quote: u128,
}

impl MetaRotationReducer {
    /// Create a reducer tracking at most `max_categories` categories, each with
    /// at most `max_creators_per_category` distinct creators (§99).
    #[must_use]
    pub fn new(
        taxonomy_version: u32,
        max_categories: usize,
        max_creators_per_category: usize,
    ) -> Self {
        MetaRotationReducer {
            taxonomy_version,
            categories: BoundedMap::with_capacity(max_categories),
            max_creators_per_category,
            total_launches: 0,
            total_buy_quote: 0,
        }
    }

    /// Ingest one category on-chain event (saturating accumulation).
    pub fn ingest(&mut self, ev: &CategoryEvent) {
        let cap = self.max_creators_per_category;
        let Some(agg) = self
            .categories
            .get_or_insert_with(ev.category_id, || CategoryAgg::new(cap))
        else {
            // Category capacity exceeded; still count market-wide launch total
            // so shares of tracked categories stay meaningful lower bounds.
            if matches!(ev.kind, CategoryEventKind::Launch { .. }) {
                self.total_launches = self.total_launches.saturating_add(1);
            }
            if let CategoryEventKind::Buy { quote_lamports } = ev.kind {
                self.total_buy_quote = self
                    .total_buy_quote
                    .saturating_add(u128::from(quote_lamports));
            }
            return;
        };

        match ev.kind {
            CategoryEventKind::Launch { creator } => {
                agg.launches = agg.launches.saturating_add(1);
                agg.creators.insert(creator);
                self.total_launches = self.total_launches.saturating_add(1);
            }
            CategoryEventKind::Graduation => {
                agg.graduations = agg.graduations.saturating_add(1);
            }
            CategoryEventKind::Buy { quote_lamports } => {
                agg.buy_quote = agg.buy_quote.saturating_add(u128::from(quote_lamports));
                agg.buy_count = agg.buy_count.saturating_add(1);
                self.total_buy_quote = self
                    .total_buy_quote
                    .saturating_add(u128::from(quote_lamports));
            }
            CategoryEventKind::Sell { quote_lamports } => {
                agg.sell_quote = agg.sell_quote.saturating_add(u128::from(quote_lamports));
                agg.sell_count = agg.sell_count.saturating_add(1);
            }
        }
    }

    /// Produce the current factual snapshot.
    #[must_use]
    pub fn snapshot(&self) -> MetaRotationState {
        let mut categories = Vec::with_capacity(self.categories.len() as usize);
        let mut completeness = self.categories.completeness();

        for (id, agg) in self.categories.iter() {
            let net_flow = i128::try_from(agg.buy_quote)
                .unwrap_or(i128::MAX)
                .saturating_sub(i128::try_from(agg.sell_quote).unwrap_or(i128::MAX));
            completeness = completeness.merge(agg.creators.completeness());
            categories.push(CategoryMeasures {
                category_id: *id,
                launches: agg.launches,
                graduations: agg.graduations,
                buy_quote: agg.buy_quote,
                sell_quote: agg.sell_quote,
                net_flow,
                buy_count: agg.buy_count,
                sell_count: agg.sell_count,
                unique_creators: agg.creators.len(),
                completeness: agg.creators.completeness(),
            });
        }
        // BoundedMap::iter is already ascending by id; keep it explicit.
        categories.sort_by_key(|c| c.category_id);

        MetaRotationState {
            taxonomy_version: self.taxonomy_version,
            categories,
            total_launches: self.total_launches,
            total_buy_quote: self.total_buy_quote,
            completeness,
        }
    }
}

/// Net buy/sell flow imbalance for a category in signed bps, a convenience
/// derived measure used by saturation research. `None` when there is no flow.
///
/// Constitution: §21.4 (per-category on-chain measures), §22 (fixed-point).
#[must_use]
pub fn category_flow_imbalance_bps(m: &CategoryMeasures) -> Option<i64> {
    let total = m.buy_quote.saturating_add(m.sell_quote);
    if total == 0 {
        return None;
    }
    let net = i128::try_from(m.buy_quote).unwrap_or(i128::MAX)
        - i128::try_from(m.sell_quote).unwrap_or(i128::MAX);
    signed_ratio_bps(net, i128::try_from(total).unwrap_or(i128::MAX))
}
