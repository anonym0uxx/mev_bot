//! §70.1 **continuous holder accounting**, folded from our OWN decoded swap flow.
//!
//! # Why this module exists
//!
//! Holder growth is one of the §70.1 "money" leading indicators: accumulation
//! *broadens* — more distinct addresses hold a non-zero balance — before price
//! confirms it. Until now the app had two things and neither was a holder count:
//!
//! * [`crate::engine::Engine::observe_holder_count`], a parallel capture seam for
//!   a third-party (RPC / indexer) holder read, which **nothing in production
//!   called**. The §70.1 fingerprint field therefore took the neutral rung on
//!   every real admit — a fabricated non-measurement dressed as a neutral one.
//! * `Features::unique_buyers`, a `count_ones()` over a 64-bit bitset indexed by
//!   `entity % 64` ([`crate::lane`]). That is a coarse *breadth* proxy: it
//!   saturates at 64, it collides (entities 3 and 67 are the same bit), and it is
//!   **monotone non-decreasing** — a bit once set is never cleared, so it can
//!   never observe distribution. It is not a holder count and must not feed a
//!   money term as if it were one.
//!
//! This module derives holder state **continuously from canonical evidence we
//! decoded ourselves**. Every [`crate::event::AppEvent::MarketTrade`] already
//! carries a real `buyer_entity: u64` and a signed base quantity. Folding those
//! into a per-mint position ledger makes the resulting holder count:
//!
//! * **canonical** (§6.1 — raw on-chain evidence we decoded, not a vendor claim);
//! * **continuous** (it updates on every swap, at watch time, not at admit time);
//! * **zero added latency** (no RPC round trip sits between the swap and the
//!   number);
//! * **replay-deterministic** (§22/§54 — the same tape yields the same series);
//! * **third-party-free**. Birdeye / DAS holder counts remain strictly
//!   corroboration-tier (§6.6) and may never populate this state.
//!
//! # The accounting
//!
//! Per watched mint we keep `entity -> net base-token position`, folded per swap:
//!
//! | event | effect |
//! |---|---|
//! | buy by an entity whose tracked position is **zero** | `live += 1`, position `= qty` |
//! | buy by an entity already holding | position `+= qty`, count unchanged |
//! | sell that takes a tracked position **to zero** | `live -= 1`, position `= 0` |
//! | sell that leaves a tracked position positive (partial) | count unchanged |
//! | sell by an entity with **no** tracked position | see below — the exactness falsifier |
//! | zero-quantity swap | no position moves, so no count moves |
//!
//! Positions saturate at zero and are never negative: a sell larger than the
//! position we tracked cannot mean a negative balance, it means the entity held
//! tokens from before our observation window.
//!
//! # THE OBSERVATION-WINDOW LAW
//!
//! **From flow alone we can only ever observe entities that trade during OUR
//! observation window.** Everything this module reports is a consequence of that
//! single sentence, so it is stated plainly and then enforced in the type:
//!
//! * A mint we have watched **from its creation event** has seen every transfer
//!   of consequence that could create a holder, because at creation the holder
//!   set is empty. Its count is [`HolderCountBasis::Exact`]: an absolute number
//!   of holders.
//! * A mint **discovered mid-life** already had holders before we arrived, and
//!   nothing in the swap stream can tell us how many. We know only the *change*
//!   in holders across our window. Its count is [`HolderCountBasis::DeltaOnly`].
//! * A mint whose distinct-entity count has exceeded [`HOLDER_ENTITY_CAP`] has a
//!   **truncated** ledger: arrivals past the cap are not tracked at all, so both
//!   the level and the rate understate reality by an unknown amount. Its count is
//!   [`HolderCountBasis::Incomplete`] — a lower bound, and §6.4 forbids reporting
//!   a lower bound as if it were a measurement.
//!
//! ## Why the distinction is the crux, not a footnote
//!
//! The field the brain fingerprint actually needs is holder-growth
//! **acceleration** — a *second derivative*. A second derivative of a series that
//! is offset by an unknown constant is unaffected by that constant in its
//! absolute form, which is exactly why `DeltaOnly` is a legitimate basis for a
//! GROWTH consumer while being useless to a LEVEL consumer. Any consumer that
//! wants "how many holders does this mint have" must refuse under `DeltaOnly`,
//! because the honest answer is "we do not know, we only know it moved by N".
//!
//! That asymmetry is enforced structurally: [`HolderReading`]'s count field is
//! private and there is no accessor that returns a bare number. A level consumer
//! calls [`HolderReading::level`] and receives `None` under `DeltaOnly` and
//! `Incomplete`; a growth consumer calls [`HolderReading::growth_level`] and
//! receives `None` only under `Incomplete`. There is no path that hands a level
//! consumer a delta-only count.
//!
//! ## The exactness falsifier (§6.4 — a claim we can disprove, we do)
//!
//! `Exact` is *claimed* by the discovery path (a creation sighting arrived before
//! any swap). It is **falsified by evidence**: if an entity ever sells from a
//! position we never tracked, that entity provably held before our window, so our
//! claim to have seen the holder set from empty was wrong, and the mint is
//! demoted `Exact -> DeltaOnly` on the spot. The basis lattice only ever moves in
//! the direction of less confidence (see [`HolderCountBasis::worst`]) — it is
//! monotone, so a late falsification can never be washed out by later good
//! behaviour.
//!
//! ## The DeltaOnly bias, stated rather than hidden
//!
//! Under `DeltaOnly` the sampled series is `live` — the count of entities we have
//! watched accumulate and still hold. That is a strict LOWER BOUND on the true
//! holder count. A *relative* growth rate computed off a lower-bound base has a
//! smaller denominator than the truth, so it **overstates** the relative rate.
//! The overstatement partially cancels in the second difference (both intervals
//! share the bias direction), but it does not vanish. This is a known, signed
//! bias in the `DeltaOnly` growth reading; it is not corrected here because any
//! correction would require the unknown pre-window base, i.e. a fabrication.
//!
//! # Bounds (§99/§57)
//!
//! [`HOLDER_ENTITY_CAP`] entities per mint and [`HOLDER_FLOW_MINT_CAP`] mints,
//! both hard. Entities live in a per-mint sorted `Vec` (binary search on the hot
//! path, insertion only on a genuinely new entity), so the per-mint footprint is
//! a flat 16 bytes per tracked entity. A new mint at capacity evicts the
//! least-recently-traded record (ties broken by the smaller mint key), which is a
//! pure function of state — no clock, no insertion-order dependence.
//!
//! # Purity
//!
//! Integer only (§22), every threshold a named const with a §-citation (§102), no
//! wall clock (the caller supplies the logical tick and the derived information
//! time), no RNG, no allocation on the per-swap fold once an entity is known.

