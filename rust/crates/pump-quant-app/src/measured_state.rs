//! The four **measured estimators** that close the fingerprint's fabricated-zero
//! gaps: holder-growth acceleration, creator track record, meta lifecycle phase,
//! and narrative family.
//!
//! # Why this module exists
//!
//! `pump_quant_brain::fingerprint::SetupInputs` has twenty fields. Until now four
//! of them were supplied by the engine from state that could not actually produce
//! the full range of the field:
//!
//! * `holder_growth_accel_bps` was the literal `0` on **every** admit — a
//!   fabricated measurement dressed as a neutral one;
//! * `creator_class` could never be [`CreatorTrack::Proven`] because nothing in
//!   the app tracked launch → migration → survival;
//! * `meta_saturation_state` could never be `Decaying` because the app's rotation
//!   vocabulary had only "emerging / saturating / running";
//! * `narrative_class` could only reach four of the brain's eight nominal slots
//!   because the app's `NarrativeClass` is a four-way axis.
//!
//! Each gap is now closed by a real leaf estimator
//! ([`pump_quant_features::holder_growth`],
//! [`pump_quant_wallet_graph::creator_ledger`],
//! [`pump_quant_market_state::meta_phase`],
//! [`pump_quant_narrative::narrative_family`]). This module is the single app-side
//! seam onto all four: it owns the state, it owns the information-time / slot
//! cursors they are queried at, and it owns the **fail-closed mapping** from each
//! leaf's `Option` / `Unknown` back onto a fingerprint field.
//!
//! # The fail-closed contract, and where the fingerprint cannot honour it
//!
//! Every one of the four estimators refuses below its own evidence floor. When it
//! refuses, this module supplies the fingerprint's documented **neutral** bucket
//! and never a fabricated measurement. Two of the four fields can carry that
//! refusal honestly and two cannot, and the difference is stated here rather than
//! papered over (§6.4):
//!
//! | field | refusal is representable? |
//! |---|---|
//! | `creator_class` | **yes** — `CreatorClass::Unknown` is a distinct nominal slot |
//! | `narrative_class` | **yes** — `NarrativeClass::Unclassified` is a distinct nominal slot |
//! | `holder_growth_accel_bps` | **no** — see [`HOLDER_ACCEL_NEUTRAL_BPS`] |
//! | `meta_saturation_state` | **no** — see [`META_PHASE_NEUTRAL`] |
//!
//! For the two that cannot, "we never measured it" and "we measured the neutral
//! value" occupy the *same* fingerprint code. That is a real, irreducible loss of
//! information in the current ladder, it is not fixable from this side of the
//! crate boundary (the brain is frozen), and it is recorded here so nobody later
//! reads a neutral bucket as evidence of a neutral market.
//!
//! # Purity
//!
//! Integer / fixed-point only (§22), bounded (§99), every threshold a named const
//! with a §-citation (§102). No wall clock: the caller supplies information time
//! (nanoseconds) and chain slots, exactly as replay does.

use std::collections::BTreeMap;

use pump_quant_brain::fingerprint::{
    CreatorClass as BrainCreatorClass, MetaSaturationState, NarrativeClass as BrainNarrativeClass,
};
use pump_quant_features::holder_growth::{
    HolderGrowthConfig, HolderGrowthEstimate, HolderGrowthTracker, HolderSample, MintKey,
};
use pump_quant_market_state::common::EntityId;
use pump_quant_market_state::meta_phase::{
    MetaPhase, MetaPhaseTracker, MetaSample, MetaSampleWrite,
};
use pump_quant_narrative::narrative_family::{
    nv_family_classify_default, FamilyClassification, FamilyEvidence, NarrativeFamily,
};
use pump_quant_wallet_graph::creator_ledger::{CreatorLedger, CreatorTrack, LedgerWrite};
use pump_quant_wallet_graph::{TokenId, WalletId};

// ---------------------------------------------------------------------------
// Named constants (§102)
// ---------------------------------------------------------------------------

