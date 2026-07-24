//! LAWs B1–B5 — the **episodic recall memory plane**: the engine's answer to
//! "what happened last time a coin looked like this?".
//!
//! This module is the app-side seam onto the frozen `pump_quant_brain` crate. It
//! owns exactly four things and nothing else:
//!
//! 1. **LAW B1 — episodic recording.** Every completed trade becomes an immutable
//!    [`Episode`] whose fingerprint was quantized from the state captured **at
//!    admit time**, never at exit. A fingerprint computed at exit would be
//!    contaminated by the very price path it is supposed to predict, which makes
//!    the whole memory worthless (and worse: flattering). The capture point is the
//!    gate (see `engine::Engine::brain_entry_at_admit`); this module only stores
//!    what it is handed.
//! 2. **LAW B2 — grounded reflection.** At the reflection cadence the plane
//!    re-queries recall for the setup classes the engine ACTUALLY traded,
//!    conditioned by venue phase / meta category / discovery lane, and caches a
//!    bounded readout for the `Report`. It also carries the meta lifecycle
//!    timeline and the social call/markout ledger, so "what is the state of the
//!    meta this week" and "who called this mint, and does that author actually
//!    earn" are answerable rather than aspirational.
//! 3. **LAW B3 — the reduce-only recall haircut.** Recall may only ever SHRINK or
//!    VETO risk ([`BrainSizeVerdict`]). There is deliberately no size-UP path:
//!    "this class historically won, so bet more" is precisely where episodic
//!    recall overfits, and §46 forbids assuming predictiveness from a sample the
//!    strategy itself generated. The type has no `Boost` variant, so a future
//!    edit cannot quietly add one without an obvious diff.
//! 4. **LAW B4 — fail-closed.** [`RecallVerdict::Unknown`] carries no estimate by
//!    construction, and [`BrainPlane::size_verdict`] maps it to
//!    [`BrainSizeVerdict::Identity`] — an unconditional no-op. Small-n recall is
//!    how a quant fools himself; the engine is provably immune because the
//!    "insufficient evidence" path has no reachable branch that touches size.
//! 5. **LAW B5 — persistence.** [`AppBlobStore`] fences the single I/O surface so
//!    the same [`BrainStore`] type serves a real filesystem, an in-memory test
//!    harness, and a no-op sink, without generics leaking into `Engine`.
//!
//! Everything here is integer / fixed-point (§22), bounded (§99), and every
//! threshold is a named const with a §-citation (§102).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use pump_quant_brain::episode::{
    DiscoveryLane as BrainLane, Episode, EpisodeContext, EpisodeOutcome, ExitReason as BrainExit,
};
use pump_quant_brain::fingerprint::{
    signed_decade, MetaSaturationState, SetupFingerprint, VenuePhase,
};
use pump_quant_brain::meta_timeline::{MetaMatchParams, MetaSnapshot, MetaTimeline, PastMetaMatch};
use pump_quant_brain::persist::{
    BlobStore, BrainStore, FileBlobStore, MemBlobStore, PersistError, RestoreReport,
};
use pump_quant_brain::recall::{
    EpisodicIndex, RecallFilter, RecallParams, RecallVerdict, EPISODE_CAP,
};
use pump_quant_brain::social_recall::{
    AuthorTrackRecord, CallMarkout, CallRecord, Platform, SocialRecallIndex, DEFAULT_CALL_WINDOW_NS,
};

// ---------------------------------------------------------------------------
// Named constants (§102). Every one carries the clause it serves.
// ---------------------------------------------------------------------------

/// §46 minimum matched episodes before a recall verdict may move size. Mirrors
/// `pump_quant_brain::recall::MIN_SAMPLE_DEFAULT`; restated here as the app's own
/// operator-tunable default so the two can be argued about independently.
pub const BRAIN_MIN_SAMPLE_DEFAULT: u32 = 8;

/// §102 stage-1 Hamming radius defining "a setup like this".
///
/// # Why 8, and not the brain's `MAX_DISTANCE_DEFAULT` of 12
///
/// The fingerprint's maximum possible unweighted distance is **77**: the fifteen
/// ORDINAL fields contribute `levels - 1` each (67 in total) and the five NOMINAL
/// fields contribute [`pump_quant_brain::fingerprint::NOMINAL_MISMATCH_COST`] `= 2`
/// each. A radius is therefore a statement about how much of the market picture
/// two setups are allowed to disagree on before they stop being "the same setup".
///
/// 12 fails that statement in a specific, disqualifying way. The order-flow
/// imbalance ladder spans 6 buckets and the CVD-decade ladder spans 6 buckets, so
/// at radius 12 a **maximally net-BUYING** setup matches a **maximally net-SELLING**
/// setup with all eighteen other fields identical. Those are not the same setup by
/// any definition a trader would accept; pooling their outcomes into one median is
/// exactly the failure §100 and §46 exist to prevent. 8 makes that pairing
/// structurally unreachable — a full reversal on BOTH primary flow axes costs 12
/// and can never fit — while still allowing a full reversal on ONE flow axis plus
/// a single nominal mismatch (`6 + 2 = 8`), which is the widest neighbourhood that
/// is still arguable.
///
/// 8 also caps nominal disagreement at four fields; since venue phase is separately
/// HARD-filtered by [`RecallFilter`] (§100 forbids pooling curve and pool
/// outcomes), that is at most four of {narrative, creator, meta slot, designated
/// caller}.
///
/// # Measured effect on the golden tape
///
/// The golden tape seals 13 episodes and the sample floor is 8, so **any** `Known`
/// verdict there necessarily pools at least 8 of 13 episodes — 62% of the entire
/// tape — into one "class". Sweeping the radius over that tape:
///
/// | radius | admit-time recalls Known / total | reflection setup classes |
/// |---|---|---|
/// | 3–6 | 0 / 245 | 0 |
/// | **8 (this default)** | **0 / 245** | **3** |
/// | 10 | 48 / 245 | 6 |
/// | 12 (old default) | 144 / 245 | 6 |
/// | 16 | 216 / 245 | 6 |
///
/// At 12 the engine formed an opinion on 59% of its admits from a 13-episode
/// memory. That is not recall, it is the tape's own average wearing a costume. 8 is
/// the widest radius at which admit-time recall still *refuses on every query* — the
/// only honest answer a 13-episode index can give — while the slower reflection
/// pass, which runs over the complete index with the full §100/§21.4 conditioning,
/// still surfaces 3 classes for inspection.
///
/// The hazard tape in `tests/brain_laws.rs` independently needed a radius of 3 to
/// keep a known winner and a known loser out of one estimate; that tape sets its
/// own radius explicitly and is unaffected by this default.
pub const BRAIN_RECALL_MAX_DISTANCE_DEFAULT: u32 = 8;