use std::collections::BTreeMap;

/// §99/§57 bound on distinct entities tracked per mint.
///
/// Five hundred and twelve distinct trading entities is well past the breadth any
/// pump.fun-scale launch reaches inside a scalp horizon, and at a flat 16 bytes
/// per entry it fixes the per-mint ledger at 8 KiB. Past this the ledger is
/// truncated and the mint's basis becomes [`HolderCountBasis::Incomplete`]
/// forever — the count degrades to a documented lower bound rather than silently
/// reporting a wrong number (§6.4).
pub const HOLDER_ENTITY_CAP: usize = 512;

/// §99/§57 bound on mints with a live holder ledger.
///
/// Matches the holder-growth tracker's own mint capacity
/// (`pump_quant_features::holder_growth::HOLDER_TRACKER_CAP`) so the two layers
/// cannot disagree about which mints exist. Worst case is
/// `512 * 512 * 16 B = 4 MiB`. A new mint at capacity evicts the
/// least-recently-traded record.
pub const HOLDER_FLOW_MINT_CAP: usize = 512;

/// Minimum logical ticks between two holder samples pushed into the §70.1
/// acceleration estimator (§20 / §102).
///
/// The estimator refuses comparison points closer than
/// `pump_quant_features::holder_growth::HOLDER_MIN_INTERVAL_NS` (1 s), because
/// sub-interval sampling turns integer quantization into a fabricated
/// acceleration. One engine tick is
/// [`crate::brain::BRAIN_TICK_NS`] = 400 ms of information time, so three ticks
/// is 1.2 s — the smallest whole-tick cadence at or above the estimator's floor.
/// Sampling on **every swap** would push many samples inside one tick, all but
/// the first of which are dropped for a non-advancing information time; this
/// cadence is the honest version of "continuous" given the estimator's floors.
pub const HOLDER_SAMPLE_INTERVAL_TICKS: u64 = 3;

