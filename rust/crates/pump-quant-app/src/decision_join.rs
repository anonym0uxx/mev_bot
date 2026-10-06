//! Decision-time join: real producers -> `BundleInputs` -> the existing byte-parity renderer.
//!
//! This module owns NO market logic. Every number comes from an existing producer (`StateLedger`,
//! `enrich`, `FlowReducer`, `AnnotationState`, `CreatorHistory`); this is the cache that feeds them
//! from live `MarketTrade` prints and the single place a prompt is assembled, so the join cannot
//! combine unrelated or future state.
//!
//! WHAT IT WILL NOT DO (each is a named [`JoinRefusal`], counted, never defaulted):
//! * invent a clock, a trader, a fee, a compute-unit figure or a launch time;
//! * treat first observation as launch time (the history must START at the launch, within
//!   [`LAUNCH_TOLERANCE_MS`], or the mint is refused as `HistoryStartsAfterLaunch`);
//! * let a partial history read as a quiet market (any print that could not feed the flow reducer
//!   poisons that mint's flow block -> `FlowMetaMissing`);
//! * serve state from after the decision clock (`FutureStateInCache`).
//!
//! The trained prompt contract is unchanged: required-field refusals are the existing
//! `AssemblyRefusal`s, plus the corpus-eligibility gates in `StateLedger::eligibility`.

use std::collections::{BTreeMap, VecDeque};

use crate::bundle_assemble::py_round;
use pump_quant_market_state::flow_reducer::{FlowOutcome, FlowReducer, WINDOW_300_MS};
use pump_quant_proposal::bundle_gate::BundlePolicy;
use pump_quant_proposal::decision::{AmmState, CurveState};
use pump_quant_proposal::render_decision;
use pump_quant_proposal::system::{system_prompt, PromptFamily};
use pump_quant_proposal::PyNum;

use crate::bundle_assemble::{assemble, AssemblyRefusal, BundleInputs};
use crate::creator_history::CreatorHistory;
use crate::curve_annotation::{AmmAttribution, AmmObservation, AnnotationState, CurveObservation};
use crate::enrichment::{enrich, EnrichmentGap, EnrichmentTrade};
use crate::flow_feed::{flow_event_from_market_trade, flow_state_from_aggregates, zero_flow_state};
use crate::state_ledger::{ClockRefusal, StateLedger, StateTrade, VenueLabel};

/// How far after a mint's launch its first observed print may be and still count as "the history
/// starts at the launch". Measured on the corpus tape: launch -> first trade is p10/p50/p90 =
/// 0/0/3 ms (n=2530). A mint first seen later was already trading: its windows are partial.
pub const LAUNCH_TOLERANCE_MS: i64 = 5_000;
/// Per-mint enrichment ring bound (§99). `enrich` reads the WHOLE prefix, so a mint that
/// overflows this is refused rather than silently truncated.
pub const MAX_ENRICH_TRADES_PER_MINT: usize = 50_000;
/// How many recent upstream-dropped prints a mint remembers. A drop poisons the window for
/// [`WINDOW_300_MS`]; a ring larger than the number of drops that can fall inside one 300 s
/// window is never needed, and the bound keeps the record off the unbounded path (§99).
const FLOW_UPSTREAM_DROP_RING: usize = 64;

/// How many recent prints a duplicate check looks back over.
const DEDUPE_LOOKBACK: usize = 64;
/// How many recent event ids a per-mint replay check remembers. A redelivery older than this many
/// ACCEPTED events of the same mint is not detected here (the producer's own dedup is the first layer).
const DEDUPE_ID_LOOKBACK: usize = 512;

/// One live print, as the join needs it. Built from `AppEvent::MarketTrade` plus the venue the
/// provenance names (the event itself carries none).
/// MUST stay bit-identical to `pump_quant_junction::laserstream::wallet_entity_id` (this crate cannot depend on the
/// junction); pinned by a cross-crate test in the junction.
pub fn wallet_entity_of(pubkey: &[u8; 32]) -> u64 {
    let lo = u64::from_le_bytes(pubkey[..8].try_into().unwrap_or([0; 8]));
    let hi = u64::from_le_bytes(pubkey[24..32].try_into().unwrap_or([0; 8]));
    let mut z = lo.wrapping_add(hi);
    z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    let z = z.wrapping_mul(0xC2B9_5A82_79D4_CEA2);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    z.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[derive(Debug, Clone, Copy)]
pub struct TradeObs {
    pub mint: [u8; 32],
    pub price_fp: i128,
    pub quote_lamports: u64,
    pub signed_base: i64,
    pub buyer_entity: u64,
    pub trader: Option<[u8; 32]>,
    pub recv_unix_ms: Option<i64>,
    pub slot: Option<u64>,
    pub fee_lamports: Option<u64>,
    pub cu_consumed: Option<u64>,
    pub venue: VenueLabel,
    /// Exact event identity (see `AppEvent::MarketTrade::event_id`). When present it is the ONLY
    /// dedup key: two distinct events never collide and a replayed delivery always does, whatever
    /// their slot/trader/size/time/price. `None` falls back to the heuristic key.
    pub event_id: Option<u128>,
    /// Corpus-definition basis for the TRAINED windows (see `event::FeatureBasis`). With an `event_id` (the
    /// transaction-event producer) and `feature: None` the trade is outside the frozen corpus population: it is
    /// counted (`outside_corpus`) and kept out of the trained windows. `price_fp`/`quote_lamports`/`signed_base`
    /// above remain the executable reserve price and swap amounts and are not read by the trained windows when a
    /// basis is present.
    pub feature: Option<crate::event::FeatureBasis>,
}

/// What ingest did with a print. Every non-`Accepted` arm is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    Accepted,
    /// No receive clock: the causal windows cannot be keyed, so the print is not admitted.
    NoClock,
    /// `price_fp <= 0`: the instruction-half placeholder. The same trade arrives again as the
    /// priced reserve print (joined to the same trader/fee/CU), so admitting both would count
    /// every trade twice in the enrichment and flow windows.
    NoPrice,
    /// Earlier than the mint's newest print.
    OutOfOrder,
    /// Same (slot, trader, base leg, clock) as a recent print.
    Duplicate,
}

/// Why no prompt was produced. Stable labels via [`JoinRefusal::as_str`].
#[derive(Debug, Clone, PartialEq)]
pub enum JoinRefusal {
    NoMint,
    /// No launch/creator record: `creator_known` would be 0, a state the corpus never trained on.
    LaunchUnknown,
    /// The first observed print is later than the launch by more than the tolerance.
    HistoryStartsAfterLaunch {
        gap_ms: i64,
    },
    /// The cache holds a print newer than the decision clock.
    FutureStateInCache {
        newest_ms: i64,
    },
    State(ClockRefusal),
    /// A print could not name its trader, so holder concentration is not the corpus's number.
    EnrichmentIdentityMissing {
        prints: u64,
    },
    EnrichmentOverflow,
    Enrichment(EnrichmentGap),
    /// A print lacked fee / compute units / slot / trader, so the flow block is partial.
    FlowMetaMissing {
        prints: u64,
    },
    /// The reducer served aggregates with no fee-p90 or CU-p50 (never rendered `none` in training).
    FlowAggregatesIncomplete,
    /// A print was dropped by the feed derivation *before* the flow reducer could see it
    /// (a reserve delta the derivation refused), so this clock's 300 s flow window is
    /// missing a print. Refused by name — never served as complete or as a quiet market —
    /// until the drop ages out of the window.
    FlowUpstreamDrop {
        drop_ms: i64,
    },
    /// The mint's CUMULATIVE history is short a print the feed derivation dropped. A timer
    /// cannot repair this: a fresh reserve snapshot does NOT restore missing trade history, so
    /// the counters stay incomplete until a bounded replay/backfill from an authoritative
    /// capture reconstructs them, or an operator reconciles. Deliberately distinct from
    /// [`JoinRefusal::FlowUpstreamDrop`], which is the timer-bounded ROLLING window.
    FlowHistoryUnreconstructable {
        drop_ms: i64,
    },
    /// Continuity of the persisted missing-history record could not be established on startup
    /// (the record was unreadable or incompatible). Refused by name rather than assuming no gap
    /// occurred. Cleared only by a reconstruction receipt, never by a bare acknowledgement.
    HistoryContinuityUnknown,
    CurveAbsent(String),
    AmmAbsent(String),
    /// More than one pool was bound to the mint and the observation's pool is not the bound one.
    AmmPoolAmbiguous,
    Assembly(AssemblyRefusal),
    /// The pool depth or mark could not be priced, so the management prompt's cost line cannot be stated.
    DepthUnknown,
    /// A reserve component the management prompt prices against is present but older than the
    /// existing pricing budget. A recent TRADE does not refresh it: each dynamic component carries
    /// its own receipt time.
    ReserveStale {
        component: &'static str,
        staleness_ms: i64,
    },
}

impl JoinRefusal {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            JoinRefusal::NoMint => "join_no_mint",
            JoinRefusal::LaunchUnknown => "join_launch_unknown",
            JoinRefusal::HistoryStartsAfterLaunch { .. } => "join_history_starts_after_launch",
            JoinRefusal::FutureStateInCache { .. } => "join_future_state_in_cache",
            JoinRefusal::State(c) => c.as_str(),
            JoinRefusal::EnrichmentIdentityMissing { .. } => "join_enrichment_identity_missing",
            JoinRefusal::EnrichmentOverflow => "join_enrichment_overflow",
            JoinRefusal::Enrichment(_) => "join_enrichment_gap",
            JoinRefusal::FlowMetaMissing { .. } => "join_flow_meta_missing",
            JoinRefusal::FlowAggregatesIncomplete => "join_flow_aggregates_incomplete",
            JoinRefusal::FlowUpstreamDrop { .. } => "join_flow_upstream_drop",
            JoinRefusal::FlowHistoryUnreconstructable { .. } => {
                "join_flow_history_unreconstructable"
            }
            JoinRefusal::HistoryContinuityUnknown => "join_history_continuity_unknown",
            JoinRefusal::CurveAbsent(_) => "join_curve_absent",
            JoinRefusal::AmmAbsent(_) => "join_amm_absent",
            JoinRefusal::AmmPoolAmbiguous => "join_amm_pool_ambiguous",
            JoinRefusal::Assembly(a) => a.as_str(),
            JoinRefusal::DepthUnknown => "join_depth_unknown",
            JoinRefusal::ReserveStale {
                component: "curve", ..
            } => "join_curve_reserve_stale",
            JoinRefusal::ReserveStale { .. } => "join_amm_reserve_stale",
        }
    }
}