/// §29.5/§46 LAW B3 haircut trigger: a setup class whose decisive win rate sits
/// at or below 35% (and whose median realized net is negative) has demonstrated
/// that it bleeds. 35% is deliberately well below a coin-flip: the point is to
/// catch classes that are *structurally* losing, not classes that had a bad week.
pub const BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT: u32 = 3_500;

/// §29.5 LAW B3 veto trigger: at or below a 15% decisive win rate the class is not
/// a trade at any size — the haircut would only shrink a bet that should not
/// exist. Strictly below [`BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT`] (validated).
pub const BRAIN_VETO_WIN_RATE_BP_DEFAULT: u32 = 1_500;

/// §29.5/§56.2 LAW B3 haircut factor: a historically-bleeding class trades at HALF
/// the size the rest of the sizing chain arrived at. Reduce-only — this multiplier
/// is structurally ≤ 10_000 and is validated as such.
pub const BRAIN_HAIRCUT_MULT_BP_DEFAULT: u32 = 5_000;

/// §99 bound on the distinct setup classes the reflection readout tracks. The
/// engine trades far fewer distinct classes than this in any realistic window;
/// past the cap new classes are simply not tracked (a lower bound on coverage,
/// never a wrong number).
pub const BRAIN_TRADED_CLASS_CAP: usize = 64;

/// §99 bound on the setup classes surfaced on the `Report` — the strongest-signal
/// head of the tracked set, not the whole set.
pub const BRAIN_REPORT_CLASS_CAP: usize = 8;

/// §99 bound on the per-author track records surfaced on the `Report`.
pub const BRAIN_REPORT_AUTHOR_CAP: usize = 8;

/// §99 bound on the current-meta rows surfaced on the `Report`.
pub const BRAIN_REPORT_META_CAP: usize = 8;

/// §99 bound on the per-mint designated-caller flag set.
pub const BRAIN_DESIGNATED_MINT_CAP: usize = 4_096;

/// §20 information-time scale: one engine tick is modelled as one Solana slot,
/// ≈400 ms. Used ONLY to project the engine's logical tick clock onto the brain's
/// nanosecond information-time axis (token age, time-of-day fold, hold duration).
/// Never a wall-clock read — replay reproduces the identical projection.
pub const BRAIN_TICK_NS: u64 = 400_000_000;

/// §21.6 LAW B1 range-state ladder: the current bar's range must reach 130% of the
/// prior bar's to count as EXPANDED.
pub const BRAIN_RANGE_EXPAND_BP: u64 = 13_000;
/// §21.6 LAW B1 range-state ladder: at or below 70% of the prior bar's range the
/// tape is COMPRESSED. Strictly below [`BRAIN_RANGE_EXPAND_BP`].
pub const BRAIN_RANGE_COMPRESS_BP: u64 = 7_000;

/// §21.7 LAW B1 burst-phase baseline: the long comparison window is this multiple
/// of the configured recent-activity window.
pub const BRAIN_BURST_BASELINE_MULT: u64 = 4;
/// §21.7 LAW B1 burst-phase ladder: recent arrival intensity at ≥200% of the
/// baseline rate is a CLIMAX.
pub const BRAIN_BURST_CLIMAX_BP: u64 = 20_000;
/// §21.7 LAW B1 burst-phase ladder: ≥120% of baseline is an ONSET.
pub const BRAIN_BURST_ONSET_BP: u64 = 12_000;
/// §21.7 LAW B1 burst-phase ladder: ≤70% of baseline, with a real baseline behind
/// it, is EXHAUSTION.
pub const BRAIN_BURST_EXHAUST_BP: u64 = 7_000;

/// §29 LAW B2 lookback window for "who called this mint" — seven days of
/// information time. Mirrors the brain's `DEFAULT_CALL_WINDOW_NS`.
pub const BRAIN_CALL_WINDOW_NS: u64 = DEFAULT_CALL_WINDOW_NS;

/// §56 LAW B5 snapshot file name appended to the operator's brain path.
pub const BRAIN_SNAPSHOT_SUFFIX: &str = ".snapshot";
/// §56 LAW B5 append-only journal file name appended to the operator's brain path.
pub const BRAIN_JOURNAL_SUFFIX: &str = ".journal";

// ---------------------------------------------------------------------------
// Blob store: the one I/O surface, un-generic so `Engine` stays concrete.
// ---------------------------------------------------------------------------

/// The app's [`BlobStore`] selector.
///
/// `BrainStore` is generic over its store, but `Engine` must be a single concrete
/// type, and the orphan rule forbids `impl BlobStore for Box<dyn BlobStore>`. A
/// local enum solves both: one concrete `BrainStore<AppBlobStore>` lives on the
/// engine, and the variant decides whether writes reach a disk, a test buffer, or
/// nowhere at all.
#[derive(Debug, Default)]
pub enum AppBlobStore {
    /// Persistence disarmed: reads are empty, writes are discarded. The default,
    /// so an engine that never opts in pays no I/O and no allocation.
    #[default]
    Null,
    /// Real filesystem.
    File(FileBlobStore),
    /// In-memory buffer — tests and replay harnesses.
    Mem(MemBlobStore),
}

impl BlobStore for AppBlobStore {
    fn read_all(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self {
            Self::Null => Ok(Vec::new()),
            Self::File(s) => s.read_all(path),
            Self::Mem(s) => s.read_all(path),
        }
    }

    fn append(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Null => Ok(()),
            Self::File(s) => s.append(path, bytes),
            Self::Mem(s) => s.append(path, bytes),
        }
    }

    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Null => Ok(()),
            Self::File(s) => s.write_atomic(path, bytes),
            Self::Mem(s) => s.write_atomic(path, bytes),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        match self {
            Self::Null => false,
            Self::File(s) => s.exists(path),
            Self::Mem(s) => s.exists(path),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry-time capture (LAW B1)
// ---------------------------------------------------------------------------

/// The entry-time episodic capture carried on a pending entry and then on the open
/// position, until the exit books.
///
/// **This is the no-look-ahead boundary.** Both members are computed once, inside
/// the gate, from state that exists strictly BEFORE the position is opened; nothing
/// downstream of the entry may modify them. `brain_laws::b1_fingerprint_is_free_of_look_ahead`
/// pins that by mutating the entire post-entry price path and asserting the recorded
/// fingerprint is byte-identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrainEntry {
    /// The quantized setup signature at admit time.
    pub fingerprint: SetupFingerprint,
    /// Where and when the admit happened (also the recall conditioning key).
    pub context: EpisodeContext,
}