/// Compile-time proof that [`HOLDER_SAMPLE_INTERVAL_TICKS`] clears the
/// estimator's minimum spacing (§102: the relationship between two named
/// constants is checked, not remembered).
const _: () = assert!(
    HOLDER_SAMPLE_INTERVAL_TICKS * crate::brain::BRAIN_TICK_NS
        >= pump_quant_features::holder_growth::HOLDER_MIN_INTERVAL_NS,
    "holder sampling cadence must be at or above the §70.1 estimator's minimum interval"
);

/// What kind of number a holder reading actually is (§6.4 UNKNOWN discipline).
///
/// Ordered by decreasing confidence. See the module docs for the
/// observation-window law that produces the distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HolderCountBasis {
    /// Observed from the mint's creation event, before any swap: the holder set
    /// started empty and we have watched every position-creating trade since.
    /// The count is an ABSOLUTE number of holders.
    Exact,
    /// Observed from mid-life: an unknown number of holders existed before our
    /// window. The count is a CHANGE, not a level. Legitimate for a derivative
    /// consumer, refused to a level consumer.
    DeltaOnly,
    /// The per-mint entity ledger overflowed [`HOLDER_ENTITY_CAP`]: arrivals past
    /// the cap are untracked, so the count understates by an unknown amount. A
    /// LOWER BOUND, refused to both level and growth consumers.
    Incomplete,
}

impl HolderCountBasis {
    /// Whether a consumer that wants an ABSOLUTE holder level may read this
    /// reading. Only [`HolderCountBasis::Exact`] qualifies.
    #[must_use]
    pub const fn admits_level(self) -> bool {
        matches!(self, HolderCountBasis::Exact)
    }

    /// Whether a consumer that wants holder GROWTH (a derivative) may read this
    /// reading. `Exact` and `DeltaOnly` qualify; `Incomplete` does not, because a
    /// truncated ledger biases the *rate* as well as the level.
    #[must_use]
    pub const fn admits_growth(self) -> bool {
        matches!(self, HolderCountBasis::Exact | HolderCountBasis::DeltaOnly)
    }

    /// Confidence rank (0 = most confident). Used only by [`Self::worst`].
    #[must_use]
    const fn rank(self) -> u8 {
        match self {
            HolderCountBasis::Exact => 0,
            HolderCountBasis::DeltaOnly => 1,
            HolderCountBasis::Incomplete => 2,
        }
    }

    /// The less confident of two bases. The basis lattice only ever moves this
    /// way, so a falsified exactness claim can never be recovered.
    #[must_use]
    pub const fn worst(self, other: HolderCountBasis) -> HolderCountBasis {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// A point-in-time holder reading for one mint, with its basis attached.
///
/// The count is **private by design**. Every accessor that yields a number is
/// gated on [`HolderCountBasis`], so it is structurally impossible for a level
/// consumer to obtain a delta-only or truncated count (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HolderReading {
    basis: HolderCountBasis,
    /// Entities with a strictly positive tracked position right now.
    live: u64,
    /// Distinct entities in the ledger: current holders PLUS entities we watched
    /// exit to zero (they are retained so a re-entry is a correct `+1`). Always
    /// `>= live`, and bounded by [`HOLDER_ENTITY_CAP`].
    entities_tracked: u32,
    truncated: u64,
    unattributed_exits: u64,
    first_ns: u64,
    last_ns: u64,
}

impl HolderReading {
    /// What kind of number this is.
    #[must_use]
    pub const fn basis(&self) -> HolderCountBasis {
        self.basis
    }