/// §6.4 neutral input for `holder_growth_accel_bps` when the estimator refuses.
///
/// `HOLDER_GROWTH_ACCEL_EDGES_BPS = [-500, 0, 500, 2_000]`, so `0` lands in the
/// bucket spanning `[0, 500)` — the ladder's "no acceleration" rung.
///
/// **Honest limitation.** This ladder has no UNKNOWN rung. A mint whose holder
/// series we never captured and a mint whose holder growth we measured at exactly
/// zero acceleration produce the *identical* fingerprint field. Recall cannot tell
/// them apart, and no amount of care on this side of the boundary can make it. The
/// mitigation available today is that the field carries a small weight in
/// [`pump_quant_brain::fingerprint::FeatureWeights`], and that
/// [`MeasuredState::holder_growth_accel_bps`] returns `Option` so a *caller* that
/// needs the distinction (the report plane, a future ladder with an UNKNOWN rung)
/// can still see it. The engine's fingerprint call site cannot.
pub const HOLDER_ACCEL_NEUTRAL_BPS: i64 = 0;

/// §6.4 neutral input for `meta_saturation_state` when the phase tracker refuses.
///
/// **Honest limitation.** [`MetaSaturationState`] is an ORDINAL lifecycle with no
/// UNKNOWN variant, and `Emerging` is both ordinal `0` and the app's existing
/// "this mint has no category at all" default. So three genuinely different
/// situations — no category, a category with too few samples to phase, and a
/// category measured as genuinely emerging — collapse into one bucket. The engine
/// therefore prefers a MEASURED phase whenever one exists and only falls back to
/// the rotation-verdict heuristic (and finally to this constant) when it does not;
/// the collapse is upstream of anything this module can control.
pub const META_PHASE_NEUTRAL: MetaSaturationState = MetaSaturationState::Emerging;

/// §99 bound on the per-mint narrative-family table. Families are launch-time
/// facts, so the table is keyed by mint and evicted lexicographically-smallest
/// first (deterministic — no clock, no insertion order).
pub const FAMILY_TABLE_CAP: usize = 4_096;

// ---------------------------------------------------------------------------
// Ordinal crosswalks
// ---------------------------------------------------------------------------

/// Map a measured [`CreatorTrack`] onto the fingerprint's nominal creator field.
///
/// The two enums' discriminants are declared to match (the leaf's doc comment
/// pins this), but the crosswalk is written out explicitly rather than cast
/// through `ordinal()`: a NOMINAL field mis-mapped by one slot is silent, total,
/// and would poison every conditioned recall keyed on creator class. An explicit
/// `match` fails to compile if either enum gains a variant; an ordinal cast would
/// not.
#[must_use]
pub const fn brain_creator_class(track: CreatorTrack) -> BrainCreatorClass {
    match track {
        CreatorTrack::Unknown => BrainCreatorClass::Unknown,
        CreatorTrack::Proven => BrainCreatorClass::Proven,
        CreatorTrack::Toxic => BrainCreatorClass::Toxic,
        CreatorTrack::Serial => BrainCreatorClass::Serial,
    }
}

/// Map a measured [`MetaPhase`] onto the fingerprint's ordinal lifecycle field.
/// Explicit for the same reason as [`brain_creator_class`].
#[must_use]
pub const fn brain_meta_saturation(phase: MetaPhase) -> MetaSaturationState {
    match phase {
        MetaPhase::Emerging => MetaSaturationState::Emerging,
        MetaPhase::Hot => MetaSaturationState::Hot,
        MetaPhase::Saturated => MetaSaturationState::Saturated,
        MetaPhase::Decaying => MetaSaturationState::Decaying,
    }
}

/// Map a measured [`NarrativeFamily`] onto the fingerprint's nominal narrative
/// field. Explicit for the same reason as [`brain_creator_class`]; this is the
/// crosswalk that makes the Animal / Stream / Seasonal slots reachable at all.
#[must_use]
pub const fn brain_narrative_class(family: NarrativeFamily) -> BrainNarrativeClass {
    match family {
        NarrativeFamily::Unclassified => BrainNarrativeClass::Unclassified,
        NarrativeFamily::Animal => BrainNarrativeClass::Animal,
        NarrativeFamily::Political => BrainNarrativeClass::Political,
        NarrativeFamily::Celebrity => BrainNarrativeClass::Celebrity,
        NarrativeFamily::Tech => BrainNarrativeClass::Tech,
        NarrativeFamily::Derivative => BrainNarrativeClass::Derivative,
        NarrativeFamily::Stream => BrainNarrativeClass::Stream,
        NarrativeFamily::Seasonal => BrainNarrativeClass::Seasonal,
    }
}