/// What recall is permitted to do to a size. **Reduce-only by construction** —
/// there is no variant that can enlarge risk (§29.5/§46).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrainSizeVerdict {
    /// No evidence, or evidence that does not condemn the class: size is untouched.
    /// This is the ONLY verdict an `Unknown` recall can produce (LAW B4).
    Identity,
    /// The class historically bled: multiply size by this bps factor (≤ 10_000).
    Haircut(u32),
    /// The class bled badly enough that no size is defensible: refuse the entry.
    Veto,
}

impl BrainSizeVerdict {
    /// The reduce-only size multiplier in bps. Structurally ≤ 10_000 for every
    /// variant, so composing it into the haircut product can never enlarge size.
    #[must_use]
    pub const fn mult_bp(self) -> u32 {
        match self {
            Self::Identity => 10_000,
            Self::Haircut(m) => m,
            Self::Veto => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Report-plane readouts (LAW B2)
// ---------------------------------------------------------------------------

/// One recalled setup class the engine actually traded, with the distribution
/// recall attaches to it. Report plane only — never read by a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrainSetupClass {
    /// Packed fingerprint signature — the audit key for the class.
    pub signature: u128,
    /// Venue phase the class is pinned to (§100: never pooled across phases).
    /// `0` = bonding curve, `1` = migrated pool.
    pub venue_phase_code: u8,
    /// Meta category the recall was conditioned on.
    pub meta_category_id: u32,
    /// Discovery lane the recall was conditioned on (`DiscoveryLane::ordinal`).
    pub discovery_lane_code: u8,
    /// Episodes the statistics were computed over.
    pub n_matched: u32,
    /// Median realized net lamports for the class.
    pub median_net_lamports: i128,
    /// Decisive win rate, bps.
    pub win_rate_bp: u32,
    /// Median hold duration, ns of information time.
    pub median_hold_ns: u64,
    /// Hamming distance to the nearest matched episode.
    pub nearest_distance: u32,
    /// `episode_id` of the nearest matched episode — the operator's audit anchor.
    pub nearest_episode_id: u64,
}

/// LAW B6: one traded setup class together with the **full** conditioned recall
/// verdict — including the refusals.
///
/// [`BrainSetupClass`] is the operator's summary readout and deliberately drops
/// classes recall declines to speak about: there is nothing to show. The
/// strategy-analysis export ([`crate::brain_analysis`]) needs the opposite —
/// the refusals ARE the interesting rows, because a consumer that cannot see
/// "this class was examined and refused, for this reason" will quietly mistake
/// absence for evidence of nothing. So this row carries the whole
/// [`RecallVerdict`], whose `Unknown` arm structurally cannot carry an estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConditionedClass {
    /// Packed fingerprint signature — the audit key for the class.
    pub signature: u128,
    /// Venue phase the class is pinned to (§100: never pooled across phases).
    pub venue_phase: VenuePhase,
    /// Meta category the recall was conditioned on.
    pub meta_category_id: u32,
    /// Discovery lane the recall was conditioned on.
    pub discovery_lane: BrainLane,
    /// The conditioned verdict: an estimate, or a refusal that carries none.
    pub verdict: RecallVerdict,
}

/// One author's realized track record — "who called this, and do they earn?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrainAuthorRecord {
    /// Author identity (FNV-1a of the handle).
    pub author_id: u64,
    /// Attributed markouts behind the record.
    pub n_markouts: u32,
    /// Median realized net lamports across those markouts.
    pub median_net_lamports: i128,
    /// Decisive win rate, bps.
    pub win_rate_bp: u32,
}

/// The current state of one meta category on the brain's lifecycle timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrainMetaState {
    /// Category identity.
    pub meta_category_id: u32,
    /// Lifecycle position (`MetaSaturationState::ordinal`).
    pub saturation_code: u8,
    /// Realized net lamports attributed to the category, signed.
    pub aggregate_net_lamports: i128,
    /// Distinct participating creators observed.
    pub participant_breadth: u32,
    /// Episodes (launches) observed in the category.
    pub episode_count: u32,
    /// Information time of the snapshot, ns.
    pub info_time_ns: u64,
}

// ---------------------------------------------------------------------------
// The plane
// ---------------------------------------------------------------------------

/// One distinct setup class the engine traded, retained so reflection can go back
/// and ask what that class actually paid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TradedClass {
    fingerprint: SetupFingerprint,
    venue_phase: VenuePhase,
    meta_category_id: u32,
    discovery_lane: BrainLane,
}

/// The engine's episodic memory plane.
///
/// Bounded in every dimension (§99): the episode index is the brain's own fixed
/// ring, the traded-class set, designated-mint set and every report vector carry
/// explicit caps, and the social/meta indices are the brain's own bounded rings.
#[derive(Debug)]
pub struct BrainPlane {
    /// Durable episodic store — `Some` once LAW B5 persistence is armed. Writes go
    /// to the index AND the append-only journal through it.
    store: Option<BrainStore<AppBlobStore>>,
    /// The in-memory episodic index used while persistence is disarmed. Exactly one
    /// of `store` / `index` is live at a time; [`BrainPlane::index`] is the single
    /// accessor so no call site has to know which.
    index: EpisodicIndex,
    /// Meta lifecycle timeline (§21.4) — one snapshot per category per reflection.
    meta: MetaTimeline,
    /// Social call + markout ledger (§29/§82).
    social: SocialRecallIndex,
    /// Mints with at least one designated-caller call, bounded (§99).
    designated_mints: BTreeSet<u64>,
    /// Recall tunables, built from config at construction.
    params: RecallParams,
    next_episode_id: u64,
    next_call_id: u64,
    /// Monotone information-time cursor for the social index (its append contract
    /// requires non-decreasing stamps; a late-arriving post is pinned forward
    /// rather than dropped).
    social_clock_ns: u64,
    /// Monotone information-time cursor for the meta timeline, same contract.
    meta_clock_ns: u64,
    episodes_recorded: u64,
    recall_known: u64,
    recall_unknown: u64,
    haircuts_applied: u64,
    vetoes: u64,
    traded: Vec<TradedClass>,
    /// Cached LAW B2 readout, refreshed at the reflection cadence.
    classes: Vec<BrainSetupClass>,
}