    /// The ABSOLUTE holder count, or `None` when the basis cannot support one.
    ///
    /// `None` under [`HolderCountBasis::DeltaOnly`] (we do not know the
    /// pre-window base) and under [`HolderCountBasis::Incomplete`] (the ledger is
    /// truncated). This is the level-consumer door and it is the only one.
    #[must_use]
    pub const fn level(&self) -> Option<u64> {
        if self.basis.admits_level() {
            Some(self.live)
        } else {
            None
        }
    }

    /// The count usable as the base of a GROWTH / derivative measurement, or
    /// `None` under [`HolderCountBasis::Incomplete`].
    ///
    /// Under `Exact` this equals [`Self::level`]. Under `DeltaOnly` it is a
    /// strict lower bound whose *changes* are the measured holder flow; see the
    /// module docs for the resulting signed bias in relative rates.
    #[must_use]
    pub const fn growth_level(&self) -> Option<u64> {
        if self.basis.admits_growth() {
            Some(self.live)
        } else {
            None
        }
    }

    /// The raw tracked count with NO basis gate, explicitly named as a lower
    /// bound. Diagnostics / report plane only — never a decision input. Callers
    /// wanting a decision-grade number use [`Self::level`] or
    /// [`Self::growth_level`].
    #[must_use]
    pub const fn lower_bound(&self) -> u64 {
        self.live
    }

    /// Distinct entities in the mint's ledger (holders plus fully-exited ones).
    #[must_use]
    pub const fn entities_tracked(&self) -> u32 {
        self.entities_tracked
    }

    /// Entity arrivals refused by [`HOLDER_ENTITY_CAP`]. Non-zero implies
    /// [`HolderCountBasis::Incomplete`].
    #[must_use]
    pub const fn truncated(&self) -> u64 {
        self.truncated
    }

    /// Sells observed from entities with no tracked position — provable
    /// pre-window holders. Non-zero falsifies an `Exact` claim.
    #[must_use]
    pub const fn unattributed_exits(&self) -> u64 {
        self.unattributed_exits
    }

    /// Information time of the first folded swap for this mint.
    #[must_use]
    pub const fn first_ns(&self) -> u64 {
        self.first_ns
    }

    /// Information time of the most recent folded swap for this mint.
    #[must_use]
    pub const fn last_ns(&self) -> u64 {
        self.last_ns
    }
}

/// What one folded swap did to a mint's holder state.
///
/// `sample` is `Some(count)` exactly when the [`HOLDER_SAMPLE_INTERVAL_TICKS`]
/// cadence fired on this swap; the caller pushes that value into the §70.1
/// acceleration estimator. It is a `u64` and not a [`HolderReading`] because the
/// estimator consumes a series of counts; the basis travels with the reading, and
/// the caller consults the reading, not the sample, when it needs to know what
/// kind of number the series is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HolderFold {
    /// Signed change in the live holder count caused by this swap (`-1`, `0`, or
    /// `+1`).
    pub delta: i8,
    /// The count to sample into the acceleration estimator, when the cadence
    /// fired.
    pub sample: Option<u64>,
    /// True when this swap pushed the mint past [`HOLDER_ENTITY_CAP`].
    pub truncated: bool,
}