/// Clamp an `i128` into `i64` without panicking (§22 explicit narrowing).
const fn clamp_i64_i128(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

// ---------------------------------------------------------------------------
// The plane
// ---------------------------------------------------------------------------

/// The four measured estimators plus the cursors they are queried at.
///
/// Every store is bounded (§99) and every query is point-in-time (§20): nothing
/// here reads a clock, and a query at slot / instant `t` can only see facts
/// recorded at or before `t`.
#[derive(Debug)]
pub struct MeasuredState {
    holder: HolderGrowthTracker,
    holder_cfg: HolderGrowthConfig,
    creators: CreatorLedger,
    meta: MetaPhaseTracker,
    families: BTreeMap<[u8; 32], FamilyClassification>,
    /// Highest CHAIN slot seen on any slot-bearing observation. The slot-keyed
    /// ledgers are queried here. `0` until a slot-bearing fact arrives, and at
    /// slot `0` an untracked creator classifies `Unknown` — so an unfed plane is
    /// fail-closed by construction, not by convention.
    chain_slot: u64,
    /// Per-mint last holder-sample information time, so a repeated capture at the
    /// same instant is dropped rather than refused noisily.
    holder_last_ns: BTreeMap<MintKey, u64>,
    /// Per-category CUMULATIVE totals as of the previous meta sample. The phase
    /// classifier detects a peak-and-decline, which a monotone cumulative counter
    /// can never exhibit, so the series is built from PER-INTERVAL deltas held
    /// against these. See [`MeasuredState::record_meta_interval`].
    meta_prev_totals: BTreeMap<EntityId, MetaTotals>,
}

/// Cumulative on-chain totals for one category at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetaTotals {
    /// Distinct creators who have ever launched into the category.
    pub unique_creators: u64,
    /// Total buy quote observed, lamports.
    pub buy_quote: u128,
    /// Total sell quote observed, lamports.
    pub sell_quote: u128,
    /// Total buy events observed.
    pub buy_count: u64,
    /// Total sell events observed.
    pub sell_count: u64,
}

impl MetaTotals {
    /// Total trade events (both sides).
    #[must_use]
    pub const fn events(&self) -> u64 {
        self.buy_count.saturating_add(self.sell_count)
    }
}

impl Default for MeasuredState {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasuredState {
    /// An empty plane: no holder series, no creator history, no meta samples, no
    /// classified families. Every estimator refuses in this state.
    #[must_use]
    pub fn new() -> Self {
        MeasuredState {
            holder: HolderGrowthTracker::default(),
            holder_cfg: HolderGrowthConfig::DEFAULT,
            creators: CreatorLedger::with_defaults(),
            meta: MetaPhaseTracker::with_defaults(),
            families: BTreeMap::new(),
            chain_slot: 0,
            holder_last_ns: BTreeMap::new(),
            meta_prev_totals: BTreeMap::new(),
        }
    }

    /// Advance the chain-slot cursor. Monotone: a late-arriving lower slot never
    /// moves it backwards (§20 — information time does not run backwards).
    pub fn observe_slot(&mut self, slot: u64) {
        self.chain_slot = self.chain_slot.max(slot);
    }

    /// The highest chain slot observed (the as-of cursor for the slot-keyed
    /// ledgers).
    #[must_use]
    pub const fn chain_slot(&self) -> u64 {
        self.chain_slot
    }

    // ---- §70.1 holder growth ------------------------------------------------

    /// Record one holder-count observation for `mint_id` at information time
    /// `info_time_ns`.
    ///
    /// Holder counts are an **account-state** fact (an RPC/indexer read), not a
    /// swap-stream fact, so they do not appear in the dossier-locked
    /// [`crate::event::AppEvent`] vocabulary and arrive through this parallel
    /// capture seam instead — the same pattern the first-slot fee record uses.
    /// A non-advancing timestamp is dropped (the series requires monotone
    /// information time); the return says whether the sample landed.
    pub fn record_holder_count(
        &mut self,
        mint_id: MintKey,
        holder_count: u64,
        info_time_ns: u64,
    ) -> bool {
        if let Some(&last) = self.holder_last_ns.get(&mint_id) {
            if info_time_ns <= last {
                return false;
            }
        }
        let ok = self
            .holder
            .push(
                mint_id,
                HolderSample {
                    ts_ns: info_time_ns,
                    holder_count,
                },
            )
            .is_ok();
        if ok {
            self.holder_last_ns.insert(mint_id, info_time_ns);
        }
        ok
    }