impl BrainPlane {
    /// Build a plane with persistence disarmed (`Null` blob store).
    ///
    /// `min_sample` and `max_distance` come from the operator config; the rest of
    /// [`RecallParams`] keeps the brain's defaults, including
    /// `require_admitted: true` (§46 — a rejected setup has no realized P&L and
    /// must never dilute an estimate).
    #[must_use]
    pub fn new(min_sample: u32, max_distance: u32) -> Self {
        let params = RecallParams {
            min_sample,
            max_distance,
            ..RecallParams::default()
        };
        Self {
            store: None,
            index: EpisodicIndex::with_capacity(EPISODE_CAP),
            meta: MetaTimeline::new(),
            social: SocialRecallIndex::new(),
            designated_mints: BTreeSet::new(),
            params,
            next_episode_id: 1,
            next_call_id: 1,
            social_clock_ns: 0,
            meta_clock_ns: 0,
            episodes_recorded: 0,
            recall_known: 0,
            recall_unknown: 0,
            haircuts_applied: 0,
            vetoes: 0,
            traded: Vec::new(),
            classes: Vec::new(),
        }
    }

    /// LAW B5: swap in a durable blob store rooted at `base_path`, restoring
    /// whatever snapshot + journal are already there.
    ///
    /// The episode-id counter is re-seeded past the restored maximum, so a restart
    /// can never re-issue an id the index has already seen (the index refuses
    /// non-monotone ids, which is what keeps recall tie-breaks total).
    ///
    /// # Errors
    /// Propagates [`PersistError`] from the restore.
    pub fn attach(
        &mut self,
        store: AppBlobStore,
        base_path: &Path,
    ) -> Result<RestoreReport, PersistError> {
        let mut snapshot = base_path.as_os_str().to_os_string();
        snapshot.push(BRAIN_SNAPSHOT_SUFFIX);
        let mut journal = base_path.as_os_str().to_os_string();
        journal.push(BRAIN_JOURNAL_SUFFIX);
        let snapshot = PathBuf::from(snapshot);
        let journal = PathBuf::from(journal);
        let (opened, report) = BrainStore::open(store, snapshot, journal, EPISODE_CAP)?;
        self.next_episode_id = opened
            .index()
            .last_episode_id()
            .map_or(1, |id| id.saturating_add(1));
        self.store = Some(opened);
        Ok(report)
    }

    /// LAW B5: detach and return the blob store, leaving persistence disarmed.
    /// The in-memory index is preserved — this severs the journal, not the memory.
    /// Used by the restart proof to hand the same buffer to a fresh engine.
    pub fn detach(&mut self) -> AppBlobStore {
        match self.store.take() {
            Some(s) => s.into_blob_store(),
            None => AppBlobStore::Null,
        }
    }

    /// LAW B5: write a snapshot so a restart does not have to replay the whole
    /// journal.
    ///
    /// # Errors
    /// Propagates [`PersistError`] from the blob store.
    pub fn snapshot_now(&mut self) -> Result<(), PersistError> {
        match &mut self.store {
            Some(s) => s.snapshot_now(),
            None => Ok(()),
        }
    }

    /// The live episodic index (read-only; recall reads through here).
    #[must_use]
    pub const fn index(&self) -> &EpisodicIndex {
        match &self.store {
            Some(s) => s.index(),
            None => &self.index,
        }
    }

    /// The social call/markout ledger (read-only).
    #[must_use]
    pub const fn social(&self) -> &SocialRecallIndex {
        &self.social
    }

    /// The meta lifecycle timeline (read-only).
    #[must_use]
    pub const fn meta_timeline(&self) -> &MetaTimeline {
        &self.meta
    }

    /// Recall parameters in force.
    #[must_use]
    pub const fn params(&self) -> &RecallParams {
        &self.params
    }

    /// Episodes recorded this run (LAW B1 readout).
    #[must_use]
    pub const fn episodes_recorded(&self) -> u64 {
        self.episodes_recorded
    }

    /// Admit-time recalls that produced an estimate.
    #[must_use]
    pub const fn recall_known(&self) -> u64 {
        self.recall_known
    }

    /// Admit-time recalls that refused to produce one (LAW B4 exercised).
    #[must_use]
    pub const fn recall_unknown(&self) -> u64 {
        self.recall_unknown
    }

    /// Reduce-only haircuts applied by LAW B3.
    #[must_use]
    pub const fn haircuts_applied(&self) -> u64 {
        self.haircuts_applied
    }

    /// Entries refused by LAW B3.
    #[must_use]
    pub const fn vetoes(&self) -> u64 {
        self.vetoes
    }

    /// LAW B3/B4: the phase-pinned admit-time recall verdict for an entry.
    ///
    /// Uses [`EpisodicIndex::recall`], which is phase-partitioned by construction
    /// (§100 — curve and pool outcomes are never pooled). Deliberately NOT
    /// conditioned on meta/lane here: over-conditioning a live sizing query
    /// guarantees `Unknown` forever, which would make the law decorative. The
    /// tighter meta/lane conditioning belongs to the report plane, where a refusal
    /// costs nothing.
    #[must_use]
    pub fn recall(&self, entry: &BrainEntry) -> RecallVerdict {
        self.index().recall(&entry.fingerprint, &self.params)
    }

    /// LAW B3 + B4: turn an admit-time recall into a **reduce-only** verdict.
    ///
    /// `Unknown` — an empty index, nothing in radius, or `n < min_sample` — maps to
    /// [`BrainSizeVerdict::Identity`] with no branch that can do otherwise. A
    /// `Known` class only loses size when it BOTH bled on the median AND failed the
    /// win-rate bar; a class with a negative median but a healthy win rate (a few
    /// large losses against many small wins) is left alone, because that is a
    /// tail-shape problem the exit ladder owns, not an admission problem.
    ///
    /// Counts every verdict for the report plane, in both arms of the A/B, so the
    /// armed and disarmed runs do identical work and differ only in whether the
    /// verdict is *acted on*.
    pub fn size_verdict(
        &mut self,
        entry: &BrainEntry,
        armed: bool,
        haircut_win_rate_bp: u32,
        veto_win_rate_bp: u32,
        haircut_mult_bp: u32,
    ) -> BrainSizeVerdict {
        let verdict = self.recall(entry);
        let Some(stats) = verdict.stats() else {
            self.recall_unknown = self.recall_unknown.saturating_add(1);
            return BrainSizeVerdict::Identity;
        };
        self.recall_known = self.recall_known.saturating_add(1);
        if !armed {
            return BrainSizeVerdict::Identity;
        }
        if stats.n_matched < self.params.min_sample {
            // Defence in depth: the brain already enforces this, but the law is
            // stated here too so a future params edit cannot smuggle small-n in.
            return BrainSizeVerdict::Identity;
        }
        let bled = stats.median_net_lamports < 0;
        if !bled {
            return BrainSizeVerdict::Identity;
        }
        if stats.win_rate_bp <= veto_win_rate_bp {
            self.vetoes = self.vetoes.saturating_add(1);
            return BrainSizeVerdict::Veto;
        }
        if stats.win_rate_bp <= haircut_win_rate_bp {
            self.haircuts_applied = self.haircuts_applied.saturating_add(1);
            // Reduce-only clamp: the configured factor can never exceed identity.
            return BrainSizeVerdict::Haircut(haircut_mult_bp.min(10_000));
        }
        BrainSizeVerdict::Identity
    }