/// One mint's holder ledger.
#[derive(Debug, Clone)]
struct MintHolders {
    /// `entity -> net base position`, sorted by entity (binary search).
    /// A retained entry with position `0` means "seen, currently not a holder" —
    /// which is what makes a re-entry a genuine `+1` and a repeated exit a no-op.
    entities: Vec<(u64, u64)>,
    /// Entities with a strictly positive position.
    live: u64,
    basis: HolderCountBasis,
    truncated: u64,
    unattributed_exits: u64,
    first_ns: u64,
    last_ns: u64,
    last_tick: u64,
    /// Tick of the last sample handed to the estimator; `None` before the first.
    last_sample_tick: Option<u64>,
}

impl MintHolders {
    fn new(basis: HolderCountBasis, now: u64, ns: u64) -> Self {
        MintHolders {
            entities: Vec::new(),
            live: 0,
            basis,
            truncated: 0,
            unattributed_exits: 0,
            first_ns: ns,
            last_ns: ns,
            last_tick: now,
            last_sample_tick: None,
        }
    }

    fn reading(&self) -> HolderReading {
        HolderReading {
            basis: self.basis,
            live: self.live,
            entities_tracked: u32::try_from(self.entities.len()).unwrap_or(u32::MAX),
            truncated: self.truncated,
            unattributed_exits: self.unattributed_exits,
            first_ns: self.first_ns,
            last_ns: self.last_ns,
        }
    }

    /// Whether the sampling cadence fires at `now` (§20: the FIRST observation of
    /// a mint always samples, so the series starts as early as the evidence does;
    /// afterwards a whole [`HOLDER_SAMPLE_INTERVAL_TICKS`] must elapse).
    fn sample_due(&self, now: u64) -> bool {
        match self.last_sample_tick {
            None => true,
            Some(t) => now.saturating_sub(t) >= HOLDER_SAMPLE_INTERVAL_TICKS,
        }
    }
}

/// The continuous per-mint holder-accounting plane (§70.1).
///
/// See the module docs for the accounting rules, the observation-window law and
/// the bounds. Construct with [`HolderFlow::new`]; fold with
/// [`HolderFlow::observe_swap`]; read with [`HolderFlow::reading`].
#[derive(Debug, Clone)]
pub struct HolderFlow {
    mints: BTreeMap<[u8; 32], MintHolders>,
    /// Mints seen at their creation event and not yet folded — the pending
    /// `Exact` claim. Bounded by the same mint cap.
    creation_seen: BTreeMap<[u8; 32], u64>,
    mint_cap: usize,
    entity_cap: usize,
    evictions: u64,
}