/// The immutable prompt snapshot a request is bound to. Nothing in it is re-read after creation.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptSnapshot {
    pub mint: [u8; 32],
    pub t_dec_ms: i64,
    pub system_prompt: String,
    pub user_prompt: String,
    pub venue: String,
    pub size_amm: bool,
    pub depth_lamports: Option<u64>,
    pub price_lamports_per_raw_token: f64,
    pub n_prior_trades: u64,
    /// FNV-1a of the user prompt: the request is bound to exactly this text.
    pub prompt_digest: u64,
    /// The cache's view of the mint when the snapshot was cut, for post-inference revalidation.
    pub marker: StateMarker,
}

/// Cheap fingerprint of one mint's cache state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateMarker {
    pub last_recv_ms: i64,
    pub n_accepted: u64,
}

/// Everything `prepare` read from the producers, shared by the entry and management snapshots.
/// Who the prepared state is for: each audience gates only on the history it actually renders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Audience {
    Entry,
    Management,
}

struct Prepared {
    state: crate::state_ledger::StateSnapshot,
    enriched: crate::enrichment::EnrichedSnapshot,
    flow: pump_quant_proposal::FlowState,
    view: crate::curve_annotation::ReserveView,
    dev: pump_quant_proposal::decision::DevHistoryDecision,
    venue: String,
    last_recv_ms: i64,
    n_accepted: u64,
}

/// The position-side inputs of one management prompt, in the TRAINED renderer's units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MgmtPositionInputs {
    /// Step index of this decision for the position (continues past the corpus's 16-step cap).
    pub step: i64,
    /// Entry price, lamports per raw token.
    pub entry_px: f64,
    /// Inventory in the corpus unit: raw tokens / 1e9, so `qty_scaled * mark_lamports` is SOL.
    pub qty_scaled: f64,
    /// Free cash, SOL.
    pub cash_sol: f64,
    /// Seconds held.
    pub held_s: f64,
    /// Causal max favourable excursion since the fill, bp.
    pub mfe_bp: f64,
    /// Causal max adverse excursion since the fill, bp.
    pub mae_bp: f64,
}

/// The immutable management prompt a request is bound to.
#[derive(Debug, Clone, PartialEq)]
pub struct MgmtSnapshot {
    pub mint: [u8; 32],
    pub t_dec_ms: i64,
    pub system_prompt: String,
    pub user_prompt: String,
    pub venue: String,
    pub size_amm: bool,
    pub mark_price_lamports_per_raw_token: f64,
    pub prompt_digest: u64,
    pub marker: StateMarker,
}

/// The corpus's mint label (base58). Hex is NOT it: the prompt carries the base58 address.
fn mint_label(mint: &[u8; 32]) -> String {
    pump_quant_ingest::base58::encode(mint)
}

#[derive(Debug, Default)]
struct MintCache {
    enrich: Vec<EnrichmentTrade>,
    first_seen_ms: i64,
    last_recv_ms: i64,
    n_accepted: u64,
    identity_missing: u64,
    flow_meta_missing: u64,
    /// Prints the feed derivation dropped before the reducer could see them (a refused reserve
    /// delta). See [`MissingObservation`] for the served features each one still blocks.
    flow_drops: VecDeque<MissingObservation>,
    enrich_overflow: bool,
    venue: VenueLabel,
    recent: VecDeque<(Option<u64>, [u8; 32], i64, i64)>,
    /// Exact event ids of the most recent id-carrying prints (see [`DEDUPE_ID_LOOKBACK`]).
    recent_ids: VecDeque<u128>,
}

/// Per-mint pool binding for the AMM plane.
#[derive(Debug, Clone, Default)]
struct PoolBinding {
    pool: String,
    conflicting: bool,
}

/// Ingest counters, so coverage is measured rather than assumed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestCounters {
    pub accepted: u64,
    pub no_clock: u64,
    pub no_price: u64,
    pub out_of_order: u64,
    pub duplicate: u64,
    /// Prints the feed derivation dropped before the reducer could see them.
    pub flow_upstream_drops: u64,
    /// Producer-identified (event_id) trades with NO corpus basis: admitted for discovery/state, excluded from the trained windows.
    pub outside_corpus: u64,
}

/// A low-frequency health view of upstream-dropped prints, so an operator can tell an
/// honestly QUIET market apart from one whose data is INCOMPLETE. Computed only when a
/// status writer asks; never on the hot path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlowDropSummary {
    /// Cumulative prints the feed derivation dropped since the cache was created.
    pub drops_total: u64,
    /// Mints whose 300 s flow window contains a drop as of the query clock — i.e. mints
    /// whose readiness is currently refused by [`JoinRefusal::FlowUpstreamDrop`].
    pub mints_incomplete_now: u64,
    /// Mints carrying an UNRECONCILED drop whose cumulative history is still short — refused by
    /// [`JoinRefusal::FlowHistoryUnreconstructable`] until a replay/backfill or reconciliation.
    pub mints_history_unreconstructed: u64,
}

/// One print the feed derivation refused BEFORE it could reach the flow reducer or the ledger
/// (an out-of-range / self-inconsistent reserve delta).
///
/// DEPENDENCY TRACE — which served features still depend on it, and how each recovers:
/// * **ROLLING 300 s flow block** (net flow, entrants, sniper/uniform/coentry shares, p90 fee,
///   p50 CU, and the ledger's trailing `ret_*` windows): bounded by the print's own 300 s
///   window — complete again at `drop_ms + WINDOW_300_MS`.
/// * **CUMULATIVE ledger state** (`n_prior_trades`, buy/sell counts, `unique_traders`, volumes,
///   `top1`/`top5` share, buyer/seller ratio, position `age`): the print never entered the tape,
///   so NO timer restores it and a fresh reserve snapshot does not either. Requires a bounded
///   replay/backfill from an authoritative capture or an explicit reconciliation.
/// * **Wallet-derived flow features** (`fresh_wallet_share` reads each buyer's rolling
///   first-activity against `fresh_ms` = 24 h; `smart_*`/coentry read cumulative wallet state):
///   the dropped print's trader is UNKNOWN — the derivation refused before resolving it — so
///   these cannot be attributed to a wallet. They are REPORTED (see the drop summary), not
///   gated on unrelated mints, because a blanket freeze would be a policy change.
/// * A configured lookback (e.g. `lookback_ms` = 7 d, `flow_lookback_d`) is a LOOKBACK/eviction
///   horizon; it is NOT evidence that any given feature depends on this print for 7 d.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MissingDeps {
    /// Rolling 300 s flow block + ledger `ret_*`: bounded by the print's own window.
    /// Needed by ENTRY and MANAGEMENT.
    pub rolling_300s: bool,
    /// Cumulative LEDGER counters rendered only in the ENTRY prompt (`n_prior_trades`, buy/sell
    /// counts, volumes, top1/top5, buyer/seller ratio). NOT read by the management prompt, and
    /// NOT part of the engine's reconciled position state (inventory, cost basis, held time).
    pub ledger_cumulative: bool,
    /// Cumulative HOLDER state (`holders_at_t`, `top1_float_share`, `holder_hhi`, bundle /
    /// round-trip wallets, wash ratio) computed from the mint's whole trade list. Rendered by
    /// ENTRY and by MANAGEMENT.
    pub holder_enrichment: bool,
    /// Wallet-derived flow features (`fresh_wallet_share`, `smart_*`, coentry). The refused
    /// derivation never resolved a trader, so attribution is UNKNOWN. These read GLOBAL wallet
    /// state (first-activity, co-entry graph), so other mints MAY carry a one-event error; that
    /// is REPORTED (health counter), neither assumed zero nor used to freeze unrelated mints.
    pub wallet_derived_uncertain: bool,
}

impl MissingDeps {
    /// Everything a refused-but-coherent reserve move could have touched.
    #[must_use]
    pub fn all_market_history() -> Self {
        Self {
            rolling_300s: true,
            ledger_cumulative: true,
            holder_enrichment: true,
            wallet_derived_uncertain: true,
        }
    }
    fn union(self, o: Self) -> Self {
        Self {
            rolling_300s: self.rolling_300s || o.rolling_300s,
            ledger_cumulative: self.ledger_cumulative || o.ledger_cumulative,
            holder_enrichment: self.holder_enrichment || o.holder_enrichment,
            wallet_derived_uncertain: self.wallet_derived_uncertain || o.wallet_derived_uncertain,
        }
    }
    /// Does a CUMULATIVE gap with these deps make the ENTRY prompt incomplete?
    #[must_use]
    pub fn blocks_entry(&self) -> bool {
        self.ledger_cumulative || self.holder_enrichment
    }
    /// Does it make the MANAGEMENT prompt incomplete? (Position inventory/cost/age are engine
    /// state and are never inputs here.)
    #[must_use]
    pub fn blocks_management(&self) -> bool {
        self.holder_enrichment
    }
}

/// What the producer actually knows about the refused observation. A failed reserve-delta
/// inference alone never proves a trade was lost, so `Confirmed` is deliberately NOT producible
/// from the reserve path: it would need an independent transaction record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingKind {
    /// Both reserves moved by representable amounts but the derivation refused the pair
    /// (same-sign move, degenerate token side): a swap-sized move we could not turn into a print.
    PossibleTrade,
    /// A reserve delta outside `i64`: the account decoded to non-physical reserves. No trade is
    /// shown to exist OR to be absent. Explicitly UNKNOWN; gated fail-closed like `PossibleTrade`
    /// (relaxing that is an operator decision, not made here).
    InvalidObservation,
}

impl MissingKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MissingKind::PossibleTrade => "possible_trade",
            MissingKind::InvalidObservation => "invalid_observation_unknown",
        }
    }
}