    /// LAW B1/B2: remember that this setup class was actually traded, so reflection
    /// can go back and ask what it paid. Bounded (§99); deduped on the exact
    /// conditioning key.
    pub fn on_admit(&mut self, entry: &BrainEntry) {
        let class = TradedClass {
            fingerprint: entry.fingerprint,
            venue_phase: entry.context.venue_phase,
            meta_category_id: entry.context.meta_category_id,
            discovery_lane: entry.context.discovery_lane,
        };
        if self.traded.contains(&class) {
            return;
        }
        if self.traded.len() >= BRAIN_TRADED_CLASS_CAP {
            return;
        }
        self.traded.push(class);
    }

    /// LAW B1: seal the completed trade as an immutable episode.
    ///
    /// The fingerprint and context are the ones captured at ADMIT; only the outcome
    /// comes from the exit. Episode ids are issued from a strictly increasing
    /// counter, so the index's monotonicity contract holds across a restart.
    ///
    /// The realized net also becomes a social markout for every author who called
    /// this mint inside the lookback window — that is what makes
    /// [`BrainAuthorRecord`] a measured track record rather than a follower count.
    pub fn record_exit(
        &mut self,
        entry: &BrainEntry,
        realized_net_lamports: i128,
        hold_duration_ns: u64,
        exit_reason: BrainExit,
        mfe_bps: i64,
        mae_bps: i64,
    ) {
        let outcome = EpisodeOutcome {
            realized_net_lamports,
            hold_duration_ns,
            exit_reason,
            mfe_bps,
            mae_bps,
            was_admitted: true,
        };
        let episode = Episode::new(
            self.next_episode_id,
            entry.fingerprint,
            entry.context,
            outcome,
        );
        // A refused record (non-monotone id, foreign schema, I/O failure) must not
        // take the engine down: the brain is advisory, and a lost memory is a
        // smaller harm than a halted trading loop. It is counted only on success,
        // so the readout never over-reports.
        let admitted = match &mut self.store {
            Some(s) => s.record(episode).is_ok(),
            None => self.index.push(episode).is_ok(),
        };
        if admitted {
            self.episodes_recorded = self.episodes_recorded.saturating_add(1);
            self.next_episode_id = self.next_episode_id.saturating_add(1);
        }
        self.attribute_markouts(
            entry.context.mint_id,
            entry.context.info_time_ns,
            realized_net_lamports,
            hold_duration_ns,
        );
    }

    /// §82 LAW B2: attribute one realized outcome back to every author who called
    /// the mint inside the lookback window.
    fn attribute_markouts(
        &mut self,
        mint_id: u64,
        entry_time_ns: u64,
        realized_net_lamports: i128,
        hold_duration_ns: u64,
    ) {
        let calls = self
            .social
            .who_called(mint_id, entry_time_ns, BRAIN_CALL_WINDOW_NS);
        if calls.is_empty() {
            return;
        }
        let stamp = self
            .social_clock_ns
            .max(entry_time_ns.saturating_add(hold_duration_ns));
        self.social_clock_ns = stamp;
        for call in calls {
            let _ = self.social.record_markout(CallMarkout {
                call_id: call.call_id,
                author_id: call.author_id,
                realized_net_lamports,
                hold_duration_ns,
                info_time_ns: stamp,
            });
        }
    }

    /// LAW B2: record one observed social call. Ids are issued monotonically and
    /// the information-time cursor is pinned non-decreasing, so a late-arriving
    /// post is stamped at the cursor instead of being dropped.
    ///
    /// Returns the issued `call_id` when the call landed, so a caller holding the
    /// post's content digest can bind a
    /// [`pump_quant_brain::social_support::ContentEchoWitness`] to it. Near-
    /// duplicate detection is what separates BREADTH (independent originators)
    /// from ECHO (the same post relayed), and without the id the support estimator
    /// can only ever report an upper bound.
    pub fn record_call(
        &mut self,
        mint_id: u64,
        author_id: u64,
        platform: Platform,
        info_time_ns: u64,
        engagement: u64,
        was_designated: bool,
    ) -> Option<u64> {
        let stamp = self.social_clock_ns.max(info_time_ns);
        self.social_clock_ns = stamp;
        let call = CallRecord {
            call_id: self.next_call_id,
            mint_id,
            author_id,
            platform,
            info_time_ns: stamp,
            // Engagement is the only reach proxy the capture lanes carry; its
            // signed decade is the brain's canonical scale-reducer (§22).
            followers_decade: signed_decade(i128::from(engagement)),
            was_designated,
        };
        let issued = if self.social.record_call(call).is_ok() {
            let id = self.next_call_id;
            self.next_call_id = self.next_call_id.saturating_add(1);
            Some(id)
        } else {
            None
        };
        if was_designated && self.designated_mints.len() < BRAIN_DESIGNATED_MINT_CAP {
            self.designated_mints.insert(mint_id);
        }
        issued
    }

    /// Whether a designated (tracked, scored) caller has called this mint — one of
    /// the twenty fingerprint fields. Sticky within a run and bounded (§99).
    #[must_use]
    pub fn designated_caller_present(&self, mint_id: u64) -> bool {
        self.designated_mints.contains(&mint_id)
    }