impl Default for HolderFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl HolderFlow {
    /// An empty plane at the named-const bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_caps(HOLDER_FLOW_MINT_CAP, HOLDER_ENTITY_CAP)
    }

    /// An empty plane with explicit bounds (tests exercise the cap behaviour at a
    /// small size; production uses [`HolderFlow::new`]). Both caps are clamped to
    /// at least 1 so the structure can always hold the live mint / entity.
    #[must_use]
    pub fn with_caps(mint_cap: usize, entity_cap: usize) -> Self {
        HolderFlow {
            mints: BTreeMap::new(),
            creation_seen: BTreeMap::new(),
            mint_cap: mint_cap.max(1),
            entity_cap: entity_cap.max(1),
            evictions: 0,
        }
    }

    /// Mints with a live holder ledger.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mints.len()
    }

    /// Whether no mint is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mints.is_empty()
    }

    /// Ledgers evicted by [`HOLDER_FLOW_MINT_CAP`].
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Configured per-mint entity bound.
    #[must_use]
    pub const fn entity_cap(&self) -> usize {
        self.entity_cap
    }

    /// Record that `mint`'s CREATION was observed, at logical tick `now`.
    ///
    /// This is the only path to [`HolderCountBasis::Exact`]: it asserts that we
    /// began watching before the holder set could become non-empty. The claim is
    /// only honoured if it arrives BEFORE any swap is folded for the mint — a
    /// creation sighting that arrives after we have already been counting flow
    /// cannot retroactively make the earlier window complete (§20/§81:
    /// information is not retroactive). It remains falsifiable thereafter (see
    /// the module docs).
    pub fn note_creation(&mut self, mint: &[u8; 32], now: u64) {
        if self.mints.contains_key(mint) {
            // Flow already folded: our window began mid-life regardless of what
            // the discovery lane says. Leave the basis alone.
            return;
        }
        if !self.creation_seen.contains_key(mint) && self.creation_seen.len() >= self.mint_cap {
            if let Some(&victim) = self.creation_seen.keys().next() {
                self.creation_seen.remove(&victim);
            }
        }
        self.creation_seen.insert(*mint, now);
    }

    /// Fold one decoded swap into `mint`'s holder ledger.
    ///
    /// `signed_base` is the swap's signed base-token quantity (positive = buy),
    /// exactly as it arrives on [`crate::event::AppEvent::MarketTrade`]. `now` is
    /// the engine's logical tick and `ns` the derived information time; neither is
    /// read from a clock here.
    pub fn observe_swap(
        &mut self,
        mint: &[u8; 32],
        entity: u64,
        signed_base: i64,
        now: u64,
        ns: u64,
    ) -> HolderFold {
        let entity_cap = self.entity_cap;
        let claimed_exact = self.creation_seen.remove(mint).is_some();
        if !self.mints.contains_key(mint) {
            if self.mints.len() >= self.mint_cap {
                if let Some(victim) = self.evict_key() {
                    self.mints.remove(&victim);
                    self.evictions = self.evictions.saturating_add(1);
                }
            }
            if self.mints.len() >= self.mint_cap {
                // Unreachable while `mint_cap >= 1` (a victim always exists), but
                // refusing keeps the bound absolute rather than best-effort (§99).
                return HolderFold::default();
            }
            let basis = if claimed_exact {
                HolderCountBasis::Exact
            } else {
                HolderCountBasis::DeltaOnly
            };
            self.mints.insert(*mint, MintHolders::new(basis, now, ns));
        }
        let Some(m) = self.mints.get_mut(mint) else {
            return HolderFold::default();
        };
        m.last_tick = now;
        m.last_ns = ns;

        let mut fold = HolderFold::default();

        // ---- the accounting (§70.1; see the module table) --------------------
        if signed_base > 0 {
            let qty = signed_base.unsigned_abs();
            match m.entities.binary_search_by_key(&entity, |(e, _)| *e) {
                Ok(pos) => {
                    if let Some(slot) = m.entities.get_mut(pos) {
                        if slot.1 == 0 {
                            // Re-entry after a full exit is a genuine new holder.
                            m.live = m.live.saturating_add(1);
                            fold.delta = 1;
                        }
                        slot.1 = slot.1.saturating_add(qty);
                    }
                }
                Err(pos) => {
                    if m.entities.len() >= entity_cap {
                        // §6.4: we cannot track this holder, so the count becomes a
                        // LOWER BOUND and says so. It is NOT incremented — an
                        // increment we cannot later decrement would corrupt the
                        // series in both directions.
                        m.truncated = m.truncated.saturating_add(1);
                        m.basis = m.basis.worst(HolderCountBasis::Incomplete);
                        fold.truncated = true;
                    } else {
                        m.entities.insert(pos, (entity, qty));
                        m.live = m.live.saturating_add(1);
                        fold.delta = 1;
                    }
                }
            }
        } else if signed_base < 0 {
            let qty = signed_base.unsigned_abs();
            match m.entities.binary_search_by_key(&entity, |(e, _)| *e) {
                Ok(pos) => {
                    let mut exited = false;
                    if let Some(slot) = m.entities.get_mut(pos) {
                        if slot.1 == 0 {
                            // Already fully exited (or a pre-window holder we have
                            // already accounted): another sell tells us nothing new.
                            exited = false;
                            m.unattributed_exits = m.unattributed_exits.saturating_add(1);
                            m.basis = m.basis.worst(HolderCountBasis::DeltaOnly);
                        } else if qty >= slot.1 {
                            // Full exit. Positions saturate at zero — a sell larger
                            // than what we tracked means pre-window inventory, which
                            // also falsifies any exactness claim.
                            if qty > slot.1 {
                                m.basis = m.basis.worst(HolderCountBasis::DeltaOnly);
                            }
                            slot.1 = 0;
                            exited = true;
                        } else {
                            // Partial sell: still a holder.
                            slot.1 -= qty;
                        }
                    }
                    if exited {
                        m.live = m.live.saturating_sub(1);
                        fold.delta = -1;
                    }
                }
                Err(pos) => {
                    // THE EXACTNESS FALSIFIER. This entity provably held before our
                    // window. We cannot know whether this sell was full or partial,
                    // so we do NOT move the count (§6.4 — an unknown exit is not a
                    // measured exit); we record the entity at zero so a later buy is
                    // a correct `+1`, and we demote the basis.
                    m.unattributed_exits = m.unattributed_exits.saturating_add(1);
                    m.basis = m.basis.worst(HolderCountBasis::DeltaOnly);
                    if m.entities.len() >= entity_cap {
                        m.truncated = m.truncated.saturating_add(1);
                        m.basis = m.basis.worst(HolderCountBasis::Incomplete);
                        fold.truncated = true;
                    } else {
                        m.entities.insert(pos, (entity, 0));
                    }
                }
            }
        }
        // signed_base == 0 moves no tokens, so it moves no holder.

        if m.sample_due(now) {
            m.last_sample_tick = Some(now);
            fold.sample = Some(m.live);
        }
        fold
    }

    /// The current holder reading for `mint`, or `None` when untracked.
    #[must_use]
    pub fn reading(&self, mint: &[u8; 32]) -> Option<HolderReading> {
        self.mints.get(mint).map(MintHolders::reading)
    }

    /// The eviction victim: least-recently-traded ledger, ties by smaller mint
    /// key. A pure function of state (§22 determinism).
    fn evict_key(&self) -> Option<[u8; 32]> {
        let mut best: Option<([u8; 32], u64)> = None;
        for (k, m) in &self.mints {
            let replace = match best {
                None => true,
                Some((bk, bt)) => m.last_tick < bt || (m.last_tick == bt && *k < bk),
            };
            if replace {
                best = Some((*k, m.last_tick));
            }
        }
        best.map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: [u8; 32] = [7u8; 32];

    #[test]
    fn basis_lattice_is_monotone_toward_less_confidence() {
        assert_eq!(
            HolderCountBasis::Exact.worst(HolderCountBasis::DeltaOnly),
            HolderCountBasis::DeltaOnly
        );
        assert_eq!(
            HolderCountBasis::Incomplete.worst(HolderCountBasis::Exact),
            HolderCountBasis::Incomplete
        );
        assert!(HolderCountBasis::Exact.admits_level());
        assert!(!HolderCountBasis::DeltaOnly.admits_level());
        assert!(HolderCountBasis::DeltaOnly.admits_growth());
        assert!(!HolderCountBasis::Incomplete.admits_growth());
    }

    #[test]
    fn buy_then_full_sell_is_plus_one_then_minus_one() {
        let mut hf = HolderFlow::new();
        hf.note_creation(&M, 0);
        assert_eq!(hf.observe_swap(&M, 1, 100, 0, 0).delta, 1);
        assert_eq!(hf.reading(&M).and_then(|r| r.level()), Some(1));
        assert_eq!(hf.observe_swap(&M, 1, -40, 1, 400_000_000).delta, 0);
        assert_eq!(hf.reading(&M).and_then(|r| r.level()), Some(1));
        assert_eq!(hf.observe_swap(&M, 1, -60, 2, 800_000_000).delta, -1);
        assert_eq!(hf.reading(&M).and_then(|r| r.level()), Some(0));
    }
}