    /// The measured holder-growth acceleration for `mint_id` as known at
    /// `as_of_ns`, or `None` when the estimator refuses (fewer than three usable
    /// samples, a stale interval, or a zero base count).
    ///
    /// `None` is the honest answer and the report plane consumes it as such. The
    /// fingerprint call site must collapse it — see [`HOLDER_ACCEL_NEUTRAL_BPS`].
    #[must_use]
    pub fn holder_growth_accel_bps(&self, mint_id: MintKey, as_of_ns: u64) -> Option<i64> {
        self.holder_estimate(mint_id, as_of_ns).map(|e| e.accel_bps)
    }

    /// The full holder-growth estimate (accel, both first differences, and the
    /// three comparison points actually used), or `None`.
    #[must_use]
    pub fn holder_estimate(&self, mint_id: MintKey, as_of_ns: u64) -> Option<HolderGrowthEstimate> {
        self.holder
            .estimate_as_of(mint_id, as_of_ns, &self.holder_cfg)
    }

    /// The fingerprint input: the measured acceleration, or the ladder's neutral
    /// rung when there is no measurement. **Never** a fabricated reading — but see
    /// [`HOLDER_ACCEL_NEUTRAL_BPS`] for why the fingerprint cannot tell the two
    /// apart once collapsed.
    #[must_use]
    pub fn holder_growth_accel_input(&self, mint_id: MintKey, as_of_ns: u64) -> i64 {
        self.holder_growth_accel_bps(mint_id, as_of_ns)
            .unwrap_or(HOLDER_ACCEL_NEUTRAL_BPS)
    }

    /// Number of mints with a holder series (bounded by the tracker capacity).
    #[must_use]
    pub fn holder_series_len(&self) -> usize {
        self.holder.len()
    }

    // ---- §29.9 creator track record -----------------------------------------

    /// Record a launch: `creator` deployed `token` at `slot`.
    pub fn record_launch(&mut self, creator: u64, token: u64, slot: u64) -> LedgerWrite {
        self.observe_slot(slot);
        self.creators
            .record_launch(WalletId(creator), TokenId(token), slot)
    }

    /// Record that `token` migrated / graduated at `slot`.
    pub fn record_migration(&mut self, creator: u64, token: u64, slot: u64) -> LedgerWrite {
        self.observe_slot(slot);
        self.creators
            .record_migration(WalletId(creator), TokenId(token), slot)
    }

    /// Record a rug / LP-pull signature on `token` at `slot`. Idempotent: the
    /// FIRST observed rug is the one that counts (§20), so re-observing a live
    /// creator dump every tick cannot inflate the count.
    pub fn record_rug(&mut self, creator: u64, token: u64, slot: u64) -> LedgerWrite {
        self.observe_slot(slot);
        self.creators
            .record_rug(WalletId(creator), TokenId(token), slot)
    }

    /// Classify `creator`'s track record as known at the current chain-slot
    /// cursor. [`CreatorTrack::Unknown`] for an untracked or thin history — the
    /// refusal is a real variant, so this one maps onto the fingerprint losslessly.
    #[must_use]
    pub fn creator_track(&self, creator: u64) -> CreatorTrack {
        self.creators
            .classify_as_of(WalletId(creator), self.chain_slot)
    }

    /// Distinct creators with a recorded history (bounded, §99).
    #[must_use]
    pub fn creator_ledger_len(&self) -> usize {
        self.creators.len()
    }

    /// Read-only view of the creator ledger, for the report plane.
    #[must_use]
    pub const fn creator_ledger(&self) -> &CreatorLedger {
        &self.creators
    }

    // ---- §21.4 meta lifecycle phase -----------------------------------------