    /// LAW B2: push one meta-lifecycle snapshot. The timeline's append contract
    /// requires non-decreasing information time, so the cursor is pinned forward.
    pub fn record_meta_snapshot(
        &mut self,
        meta_category_id: u32,
        info_time_ns: u64,
        saturation: MetaSaturationState,
        aggregate_net_lamports: i128,
        participant_breadth: u32,
        episode_count: u32,
    ) {
        let stamp = self.meta_clock_ns.max(info_time_ns);
        self.meta_clock_ns = stamp;
        let _ = self.meta.push(MetaSnapshot {
            meta_category_id,
            info_time_ns: stamp,
            saturation,
            aggregate_net_lamports,
            participant_breadth,
            episode_count,
        });
    }

    /// LAW B2: re-run conditioned recall over every setup class the engine actually
    /// traded and cache the result for the `Report`.
    ///
    /// The conditioning is the full §100/§21.4/§29.9 key — venue phase, exact meta
    /// category, discovery lane — because on the report plane a refusal is free and
    /// a pooled estimate is a lie. Classes recall declines to speak about are simply
    /// absent from the readout (`Unknown` carries nothing to display).
    pub fn refresh_reflection(&mut self) {
        let mut out: Vec<BrainSetupClass> = Vec::new();
        for class in &self.traded {
            let filter = RecallFilter::for_phase(class.venue_phase)
                .with_meta_category(class.meta_category_id)
                .with_discovery_lane(class.discovery_lane);
            let verdict =
                self.index()
                    .recall_conditioned(&class.fingerprint, &self.params, &filter);
            let Some(stats) = verdict.stats() else {
                continue;
            };
            out.push(BrainSetupClass {
                signature: class.fingerprint.signature(),
                venue_phase_code: class.venue_phase.ordinal(),
                meta_category_id: class.meta_category_id,
                discovery_lane_code: class.discovery_lane.ordinal(),
                n_matched: stats.n_matched,
                median_net_lamports: stats.median_net_lamports,
                win_rate_bp: stats.win_rate_bp,
                median_hold_ns: stats.median_hold_ns,
                nearest_distance: stats.nearest_distance,
                nearest_episode_id: stats.nearest_episode_id,
            });
        }
        // Strongest signal first: largest sample, then largest median net, then the
        // signature — a total order, so the readout is deterministic (§22).
        out.sort_by(|a, b| {
            b.n_matched
                .cmp(&a.n_matched)
                .then(b.median_net_lamports.cmp(&a.median_net_lamports))
                .then(a.signature.cmp(&b.signature))
        });
        out.truncate(BRAIN_REPORT_CLASS_CAP);
        self.classes = out;
    }

    /// The cached LAW B2 setup-class readout.
    #[must_use]
    pub fn setup_classes(&self) -> Vec<BrainSetupClass> {
        self.classes.clone()
    }