/// Evidence that missing history was actually RECONSTRUCTED from an authoritative source and
/// installed with a coverage boundary. A receipt is the ONLY way a cumulative gap is resolved:
/// there is deliberately NO API that clears the gap without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructionReceipt {
    /// Where the reconstructed events came from (capture path / replay run id). Must be non-empty.
    pub provenance: String,
    /// The reconstructed window must COVER the drop instant.
    pub coverage_from_ms: i64,
    pub coverage_to_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingObservation {
    /// Receive instant of the (earliest folded) refused observation.
    pub drop_ms: i64,
    /// What the producer knows: possible trade vs invalid (unknown) observation.
    pub kind: MissingKind,
    /// How many refused observations this record stands for (compaction folds, never drops).
    pub count: u32,
    /// Source/event identity where the producer has one (empty when it does not).
    pub source_id: String,
    pub deps: MissingDeps,
    /// Installed reconstruction receipt; `None` until the history is genuinely repaired.
    pub receipt: Option<ReconstructionReceipt>,
}

/// Per-mint readiness for the low-frequency status writer: what is unavailable, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingHistoryStatus {
    pub mint: [u8; 32],
    pub drop_ms: i64,
    pub source_id: String,
    pub deps: MissingDeps,
    pub kind: MissingKind,
    pub count: u32,
    pub entry_unavailable: bool,
    pub management_unavailable: bool,
    /// `rolling_pending` | `reconstruction_unsupported` | `reconstructed` | `continuity_unknown`
    pub recovery: &'static str,
}

/// Why a reconstruction was refused. Never silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileRefusal {
    NoGap,
    AlreadyReconstructed,
    EmptyProvenance,
    CoverageDoesNotSpanTheGap,
    UnorderedCoverage,
    /// Production recovery requires an INSTALLER that reconstructs aggregates with provenance and
    /// a coverage boundary. Metadata alone cannot unlock inference over unchanged incomplete
    /// state, so this is returned even for a syntactically valid, covering receipt.
    ReconstructionUnsupported,
}

/// Why restoring persisted missing-history state was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreRefusal {
    /// The record could not be read/parsed: continuity is NOT assumed.
    Unreadable,
    /// The record is structurally incompatible with this build: continuity is NOT assumed.
    Incompatible,
}

/// The decision-time cache. One owner (the engine); no interior mutability.
pub struct DecisionCache {
    ledger: StateLedger,
    flow: FlowReducer,
    annotation: AnnotationState,
    creators: CreatorHistory,
    launch_ms: BTreeMap<[u8; 32], i64>,
    mints: BTreeMap<[u8; 32], MintCache>,
    pools: BTreeMap<[u8; 32], PoolBinding>,
    policy: BundlePolicy,
    counters: IngestCounters,
    /// Set when startup continuity could NOT be established (unreadable or incompatible persisted
    /// record). Refuses every prompt by [`JoinRefusal::HistoryContinuityUnknown`] until a
    /// reconstruction receipt clears it — never by assuming no gap occurred.
    history_continuity_unknown: bool,
    /// Bumped on every change to the unresolved-gap set, so the persister can skip unchanged ticks
    /// with one integer compare.
    missing_rev: u64,
}