    /// Record one factual meta observation for `category` at `sample.slot`.
    ///
    /// Criterion 83 binds here: only decoded on-chain measures may populate this
    /// series. The engine feeds participation from distinct creators, attention
    /// from the category's on-chain event count, and the realized-outcome axis
    /// from the category's measured flow imbalance — never from a social score.
    ///
    /// **Time axis.** This series is sampled on the engine's *logical tick* (the
    /// reflection cadence), NOT on the chain slot: a category's health is measured
    /// over the engine's own observation windows, and the reflection cadence is
    /// what makes the sample spacing regular. The creator ledger is the opposite —
    /// its survival horizon is a chain-slot quantity — so the two deliberately do
    /// not share a cursor, and [`Self::observe_slot`] is NOT called here. Mixing
    /// them would either stall the meta series on an engine that sees no launches
    /// or corrupt the creator survival clock with tick counts.
    pub fn record_meta_sample(
        &mut self,
        category: EntityId,
        sample: MetaSample,
    ) -> MetaSampleWrite {
        self.meta.record(category, sample)
    }

    /// Record one meta observation from CUMULATIVE totals, differencing against
    /// the previous totals for that category.
    ///
    /// # Why the delta, and not the totals
    ///
    /// [`MetaPhaseTracker`] classifies `Decaying` by detecting a **peak and a
    /// decline** in participation / attention / realized outcome. A cumulative
    /// counter is monotone non-decreasing, so it never declines and `Decaying`
    /// would be exactly as structurally unreachable as it was before this wiring —
    /// one unreachable state swapped for another. The engine's `MetaRotationState`
    /// measures ARE cumulative, so the honest fix is to sample the interval:
    ///
    /// * `participation` — NEW distinct creators launching into the category this
    ///   interval. A meta whose arrivals are falling off a peak is exactly the
    ///   §21.4 "participation falling" signal.
    /// * `attention` — trade events attributed to the category this interval. An
    ///   ACTIVITY level, not a social score: criterion 83 forbids social
    ///   interpretation from populating factual meta state.
    /// * `realized_outcome_bps` — the interval's flow imbalance,
    ///   `(buy - sell) * 10_000 / (buy + sell)`, signed.
    ///
    /// An interval with **no** flow is not a measurement of a quiet meta, it is the
    /// absence of a measurement: the sample is SKIPPED and `None` is returned
    /// (§6.4). The totals are still advanced so the next interval is correct.
    pub fn record_meta_interval(
        &mut self,
        category: EntityId,
        slot: u64,
        totals: MetaTotals,
    ) -> Option<MetaSampleWrite> {
        let prev = self
            .meta_prev_totals
            .insert(category, totals)
            .unwrap_or_default();
        let buy = totals.buy_quote.saturating_sub(prev.buy_quote);
        let sell = totals.sell_quote.saturating_sub(prev.sell_quote);
        let gross = buy.saturating_add(sell);
        if gross == 0 {
            return None;
        }
        // Signed interval flow imbalance in bps, evaluated in i128 (§22).
        let net =
            i128::try_from(buy).unwrap_or(i128::MAX) - i128::try_from(sell).unwrap_or(i128::MAX);
        let denom = i128::try_from(gross).unwrap_or(i128::MAX).max(1);
        let realized_outcome_bps = clamp_i64_i128(net.saturating_mul(10_000) / denom);
        Some(self.record_meta_sample(
            category,
            MetaSample {
                slot,
                participation: totals.unique_creators.saturating_sub(prev.unique_creators),
                attention: totals.events().saturating_sub(prev.events()),
                realized_outcome_bps,
            },
        ))
    }

    /// The measured lifecycle phase of `category` as known at `as_of_tick`, or
    /// `None` when the tracker refuses (untracked, below the sample floor, or the
    /// measures name no phase). `as_of_tick` is on the same logical-tick axis the
    /// samples were recorded on (see [`Self::record_meta_sample`]).
    #[must_use]
    pub fn meta_phase_of(&self, category: EntityId, as_of_tick: u64) -> Option<MetaPhase> {
        self.meta.phase_as_of(category, as_of_tick)
    }

    /// Categories with a recorded phase series.
    #[must_use]
    pub fn meta_phase_len(&self) -> u32 {
        self.meta.len()
    }

    // ---- §21.4 narrative family ---------------------------------------------