    /// LAW B6: every traded setup class with its FULL conditioned verdict,
    /// refusals included.
    ///
    /// Same conditioning key as [`BrainPlane::refresh_reflection`] (§100 venue
    /// phase × §21.4 meta × §29.9 discovery lane), same recall params — the only
    /// difference is that a class recall declines to speak about is RETAINED,
    /// carrying its refusal reason instead of being silently dropped.
    ///
    /// Ordered by `(venue_phase, discovery_lane, meta_category, signature)` — a
    /// total order over the conditioning key itself, so the row order is a pure
    /// function of WHAT was traded and never of when it was traded or of a
    /// statistic that may be absent. Bounded by [`BRAIN_TRADED_CLASS_CAP`]
    /// upstream (`on_admit` refuses past the cap).
    #[must_use]
    pub fn conditioned_classes(&self) -> Vec<ConditionedClass> {
        let mut out: Vec<ConditionedClass> = self
            .traded
            .iter()
            .map(|class| {
                let filter = RecallFilter::for_phase(class.venue_phase)
                    .with_meta_category(class.meta_category_id)
                    .with_discovery_lane(class.discovery_lane);
                ConditionedClass {
                    signature: class.fingerprint.signature(),
                    venue_phase: class.venue_phase,
                    meta_category_id: class.meta_category_id,
                    discovery_lane: class.discovery_lane,
                    verdict: self.index().recall_conditioned(
                        &class.fingerprint,
                        &self.params,
                        &filter,
                    ),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.venue_phase
                .ordinal()
                .cmp(&b.venue_phase.ordinal())
                .then(a.discovery_lane.ordinal().cmp(&b.discovery_lane.ordinal()))
                .then(a.meta_category_id.cmp(&b.meta_category_id))
                .then(a.signature.cmp(&b.signature))
        });
        out
    }

    /// LAW B6: `(episodes held in the index, of which were admitted)`.
    ///
    /// The index also retains rejected/observational episodes when a caller pushes
    /// them; only the admitted ones carry realized P&L, and only they may back an
    /// estimate (§46). The export reports both so a consumer can see the ratio
    /// rather than infer it.
    #[must_use]
    pub fn episode_counts(&self) -> (u64, u64) {
        let mut total = 0u64;
        let mut admitted = 0u64;
        for e in self.index().iter_oldest_first() {
            total = total.saturating_add(1);
            if e.outcome().was_admitted {
                admitted = admitted.saturating_add(1);
            }
        }
        (total, admitted)
    }

    /// LAW B2: **"does this match the current meta, or a past one?"** — the
    /// match-past-meta seam over the recorded lifecycle timeline.
    ///
    /// Kept as an inspection API rather than a `Report` field on purpose: it is a
    /// QUERY (it needs a snapshot to match against), and its answer is a research
    /// input, never a sizing or gating one. Fail-closed below
    /// [`MetaMatchParams::min_snapshots`], like every other estimator here.
    #[must_use]
    pub fn match_past_meta(
        &self,
        query: &MetaSnapshot,
        params: &MetaMatchParams,
    ) -> Vec<PastMetaMatch> {
        self.meta.match_past_meta(query, params)
    }

    /// LAW B2: the current lifecycle state of each tracked meta, bounded and
    /// ordered by category id (deterministic).
    #[must_use]
    pub fn meta_state(&self) -> Vec<BrainMetaState> {
        let mut out: Vec<BrainMetaState> = self
            .meta
            .current_metas()
            .into_iter()
            .map(|s| BrainMetaState {
                meta_category_id: s.meta_category_id,
                saturation_code: s.saturation.ordinal(),
                aggregate_net_lamports: s.aggregate_net_lamports,
                participant_breadth: s.participant_breadth,
                episode_count: s.episode_count,
                info_time_ns: s.info_time_ns,
            })
            .collect();
        out.sort_by_key(|m| m.meta_category_id);
        out.truncate(BRAIN_REPORT_META_CAP);
        out
    }

    /// LAW B2: measured author track records — "does this caller actually earn?".
    ///
    /// Fail-closed: an author below the sample floor comes back `Unknown` from the
    /// brain and is simply omitted here, so the readout never presents a two-trade
    /// record as evidence. Ordered by realized median descending, then by author id.
    #[must_use]
    pub fn author_records(&self, min_sample: u32) -> Vec<BrainAuthorRecord> {
        let mut authors: Vec<u64> = self
            .social
            .iter_markouts_oldest_first()
            .map(|m| m.author_id)
            .collect();
        authors.sort_unstable();
        authors.dedup();
        let mut out: Vec<BrainAuthorRecord> = Vec::new();
        for id in authors {
            if let AuthorTrackRecord::Known(stats) = self.social.author_track_record(id, min_sample)
            {
                out.push(BrainAuthorRecord {
                    author_id: stats.author_id,
                    n_markouts: stats.n_markouts,
                    median_net_lamports: stats.median_net_lamports,
                    win_rate_bp: stats.win_rate_bp,
                });
            }
        }
        out.sort_by(|a, b| {
            b.median_net_lamports
                .cmp(&a.median_net_lamports)
                .then(a.author_id.cmp(&b.author_id))
        });
        out.truncate(BRAIN_REPORT_AUTHOR_CAP);
        out
    }
}

// ---------------------------------------------------------------------------
// Pure mapping helpers (used by the engine's entry-time capture)
// ---------------------------------------------------------------------------

/// §21.6 LAW B1: classify the range state from the current and prior bar ranges.
/// `prior == 0` is UNKNOWN and maps to the neutral bucket (§6.4).
#[must_use]
pub fn range_state_of(
    range_bps: u64,
    prior_range_bps: u64,
) -> pump_quant_brain::fingerprint::RangeState {
    use pump_quant_brain::fingerprint::RangeState;
    if prior_range_bps == 0 {
        return RangeState::Normal;
    }
    let ratio = range_bps.saturating_mul(10_000) / prior_range_bps;
    if ratio >= BRAIN_RANGE_EXPAND_BP {
        RangeState::Expanded
    } else if ratio <= BRAIN_RANGE_COMPRESS_BP {
        RangeState::Compressed
    } else {
        RangeState::Normal
    }
}

/// §21.7 LAW B1: classify the volume-burst lifecycle from a short recent trade
/// count against a longer baseline window of `mult`× the length. No baseline
/// (`long == 0`) is UNKNOWN and maps to `None` (§6.4).
#[must_use]
pub fn burst_phase_of(
    short_trades: u32,
    long_trades: u32,
    mult: u64,
) -> pump_quant_brain::fingerprint::BurstPhase {
    use pump_quant_brain::fingerprint::BurstPhase;
    if long_trades == 0 || mult == 0 {
        return BurstPhase::None;
    }
    // Rate ratio in bps: (short / 1) vs (long / mult).
    let ratio = u64::from(short_trades)
        .saturating_mul(mult)
        .saturating_mul(10_000)
        / u64::from(long_trades);
    if ratio >= BRAIN_BURST_CLIMAX_BP {
        BurstPhase::Climax
    } else if ratio >= BRAIN_BURST_ONSET_BP {
        BurstPhase::Onset
    } else if ratio <= BRAIN_BURST_EXHAUST_BP {
        BurstPhase::Exhaustion
    } else {
        BurstPhase::None
    }
}

/// LAW B1: map the app's exit reason onto the brain's exit taxonomy.
///
/// The two vocabularies are close but not identical; this is the single, explicit
/// crosswalk so two call sites can never disagree. `RugPrecursor` maps to
/// `LiquidityFail` (the exit fired because the ability to unwind was collapsing),
/// and `ThesisInvalidation` to `StructureBreak` (the thesis broke before a stop).
#[must_use]
pub const fn exit_reason_of(reason: crate::position::ExitReason) -> BrainExit {
    use crate::position::ExitReason as AppExit;
    match reason {
        AppExit::RugPrecursor => BrainExit::LiquidityFail,
        AppExit::HardStop => BrainExit::StopLoss,
        AppExit::ThesisInvalidation => BrainExit::StructureBreak,
        AppExit::TakeProfitLadder | AppExit::IntoStrength => BrainExit::TakeProfit,
        AppExit::TrailingStop => BrainExit::TrailingStop,
        AppExit::TimeStop | AppExit::ForceClose => BrainExit::TimeStop,
        AppExit::CreatorDump => BrainExit::ManualKill,
    }
}

/// LAW B2: map the app's social platform taxonomy onto the brain's.
///
/// The app's capture lanes are finer-grained than the brain's five nominal slots;
/// the mapping is documented and stable, which is all a NOMINAL field requires.
/// TikTok and the general web are amplifier surfaces, so they land on
/// `Aggregator` alongside the aggregator lane itself.
#[must_use]
pub const fn platform_of(p: pump_quant_ingest::social_parse::SocialPlatform) -> Platform {
    use pump_quant_ingest::social_parse::SocialPlatform as S;
    match p {
        S::X => Platform::X,
        S::Telegram => Platform::Telegram,
        S::Discord => Platform::Discord,
        S::Twitch | S::Pump => Platform::Stream,
        S::TikTok | S::Web | S::Aggregator => Platform::Aggregator,
    }
}

/// LAW B1: map the app's discovery-lane taxonomy onto the brain's.
#[must_use]
pub const fn discovery_lane_of(lane: pump_quant_watchlist::candidate::DiscoveryLane) -> BrainLane {
    use pump_quant_watchlist::candidate::DiscoveryLane as D;
    match lane {
        D::OnchainCreation => BrainLane::NewMint,
        D::ActiveMarket => BrainLane::Rescan,
        D::NarrativeAttentionVelocity => BrainLane::Watchlist,
        D::SocialCaller | D::AlphaCall => BrainLane::SocialCall,
        D::WalletSmartMoney => BrainLane::WhaleFollow,
    }
}

/// LAW B1: map the app's 4-class narrative taxonomy onto the brain's 8 nominal
/// slots. Injective — which is the only property a NOMINAL encoding needs — but
/// deliberately not onto: the app has no Animal/Seasonal/Stream classifier yet, so
/// those slots stay unreachable rather than being faked.
#[must_use]
pub const fn narrative_class_of(
    class: Option<pump_quant_narrative::narrative::NarrativeClass>,
) -> pump_quant_brain::fingerprint::NarrativeClass {
    use pump_quant_brain::fingerprint::NarrativeClass as B;
    use pump_quant_narrative::narrative::NarrativeClass as N;
    match class {
        None => B::Unclassified,
        Some(N::Trend) => B::Derivative,
        Some(N::News) => B::Political,
        Some(N::Tech) => B::Tech,
        Some(N::Culture) => B::Celebrity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_brain::fingerprint::{BurstPhase, RangeState, SetupInputs};

    fn entry(inputs: &SetupInputs, mint_id: u64) -> BrainEntry {
        BrainEntry {
            fingerprint: SetupFingerprint::from_inputs(inputs),
            context: EpisodeContext {
                mint_id,
                venue_phase: inputs.venue_phase,
                meta_category_id: inputs.meta_category_id,
                discovery_lane: BrainLane::NewMint,
                info_time_ns: inputs.info_time_ns,
                slot: 0,
            },
        }
    }

    #[test]
    fn null_store_is_a_pure_in_memory_index() {
        let mut plane =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        let e = entry(&SetupInputs::default(), 1);
        plane.record_exit(&e, -1_000, 10, BrainExit::StopLoss, 0, 0);
        assert_eq!(plane.episodes_recorded(), 1);
        assert_eq!(plane.index().len(), 1);
    }

    #[test]
    fn unknown_recall_is_always_identity_even_when_armed() {
        let mut plane =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        let e = entry(&SetupInputs::default(), 1);
        // Empty index → Unknown → Identity, armed or not.
        for armed in [false, true] {
            assert_eq!(
                plane.size_verdict(
                    &e,
                    armed,
                    BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
                    BRAIN_VETO_WIN_RATE_BP_DEFAULT,
                    BRAIN_HAIRCUT_MULT_BP_DEFAULT
                ),
                BrainSizeVerdict::Identity
            );
        }
        // One short of the sample floor is still Identity.
        for _ in 0..(BRAIN_MIN_SAMPLE_DEFAULT - 1) {
            plane.record_exit(&e, -1_000_000, 10, BrainExit::StopLoss, 0, -500);
        }
        assert_eq!(
            plane.size_verdict(
                &e,
                true,
                BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
                BRAIN_VETO_WIN_RATE_BP_DEFAULT,
                BRAIN_HAIRCUT_MULT_BP_DEFAULT
            ),
            BrainSizeVerdict::Identity
        );
    }

    #[test]
    fn a_bleeding_class_is_vetoed_and_a_marginal_one_is_only_haircut() {
        let mut plane =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        let e = entry(&SetupInputs::default(), 1);
        // All-losses class: win rate 0 bp → veto.
        for _ in 0..BRAIN_MIN_SAMPLE_DEFAULT {
            plane.record_exit(&e, -1_000_000, 10, BrainExit::StopLoss, 0, -500);
        }
        assert_eq!(
            plane.size_verdict(
                &e,
                true,
                BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
                BRAIN_VETO_WIN_RATE_BP_DEFAULT,
                BRAIN_HAIRCUT_MULT_BP_DEFAULT
            ),
            BrainSizeVerdict::Veto
        );
        // Add wins until the win rate crosses out of the veto band but stays under
        // the haircut bar, with the median still negative.
        for _ in 0..2 {
            plane.record_exit(&e, 10_000, 10, BrainExit::TakeProfit, 100, 0);
        }
        // 2 wins / 10 decisive = 2_000 bp: above the 1_500 veto bar, at/below the
        // 3_500 haircut bar, median still negative.
        assert_eq!(
            plane.size_verdict(
                &e,
                true,
                BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
                BRAIN_VETO_WIN_RATE_BP_DEFAULT,
                BRAIN_HAIRCUT_MULT_BP_DEFAULT
            ),
            BrainSizeVerdict::Haircut(BRAIN_HAIRCUT_MULT_BP_DEFAULT)
        );
    }

    #[test]
    fn a_winning_class_is_never_sized_up() {
        let mut plane =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        let e = entry(&SetupInputs::default(), 1);
        for _ in 0..(BRAIN_MIN_SAMPLE_DEFAULT * 2) {
            plane.record_exit(&e, 5_000_000, 10, BrainExit::TakeProfit, 900, 0);
        }
        let v = plane.size_verdict(
            &e,
            true,
            BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
            BRAIN_VETO_WIN_RATE_BP_DEFAULT,
            BRAIN_HAIRCUT_MULT_BP_DEFAULT,
        );
        assert_eq!(v, BrainSizeVerdict::Identity);
        assert_eq!(v.mult_bp(), 10_000, "recall may never enlarge risk");
    }

    #[test]
    fn every_verdict_is_reduce_only() {
        assert_eq!(BrainSizeVerdict::Identity.mult_bp(), 10_000);
        assert!(BrainSizeVerdict::Haircut(9_999).mult_bp() <= 10_000);
        assert_eq!(BrainSizeVerdict::Veto.mult_bp(), 0);
    }

    #[test]
    fn range_and_burst_ladders_are_monotone_and_unknown_safe() {
        assert_eq!(range_state_of(100, 0), RangeState::Normal);
        assert_eq!(range_state_of(200, 100), RangeState::Expanded);
        assert_eq!(range_state_of(50, 100), RangeState::Compressed);
        assert_eq!(range_state_of(100, 100), RangeState::Normal);
        assert_eq!(burst_phase_of(10, 0, 4), BurstPhase::None);
        assert_eq!(burst_phase_of(10, 20, 4), BurstPhase::Climax);
        assert_eq!(burst_phase_of(4, 12, 4), BurstPhase::Onset);
        assert_eq!(burst_phase_of(1, 100, 4), BurstPhase::Exhaustion);
    }

    #[test]
    fn persistence_round_trips_through_the_mem_store() {
        let mut plane =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        let base = PathBuf::from("brain-test");
        plane
            .attach(AppBlobStore::Mem(MemBlobStore::new()), &base)
            .expect("attach");
        let e = entry(&SetupInputs::default(), 1);
        for _ in 0..BRAIN_MIN_SAMPLE_DEFAULT {
            plane.record_exit(&e, -1_000_000, 10, BrainExit::StopLoss, 0, -500);
        }
        plane.snapshot_now().expect("snapshot");
        let before = plane.recall(&e);
        let n_before = plane.index().len();
        let store = plane.detach();
        let mut restored =
            BrainPlane::new(BRAIN_MIN_SAMPLE_DEFAULT, BRAIN_RECALL_MAX_DISTANCE_DEFAULT);
        restored.attach(store, &base).expect("restore");
        assert_eq!(restored.index().len(), n_before);
        assert_eq!(restored.recall(&e), before);
    }
}