impl Default for DecisionCache {
    fn default() -> Self {
        Self::new()
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Bounded compaction that NEVER discards an unresolved cumulative gap: when the ring is full,
/// an unresolved record being evicted folds its earliest instant into the incoming record, so the
/// gap survives compaction as a single earliest observation instead of vanishing.
fn push_missing_bounded(ring: &mut VecDeque<MissingObservation>, mut new: MissingObservation) {
    while ring.len() >= FLOW_UPSTREAM_DROP_RING {
        let Some(old) = ring.pop_front() else { break };
        if old.receipt.is_none() {
            // Fold, never drop: earliest instant, UNION of dependencies, summed count, the more
            // conservative kind, and a surviving source id.
            new.drop_ms = new.drop_ms.min(old.drop_ms);
            new.deps = new.deps.union(old.deps);
            new.count = new.count.saturating_add(old.count);
            if old.kind == MissingKind::PossibleTrade {
                new.kind = MissingKind::PossibleTrade;
            }
            if new.source_id.is_empty() {
                new.source_id = old.source_id;
            }
        }
    }
    ring.push_back(new);
}

impl DecisionCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ledger: StateLedger::new(),
            flow: FlowReducer::new(),
            annotation: AnnotationState::new(),
            creators: CreatorHistory::new(),
            launch_ms: BTreeMap::new(),
            mints: BTreeMap::new(),
            pools: BTreeMap::new(),
            policy: BundlePolicy::trained_only(),
            counters: IngestCounters::default(),
            missing_rev: 0,
            history_continuity_unknown: false,
        }
    }

    #[must_use]
    pub fn counters(&self) -> IngestCounters {
        self.counters
    }

    /// Low-frequency health view of upstream-dropped prints at the decision clock
    /// `t_dec_ms`: the cumulative drop count plus how many mints' 300 s flow windows are
    /// currently incomplete (would be refused by [`JoinRefusal::FlowUpstreamDrop`]). A
    /// bounded scan of the per-mint rings, so it must NOT be called per tick — only by a
    /// status/reporting writer.
    #[must_use]
    pub fn flow_drop_summary(&self, t_dec_ms: i64) -> FlowDropSummary {
        let mints_incomplete_now = self
            .mints
            .values()
            .filter(|mc| {
                mc.flow_drops
                    .iter()
                    .any(|m| m.drop_ms >= t_dec_ms - WINDOW_300_MS && m.drop_ms < t_dec_ms)
            })
            .count() as u64;
        let mints_history_unreconstructed = self
            .mints
            .values()
            .filter(|mc| mc.flow_drops.iter().any(|m| m.receipt.is_none()))
            .count() as u64;
        FlowDropSummary {
            drops_total: self.counters.flow_upstream_drops,
            mints_incomplete_now,
            mints_history_unreconstructed,
        }
    }

    /// Record a launch (creator + launch time). `launch_unix_ms` is the LAUNCH event's receive
    /// time -- never the first trade's. Returns false when the creator history refused it.
    pub fn observe_launch(
        &mut self,
        mint: [u8; 32],
        creator: [u8; 32],
        launch_unix_ms: i64,
    ) -> bool {
        self.flow.track_mint(mint);
        self.flow.set_creator(mint, creator);
        self.launch_ms.insert(mint, launch_unix_ms);
        let creator_id =
            pump_quant_wallet_graph::tracked_wallet_matcher::wallet_entity_id(&creator);
        self.creators.observe(mint, creator_id, launch_unix_ms)
    }

    pub fn observe_curve(&mut self, mint: [u8; 32], obs: CurveObservation) -> bool {
        self.annotation.observe_curve(mint, obs)
    }

    /// Bind the AMM pool a mint trades on. A second, different pool marks the binding
    /// conflicting: the plane then refuses by name instead of choosing one.
    pub fn bind_pool(&mut self, mint: [u8; 32], pool: &str) {
        let b = self.pools.entry(mint).or_default();
        if b.pool.is_empty() {
            b.pool = pool.to_string();
        } else if b.pool != pool {
            b.conflicting = true;
        }
    }

    pub fn observe_amm(
        &mut self,
        mint: [u8; 32],
        obs: AmmObservation,
        attribution: AmmAttribution,
    ) -> bool {
        self.annotation.set_attribution(mint, attribution);
        self.annotation.observe_amm(mint, obs)
    }

    /// Feed one print. Nothing is defaulted: a print missing a clock is refused; one missing
    /// trader/fee/CU/slot still feeds the price state but poisons that mint's flow/enrichment
    /// blocks so the gap is visible at decision time.
    pub fn observe_trade(&mut self, t: &TradeObs) -> Ingest {
        let Some(recv) = t.recv_unix_ms else {
            self.counters.no_clock += 1;
            return Ingest::NoClock;
        };
        if t.price_fp <= 0 {
            self.counters.no_price += 1;
            return Ingest::NoPrice;
        }
        let mc = self.mints.entry(t.mint).or_default();
        if mc.n_accepted > 0 && recv < mc.last_recv_ms {
            self.counters.out_of_order += 1;
            return Ingest::OutOfOrder;
        }
        // Identity, not price. A print carrying an exact `event_id` (transaction-event producer) is
        // deduplicated ONLY on that id: two distinct events that share slot, trader, size, time AND price
        // both survive, and a repeated delivery of one event is always dropped. Prints without an id
        // (legacy/derived) keep the original heuristic key -- price is NOT part of it.
        match t.event_id {
            Some(id) => {
                if mc.recent_ids.contains(&id) {
                    self.counters.duplicate += 1;
                    return Ingest::Duplicate;
                }
                if mc.recent_ids.len() >= DEDUPE_ID_LOOKBACK {
                    mc.recent_ids.pop_front();
                }
                mc.recent_ids.push_back(id);
            }
            None => {
                let key = (t.slot, t.trader.unwrap_or([0u8; 32]), t.signed_base, recv);
                if mc.recent.iter().any(|k| *k == key) {
                    self.counters.duplicate += 1;
                    return Ingest::Duplicate;
                }
                if mc.recent.len() >= DEDUPE_LOOKBACK {
                    mc.recent.pop_front();
                }
                mc.recent.push_back(key);
            }
        }
        if mc.n_accepted == 0 {
            mc.first_seen_ms = recv;
        }
        mc.last_recv_ms = recv;
        mc.venue = t.venue;
        mc.n_accepted += 1;
        self.counters.accepted += 1;

        // TRAINED-WINDOW inputs. With a corpus basis these are the corpus's own quantities (trader native+WSOL
        // delta, trader token delta, resolved trader; price = |sol|/|tokens| as `build_states_v2` computes it).
        // Without a basis: a legacy producer (no event_id) keeps its historical inputs unchanged; a
        // transaction-event trade is OUTSIDE the corpus population and is kept out of the trained windows.
        let (w_price, w_quote, w_base, w_trader, w_entity, w_ok) = match (t.feature, t.event_id) {
            (Some(f), _) => {
                let sol = f.sol_lamports.unsigned_abs();
                let tok = i128::from(f.tokens_raw.unsigned_abs());
                let px = if tok > 0 {
                    i128::from(sol).saturating_mul(1_000_000_000) / tok
                } else {
                    0
                };
                (
                    px,
                    sol,
                    f.tokens_raw,
                    Some(f.trader),
                    wallet_entity_of(&f.trader),
                    true,
                )
            }
            (None, Some(_)) => {
                self.counters.outside_corpus += 1;
                (0, 0, 0, None, 0, false)
            }
            (None, None) => (
                t.price_fp,
                t.quote_lamports,
                t.signed_base,
                t.trader,
                t.buyer_entity,
                true,
            ),
        };
        if !w_ok {
            return Ingest::Accepted;
        }

        let st = match t.feature {
            Some(f) => {
                StateTrade::from_corpus_basis(recv, f.sol_lamports, f.tokens_raw, w_entity, t.venue)
            }
            None => StateTrade::from_market_trade(
                recv,
                w_price,
                w_quote,
                w_base,
                w_entity,
                t.venue,
                Some(w_base),
            ),
        };
        if let Some(st) = st {
            self.ledger.on_trade(&t.mint, st);
        }

        // Enrichment (holders / bundles): needs the wallet and both legs.
        match w_trader {
            Some(w) if w_base != 0 => {
                if mc.enrich.len() >= MAX_ENRICH_TRADES_PER_MINT {
                    mc.enrich_overflow = true;
                } else {
                    mc.enrich.push(EnrichmentTrade {
                        recv_unix_ms: recv,
                        trader: w,
                        tokens_raw: i128::from(w_base),
                        sol_lamports: w_quote,
                        slot: t.slot,
                    });
                }
            }
            _ => mc.identity_missing += 1,
        }

        // Flow: wallet + slot + total fee are required by the reducer's event; CU is carried as
        // Option. A print lacking any of them cannot enter the window, and that is counted.
        let flow_ev = match (w_trader, t.slot, t.fee_lamports) {
            (Some(w), Some(slot), Some(fee)) => flow_event_from_market_trade(
                &t.mint,
                slot,
                Some(recv),
                w_quote,
                w_base,
                w,
                fee,
                t.cu_consumed,
            ),
            _ => None,
        };
        match flow_ev {
            Some(e) if t.cu_consumed.is_some() => self.flow.on_event(&e),
            _ => mc.flow_meta_missing += 1,
        }
        Ingest::Accepted
    }

    /// Track `mint` in the flow reducer without a launch record. Measurement/replay only: the
    /// serving path tracks via `observe_launch`, and a mint with no launch is refused upstream of
    /// flow (`LaunchUnknown`) -- this does not weaken that.
    pub fn track_flow_mint_for_measurement(&mut self, mint: [u8; 32]) {
        self.flow.track_mint(mint);
    }

    /// Read-only view of the flow reducer's aggregates for `mint` at `t_dec_ms` (measurement and
    /// tests; the serving path goes through `snapshot`, which also applies every refusal).
    #[must_use]
    pub fn flow_aggregates(
        &self,
        mint: &[u8; 32],
        t_dec_ms: i64,
    ) -> pump_quant_market_state::flow_reducer::FlowOutcome {
        self.flow.serve(mint, t_dec_ms)
    }

    /// Record that the feed derivation dropped a print for `mint` at `drop_unix_ms` — a reserve
    /// delta the derivation refused before it could reach the flow reducer or the ledger. Such a
    /// print is invisible to every received-print check, so the cache remembers the instant and
    /// refuses the affected prompt by dependency class (see [`MissingObservation`]): the rolling
    /// 300 s window for [`WINDOW_300_MS`], then the cumulative history until it is reconstructed
    /// or reconciled. Never served as complete or as a quietly idle market.
    pub fn note_flow_upstream_drop(&mut self, mint: [u8; 32], drop_unix_ms: i64) {
        self.note_missing_observation(
            mint,
            drop_unix_ms,
            MissingKind::PossibleTrade,
            String::new(),
        );
    }

    /// As [`Self::note_flow_upstream_drop`], carrying the source identity where the producer has
    /// one (e.g. the slot) and the producer's classification.
    pub fn note_missing_observation(
        &mut self,
        mint: [u8; 32],
        drop_unix_ms: i64,
        kind: MissingKind,
        source_id: String,
    ) {
        let mc = self.mints.entry(mint).or_default();
        push_missing_bounded(
            &mut mc.flow_drops,
            MissingObservation {
                drop_ms: drop_unix_ms,
                kind,
                count: 1,
                source_id,
                // The refused derivation never resolved a trader or a trade, so every history the
                // missing print could have entered is carried; each audience then gates on its
                // own subset (see `MissingDeps::blocks_entry` / `blocks_management`).
                deps: MissingDeps::all_market_history(),
                receipt: None,
            },
        );
        self.counters.flow_upstream_drops += 1;
        self.missing_rev += 1;
    }

    /// Resolve the CUMULATIVE gap for `mint` ONLY by installing a validated reconstruction
    /// receipt whose coverage spans the drop. There is deliberately NO API that clears the gap
    /// without one: an operator may INITIATE reconstruction, but acknowledgement alone is not
    /// evidence. When no authoritative source exists, the gap is simply never resolved.
    pub fn reconcile_flow_history(
        &mut self,
        mint: &[u8; 32],
        receipt: &ReconstructionReceipt,
    ) -> Result<(), ReconcileRefusal> {
        if receipt.provenance.trim().is_empty() {
            return Err(ReconcileRefusal::EmptyProvenance);
        }
        if receipt.coverage_from_ms > receipt.coverage_to_ms {
            return Err(ReconcileRefusal::UnorderedCoverage);
        }
        let Some(mc) = self.mints.get(mint) else {
            return Err(ReconcileRefusal::NoGap);
        };
        let Some(m) = mc.flow_drops.iter().find(|m| m.receipt.is_none()) else {
            return Err(ReconcileRefusal::AlreadyReconstructed);
        };
        if !(receipt.coverage_from_ms <= m.drop_ms && m.drop_ms <= receipt.coverage_to_ms) {
            return Err(ReconcileRefusal::CoverageDoesNotSpanTheGap);
        }
        // Provenance and coverage are NECESSARY, not SUFFICIENT. While no reconstruction installer
        // exists, no aggregates are installed, so inference over unchanged incomplete state must
        // NOT be unlocked. Refuse.
        let _ = m;
        Err(ReconcileRefusal::ReconstructionUnsupported)
    }

    /// TEST-ONLY fixture restoration: installs a receipt WITHOUT reconstructing any aggregates, so
    /// unit tests can exercise how the gate opens when reconstruction genuinely exists. Compiled
    /// only under `cfg(test)`; never reachable from production code, and deliberately a different
    /// name from [`Self::reconcile_flow_history`].
    #[cfg(test)]
    pub(crate) fn install_reconstructed_fixture(
        &mut self,
        mint: &[u8; 32],
        receipt: &ReconstructionReceipt,
    ) -> Result<(), ReconcileRefusal> {
        if receipt.provenance.trim().is_empty() {
            return Err(ReconcileRefusal::EmptyProvenance);
        }
        if receipt.coverage_from_ms > receipt.coverage_to_ms {
            return Err(ReconcileRefusal::UnorderedCoverage);
        }
        let Some(mc) = self.mints.get_mut(mint) else {
            return Err(ReconcileRefusal::NoGap);
        };
        let Some(m) = mc.flow_drops.iter_mut().find(|m| m.receipt.is_none()) else {
            return Err(ReconcileRefusal::AlreadyReconstructed);
        };
        if !(receipt.coverage_from_ms <= m.drop_ms && m.drop_ms <= receipt.coverage_to_ms) {
            return Err(ReconcileRefusal::CoverageDoesNotSpanTheGap);
        }
        m.receipt = Some(receipt.clone());
        Ok(())
    }

    /// Per-mint readiness for the status writer (never on the hot path).
    #[must_use]
    pub fn missing_history_status(&self, mint: &[u8; 32]) -> Option<MissingHistoryStatus> {
        let m = self
            .mints
            .get(mint)?
            .flow_drops
            .iter()
            .find(|m| m.receipt.is_none())?;
        Some(MissingHistoryStatus {
            mint: *mint,
            drop_ms: m.drop_ms,
            source_id: m.source_id.clone(),
            deps: m.deps,
            kind: m.kind,
            count: m.count,
            entry_unavailable: m.deps.blocks_entry(),
            management_unavailable: m.deps.blocks_management(),
            recovery: "reconstruction_unsupported",
        })
    }

    /// Every mint with an UNRESOLVED missing observation (for persistence and reporting).
    #[must_use]
    pub fn missing_history_records(&self) -> Vec<([u8; 32], MissingObservation)> {
        let mut out = Vec::new();
        for (mint, mc) in &self.mints {
            for m in mc.flow_drops.iter().filter(|m| m.receipt.is_none()) {
                out.push((*mint, m.clone()));
            }
        }
        out
    }

    /// Restore persisted missing-history state BEFORE entry/management inference resumes.
    /// `integrity_ok` is false when the record could not be read or is incompatible with this
    /// build: the cache then raises the CONSERVATIVE named refusal
    /// [`JoinRefusal::HistoryContinuityUnknown`] rather than assuming no gap occurred.
    pub fn restore_missing_history(
        &mut self,
        records: &[([u8; 32], MissingObservation)],
        integrity_ok: bool,
    ) -> Result<(), RestoreRefusal> {
        if !integrity_ok {
            self.history_continuity_unknown = true;
            return Err(RestoreRefusal::Unreadable);
        }
        for (mint, m) in records {
            let mc = self.mints.entry(*mint).or_default();
            push_missing_bounded(&mut mc.flow_drops, m.clone());
        }
        self.missing_rev += 1;
        Ok(())
    }

    /// Clear a continuity failure ONLY with evidence: a reconstruction receipt. A bare operator
    /// acknowledgement is not accepted.
    pub fn clear_history_continuity(
        &self,
        receipt: &ReconstructionReceipt,
    ) -> Result<(), ReconcileRefusal> {
        if receipt.provenance.trim().is_empty() {
            return Err(ReconcileRefusal::EmptyProvenance);
        }
        if receipt.coverage_from_ms > receipt.coverage_to_ms {
            return Err(ReconcileRefusal::UnorderedCoverage);
        }
        // Production continuity clearing needs a real reconstruction; a plausible receipt cannot
        // clear it.
        Err(ReconcileRefusal::ReconstructionUnsupported)
    }

    /// TEST-ONLY continuity clear (compiled under `cfg(test)` only).
    #[cfg(test)]
    pub(crate) fn clear_history_continuity_fixture(&mut self) {
        self.history_continuity_unknown = false;
    }

    /// Revision of the unresolved-gap set (changes whenever it does).
    #[must_use]
    pub fn missing_rev(&self) -> u64 {
        self.missing_rev
    }

    /// Whether startup continuity could not be established.
    #[must_use]
    pub fn history_continuity_unknown(&self) -> bool {
        self.history_continuity_unknown
    }

    /// One mint's unreconciled drops (status writer only; never on the hot path).
    #[must_use]
    pub fn unreconciled_drops(&self, mint: &[u8; 32]) -> u64 {
        self.mints.get(mint).map_or(0, |mc| {
            mc.flow_drops.iter().filter(|m| m.receipt.is_none()).count() as u64
        })
    }

    /// The last curve reserve observation for a mint (for the paper fill), if any.
    #[must_use]
    pub fn curve_obs(&self, mint: &[u8; 32]) -> Option<CurveObservation> {
        self.annotation.curve_of(mint).copied()
    }

    /// The last AMM reserve observation for a mint (for the paper fill), if any.
    #[must_use]
    pub fn amm_obs(&self, mint: &[u8; 32]) -> Option<AmmObservation> {
        self.annotation.amm_of(mint).cloned()
    }

    /// What coverage reporting needs to split by: the mint's last print venue label, its age at
    /// `t_dec_ms` measured from the LAUNCH (None when no launch is known -- never first-seen),
    /// and how many prints the cache has accepted for it (the warm-up depth).
    #[must_use]
    pub fn describe(&self, mint: &[u8; 32], t_dec_ms: i64) -> (&'static str, Option<f64>, u64) {
        let mc = self.mints.get(mint);
        let venue = mc.map_or("unknown", |m| m.venue.as_str());
        let age = self
            .launch_ms
            .get(mint)
            .map(|l| (t_dec_ms - *l) as f64 / 1_000.0); // LINT-ALLOW(money_float_cast): seconds, not money
        (venue, age, mc.map_or(0, |m| m.n_accepted))
    }

    #[must_use]
    pub fn marker(&self, mint: &[u8; 32]) -> Option<StateMarker> {
        self.mints.get(mint).map(|m| StateMarker {
            last_recv_ms: m.last_recv_ms,
            n_accepted: m.n_accepted,
        })
    }

    /// Whether the cache's latest view of `mint` is the AMM plane (a sell would land on the pool).
    #[must_use]
    pub fn snapshot_venue_is_amm(&self, mint: &[u8; 32]) -> bool {
        self.amm_obs(mint).is_some()
            && self
                .mints
                .get(mint)
                .is_some_and(|m| matches!(m.venue, VenueLabel::Pumpswap))
    }

    /// Forget a mint that left the watchlist (bounded state, §99).
    pub fn forget(&mut self, mint: &[u8; 32]) {
        self.ledger.forget(mint);
        self.mints.remove(mint);
        self.pools.remove(mint);
        self.launch_ms.remove(mint);
    }

    /// Cut an immutable prompt snapshot for `mint` as of `t_dec_ms`, or name why not.
    ///
    /// Order of refusals is the order of cheapness and of blame: identity of the mint, launch
    /// provenance, causality, corpus eligibility, then each plane. Every plane is read from its
    /// existing producer; nothing here computes a market quantity.
    fn prepare(
        &self,
        mint: &[u8; 32],
        t_dec_ms: i64,
        audience: Audience,
    ) -> Result<Prepared, JoinRefusal> {
        let Some(mc) = self.mints.get(mint) else {
            return Err(JoinRefusal::NoMint);
        };
        let Some(&launch) = self.launch_ms.get(mint) else {
            return Err(JoinRefusal::LaunchUnknown);
        };
        let dev = self.creators.dev_history(mint);
        if dev.creator_known == 0 {
            return Err(JoinRefusal::LaunchUnknown);
        }
        // First observation is NOT launch time: the history must begin at the launch.
        let gap = mc.first_seen_ms - launch;
        if gap > LAUNCH_TOLERANCE_MS {
            return Err(JoinRefusal::HistoryStartsAfterLaunch { gap_ms: gap });
        }
        if mc.last_recv_ms > t_dec_ms {
            return Err(JoinRefusal::FutureStateInCache {
                newest_ms: mc.last_recv_ms,
            });
        }
        if self.history_continuity_unknown {
            return Err(JoinRefusal::HistoryContinuityUnknown);
        }
        // UPSTREAM DROP — fail-closed, blamed before any "quiet"/"few trades" verdict, and
        // per-DEPENDENCY (see [`MissingObservation`]).
        // (A) ROLLING: this clock's 300 s flow window is missing a print the derivation refused.
        //     Known-incomplete until the drop leaves that window — not before.
        if let Some(m) = mc
            .flow_drops
            .iter()
            .find(|m| m.drop_ms >= t_dec_ms - WINDOW_300_MS && m.drop_ms < t_dec_ms)
        {
            return Err(JoinRefusal::FlowUpstreamDrop { drop_ms: m.drop_ms });
        }
        // (B) CUMULATIVE: the same print never entered the mint's tape, so its cumulative
        //     counters (n_prior_trades / volumes / unique traders / shares / age) are short.
        //     NO timer repairs that — a fresh reserve snapshot does not restore trade history —
        //     so it stays refused until reconstructed from a capture or reconciled.
        if let Some(m) = mc.flow_drops.iter().find(|m| {
            m.receipt.is_none()
                && match audience {
                    Audience::Entry => m.deps.blocks_entry(),
                    Audience::Management => m.deps.blocks_management(),
                }
        }) {
            return Err(JoinRefusal::FlowHistoryUnreconstructable { drop_ms: m.drop_ms });
        }
        self.ledger
            .eligibility(mint, t_dec_ms)
            .map_err(JoinRefusal::State)?;
        let state = self
            .ledger
            .serve(mint, t_dec_ms)
            .ok_or(JoinRefusal::State(ClockRefusal::TapeIncomplete))?;

        if mc.identity_missing > 0 {
            return Err(JoinRefusal::EnrichmentIdentityMissing {
                prints: mc.identity_missing,
            });
        }
        if mc.enrich_overflow {
            return Err(JoinRefusal::EnrichmentOverflow);
        }
        let upto = mc.enrich.partition_point(|e| e.recv_unix_ms <= t_dec_ms);
        let enriched = enrich(&mc.enrich[..upto], t_dec_ms).map_err(JoinRefusal::Enrichment)?;

        if mc.flow_meta_missing > 0 {
            return Err(JoinRefusal::FlowMetaMissing {
                prints: mc.flow_meta_missing,
            });
        }
        let flow = match self.flow.serve(mint, t_dec_ms) {
            FlowOutcome::NoPriorFlow => return Err(JoinRefusal::FlowAggregatesIncomplete),
            FlowOutcome::Aggregates(a) => {
                if a.entrant_fee_p90_lamports.is_none() || a.entrant_cu_p50.is_none() {
                    return Err(JoinRefusal::FlowAggregatesIncomplete);
                }
                flow_state_from_aggregates(&a)
            }
        };
        let _ = zero_flow_state; // the zero block is never served: the corpus has no such row

        let view = self.annotation.reserve_view(mint, t_dec_ms);
        let venue = state.venue.clone();
        let needs_curve = venue == "pumpfun" || venue == "mixed";
        let needs_amm = venue == "pumpswap" || venue == "mixed";
        if needs_curve {
            if let CurveState::Absent { reason } = &view.curve {
                return Err(JoinRefusal::CurveAbsent(reason.clone()));
            }
        }
        if needs_amm {
            if self.pools.get(mint).is_some_and(|b| b.conflicting) {
                return Err(JoinRefusal::AmmPoolAmbiguous);
            }
            if let AmmState::Absent { reason } = &view.amm {
                return Err(JoinRefusal::AmmAbsent(reason.clone()));
            }
        }

        Ok(Prepared {
            state,
            enriched,
            flow,
            view,
            dev,
            venue,
            last_recv_ms: mc.last_recv_ms,
            n_accepted: mc.n_accepted,
        })
    }

    pub fn snapshot(&self, mint: &[u8; 32], t_dec_ms: i64) -> Result<PromptSnapshot, JoinRefusal> {
        let Prepared {
            state,
            enriched,
            flow,
            view,
            dev,
            venue,
            last_recv_ms,
            n_accepted,
        } = self.prepare(mint, t_dec_ms, Audience::Entry)?;
        let mc_last_recv_ms = last_recv_ms;
        let mc_n_accepted = n_accepted;
        let inputs = BundleInputs {
            snapshot: &state,
            enriched: &enriched,
            t_dec_ms,
            venue: &venue,
            mcap_sol_at_t: view.mcap_sol_at_t,
            mcap_source: view.mcap_source,
            flow: &flow,
            flow_no_prior: false,
            curve: view.curve.clone(),
            amm: view.amm.clone(),
            dev,
            size_depth_sol: view.size_depth_sol,
            size_amm: view.size_amm,
            identity: None,
            policy: &self.policy,
        };
        let bundle = assemble(&inputs).map_err(JoinRefusal::Assembly)?;
        let user_prompt = render_decision(&bundle);
        let depth_lamports = view
            .size_depth_sol
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| (d * 1e9) as u64); // LINT-ALLOW(money_float_cast): view depth is SOL f64 by contract
        Ok(PromptSnapshot {
            mint: *mint,
            t_dec_ms,
            system_prompt: system_prompt(PromptFamily::Decision).to_string(),
            prompt_digest: fnv1a(&user_prompt),
            user_prompt,
            venue,
            size_amm: view.size_amm,
            depth_lamports,
            price_lamports_per_raw_token: state.price_lamports_per_raw_token,
            n_prior_trades: state.n_prior_trades,
            marker: StateMarker {
                last_recv_ms: mc_last_recv_ms,
                n_accepted: mc_n_accepted,
            },
        })
    }

    /// Cut an immutable MANAGEMENT prompt for a held position. Every market-side input comes from
    /// the same producers as the entry snapshot (one `prepare`); the position-side inputs are the
    /// caller's and are passed through the trained renderer unchanged.
    pub fn management_snapshot(
        &self,
        mint: &[u8; 32],
        t_dec_ms: i64,
        pos: &MgmtPositionInputs,
    ) -> Result<MgmtSnapshot, JoinRefusal> {
        let Prepared {
            state,
            enriched,
            flow,
            view,
            dev,
            venue,
            last_recv_ms,
            n_accepted,
        } = self.prepare(mint, t_dec_ms, Audience::Management)?;
        // FRESHNESS PER COMPONENT (management only). The entry corpus renders a stale reserve with
        // pricing_eligible=false and lets the model weigh it; a HELD position is marked and sized
        // from these reserves, so a stale one is refused here. Bound = the existing
        // `PRICING_BUDGET_MS` (the annotation's own "may be priced against" contract), not a new number.
        if venue == "pumpfun" || venue == "mixed" {
            if let CurveState::Present {
                staleness_ms,
                pricing_eligible: false,
                ..
            } = &view.curve
            {
                return Err(JoinRefusal::ReserveStale {
                    component: "curve",
                    staleness_ms: *staleness_ms,
                });
            }
        }
        if venue == "pumpswap" || venue == "mixed" {
            if let AmmState::Present {
                staleness_ms,
                pricing_eligible: false,
                ..
            } = &view.amm
            {
                return Err(JoinRefusal::ReserveStale {
                    component: "amm",
                    staleness_ms: *staleness_ms,
                });
            }
        }
        let depth_sol = view
            .size_depth_sol
            .filter(|d| d.is_finite() && *d > 0.0)
            .ok_or(JoinRefusal::DepthUnknown)?;
        let mark = state.price_lamports_per_raw_token;
        if !(mark.is_finite() && mark > 0.0) {
            return Err(JoinRefusal::DepthUnknown);
        }
        let market = if venue == "pumpswap" {
            "graduated_amm"
        } else {
            "bonding_curve"
        };
        let known = dev.creator_known == 1;
        let r6 = |v: f64| PyNum::Float(py_round(v, 6));
        let bundle = pump_quant_proposal::ManagementBundle {
            mint: mint_label(mint),
            t_dec: t_dec_ms,
            step: pos.step,
            venue: venue.clone(),
            market: market.to_string(),
            depth_sol,
            mark,
            entry_px: pos.entry_px,
            upnl_bp: (mark / pos.entry_px - 1.0) * 1e4,
            held_s: pos.held_s,
            mfe_bp: pos.mfe_bp,
            mae_bp: pos.mae_bp,
            qty_pre: pos.qty_scaled,
            cash_pre: pos.cash_sol,
            enriched: pump_quant_proposal::management::EnrichedManagement {
                holders_at_t: Some(PyNum::Int(enriched.holders_at_t as i64)),
                top1_float_share: Some(r6(enriched.top1_float_share)),
                holder_hhi: Some(r6(enriched.holder_hhi)),
                mcap_sol_at_t: view.mcap_sol_at_t.map(r6),
                bundle_wallets: Some(PyNum::Int(enriched.bundle_wallets as i64)),
                round_trip_wallets: Some(PyNum::Int(enriched.round_trip_wallets as i64)),
                creator_past_launches: dev.creator_past_launches.map(PyNum::Int),
                creator_known: Some(PyNum::Bool(known)),
                wash_ratio: Some(r6(enriched.wash_ratio)),
            },
            dev: pump_quant_proposal::management::DevHistoryManagement {
                creator_past_launches: dev.creator_past_launches.map(PyNum::Int),
                creator_known: Some(PyNum::Bool(known)),
                bundle_wallets: Some(PyNum::Int(enriched.bundle_wallets as i64)),
                wash_ratio: Some(r6(enriched.wash_ratio)),
            },
            flow,
            flow_no_prior: false,
        };
        let user_prompt = pump_quant_proposal::render_management(&bundle);
        Ok(MgmtSnapshot {
            mint: *mint,
            t_dec_ms,
            system_prompt: system_prompt(PromptFamily::Management).to_string(),
            prompt_digest: fnv1a(&user_prompt),
            user_prompt,
            venue,
            size_amm: view.size_amm,
            mark_price_lamports_per_raw_token: mark,
            marker: StateMarker {
                last_recv_ms,
                n_accepted,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINT: [u8; 32] = [7u8; 32];
    const CREATOR: [u8; 32] = [9u8; 32];
    const T0: i64 = 1_800_000_000_000;

    fn wallet(i: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[0] = i;
        w[31] = 1;
        w
    }

    fn trade(i: u32) -> TradeObs {
        #[allow(clippy::manual_is_multiple_of)] // MSRV 1.85: is_multiple_of stabilised in 1.87
        let buy = i % 3 != 0;
        TradeObs {
            mint: MINT,
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i),
            trader: Some(wallet((i % 40) as u8 + 1)),
            recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
            slot: Some(1_000 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: VenueLabel::Pumpfun,
            event_id: None,
            feature: None,
        }
    }

    /// Two DISTINCT events sharing slot, trader, size, time AND price both survive; a repeated
    /// delivery of one event does not. Price plays no part in identity.
    #[test]
    fn distinct_events_identical_in_every_field_survive_and_a_redelivery_does_not() {
        let mut c = DecisionCache::new();
        assert!(c.observe_launch(MINT, CREATOR, T0));
        let mut a = trade(0);
        a.event_id = Some(1);
        let mut b = a; // identical slot/trader/size/time/price ...
        b.event_id = Some(2); // ... but a different underlying event
        assert_eq!(c.observe_trade(&a), Ingest::Accepted);
        assert_eq!(
            c.observe_trade(&b),
            Ingest::Accepted,
            "distinct event must survive"
        );
        assert_eq!(
            c.observe_trade(&a),
            Ingest::Duplicate,
            "same event redelivered"
        );
        assert_eq!(c.observe_trade(&b), Ingest::Duplicate);
        assert_eq!((c.counters().accepted, c.counters().duplicate), (2, 2));
    }

    /// Without an id the original heuristic key applies and price is NOT part of it.
    #[test]
    fn id_less_prints_keep_the_heuristic_key_price_is_not_identity() {
        let mut c = DecisionCache::new();
        assert!(c.observe_launch(MINT, CREATOR, T0));
        let a = trade(0);
        let mut b = a;
        b.price_fp += 1;
        assert_eq!(c.observe_trade(&a), Ingest::Accepted);
        assert_eq!(c.observe_trade(&b), Ingest::Duplicate);
    }

    fn curve() -> CurveObservation {
        CurveObservation {
            v_sol_lamports: 37_900_000_000,
            v_tokens: 849_000_000_000_000,
            real_sol_lamports: 7_900_000_000,
            real_tokens: 569_000_000_000_000,
            ts_ms: T0 + 60_000,
            slot: 1_100,
        }
    }

    /// A cache with a launch at T0, `n` fully-attributed prints, and a curve observation.
    fn ready(n: u32) -> DecisionCache {
        let mut c = DecisionCache::new();
        assert!(c.observe_launch(MINT, CREATOR, T0));
        for i in 0..n {
            assert_eq!(c.observe_trade(&trade(i)), Ingest::Accepted);
        }
        assert!(c.observe_curve(MINT, curve()));
        c
    }

    fn t_dec(n: u32) -> i64 {
        T0 + 1_000 + i64::from(n) * 2_000 + 1_000
    }

    #[test]
    fn a_fully_attributed_curve_mint_renders_the_trained_prompt() {
        let c = ready(40);
        let s = c.snapshot(&MINT, t_dec(40)).expect("snapshot");
        assert!(s.user_prompt.starts_with("DECISION CLOCK"));
        assert!(s.user_prompt.contains("venue=pumpfun"));
        assert!(s.user_prompt.contains("entrant_fee_p90_lamports="));
        assert!(s.user_prompt.contains("entrant_cu_p50="));
        assert!(s
            .user_prompt
            .contains("DEV HISTORY: creator_past_launches=0 creator_known=1"));
        assert!(s.user_prompt.contains("curve_reserves=present"));
        assert_eq!(s.n_prior_trades, 40);
        assert_eq!(s.prompt_digest, fnv1a(&s.user_prompt));
        assert!(s.depth_lamports.is_some());
    }

    #[test]
    fn the_snapshot_is_immutable_while_the_market_keeps_moving() {
        let mut c = ready(40);
        let s = c.snapshot(&MINT, t_dec(40)).expect("snapshot");
        let before = s.clone();
        assert_eq!(c.observe_trade(&trade(40)), Ingest::Accepted);
        assert_ne!(c.marker(&MINT).unwrap(), s.marker, "the cache moved on");
        assert_eq!(s, before, "a cut snapshot never changes");
    }

    #[test]
    fn a_mint_with_no_launch_record_is_refused_not_given_a_clean_creator() {
        let mut c = DecisionCache::new();
        for i in 0..40 {
            c.observe_trade(&trade(i));
        }
        c.observe_curve(MINT, curve());
        assert_eq!(
            c.snapshot(&MINT, t_dec(40)).unwrap_err(),
            JoinRefusal::LaunchUnknown
        );
    }

    #[test]
    fn first_observation_is_not_launch_time() {
        let mut c = DecisionCache::new();
        // Launched an hour before the bot first saw it trade.
        c.observe_launch(MINT, CREATOR, T0 - 3_600_000);
        for i in 0..40 {
            c.observe_trade(&trade(i));
        }
        c.observe_curve(MINT, curve());
        match c.snapshot(&MINT, t_dec(40)).unwrap_err() {
            JoinRefusal::HistoryStartsAfterLaunch { gap_ms } => assert!(gap_ms >= 3_600_000),
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    #[test]
    fn a_print_without_a_clock_is_refused_at_ingest() {
        let mut c = DecisionCache::new();
        let mut t = trade(0);
        t.recv_unix_ms = None;
        assert_eq!(c.observe_trade(&t), Ingest::NoClock);
        assert_eq!(c.counters().no_clock, 1);
        assert!(c.marker(&MINT).is_none(), "nothing was admitted");
    }

    #[test]
    fn out_of_order_and_duplicate_prints_are_refused_and_counted() {
        let mut c = ready(30);
        assert_eq!(c.observe_trade(&trade(5)), Ingest::OutOfOrder);
        let last = trade(29);
        assert_eq!(c.observe_trade(&last), Ingest::Duplicate);
        let k = c.counters();
        assert_eq!((k.out_of_order, k.duplicate), (1, 1));
        assert_eq!(c.marker(&MINT).unwrap().n_accepted, 30);
    }

    #[test]
    fn missing_fee_cu_or_slot_poison_the_flow_block_instead_of_reading_as_quiet() {
        for which in 0..3 {
            let mut c = DecisionCache::new();
            c.observe_launch(MINT, CREATOR, T0);
            for i in 0..40 {
                let mut t = trade(i);
                if i == 20 {
                    match which {
                        0 => t.fee_lamports = None,
                        1 => t.cu_consumed = None,
                        _ => t.slot = None,
                    }
                }
                c.observe_trade(&t);
            }
            c.observe_curve(MINT, curve());
            assert!(
                matches!(
                    c.snapshot(&MINT, t_dec(40)),
                    Err(JoinRefusal::FlowMetaMissing { prints: 1 })
                ),
                "case {which}"
            );
        }
    }

    #[test]
    fn a_print_with_no_trader_poisons_enrichment_by_name() {
        let mut c = DecisionCache::new();
        c.observe_launch(MINT, CREATOR, T0);
        for i in 0..40 {
            let mut t = trade(i);
            if i == 10 {
                t.trader = None;
            }
            c.observe_trade(&t);
        }
        c.observe_curve(MINT, curve());
        assert!(matches!(
            c.snapshot(&MINT, t_dec(40)),
            Err(JoinRefusal::EnrichmentIdentityMissing { prints: 1 })
        ));
    }

    #[test]
    fn too_few_prints_and_an_idle_market_are_the_corpus_gates() {
        let c = ready(10);
        assert!(matches!(
            c.snapshot(&MINT, t_dec(10)).unwrap_err(),
            JoinRefusal::State(ClockRefusal::FewPriorTrades { have: 10, .. })
        ));
        let c = ready(40);
        assert!(matches!(
            c.snapshot(&MINT, t_dec(40) + 120_000).unwrap_err(),
            JoinRefusal::State(ClockRefusal::IdleTooLong { .. })
        ));
    }

    #[test]
    fn state_newer_than_the_decision_clock_never_enters_a_prompt() {
        let c = ready(40);
        // Ask for a clock in the middle of the history: the cache already holds later prints.
        let early = T0 + 1_000 + 25 * 2_000;
        assert!(matches!(
            c.snapshot(&MINT, early).unwrap_err(),
            JoinRefusal::FutureStateInCache { .. }
        ));
    }

    #[test]
    fn a_missing_curve_observation_is_a_named_refusal_not_a_blank_plane() {
        let mut c = DecisionCache::new();
        c.observe_launch(MINT, CREATOR, T0);
        for i in 0..40 {
            c.observe_trade(&trade(i));
        }
        match c.snapshot(&MINT, t_dec(40)).unwrap_err() {
            JoinRefusal::CurveAbsent(r) => assert_eq!(r, "mint_absent"),
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    fn mgmt_inputs() -> MgmtPositionInputs {
        MgmtPositionInputs {
            step: 1,
            entry_px: 44.0,
            qty_scaled: 0.0057,
            cash_sol: 0.75,
            held_s: 90.0,
            mfe_bp: 10.0,
            mae_bp: -5.0,
        }
    }

    #[test]
    fn management_refuses_a_stale_reserve_even_when_the_latest_trade_is_fresh() {
        // 80 prints => the newest trade is 1 s before the decision clock (fresh by the ledger's own
        // idle bound), while the only curve observation is ~100 s old (> PRICING_BUDGET_MS).
        let c = ready(80);
        let t = t_dec(80);
        match c.management_snapshot(&MINT, t, &mgmt_inputs()) {
            Err(JoinRefusal::ReserveStale {
                component: "curve",
                staleness_ms,
            }) => {
                assert!(
                    staleness_ms > crate::curve_annotation::PRICING_BUDGET_MS,
                    "{staleness_ms}"
                )
            }
            other => panic!("a fresh trade must not launder a stale reserve: {other:?}"),
        }
        // CONTROL: the same cache with a curve observation inside the budget is NOT refused for
        // freshness, so the check can fail.
        let mut c2 = ready(80);
        let mut fresh = curve();
        fresh.ts_ms = t - 5_000;
        fresh.slot = 2_000;
        assert!(c2.observe_curve(MINT, fresh));
        assert!(!matches!(
            c2.management_snapshot(&MINT, t, &mgmt_inputs()),
            Err(JoinRefusal::ReserveStale { .. })
        ));
    }

    #[test]
    fn a_stale_curve_observation_is_refused() {
        let mut c = DecisionCache::new();
        c.observe_launch(MINT, CREATOR, T0);
        for i in 0..40 {
            c.observe_trade(&trade(i));
        }
        let mut old = curve();
        old.ts_ms = T0 - 2 * 24 * 3_600_000;
        c.observe_curve(MINT, old);
        assert!(matches!(
            c.snapshot(&MINT, t_dec(40)).unwrap_err(),
            JoinRefusal::CurveAbsent(_)
        ));
    }

    #[test]
    fn two_pools_for_one_mint_refuse_the_amm_plane_by_name() {
        let mut c = DecisionCache::new();
        c.observe_launch(MINT, CREATOR, T0);
        for i in 0..40 {
            let mut t = trade(i);
            t.venue = VenueLabel::Pumpswap;
            c.observe_trade(&t);
        }
        c.bind_pool(MINT, "PoolAAAA");
        c.bind_pool(MINT, "PoolBBBB");
        assert_eq!(
            c.snapshot(&MINT, t_dec(40)).unwrap_err(),
            JoinRefusal::AmmPoolAmbiguous
        );
    }

    #[test]
    fn an_upstream_dropped_print_refuses_the_flow_block_by_name_and_recovers() {
        // 80 fully-attributed prints so the refusal clock is past the drop and the market is
        // not otherwise idle.
        let mut c = ready(80);
        let t_refuse = t_dec(80);
        // A print dropped early in the window, before the decision clock: the reserve delta
        // was refused by the feed derivation, so it never reached the reducer.
        let drop_ms = T0 + 1_000;
        c.note_flow_upstream_drop(MINT, drop_ms);
        assert_eq!(c.counters().flow_upstream_drops, 1);
        assert_eq!(
            c.snapshot(&MINT, t_refuse),
            Err(JoinRefusal::FlowUpstreamDrop { drop_ms }),
            "a dropped print must refuse the window by name, not serve it"
        );

        // (A) ROLLING window clears: extend the history past drop_ms + 300 s. The drop is now
        // outside the served flow WINDOW, so the rolling reason no longer fires...
        for i in 80..155 {
            assert_eq!(c.observe_trade(&trade(i)), Ingest::Accepted);
        }
        let t_recover = t_dec(155);
        assert!(
            t_recover - drop_ms > WINDOW_300_MS,
            "the drop must have left the 300 s window: {drop_ms} vs {t_recover}"
        );
        // ...but the mint's CUMULATIVE history is still short the print, and NO timer repairs
        // that (a fresh reserve snapshot does not restore missing trade history). The old
        // behaviour — serving again merely because 300 s elapsed — would have presented the
        // cumulative counters as complete while they still depended on the missing print.
        assert_eq!(
            c.snapshot(&MINT, t_recover),
            Err(JoinRefusal::FlowHistoryUnreconstructable { drop_ms }),
            "a timer must not clear cumulative incompleteness"
        );

        // RECOVERY is by RECONSTRUCTION only — a bounded replay/backfill or an operator
        // reconciliation — never by a timer.
        // NO API clears the gap without a valid reconstruction receipt: an operator may initiate
        // reconstruction, but an acknowledgement alone is not evidence.
        assert_eq!(
            c.reconcile_flow_history(
                &MINT,
                &ReconstructionReceipt {
                    provenance: String::new(),
                    coverage_from_ms: drop_ms - 1,
                    coverage_to_ms: drop_ms + 1,
                }
            ),
            Err(ReconcileRefusal::EmptyProvenance),
            "an acknowledgement without provenance must not clear the gap"
        );
        assert_eq!(
            c.reconcile_flow_history(
                &MINT,
                &ReconstructionReceipt {
                    provenance: "capture:test".into(),
                    coverage_from_ms: drop_ms + 1,
                    coverage_to_ms: drop_ms + 2,
                }
            ),
            Err(ReconcileRefusal::CoverageDoesNotSpanTheGap),
            "a window that does not span the drop must not clear it"
        );
        // A PLAUSIBLE, covering receipt is NECESSARY but NOT SUFFICIENT: no aggregates have been
        // reconstructed, so inference over unchanged incomplete state must stay refused.
        assert_eq!(
            c.reconcile_flow_history(
                &MINT,
                &ReconstructionReceipt {
                    provenance: "capture:test".into(),
                    coverage_from_ms: drop_ms - 1,
                    coverage_to_ms: drop_ms + 1,
                }
            ),
            Err(ReconcileRefusal::ReconstructionUnsupported),
            "metadata alone must not unlock inference"
        );
        assert!(
            c.snapshot(&MINT, t_recover).is_err(),
            "a plausible receipt must leave the incomplete state refused"
        );
        // TEST-ONLY fixture restoration, deliberately separate from production recovery
        // (`reconcile_flow_history`), shows the gate opening only when state is installed.
        assert!(c
            .install_reconstructed_fixture(
                &MINT,
                &ReconstructionReceipt {
                    provenance: "capture:test".into(),
                    coverage_from_ms: drop_ms - 1,
                    coverage_to_ms: drop_ms + 1,
                }
            )
            .is_ok());
        assert!(
            c.snapshot(&MINT, t_recover).is_ok(),
            "after a genuine reconstruction the mint serves again"
        );
    }

    #[test]
    fn persisted_missing_history_survives_a_restart_and_unreadable_state_never_reads_as_complete() {
        // A live cache records the gap.
        let mut live = ready(80);
        let drop_ms = T0 + 1_000;
        live.note_flow_upstream_drop(MINT, drop_ms);
        for i in 80..155 {
            let _ = live.observe_trade(&trade(i));
        }
        let t = t_dec(155);
        assert_eq!(
            live.snapshot(&MINT, t),
            Err(JoinRefusal::FlowHistoryUnreconstructable { drop_ms }),
            "the cumulative gap must refuse before any restart"
        );
        let records = live.missing_history_records();
        assert_eq!(records.len(), 1, "the unresolved record must be exportable");

        // RESTART: history is rebuilt independently and the persisted gap is restored BEFORE
        // inference resumes — the restart must NOT erase the gap.
        let mut restarted = ready(155);
        assert!(restarted.restore_missing_history(&records, true).is_ok());
        assert_eq!(
            restarted.snapshot(&MINT, t),
            Err(JoinRefusal::FlowHistoryUnreconstructable { drop_ms }),
            "a restart must restore the gap, not lose it"
        );
        assert_eq!(restarted.unreconciled_drops(&MINT), 1);

        // AN UNREADABLE / INCOMPATIBLE record must NOT silently become "complete": continuity is
        // refused by name for every prompt.
        let mut corrupt = ready(155);
        assert_eq!(
            corrupt.restore_missing_history(&[], false),
            Err(RestoreRefusal::Unreadable)
        );
        assert!(corrupt.history_continuity_unknown());
        assert_eq!(
            corrupt.snapshot(&MINT, t),
            Err(JoinRefusal::HistoryContinuityUnknown),
            "unreadable persisted state must refuse, never assume no gap occurred"
        );
        // Only EVIDENCE clears continuity; a bare acknowledgement is not evidence.
        assert_eq!(
            corrupt.clear_history_continuity(&ReconstructionReceipt {
                provenance: String::new(),
                coverage_from_ms: 0,
                coverage_to_ms: 1,
            }),
            Err(ReconcileRefusal::EmptyProvenance)
        );
        assert!(corrupt.history_continuity_unknown(), "still refused");
        assert_eq!(
            corrupt.clear_history_continuity(&ReconstructionReceipt {
                provenance: "capture:test".into(),
                coverage_from_ms: 0,
                coverage_to_ms: 1,
            }),
            Err(ReconcileRefusal::ReconstructionUnsupported),
            "a plausible receipt must not clear continuity"
        );
        assert!(corrupt.history_continuity_unknown(), "still refused");
        corrupt.clear_history_continuity_fixture();
        assert!(!corrupt.history_continuity_unknown());
    }

    #[test]
    fn compaction_never_discards_an_unresolved_gap() {
        let mut c = ready(80);
        let first = T0 + 1_000;
        c.note_flow_upstream_drop(MINT, first);
        for i in 0..(FLOW_UPSTREAM_DROP_RING + 5) {
            c.note_flow_upstream_drop(MINT, first + 1_000 + i as i64);
        }
        let recs = c.missing_history_records();
        assert!(
            recs.iter().any(|(m, r)| *m == MINT && r.drop_ms == first),
            "bounding the record must not discard an unresolved cumulative gap: {recs:?}"
        );
        assert!(c.unreconciled_drops(&MINT) >= 1);
    }

    #[test]
    fn an_ordinary_no_trade_window_is_not_flagged_as_feed_loss() {
        // No drop recorded: the ordinary path serves, so the new check cannot fire on a
        // market that simply had prints.
        let c = ready(40);
        assert!(
            c.snapshot(&MINT, t_dec(40)).is_ok(),
            "an ordinary window with no upstream drop must serve"
        );
        assert_eq!(c.counters().flow_upstream_drops, 0);

        // An honestly quiet window keeps its existing, honest refusal — IdleTooLong — and is
        // never relabelled as feed loss.
        match c.snapshot(&MINT, t_dec(40) + 120_000) {
            Err(JoinRefusal::FlowUpstreamDrop { .. }) => {
                panic!("a quiet window was mislabelled as an upstream drop")
            }
            Err(JoinRefusal::State(ClockRefusal::IdleTooLong { .. })) | Ok(_) => {}
            other => panic!("unexpected refusal for a quiet window: {other:?}"),
        }
    }

    #[test]
    fn every_refusal_has_a_distinct_stable_label() {
        let all = [
            JoinRefusal::NoMint,
            JoinRefusal::LaunchUnknown,
            JoinRefusal::HistoryStartsAfterLaunch { gap_ms: 1 },
            JoinRefusal::FutureStateInCache { newest_ms: 1 },
            JoinRefusal::EnrichmentIdentityMissing { prints: 1 },
            JoinRefusal::EnrichmentOverflow,
            JoinRefusal::FlowMetaMissing { prints: 1 },
            JoinRefusal::FlowAggregatesIncomplete,
            JoinRefusal::FlowUpstreamDrop { drop_ms: 1 },
            JoinRefusal::CurveAbsent(String::new()),
            JoinRefusal::AmmAbsent(String::new()),
            JoinRefusal::AmmPoolAmbiguous,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for r in &all {
            assert!(seen.insert(r.as_str()), "duplicate label {}", r.as_str());
        }
    }
    #[test]
    fn the_unpriced_instruction_half_is_not_a_second_copy_of_the_trade() {
        let mut c = DecisionCache::new();
        let mut half = trade(0);
        half.price_fp = 0;
        assert_eq!(c.observe_trade(&half), Ingest::NoPrice);
        assert_eq!(c.counters().no_price, 1);
        assert_eq!(
            c.counters().accepted,
            0,
            "it never reaches enrichment or flow"
        );
    }

    fn obs_with(deps: MissingDeps, kind: MissingKind, ms: i64) -> MissingObservation {
        MissingObservation {
            drop_ms: ms,
            kind,
            count: 1,
            source_id: String::new(),
            deps,
            receipt: None,
        }
    }

    #[test]
    fn entry_and_management_gate_on_different_dependencies() {
        let ledger_only = MissingDeps {
            ledger_cumulative: true,
            ..MissingDeps::default()
        };
        // Past the 300 s rolling horizon so only the CUMULATIVE dependency is in play.
        let t = t_dec(155) + 400_000;
        let mut c = ready(155);
        c.restore_missing_history(
            &[(
                MINT,
                obs_with(ledger_only, MissingKind::PossibleTrade, T0 + 1_000),
            )],
            true,
        )
        .unwrap();
        assert_eq!(
            c.snapshot(&MINT, t),
            Err(JoinRefusal::FlowHistoryUnreconstructable {
                drop_ms: T0 + 1_000
            }),
            "a ledger-cumulative gap blocks ENTRY"
        );
        let st = c.missing_history_status(&MINT).unwrap();
        assert!(st.entry_unavailable && !st.management_unavailable);
        let held = mgmt_inputs();
        let m = c.management_snapshot(&MINT, t, &held);
        assert!(
            !matches!(m, Err(JoinRefusal::FlowHistoryUnreconstructable { .. })),
            "a ledger-only gap must NOT block MANAGEMENT (it renders none of it): {m:?}"
        );
        // A holder-enrichment gap blocks both.
        let holder = MissingDeps {
            holder_enrichment: true,
            ..MissingDeps::default()
        };
        let mut c2 = ready(155);
        c2.restore_missing_history(
            &[(
                MINT,
                obs_with(holder, MissingKind::PossibleTrade, T0 + 1_000),
            )],
            true,
        )
        .unwrap();
        assert!(matches!(
            c2.management_snapshot(&MINT, t, &held),
            Err(JoinRefusal::FlowHistoryUnreconstructable { .. })
        ));
        // CONTROL: no gap, no refusal of that kind.
        assert!(!matches!(
            ready(155).snapshot(&MINT, t),
            Err(JoinRefusal::FlowHistoryUnreconstructable { .. })
        ));
    }

    #[test]
    fn compaction_preserves_the_union_of_dependencies_count_and_conservative_kind() {
        let mut c = ready(80);
        let only_ledger = MissingDeps {
            ledger_cumulative: true,
            ..MissingDeps::default()
        };
        let only_holder = MissingDeps {
            holder_enrichment: true,
            ..MissingDeps::default()
        };
        let mut recs = vec![(
            MINT,
            obs_with(only_ledger, MissingKind::InvalidObservation, T0 + 1),
        )];
        recs.push((
            MINT,
            obs_with(only_holder, MissingKind::PossibleTrade, T0 + 2),
        ));
        c.restore_missing_history(&recs, true).unwrap();
        // Fill the ring with rolling-only observations until the two originals are folded.
        let rolling = MissingDeps {
            rolling_300s: true,
            ..MissingDeps::default()
        };
        let extra: Vec<_> = (0..FLOW_UPSTREAM_DROP_RING as i64 + 3)
            .map(|i| {
                (
                    MINT,
                    obs_with(rolling, MissingKind::InvalidObservation, T0 + 10 + i),
                )
            })
            .collect();
        c.restore_missing_history(&extra, true).unwrap();
        let all = c.missing_history_records();
        assert!(all.len() <= FLOW_UPSTREAM_DROP_RING);
        let u = all
            .iter()
            .fold(MissingDeps::default(), |a, (_, r)| a.union(r.deps));
        assert!(
            u.ledger_cumulative && u.holder_enrichment && u.rolling_300s,
            "union kept: {u:?}"
        );
        assert_eq!(
            all.iter().map(|(_, r)| r.drop_ms).min(),
            Some(T0 + 1),
            "earliest instant kept"
        );
        assert!(
            all.iter()
                .any(|(_, r)| r.kind == MissingKind::PossibleTrade),
            "conservative kind kept"
        );
        assert_eq!(
            all.iter().map(|(_, r)| u64::from(r.count)).sum::<u64>(),
            2 + extra.len() as u64,
            "no observation silently dropped from the count"
        );
    }
}