    /// Classify and remember `mint`'s narrative family from its launch metadata.
    ///
    /// This is a **separate axis** from the attention plane's `NarrativeClass`,
    /// which keeps owning the ceiling/conviction semantics; this one owns the
    /// eight-slot nominal identity the brain's fingerprint keys on. First
    /// classification wins — a launch's family is a launch-time fact and a later
    /// re-read must not silently re-label an already-fingerprinted mint (§81).
    /// Bounded (§99), deterministic eviction.
    pub fn classify_family(
        &mut self,
        mint: [u8; 32],
        name: &str,
        symbol: &str,
        live_stream_active: Option<bool>,
        derivative_similarity_bps: Option<u32>,
    ) -> FamilyClassification {
        if let Some(existing) = self.families.get(&mint) {
            return *existing;
        }
        let c = nv_family_classify_default(&FamilyEvidence {
            name,
            symbol,
            live_stream_active,
            derivative_similarity_bps,
        });
        if self.families.len() >= FAMILY_TABLE_CAP {
            if let Some(&victim) = self.families.keys().next() {
                self.families.remove(&victim);
            }
        }
        self.families.insert(mint, c);
        c
    }

    /// The recorded family classification for `mint`, or `None` if its launch
    /// metadata was never observed.
    #[must_use]
    pub fn family_of(&self, mint: &[u8; 32]) -> Option<FamilyClassification> {
        self.families.get(mint).copied()
    }

    /// Mints with a recorded family (bounded, §99).
    #[must_use]
    pub fn family_len(&self) -> usize {
        self.families.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_wallet_graph::creator_ledger::{
        CREATOR_MIN_SURVIVED_FOR_PROVEN, CREATOR_SURVIVAL_HORIZON_SLOTS,
    };

    const SEC: u64 = 1_000_000_000;

    #[test]
    fn an_unfed_plane_refuses_every_estimator() {
        let m = MeasuredState::new();
        assert_eq!(m.holder_growth_accel_bps(1, 10 * SEC), None);
        assert_eq!(m.creator_track(7), CreatorTrack::Unknown);
        assert_eq!(m.meta_phase_of(3, 0), None);
        assert_eq!(m.family_of(&[0u8; 32]), None);
        // …and the fingerprint inputs are the documented neutral rungs.
        assert_eq!(
            m.holder_growth_accel_input(1, 10 * SEC),
            HOLDER_ACCEL_NEUTRAL_BPS
        );
    }

    #[test]
    fn holder_growth_needs_three_samples_and_then_measures() {
        let mut m = MeasuredState::new();
        assert!(m.record_holder_count(1, 100, SEC));
        assert!(m.record_holder_count(1, 110, 2 * SEC));
        // Two samples: still refuses (fail-closed below the floor).
        assert_eq!(m.holder_growth_accel_bps(1, 2 * SEC), None);
        assert!(m.record_holder_count(1, 140, 3 * SEC));
        // Third sample: growth ACCELERATED (10% then ~27%), so accel > 0.
        let accel = m.holder_growth_accel_bps(1, 3 * SEC).expect("measured");
        assert!(accel > 0, "accelerating holder growth must read positive");
        // A non-advancing timestamp is dropped, not accepted out of order.
        assert!(!m.record_holder_count(1, 999, 3 * SEC));
    }

    #[test]
    fn creator_becomes_proven_only_after_survival_and_never_before() {
        let mut m = MeasuredState::new();
        let horizon = CREATOR_SURVIVAL_HORIZON_SLOTS;
        for i in 0..u64::from(CREATOR_MIN_SURVIVED_FOR_PROVEN) {
            m.record_launch(9, 100 + i, 10 + i);
            m.record_migration(9, 100 + i, 20 + i);
        }
        // Before the horizon elapses nothing is proven.
        assert_eq!(m.creator_track(9), CreatorTrack::Unknown);
        m.observe_slot(30 + horizon);
        assert_eq!(
            m.creator_track(9),
            CreatorTrack::Proven,
            "Proven is now REACHABLE from app state (it never was before)"
        );
        // A rug dominates: the risk read wins over the survival read.
        m.record_rug(9, 100, 30 + horizon);
        assert_eq!(m.creator_track(9), CreatorTrack::Toxic);
        // …and re-observing the same live dump cannot inflate the count.
        assert_eq!(
            m.record_rug(9, 100, 30 + horizon + 1),
            LedgerWrite::Refused,
            "the FIRST observed rug is the one that counts (§20)"
        );
    }

    #[test]
    fn meta_phase_reaches_decaying_which_the_app_could_never_express() {
        let mut m = MeasuredState::new();
        // Rise then fall in participation AND attention: two falling measures.
        let series: &[(u64, u64, u64, i64)] = &[
            (10, 40, 1_000, 500),
            (20, 60, 2_000, 600),
            (30, 30, 900, -200),
            (40, 20, 500, -400),
        ];
        for &(slot, participation, attention, outcome) in series {
            m.record_meta_sample(
                7,
                MetaSample {
                    slot,
                    participation,
                    attention,
                    realized_outcome_bps: outcome,
                },
            );
        }
        assert_eq!(m.meta_phase_of(7, 40), Some(MetaPhase::Decaying));
        assert_eq!(
            brain_meta_saturation(MetaPhase::Decaying),
            MetaSaturationState::Decaying
        );
    }

    #[test]
    fn family_reaches_the_slots_the_four_way_class_could_not() {
        let mut m = MeasuredState::new();
        let animal = m.classify_family([1u8; 32], "Doge Killer", "DOGE", None, None);
        assert_eq!(animal.family, NarrativeFamily::Animal);
        assert_eq!(
            brain_narrative_class(animal.family),
            BrainNarrativeClass::Animal
        );
        let stream = m.classify_family([2u8; 32], "Anything", "ANY", Some(true), None);
        assert_eq!(stream.family, NarrativeFamily::Stream);
        let seasonal = m.classify_family([3u8; 32], "Santa Rally", "XMAS", None, None);
        assert_eq!(seasonal.family, NarrativeFamily::Seasonal);
        // No evidence stays UNCLASSIFIED — a refusal the nominal field CAN carry.
        let none = m.classify_family([4u8; 32], "Zorble", "ZRB", None, None);
        assert_eq!(none.family, NarrativeFamily::Unclassified);
        assert_eq!(
            brain_narrative_class(none.family),
            BrainNarrativeClass::Unclassified
        );
        // First classification wins: a later re-read cannot re-label the mint.
        let again = m.classify_family([4u8; 32], "Doge", "DOGE", None, None);
        assert_eq!(again.family, NarrativeFamily::Unclassified);
    }

    #[test]
    fn cumulative_totals_are_differenced_so_decaying_stays_reachable() {
        let mut m = MeasuredState::new();
        // Feeding the CUMULATIVE totals directly would be monotone in every axis
        // and could never decline; the interval form declines correctly.
        let rounds: &[(u64, u64, u128, u128, u64, u64)] = &[
            // slot, cum creators, cum buy, cum sell, cum buys, cum sells
            (10, 10, 10_000, 4_000, 40, 20),
            (20, 25, 40_000, 12_000, 140, 60),
            (30, 27, 42_000, 30_000, 150, 130),
            (40, 27, 42_500, 42_000, 152, 200),
        ];
        for &(slot, creators, buy, sell, bc, sc) in rounds {
            m.record_meta_interval(
                5,
                slot,
                MetaTotals {
                    unique_creators: creators,
                    buy_quote: buy,
                    sell_quote: sell,
                    buy_count: bc,
                    sell_count: sc,
                },
            );
        }
        assert_eq!(m.meta_phase_of(5, 40), Some(MetaPhase::Decaying));
    }

    #[test]
    fn an_interval_with_no_flow_is_not_a_measurement() {
        let mut m = MeasuredState::new();
        let totals = MetaTotals {
            unique_creators: 3,
            buy_quote: 100,
            sell_quote: 50,
            buy_count: 2,
            sell_count: 1,
        };
        assert!(m.record_meta_interval(1, 10, totals).is_some());
        // Identical totals ⇒ zero flow in the interval ⇒ SKIPPED, not a zero.
        assert!(m.record_meta_interval(1, 20, totals).is_none());
    }

    #[test]
    fn the_chain_slot_cursor_is_monotone() {
        let mut m = MeasuredState::new();
        m.observe_slot(50);
        m.observe_slot(10);
        assert_eq!(m.chain_slot(), 50, "a late lower slot never rewinds time");
    }
}
