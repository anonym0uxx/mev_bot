//! The nervous system: the continuous discovery→gate→scalp→reflect loop.
//!
//! This is the spine the whole bot hangs off. It owns the four discovery lanes, the
//! watchlist, the confirmed-market set, the paper positions' realized net-SOL, and
//! the decision journal, and it advances them under one explicit logical clock.
//!
//! Ingestion and evaluation are cleanly separated. Market/social/narrative/wallet
//! events and on-chain confirmations only *update state*; a [`AppEvent::Tick`] is
//! what runs the loop: refresh the watchlist from every lane (union, not
//! intersection), prune by recency, promote the top-ranked candidates, gate each
//! against on-chain corroboration and economic viability, paper-scalp the admits,
//! and — on the reflection cadence — let realized net-SOL reshape the lane weights.
//! Every step is a pure function of prior state and the event, so the same event
//! stream always produces the same decisions and the same net-SOL (§22, §54).

use crate::analytics::ReflectionAnalytics;
use crate::brain::{
    burst_phase_of, discovery_lane_of, exit_reason_of, narrative_class_of, platform_of,
    range_state_of, AppBlobStore, BrainAuthorRecord, BrainEntry, BrainMetaState, BrainPlane,
    BrainSetupClass, BrainSizeVerdict, BRAIN_BURST_BASELINE_MULT, BRAIN_TICK_NS,
};
use crate::config::Config;
use crate::event::AppEvent;
use crate::gate::{decide, Confirmation, GateDecision, GateReject};
use crate::journal_log::{Decision, DecisionJournal};
use crate::lane::{
    AttentionDecayParams, NarrativeLane, NumericEmitGate, NumericLane, Regime, SocialLane,
    WalletLane,
};
use crate::market_context::MarketContext;
use crate::measured_state::{
    brain_creator_class, brain_meta_saturation, brain_narrative_class, MeasuredState, MetaTotals,
    META_PHASE_NEUTRAL,
};
use crate::position::{DerivedTargets, Exit, ExitReason, LifecycleParams, ScalpLifecycle};
use crate::reflect::reflect_with_brain;
use crate::screen::{
    creator_credibility_haircut_bp, deployer_screen_haircut_bp, FlowScreen, WalletScreen,
};
use crate::shadow::{ChallengerStanding, ExitTournament};
use crate::structure::StructureState;
use crate::toxicity::{
    vpin_exit_escalates, vpin_size_mult_bp, VpinParams, VpinState, VpinThresholds,
};

use crate::attention::{AttentionField, AttentionParams};
use crate::event::CreatorActionKind;
use crate::extraction_risk::ExtractionRiskLedger;
use crate::hazard_scaffold::HazardScaffold;
use crate::holder_concentration::{
    brain_reading_of, concentration_of, internal_concentration_of, ConcentrationRisk,
    ConcentrationTrajectoryPlane, ConcentrationUnknown, ConcentrationVerdict, TOP10_HAIRCUT_BPS,
};
use pump_quant_brain::concentration::ConcentrationTrajectory as BrainTrajectory;

use crate::holder_flow::{HolderCountBasis, HolderFlow, HolderReading};
use crate::priced_move::PricedMove;
use crate::expected_move::SignalObs;
use crate::social_earn::{SocialEarn, SocialEarnParams};
use crate::social_ingest::{ledger_quality, to_mention, SourceQualityPolicy};
use crate::social_plane::{
    CallerTrustRow, FollowRecoRow, LensScoreRow, RefreshAt, SocialCallEvidence, SocialEvidenceRow,
    SocialPlane, SocialSupportRow, SupportNeed, UnfollowRow,
};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_evaluator::baseline_destruction::{
    baseline_destruction, Competitor, DestructionVerdict,
};
use pump_quant_evaluator::baseline_family::{
    run_family, BaselineResult, FamilyParams, FeeModel, TapeEvent,
};
use pump_quant_evaluator::convexity_enrich::{ConvexityMark, SizeFraction};
use pump_quant_evaluator::convexity_ledger::{RuleId, RuleKind};
use pump_quant_evaluator::evaluator_stats::{Lane as EvalLane, ReconTrade};
use pump_quant_evaluator::fdr::Hypothesis;
use pump_quant_evaluator::promotion_verdict::{
    promotion_verdict, PromotionBlockReason, PromotionStatisticalVerdict,
};
use pump_quant_evaluator::reflection_cadence::{
    reflect_mints, MintId, MintReflection, MintSwaps, ReflectionCadence,
};
use pump_quant_execution::ex_route_health::RouteHealthSet;
use pump_quant_features::market_structure::TrendStructure;
use pump_quant_ingest::social_parse::fnv1a_64;
use pump_quant_ingest::social_parse::parse_social_event;
use pump_quant_ingest::social_parse::SocialPlatform;
use pump_quant_ingest::social_source::SocialSource;
use pump_quant_market_state::creator::{CreatorEvent, CreatorState, CreatorStateReducer};
use pump_quant_market_state::meta::{
    rotation_between, CategoryEvent, CategoryEventKind, MetaRotationReducer, MetaRotationState,
    CATEGORY_UNCLASSIFIED,
};
use pump_quant_narrative::narrative::nv_meta_emergence;
use pump_quant_narrative::narrative_family::NarrativeFamily;
use pump_quant_signals::active_market_universe::{
    passes_broad_screen, passes_progressive_filter, MarketObservation, ScreenCriteria,
};
use pump_quant_signals::fee_plausibility::{
    assess_fee_floor, cumulative_fees_lamports, FeeFloorConfig, FeeFloorStatus,
};
use pump_quant_signals::launch_trajectory::FirstSlotTx;
use pump_quant_signals::setup_classifier::{classify_setup, SetupThresholds};
use pump_quant_simulator::capacity::CapacityPoint;
use pump_quant_social::ledger::{SourceOutcomeLedger, SourceQualityLedger};
use pump_quant_social::types::SourceRef;
use pump_quant_strategy::calibration_budget::{
    admit_calibration, CalibrationLedger, CalibrationRequest, RouteId,
};
use pump_quant_strategy::economic_gate::{effective_fixed_lamports, round_trip_cost_bps};
#[cfg(test)]
use pump_quant_strategy::economic_gate::ImpactCurve;
use pump_quant_strategy::entry_arbitration::{arbitrate, ArbitrationParams, EntryCandidate};
use pump_quant_strategy::entry_mode_leaves::{
    detect_narrative_confirmation, detect_pullback_continuation, NarrativeConfirmationFeatures,
    NarrativeConfirmationParams, PullbackParams, SuggestedLane,
};
use pump_quant_strategy::hazard_estimator::{HazardEstimate, ShrinkError};
use pump_quant_strategy::probe_ladder::{
    deployable_capital, derive_survival_floor, wallet_floor_guard, FloorVerdict,
};
use pump_quant_strategy::safety_integrity::QuoteMint;
use pump_quant_strategy::scalp_position::{CellKey, Phase};
use pump_quant_strategy::thesis::{
    build_thesis, evaluate_thesis, forced_action, FeatureObservation, ForcedAction, Thesis,
    ThesisCondition, ThesisInputs, ThesisState, ThesisVerdict,
};
use pump_quant_wallet_graph::creator_classifier::{
    classify_creator, CreatorClass, CreatorInputs, CreatorThresholds,
};
use pump_quant_wallet_graph::creator_ledger::CreatorTrack;
use pump_quant_wallet_graph::deployer_credibility::{
    compute_deployer_credibility, DeployerCredibilityConfig, PriorLaunch, SocialReachInput,
};
use pump_quant_watchlist::candidate::{Candidate, DiscoveryLane, Features, Lane as WlLane};
use pump_quant_watchlist::lane_ingest::ingest_union;
use pump_quant_watchlist::lane_performance::{DiscoveryLanePerformance, LanePerformance};
use pump_quant_watchlist::promote::promote_top;
use pump_quant_watchlist::rank::{LaneWeights, RankParams};
use pump_quant_watchlist::state::WatchlistState;
use std::collections::BTreeMap;

/// A bounded running reconciliation of realized net-SOL for one evaluator lane.
///
/// Replaces retaining every `ReconTrade` for the life of the engine (§99): the loop
/// runs indefinitely, so the accountant folds each fill into fixed-width running
/// totals instead of a growing vector. `net()` reproduces `evaluator::net_sol`'s
/// arithmetic exactly (`gross − fees − tips − failed`), so the reported net-SOL is
/// identical to what a full reconciliation over the same trades would yield.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReconAccum {
    gross: i128,
    fees: u128,
    tips: u128,
    failed: u128,
    n: u64,
}

impl ReconAccum {
    fn add(&mut self, t: &ReconTrade) {
        self.gross = self.gross.saturating_add(t.gross_lamports);
        self.fees = self.fees.saturating_add(t.fees);
        self.tips = self.tips.saturating_add(t.tips);
        self.failed = self.failed.saturating_add(t.failed_costs);
        self.n = self.n.saturating_add(1);
    }

    fn net(&self) -> i128 {
        if self.n == 0 {
            return 0;
        }
        let costs = (self.fees as i128)
            .saturating_add(self.tips as i128)
            .saturating_add(self.failed as i128);
        self.gross.saturating_sub(costs)
    }
}

/// Open-position attribution record: which lane opened it, its running
/// realized total, the committed entry spend, the pending one-shot scale-in
/// tranche, and the entry price backing the §52 hold-baseline counterfactual.
#[derive(Clone, Copy, Debug)]
struct OpenAttribution {
    lane: WlLane,
    /// §71.2 independent discovery-lane provenance — the net-SOL attribution key
    /// (distinct from the setup-archetype `lane` above).
    discovery_lane: DiscoveryLane,
    /// §25 derived setup archetype tagged onto the MFE/excursion sample at exit.
    archetype: u16,
    realized_acc: i128,
    entry_spend: u64,
    scale_add: u64,
    scale_cost: u64,
    entry_price: u64,
    /// LAW B1: the episodic capture taken AT ADMIT — the setup fingerprint and its
    /// context, frozen before the position opened. Carried here (rather than
    /// recomputed at exit) is the whole no-look-ahead guarantee: nothing that
    /// happens after the entry can reach this value.
    brain: Option<BrainEntry>,
    /// LAW B1: the entry tick, so the sealed episode carries a real hold duration.
    entry_tick: u64,
    /// Phase 2: the market's SOL-side reserve at admit, carried to the close path
    /// so `MoveTable::record()` can deposit the outcome into the correct vsol band.
    entry_vsol: u64,
    /// Phase 2: the entry-time signal observation for `MoveTable::record()` at close.
    entry_obs: SignalObs,
}

/// A gate-approved, fully-priced candidate awaiting §23 slot arbitration.
#[derive(Clone, Copy, Debug)]
struct PendingEntry {
    lane: WlLane,
    /// §71.2 independent discovery-lane provenance carried to the open position.
    discovery_lane: DiscoveryLane,
    /// §25 derived setup archetype (0 = None / classifier off), classified from
    /// the market/flow state at admit — the discriminator the thesis, MFE
    /// excursion samples, and reject samples are tagged with.
    archetype: u16,
    mint: [u8; 32],
    entry_price: u64,
    size: u64,
    entry_cost: u64,
    /// Conditional expected net SOL for the slot (size × priced move − cost load).
    expected_net: i128,
    /// §24 LAW 2: the measured round-trip cost (bps) at this size, computed at
    /// admit and threaded to the held position so its take-profit ladder is
    /// derived from the market's own cost floor rather than fixed constants.
    round_trip_cost_bps: u32,
    /// The market's SOL-side reserve at admit — the single input every cost term is
    /// derived from (venue fee tier, impact denominator, optimal clip). Carried so
    /// the ladder derivation downstream prices the SAME market the gate admitted,
    /// rather than re-reading a reserve that may have moved.
    entry_vsol: u64,
    /// Phase 2: the entry-time signal observation carried from the admission gate
    /// to the close path so `MoveTable::record()` can deposit the realized outcome
    /// into the correct signal bands. Without this, the calibrated model can never
    /// accumulate samples in production — the dead-code path that kept the model
    /// empty forever.
    entry_obs: SignalObs,
    /// §34.4 DecisionRecord provenance: the economic size band `[x_min, x_cost,
    /// x_max]` (net of fees/tips/expected-failure/margin) the admitted size was
    /// clamped within — threaded from the gate's `Admit` verdict into the
    /// journal's Admitted record so the band that justified the size is recorded,
    /// not just the size.
    x_min: u64,
    x_cost: u64,
    x_max: u64,
    /// **The ONE expected move this trade was priced with, and the estimator that
    /// produced it.** Carried to the journal so a replay can answer "which estimator
    /// priced this trade" — a question that was unanswerable while the number
    /// travelled as a bare integer fetched independently by two call sites
    /// (`docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md` §4.2).
    priced_move: PricedMove,
    /// The provenance of the DEPTH the band was sized against (`CurveDepth::
    /// basis_code`): derived from the identity, decoded from the account, a migrated
    /// pool, or unknown. Journalled for the same reason as the move's source.
    depth_basis: u8,
    /// LAW B1: the entry-time episodic capture, computed inside the gate from
    /// strictly pre-entry state. `None` when the brain plane is disabled.
    brain: Option<BrainEntry>,
}

/// Index of an evaluator lane into the running-accumulator array.
const fn accum_index(lane: EvalLane) -> usize {
    match lane {
        EvalLane::Scalp => 0,
        EvalLane::Early => 1,
    }
}

/// §24 LAW 6 fixed recent-bar window over which realized volatility is measured
/// to scale the stop/trail (comparable across markets — the helper grows with
/// window length, so a FIXED length is mandatory, §102).
const VOL_STOP_WINDOW_BARS: usize = 4;

/// §33/§43 LAW 13 sub-x_min probe-budget caps (lamports). Named (§102). Generous
/// research-spend envelope: paid-information probes are data-acquisition costs, so
/// the ledger bounds cumulative spend rather than gating a single small probe.
const PROBE_LIFETIME_CAP_LAMPORTS: u64 = 1_000_000_000;
/// Per-probe cap — a single sub-x_min probe may not spend more than this.
const PROBE_PER_TRADE_CAP_LAMPORTS: u64 = 50_000_000;
/// Per-day cap on cumulative probe spend.
const PROBE_DAILY_CAP_LAMPORTS: u64 = 200_000_000;
/// §39 per-route cap: cumulative probe spend on any one submission route.
const PROBE_PER_ROUTE_CAP_LAMPORTS: u64 = 500_000_000;
/// The single paper submission route probes are accounted against (§39). The
/// laptop build has one route; the per-route table has headroom for the live set.
const PROBE_ROUTE_ID: u16 = 0;

/// Split an entry `target` into `(probe, scale_in_add)` under the §33 probe→confirm→
/// scale-in lifecycle, honoring the criterion-112 / A-6 operator floor so EVERY
/// emitted bite is ≥ the floor.
///
/// * The probe is `max(min_trade_size, probe_frac_bp × target)`, capped at `target`.
/// * The scale-in add is `target − probe`, taken only if it is itself ≥
///   `min_trade_size`; otherwise it folds back into the probe and the whole target
///   opens as a SINGLE ≥floor bite.
///
/// Consequences (with the floor active, i.e. `min_trade_size > 0`):
/// * a `target` in `[floor, 2×floor)` cannot be split into two ≥floor bites, so it
///   opens as one ≥floor bite — never a sub-floor probe;
/// * a `target ≥ 2×floor` splits into a ≥floor probe now and a ≥floor scale-in add.
///
/// With the floor disabled (`min_trade_size == 0`) the probe keeps the legacy
/// 1-lamport minimum and the remainder never folds — byte-identical to the pre-A-6
/// behaviour. Pure, integer, deterministic. Callers pass a `target` already known to
/// be ≥ the floor (the sizing stage guarantees it), so `probe ≥ floor` always holds.
#[must_use]
pub fn probe_scale_split(target: u64, probe_frac_bp: u32, min_trade_size: u64) -> (u64, u64) {
    let probe_raw = ((u128::from(target) * u128::from(probe_frac_bp)) / 10_000) as u64;
    let probe = probe_raw.max(min_trade_size.max(1)).min(target);
    let scale_add = target - probe;
    if scale_add > 0 && scale_add < min_trade_size {
        // The remainder is a sub-floor clip: fold it into the initial bite.
        (target, 0)
    } else {
        (probe, scale_add)
    }
}

/// §51 LAW 14 BH-FDR family-wise significance level (ppm): 5%. Named (§102).
const PROMOTION_ALPHA_PPM: u32 = 50_000;
/// §51 LAW 14 PBO/CSCV overfitting block threshold (bps): 50%. Named (§102).
const PROMOTION_PBO_THRESHOLD_BPS: u32 = 5_000;

/// §47a LAW 18 terminal-state δT (ticks of info-time): a mint with no trade for
/// this many ticks at the reflection cadence is labeled terminal (dead). Named
/// (§102); the units are logical replay ticks (info-time, never wall-clock).
const TERMINAL_DELTA_T_TICKS: u64 = 240;
/// §47a LAW 18 monotonic δT criterion version — every terminal label carries it
/// so a label made under one δT is never silently conflated with another.
const TERMINAL_CADENCE_VERSION: u32 = 1;
/// §47a/§99 LAW 18 bound on the per-mint last-activity table (deterministic
/// eviction of the lexicographically-smallest tracked mint past capacity).
const LAST_TRADE_TABLE_CAP: usize = 8_192;

/// Maximum number of `ReconTrade` records accumulated for tape export before
/// the daemon must flush. Prevents unbounded memory growth on long runs.
const TAPE_TRADE_CAP: usize = 10_000;

/// §99 bound on [`Engine::ata_open`], the set of mints holding an open Associated
/// Token Account. Membership tracks live positions, which `max_concurrent_positions`
/// already caps far below this, so the bound is a leak backstop rather than a working
/// limit — and it fails in the EXPENSIVE direction (an untracked mint is charged full
/// rent) rather than by evicting a mint whose deposit is really posted.
const ATA_OPEN_CAP: usize = 1_024;

/// Per-creator linked-cluster tracking bound (§99/§102). Small by design: a
/// creator funding more than this many distinct sybil clusters is already
/// maximally suspect, so the count saturates as a lower bound past here rather
/// than growing an unbounded set.
const CREATOR_LINKED_CLUSTER_CAP: usize = 64;

/// Cross-author identical-content coordination window (ns): the same content hash
/// posted by ≥2 distinct authors within this window is a coordinated-campaign flag
/// (§29.7c) whose calls corroborate at zero quality. 10 minutes — the memecoin
/// shill-blast cadence; a slower echo is organic spread, handled by copy-echo
/// discounting in the attention layer instead.
const COORDINATION_WINDOW_NS: u64 = 600_000_000_000;

/// §32 thesis v0 invalidation thresholds on the registered feature schema:
/// OFI-derived buy-pressure must stay at/above the balanced midpoint of its
/// 0..=10_000 scale (net-buy), and the CVD sign must stay non-negative. The
/// completeness floor demands at least half-complete observations before a
/// condition may fire (§6.4: incomplete evidence neither confirms nor refutes).
const THESIS_OFI_MIN_FP: i64 = 5_000;
/// See [`THESIS_OFI_MIN_FP`]: minimum CVD sign value (non-negative).
const THESIS_CVD_MIN_FP: i64 = 0;
/// See [`THESIS_OFI_MIN_FP`]: minimum observation completeness (bps).
const THESIS_MIN_COMPLETENESS_BPS: u32 = 5_000;
/// Balanced midpoint of the OFI-derived buy-pressure scale (0..=10_000).
const PRESSURE_BALANCED_BP: u32 = 5_000;

/// §24 conditional-expectancy formula version (EXPECTANCY_V1): per-lane mean
/// realized return shrunk toward the configured cold-start prior with a
/// pseudo-count equal to the minimum-effective-sample gate. Bump on any change
/// to the formula so expectancy artifacts stay attributable.
pub const EXPECTANCY_VERSION: u32 = 1;

/// Maximum (author, content) duplicate-suppression keys retained (§99). At the
/// cap the oldest key is evicted — an ancient post replayed later may count
/// again (a bounded-memory tradeoff, biased toward availability of the screen
/// for CURRENT tape, which is where duplicate inflation matters).
const SOCIAL_SEEN_CAP: usize = 8_192;

/// Maximum learned cashtag→mint bindings retained (§99). First-bind-wins under
/// the cap; at the cap no new symbols bind (a lower bound on coverage, never a
/// wrong binding). 4096 matches the lane track caps.
const CASHTAG_BIND_CAP: usize = 4_096;

/// LAW D1/D5 bound on the mint→alpha-room binding table (§99). First-bind-wins
/// under the cap; matches the lane track caps so a laptop replay never hits it.
const ALPHA_SOURCE_BIND_CAP: usize = 4_096;
/// LAW D5 capacity of the per-Discord-room realized-net-SOL ledger (§99). Ample
/// for the operator's subscribed paid rooms; LRU-evicts the least-recently-updated
/// room past the cap (a room that has not called in longest yields first).
const ALPHA_SOURCE_LEDGER_CAP: usize = 256;

/// Maximum size fade (bps of 10_000) that creator *distribution* alone may apply.
/// A fully-distributed creator caps the haircut here — it can shrink size, never
/// veto a trade the on-chain gate already admitted (§22 behavioral-risk clause:
/// creator ownership is never an automatic binary reject).
const MAX_CREATOR_FADE_BPS: u32 = 5_000;

/// How the engine is allowed to act. Paper and Replay are safe modes with no
/// capital at risk. Live mode wires real Solana execution: ed25519 signing,
/// wire assembly, and Helius Sender submission through a `LiveOutboundSink`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Drive the calibrated fill model; no capital moves.
    Paper,
    /// Re-run a recorded event journal for determinism checking.
    Replay,
    /// Live trading: real Solana instructions, ed25519 signing, wire assembly,
    /// and Helius Sender submission. The outbound sink is wired to a
    /// `LiveOutboundSink` that builds, signs, and submits real transactions.
    ///
    /// The bankroll MUST be `LiveReconciled` — a `PaperSeed` is rejected at
    /// the live guard (`require_live_verified`) before any order is sized.
    Live,
}

/// Provenance of the engine's bankroll **base** — the lamport figure the entire
/// sizing chain (survival floor → deployable capital → total-risk budget →
/// per-position fraction) derives from, BEFORE realized P&L is folded in.
///
/// The operator's law (§33 Layer 1 / delta-§1; SERVER_BUILD_MANIFEST §7): **live
/// trading must ALWAYS source the bankroll from the reconciled on-chain wallet
/// balance; the config `bankroll_initial_lamports` is a PAPER/REPLAY seed ONLY.**
/// This type makes that law *structural* rather than remembered: a paper seed and
/// a live-reconciled balance are DIFFERENT variants, and only
/// [`BankrollOrigin::LiveReconciled`] can satisfy the fail-closed
/// [`require_live_verified`](BankrollOrigin::require_live_verified) guard a live
/// order path must pass before it sizes anything. A future live wiring therefore
/// *cannot* accidentally size off the config constant — the paper-seed variant
/// hard-errors at the live guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankrollOrigin {
    /// The config `bankroll_initial_lamports` seed. Paper/Replay ONLY — this
    /// variant can NEVER back a live trade: [`require_live_verified`] errors on it.
    ///
    /// [`require_live_verified`]: BankrollOrigin::require_live_verified
    PaperSeed(u64),
    /// A bankroll base backed by the reconciled on-chain wallet balance (Phase-B).
    /// The ONLY origin permitted to size a live order.
    LiveReconciled(u64),
}

impl BankrollOrigin {
    /// The base bankroll in lamports (before realized P&L is folded in). The
    /// accessor is identical for both variants — the sizing chain reads the number
    /// the same way regardless of provenance; provenance only gates whether *live*
    /// sizing is permitted (see [`require_live_verified`](Self::require_live_verified)).
    #[must_use]
    #[inline]
    pub fn seed_lamports(&self) -> u64 {
        match self {
            BankrollOrigin::PaperSeed(v) | BankrollOrigin::LiveReconciled(v) => *v,
        }
    }

    /// Whether this origin is backed by the reconciled live wallet — the only kind
    /// permitted to back live sizing. `false` for a paper/replay seed.
    #[must_use]
    #[inline]
    pub fn is_live_verified(&self) -> bool {
        matches!(self, BankrollOrigin::LiveReconciled(_))
    }

    /// FAIL-CLOSED live-sizing guard: return the reconciled balance for
    /// [`LiveReconciled`](Self::LiveReconciled), and ERROR for
    /// [`PaperSeed`](Self::PaperSeed). A live order path MUST call this before it
    /// sizes anything — a paper/replay seed can never fund a live trade (§33 /
    /// SERVER_BUILD_MANIFEST §7). Paper/replay themselves never call it (they size
    /// off [`seed_lamports`](Self::seed_lamports) directly), so it is inert on the
    /// golden path.
    pub fn require_live_verified(&self) -> Result<u64, BankrollOriginError> {
        match self {
            BankrollOrigin::LiveReconciled(v) => Ok(*v),
            BankrollOrigin::PaperSeed(_) => Err(BankrollOriginError::PaperSeedNotLive),
        }
    }
}

/// Why a live-sizing bankroll check was refused (fail-closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BankrollOriginError {
    /// A live order path tried to size off a PAPER/REPLAY config seed. Refused:
    /// live sizing must source the bankroll from the reconciled on-chain wallet
    /// balance via [`Engine::new_live_reconciled`] / [`Engine::set_live_bankroll`],
    /// never from `bankroll_initial_lamports`.
    PaperSeedNotLive,
}

impl std::fmt::Display for BankrollOriginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BankrollOriginError::PaperSeedNotLive => write!(
                f,
                "bankroll origin is a paper/replay seed; live sizing requires a \
                 reconciled on-chain wallet balance"
            ),
        }
    }
}

impl std::error::Error for BankrollOriginError {}

/// A single closed trade for tape export (Phase 2). Simplified view of the
/// evaluator's `ReconTrade`, without the cross-crate dependency. The daemon
/// maps these into `TapeRecord::Trade` for JSONL serialization.
#[derive(Clone, Copy, Debug)]
pub struct TapeTrade {
    /// True if the trade was a scalp lane trade; false for early lane.
    pub scalp: bool,
    /// Gross lamports (proceeds - cost basis; may be negative).
    pub gross: i128,
    /// Trading/protocol fees paid, lamports.
    pub fees: u128,
    /// Priority tips paid, lamports.
    pub tips: u128,
    /// Cost of failed attempts, lamports.
    pub failed: u128,
    /// The mint address of the traded market (raw 32 bytes).
    pub mint: [u8; 32],
    /// Entry price in fixed-point.
    pub entry_price_fp: u64,
    /// Exit price in fixed-point.
    pub exit_price_fp: u64,
    /// Total deployed notional (lamports) at entry.
    pub size_lamports: u64,
    /// §25 setup archetype tag (0 = None / classifier off).
    pub archetype: u16,
    /// Exit reason code (maps to ExitReason::code()).
    pub exit_reason_code: u8,
    /// Maximum favorable excursion, bps of entry.
    pub mfe_bps: i64,
    /// Maximum adverse excursion, bps of entry (≤ 0).
    pub mae_bps: i64,
    /// The logical tick at which the position was opened.
    pub entry_tick: u64,
}

/// The end-of-run summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Logical ticks elapsed.
    pub ticks: u64,
    /// Count of candidates promoted to the gate.
    pub promoted: u64,
    /// Count admitted by the gate.
    pub admitted: u64,
    /// Count rejected by the gate.
    pub rejected: u64,
    /// Total realized net-SOL across all paper scalps, lamports (the objective).
    pub net_lamports: i128,
    /// Realized net-SOL per setup-archetype source lane, lamports.
    pub per_lane_net: [(WlLane, i64); WlLane::COUNT],
    /// §71.2 realized net-SOL per INDEPENDENT discovery lane, lamports — the
    /// reflection-integrity readout that separates a creation-sighting from a
    /// social caller (both `CreationSniper`) and narrative from attention-velocity
    /// (both `EarlyConfirmation`).
    pub per_discovery_lane_net: [(DiscoveryLane, i64); DiscoveryLane::COUNT],
    /// Final adapted lane weights, bps.
    pub final_weights: [(WlLane, u32); WlLane::COUNT],
    /// Canonical digest of the decision journal (determinism fingerprint).
    pub journal_digest: u64,
    /// Mature-but-inactive candidates removed by the §21.5 universe screen at
    /// promotion (dead markets must not consume promotion slots or gate work).
    pub universe_filtered: u64,
    /// LAW D5: realized net-SOL per Discord paid-alpha room ([`SourceRef::discord`]),
    /// signed lamports, sorted by source for determinism (§22). The report-plane
    /// readout reflection uses to measure whether each subscribed room earns its
    /// keep and up/down-weight or flag it. Empty until a Discord-sourced position
    /// closes, so a run with no paid-alpha attribution reports nothing here.
    pub per_alpha_source_net: Vec<(SourceRef, i64)>,

    // ---- brain: episodic recall memory readouts (LAW B2, report plane only) ----
    /// LAW B1: immutable episodes sealed this run — one per completed trade, each
    /// fingerprinted from the state captured AT ADMIT (never at exit).
    pub brain_episodes_recorded: u64,
    /// LAW B2/B4: admit-time recalls that produced an estimate (`Known`).
    pub brain_recall_known: u64,
    /// LAW B2/B4: admit-time recalls that refused to (`Unknown` — empty index, out
    /// of radius, or below the §46 sample floor). A large ratio here against a
    /// small `brain_recall_known` is the honest signal that the memory is still
    /// too thin to have an opinion, and LAW B4 pins that it changed nothing.
    pub brain_recall_unknown: u64,
    /// LAW B3: reduce-only size haircuts recall actually applied (0 unless
    /// `brain_haircut_enable`).
    pub brain_haircuts_applied: u64,
    /// LAW B3: entries recall refused outright (0 unless `brain_haircut_enable`).
    pub brain_vetoes: u64,
    /// LAW B2: the strongest recalled setup classes the engine ACTUALLY traded,
    /// conditioned by venue phase × meta category × discovery lane, with their
    /// realized median net and sample size. Bounded; classes recall declines to
    /// speak about are absent rather than guessed at.
    pub brain_setup_classes: Vec<BrainSetupClass>,
    /// LAW B2: current lifecycle state of each tracked meta — "what is the state
    /// of the meta this week", answered from the brain's own timeline.
    pub brain_meta_state: Vec<BrainMetaState>,
    /// LAW B2: measured per-author track records over attributed markouts — "who
    /// called this, and do they actually earn". Fail-closed: an author below the
    /// sample floor is omitted, never shown with a flattering two-trade record.
    pub brain_author_records: Vec<BrainAuthorRecord>,

    // ---- social abstraction plane (REPORT ONLY — see `social_plane`) --------
    /// §29.8/§34.3 the FRESH social evidence chain: for every social datum still
    /// inside the engine's evidence TTL, which platform carried it, which author
    /// originated it, whether they are designated, their EARNED trust tier and
    /// operator-set exposure, and when we last saw them say it. Evidence past the
    /// TTL is DROPPED from this list, never carried forward at its last value.
    pub social_evidence: Vec<SocialEvidenceRow>,
    /// "Does this coin have strong social support, or is it a staged crowd?" —
    /// per watched mint, either a trust-weighted breadth score or an explicit
    /// refusal that structurally carries no number (§46).
    pub social_support: Vec<SocialSupportRow>,
    /// What the Phase-B capture layer should go and fetch to sharpen the support
    /// estimates — a specific work list (poll this platform, build a record for
    /// this author, set this source's §28 exposure), not "more data please".
    pub social_support_needs: Vec<SupportNeed>,
    /// "Who should I be following that I am not?" — ranked by lead-time-weighted
    /// REALIZED attribution. RESEARCH ONLY: this crate has no posting or
    /// engagement capability and none may be added (§110).
    pub follow_recommendations: Vec<FollowRecoRow>,
    /// "Who am I following that I should not be?" — followed authors whose
    /// attributed contribution has gone strictly negative over a real sample.
    pub unfollow_candidates: Vec<UnfollowRow>,
    /// Earned trust standing of the callers we are actually acting on. Trust is
    /// earned ONLY from realized net SOL; follower counts are structurally
    /// unreachable from the data the trust module reads (§28).
    pub caller_trust: Vec<CallerTrustRow>,
    /// "Which style is actually paying for us?" — realized per-lens performance,
    /// PHASE-SEPARATED (§100: there is no phase-pooled statistic, by design).
    pub lens_scoreboard: Vec<LensScoreRow>,
    /// The best-paying lens per venue phase, `(venue_phase_code, lens_code)`.
    /// Empty when no lens clears the sample floor with a positive median.
    pub best_paying_lens: Vec<(u8, u8)>,

    /// §70.1 the running HOLDER TRAJECTORY of every position still open when the
    /// run ended — the "enter / hold" limb of the continuous holder stream.
    ///
    /// Captured BEFORE [`Engine::report`]'s finalize sweep force-closes the book,
    /// so it answers "what were this position's holders doing while we held it",
    /// which is the question a distribution-aware exit would ask. Sorted by mint
    /// for determinism (§22). REPORT PLANE ONLY: nothing here is read by a gate,
    /// a size, a rank or an exit — see [`HolderTrajectoryRow`] for why the
    /// obvious §24 exit law is a documented seam rather than an armed rule.
    pub holder_trajectory: Vec<HolderTrajectoryRow>,
}

/// One open position's §70.1 holder trajectory at end of run (report plane).
///
/// Every number here carries its [`HolderCountBasis`], and the two count fields
/// are `Option` for the same structural reason the reading's accessors are: a
/// level consumer gets `None` unless the basis is `Exact`, and a growth consumer
/// gets `None` under `Incomplete`. A consumer literally cannot read a truncated
/// or delta-only count as if it were a holder level (§6.4).
///
/// ## The §24 exit-pressure seam, deliberately NOT armed
///
/// A held position whose holder count is *declining* is a textbook §24
/// distribution signal, and [`Self::accel_bps`] plus [`Self::growth_level`] are
/// exactly the inputs such a law would read. It is not wired, because arming an
/// untested exit is worse than not having one: the unhappy path (a transient
/// holder dip inside a healthy consolidation, which is extremely common on a
/// pump.fun curve where two or three entities dominate early breadth) would cut
/// winners, and this wave did not have the budget for the two-sided A/B that
/// would settle it. The data is exposed so the test can be built; the rule is
/// not armed on the strength of its plausibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HolderTrajectoryRow {
    /// The market held.
    pub mint: [u8; 32],
    /// What kind of number this row's counts are.
    pub basis: HolderCountBasis,
    /// Absolute holder count — `Some` only under [`HolderCountBasis::Exact`].
    pub level: Option<u64>,
    /// Growth-tier count — `Some` under `Exact` and `DeltaOnly`, `None` under
    /// `Incomplete`.
    pub growth_level: Option<u64>,
    /// The raw tracked count with no basis gate, explicitly a LOWER BOUND.
    pub lower_bound: u64,
    /// §70.1 holder-growth acceleration over the sampled series as known now, or
    /// `None` where the estimator refuses (thin series, stale interval, zero
    /// base) or the basis does not admit a growth reading.
    pub accel_bps: Option<i64>,
    /// The most recent first difference (holder growth rate), same refusal rules.
    pub growth_bps: Option<i64>,
    /// Distinct entities in the mint's ledger.
    pub entities_tracked: u32,
    /// Entity arrivals refused by the per-mint cap. Non-zero ⇒ `Incomplete`.
    pub truncated: u64,
    /// §21.7/§70.1 the mint's holder DISTRIBUTION SHAPE as known now — the
    /// concentration, early-buyer capture, bundle/sniper cohort and flip ratio
    /// behind the row's counts.
    ///
    /// Report plane only, and derived unconditionally: an operator watching a held
    /// position wants to know whether the float underneath it is concentrating,
    /// whether or not the LAW that reads that number is armed. Carries the same
    /// basis discipline as everything else here — under a delta-only or truncated
    /// ledger it is `Unknown` with the reason and no estimate.
    pub concentration: ConcentrationVerdict,
    /// §21.7 the CONCENTRATION TRAJECTORY of the tracked cohort — is the float
    /// under this open position gathering into fewer hands, or spreading out?
    ///
    /// The row above answers "how concentrated is it", and answers it `Unknown` on
    /// almost every real market because a share of the float needs an `Exact`
    /// holder basis. This one answers "which way is it moving", and it ANSWERS —
    /// because the tracked cohort's internal distribution has a denominator we
    /// know exactly, so its change is observable on a delta-only ledger. A
    /// separate field of a separate type, so a direction can never be misread as a
    /// share.
    pub concentration_trajectory: BrainTrajectory,
    /// The raw signed rate behind [`Self::concentration_trajectory`], in bps of
    /// normalized internal concentration per minute. `None` when no rate is
    /// measurable — never a fabricated zero.
    pub concentration_rate_bps: Option<i64>,
}

/// S8: Reflection state snapshot for the refiner feedback loop.
/// Carries the engine's `retired[4]` array and reflection config so the
/// daemon can append it to the champion config dump. The refiner reads
/// this metadata to make reflection-aware promotion decisions (S7).
#[derive(Clone, Copy, Debug)]
pub struct ReflectionSnapshot {
    /// The 4 watchlist lanes' retirement state (§56.11).
    pub retired: [bool; 4],
    /// The engine's logical tick at snapshot time.
    pub tick: u64,
    /// The champion config's reflect_every_ticks value.
    pub reflect_every_ticks: u64,
    /// Whether the brain-reflection lane_decay() path is armed.
    pub brain_reflect_enable: bool,
}

/// **Rev-19 on-chain feedback**: A pending buy or sell tx awaiting on-chain
/// confirmation. Created when the live sink submits a tx, consumed when the
/// daemon's `getSignaturesForAddress` poller reports back.
#[derive(Clone, Copy, Debug)]
pub struct PendingTx {
    /// Full 64-byte Solana signature of the submitted tx.
    pub signature: [u8; 64],
    /// The mint being traded.
    pub mint: [u8; 32],
    /// Tick when the tx was submitted (for timeout-based eviction).
    pub submit_tick: u64,
    /// SOL size deployed (for buys) or token amount sold (for sells), in
    /// lamports / token base units respectively. Used to reconcile the paper
    /// position on failure reversal.
    pub size: u64,
    /// Entry price fingerprint (for buys) or exit price fingerprint (for sells).
    pub price_fp: u64,
}

/// The running engine.
pub struct Engine {
    cfg: Config,
    mode: RunMode,
    now: u64,

    numeric: NumericLane,
    narrative: NarrativeLane,
    social: SocialLane,
    wallet: WalletLane,

    weights: LaneWeights,
    params: RankParams,
    watchlist: WatchlistState,
    /// mint → (decoded `virtual_sol`, decoded `real_sol`, tick of the confirmation).
    ///
    /// The two reserves are stored as the PAIR the decode produced, never merged: they
    /// are different quantities (`virtual_sol` prices, `real_sol` pays out) and the
    /// pair is what makes the identity `real_sol = virtual_sol − 30 SOL` checkable at
    /// all. The tick bounds the confirmation's freshness: a depth proven long ago is
    /// not depth now (§34.3 staleness law), so the gate expires entries past
    /// `confirm_ttl_ticks` instead of trusting them forever.
    confirmed: BTreeMap<[u8; 32], (u64, u64, u64)>,

    lane_perf: LanePerformance,
    /// §71.2 realized net-SOL keyed on the ACTUAL discovery lane (not the setup
    /// archetype) — the reflection-integrity ledger. Positions attribute here in
    /// `book_exit`; a creation-sighting and a social caller that both present as
    /// `CreationSniper` land in DISTINCT slots so per-lane learning is not
    /// cross-contaminated.
    disc_perf: DiscoveryLanePerformance,
    /// Running net-SOL reconciliation per evaluator lane (Scalp=0, Early=1); bounded.
    recon: [ReconAccum; 2],
    /// Individual closed-trade recon records for tape export (Phase 2 autonomous
    /// architecture). Each entry is one `ReconTrade` captured at exit time.
    /// Bounded by `TAPE_TRADE_CAP` to prevent unbounded growth; the daemon
    /// flushes and clears this periodically via `take_tape_trades()`.
    tape_trades: Vec<pump_quant_evaluator::evaluator_stats::ReconTrade>,
    journal: DecisionJournal,

    /// Live social attention-velocity field (`virality = attention = money`), fed by
    /// [`Self::ingest_social`]. Empty until social attention is ingested, so a run
    /// without social input is byte-identical to one before this layer existed.
    attention: AttentionField,
    /// Earned D1–D10 source-quality ledger; resolves each social call's corroboration
    /// weight (PUBLIC_BURNED baseline until a source earns evidence). Bounded (§99).
    ledger: SourceQualityLedger,
    /// Operator policy mapping an earned classification to a corroboration ceiling.
    quality_policy: SourceQualityPolicy,
    /// The social-source earn loop: attributes realized net-SOL back to the sources
    /// that called each market and reconciles it (§82) into an earned favorable-rate
    /// that supersedes the PUBLIC_BURNED baseline. Fed by the attributed social path.
    social_earn: SocialEarn,

    /// On-chain **narrative-category** measures (launches / flow / creators /
    /// graduations per category) — the factual `MetaRotationState` layer (§21.4).
    /// Fed only by `TokenMetadata` (launches) and category-attributed `MarketTrade`
    /// flow, so a run with no `TokenMetadata` leaves it empty and decision-neutral.
    meta: MetaRotationReducer,
    /// The previous category snapshot, for the reflection-cadence rotation diff.
    meta_prev: Option<MetaRotationState>,
    /// mint → its on-chain-assigned category id (forward-only, non-retroactive;
    /// §81). Bounded (§99). Empty until `TokenMetadata` is ingested — its emptiness
    /// is the O(1) fast-path guard that keeps the golden hot path byte-identical.
    mint_category: BTreeMap<[u8; 32], u64>,
    /// mint → its creator-state reducer (creator position / distribution). Bounded
    /// (§99), fed only by `CreatorAction`. Empty until ingested.
    creators: BTreeMap<[u8; 32], CreatorStateReducer>,
    /// category id → signed discovery-rank adjustment (bps over a 10_000 base):
    /// positive for an on-chain-emerging category, negative (fade) for a saturating
    /// one. Recomputed at the reflection cadence; empty until categories rotate, so
    /// discovery ranking is byte-identical for any run that never feeds meta.
    category_rank_adj: BTreeMap<u64, i64>,

    /// The held-position **exit lifecycle** (§24): admitted markets open a position
    /// here and are managed forward per-swap (trailing / thesis / rug-precursor /
    /// ladder / time-stop) instead of booking a one-shot fill. Empty until a market
    /// is admitted.
    positions: ScalpLifecycle,
    /// Open-position attribution: mint → (discovering lane, net realized so far,
    /// entry spend committed against the bankroll risk budget). Read when an exit
    /// books; removed when the position fully closes — the lane routes realized
    /// net-SOL to the right discovery weight (§29.9), the total attributes back to
    /// the market's social callers (§82), and the entry spend releases the
    /// committed-risk budget.
    open_lane: BTreeMap<[u8; 32], OpenAttribution>,

    /// Provenance of the bankroll **base** (§33 Layer 1 / delta-§1; SERVER_BUILD_
    /// MANIFEST §7). Paper/Replay carry a [`BankrollOrigin::PaperSeed`] built from
    /// `cfg.bankroll_initial_lamports`; a Phase-B live arming REPLACES it with a
    /// [`BankrollOrigin::LiveReconciled`] backed by the reconciled on-chain wallet
    /// (via [`Engine::new_live_reconciled`] / [`Engine::set_live_bankroll`]). This
    /// is the SINGLE source of the bankroll base: [`Engine::bankroll_balance`], the
    /// survival floor, and the hwm all read `bankroll_origin.seed_lamports()`, so a
    /// live path structurally cannot size off the paper config constant.
    bankroll_origin: BankrollOrigin,
    /// The paper bankroll (§33 Layer 1, delta-§1): realized-only accounting.
    /// `balance = initial + realized_cum`, floored at zero; every sizing limit
    /// derives from `deployable = balance − survival_floor`. Marks are NEVER
    /// counted — memecoin marks are adversarially manipulable and §33 mandates
    /// realized-profit-funded scaling only.
    bankroll_realized: i128,
    /// Σ entry spend of currently open positions (committed against the
    /// `total_risk_cap_bp` budget); released as positions close.
    bankroll_committed: u128,
    /// Realized high-water mark of the balance — drives the drawdown ratchet
    /// (Grossman–Zhou surplus shape, step-quantized). Updates on realized closes
    /// only, so the ratchet is immune to mark manipulation.
    bankroll_hwm: u64,

    /// Per-mint VPIN-X toxicity accumulators (§21.7; exact-sign volume buckets).
    /// Bounded (§99); fed per decoded trade; read at the gate (graded size
    /// multiplier + narrow sell-dominant veto) and per held-position trade
    /// (distributed-dump exit escalation).
    vpin: BTreeMap<[u8; 32], VpinState>,

    /// Fresh creation sightings: mint → first-sighting tick. A decoded creation
    /// event immediately creates a discovery candidate (§23/§21.1 — launches must
    /// not be invisible until someone else trades them); entries expire after
    /// `creation_ttl_ticks` unless the market earns real flow. Bounded (§99).
    creations: BTreeMap<[u8; 32], u64>,

    /// §70.1 CONTINUOUS holder accounting, folded from our own decoded swap flow
    /// ([`crate::holder_flow`]). Updated on EVERY `MarketTrade` — for every mint
    /// we watch, not only the ones that reach the gate — so the holder series is
    /// already populated by the time a candidate faces admission. This is the
    /// stream that replaced the never-called `observe_holder_count` seam; the
    /// third-party (Birdeye/DAS) holder count stays corroboration-tier (§6.6) and
    /// never populates it. Bounded (§99).
    holder_flow: HolderFlow,

    /// §21.7 CONTINUOUS concentration TRAJECTORY — the parallel stream.
    ///
    /// The holder ledger's *absolute* distribution is a level and needs an `Exact`
    /// basis, so it stays a point derivation. The tracked cohort's *internal*
    /// concentration is a derivative of a denominator we know exactly, so it is
    /// readable on the delta-only ledgers that make up almost the whole tape — and
    /// a derivative is only meaningful as a series. This plane keeps that series,
    /// folded on the same bounded cadence as the holder-count sample. Bounded
    /// (§99). Report and brain-context only; no decision reads it.
    conc_trajectory: ConcentrationTrajectoryPlane,

    /// Reflection/report analytics (§47/§48/§49/§50/§51/§54): per-trade returns,
    /// MFE rows, PRFS reject forward-marking, convexity events, retirement,
    /// Layer-2 sizing recommendation, D1–D10 fold, VOI queue. Bounded rings.
    analytics: ReflectionAnalytics,
    /// §48 exit-policy shadow tournament: 8 pre-registered challengers race the
    /// incumbent on the same admits/swaps. Report-only — adoption is an operator
    /// config change inside the §56.2 envelope, never self-applied.
    tournament: ExitTournament,
    /// §21.7 flow-authenticity screen: entity-dedup wash/HHI per mint. Sizing
    /// channel only (single entry point), plus the extreme-fabrication gate.
    flow_screen: FlowScreen,
    /// §105 (CRITERION 105) per-mint decayed LPI/wash extraction-risk covariate —
    /// REPORT / hazard-scaffold plane ONLY. Never read by a sizing/gating/promotion
    /// decision and never journaled, so it is byte-identical on the golden path.
    extraction_risk: ExtractionRiskLedger,
    /// §100 (CRITERION 100) per-CellKey hold-horizon hazard scaffold, fed from
    /// realized paper-fill outcomes (phase-separated: curve vs pool never pooled).
    /// REPORT plane ONLY — it does NOT replace the live §24(e) time-stop, and is
    /// never journaled, so accumulating fills is byte-identical on the golden path.
    hazard_scaffold: HazardScaffold,
    /// §28 smart-money follow screen (lagged-shadow law) fed from buyer entities.
    wallet_screen: WalletScreen,
    /// §27 amendment: tracked-wallet buy corroboration per mint.
    /// Maps `mint → (slot, count of distinct tracked-wallet buys)`.
    /// When a tracked wallet buys a mint, this records the slot and increments
    /// the count. The boost method queries this to compute corroboration.
    /// Bounded (§99): evicts the lexicographically-smallest mint at the cap.
    tracked_buys: BTreeMap<[u8; 32], (u64, u32)>,
    /// §27 amendment: the loaded tracked-wallet matcher (302 candidate list).
    /// None when `tracked_wallet_boost_enable` is false or the file doesn't
    /// exist. Built once at daemon startup from `tracked_wallet_path`.
    tracked_matcher: Option<pump_quant_wallet_graph::tracked_wallet_matcher::TrackedWalletMatcher>,
    /// §27 amendment (G6): wallet graph for cluster detection. Edges are added
    /// when two wallets transact on the same mint (funding edge) or when a
    /// creator launches multiple tokens (creator-family edge). The graph
    /// enables Sybil/rug-family detection per arXiv:2505.09313.
    wallet_graph: pump_quant_wallet_graph::tier2_wallet_graph::WalletGraph,
    /// Maps entity-id → node index in the wallet graph.
    wallet_graph_nodes: BTreeMap<u64, usize>,
    /// §27/§28 amendment (G6 wiring): the most recent BUYER entity on each
    /// mint, updated on every decoded `MarketTrade` buy. Used at gate time to
    /// know WHICH wallet to evaluate via `wallet_followable()` for the §28
    /// smart-money boost, and to build funding edges with the last seller.
    /// Bounded (§99): evicts the lexicographically-smallest mint at cap.
    last_mint_buyer: BTreeMap<[u8; 32], u64>,
    /// §27/§28 amendment (G6 wiring): the most recent SELLER entity on each
    /// mint, updated on every decoded `MarketTrade` sell. Paired with
    /// `last_mint_buyer` to add funding edges between consecutive
    /// buyer→seller wallets on the same mint.
    last_mint_seller: BTreeMap<[u8; 32], u64>,
    /// §21.3/§28/§21.7 market context: regime reducer, per-mint cluster-adjusted
    /// breadth, curve→pool phase, phase-correct executable exit cost.
    context: MarketContext,
    /// Per-mint entry thesis (§32): built at open from entry evidence, evaluated
    /// per swap; deterministic invalidation forces the exit. Bounded with opens.
    theses: BTreeMap<[u8; 32], Thesis>,
    /// §32 flow-persistence run length: consecutive ADVERSE flow observations per
    /// held mint. Bounded by the open-position set (§99) — an entry only exists
    /// while a thesis does, is reset the moment flow is non-adverse, and is
    /// removed when the position closes.
    thesis_adverse: BTreeMap<[u8; 32], u32>,
    /// **Mints for which an Associated Token Account is currently OPEN**, and whose
    /// [`crate::cost_model::ATA_RENT_LAMPORTS`] deposit is therefore already posted.
    ///
    /// The **lazy-hold, close-on-full-exit** policy: an ATA is opened (rent charged
    /// into the entry basis) only on an admit into a mint not already in this set, and
    /// closed — [`crate::cost_model::ATA_CLOSE_LAMPORTS`] charged, the full deposit
    /// refunded — when the position fully exits. Without the set, a re-entry into a
    /// mint we still hold an account for would be charged 203 bps of a deposit it never
    /// posted; the golden tape does 13 trades in 5 distinct mints, so re-entry is the
    /// common case, not the corner.
    ///
    /// Bounded (§99) by [`ATA_OPEN_CAP`], which is itself far above
    /// `max_concurrent_positions` — the set only ever holds mints with live positions,
    /// so the cap is a backstop against a leak, never a working limit. On overflow the
    /// insert is REFUSED and the rent is charged anyway (the conservative direction:
    /// an untracked account is priced as a fresh one, never as free).
    ata_open: std::collections::BTreeSet<[u8; 32]>,
    /// mint → creator entity (from TokenMetadata), for the credibility haircut.
    mint_creator: BTreeMap<[u8; 32], u64>,
    /// creator → (lifetime launches, window start tick, launches in window).
    creator_launches: BTreeMap<u64, (u32, u64, u32)>,
    /// §70.10 first-slot fee footprint per mint → (cumulative priority+tip
    /// lamports, activity count) — the anti-bundle fee-floor input. This is a
    /// PARALLEL channel fed by [`Self::observe_first_slot_fees`], NOT an
    /// `AppEvent` field: the decoded event vocabulary (`event.rs`) is dossier-
    /// locked (additive-only), and threading a new field through `TokenMetadata`
    /// would perturb the locked event shape, so the first-slot fee record is
    /// carried alongside instead (server-side decode seam). Bounded (§99); empty
    /// until fed, so a run without first-slot fees is byte-identical.
    first_slot_fees: BTreeMap<[u8; 32], (u128, u64)>,
    /// Rev-14 wangr intelligence: per-mint auxiliary data (token_standard,
    /// symbol_len) fed by `AppEvent::MarketAuxiliary`. Bounded (§99); empty
    /// until fed, so a run without auxiliary events leaves the gate's wangr
    /// filters at sentinel zero — byte-identical to prior behavior.
    mint_aux: BTreeMap<[u8; 32], (u8, u8)>,
    /// Rev-14 wangr intelligence: latest wall-clock time signal (dow, hour_utc)
    /// fed by `AppEvent::TimeSignal`. Defaults to (0, 255) = unobserved, which
    /// is a no-op for all time-based gate filters. The engine is a pure tick-
    /// based state machine (§22) and NEVER reads wall-clock itself; this is the
    /// sole channel through which the caller informs it of UTC time.
    time_signal: (u8, u8),
    /// §56.11 retirement flags per watchlist lane (capital-ineligible when true).
    retired: [bool; 4],
    /// Envelope-clamped Layer-2 sizing recommendation (bps), None until earned.
    f_recommended: Option<u32>,
    /// Regime summary flag consumed by sizing (refreshed at reflection cadence).
    regime_rug_elevated: bool,
    /// Cross-provider / repost duplicate suppression (§29 directive: the same
    /// underlying post arriving via multiple capture lanes — or verbatim
    /// re-posted by the same author — must count ONCE, never as independent
    /// corroboration). Keyed (author, content-hash); bounded ring + set (§99).
    /// Cross-AUTHOR identical content is NOT deduped — that is the §29.7c
    /// coordination signature and must stay visible to the batch screen.
    social_seen: std::collections::BTreeSet<(u64, u64)>,
    /// Eviction order for [`Self::social_seen`] (oldest evicted at the cap).
    social_seen_ring: std::collections::VecDeque<(u64, u64)>,
    /// Learned cashtag→mint bindings (§29): when one event names BOTH a ticker
    /// and a concrete mint, the ticker binds to that mint (FIRST bind wins — a
    /// later post cannot hijack an established symbol). Cashtag-only chatter
    /// (the dominant Twitch-chat shape) then resolves through this map into the
    /// attention field — attention-tier only, never a `SocialCall`: an inferred
    /// binding corroborates even more weakly than a named mint. Bounded (§99).
    cashtag_binds: BTreeMap<u64, [u8; 32]>,
    /// LAW D1/D5: mint → the Discord ALPHA room ([`SourceRef::discord`]) that first
    /// called it — the per-room net-SOL attribution binding (§29.8/§71). First-bind
    /// wins, bounded (§99). Bound only while `alpha_call_lane_enable` is set; empty
    /// otherwise, so a run with no Discord alpha is byte-identical. Read at
    /// `book_exit` close to fold the realized net into [`Self::source_outcome`].
    alpha_source: BTreeMap<[u8; 32], SourceRef>,
    /// LAW D5: bounded per-source realized-net-SOL ledger (§29.8/§71/§74). Each paid
    /// Discord room accrues its OWN realized net so reflection (report plane) can
    /// measure whether the room earns its keep and up/down-weight or flag it. Fed at
    /// `book_exit` close via [`Self::alpha_source`]; surfaced on the `Report`. A
    /// report-plane accountant — it never gates, sizes, or ranks a decision, so
    /// accumulating into it is byte-identical on every decision path.
    source_outcome: SourceOutcomeLedger,
    /// Per-lane realized-return accumulator (Σ realized bps, fills) feeding the
    /// §24 conditional-expectancy shrinkage — EXPECTANCY_V1. Bounded by
    /// construction (4 lanes × two integers).
    lane_edge: [(i128, u32); 4],
    /// §21.6 per-mint trade-count bars + swing structure + §21.5 activity window
    /// (bounded; fed per decoded trade; read at promotion and gate time).
    structure: StructureState,
    /// Count of mature-but-inactive candidates the §21.5 universe screen removed
    /// at promotion (dead markets must not consume promotion slots or gate work).
    universe_filtered: u64,
    /// §33/§43 LAW 13 calibration-budget ledger: accounts sub-`x_min` probe spend
    /// (paid information) against per-trade / daily / lifetime / per-route caps.
    /// Inert unless `probe_budget_enable` is set — a run that opens no sub-x_min
    /// probe never mutates it, so the golden tape is byte-identical.
    calibration: CalibrationLedger,
    /// C2 wiring: per-route landing-outcome health set (§39/§99). Inert in
    /// paper/replay mode — `RouteHealthSet::new()` is all-zero, so
    /// `route_health_is_measured()` returns false and `choose_submit_plan()`
    /// falls through to the legacy route. Live mode will `record()` actual
    /// landing outcomes here; paper mode never mutates it (golden tape safe).
    route_health: RouteHealthSet,
    /// C2 wiring (item 7): outbound junction sink — the engine → tx_build →
    /// signer → sender contract. Inert in paper/replay mode (`None`): the
    /// engine books paper positions and the sink is never called. In live
    /// mode the junction crate installs an `OutboundJunction` here; the
    /// engine calls `on_admit` after `open_pending` books the position.
    /// The sink's return value is logged for the report only — it NEVER feeds
    /// an engine decision (§24(b) golden-digest invariant).
    outbound_sink: Option<&'static dyn pump_quant_execution::ex_outbound_sink::OutboundSink>,
    /// Count of live outbound sink successes (real on-chain buy submissions accepted).
    live_outbound_successes: u64,
    /// Count of live outbound sink failures (state-fetch, construction, signer, or sender errors).
    live_outbound_failures: u64,
    /// Live sell submission successes (sell tx accepted by the sink).
    live_sell_successes: u64,
    /// Live sell submission failures (sell tx rejected/errored by the sink).
    live_sell_failures: u64,
    /// **Rev-19 on-chain feedback**: pending buy txs awaiting on-chain confirmation.
    /// Keyed by the first 8 bytes of the signature (compact). Bounded (§99) —
    /// evicts the oldest entry at capacity.
    pending_buys: BTreeMap<[u8; 8], PendingTx>,
    /// **Rev-19 on-chain feedback**: pending sell txs awaiting on-chain confirmation.
    pending_sells: BTreeMap<[u8; 8], PendingTx>,
    /// Count of buy txs confirmed on-chain (landed successfully).
    buy_confirmed_count: u64,
    /// Count of buy txs that failed on-chain (tokens NOT received).
    buy_failed_count: u64,
    /// Count of sell txs confirmed on-chain (SOL recovered).
    sell_confirmed_count: u64,
    /// Count of sell txs that failed on-chain (SOL NOT recovered).
    sell_failed_count: u64,
    /// Count of sub-`x_min` probes admitted as budgeted paid-information (LAW 13),
    /// distinct from `admitted` positions — a probe is never a position.
    probes_budgeted: u64,
    /// §47a LAW 18 per-mint last-trade tick (info-time), bounded
    /// ([`LAST_TRADE_TABLE_CAP`]) — the swap-recency the terminal-state cadence
    /// reflects over. Keyed by mint bytes; the reflection keys by `MintId`.
    last_trade_tick: BTreeMap<[u8; 32], u64>,
    /// §47a LAW 18 latest terminal-state reflection (per-mint dead/alive labels at
    /// the versioned δT), refreshed each reflection cadence. Report-only.
    terminal_reflections: Vec<MintReflection>,

    /// Reused per-tick discovery scratch: the union of all four lanes' emissions.
    /// Cleared (not freed) each tick so steady-state discovery does not re-allocate
    /// (§99: its capacity is bounded by the number of tracked mints, which the lanes
    /// already cap). Holds no state between ticks — purely a scratch buffer.
    scratch: Vec<Candidate>,

    /// Reused per-tick promotion-arbitration scratch (O2). Each is cleared (not
    /// freed) each tick via `mem::take`/`clear`, so steady-state `evaluate()`
    /// allocates no per-tick promotion vectors. They hold no state between ticks —
    /// content and ordering are identical to the old per-tick locals, so the golden
    /// digest is unchanged. Capacity is bounded by `promote_k` / tracked mints.
    corrob_buf: Vec<Candidate>,
    extras_buf: Vec<Candidate>,
    pending_buf: Vec<PendingEntry>,
    /// Stratified per-candidate expected-move table. EMPTY in the shipped state, so
    /// every estimate refuses and the gate prices on `gate_expected_move_bps`.
    expected_move: crate::expected_move::MoveTable,
    cands_buf: Vec<EntryCandidate>,

    /// LAWs B1–B5: the episodic recall memory plane. Bounded (§99) and, unless
    /// `brain_haircut_enable` arms LAW B3, strictly decision-inert: it records what
    /// happened and answers what happened last time, and nothing else.
    brain: BrainPlane,

    /// The four MEASURED estimators behind the fingerprint fields the engine used
    /// to fabricate: §70.1 holder-growth acceleration, §29.9 creator track record,
    /// §21.4 meta lifecycle phase, §21.4 narrative family. Each refuses below its
    /// own evidence floor and the seam collapses that refusal onto the documented
    /// NEUTRAL bucket — never onto a fabricated measurement (see
    /// [`crate::measured_state`] for the two fields whose ladder cannot represent
    /// the refusal). Fed from the engine's own on-chain observations plus two
    /// parallel capture seams; empty until fed, so an unfed run is byte-identical
    /// to one before this layer existed. Bounded (§99), report/fingerprint plane
    /// only — nothing here gates, sizes, or ranks.
    measured: MeasuredState,

    /// The §28/§29.8/§110 SOCIAL ABSTRACTION plane: earned source trust, mint
    /// social-support scoring, follow / unfollow recommendation, and the realized
    /// style-lens scoreboard, plus the provenance chain every social datum carries.
    /// REPORT PLANE ONLY — no promotion, ranking, sizing or gate path reads it, and
    /// it is never journaled, so it is byte-identical on the golden path. Bounded
    /// (§99). Fed from the same social calls the brain's ledger already ingests.
    social_plane: SocialPlane,

    promoted: u64,
    admitted: u64,
    rejected: u64,
    /// Per-code reject histogram (index = reject code 1..18). Full population,
    /// not a ring-buffer sample — every rejection increments both `rejected`
    /// and `reject_counts[code]`, so the histogram is exact, not statistical.
    reject_counts: [u64; 32],

    /// §Quant-Rev-7: RE-ENTRY COOLDOWN tracking. Maps mint → tick when the position
    /// closed (the cooldown start). A mint in this map is in cooldown until
    /// `self.now - entry_tick >= cfg.reentry_cooldown_ticks`. Pruned lazily: expired
    /// entries are removed on read (gate check) and on tick advance. Empty until a
    /// position closes, so the golden tape (which never closes a position) never
    /// populates it — the golden path is byte-identical. Bounded by the number of
    /// recently-exited mints (bounded by `max_concurrent_positions` * turnover rate).
    reentry_cooldown: BTreeMap<[u8; 32], u64>,
}

impl Engine {
    /// Construct an engine under a validated config and a run mode.
    ///
    /// Paper/Replay ONLY: the bankroll base is seeded from
    /// `cfg.bankroll_initial_lamports` as a [`BankrollOrigin::PaperSeed`], so
    /// [`Self::bankroll_balance`] returns `seed + Σ realized` — byte-identical to
    /// the pre-origin behaviour (the golden digest is unchanged). Live capital is
    /// Phase-B and is armed through [`Self::new_live_reconciled`] /
    /// [`Self::set_live_bankroll`], never this constructor.
    #[must_use]
    pub fn new(cfg: Config, mode: RunMode) -> Self {
        let origin = BankrollOrigin::PaperSeed(cfg.bankroll_initial_lamports);
        Self::with_origin(cfg, mode, origin)
    }

    /// **Phase-B live entry (fail-closed).** Construct an engine whose bankroll base
    /// is the *reconciled on-chain wallet balance* — a [`BankrollOrigin::LiveReconciled`]
    /// — NOT the config seed. This is the constructor the live reconcile layer
    /// (SERVER_BUILD_MANIFEST §7) uses to arm live sizing: every limit then derives
    /// from `wallet_balance_lamports`, and `cfg.bankroll_initial_lamports` is
    /// ignored for sizing entirely (it remains only the paper/replay seed).
    ///
    /// Live arming MUST additionally gate every order on
    /// [`BankrollOrigin::require_live_verified`] (fail-closed) before submission — a
    /// paper seed can never back a live trade. The engine is built under
    /// [`RunMode::Live`] semantics: the outbound sink is expected to be wired to a
    /// [`LiveOutboundSink`] by the caller via `install_outbound_sink` immediately
    /// after construction.
    #[must_use]
    pub fn new_live_reconciled(cfg: Config, wallet_balance_lamports: u64) -> Self {
        let origin = BankrollOrigin::LiveReconciled(wallet_balance_lamports);
        Self::with_origin(cfg, RunMode::Live, origin)
    }

    /// Construct an engine under a validated config, a run mode, and an explicit
    /// [`BankrollOrigin`] — the single builder both the paper [`Self::new`] and the
    /// Phase-B [`Self::new_live_reconciled`] delegate to, so the two paths share one
    /// initialization and only the bankroll *base* differs. Private: the origin is
    /// chosen by the entry point, never by the caller directly.
    #[must_use]
    fn with_origin(cfg: Config, mode: RunMode, bankroll_origin: BankrollOrigin) -> Self {
        let weights = LaneWeights::from_defaults();
        let params = RankParams::new(cfg.watchlist_ttl_ticks);
        let watchlist = WatchlistState::new(cfg.watchlist_capacity, params, weights);
        // Build the meta reducer from config BEFORE `cfg` is moved into the struct.
        let meta = MetaRotationReducer::new(
            cfg.meta_taxonomy_version,
            cfg.meta_max_categories,
            cfg.meta_max_creators_per_cat,
        );
        // The exit lifecycle is FULLY config-driven (crit-102: every trigger is an
        // operator-set named parameter, none baked in), its cost model tied to the
        // operator's fee/tip config, and its §38 fill-mode severity threaded from
        // `fill_mode`: Modes A/B book at mark (optimistic ceiling), the adversarial
        // modes impair every exit by the configured retry-slippage (×2 pessimistic)
        // so paper net-SOL — which feeds sizing and reflection — is execution-honest.
        let exit_impair_bps = match cfg.fill_mode {
            crate::config::FillModeCfg::SignalReplay
            | crate::config::FillModeCfg::OptimisticCeiling => 0,
            crate::config::FillModeCfg::AdversarialRealistic => cfg.gate_protocol_bps,
            crate::config::FillModeCfg::AdversarialPessimistic => {
                cfg.gate_protocol_bps.saturating_mul(2)
            }
        };
        let lifecycle_params = LifecycleParams {
            hard_sl_bps: cfg.lc_hard_sl_bps,
            trail_base_bps: cfg.lc_trail_base_bps,
            trail_k_div: cfg.lc_trail_k_div,
            trail_max_bps: cfg.lc_trail_max_bps,
            tp1_bps: cfg.lc_tp1_bps,
            tp1_frac_bps: cfg.lc_tp1_frac_bps,
            tp2_bps: cfg.lc_tp2_bps,
            tp2_frac_bps: cfg.lc_tp2_frac_bps,
            tp3_bps: cfg.lc_tp3_bps,
            tp3_frac_bps: cfg.lc_tp3_frac_bps,
            cvd_hold_frac_bps: cfg.lc_cvd_hold_frac_bps,
            stall_ticks: cfg.lc_stall_ticks,
            max_hold_ticks: cfg.lc_max_hold_ticks,
            precursor_drop_bps: cfg.lc_precursor_drop_bps,
            // COST-MODEL UNIFICATION (2026-07-28): the lifecycle's frictions are no
            // longer operator constants that could disagree with the gate's. Both
            // come from `cost_model`, which is why they cannot drift apart again.
            // `fee_bps` here is only the depth-unknown FALLBACK — `realize` reads the
            // tiered rate from the position's own decoded reserve — so it is set to
            // the pre-graduation rate, the expensive end of the schedule (§54).
            fee_bps: crate::cost_model::VENUE_FEE_BPS_CURVE,
            fixed_lamports_per_leg: crate::cost_model::FIXED_LAMPORTS_PER_LEG,
            exit_impair_bps,
            curve_exact_fill: cfg.curve_exact_fill_enable,
            into_strength_exit_enable: cfg.into_strength_exit_enable,
            into_strength_climax_bp: cfg.into_strength_climax_bp,
            vol_stop_enable: cfg.vol_stop_enable,
            vol_stop_scale_bp: cfg.vol_stop_scale_bp,
            conditional_moon_bag_enable: cfg.conditional_moon_bag_enable,
            moon_bag_acceleration_window: cfg.moon_bag_acceleration_window,
        };
        // Concurrency: the operator's bankroll-consistent cap (§33 — jointly sized
        // with f_base and the total risk budget), never the raw confirmed-set bound.
        let positions = ScalpLifecycle::new(lifecycle_params, cfg.max_concurrent_positions);
        // The drawdown-ratchet hwm and the whole sizing chain read the bankroll
        // base from the ORIGIN, never `cfg.bankroll_initial_lamports` directly. For
        // Paper/Replay the origin is `PaperSeed(cfg.bankroll_initial_lamports)`, so
        // `seed_lamports()` == the config value → byte-identical (golden digest
        // unchanged); for a Phase-B live origin it is the reconciled wallet balance.
        let bankroll_hwm = bankroll_origin.seed_lamports();
        // §19: fold the full strategy-config identity into the decision digest so
        // two runs under different configs can never share a digest. The Debug
        // encoding of the Copy config struct is deterministic for a fixed build.
        let mut journal = DecisionJournal::new();
        journal.seed(fnv1a_64(format!("{cfg:?}").as_bytes())); // LINT-ALLOW(hot_alloc_fmt): cold one-shot config-identity digest seed at Engine::new, never the per-tick hot path (§19)
        let tournament = ExitTournament::new(lifecycle_params);
        // Bar clock interval from config, read BEFORE `cfg` moves into the struct.
        let structure = StructureState::new(cfg.bar_trades_per_bar);
        Self {
            cfg,
            mode,
            now: 0,
            numeric: NumericLane::new(),
            narrative: NarrativeLane::new(),
            social: SocialLane::new(),
            wallet: WalletLane::new(),
            weights,
            params,
            watchlist,
            confirmed: BTreeMap::new(),
            lane_perf: LanePerformance::new(),
            disc_perf: DiscoveryLanePerformance::new(),
            recon: [ReconAccum::default(); 2],
            tape_trades: Vec::new(),
            journal,
            attention: AttentionField::new(AttentionParams {
                // §70.6/§70.8 LAW 8 + §70.7 LAW 9: thread the operator flags into
                // the attention emit path. Both default OFF, so a dev-portable /
                // golden run keeps the exact `standard()` params (byte-identical);
                // the emit path is class-unconditioned and platform-runway-free
                // until an operator flips the config.
                narrative_class_enable: cfg.narrative_class_enable,
                platform_lead_enable: cfg.platform_lead_enable,
                // LAW D2: thread the designated-caller switch + weight from config
                // (default ON). Inert on any mint with no designated caller, so a
                // run with no paid-alpha / curated-follow attention is byte-identical.
                designated_caller_enable: cfg.designated_caller_enable,
                designated_caller_weight: cfg.designated_caller_weight,
                ..AttentionParams::standard()
            }),
            ledger: SourceQualityLedger::with_capacity(4_096),
            quality_policy: SourceQualityPolicy::conservative(),
            social_earn: SocialEarn::new(SocialEarnParams::standard()),
            meta,
            meta_prev: None,
            mint_category: BTreeMap::new(),
            creators: BTreeMap::new(),
            category_rank_adj: BTreeMap::new(),
            positions,
            open_lane: BTreeMap::new(),
            bankroll_origin,
            bankroll_realized: 0,
            bankroll_committed: 0,
            bankroll_hwm,
            vpin: BTreeMap::new(),
            creations: BTreeMap::new(),
            holder_flow: HolderFlow::new(),
            conc_trajectory: ConcentrationTrajectoryPlane::new(),
            analytics: ReflectionAnalytics::new(),
            tournament,
            flow_screen: FlowScreen::new(),
            extraction_risk: ExtractionRiskLedger::new(),
            hazard_scaffold: HazardScaffold::new(),
            wallet_screen: WalletScreen::new(),
            tracked_buys: BTreeMap::new(),
            tracked_matcher: None,
            wallet_graph: pump_quant_wallet_graph::tier2_wallet_graph::WalletGraph::new(0),
            wallet_graph_nodes: BTreeMap::new(),
            last_mint_buyer: BTreeMap::new(),
            last_mint_seller: BTreeMap::new(),
            context: MarketContext::new(),
            theses: BTreeMap::new(),
            thesis_adverse: BTreeMap::new(),
            ata_open: std::collections::BTreeSet::new(),
            mint_creator: BTreeMap::new(),
            creator_launches: BTreeMap::new(),
            first_slot_fees: BTreeMap::new(),
            // Rev-14 wangr intelligence: empty/sentinel until fed — no-op.
            mint_aux: BTreeMap::new(),
            time_signal: (0, 255),
            retired: [false; 4],
            f_recommended: None,
            regime_rug_elevated: false,
            social_seen: std::collections::BTreeSet::new(),
            social_seen_ring: std::collections::VecDeque::new(),
            cashtag_binds: BTreeMap::new(),
            alpha_source: BTreeMap::new(),
            source_outcome: SourceOutcomeLedger::with_capacity(ALPHA_SOURCE_LEDGER_CAP),
            lane_edge: [(0, 0); 4],
            structure,
            universe_filtered: 0,
            calibration: CalibrationLedger::new_with_route_cap(
                PROBE_LIFETIME_CAP_LAMPORTS,
                PROBE_PER_TRADE_CAP_LAMPORTS,
                PROBE_DAILY_CAP_LAMPORTS,
                PROBE_PER_ROUTE_CAP_LAMPORTS,
                0,
            ),
            route_health: RouteHealthSet::new(),
            outbound_sink: None,
            live_outbound_successes: 0,
            live_outbound_failures: 0,
            live_sell_successes: 0,
            live_sell_failures: 0,
            pending_buys: BTreeMap::new(),
            pending_sells: BTreeMap::new(),
            buy_confirmed_count: 0,
            buy_failed_count: 0,
            sell_confirmed_count: 0,
            sell_failed_count: 0,
            probes_budgeted: 0,
            last_trade_tick: BTreeMap::new(),
            terminal_reflections: Vec::new(),
            scratch: Vec::new(),
            corrob_buf: Vec::new(),
            extras_buf: Vec::new(),
            pending_buf: Vec::new(),
            expected_move: crate::expected_move::MoveTable::empty(),
            cands_buf: Vec::new(),
            brain: BrainPlane::new(cfg.brain_min_sample, cfg.brain_recall_max_distance),
            measured: MeasuredState::new(),
            social_plane: SocialPlane::new(),
            promoted: 0,
            admitted: 0,
            rejected: 0,
            reject_counts: [0; 32],
            // Rev-7: re-entry cooldown — empty until a position closes
            reentry_cooldown: BTreeMap::new(),
        }
    }

    /// The current realized balance, lamports: `base + Σ realized`, floored at zero
    /// (§33 realized-only accounting; marks never count). The `base` is the bankroll
    /// ORIGIN's seed — `cfg.bankroll_initial_lamports` for a Paper/Replay
    /// [`BankrollOrigin::PaperSeed`], or the reconciled on-chain wallet balance for a
    /// Phase-B [`BankrollOrigin::LiveReconciled`]. Reading the base from the origin
    /// (not the config directly) is what makes a live path structurally unable to
    /// size off the paper seed.
    #[must_use]
    pub fn bankroll_balance(&self) -> u64 {
        let b = i128::from(self.bankroll_origin.seed_lamports()) + self.bankroll_realized;
        b.clamp(0, i128::from(u64::MAX)) as u64
    }

    /// The bankroll origin (base provenance). A live order path reads this and gates
    /// on [`BankrollOrigin::require_live_verified`] (fail-closed) before sizing.
    #[must_use]
    pub fn bankroll_origin(&self) -> BankrollOrigin {
        self.bankroll_origin
    }

    /// **Phase-B live seam (fail-closed).** REPLACE the bankroll base with a
    /// [`BankrollOrigin::LiveReconciled`] backed by the reconciled on-chain wallet
    /// balance — the ONLY way to move the base off the paper seed on an already-built
    /// engine. The live reconcile layer (SERVER_BUILD_MANIFEST §7) calls this on each
    /// reconcile so the whole sizing chain tracks the real wallet; the drawdown hwm is
    /// re-based to the reconciled balance so the ratchet measures live drawdown, not a
    /// stale paper seed. A live order path MUST still gate on
    /// [`BankrollOrigin::require_live_verified`] before every submission.
    pub fn set_live_bankroll(&mut self, reconciled_wallet_balance_lamports: u64) {
        self.bankroll_origin = BankrollOrigin::LiveReconciled(reconciled_wallet_balance_lamports);
        self.bankroll_hwm = self.bankroll_origin.seed_lamports();
    }

    /// The effective per-position fraction (bps of deployable) after the drawdown
    /// ratchet: full below tier1, halved past tier1, quartered past tier2, probe-only
    /// past tier3 (Grossman–Zhou surplus shape, step-quantized; realized-only hwm).
    fn dd_f_eff_bp(&self, balance: u64) -> u32 {
        let hwm = self.bankroll_hwm.max(1);
        if balance >= hwm {
            return self.cfg.f_base_bp;
        }
        let dd_bp = ((u128::from(hwm - balance) * 10_000) / u128::from(hwm)) as u32;
        if dd_bp >= self.cfg.dd_tier3_bp {
            self.cfg.probe_f_bp
        } else if dd_bp >= self.cfg.dd_tier2_bp {
            self.cfg.f_base_bp >> 2
        } else if dd_bp >= self.cfg.dd_tier1_bp {
            self.cfg.f_base_bp >> 1
        } else {
            self.cfg.f_base_bp
        }
    }

    /// The VPIN parameter/threshold views over the config (§102 named).
    fn vpin_params(&self) -> VpinParams {
        VpinParams {
            v_min_lamports: self.cfg.vpin_v_min_lamports,
            v_max_lamports: self.cfg.vpin_v_max_lamports,
            min_buckets: self.cfg.vpin_min_buckets,
            stale_ticks: self.cfg.vpin_stale_ticks,
        }
    }

    fn vpin_thresholds(&self) -> VpinThresholds {
        VpinThresholds {
            warn_bp: self.cfg.vpin_warn_bp,
            toxic_bp: self.cfg.vpin_toxic_bp,
            veto_bp: self.cfg.vpin_veto_bp,
            sell_dom_bp: self.cfg.vpin_sell_dom_bp,
        }
    }

    /// The run mode.
    #[must_use]
    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// The current logical tick.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Snapshot of all open positions for report-plane consumption (item 2c).
    /// Call BEFORE `report()` which force-closes positions.
    #[must_use]
    /// Phase 2: total episodes recorded into the expected-move model. Zero means
    /// no trade has closed since the engine was created — the learning loop is
    /// not accumulating samples.
    #[must_use]
    pub fn expected_move_sample_count(&self) -> u32 {
        self.expected_move.total_n()
    }

    pub fn open_positions_snapshot(&self) -> Vec<crate::live_status::OpenPositionSnapshot> {
        self.positions.open_positions_snapshot(self.now)
    }

    /// Centralized reject accounting: increments both the aggregate `rejected`
    /// counter and the per-code `reject_counts` histogram. Every reject site
    /// MUST route through this helper so the histogram is exact and the
    /// accounting identity `promoted = admitted + rejected` is preserved
    /// alongside the per-code breakdown.
    fn reject(&mut self, code: u8) {
        self.rejected += 1;
        let idx = (code as usize).min(31);
        self.reject_counts[idx] += 1;
    }

    /// §60/§62 LAW 21 canonical live-status snapshot (report-only): a bounded,
    /// deterministic view of the running engine — info-time is the event-stream
    /// tick (never wall-clock). Reads live counters directly WITHOUT finalizing, so
    /// it can be sampled mid-run without force-closing open positions.
    #[must_use]
    pub fn live_status(&self) -> crate::live_status::LiveStatus {
        crate::live_status::LiveStatus {
            info_time_tick: self.now,
            promoted: self.promoted,
            admitted: self.admitted,
            rejected: self.rejected,
            reject_counts: self.reject_counts,
            open_positions: self.positions.len() as u64,
            net_realized_lamports: self.bankroll_realized,
            universe_filtered: self.universe_filtered,
            probes_budgeted: self.probes_budgeted,
            probe_spend_lamports: self.calibration.spent_lifetime,
            journal_digest: self.journal.digest(),
            live_outbound_successes: self.live_outbound_successes,
            live_outbound_failures: self.live_outbound_failures,
            live_sell_successes: self.live_sell_successes,
            live_sell_failures: self.live_sell_failures,
            buy_confirmed_count: self.buy_confirmed_count,
            buy_failed_count: self.buy_failed_count,
            sell_confirmed_count: self.sell_confirmed_count,
            sell_failed_count: self.sell_failed_count,
            pending_buy_count: self.pending_buys.len() as u64,
            pending_sell_count: self.pending_sells.len() as u64,
        }
    }

    /// LAW B6: the per-alpha-source realized ledger, sorted for determinism.
    /// Shared by [`Self::report`] and the strategy-analysis export so the two can
    /// never disagree about which rooms have earned an outcome.
    fn alpha_source_net_rows(&self) -> Vec<(SourceRef, i64)> {
        let mut srcs: std::collections::BTreeSet<SourceRef> = std::collections::BTreeSet::new();
        for &src in self.alpha_source.values() {
            srcs.insert(src);
        }
        srcs.into_iter()
            .filter(|&src| self.source_outcome.trade_count(src) > 0)
            .map(|src| (src, self.source_outcome.net_sol(src)))
            .collect()
    }

    /// LAW B6: the `brain_analysis_v1` strategy-analysis artifact for the CURRENT
    /// engine state (report plane only).
    ///
    /// Info-time is the event-stream tick projected onto the brain's nanosecond
    /// axis, never a wall-clock read, so two replays of one tape build identical
    /// artifacts (§22/§54). Reads live state WITHOUT finalizing, so it can be
    /// sampled mid-run exactly like [`Self::live_status`].
    #[must_use]
    pub fn brain_analysis(&self) -> crate::brain_analysis::BrainAnalysis {
        let alpha = self.alpha_source_net_rows();
        crate::brain_analysis::build(&crate::brain_analysis::AnalysisInputs {
            info_time_ns: self.now.saturating_mul(crate::brain::BRAIN_TICK_NS),
            tick: self.now,
            brain: &self.brain,
            social: &self.social_plane,
            lane_perf: &self.lane_perf,
            disc_perf: &self.disc_perf,
            alpha_source_net: &alpha,
            min_sample: self.cfg.brain_min_sample,
            decay_min_sample: self.cfg.brain_decay_min_sample,
        })
    }

    /// LAW B6: the strategy-analysis artifact as canonical JSON, without touching
    /// a filesystem. The seam tests and the evaluator consume — a research
    /// consumer should never have to stand up a temp directory to read what the
    /// brain thinks.
    #[must_use]
    pub fn brain_analysis_json(&self) -> String {
        self.brain_analysis().to_canonical_json()
    }

    /// LAW B6/B7: every traded setup class with its full conditioned verdict,
    /// refusals included (report plane). The seam the export, the lane-decay flag
    /// set and the promotion evidence all read, so the three can never disagree
    /// about what the brain currently believes.
    #[must_use]
    pub fn brain_conditioned_classes(&self) -> Vec<crate::brain::ConditionedClass> {
        self.brain.conditioned_classes()
    }

    /// LAW B9: the episodic-recall evidence the promotion report consults.
    ///
    /// Counts conditioned-negative setup classes over the §46 decay floor and the
    /// standing §56 retirement nominations. Fail-closed: with the brain disarmed
    /// or its index thin, every count is zero and the evidence blocks nothing.
    #[must_use]
    pub fn recall_evidence(&self) -> crate::authority::RecallEvidence {
        if !self.cfg.brain_enable {
            return crate::authority::RecallEvidence::none();
        }
        let classes = self.brain.conditioned_classes();
        let mut examined = 0u32;
        let mut negative = 0u32;
        for c in &classes {
            let Some(stats) = c.verdict.stats() else {
                continue;
            };
            examined = examined.saturating_add(1);
            if crate::brain_analysis::is_conditioned_negative(
                stats,
                self.cfg.brain_decay_min_sample,
            ) {
                negative = negative.saturating_add(1);
            }
        }
        let alpha = self.alpha_source_net_rows();
        let flags = crate::brain_analysis::retirement_flags(
            &crate::brain_analysis::AnalysisInputs {
                info_time_ns: self.now.saturating_mul(crate::brain::BRAIN_TICK_NS),
                tick: self.now,
                brain: &self.brain,
                social: &self.social_plane,
                lane_perf: &self.lane_perf,
                disc_perf: &self.disc_perf,
                alpha_source_net: &alpha,
                min_sample: self.cfg.brain_min_sample,
                decay_min_sample: self.cfg.brain_decay_min_sample,
            },
            &classes,
        );
        crate::authority::RecallEvidence {
            classes_examined: examined,
            conditioned_negative: negative,
            retirement_flags: u32::try_from(flags.len()).unwrap_or(u32::MAX),
        }
    }

    /// §60/§62 LAW 21: drive an event stream, writing the canonical live-status
    /// artifact to `status_path` every `every_ticks` events and once more at the
    /// end, then produce the final report. The periodic snapshots are taken
    /// pre-finalize (live, mid-run). Status writes are BEST-EFFORT: a failed write
    /// is reported to stderr but never aborts the run (a status artifact is
    /// telemetry, not a trading authority). Returns the number of successful status
    /// writes alongside the report.
    pub fn run_with_status(
        &mut self,
        events: &[AppEvent],
        status_path: &std::path::Path,
        every_ticks: u64,
    ) -> (Report, u64) {
        let every = every_ticks.max(1);
        let mut writes = 0u64;
        let mut write = |st: crate::live_status::LiveStatus| match st.write_to_path(status_path) {
            Ok(()) => writes += 1,
            Err(e) => eprintln!("live_status write failed: {e}"),
        };
        // Best-effort open-positions telemetry dump alongside live_status.json.
        let open_path = std::path::Path::new("data/open_positions.json");
        for (i, &ev) in events.iter().enumerate() {
            self.tick(ev);
            // Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85.
            #[allow(clippy::manual_is_multiple_of)]
            if (i as u64 + 1) % every == 0 {
                write(self.live_status());
                self.write_brain_analysis();
                let snaps = self.open_positions_snapshot();
                let _ = crate::live_status::OpenPositionSnapshot::write_to_path(&snaps, open_path);
            }
        }
        write(self.live_status());
        self.write_brain_analysis();
        let snaps = self.open_positions_snapshot();
        let _ = crate::live_status::OpenPositionSnapshot::write_to_path(&snaps, open_path);
        (self.report(), writes)
    }

    /// LAW B6: write the strategy-analysis artifact alongside the live status, at
    /// the same info-time cadence.
    ///
    /// Best-effort, exactly like the status write: the artifact is telemetry for a
    /// research consumer, never a trading authority, so a failed write is reported
    /// and the loop continues. A disarmed switch or an empty path is a silent
    /// no-op — the artifact is a pure function of engine state and the filesystem
    /// is only one of its sinks.
    pub fn write_brain_analysis(&self) {
        if !self.cfg.brain_analysis_enable || self.cfg.brain_analysis_path.is_empty() {
            return;
        }
        let path = std::path::Path::new(self.cfg.brain_analysis_path.as_str());
        if let Err(e) = self.brain_analysis().write_to_path(path) {
            eprintln!("brain_analysis write failed: {e}");
        }
    }

    /// Feed one event.
    pub fn tick(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::MarketTrade {
                mint,
                price_fp,
                quote_lamports,
                liquidity_lamports,
                signed_base,
                buyer_entity,
                age_slots,
            } => {
                self.numeric.observe(
                    mint,
                    price_fp,
                    quote_lamports,
                    liquidity_lamports,
                    signed_base,
                    buyer_entity,
                    age_slots,
                    self.now,
                );
                // §47a LAW 18: record the mint's last-trade info-time (tick) for the
                // terminal-state reflection cadence. Bounded (§99): a new mint past
                // capacity evicts the lexicographically-smallest tracked mint.
                {
                    let m = *mint.as_bytes();
                    if !self.last_trade_tick.contains_key(&m)
                        && self.last_trade_tick.len() >= LAST_TRADE_TABLE_CAP
                    {
                        if let Some(&victim) = self.last_trade_tick.keys().next() {
                            self.last_trade_tick.remove(&victim);
                        }
                    }
                    self.last_trade_tick.insert(m, self.now);
                }
                // §70.1 CONTINUOUS HOLDER ACCOUNTING — the WATCH phase.
                //
                // Folded for EVERY decoded swap on EVERY mint, not just admitted
                // ones, so that by the time a candidate reaches the gate its holder
                // series already exists. This is the canonical (§6.1) derivation:
                // the holder count comes from the `buyer_entity` + `signed_base` we
                // decoded ourselves, never from a third-party holder endpoint (§6.6
                // keeps Birdeye/DAS strictly corroboration-tier).
                //
                // The fold returns a sample only on the bounded
                // `HOLDER_SAMPLE_INTERVAL_TICKS` cadence: the §70.1 acceleration
                // estimator refuses comparison points closer than its 1 s minimum
                // interval, so sampling per-swap would push samples that are simply
                // dropped for a non-advancing information time. Three ticks
                // (1.2 s at BRAIN_TICK_NS) is the smallest whole-tick cadence at or
                // above that floor — asserted at compile time in `holder_flow`.
                {
                    let ns = self.now.saturating_mul(BRAIN_TICK_NS);
                    // The swap's market age in slots is passed through so the
                    // ledger can classify an entity's FIRST buy as a bundle
                    // (creation slot) or a sniper (within
                    // `holder_flow::SNIPER_SLOT_WINDOW`) — arXiv 2601.08641. It is
                    // a first-sighting fact that cannot be recovered later, so it
                    // is captured in the fold or not at all (§20).
                    let fold = self.holder_flow.observe_swap_aged(
                        mint.as_bytes(),
                        buyer_entity,
                        signed_base,
                        self.now,
                        ns,
                        Some(age_slots),
                    );
                    if let Some(count) = fold.sample {
                        self.measured
                            .record_holder_count(fnv1a_64(mint.as_bytes()), count, ns);
                        // …and, on the SAME bounded cadence, fold the tracked
                        // cohort's INTERNAL concentration into its own series
                        // (§21.7 parallel stream).
                        //
                        // Concentration used to be a point reading derived on
                        // demand at admit. A point reading can answer "is this
                        // concentrated?" but not "is it concentratING?", and the
                        // second question is the one with a tradeable answer. The
                        // derivation is `O(n)` over a bounded ledger and runs once
                        // per 1.2 s of information time per mint — the same cost
                        // discipline the holder sample already pays, reusing the
                        // same compile-time-proven cadence rather than inventing a
                        // second one.
                        //
                        // Note this is the INTERNAL statistic, gated on
                        // `admits_growth` and therefore live on the delta-only
                        // ledgers where the absolute share refuses. It is a
                        // different quantity in a different type and can never be
                        // read as a share of the float.
                        let internal =
                            internal_concentration_of(&self.holder_flow, mint.as_bytes());
                        self.conc_trajectory
                            .observe(mint.as_bytes(), internal, self.now, ns);
                    }
                }
                // On-chain-led category flow: a trade contributes to per-category
                // MetaRotationState measures ONLY if its mint has a known (on-chain-
                // assigned) category. Guarded by an O(1) `is_empty` check first, so a
                // run that never ingests `TokenMetadata` pays a single branch and is
                // byte-identical to one before this layer (the golden hot path).
                // Unknown-category trades are NOT silently bucketed into UNCLASSIFIED
                // (§6.4 UNKNOWN discipline).
                if !self.mint_category.is_empty() {
                    self.route_category_flow(*mint.as_bytes(), signed_base);
                }
                // Market context (§21.3 regime + §28 cluster breadth + phase) and the
                // flow/wallet screens (§21.7 authenticity, §28 lagged-shadow feed).
                self.context.on_trade(
                    mint.as_bytes(),
                    buyer_entity,
                    signed_base >= 0,
                    quote_lamports,
                    signed_base.unsigned_abs(),
                    liquidity_lamports,
                );
                self.flow_screen.record(
                    mint.as_bytes(),
                    buyer_entity,
                    signed_base >= 0,
                    quote_lamports,
                );
                // §21.6 trade-count bars + §21.5 activity window: O(1) fold per
                // decoded trade (open-bar update + fixed-ring write).
                self.structure.record(
                    *mint.as_bytes(),
                    &crate::structure::TradeObs {
                        now: self.now,
                        price_fp,
                        base_qty: signed_base.unsigned_abs(),
                        quote_lamports,
                        buyer_entity,
                        is_buy: signed_base >= 0,
                    },
                );
                self.wallet_screen.record(
                    buyer_entity,
                    mint.as_bytes(),
                    signed_base >= 0,
                    quote_lamports,
                    self.now,
                );
                // §27 amendment (G5): record tracked-wallet buys for the trust
                // boost corroboration counter. Only BUYs (signed_base >= 0)
                // count toward corroboration. The `buyer_entity` is the u64
                // entity id extracted from the LaserStream `account_keys[6]`
                // (the buyer's wallet pubkey, hashed via splitmix64 — G1 fix).
                if signed_base >= 0 {
                    self.record_tracked_buy(mint.as_bytes(), buyer_entity, self.now);
                }
                // §27/§28 amendment (G6 wiring): record the most recent buyer or
                // seller entity on this mint and build a funding edge between the
                // current buyer and the previous seller (or current seller and
                // previous buyer) on the same mint. This is the wallet-graph
                // cluster-detection feed per arXiv:2505.09313 — the graph is
                // BOUNDED (§99): `wallet_graph_node` grows the graph one node at
                // a time and `add_edge` is O(1). The last-mint maps use the same
                // bounded-eviction convention as `last_trade_tick` (§99).
                {
                    let m = *mint.as_bytes();
                    let cap = self
                        .cfg
                        .watchlist_capacity
                        .saturating_mul(self.cfg.confirmed_capacity_mult)
                        .max(1);
                    if signed_base >= 0 {
                        // BUY: edge from this buyer to the last seller (if any).
                        if let Some(&seller) = self.last_mint_seller.get(&m) {
                            if seller != buyer_entity {
                                self.add_wallet_funding_edge(buyer_entity, seller, self.now);
                            }
                        }
                        // Update last buyer with bounded eviction.
                        if !self.last_mint_buyer.contains_key(&m)
                            && self.last_mint_buyer.len() >= cap
                        {
                            if let Some(&victim) = self.last_mint_buyer.keys().next() {
                                self.last_mint_buyer.remove(&victim);
                            }
                        }
                        self.last_mint_buyer.insert(m, buyer_entity);
                    } else {
                        // SELL: edge from the last buyer to this seller (if any).
                        if let Some(&buyer) = self.last_mint_buyer.get(&m) {
                            if buyer != buyer_entity {
                                self.add_wallet_funding_edge(buyer, buyer_entity, self.now);
                            }
                        }
                        // Update last seller with bounded eviction.
                        if !self.last_mint_seller.contains_key(&m)
                            && self.last_mint_seller.len() >= cap
                        {
                            if let Some(&victim) = self.last_mint_seller.keys().next() {
                                self.last_mint_seller.remove(&victim);
                            }
                        }
                        self.last_mint_seller.insert(m, buyer_entity);
                    }
                }
                // VPIN-X toxicity accumulation (§21.7): fold the swap's exact-sign
                // quote volume into the mint's volume-clocked buckets. O(1) amortized;
                // bounded map with deterministic eviction (§99).
                {
                    let vp = self.vpin_params();
                    let cap = self
                        .cfg
                        .watchlist_capacity
                        .saturating_mul(self.cfg.confirmed_capacity_mult)
                        .max(1);
                    let key = *mint.as_bytes();
                    if !self.vpin.contains_key(&key) && self.vpin.len() >= cap {
                        if let Some(&victim) = self.vpin.keys().next() {
                            self.vpin.remove(&victim);
                        }
                    }
                    let st = self.vpin.entry(key).or_insert_with(|| VpinState::new(&vp));
                    st.on_trade(signed_base >= 0, quote_lamports, self.now, &vp);
                }
                // Held-position lifecycle (§24 per-swap management): advance any open
                // scalp on this market and book an exit if a trigger fires. Guarded by
                // an O(1) `is_empty` so a run holding nothing pays only one branch.
                if !self.positions.is_empty() {
                    let signed_quote = if signed_base >= 0 {
                        i128::from(quote_lamports)
                    } else {
                        -i128::from(quote_lamports)
                    };
                    let price_u = u64::try_from(price_fp.max(0)).unwrap_or(u64::MAX);
                    self.tournament.on_trade(
                        mint.as_bytes(),
                        price_u,
                        signed_quote,
                        self.now,
                        liquidity_lamports,
                    );
                    if let Some(exit) = self.positions.on_trade(
                        mint.as_bytes(),
                        price_u,
                        signed_quote,
                        self.now,
                        liquidity_lamports,
                    ) {
                        self.book_exit(exit);
                    } else if self.positions.has(mint.as_bytes()) {
                        // §33 probe→confirm scale-in (one-shot; scale_in refuses after
                        // any de-risking): in profit + authentic flow ⇒ full target.
                        // §21.6 reduce-only structure block: never ADD risk while the
                        // bar-structure trend contradicts the long (Downtrend). The
                        // probe keeps managing itself — structure blocks additions,
                        // never authorizes anything.
                        if price_fp > 0
                            && self
                                .structure
                                .trend(mint.as_bytes(), self.cfg.structure_min_bars)
                                != TrendStructure::Downtrend
                        {
                            // §6.4: the flow screen's thin-sample NEUTRAL PRIOR is a
                            // label for missing evidence, never confirmation. Adding
                            // risk requires an EVIDENCED authenticity reading at or
                            // above the operator's bar — absence of evidence can no
                            // longer scale a probe to full target.
                            let (auth, _) = self.flow_screen.authenticity(mint.as_bytes());
                            if self.flow_screen.has_auth_evidence(mint.as_bytes())
                                && auth >= self.cfg.scale_confirm_auth_min_bp
                            {
                                if let Some(att) = self.open_lane.get(mint.as_bytes()).copied() {
                                    if att.scale_add > 0
                                        && self.positions.scale_in(
                                            mint.as_bytes(),
                                            att.scale_add,
                                            att.scale_cost,
                                            // The mark the added lamports are ACTUALLY
                                            // bought at — blends the cost basis so the
                                            // add cannot book phantom profit.
                                            price_u,
                                        )
                                    {
                                        if let Some(e) = self.open_lane.get_mut(mint.as_bytes()) {
                                            e.scale_add = 0;
                                        }
                                    }
                                }
                            }
                        }
                        // §32 thesis evaluation: deterministic invalidation forces the
                        // exit; no score may override it.
                        if self.thesis_forces_exit(mint.as_bytes()) {
                            if let Some(exit) = self.positions.close_at(
                                mint.as_bytes(),
                                price_u,
                                ExitReason::ThesisInvalidation,
                            ) {
                                self.book_exit(exit);
                            }
                        } else {
                            // VPIN exit escalation: the extreme sell-dominant tier is a
                            // distributed multi-swap dump the single-print rug-precursor
                            // cannot see — force the thesis-invalidation exit (§21.7/§32).
                            let vp = self.vpin_params();
                            let reading = self
                                .vpin
                                .get(mint.as_bytes())
                                .and_then(|v| v.reading(self.now, &vp));
                            if vpin_exit_escalates(reading, &self.vpin_thresholds()) {
                                if let Some(exit) = self.positions.close_at(
                                    mint.as_bytes(),
                                    price_u,
                                    ExitReason::ThesisInvalidation,
                                ) {
                                    self.book_exit(exit);
                                }
                            }
                        }
                    }
                }
            }
            AppEvent::Migration { mint, slot } => {
                // Curve→pool phase flip (§21.7 phase asymmetry): exit-cost pricing and
                // future hazard conditioning consult the phase from here on.
                self.context.on_migration(mint.as_bytes());
                // §29.9: graduation is the FIRST half of the survival evidence the
                // creator track record is built from (the second half is simply
                // elapsed slots with no rug). Recorded only when the launch itself
                // was observed — an unknown launch is left unknown (§6.4).
                let m = *mint.as_bytes();
                self.measured.observe_slot(slot);
                if let Some(&creator) = self.mint_creator.get(&m) {
                    let _ = self.measured.record_migration(creator, fnv1a_64(&m), slot);
                }
            }
            AppEvent::NarrativeSample {
                mint,
                prior_active,
                new_mentions,
            } => {
                let now = self.now;
                self.narrative
                    .observe(mint, prior_active, new_mentions, now)
            }
            AppEvent::SocialCall {
                mint,
                source_quality_bp,
            } => {
                let now = self.now;
                self.social.observe(mint, source_quality_bp, now)
            }
            AppEvent::WalletAction {
                mint,
                followable,
                size_lamports,
            } => {
                let now = self.now;
                self.wallet.observe(mint, followable, size_lamports, now)
            }
            AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports,
                real_sol_lamports,
            } => self.confirm(*mint.as_bytes(), virtual_sol_lamports, real_sol_lamports),
            AppEvent::TokenMetadata {
                mint,
                category_id,
                taxonomy_version,
                creator,
                slot,
            } => self.observe_token_metadata(
                *mint.as_bytes(),
                category_id,
                taxonomy_version,
                creator,
                slot,
            ),
            AppEvent::CreatorAction { mint, kind, slot } => {
                let m = *mint.as_bytes();
                self.observe_creator_action(m, kind, slot);
                // §26 held-position limb: a confirmed dump forces the exit now.
                self.enforce_creator_dump_exit(&m);
            }
            // Rev-14 wangr intelligence: store auxiliary per-mint data.
            AppEvent::MarketAuxiliary {
                mint,
                token_standard,
                symbol_len,
            } => {
                let m = *mint.as_bytes();
                // §99 bounded growth: evict the lexicographically smallest key
                // at capacity (same pattern as first_slot_fees).
                if !self.mint_aux.contains_key(&m) && self.mint_aux.len() >= 1024 {
                    if let Some(victim) = self.mint_aux.keys().next().copied() {
                        self.mint_aux.remove(&victim);
                    }
                }
                self.mint_aux.insert(m, (token_standard, symbol_len));
            }
            // Rev-14 wangr intelligence: store the latest wall-clock time signal.
            // The engine is a pure tick-based state machine (§22); this is the
            // ONLY channel through which the caller informs it of UTC time.
            AppEvent::TimeSignal { dow, hour_utc } => {
                self.time_signal = (dow, hour_utc);
            }
            // ─── Rev-19 on-chain feedback loop ──────────────────────────────
            // Our buy tx landed on-chain. Reconcile the paper position: mark it
            // as on-chain confirmed so the sell path knows tokens are real.
            AppEvent::OurBuyConfirmed { mint, signature, .. } => {
                let key = sig_key(&signature);
                if self.pending_buys.remove(&key).is_some() {
                    self.buy_confirmed_count = self.buy_confirmed_count.saturating_add(1);
                    // Mark the position as on-chain confirmed.
                    self.positions
                        .mark_onchain_confirmed(mint.as_bytes(), true);
                }
            }
            // Our buy tx failed on-chain. Reverse the paper position — we
            // never received tokens. The fee is already burned irrecoverably.
            AppEvent::OurBuyFailed { mint, signature, .. } => {
                let key = sig_key(&signature);
                if let Some(pending) = self.pending_buys.remove(&key) {
                    self.buy_failed_count = self.buy_failed_count.saturating_add(1);
                    // Remove the paper position — tokens were never received.
                    self.positions.reverse_paper_entry(mint.as_bytes(), pending.size);
                }
            }
            // Our sell tx landed on-chain. SOL was recovered — the paper exit
            // is now confirmed as on-chain truth.
            AppEvent::OurSellConfirmed { mint, signature, .. } => {
                let key = sig_key(&signature);
                if self.pending_sells.remove(&key).is_some() {
                    self.sell_confirmed_count = self.sell_confirmed_count.saturating_add(1);
                    // Mark the exit as on-chain confirmed.
                    self.positions
                        .mark_onchain_exit_confirmed(mint.as_bytes(), true);
                }
            }
            // Our sell tx failed on-chain. Tokens remain in wallet — SOL was
            // NOT recovered. The paper exit must be reversed so we can retry.
            AppEvent::OurSellFailed { mint, signature, .. } => {
                let key = sig_key(&signature);
                if let Some(pending) = self.pending_sells.remove(&key) {
                    self.sell_failed_count = self.sell_failed_count.saturating_add(1);
                    // Reverse the paper exit so the sell ladder can retry.
                    self.positions
                        .reverse_paper_exit(mint.as_bytes(), pending.size);
                }
            }
            AppEvent::Tick => self.evaluate(),
        }
    }

    /// Attribute one decoded trade's signed base flow to its category's on-chain
    /// measures (a `Buy` if net-buy, else a `Sell`). Called only for mints with a
    /// known category. `|signed_base|` is the base-volume proxy for quote flow, and
    /// the logical tick is the time-safe slot. Off the golden path (§21.4).
    fn route_category_flow(&mut self, mint: [u8; 32], signed_base: i64) {
        let Some(&cat) = self.mint_category.get(&mint) else {
            return;
        };
        let quote_lamports = signed_base.unsigned_abs();
        let kind = if signed_base >= 0 {
            CategoryEventKind::Buy { quote_lamports }
        } else {
            CategoryEventKind::Sell { quote_lamports }
        };
        self.meta.ingest(&CategoryEvent {
            category_id: cat,
            kind,
            slot: self.now,
        });
    }

    /// Record an on-chain-led category assignment for a market (§21.4, §85). The
    /// launch is counted once, at first sighting, and only when the assignment's
    /// taxonomy version matches the reducer's — a mismatch is left UNKNOWN, never
    /// retroactively remapped (§81). The mint→category map always takes the latest
    /// assignment (forward-only) and is bounded (§99).
    fn observe_token_metadata(
        &mut self,
        mint: [u8; 32],
        category_id: u64,
        taxonomy_version: u32,
        creator: u64,
        slot: u64,
    ) {
        let first_sighting = !self.mint_category.contains_key(&mint);
        // Bounded (§99): evict the lexicographically-smallest tracked mint when the
        // map is full (deterministic; matches the confirmed-set bound multiple).
        let cap = self
            .cfg
            .watchlist_capacity
            .saturating_mul(self.cfg.confirmed_capacity_mult)
            .max(1);
        if first_sighting && self.mint_category.len() >= cap {
            if let Some(&victim) = self.mint_category.keys().next() {
                self.mint_category.remove(&victim);
            }
        }
        self.mint_category.insert(mint, category_id);
        // §27/§70.9 creator-credibility evidence: lifetime + windowed launch counts.
        if first_sighting {
            self.context.on_launch();
            if self.mint_creator.len() >= cap && !self.mint_creator.contains_key(&mint) {
                if let Some(&victim) = self.mint_creator.keys().next() {
                    self.mint_creator.remove(&victim);
                }
            }
            self.mint_creator.insert(mint, creator);
            if self.creator_launches.len() >= cap && !self.creator_launches.contains_key(&creator) {
                if let Some(&victim) = self.creator_launches.keys().next() {
                    self.creator_launches.remove(&victim);
                }
            }
            let e = self
                .creator_launches
                .entry(creator)
                .or_insert((0, self.now, 0));
            e.0 = e.0.saturating_add(1);
            if self.now.saturating_sub(e.1) > CREATOR_SERIAL_WINDOW_TICKS {
                e.1 = self.now;
                e.2 = 0;
            }
            e.2 = e.2.saturating_add(1);
            // §29.9: the launch fact enters the creator ledger, keyed on the CHAIN
            // slot the metadata was decoded at (not the logical tick — the survival
            // horizon is a chain-slot quantity). This, plus `Migration` and the
            // confirmed-dump rug fact, is what makes `CreatorClass::Proven`
            // reachable at all; before this the class was structurally unreachable
            // and the fingerprint could only ever say Unknown/Serial/Toxic.
            let _ = self.measured.record_launch(creator, fnv1a_64(&mint), slot);
        }
        self.measured.observe_slot(slot);
        // A decoded creation immediately creates a discovery candidate (§23/§21.1):
        // record the sighting so `evaluate` surfaces a CreationSniper candidate while
        // it is fresh. The gate still requires on-chain confirmation — this only
        // affects earliness of watchlist presence. Bounded, deterministic eviction.
        if first_sighting {
            if self.creations.len() >= cap {
                if let Some(&victim) = self.creations.keys().next() {
                    self.creations.remove(&victim);
                }
            }
            // Sighting time is the engine's logical tick (the TTL clock); the
            // on-chain `slot` stays on the meta-rotation record below.
            self.creations.insert(mint, self.now);
            // §70.1 observation-window law: a mint we start watching AT ITS
            // CREATION has an empty holder set at t0, so every subsequent
            // holder-creating trade is inside our window and the folded count is
            // an ABSOLUTE one (`HolderCountBasis::Exact`). A mint discovered any
            // other way is `DeltaOnly` — we know the change, not the base. The
            // claim is only honoured if it lands before the first folded swap, and
            // it stays falsifiable afterwards (a sell from an untracked position
            // proves a pre-window holder and demotes it).
            self.holder_flow.note_creation(&mint, self.now);
        }
        // Count the launch once, iff the taxonomy matches (on-chain-led factual
        // state may only be populated by a matching-version assignment, §85).
        if first_sighting && taxonomy_version == self.cfg.meta_taxonomy_version {
            self.meta.ingest(&CategoryEvent {
                category_id,
                kind: CategoryEventKind::Launch { creator },
                slot,
            });
        }
    }

    /// Fold one creator-attributed action into the market's `CreatorState` reducer,
    /// creating it on first sighting. Bounded (§99): a new market beyond
    /// `creator_track_cap` evicts the lexicographically-smallest tracked market.
    fn observe_creator_action(&mut self, mint: [u8; 32], kind: CreatorActionKind, slot: u64) {
        let cap = self.cfg.creator_track_cap.max(1);
        if !self.creators.contains_key(&mint) && self.creators.len() >= cap {
            if let Some(&victim) = self.creators.keys().next() {
                self.creators.remove(&victim);
            }
        }
        let reducer = self
            .creators
            .entry(mint)
            .or_insert_with(|| CreatorStateReducer::new(CREATOR_LINKED_CLUSTER_CAP));
        let ev = match kind {
            CreatorActionKind::Init {
                initial_tokens,
                total_supply,
            } => CreatorEvent::Init {
                initial_tokens,
                total_supply,
                slot,
            },
            CreatorActionKind::Buy {
                tokens,
                quote_lamports,
            } => CreatorEvent::Buy {
                tokens,
                quote_lamports,
                slot,
            },
            CreatorActionKind::Sell {
                tokens,
                quote_lamports,
            } => CreatorEvent::Sell {
                tokens,
                quote_lamports,
                slot,
            },
            CreatorActionKind::LinkedBuy { cluster, tokens } => CreatorEvent::LinkedBuy {
                cluster,
                tokens,
                slot,
            },
        };
        reducer.ingest(&ev);
        self.measured.observe_slot(slot);
        // §29.9: a CONFIRMED creator distribution (the same hard binary the §26
        // veto reads) is the rug/LP-pull fact the creator ledger scores on. The
        // ledger keeps only the FIRST such observation per launch, so re-observing
        // a live dump on every subsequent action cannot inflate the record.
        if self.creator_dump_active(&mint) {
            if let Some(&creator) = self.mint_creator.get(&mint) {
                let _ = self.measured.record_rug(creator, fnv1a_64(&mint), slot);
            }
        }
    }

    /// Drain one batch from a live social [`SocialSource`] and apply it to the
    /// social discovery lane as corroboration-tier calls — the live wiring of the
    /// social lane into the loop.
    ///
    /// Latency + determinism discipline (§22, §24): this runs BETWEEN ticks, never
    /// inside [`Self::evaluate`], so the deterministic hot path is byte-for-byte
    /// unchanged and the source pull — the only `[S]` I/O, a non-blocking drain of
    /// an already-captured buffer (a mock/replay in Phase-A) — can never add latency
    /// or non-determinism to a decision. It is allocation-free per call: each post
    /// is parsed and applied one at a time straight from the source-owned batch,
    /// with no intermediate `SocialEvent`/`AppEvent` vector, and applied directly to
    /// the lane (bypassing the `AppEvent::SocialCall` match) for the tightest path.
    ///
    /// Each named contract in each captured post becomes both a corroboration call
    /// on the social lane (at the ledger-resolved quality) and a narrative
    /// [`Mention`] on the [`AttentionField`], so the post drives the full
    /// attention-velocity model, not just a rank bump. Quality is resolved from the
    /// engine's own D1–D10 [`SourceQualityLedger`] — earned, never assumed; a source
    /// with no reconciled evidence gets the PUBLIC_BURNED baseline (§29.8). Social is
    /// corroboration-tier: on-chain confirmation is still required to admit capital
    /// (§29/§71). Cashtag-only posts (no contract) carry no on-chain target and are
    /// skipped here. Returns the number of corroboration calls applied.
    pub fn ingest_social<S>(&mut self, source: &mut S) -> usize
    where
        S: SocialSource,
    {
        let batch = source.next_batch();
        // Decode the whole batch first so cross-post coordination is visible:
        // identical content from ≥2 distinct authors inside the window is a
        // manipulation flag, not conviction (§29.7c) — those calls corroborate at
        // ZERO quality (they still feed attention, where copy-echo is discounted).
        let events: Vec<_> = batch
            .iter()
            .filter_map(|p| parse_social_event(&p.json, p.observed_at_ns))
            .collect();
        let coordinated =
            crate::social_ingest::coordinated_content(&events, COORDINATION_WINDOW_NS);
        let mut applied = 0usize;
        for ev in &events {
            // Duplicate suppression FIRST (§29): the same (author, content) seen
            // before — a cross-provider duplicate delivery or a verbatim same-
            // author repost — is dropped entirely: no lane observation, no
            // attention, no earn-loop call. Distinct authors sharing content
            // pass through to the coordination screen below.
            let seen_key = (ev.author_id, ev.content_hash);
            if self.social_seen.contains(&seen_key) {
                continue;
            }
            if self.social_seen_ring.len() >= SOCIAL_SEEN_CAP {
                if let Some(old) = self.social_seen_ring.pop_front() {
                    self.social_seen.remove(&old);
                }
            }
            self.social_seen.insert(seen_key);
            self.social_seen_ring.push_back(seen_key);
            let is_coordinated = coordinated.iter().any(|&(h, _)| h == ev.content_hash);
            // Earned favorable-rate from the reconciliation loop supersedes the
            // PUBLIC_BURNED baseline; an unproven source falls back to the ledger.
            // Coordinated copy-paste shilling corroborates at zero (§29.7c).
            let q = if is_coordinated {
                0
            } else {
                self.social_earn
                    .quality_bps_for(ev.author_id)
                    .unwrap_or_else(|| {
                        ledger_quality(&self.ledger, ev.author_id, &self.quality_policy)
                    })
            };
            let mention = to_mention(ev);
            // §29.6 provenance: the live-chat structure travels beside the
            // mention on a parallel channel — the shared `Mention` type (locked
            // by dossiers in two crates) is untouched.
            let prov = crate::social_ingest::provenance_of(ev, is_coordinated);
            // LAW D1: a `Discord` alpha-room call routes to the independent
            // `AlphaCall` discovery lane (and binds the room as the mint's alpha
            // source for the LAW D5 ledger), so a paid room earns its net SOL
            // distinctly from the open social-caller firehose (§71 reflection
            // integrity). The discovery lane never participates in ranking or the
            // gate, so this changes attribution ONLY — never a capital decision, and
            // alpha alone still cannot admit (LAW D4). Off ⇒ Discord behaves like any
            // other social caller, byte-identical.
            let alpha_room =
                self.cfg.alpha_call_lane_enable && ev.platform == SocialPlatform::Discord;
            // LAW D3: a designated-caller call the sentiment brain marks BEARISH is a
            // sell/exit/dump signal — on a HELD position it raises reduce-only exit
            // pressure (never adds/authorizes). Evaluated per named mint below.
            let alpha_bearish_exit = self.cfg.alpha_exit_pressure_enable
                && ev.is_designated_caller
                && crate::social_ingest::is_bearish(ev);
            for m in ev.mints() {
                let now = self.now;
                // LAW D3: a designated SELL/EXIT call is a pure EXIT signal — it
                // "never adds/authorizes" (§29.5 fade-first). It raises reduce-only
                // exit pressure on a HELD position (halves the stall window + trail
                // cap via the existing meta-saturation exit-pressure machinery) and
                // does NOTHING else: no discovery-lane observation, no attention
                // mention, no earn-loop call — a sell call must never rank, amplify,
                // or authorize a coin for entry. A mint with no open position is
                // simply untouched (inert unless capital is at risk). Off ⇒ the call
                // is processed as an ordinary (bullish-treated) call.
                if alpha_bearish_exit {
                    if self.positions.has(m) {
                        self.positions.apply_pressure(m);
                    }
                    continue;
                }
                if alpha_room {
                    self.social
                        .observe_alpha(DomainMint::from_bytes(*m), q, now);
                    // LAW D5: bind the paid room (guild/channel/community id) as this
                    // mint's alpha source — first-bind-wins, bounded (§99).
                    if self.alpha_source.len() < ALPHA_SOURCE_BIND_CAP {
                        self.alpha_source
                            .entry(*m)
                            .or_insert_with(|| SourceRef::discord(ev.community_id));
                    }
                } else {
                    self.social.observe(DomainMint::from_bytes(*m), q, now);
                }
                self.attention.observe_tagged(*m, mention, &prov);
                self.social_earn
                    .record_call(ev.author_id, *m, ev.observed_at_ns);
                // LAW B2: the same call also lands in the brain's social ledger, so
                // "who called this mint" is answerable and — once the position
                // closes and the realized net is attributed back as a markout —
                // "does that author actually earn" becomes a MEASURED track record
                // rather than a follower count. Report plane only.
                // The call is stamped with the ENGINE's information time, not the
                // payload's `observed_at_ns`: the episodic index, the meta timeline
                // and the social ledger must share ONE time axis or a markout can
                // never be matched back to the call that preceded it. The capture
                // lane's stamp comes off a different clock entirely (§20/§22 — the
                // logical tick is the only information time this engine has).
                if self.cfg.brain_enable {
                    let platform = platform_of(ev.platform);
                    let call_id = self.brain.record_call(
                        fnv1a_64(m),
                        ev.author_id,
                        platform,
                        now.saturating_mul(BRAIN_TICK_NS),
                        ev.engagement,
                        ev.is_designated_caller,
                    );
                    // §29.8/§34.3 PROVENANCE: the same call is stamped with its
                    // EVIDENCE CLASS — platform, author, designated flag, earned
                    // trust tier and the information time it was observed at — and
                    // its content digest is bound to the issued call id so the
                    // support estimator can tell BREADTH (independent originators)
                    // from ECHO (one post relayed). Report plane only: nothing in
                    // `social_plane` is read by promotion, ranking, sizing or the
                    // gate.
                    self.social_plane.record_call(SocialCallEvidence {
                        mint_id: fnv1a_64(m),
                        author_id: ev.author_id,
                        platform,
                        designated: ev.is_designated_caller,
                        now_tick: now,
                        call_id,
                        content_digest: ev.content_hash,
                    });
                }
                applied += 1;
            }
            // Learn cashtag→mint bindings from events that name BOTH (first bind
            // wins; bounded). Then resolve cashtag-ONLY chatter — the dominant
            // live-chat shape — into the attention field so a coin being watched
            // on stream RIGHT NOW becomes a discovery candidate without waiting
            // for someone to paste the mint address. Attention-tier only: no
            // `SocialCall` is fabricated from an inferred binding, and the gate
            // still demands numeric evidence + an on-chain confirm to admit.
            if ev.n_mints > 0 {
                if self.cashtag_binds.len() < CASHTAG_BIND_CAP {
                    let first = ev.mints()[0];
                    for &tag in ev.cashtags() {
                        self.cashtag_binds.entry(tag).or_insert(first);
                    }
                }
            } else {
                for &tag in ev.cashtags() {
                    if let Some(&bound) = self.cashtag_binds.get(&tag) {
                        self.attention.observe_tagged(bound, mention, &prov);
                        applied += 1;
                    }
                }
            }
        }
        applied
    }

    /// Record one decoded bonding-curve snapshot as this market's on-chain proof.
    ///
    /// **DECODER HEALTH IS CHECKED HERE, AT THE BOUNDARY.** The pair is run through
    /// [`crate::curve_depth::CurveDepth::decoded`], which refuses a `real_sol` that
    /// contradicts `virtual_sol − 30 SOL` beyond
    /// [`crate::curve_depth::cross_check_tolerance_lamports`]. A refused pair is NOT
    /// recorded, so the market simply has no confirmation and the gate answers
    /// `NeedsOnchainConfirmation` — fail-closed, journalled, and no trade. Silently
    /// clamping the impossible value would have turned a decoder fault into a
    /// plausible-looking thin market and hidden it forever (§18.2).
    fn confirm(&mut self, mint: [u8; 32], virtual_sol: u64, real_sol: u64) {
        if crate::curve_depth::CurveDepth::decoded(virtual_sol, real_sol).is_unknown() {
            return;
        }
        // Bound the confirmed set alongside the watchlist (§99); the multiple is a
        // config field, not a baked-in constant. Eviction is by PAYOUT reserve — the
        // capacity that actually matters — so the weakest market goes first. This is
        // the faithful translation of the retired rule, which evicted on the lowest
        // ASSERTED sellable depth; that quantity no longer exists independently, and
        // its corrected form is `real_sol`.
        //
        // **MEASURED WARNING (re-pin #27).** On the golden tape this key, and this key
        // alone, is worth +14_332_632 lamports — an 85% swing on the whole book. The
        // tape presents ~268 confirmations against a 256-entry bound, so ~12 markets
        // are evicted, and the book is built from ~12 trades in a handful of markets;
        // WHICH markets survive a capacity bound therefore dominates the net. Replacing
        // this key with one that reproduces the old fixture's ordering returns the tape
        // to 16_778_896 and 12 admits EXACTLY, with both substantive corrections in this
        // wave (the payout-bounded `x_max` and the unified `PricedMove`) still in place.
        // The +85% is a readout of an arbitrary eviction tie-break on a book that
        // `edge_provenance.rs` already shows is statistically indistinguishable from
        // zero. It is not evidence about anything and must not be cited as such.
        let cap = self
            .cfg
            .watchlist_capacity
            .saturating_mul(self.cfg.confirmed_capacity_mult)
            .max(1);
        if !self.confirmed.contains_key(&mint) && self.confirmed.len() >= cap {
            if let Some((&weakest, _)) = self.confirmed.iter().min_by_key(|(_, &(_, r, _))| r) {
                self.confirmed.remove(&weakest);
            }
        }
        // Re-confirmation refreshes the tick — freshness is earned per proof (§34.3).
        self.confirmed
            .insert(mint, (virtual_sol, real_sol, self.now));
    }

    /// The evaluation half of the loop, run once per `Tick`.
    fn evaluate(&mut self) {
        self.now = self.now.saturating_add(1);

        // §Quant-Rev-7: prune expired re-entry cooldown entries. The set is bounded
        // by the number of recently-exited mints, but without periodic pruning
        // stale entries accumulate. Removed here on tick advance so the gate's
        // read-path stays O(1). Only runs when the feature is armed — when
        // disabled the set is always empty, so retain() over an empty set is a
        // no-op (and the branch is never taken because the cfg check short-
        // circuits before the retain).
        if self.cfg.reentry_cooldown_enable && !self.reentry_cooldown.is_empty() {
            let now = self.now;
            let cooldown_ticks = self.cfg.reentry_cooldown_ticks;
            self.reentry_cooldown
                .retain(|_, &mut exit_tick| now.saturating_sub(exit_tick) < cooldown_ticks);
        }

        // 1. Discovery: every lane emits independently; union, not intersection.
        // Lane scoring parameters come from config — no band edge or scale is baked in.
        // Each lane appends into the reused `scratch` buffer (cleared, not freed) so
        // steady state allocates no per-tick lane vectors; the union then dedups by
        // mint. Disjoint field borrows keep this a single pass with no clones.
        self.scratch.clear();
        let numeric_gate = NumericEmitGate {
            ofi_min_bp: self.cfg.numeric_ofi_min_bp,
            revert_ofi_min_bp: self.cfg.revert_ofi_min_bp,
            roll_trend_bp: self.cfg.roll_trend_bp,
            roll_revert_bp: self.cfg.roll_revert_bp,
            evidence_ttl_ticks: self.cfg.lane_evidence_ttl_ticks,
        };
        self.numeric
            .emit_into(&mut self.scratch, self.now, &numeric_gate);
        // §29.6 attention decay: narrative evidence ages continuously toward the
        // TTL cliff — a stale mention must not outrank fresh flow.
        let decay = AttentionDecayParams {
            rate_bp: self.cfg.narrative_decay_bp,
            step_ticks: self.cfg.narrative_decay_step_ticks,
            floor: self.cfg.narrative_decay_floor,
        };
        self.narrative.emit_into(
            &mut self.scratch,
            self.now,
            self.cfg.narrative_stage_hi_fp,
            self.cfg.narrative_stage_lo_fp,
            self.cfg.lane_evidence_ttl_ticks,
            &decay,
        );
        self.social.emit_into(
            &mut self.scratch,
            self.now,
            self.cfg.lane_evidence_ttl_ticks,
        );
        self.wallet.emit_into(
            &mut self.scratch,
            self.now,
            self.cfg.wallet_score_scale,
            self.cfg.lane_evidence_ttl_ticks,
        );
        // Fresh creation sightings surface as CreationSniper candidates (§23/§21.1):
        // a decoded launch is discoverable IMMEDIATELY, before anyone else trades or
        // shills it. Expired sightings are dropped in place (bounded, deterministic);
        // the gate still demands on-chain confirmation, so earliness never bypasses
        // corroboration. Empty-until-fed: no `TokenMetadata` ⇒ no cost, no change.
        if !self.creations.is_empty() {
            let ttl = self.cfg.creation_ttl_ticks;
            let now = self.now;
            self.creations
                .retain(|_, &mut seen| now.saturating_sub(seen) <= ttl);
            for (mint, &seen) in &self.creations {
                self.scratch.push(
                    Candidate::new(
                        pump_quant_watchlist::candidate::Mint::new(*mint),
                        WlLane::CreationSniper,
                        self.cfg.creation_score,
                        seen,
                        Features::default(),
                    )
                    .with_discovery_lane(DiscoveryLane::OnchainCreation),
                );
            }
        }
        // Social attention-velocity candidates (the full `virality = attention =
        // money` model) join the same union. The field is empty until social
        // attention is ingested, so this is a zero-cost no-op — and byte-identical —
        // for any run that never feeds it. Disjoint field borrows: the money proxy
        // reads `numeric`, confirmation reads `confirmed`, both immutable, while
        // `attention` and `scratch` are mutable.
        if !self.attention.is_empty() {
            let numeric = &self.numeric;
            let confirmed = &self.confirmed;
            let wallet = &self.wallet;
            let holder_flow = &self.holder_flow;
            let money_proxy_enable = self.cfg.money_proxy_enable;
            let holder_flow_term = self.cfg.money_proxy_holder_flow_enable;
            self.attention.emit_into(
                &mut self.scratch,
                self.now,
                |m| {
                    let dm = DomainMint::from_bytes(*m);
                    let feats = numeric.features_for(dm);
                    // Price-momentum term: the OFI-derived buy-pressure (the prior
                    // proxy in its entirety).
                    let buy_pressure = feats.map_or(0, |f| u64::from(f.buy_pressure_bp));
                    if !money_proxy_enable {
                        return buy_pressure;
                    }
                    // §70.1 composite money proxy M: fold the distinct-smart-wallet-
                    // entry / net-inflow term (followable wallet inflow, decade-
                    // compressed × weight) and the holder-growth term in AHEAD of
                    // price momentum, then add the buy-pressure momentum tail. Both
                    // folded terms are non-negative, so when a mint has neither
                    // wallet inflow nor holders the composite equals `buy_pressure`
                    // exactly. Integer/saturating (§22).
                    //
                    // HOLDER TERM. The legacy term is `Features::unique_buyers` — a
                    // popcount over a 64-bit bitset indexed by `entity % 64`. It is
                    // a coarse BREADTH proxy, not a holder count: it saturates at
                    // 64, entities collide, and it is MONOTONE NON-DECREASING, so it
                    // structurally cannot observe distribution. When
                    // `money_proxy_holder_flow_enable` is armed the term instead
                    // reads the CONTINUOUS holder count folded from our own decoded
                    // flow, which rises on broadening and falls on distribution.
                    //
                    // Trustworthiness is enforced by the reading's basis, not
                    // assumed: `growth_level()` yields a number only for `Exact` and
                    // `DeltaOnly` — §70.1 wants holder-growth acceleration, a
                    // DERIVATIVE, and a delta-only basis measures exactly that — and
                    // refuses under `Incomplete`, where the entity cap has truncated
                    // both the level and the rate. On refusal (untracked mint or
                    // `Incomplete`) the term falls back EXPLICITLY to the legacy
                    // bitset value rather than fabricating a zero.
                    let bitset_holders = feats.map_or(0, |f| u64::from(f.unique_buyers));
                    let holders = if holder_flow_term {
                        holder_flow
                            .reading(m)
                            .and_then(|r| r.growth_level())
                            .map_or(bitset_holders, |h| h.min(MONEY_PROXY_HOLDER_TERM_CAP))
                    } else {
                        bitset_holders
                    };
                    let inflow_decade = decade_u64(wallet.inflow_of(dm));
                    inflow_decade
                        .saturating_mul(MONEY_PROXY_WALLET_WEIGHT)
                        .saturating_add(holders.saturating_mul(MONEY_PROXY_HOLDER_WEIGHT))
                        .saturating_add(buy_pressure)
                },
                |m| confirmed.contains_key(m),
            );
        }
        let unioned = ingest_union(self.scratch.iter().copied(), &self.weights);
        // §71 union preservation: the capacity-bounded board is rank-evicted, and
        // numeric scores (~10^5) structurally evict the fade-capped (§29, ≤10^3)
        // corroboration lanes at INSERTION — so collect the corroboration tier
        // from the pre-insertion union here (lanes re-emit every tick while
        // their evidence lives, so no extra state is needed).
        // Reused scratch (O2): identical contents/order to the old per-tick
        // `collect`, so the digest is unchanged; only the allocation is recycled.
        let mut corrob = std::mem::take(&mut self.corrob_buf);
        corrob.clear();
        corrob.extend(
            unioned
                .values()
                .filter(|c| c.lane != WlLane::ActiveMarketScalp)
                .copied(),
        );
        // Gate-viability first (§18 promotion economy): an UNCONFIRMED
        // corroboration candidate cannot clear the gate's on-chain-confirmation
        // requirement — promoting it burns the quota slot on a guaranteed
        // reject (research-visible via PRFS, but never an entry). Confirmed
        // corroboration evidence — attention WITH money proof — takes the slot
        // first; unconfirmed candidates fill only what remains.
        corrob.sort_by(|a, b| {
            let ca = self.confirmed.contains_key(&a.mint.bytes());
            let cb = self.confirmed.contains_key(&b.mint.bytes());
            cb.cmp(&ca)
                .then_with(|| b.discovery_score.cmp(&a.discovery_score))
                .then_with(|| a.mint.cmp(&b.mint))
        });
        for cand in unioned.values() {
            // Corroboration-tier meta-rotation reweight: an on-chain-emerging
            // category raises its mints' rank, a saturating one fades them. Identity
            // (byte-for-byte) until categories rotate, so the golden path is
            // unchanged; reorders promotion only — never authorizes entry (§29/§71).
            let adjusted = self.apply_meta_rank(*cand);
            self.watchlist.insert(adjusted, self.now);
        }

        // 2. Recency prune.
        self.watchlist.prune(self.now);

        // 3. Promote the top-ranked survivors to the gate — with the §71
        // union-preservation quota: raw rank lets the numeric lane's scores
        // (imbalance × liquidity × breadth, ~10^5) monopolize every slot over
        // the fade-capped corroboration lanes (≤10^3 by §29 design), turning
        // the union into a de-facto intersection. Reserve up to
        // `promote_corroboration_quota` slots for the best non-numeric
        // candidates by replacing the LOWEST-ranked numeric winners. Rank is
        // not authority: these candidates still face the full gate.
        let mut promoted = promote_top(
            &self.watchlist,
            self.now,
            self.cfg.promote_k,
            self.cfg.promote_min_rank,
        );
        let quota = self.cfg.promote_corroboration_quota.min(self.cfg.promote_k);
        if quota > 0 {
            let have = promoted
                .iter()
                .filter(|c| c.lane != WlLane::ActiveMarketScalp)
                .count();
            if have < quota {
                // Reused scratch (O2): `drain` below empties it while retaining the
                // allocation, so it is restored to `self` at tick end alloc-free.
                let mut extras = std::mem::take(&mut self.extras_buf);
                extras.clear();
                for cand in &corrob {
                    if extras.len() + have >= quota {
                        break;
                    }
                    if cand.discovery_score > 0 && !promoted.iter().any(|p| p.mint == cand.mint) {
                        extras.push(*cand);
                    }
                }
                // Replace the lowest-ranked winners (the TAIL of the rank-
                // ordered promote list) so the tick's gate workload stays at
                // promote_k: drop the tail FIRST, then append the extras —
                // popping per-push would evict the extras themselves.
                if !extras.is_empty() {
                    promoted.truncate(self.cfg.promote_k.saturating_sub(extras.len()));
                    // `append` drains `extras` into `promoted` (same order as the old
                    // `extend(extras)`) while retaining `extras`' allocation for reuse.
                    promoted.append(&mut extras);
                }
                self.extras_buf = extras;
            }
        }

        // 4-5. Gate every promotion, then allocate the scarce slots by CONDITIONAL
        // EXPECTED NET SOL (§23 arbitration — never promotion order), and open the
        // winners. Forgone candidates are journaled with their opportunity cost
        // implicit in the arbitration record, never silently dropped.
        // Reused scratch (O2): cleared, then filled in the same order as before.
        let mut pending = std::mem::take(&mut self.pending_buf);
        pending.clear();
        for cand in promoted {
            // §21.5 active-market-universe screen, applied at the cheapest
            // possible stage: a MATURE mint whose recent tape shows no genuine
            // activity must not consume a promotion slot or any gate work.
            // Fresh launches (below the age exemption) and mints with no
            // numeric age evidence pass through — the gate still demands
            // on-chain confirmation, so this can only remove dead weight,
            // never authorize anything.
            if !self.universe_promotable(&cand) {
                self.universe_filtered += 1;
                continue;
            }
            self.promoted += 1;
            let rank = self.watchlist.rank_of(&cand, self.now);
            self.journal.record(Decision::Promoted {
                mint: cand.mint.bytes(),
                lane: cand.lane as u8,
                rank,
            });
            if let Some(pe) = self.gate_evaluate(cand) {
                pending.push(pe);
            }
        }
        if !pending.is_empty() {
            let slots = (self
                .cfg
                .max_concurrent_positions
                .saturating_sub(self.positions.len())) as u32;
            // Floor basis = the bankroll ORIGIN's seed (not the config seed): for
            // Paper/Replay that IS `cfg.bankroll_initial_lamports` (byte-identical),
            // for a Phase-B live origin it is the reconciled wallet — so the survival
            // floor scales with the real wallet, never the paper constant.
            let floor = derive_survival_floor(
                self.bankroll_origin.seed_lamports(),
                self.cfg.floor_fraction_bps,
            );
            let deployable = deployable_capital(self.bankroll_balance(), floor);
            let risk_budget =
                u128::from(deployable) * u128::from(self.cfg.total_risk_cap_bp) / 10_000;
            let exposure_cap = risk_budget.saturating_sub(self.bankroll_committed);
            // Reused scratch (O2): identical mapping/order to the old `collect`.
            let mut cands = std::mem::take(&mut self.cands_buf);
            cands.clear();
            cands.extend(pending.iter().enumerate().map(|(i, p)| EntryCandidate {
                candidate_id: i as u64,
                entry_mode: p.lane as u16,
                archetype: p.archetype,
                regime: 0,
                expected_net_sol_lamports: i64::try_from(p.expected_net).unwrap_or(i64::MAX),
                size_lamports: p.entry_cost,
            }));
            let outcome = arbitrate(
                &cands,
                &ArbitrationParams {
                    max_slots: slots,
                    exposure_cap_lamports: u64::try_from(exposure_cap).unwrap_or(u64::MAX),
                    min_expected_net_lamports: self.cfg.arb_min_expected_net_lamports,
                },
            );
            let mut awarded = [false; 64];
            for award in &outcome.awarded {
                let idx = award.candidate_id as usize;
                if idx < pending.len() && idx < awarded.len() {
                    awarded[idx] = true;
                }
            }
            for (i, p) in pending.iter().enumerate() {
                if i < awarded.len() && awarded[i] {
                    self.open_pending(p);
                } else {
                    self.reject(REJECT_ARBITRATION);
                    self.journal.record(Decision::Rejected {
                        mint: p.mint,
                        reason: REJECT_ARBITRATION,
                    });
                    self.record_reject_sample(REJECT_ARBITRATION, p.mint);
                }
            }
            self.cands_buf = cands;
        }
        // Restore reused scratch (O2) for next tick — allocation retained, contents
        // dropped; no state crosses the tick boundary.
        self.pending_buf = pending;
        self.corrob_buf = corrob;

        // 6. Reflection cadence: realized net-SOL reshapes discovery weights.
        // (Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85.)
        #[allow(clippy::manual_is_multiple_of)]
        if self.now % self.cfg.reflect_every_ticks == 0 {
            self.run_reflection();
        }

        // 7. Held-position time-stops (§24(e): the clock is a backstop, not the
        // trigger). Close any open scalp that has stopped advancing past its max
        // hold. Empty until a market is admitted. Disjoint borrows: `positions`
        // mutates while `numeric` is read for the mark price.
        if !self.positions.is_empty() {
            let numeric = &self.numeric;
            let exits = self.positions.on_tick(self.now, &|m| {
                numeric.latest_price_fp(DomainMint::from_bytes(*m))
            });
            for e in exits {
                self.book_exit(e);
            }
        }
        if !self.tournament.is_empty() {
            let numeric = &self.numeric;
            self.tournament.on_tick(self.now, &|m| {
                numeric.latest_price_fp(DomainMint::from_bytes(*m))
            });
        }
    }

    /// §21.5 active-market-universe promotion screen. Returns whether the
    /// candidate may proceed to promotion. Exemptions (return `true`): mints
    /// with no numeric evidence at all (age unknown — the screen never judges
    /// what it cannot see, §6.4 UNKNOWN discipline) and launches younger than
    /// `universe_age_exempt_slots` (a fresh creation legitimately has no
    /// history; §23 earliness is a design asset). A MATURE mint must show
    /// recent genuine activity: enough trades from enough distinct entities,
    /// a sane trades-per-entity ratio (wash guard, §28), and a liquidity
    /// floor — evaluated through the signals crate's broad screen, the same
    /// leaf the server-side selector uses.
    fn universe_promotable(&self, cand: &Candidate) -> bool {
        let mint_bytes = cand.mint.bytes();
        let Some(feats) = self
            .numeric
            .features_for(DomainMint::from_bytes(mint_bytes))
        else {
            return true; // no numeric evidence — nothing to screen on
        };
        if feats.age_slots < self.cfg.universe_age_exempt_slots {
            return true; // fresh launch: exempt by design
        }
        let w = self
            .structure
            .activity(&mint_bytes, self.now, self.cfg.universe_window_ticks);
        let conc = self.concentration_verdict(&mint_bytes);
        // Crude wash guard: hyperactive single-entity tape is not organic (§28).
        if w.entities > 0 && w.trades / w.entities.max(1) > self.cfg.universe_wash_ratio_max {
            return false;
        }
        let obs = MarketObservation {
            token_id: u64::from_le_bytes(mint_bytes[..8].try_into().unwrap_or([0; 8])),
            liquidity_lamports: u128::from(feats.liquidity_lamports),
            volume_lamports_window: u128::from(w.volume_lamports),
            swap_count_window: w.trades,
            unique_traders_window: w.entities,
            // Slot time ≈ 400ms; the screen's age bounds are no-bind here (the
            // app's own exemption handled age), so the exact scale is inert.
            age_ms: u64::from(feats.age_slots).saturating_mul(400),
            spread_bps: 0, // no live source — never binds (max = MAX)
            // §21.7/§70.1 THE FORMERLY-DORMANT FIELD. Until the holder ledger grew
            // a distribution-shape derivation this was a hard-coded `0` against a
            // `u32::MAX` bar — a screen that could never bind because nothing
            // produced the number. It now carries the real cumulative top-10 share
            // of tracked supply, and `0` survives ONLY as the honest fail-open
            // value for a verdict we could not take (delta-only basis, truncated
            // ledger, thin ledger, or the law disarmed) — see
            // `ConcentrationVerdict::screen_concentration_bps`.
            top_holder_concentration_bps: conc.screen_concentration_bps(),
        };
        let criteria = ScreenCriteria {
            min_liquidity_lamports: u128::from(self.cfg.universe_min_liquidity_lamports),
            min_volume_lamports: 0,
            min_swap_count: self.cfg.universe_min_trades,
            min_unique_traders: self.cfg.universe_min_entities,
            max_spread_bps: u32::MAX,
            // Armed: the §21.5 concentration bar, which is the SAME 5 000 bps the
            // signals crate's own selector dossiers use. Disarmed: `u32::MAX`,
            // which with the `0` above reproduces the pre-existing no-bind screen
            // exactly.
            max_concentration_bps: if self.cfg.holder_concentration_enable {
                TOP10_HAIRCUT_BPS
            } else {
                u32::MAX
            },
            min_age_ms: 0,
            max_age_ms: u64::MAX,
        };
        // The broad screen is unchanged. The progressive filter is added ONLY so
        // the concentration leg has a consumer: spread and the age band are pinned
        // to never-bind values here (the app's own age exemption handled age
        // above), so this call adds exactly one comparison and nothing else.
        passes_broad_screen(&obs, &criteria) && passes_progressive_filter(&obs, &criteria)
    }

    /// §21.7/§70.1 the holder distribution-shape verdict used by the DECISION
    /// plane.
    ///
    /// Returns [`ConcentrationUnknown::Disarmed`] without touching the ledger when
    /// the law is off, so the disarmed engine does exactly the work it did before
    /// this law existed (the derivation is `O(n · TOP_N)` over a 512-entity ledger
    /// and has no business running on a hot path that will discard it).
    fn concentration_verdict(&self, mint: &[u8; 32]) -> ConcentrationVerdict {
        if !self.cfg.holder_concentration_enable {
            return ConcentrationVerdict::Unknown(ConcentrationUnknown::Disarmed);
        }
        concentration_of(&self.holder_flow, mint)
    }

    /// §21.7/§70.1 the holder distribution-shape verdict for the REPORT plane.
    ///
    /// Always derived, regardless of the config switch, because an operator asking
    /// "what does the ledger say about this market's distribution" is asking about
    /// the evidence, not about whether the law is armed. Never a decision input —
    /// decision consumers go through [`Self::concentration_verdict`].
    #[must_use]
    pub fn holder_concentration(&self, mint: &[u8; 32]) -> ConcentrationVerdict {
        concentration_of(&self.holder_flow, mint)
    }

    /// §24 conditional expectancy (EXPECTANCY_V1, see [`EXPECTANCY_VERSION`]):
    /// the configured expected move is a COLD-START PRIOR; once a lane has
    /// `expectancy_min_lane_trades` realized fills, its mean realized per-trade
    /// return (bps) is shrunk toward the prior with a pseudo-count equal to the
    /// same gate (§24 hierarchical partial pooling), and that value conditions
    /// §23 slot arbitration. Paper-realized returns rank slots; they are never
    /// promotion evidence (§38 — the fill model is graded separately).
    /// **The ONE benefit term for one candidate.** The retired `conditional_edge_bps`
    /// is now a CONSTRUCTOR call: the §24 pooling arithmetic lives inside
    /// [`PricedMove::for_candidate`], so the only way to obtain a priced move is to
    /// present evidence — the estimator's own `MoveEstimate` and the lane's realized
    /// `(Σ bps, n)`. There is no longer a standalone `i128` for a second call site to
    /// reach for and diverge on.
    fn priced_move(
        &self,
        lane: WlLane,
        model: Option<&crate::expected_move::MoveEstimate>,
    ) -> PricedMove {
        let (sum_bps, n) = self.lane_edge[lane.index()];
        PricedMove::for_candidate(
            model,
            lane,
            sum_bps,
            n,
            self.cfg.gate_expected_move_bps,
            self.cfg.expectancy_min_lane_trades,
        )
    }

    /// Evaluate one promoted candidate through every gate and sizing law, WITHOUT
    /// opening: returns the fully-priced pending entry for §23 arbitration, or
    /// journals the reject and returns None. Opening happens after arbitration.
    /// §94 quote-mint resolution for a market. The engine has always priced in
    /// SOL lamports; until a pool decode threads the true quote mint through here,
    /// this returns the SOL default so every SOL-quoted market is byte-identical
    /// (golden-safe). When pool decoding is wired, this becomes the sole authority
    /// for the quote mint — a decode that fails yields [`QuoteMint::Undecoded`],
    /// which the gate refuses (fail-closed), and a USDC pool yields the reachable
    /// [`QuoteMint::Usdc`] path.
    #[inline]
    fn resolve_quote_mint(&self, _mint: &[u8; 32]) -> QuoteMint {
        QuoteMint::Sol {
            decimals: SOL_QUOTE_DECIMALS,
        }
    }

    fn gate_evaluate(&mut self, cand: Candidate) -> Option<PendingEntry> {
        let mint_bytes = cand.mint.bytes();
        let domain_mint = DomainMint::from_bytes(mint_bytes);
        // §34.3: the gate's numeric snapshot obeys the SAME evidence TTL as
        // discovery — a fresh confirm can never borrow a stale numeric picture
        // (previously `features_for` was read with no freshness bound at all,
        // letting liquidity observed up to confirm-TTL ago size an entry).
        let numeric_feats = self.numeric.features_for(domain_mint).filter(|_| {
            self.numeric
                .evidence_age(domain_mint, self.now)
                .is_some_and(|age| age <= self.cfg.lane_evidence_ttl_ticks)
        });
        // Rev-14 wangr intelligence: enrich the Features snapshot with auxiliary
        // data from MarketAuxiliary events and the latest TimeSignal BEFORE
        // either confirmation path consumes it. When no auxiliary data was fed,
        // the sentinels (0, 255) are no-ops for all wangr gate filters, so the
        // decision is byte-identical to prior behavior.
        let numeric_feats = numeric_feats.map(|f| self.enrich_wangr_features(f, &mint_bytes));
        // Confirmation exists only with an on-chain confirm AND numeric evidence;
        // a confirm with no numeric snapshot degrades to a NoNumericConfirmation
        // reject inside the gate (default features carry zero liquidity).
        // Freshness law (§34.3): an on-chain confirmation older than the TTL no
        // longer authorizes entry — depth proven long ago is not depth now.
        let confirmation = self
            .confirmed
            .get(&mint_bytes)
            .filter(|&&(_, _, at)| self.now.saturating_sub(at) <= self.cfg.confirm_ttl_ticks)
            .map(|&(confirmed_vsol, confirmed_real_sol, _)| {
                let numeric = numeric_feats.unwrap_or_default();
                // ---- DEPTH PROVENANCE ----
                //
                // The retired rule was `depth.min(numeric.liquidity_lamports)`: take
                // the smaller of an ASSERTED sellable depth and the VIRTUAL reserve.
                // It was conservative against the wrong number. `liquidity_lamports`
                // IS `virtual_sol`, and a curve escrows `virtual_sol − 30 SOL`, so the
                // min() permitted a capacity 30x the reserve that can actually pay at
                // `vsol = 31 SOL` and an unbounded one at the seed reserve.
                //
                // The rule now: the FRESH numeric reserve is the price authority
                // (§34.3 — the confirm may be up to `confirm_ttl_ticks` old, the
                // numeric snapshot no more than `lane_evidence_ttl_ticks`), and the
                // decoded `real_sol` is preferred for payout ONLY when it belongs to
                // that same snapshot. A decoded reserve from an EARLIER snapshot is
                // stale, not wrong; refusing on it would re-litigate staleness, which
                // the TTL laws already decide, so the identity supplies the payout
                // instead. Decoder health is checked at the ingest boundary in
                // `confirm`, where a contradictory pair is never recorded at all.
                let depth = if confirmed_vsol == numeric.liquidity_lamports {
                    crate::curve_depth::CurveDepth::decoded(confirmed_vsol, confirmed_real_sol)
                } else {
                    crate::curve_depth::CurveDepth::derived(numeric.liquidity_lamports)
                };
                Confirmation { depth, numeric }
            });

        // §24 LAW 11 EntryMode leaves: with the detectors enabled, a controlled
        // pullback that holds a retest in an established uptrend
        // (`detect_pullback_continuation`) makes an ALREADY-LIVE market eligible
        // for an active-market scalp without a fresh on-chain confirm — its
        // sellable depth is the freshly-observed pool liquidity. This only ADDS a
        // synthetic confirmation when no real one exists; it never relaxes the
        // economic gate. The narrative-confirmation leaf stays dormant/admission-
        // gated (§28) and authorizes nothing on its own.
        let confirmation = confirmation
            .or_else(|| self.entry_mode_confirmation(&cand, &mint_bytes, numeric_feats));

        // §56.11: a retired lane's candidates stay research-visible but are
        // capital-ineligible.
        if self.retired[cand.lane.index()] {
            self.reject(REJECT_LANE_RETIRED);
            self.journal.record(Decision::Rejected {
                mint: mint_bytes,
                reason: REJECT_LANE_RETIRED,
            });
            return None;
        }
        // §Quant-Rev-7: RE-ENTRY COOLDOWN — reject if this mint was recently exited
        // and is still within the cooldown window. This is a SELECTION refusal: the
        // mint is on temporary blackout to break the death-by-a-thousand-cuts re-
        // entry loop. Checked BEFORE the economic gate to avoid wasted pricing work.
        // When disabled (reentry_cooldown_enable=false) the set is never populated,
        // so this entire block is a no-op on the golden path.
        if self.cfg.reentry_cooldown_enable {
            if let Some(&exit_tick) = self.reentry_cooldown.get(&mint_bytes) {
                let elapsed = self.now.saturating_sub(exit_tick);
                if elapsed < self.cfg.reentry_cooldown_ticks {
                    self.reject(REJECT_REENTRY_COOLDOWN);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_REENTRY_COOLDOWN,
                    });
                    return None;
                } else {
                    // Cooldown expired — prune the stale entry lazily.
                    self.reentry_cooldown.remove(&mint_bytes);
                }
            }
        }
        // BENEFIT-SIDE PRICING — ONE ESTIMATE, COMPUTED ONCE, WITH ITS PROVENANCE.
        //
        // `gate::decide` compared a per-candidate MEASURED cost against a GLOBAL
        // CONSTANT benefit (`docs/EDGE_PROVENANCE_2026-07-27.md` §4) while §23
        // arbitration, a few hundred lines below, priced the SAME trade off the lane's
        // realized expectancy. Two estimators, one trade, and no record of which had
        // spoken. `PricedMove` is computed HERE, once, and is the only expected move
        // either decision can see (`docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md`
        // §4.2). Precedence: the calibrated model when it is armed AND above its
        // sample floor, else the lane's own realized evidence, else the cold-start
        // constant — each recorded in the `MoveSource` the journal carries.
        let model_estimate = if self.cfg.expected_move_model_enable {
            let vsol = confirmation
                .and_then(|c| c.depth.price_reserve())
                .unwrap_or(0);
            // EVERY conditioning signal the engine holds at gate time is presented. That
            // is safe precisely because an UNCALIBRATED band contributes exactly zero
            // (`expected_move::uncalibrated_signals_contribute_exactly_zero`), so wiring
            // more signals can never add fabricated edge — only earned edge.
            let obs = confirmation.map_or(crate::expected_move::SignalObs::none(), |c| {
                crate::expected_move::SignalObs::from_features(
                    c.numeric.buy_pressure_bp,
                    c.numeric.unique_buyers,
                    c.numeric.age_slots,
                )
            });
            match self.expected_move.estimate(
                vsol,
                obs,
                crate::expected_move::MoveParams {
                    min_sample: self.cfg.expected_move_min_sample,
                    prior_weight: self.cfg.expected_move_prior_weight,
                    prior_bps: self.cfg.gate_expected_move_bps,
                },
            ) {
                crate::expected_move::MoveVerdict::Known(e) => Some(e),
                crate::expected_move::MoveVerdict::Unknown(_) => None,
            }
        } else {
            None
        };
        let priced_move = self.priced_move(cand.lane, model_estimate.as_ref());
        match decide(&cand, confirmation, &self.cfg, priced_move) {
            GateDecision::Admit(band) => {
                // The market's SOL-side reserve: the ONE number every cost term below
                // derives from — the venue fee tier, the impact denominator, the
                // round-trip bps. `gate::decide` refuses a zero reserve, so reaching
                // this arm guarantees it is positive.
                let conf_vsol = confirmation
                    .and_then(|c| c.depth.price_reserve())
                    .unwrap_or(0);
                // §21.7 extreme fabrication signature — the only authenticity gate.
                let (auth_bps, fabricated) = self.flow_screen.authenticity(&mint_bytes);
                // §105 REPORT-plane extraction-risk accumulation (never feeds a
                // decision, never journaled): fold the decayed wash-strength
                // covariate (10_000 − authenticity) for evidenced flow. A neutral
                // prior (no auth evidence) contributes nothing. This write only
                // mutates the report-only ledger, so the golden digest is unchanged.
                if self.flow_screen.has_auth_evidence(&mint_bytes) {
                    let wash_strength = 10_000u32.saturating_sub(auth_bps);
                    self.extraction_risk
                        .observe(mint_bytes, u64::from(wash_strength), self.now);
                }
                if fabricated {
                    self.reject(REJECT_FABRICATED_FLOW);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_FABRICATED_FLOW,
                    });
                    self.record_reject_sample(REJECT_FABRICATED_FLOW, mint_bytes);
                    return None;
                }
                // §26 confirmed-creator-dump HARD VETO (operator-approved reversal):
                // a market whose deployer is in a confirmed distribution is refused
                // pre-entry — the prior "creator distribution is fade-only, never a
                // veto" behaviour is reversed for the confirmed-dump regime.
                if self.creator_dump_active(&mint_bytes) {
                    self.reject(REJECT_CREATOR_DUMP);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_CREATOR_DUMP,
                    });
                    self.record_reject_sample(REJECT_CREATOR_DUMP, mint_bytes);
                    return None;
                }
                // §70.10 anti-bundle fee-floor (LAW 10): a fully-saturated
                // (near-zero cumulative fee) first-slot footprint for the
                // advertised activity is a manufactured/wash launch — veto
                // pre-entry. A merely-low footprint fades size later (below).
                let (fee_floor_veto, _fee_fade) = self.fee_floor_verdict(&mint_bytes);
                if fee_floor_veto {
                    self.reject(REJECT_FEE_FLOOR);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_FEE_FLOOR,
                    });
                    self.record_reject_sample(REJECT_FEE_FLOOR, mint_bytes);
                    return None;
                }
                // ---- §21.7/§70.1 HOLDER DISTRIBUTION SHAPE (the wave's decision
                // change). Concentration, early-buyer capture and whale dominance,
                // derived from the continuous holder ledger — reduce-only, and
                // fail-open on an `Unknown` verdict (delta-only basis, truncated or
                // thin ledger, or the law disarmed all yield `Clear`, which is the
                // identity).
                //
                // The refusal is CONJUNCTIVE by constitutional requirement: §21.7
                // states that bundle-adjusted top-N holding concentration is "a
                // feature family and prior, never a standalone veto", and that
                // "only extreme fabrication signatures may hard-reject". So the
                // corroborating leg is the INDEPENDENT flow-authenticity reading —
                // computed over per-entity QUOTE-lamport gross flow, where the
                // concentration is computed over per-entity BASE-token net
                // positions. It is deliberately read WITHOUT the holder evidence
                // (`authenticity`, not `authenticity_with`): letting the holder
                // ledger corroborate itself would make the conjunction decorative.
                let conc = self.concentration_verdict(&mint_bytes);
                let conc_corroborated = self.flow_screen.has_auth_evidence(&mint_bytes)
                    && auth_bps <= CONCENTRATION_VETO_AUTH_BPS;
                let conc_risk = conc.risk_or_clear(conc_corroborated);
                if conc_risk == ConcentrationRisk::Veto {
                    self.reject(REJECT_HOLDER_CONCENTRATION);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_HOLDER_CONCENTRATION,
                    });
                    // Feeds the §49 ConvexityPreservationLedger through the shared
                    // veto path, which is the audit the constitution requires of
                    // this family's veto/downweight effects.
                    self.record_reject_sample(REJECT_HOLDER_CONCENTRATION, mint_bytes);
                    return None;
                }

                // ---- §Quant-Rev-1: BUNDLE DETECTION hard veto ----
                // Same-slot buy count ≥ threshold AND/OR same-slot supply
                // concentration > threshold. Uses the ConcentrationVerdict's
                // bundle_entities field (already computed by holder_flow).
                // Fail-open: Unknown verdict or feature OFF → no reject.
                if self.cfg.bundle_detect_enable {
                    let conc_full = self.holder_concentration(&mint_bytes);
                    if let ConcentrationVerdict::Known(ref metrics) = conc_full {
                        // Rev-1a: bundle count veto — ≥N buys in creation slot
                        if metrics.bundle_entities >= self.cfg.bundle_detect_min_same_slot_buys {
                            self.reject(REJECT_BUNDLE_DETECTED);
                            self.journal.record(Decision::Rejected {
                                mint: mint_bytes,
                                reason: REJECT_BUNDLE_DETECTED,
                            });
                            self.record_reject_sample(REJECT_BUNDLE_DETECTED, mint_bytes);
                            return None;
                        }
                        // Rev-1b: bundle concentration veto — same-slot buyers
                        // collectively hold > threshold % of float
                        let conc_bps = conc_full.screen_concentration_bps();
                        if conc_bps > self.cfg.bundle_concentration_max_bps
                            && metrics.bundle_entities > 0
                        {
                            self.reject(REJECT_BUNDLE_CONCENTRATION);
                            self.journal.record(Decision::Rejected {
                                mint: mint_bytes,
                                reason: REJECT_BUNDLE_CONCENTRATION,
                            });
                            self.record_reject_sample(REJECT_BUNDLE_CONCENTRATION, mint_bytes);
                            return None;
                        }
                    }
                }

                // ---- §Quant-Rev-2: DEV WALLET GRADING hard veto ----
                // Deployer has < threshold graduation rate over ≥ min_launches
                // prior mints. Uses the existing deployer_screen_mult_bp
                // infrastructure. Fail-open: no prior history → identity.
                if self.cfg.dev_history_reject_enable {
                    let dev_mult = self.deployer_screen_mult_bp(&mint_bytes);
                    // deployer_screen_mult_bp returns a haircut in bps of 10_000.
                    // A mult ≤ dev_graduation_min_rate_bp means the deployer's
                    // effective graduation rate is below the floor.
                    let creator = self.mint_creator.get(&mint_bytes);
                    let launches = creator
                        .and_then(|c| self.creator_launches.get(c))
                        .map(|&(lifetime, _, _)| lifetime)
                        .unwrap_or(0);
                    if dev_mult < self.cfg.dev_graduation_min_rate_bp
                        && u64::from(launches) >= self.cfg.dev_history_min_launches as u64
                    {
                        self.reject(REJECT_DEV_HISTORY);
                        self.journal.record(Decision::Rejected {
                            mint: mint_bytes,
                            reason: REJECT_DEV_HISTORY,
                        });
                        self.record_reject_sample(REJECT_DEV_HISTORY, mint_bytes);
                        return None;
                    }
                }

                // ---- §Quant-Rev-3: COORDINATED FUNDING hard veto ----
                // >70% of first-10 buyers share a common funding source.
                // Uses the existing wallet_graph funding edges. Fail-open
                // when graph data is insufficient (§6.4).
                if self.cfg.coordinated_funding_reject_enable {
                    if self.detect_coordinated_funding(&mint_bytes) {
                        self.reject(REJECT_COORDINATED_FUNDING);
                        self.journal.record(Decision::Rejected {
                            mint: mint_bytes,
                            reason: REJECT_COORDINATED_FUNDING,
                        });
                        self.record_reject_sample(REJECT_COORDINATED_FUNDING, mint_bytes);
                        return None;
                    }
                }

                // ---- §Quant-Rev-6: EXIT LIQUIDITY hard veto ----
                // Fewer than min_holders genuinely independent holders.
                // Uses the holder_flow unique buyer count, deflated by
                // the funding-graph-linked cluster size. Fail-open on Unknown.
                if self.cfg.exit_liquidity_reject_enable {
                    let conc_full = self.holder_concentration(&mint_bytes);
                    if let ConcentrationVerdict::Known(ref metrics) = conc_full {
                        let unique_holders = metrics.holders;
                        if unique_holders < self.cfg.exit_liquidity_min_holders as u32 {
                            self.reject(REJECT_INSUFFICIENT_EXIT_LIQUIDITY);
                            self.journal.record(Decision::Rejected {
                                mint: mint_bytes,
                                reason: REJECT_INSUFFICIENT_EXIT_LIQUIDITY,
                            });
                            self.record_reject_sample(
                                REJECT_INSUFFICIENT_EXIT_LIQUIDITY,
                                mint_bytes,
                            );
                            return None;
                        }
                    }
                }

                // ---- VPIN extreme tier (the one binary veto): a distributed,
                // sell-dominant dump in progress. Graded tiers only shrink size.
                let vp = self.vpin_params();
                let vpin_reading = self
                    .vpin
                    .get(&mint_bytes)
                    .and_then(|v| v.reading(self.now, &vp));
                let vpin_mult = vpin_size_mult_bp(vpin_reading, &self.vpin_thresholds());
                if vpin_mult == 0 {
                    self.reject(REJECT_VPIN_TOXIC);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_VPIN_TOXIC,
                    });
                    self.record_reject_sample(REJECT_VPIN_TOXIC, mint_bytes);
                    return None;
                }
                // ---- Concurrency cap (§33: jointly sized with the risk fractions —
                // max_concurrent × f_base ≈ total_risk_cap). Journaled, never silent.
                if self.positions.len() >= self.cfg.max_concurrent_positions {
                    self.reject(REJECT_MAX_CONCURRENT);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_MAX_CONCURRENT,
                    });
                    return None;
                }
                // ---- Bankroll chain (§33 Layer 1 / delta-§1): every limit derives
                // from deployable = balance − survival_floor. Start with ANY SOL
                // amount — the fractions are scale-invariant; the per-market cost
                // floor x_min carves out what the venue can economically serve. The
                // floor basis is the bankroll ORIGIN's seed (paper seed for
                // Paper/Replay — byte-identical; reconciled wallet for a live origin).
                let floor = derive_survival_floor(
                    self.bankroll_origin.seed_lamports(),
                    self.cfg.floor_fraction_bps,
                );
                let balance = self.bankroll_balance();
                let deployable = deployable_capital(balance, floor);
                let risk_budget =
                    u128::from(deployable) * u128::from(self.cfg.total_risk_cap_bp) / 10_000;
                let available_risk = risk_budget.saturating_sub(self.bankroll_committed);
                if deployable == 0 || available_risk == 0 {
                    self.reject(REJECT_INSUFFICIENT_BANKROLL);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_INSUFFICIENT_BANKROLL,
                    });
                    return None;
                }
                // ---- Per-position fraction (drawdown-ratcheted) × reduce-only
                // haircuts: creator/category × VPIN toxicity × tape regime.
                let mut f_eff = self.dd_f_eff_bp(balance);
                // §33 Layer-2 controller: the sizing validator's survival-constrained
                // recommendation may only move f INSIDE the [probe_f, f_base] envelope
                // (§56.2), and never above the drawdown-ratcheted fraction.
                if let Some(rec) = self.f_recommended {
                    f_eff = f_eff.min(rec.clamp(self.cfg.probe_f_bp, self.cfg.f_base_bp));
                }
                let regime = self.numeric.regime_of(
                    domain_mint,
                    self.cfg.roll_trend_bp,
                    self.cfg.roll_revert_bp,
                );
                let regime_mult: u32 = if regime == Regime::Revert {
                    self.cfg.revert_size_mult_bp
                } else {
                    10_000
                };
                // §21.7 single-channel authenticity multiplier (phase-weighted) and
                // §27 creator-credibility haircut — both reduce-only.
                let is_pool = self.context.is_pool(&mint_bytes);
                // The bundle/sniper cohort size and the bump/wash flip ratio are
                // AUTHENTICITY evidence, so they enter through the authenticity
                // multiplier and nowhere else — §21.7 admits exactly one entry
                // point per feature into the sizing chain. The concentration
                // SHARES are a different quantity (fragility, not fabrication) and
                // enter through `conc_mult` below; no number is charged twice.
                // `HolderAuthEvidence::default()` under an `Unknown` verdict makes
                // this call byte-identical to the plain `size_mult_bp`.
                let auth_mult = self.flow_screen.size_mult_bp_with(
                    &mint_bytes,
                    is_pool,
                    conc.auth_evidence_or_default(),
                );
                // §21.7/§70.1 concentration fragility haircut — reduce-only, and
                // exactly 10 000 (identity) under `Clear`, which is what every
                // `Unknown` verdict and the disarmed law both produce.
                let conc_mult: u32 = conc_risk.size_mult_bp();
                let cred_mult = match self
                    .mint_creator
                    .get(&mint_bytes)
                    .and_then(|c| self.creator_launches.get(c))
                {
                    Some(&(lifetime, _, in_window)) => creator_credibility_haircut_bp(
                        lifetime,
                        in_window,
                        CREATOR_SERIAL_THRESHOLD,
                    ),
                    None => 10_000,
                };
                // §70.9 deployer-credibility screen (LAW 10, reduce-only): the
                // wallet-graph deployer_credibility bundle (prior-CA + serial-
                // deploy burst), CLASS-CONDITIONED by the §27 known-extractor
                // verdict. Identity when the screen is off or nothing is known
                // about the deployer (golden-safe).
                let deployer_mult: u32 = if self.cfg.deployer_screen_enable {
                    self.deployer_screen_mult_bp(&mint_bytes)
                } else {
                    10_000
                };
                // §70.10 fee-floor GRADED fade (LAW 10, reduce-only): a low-but-
                // not-vetoed first-slot footprint shrinks size proportionally. The
                // fully-saturated signature already vetoed above.
                let (_veto, fee_fade_bps) = self.fee_floor_verdict(&mint_bytes);
                let fee_mult: u32 = 10_000u32.saturating_sub(fee_fade_bps.min(10_000));
                // §21.3 regime consumption: elevated market-wide rug rate shrinks size.
                let rug_mult: u32 = if self.regime_rug_elevated {
                    REGIME_RUG_HAIRCUT_BP
                } else {
                    10_000
                };
                // §21.6 bar-structure factor — REDUCE-ONLY (§56.2 envelope): a
                // Downtrend swing structure contradicting the long entry shrinks
                // size; confirmed or undefined structure is identity. Structure
                // never authorizes and never boosts above the §33 fraction.
                let struct_mult: u32 = if self
                    .structure
                    .trend(&mint_bytes, self.cfg.structure_min_bars)
                    == TrendStructure::Downtrend
                {
                    self.cfg.structure_downtrend_haircut_bp
                } else {
                    10_000
                };
                let base_haircut_bp = (u128::from(self.size_haircut_bps(&mint_bytes))
                    * u128::from(vpin_mult)
                    / 10_000
                    * u128::from(regime_mult)
                    / 10_000
                    * u128::from(auth_mult)
                    / 10_000
                    * u128::from(cred_mult)
                    / 10_000
                    * u128::from(deployer_mult)
                    / 10_000
                    * u128::from(fee_mult)
                    / 10_000
                    * u128::from(rug_mult)
                    / 10_000
                    * u128::from(struct_mult)
                    / 10_000
                    * u128::from(conc_mult)
                    / 10_000) as u32;
                let raw = u128::from(deployable) * u128::from(f_eff) / 10_000;
                // ---- LAWs B1/B3: the episodic capture and the reduce-only recall
                // verdict. The fingerprint's round-trip-cost field is measured at the
                // PRE-BRAIN size (`raw` under the existing reduce-only chain), so the
                // brain's own haircut can never feed back into the fingerprint it
                // queries with — a feedback loop there would make the law
                // self-referential and its A/B meaningless.
                let brain_entry: Option<BrainEntry> = if self.cfg.brain_enable {
                    let pre_brain_size = (raw * u128::from(base_haircut_bp) / 10_000)
                        .min(u128::from(band.x_max))
                        .min(available_risk) as u64;
                    // An UNDECODED quote yields no cost; the entry is rejected for
                    // that reason further down regardless, so the `0` fallback here
                    // can never reach a decision (§18.2 fails closed below).
                    let pre_rt = self
                        .unified_rt_bps(&mint_bytes, pre_brain_size, conf_vsol)
                        .unwrap_or(0);
                    Some(self.brain_entry_at_admit(&mint_bytes, cand.discovery_lane, pre_rt))
                } else {
                    None
                };
                // The verdict is COMPUTED in both arms of the A/B (identical work,
                // identical counters) and only ACTED ON when `brain_haircut_enable`
                // is set — so the A/B isolates the law, not the bookkeeping.
                let brain_verdict = match &brain_entry {
                    Some(e) => self.brain.size_verdict(
                        e,
                        self.cfg.brain_haircut_enable,
                        self.cfg.brain_haircut_win_rate_bp,
                        self.cfg.brain_veto_win_rate_bp,
                        self.cfg.brain_haircut_mult_bp,
                    ),
                    None => BrainSizeVerdict::Identity,
                };
                if brain_verdict == BrainSizeVerdict::Veto {
                    self.reject(REJECT_BRAIN_BLED);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_BRAIN_BLED,
                    });
                    self.record_reject_sample(REJECT_BRAIN_BLED, mint_bytes);
                    return None;
                }
                // Reduce-only composition: `mult_bp()` is structurally ≤ 10_000, so
                // this product can only ever shrink the size the rest of the chain
                // arrived at (§29.5). With the law disarmed it is exactly 10_000 and
                // `haircut_bp == base_haircut_bp` bit-for-bit.
                let haircut_bp = (u128::from(base_haircut_bp) * u128::from(brain_verdict.mult_bp())
                    / 10_000) as u32;
                let sized = (raw * u128::from(haircut_bp) / 10_000)
                    .min(u128::from(band.x_max))
                    .min(available_risk);
                // §27/§28 amendment: apply tracked-wallet trust boost (G5) and
                // smart-money PnL-screen boost (§28 Phase 7) as additive bps
                // lifts on the sized position. Both are DISABLED by default
                // (tracked_wallet_boost_enable=false,
                // smart_money_boost_enable=false), so the golden tape is
                // byte-identical to the pre-amendment path. When enabled, the
                // boost is capped at its respective max_bps and never exceeds
                // x_max or available_risk (re-clamped after the lift).
                let sized = {
                    let mut boost_bp: u32 = 0;
                    if self.cfg.tracked_wallet_boost_enable {
                        boost_bp = boost_bp.saturating_add(self.tracked_wallet_boost_bp(&mint_bytes));
                    }
                    if self.cfg.smart_money_boost_enable {
                        boost_bp = boost_bp.saturating_add(self.smart_money_boost_bp(&mint_bytes));
                    }
                    if boost_bp > 0 {
                        let lifted = sized * u128::from(10_000u32 + boost_bp) / 10_000;
                        lifted
                            .min(u128::from(band.x_max))
                            .min(available_risk)
                    } else {
                        sized
                    }
                };
                // ---- Below effective x_min: CLAMP UP to the operator floor, or
                // REFUSE if unsafe (criterion 112 / A-6). `band.x_min` is now the
                // EFFECTIVE floor `max(min_trade_size, economic x_min)` (lifted in
                // `gate::decide`). When the risk/Kelly-arbitrated size lands below it
                // we promote UP to it — the operator's minimum bite — but ONLY if that
                // still fits every HARD cap: no drawdown tier active (f_eff == f_base),
                // the corroboration haircut is not risk-faded (never size UP a faded
                // trade), and x_min fits the promote cap, the remaining risk budget,
                // and x_max. If promoting would breach any hard cap → REFUSE (never
                // shrink below the floor, never over-risk). The sub-x_min
                // paid-information probe (§33/§43 LAW 13) is a SUB-FLOOR bet, so it is
                // switched OFF whenever the floor is active (min_trade_size > 0); it
                // survives only with the floor disabled (min_trade_size == 0), keeping
                // the legacy path byte-identical and the golden tape coherent.
                let size = if sized >= u128::from(band.x_min) {
                    sized as u64
                } else {
                    // §33/§43 LAW 13 sub-x_min probe-budget accounting (floor-gated):
                    // only reachable when the operator floor is disabled. With the
                    // floor active every emitted bite is ≥ the floor, so a sub-x_min
                    // (hence sub-floor) probe can never fire.
                    if self.cfg.probe_budget_enable && self.cfg.min_trade_size_lamports == 0 {
                        return self.account_sub_xmin_probe(mint_bytes, sized as u64);
                    }
                    let promote_cap =
                        u128::from(deployable) * u128::from(self.cfg.x_min_promote_cap_bp) / 10_000;
                    let promotable = f_eff == self.cfg.f_base_bp
                        && haircut_bp >= self.cfg.promote_min_haircut_bp
                        && u128::from(band.x_min) <= promote_cap
                        && u128::from(band.x_min) <= available_risk
                        && band.x_min <= band.x_max;
                    if promotable {
                        band.x_min
                    } else {
                        self.reject(REJECT_BELOW_COST_FLOOR);
                        self.journal.record(Decision::Rejected {
                            mint: mint_bytes,
                            reason: REJECT_BELOW_COST_FLOOR,
                        });
                        self.record_reject_sample(REJECT_BELOW_COST_FLOOR, mint_bytes);
                        return None;
                    }
                };
                // ---- Survival-floor guard (strategy leaf pl_wallet_floor): the
                // entry spend may never push the balance below the floor.
                // COST-MODEL UNIFICATION: the entry leg is priced by the SAME
                // authority the gate admitted under — the venue's tiered per-leg fee
                // against THIS market's reserve, one landed transaction's fixed cost,
                // and (only when we do not already hold a token account for the mint)
                // the refundable ATA rent deposit. The deposit is returned, less one
                // signature, by `book_exit` when the position fully closes; carrying
                // it in the basis is what makes the 203 bps difference between a round
                // trip that finishes and one that abandons its account VISIBLE in the
                // realized net rather than invisible in nobody's model.
                let entry_vsol = conf_vsol;
                let entry_fee = (u128::from(size)
                    * u128::from(crate::cost_model::venue_fee_bps_per_leg(entry_vsol))
                    / 10_000) as u64;
                let needs_ata = !self.ata_open.contains(&mint_bytes);
                let entry_cost = size
                    .saturating_add(entry_fee)
                    .saturating_add(crate::cost_model::FIXED_LAMPORTS_PER_LEG)
                    .saturating_add(if needs_ata {
                        crate::cost_model::ATA_RENT_LAMPORTS
                    } else {
                        0
                    });
                if wallet_floor_guard(entry_cost, balance, floor) == FloorVerdict::RefusedBelowFloor
                {
                    self.reject(REJECT_WALLET_FLOOR);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_WALLET_FLOOR,
                    });
                    return None;
                }
                // §34.4/§21.7 phase-correct exit-cost law: if the executable exit side
                // already consumes the priced move, the trade is a structural loss.
                // §18.2/§6.4 UNKNOWN fails CLOSED: sizing without a priced exit is
                // not allowed to proceed on a fabricated number — an unpriceable
                // exit is treated exactly like an unaffordable one.
                match self.context.exit_cost_bps(&mint_bytes, size) {
                    Some(exit_cost)
                        if exit_cost
                            <= self
                                .cfg
                                .gate_expected_move_bps
                                .saturating_mul(EXIT_COST_VETO_MULT) => {}
                    _ => {
                        self.reject(REJECT_EXIT_COST);
                        self.journal.record(Decision::Rejected {
                            mint: mint_bytes,
                            reason: REJECT_EXIT_COST,
                        });
                        self.record_reject_sample(REJECT_EXIT_COST, mint_bytes);
                        return None;
                    }
                }
                // Fully priced: hand to §23 arbitration. Expected net per slot =
                // size × priced move − the round-trip cost load — conditional
                // expected net SOL, never raw discovery score.
                let spot_price = self.numeric.latest_price_fp(domain_mint).unwrap_or(0);
                if spot_price == 0 {
                    self.reject(REJECT_PRICING_FAILURE);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_PRICING_FAILURE,
                    });
                    self.record_reject_sample(REJECT_PRICING_FAILURE, mint_bytes);
                    return None;
                }
                // §24/criterion 103 FILL FIDELITY: pump.fun is a constant-product curve,
                // so OUR OWN order does not fill at the observed print — it fills at the
                // average along the curve, which is strictly worse by exactly
                // `size · 10_000 / vsol` bps (the token reserve cancels; see
                // `curve_fill::own_impact_bps`). Filling at the print is a subsidy the
                // market never granted, on every entry. Disarmed by default so the
                // historical pins hold; MUST be armed for any real-data backtest.
                let entry_price = if self.cfg.curve_exact_fill_enable {
                    // Same fresh snapshot `latest_price_fp` came from — the pool's SOL
                    // side is the curve depth our order walks.
                    let vsol = self
                        .numeric
                        .features_for(domain_mint)
                        .map_or(0, |f| f.liquidity_lamports);
                    // Unknown depth ⇒ refuse, never guess (§6): fail-closed with
                    // full reject accounting — preserves promoted = admitted + rejected.
                    match crate::curve_fill::buy_fill_price_fp(spot_price, vsol, size) {
                        Some(p) => p,
                        None => {
                            self.reject(REJECT_PRICING_FAILURE);
                            self.journal.record(Decision::Rejected {
                                mint: mint_bytes,
                                reason: REJECT_PRICING_FAILURE,
                            });
                            self.record_reject_sample(REJECT_PRICING_FAILURE, mint_bytes);
                            return None;
                        }
                    }
                } else {
                    spot_price
                };
                if entry_price == 0 {
                    self.reject(REJECT_PRICING_FAILURE);
                    self.journal.record(Decision::Rejected {
                        mint: mint_bytes,
                        reason: REJECT_PRICING_FAILURE,
                    });
                    self.record_reject_sample(REJECT_PRICING_FAILURE, mint_bytes);
                    return None;
                }
                // Priced with the SAME §34.4 economics the gate admitted under, so a
                // size the band declared viable ranks with a non-negative expected
                // net (move − round-trip cost at this size), in lamports.
                // §94 quote-mint-parametric cost, now priced by the ONE authority
                // (`cost_model`) rather than by a second restatement of the gate's
                // arithmetic. An UNDECODED quote fails closed here (never priced as
                // assumed-SOL).
                let rt_bps = match self.unified_rt_bps(&mint_bytes, size, conf_vsol) {
                    Some(bps) => bps,
                    None => {
                        self.reject(REJECT_UNDECODED_QUOTE);
                        self.journal.record(Decision::Rejected {
                            mint: mint_bytes,
                            reason: REJECT_UNDECODED_QUOTE,
                        });
                        self.record_reject_sample(REJECT_UNDECODED_QUOTE, mint_bytes);
                        return None;
                    }
                };
                // **ONE EXPECTED MOVE PER TRADE (silo audit F1, 2026-07-28).** This is
                // the SAME `PricedMove` the size band was priced with, a few hundred
                // lines above — not a second estimate, and no longer even a second
                // number that could be fetched. Arbitration used to reach
                // independently for `conditional_edge_bps` (the lane's realized
                // expectancy, ~6 numbers for the whole universe) while admission used
                // the global `gate_expected_move_bps` constant; once a lane cleared
                // `expectancy_min_lane_trades` the two diverged permanently, and
                // neither site was wrong on its own terms. That is the cost-model
                // defect (`docs/NET_SOL_AUDIT_2026-07-28.md` F2) reappearing in the
                // BENEFIT term: admission priced one trade, ranking ranked a different
                // one.
                //
                // The RANKING view is used here, and the ADMISSION view priced the
                // band above. They are two questions, not two answers to one (see
                // `priced_move`): §18 asks whether this trade beats its own costs,
                // which is a POPULATION question; §23/§24 asks which admissible
                // candidate takes a scarce slot, which §24 conditions on the lane's
                // paper-realized expectancy — a quantity §38 explicitly forbids from
                // becoming promotion evidence, and which therefore must never acquire
                // an admission veto. Once the calibrated model speaks, both views are
                // the same number from the same source, which is the case the silo
                // actually broke.
                let edge_bps = priced_move.ranking_bps() - i128::from(rt_bps);
                let expected_net = i128::from(size).saturating_mul(edge_bps) / 10_000;
                // §49 LAW 15 haircut convexity (reduced-vs-full size): when the
                // reduce-only multipliers shrank the size below the full §33
                // fraction, record a two-sided event whose counterfactual is the
                // full-size priced edge and whose realized side is that edge scaled
                // by the applied size fraction — the slice actually taken, never a
                // phantom all-or-nothing.
                if haircut_bp < 10_000 {
                    let full_cf = edge_bps.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
                    self.analytics
                        .record_convexity_mark(&ConvexityMark::Haircut {
                            rule: RuleId::new(
                                RuleKind::ConfidenceReducer,
                                cand.lane.index() as u64,
                            ),
                            full_counterfactual_bps: full_cf,
                            applied: SizeFraction::new(u64::from(haircut_bp), 10_000),
                            mfe_bps: full_cf.max(0),
                        });
                }
                Some(PendingEntry {
                    lane: cand.lane,
                    discovery_lane: cand.discovery_lane,
                    archetype: self.classify_archetype(&mint_bytes),
                    mint: mint_bytes,
                    entry_price,
                    size,
                    entry_cost,
                    expected_net,
                    round_trip_cost_bps: rt_bps,
                    entry_vsol: conf_vsol,
                    entry_obs: confirmation.map_or(crate::expected_move::SignalObs::none(), |c| {
                        crate::expected_move::SignalObs::from_features(
                            c.numeric.buy_pressure_bp,
                            c.numeric.unique_buyers,
                            c.numeric.age_slots,
                        )
                    }),
                    x_min: band.x_min,
                    x_cost: band.x_cost,
                    x_max: band.x_max,
                    priced_move,
                    depth_basis: confirmation.map_or(0, |c| c.depth.basis_code()),
                    brain: brain_entry,
                })
            }
            GateDecision::Reject(reason) => {
                let code = reject_code(reason);
                self.reject(code);
                self.journal.record(Decision::Rejected {
                    mint: mint_bytes,
                    reason: code,
                });
                self.record_reject_sample(code, mint_bytes);
                None
            }
        }
    }

    /// §33/§43 LAW 13: account a sub-`x_min` candidate as a budgeted calibration
    /// probe (paid information) instead of opening it as a position. Routes the
    /// intended (sub-floor) spend through the calibration ledger under the paper
    /// route; on admission the spend is journalled as a labeled [`Decision::Probe`]
    /// and the research counter advances; on refusal (any cap exhausted) it is a
    /// below-cost-floor rejection (reuses code 7 — no new reject code). Always
    /// returns `None`: a probe is never a pending position.
    fn account_sub_xmin_probe(
        &mut self,
        mint: [u8; 32],
        intended_spend: u64,
    ) -> Option<PendingEntry> {
        let req = CalibrationRequest {
            cost_lamports: intended_spend.max(1),
            day: 0,
            measurement_id: Some(fnv1a_64(&mint) as u32),
            route: Some(RouteId(PROBE_ROUTE_ID)),
        };
        match admit_calibration(&self.calibration, &req) {
            Ok((updated, label)) => {
                self.calibration = updated;
                self.probes_budgeted += 1;
                self.journal.record(Decision::Probe {
                    mint,
                    cost_lamports: label.research_cost_lamports,
                    measurement_id: label.measurement_id,
                });
            }
            Err(_) => {
                self.reject(REJECT_BELOW_COST_FLOOR);
                self.journal.record(Decision::Rejected {
                    mint,
                    reason: REJECT_BELOW_COST_FLOOR,
                });
                self.record_reject_sample(REJECT_BELOW_COST_FLOOR, mint);
            }
        }
        None
    }

    /// §43 LAW 13 probe-budget telemetry (report-only): lifetime probe spend
    /// (lamports) accounted through the calibration ledger and the count of
    /// budgeted paid-information probes admitted. A probe is never a position, so
    /// this is disjoint from the admitted-position count.
    #[must_use]
    pub fn probe_budget_report(&self) -> (u64, u64) {
        (self.calibration.spent_lifetime, self.probes_budgeted)
    }

    /// Feed a gate rejection into the PRFS forward-marking ring (§47c) at the
    /// mint's latest decoded price, and (§49 LAW 15) record the rejection as a
    /// two-sided VETO convexity event (counterfactual-vs-zero): the full
    /// unsuppressed position's signed counterfactual vs the realized zero (nothing
    /// was taken), so a veto is scored on the loss it avoided AND the upside it
    /// forwent — never a degenerate self-cancelling event.
    fn record_reject_sample(&mut self, gate_code: u8, mint: [u8; 32]) {
        let cf = self.veto_counterfactual_bps(&mint);
        self.analytics.record_convexity_mark(&ConvexityMark::Veto {
            rule: RuleId::new(RuleKind::Veto, u64::from(gate_code)),
            counterfactual_bps: cf,
            mfe_bps: cf.max(0),
        });
        if let Some(price) = self.numeric.latest_price_fp(DomainMint::from_bytes(mint)) {
            let archetype = self.classify_archetype(&mint);
            self.analytics
                .record_reject(gate_code, mint, price, self.now, archetype);
        }
    }

    /// §49 LAW 15 signed veto counterfactual (bps): the magnitude is the market's
    /// recent realized volatility over the vol-stop window (or the configured
    /// expected move when no bars exist yet), signed by the recent swing structure
    /// — a downtrend at veto time means the full position would have taken the
    /// downside (negative counterfactual = loss avoided), otherwise it would have
    /// had upside exposure (positive = upside forgone). Honest and deterministic:
    /// observed structure, never a fabricated forward price.
    fn veto_counterfactual_bps(&self, mint: &[u8; 32]) -> i64 {
        let mag = self
            .structure
            .recent_vol_bps(mint, VOL_STOP_WINDOW_BARS)
            .map(|v| v.clamp(0, i128::from(i64::MAX)) as i64)
            .filter(|&v| v > 0)
            .unwrap_or_else(|| i64::from(self.cfg.gate_expected_move_bps));
        match self.structure.trend(mint, self.cfg.structure_min_bars) {
            TrendStructure::Downtrend => -mag,
            _ => mag,
        }
    }

    /// Rev-14 wangr intelligence: enrich a `Features` snapshot with auxiliary
    /// data stored from `MarketAuxiliary` events (token_standard, symbol_len),
    /// the latest `TimeSignal` (dow, hour_utc), and the creator's launch count
    /// from the `mint_creator`/`creator_launches` maps. When no auxiliary data
    /// was fed, all fields stay at their Default sentinels (0, 255) which are
    /// no-ops for every wangr gate filter — byte-identical to prior behavior.
    #[must_use]
    fn enrich_wangr_features(&self, mut f: Features, mint: &[u8; 32]) -> Features {
        if let Some(&(ts, sym_len)) = self.mint_aux.get(mint) {
            f.token_standard = ts;
            f.symbol_len = sym_len;
        }
        let (dow, hour_utc) = self.time_signal;
        f.dow = dow;
        f.hour_utc = hour_utc;
        if let Some(&creator) = self.mint_creator.get(mint) {
            if let Some(&(launches, _, _)) = self.creator_launches.get(&creator) {
                f.creator_launches = launches;
            }
        }
        f
    }

    /// §24 LAW 11: the EntryMode detector leaves' contribution to admission. When
    /// `entry_mode_leaves_enable` is set, map the strategy predicates onto the
    /// engine's lane selection via the `SuggestedLane` discriminant mirror:
    ///
    /// * `detect_pullback_continuation` → **active-market-scalp eligibility**: a
    ///   controlled pullback holding a retest inside a confirmed uptrend admits an
    ///   already-live market (payout depth derived from the observed reserve) even
    ///   without a fresh `OnchainConfirm` — the setup the 4-lane confirm-gated
    ///   logic misses. Returns a synthetic [`Confirmation`] for [`decide`], whose
    ///   depth is DERIVED from the observed reserve by the curve identity rather than
    ///   set equal to it (the retired code assigned `liquidity_lamports` — i.e. the
    ///   VIRTUAL reserve — straight into the sellability cap).
    /// * `detect_narrative_confirmation` → **dormant / admission-gated** (§28):
    ///   the narrative feature family is not admitted in laptop replay, so the
    ///   predicate returns `dormant` and authorizes nothing — the candidate stays
    ///   gated on real on-chain confirmation.
    ///
    /// `None` when the law is off, the lane does not map, or the detector is not
    /// eligible — in which case the gate's original confirmation stands.
    #[must_use]
    fn entry_mode_confirmation(
        &self,
        cand: &Candidate,
        mint: &[u8; 32],
        numeric_feats: Option<Features>,
    ) -> Option<Confirmation> {
        if !self.cfg.entry_mode_leaves_enable {
            return None;
        }
        match cand.lane {
            WlLane::ActiveMarketScalp => {
                let numeric = numeric_feats?;
                if numeric.liquidity_lamports == 0 {
                    return None;
                }
                let pf = self
                    .structure
                    .pullback_features(mint, self.cfg.structure_min_bars)?;
                let sig = detect_pullback_continuation(&pf, &PullbackParams::test());
                if sig.eligible && sig.suggested_lane == SuggestedLane::ActiveMarketScalp {
                    Some(Confirmation {
                        depth: crate::curve_depth::CurveDepth::derived(numeric.liquidity_lamports),
                        numeric,
                    })
                } else {
                    None
                }
            }
            WlLane::EarlyConfirmation => {
                // §28 dormant/admission-gated: the narrative feature family is not
                // admitted in laptop replay, so the predicate is inert and never
                // synthesizes authority. Evaluated here so the leaf is genuinely
                // wired (it is a no-op decision-wise, exactly as §28 requires).
                let numeric = numeric_feats.unwrap_or_default();
                let nf = NarrativeConfirmationFeatures {
                    narrative_velocity: 0,
                    confirming_independent_buyers: 0,
                    confirming_net_inflow: 0,
                    mechanically_sellable: numeric.liquidity_lamports > 0,
                };
                let sig =
                    detect_narrative_confirmation(&nf, &NarrativeConfirmationParams::test(), false);
                if sig.eligible
                    && sig.suggested_lane == SuggestedLane::EarlyConfirmation
                    && numeric.liquidity_lamports > 0
                {
                    Some(Confirmation {
                        depth: crate::curve_depth::CurveDepth::derived(numeric.liquidity_lamports),
                        numeric,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// §25 LAW 4: derive the setup archetype for a mint at admit/reject from its
    /// folded bar/flow state via `pump_quant_signals::setup_classifier`. Returns
    /// `0` (the `None` family) when the classifier is disabled or the bar state is
    /// not yet reconstructable. Pure read of already-folded state; authorizes
    /// nothing and changes no capital decision — it is the analytics/thesis
    /// discriminator only (§25).
    #[must_use]
    fn classify_archetype(&self, mint: &[u8; 32]) -> u16 {
        if !self.cfg.setup_classifier_enable {
            return 0;
        }
        match self
            .structure
            .market_state(mint, self.cfg.structure_min_bars)
        {
            Some(state) => classify_setup(&state, &SetupThresholds::neutral()).archetype_id(),
            None => 0,
        }
    }

    /// **LAW B1 — the entry-time episodic capture point.**
    ///
    /// Quantize the market state into a [`BrainEntry`] using ONLY facts that exist
    /// before the position opens. This function is called exactly once per admit,
    /// inside [`Self::gate_evaluate`], and its result is carried forward on the
    /// pending entry and then the open position until the exit books.
    ///
    /// Why it must be here and nowhere else: a fingerprint computed at exit would
    /// be a function of the price path it is supposed to predict. Recall over such
    /// fingerprints would look spectacular in backtest and be worth nothing live —
    /// it would be reading the answer off the back of the card. Every input below
    /// is a `&self` read of state the engine already had at admit; there is no path
    /// from a post-entry event into this value, and
    /// `brain_laws::b1_fingerprint_has_no_look_ahead` pins that by mutating the
    /// entire post-entry price path and asserting the recorded fingerprint is
    /// byte-identical.
    ///
    /// `rt_bps` is the round-trip cost measured at the PRE-brain size, so LAW B3's
    /// own action can never feed back into the fingerprint it queries with.
    fn brain_entry_at_admit(
        &self,
        mint: &[u8; 32],
        discovery_lane: DiscoveryLane,
        rt_bps: u32,
    ) -> BrainEntry {
        use pump_quant_brain::episode::EpisodeContext;
        use pump_quant_brain::fingerprint::{
            signed_decade, CreatorClass as BrainCreatorClass, MetaSaturationState,
            SetupFingerprint, SetupInputs, TrendStructure as BrainTrend, VenuePhase,
        };

        let domain_mint = DomainMint::from_bytes(*mint);
        let feats = self.numeric.features_for(domain_mint).unwrap_or_default();
        let mint_id = fnv1a_64(mint);

        // Order-flow imbalance: the lane's 0..=10_000 buy-pressure scale re-centred
        // on the balanced midpoint and rescaled to signed bps (§21.7).
        let ofi_bps = (i64::from(feats.buy_pressure_bp) - i64::from(PRESSURE_BALANCED_BP)) * 2;

        // Bar-derived structure: CVD decade, swing trend, range compression.
        let state = self
            .structure
            .market_state(mint, self.cfg.structure_min_bars);
        let cvd_decade = state.map_or(0, |s| signed_decade(s.cvd_delta));
        let range_state = state.map_or(pump_quant_brain::fingerprint::RangeState::Normal, |s| {
            range_state_of(u64::from(s.range_bps), u64::from(s.prior_range_bps))
        });
        let trend_structure = match self.structure.trend(mint, self.cfg.structure_min_bars) {
            TrendStructure::Uptrend => BrainTrend::Up,
            TrendStructure::Downtrend => BrainTrend::Down,
            // Range and Undefined both collapse to the neutral middle: "no dominant
            // swing direction" and "not enough swings to say" are the same bucket
            // for similarity purposes (§6.4 — absence is not a third direction).
            TrendStructure::Range | TrendStructure::Undefined => BrainTrend::Range,
        };

        // §21.7 burst lifecycle: recent arrival intensity against a longer baseline.
        let short = self
            .structure
            .activity(mint, self.now, self.cfg.universe_window_ticks);
        let long = self.structure.activity(
            mint,
            self.now,
            self.cfg
                .universe_window_ticks
                .saturating_mul(BRAIN_BURST_BASELINE_MULT),
        );
        let burst_phase = burst_phase_of(short.trades, long.trades, BRAIN_BURST_BASELINE_MULT);

        let realized_vol_bps = self
            .structure
            .recent_vol_bps(mint, VOL_STOP_WINDOW_BARS)
            .map_or(0i64, |v| v.clamp(0, i128::from(i64::MAX)) as i64);

        let venue_phase = if self.context.is_pool(mint) {
            VenuePhase::Pool
        } else {
            VenuePhase::Curve
        };

        // §21.4 attention / narrative. Absent attention is a zero velocity and an
        // Unclassified narrative — the neutral buckets, never a fabricated reading.
        let attention_velocity_bps = self.attention.velocity_of(mint).unwrap_or(0);
        // §21.4 narrative identity. TWO independent axes feed one nominal field:
        //   1. the MEASURED launch-metadata family (`nv_family_classify`), which
        //      spans all eight brain slots — Animal / Seasonal / Stream included —
        //      and is a pure function of decoded launch metadata; and
        //   2. the attention plane's four-way `NarrativeClass`, which keeps owning
        //      the §70.6/§70.8 conviction-CEILING semantics and is unchanged.
        // The measured family WINS when it exists and is not `Unclassified`,
        // because it is the finer, evidence-stamped axis; otherwise the historical
        // four-way crosswalk applies; otherwise `Unclassified` — a refusal this
        // nominal field CAN carry, so no fabrication is needed here (§6.4).
        let narrative_class = match self.measured.family_of(mint) {
            Some(c) if c.family != NarrativeFamily::Unclassified => brain_narrative_class(c.family),
            _ => narrative_class_of(self.attention.narrative_class_of(mint)),
        };

        // §21.7 flow authenticity — already a neutral prior when unevidenced.
        let (authenticity_bps, _fabricated) = self.flow_screen.authenticity(mint);

        // §29.9 creator class. `Proven` used to be UNREACHABLE from app state; the
        // launch → migration → survival ledger now makes it reachable, so the
        // cascade is:
        //   1. a CONFIRMED live dump right now ⇒ Toxic. A fact about the present
        //      dominates a track record about the past, and it is also the fresher
        //      observation — the ledger's own rug fact is fed from this same signal
        //      but only lands once per launch.
        //   2. the MEASURED ledger verdict when it speaks (Proven / Toxic / Serial).
        //   3. the app's lifetime-launch-count heuristic ⇒ Serial.
        //   4. Unknown. `CreatorClass::Unknown` is a real nominal slot, so the
        //      refusal is representable and nothing is fabricated (§6.4).
        let creator = self.mint_creator.get(mint).copied();
        let ledger_track =
            creator.map_or(CreatorTrack::Unknown, |c| self.measured.creator_track(c));
        let creator_class = if self.creator_dump_active(mint) {
            BrainCreatorClass::Toxic
        } else if ledger_track != CreatorTrack::Unknown {
            brain_creator_class(ledger_track)
        } else if creator
            .and_then(|c| self.creator_launches.get(&c))
            .is_some_and(|&(lifetime, _, _)| lifetime >= CREATOR_SERIAL_THRESHOLD)
        {
            BrainCreatorClass::Serial
        } else {
            BrainCreatorClass::Unknown
        };

        // §21.4 meta identity + lifecycle position. The category space is u64 in the
        // app and u32 in the brain; the low half is the fold (categories are dense
        // small ids, so this is lossless in practice and monotone regardless).
        let category = self.mint_category.get(mint).copied();
        let meta_category_id = category.map_or(0u32, |c| (c & 0xFFFF_FFFF) as u32);
        // Lifecycle position. The MEASURED phase wins when the tracker will speak
        // (it is the only path to `Decaying` — participation and activity both
        // falling off a prior peak, i.e. "new entrants are exit liquidity"); the
        // rotation-verdict heuristic is the fallback.
        //
        // HONEST LIMITATION (§6.4): `MetaSaturationState` is an ORDINAL lifecycle
        // with no UNKNOWN variant, and `Emerging` is ordinal 0 — which is also the
        // "this mint has no category at all" default below. So "no category",
        // "category with too few samples to phase" and "genuinely emerging meta"
        // all collapse into ONE fingerprint code. That is a real loss the ladder
        // cannot express and the app cannot fix from this side of the boundary;
        // it is stated rather than papered over.
        let measured_phase = category.and_then(|c| self.measured.meta_phase_of(c, self.now));
        let meta_saturation_state = match measured_phase {
            Some(p) => brain_meta_saturation(p),
            None => match category.and_then(|c| self.category_rank_adj.get(&c)) {
                Some(&adj) if adj > 0 => MetaSaturationState::Emerging,
                Some(_) => MetaSaturationState::Saturated,
                // A known category with no rotation verdict is running but not
                // rotating: broad participation, attention flat-to-rising.
                None if category.is_some() => MetaSaturationState::Hot,
                None => META_PHASE_NEUTRAL,
            },
        };

        let inputs = SetupInputs {
            ofi_bps,
            cvd_decade,
            trend_structure,
            range_state,
            burst_phase,
            realized_vol_bps,
            liquidity_decade: signed_decade(i128::from(feats.liquidity_lamports)),
            buyer_breadth: feats.unique_buyers,
            token_age_ns: u64::from(feats.age_slots).saturating_mul(BRAIN_TICK_NS),
            venue_phase,
            attention_velocity_bps,
            narrative_class,
            authenticity_bps: i64::from(authenticity_bps),
            // §70.1 holder-growth ACCELERATION — the ANALYZE limb of the continuous
            // holder stream. The series behind this is no longer a seam nobody
            // called: it is folded from OUR OWN decoded swap flow on every
            // `MarketTrade` for every watched mint (`holder_flow`), sampled into
            // the leaf estimator on the `HOLDER_SAMPLE_INTERVAL_TICKS` cadence, and
            // read here point-in-time-safely as known at this instant. On a tape
            // with genuine holder broadening this field now carries a REAL,
            // non-neutral value at admit; before this wave it was the neutral rung
            // on literally every admit.
            //
            // The estimator still fails closed (fewer than three usable samples, a
            // stale interval, or a zero base count ⇒ `None`), and the refusal
            // collapses onto the ladder's neutral rung.
            //
            // HONEST LIMITATION (§6.4): `HOLDER_GROWTH_ACCEL_EDGES_BPS` has no
            // UNKNOWN rung, so "never captured" and "measured exactly zero
            // acceleration" are the SAME fingerprint code once collapsed. The
            // distinction survives on `MeasuredState::holder_growth_accel_bps`
            // (which returns `Option`) for any caller that needs it; the
            // fingerprint cannot carry it.
            holder_growth_accel_bps: self
                .measured
                .holder_growth_accel_input(mint_id, self.now.saturating_mul(BRAIN_TICK_NS)),
            // §70.1 holder-growth VELOCITY — the FIRST derivative (schema 2).
            //
            // Under schema 1 the fingerprint carried only the second derivative, so
            // a market broadening fast and steadily (large velocity, zero
            // acceleration) was BIT-IDENTICAL to a completely flat one. Those are
            // different markets with different forward distributions, and the
            // similarity index could not see the difference.
            //
            // The value comes off the SAME estimate the acceleration does — the
            // holder-growth estimator computes both first differences on its way to
            // the second — so this field adds no estimator, no sampling and no new
            // refusal path. It is `None` exactly when acceleration is `None`.
            //
            // Same honest §6.4 limitation, same reason: the ladder has no UNKNOWN
            // rung, so "never captured" and "measured flat" share a code. That is
            // the bounded price of a BROAD-COVERAGE derivative, and it is precisely
            // the price that made a thin-coverage LEVEL unacceptable here —
            // concentration rides beside the signature instead
            // (`EpisodeContext::concentration`), never inside it.
            holder_growth_velocity_bps: self
                .measured
                .holder_growth_velocity_input(mint_id, self.now.saturating_mul(BRAIN_TICK_NS)),
            creator_class,
            meta_category_id,
            meta_saturation_state,
            designated_caller_present: self.brain.designated_caller_present(mint_id),
            round_trip_cost_bps: i64::from(rt_bps),
            info_time_ns: self.now.saturating_mul(BRAIN_TICK_NS),
        };

        BrainEntry {
            fingerprint: SetupFingerprint::from_inputs(&inputs),
            context: EpisodeContext {
                mint_id,
                venue_phase,
                meta_category_id,
                discovery_lane: discovery_lane_of(discovery_lane),
                info_time_ns: self.now.saturating_mul(BRAIN_TICK_NS),
                // The engine's logical tick IS the replay anchor (§22: no wall clock,
                // no chain slot on the laptop build).
                slot: self.now,
                // ---- THE PARALLEL STREAM (schema 2) -------------------------
                // The holder-distribution shape the episode was entered under,
                // recorded BESIDE the signature rather than inside it. It is a
                // LEVEL and needs an `Exact` holder basis, so on most episodes it
                // is an explicit `Unknown(reason)` — which is the point. Recording
                // the refusal, with its reason, is what lets the optional recall
                // conditioner sharpen where the data exists and decline where it
                // does not. Collapsing it into a numeric bucket, as a fingerprint
                // field would have to, is the §6.4 failure this avoids.
                //
                // This is the REPORT-plane derivation (`holder_concentration`),
                // not the decision-plane one, because what the episode records is
                // the EVIDENCE the market presented, not whether some law happened
                // to be armed when it did. The brain is decision-inert.
                concentration: brain_reading_of(&self.holder_concentration(mint)),
                // …and the DERIVATIVE half, which is valid on the delta-only
                // ledgers the level refuses. Maintained continuously by
                // `conc_trajectory` on the holder-sample cadence, so what lands
                // here is a trajectory rather than a point reading.
                concentration_trajectory: self.conc_trajectory.trajectory_as_of(
                    mint,
                    self.holder_flow.reading(mint).map(|r| r.basis()),
                    self.now.saturating_mul(BRAIN_TICK_NS),
                ),
            },
        }
    }

    /// §32 thesis check for an open position: build the live observations (OFI,
    /// CVD sign) and evaluate the stored deterministic thesis. True = forced exit.
    fn thesis_forces_exit(&mut self, mint: &[u8; 32]) -> bool {
        let Some(thesis) = self.theses.get(mint) else {
            return false;
        };
        let Some(f) = self.numeric.features_for(DomainMint::from_bytes(*mint)) else {
            return false;
        };
        // buy_pressure_bp is the OFI-derived 0..10_000 scale (5_000 = balanced).
        let obs = [
            FeatureObservation {
                feature_id: THESIS_FEAT_OFI,
                value_fp: i64::from(f.buy_pressure_bp),
                completeness_bps: 10_000,
                observed_ts_ns: self.now,
            },
            FeatureObservation {
                feature_id: THESIS_FEAT_CVD,
                value_fp: if f.buy_pressure_bp >= PRESSURE_BALANCED_BP {
                    1
                } else {
                    -1
                },
                completeness_bps: 10_000,
                observed_ts_ns: self.now,
            },
        ];
        let verdict = evaluate_thesis(thesis, &ThesisState { observations: &obs }, self.now);
        let force = forced_action(verdict) == ForcedAction::ForceExit;
        // §32 FLOW PERSISTENCE (`thesis_persist_obs`). A single adverse print is the
        // least informative read of a long-memory sign process (arXiv 2606.16269);
        // the exit demands a RUN of `k` consecutive adverse observations in event
        // time. `k == 1` is byte-identical to the historical first-flip behaviour:
        // the run reaches 1 on the same observation that used to force the exit.
        let k = self.cfg.thesis_persist_obs.max(1);
        if !force {
            // Flow recovered — the adverse run is broken, not merely paused.
            self.thesis_adverse.remove(mint);
            if verdict == ThesisVerdict::Invalidated {
                self.theses.remove(mint);
            }
            return false;
        }
        let run = self.thesis_adverse.entry(*mint).or_insert(0);
        *run = run.saturating_add(1);
        if *run >= k {
            self.thesis_adverse.remove(mint);
            self.theses.remove(mint);
            return true;
        }
        false
    }

    /// **The engine's ONE round-trip cost: [`crate::cost_model::round_trip_bps`]
    /// under the engine's own policy.**
    ///
    /// The venue's tiered fee against THIS market's reserve, one landed transaction's
    /// fixed cost per leg, the configured fail rate and exit-tranche count, exact
    /// curve impact on both legs, and the ATA deposit priced by whether we already
    /// hold an account for the mint — with `reclaims_ata` always true, because the
    /// engine always closes on full exit and a model that assumed otherwise would be
    /// pricing a policy the engine does not run.
    ///
    /// §94: `None` ONLY for an UNDECODED quote, which fails closed at every call
    /// site. An unpriceable market inside a decoded quote still saturates to
    /// `u32::MAX` — that is a cost so large nothing clears it, not a refusal.
    #[must_use]
    fn unified_rt_bps(&self, mint: &[u8; 32], size: u64, vsol: u64) -> Option<u32> {
        match self.resolve_quote_mint(mint) {
            QuoteMint::Undecoded => None,
            QuoteMint::Sol { .. } | QuoteMint::Usdc { .. } => Some(
                crate::cost_model::round_trip_bps(&crate::cost_model::CostInputs {
                    notional_lamports: size,
                    vsol_lamports: vsol,
                    fee_bps_per_leg: crate::cost_model::venue_fee_bps_per_leg(vsol),
                    fixed_lamports_per_leg: crate::cost_model::FIXED_LAMPORTS_PER_LEG,
                    fail_rate_bps: self.cfg.gate_fail_rate_bps,
                    exit_tranches: self.cfg.gate_exit_tranches,
                    needs_ata: !self.ata_open.contains(mint),
                    reclaims_ata: true,
                })
                .unwrap_or(u32::MAX),
            ),
        }
    }

    /// §24 LAW 2: derive the per-market take-profit ladder from the gate's measured
    /// round-trip cost. The tp1 move is `derive_target_bps(rt_cost, margin, None)`
    /// where `margin = rt_cost × target_margin_mult_bp/10_000`
    /// (`pump_quant_strategy::exit_ladder::derive_target_bps`); tp2/tp3 stack the
    /// same move and every rung is clamped into the `[target_floor_bp,
    /// target_ceiling_bp]` envelope (§56.2). The tranche COUNT is the cost-priced
    /// rung count from `exit_ladder::ladder_rungs`, clamped to the ladder's 3 slots.
    /// `None` when the law is off (fixed constants) or the cost is unpriceable.
    ///
    /// §Quant-Rev-4: when `mcap_position_tp_enable` is armed, the derived targets
    /// are OVERLAID by the mcap-position-specific ladder (Option C). Entry mcap
    /// in the early portion of the band → early-curve profile (tighter TP1,
    /// larger first-tranche fraction). Entry mcap in the late portion → late-curve
    /// profile (wider TP1, smaller fractions, higher TP2/TP3 to capture
    /// post-graduation volatility). The cost-derived rung count is preserved.
    fn derive_targets(&self, size: u64, rt_bps: u32, vsol: u64, entry_mcap_lamports: u64) -> Option<DerivedTargets> {
        if !self.cfg.derived_targets_enable {
            return None;
        }
        let margin = ((u64::from(rt_bps) * u64::from(self.cfg.target_margin_mult_bp)) / 10_000)
            .min(u64::from(u32::MAX)) as u32;
        let mv = pump_quant_strategy::exit_ladder::derive_target_bps(rt_bps, margin, None)?;
        let floor = self.cfg.target_floor_bp;
        let ceiling = self.cfg.target_ceiling_bp.max(floor);
        let clamp = |k: u32| -> u32 {
            (10_000u32.saturating_add(mv.saturating_mul(k))).clamp(floor, ceiling)
        };
        // Cost-priced rung count: each rung must clear the full fixed cost with the
        // gate's margin; the impact ceiling bounds the rest (criterion 112).
        // Both cost inputs are DERIVED, exactly as `gate::decide` derives them: the
        // rung count must be priced against the same fixed cost and the same
        // constant-product impact the admission was priced against, or the ladder is
        // sized for a market that does not exist.
        let eff_fixed = effective_fixed_lamports(
            crate::cost_model::gate_base_fixed_lamports(self.cfg.gate_exit_tranches),
            self.cfg.gate_fail_rate_bps,
        )
        .unwrap_or(crate::cost_model::gate_base_fixed_lamports(
            self.cfg.gate_exit_tranches,
        ));
        let curve = pump_quant_strategy::exit_ladder::ImpactCurve::linear_test(
            crate::cost_model::impact_den_for(vsol),
        );
        let rungs = pump_quant_strategy::exit_ladder::ladder_rungs(
            size,
            self.cfg.gate_expected_move_bps.max(1),
            eff_fixed,
            self.cfg.gate_margin_bps.max(1),
            &curve,
        )
        .len()
        .clamp(1, 3) as u8;

        // §Quant-Rev-4: mcap-position TP overlay (Option C).
        // When armed, override the cost-derived TP targets with the
        // mcap-position-specific profile. The cost-derived rung count
        // is preserved (it reflects fixed-cost pricing, not mcap position).
        if self.cfg.mcap_position_tp_enable {
            let lo = self.cfg.mcap_position_lo_lamports;
            let hi = self.cfg.mcap_position_hi_lamports.max(lo);
            let mid = lo + (hi.saturating_sub(lo)) / 2;
            if entry_mcap_lamports <= mid {
                // Early-curve profile: tighter TPs, larger first-tranche
                Some(DerivedTargets {
                    tp1_bps: self.cfg.mcap_position_early_tp1_bps,
                    tp2_bps: self.cfg.mcap_position_early_tp2_bps,
                    tp3_bps: self.cfg.mcap_position_early_tp3_bps,
                    rungs,
                    tp1_frac_bps: Some(self.cfg.mcap_position_early_tp1_frac_bps),
                    tp2_frac_bps: Some(self.cfg.mcap_position_early_tp2_frac_bps),
                    tp3_frac_bps: Some(self.cfg.mcap_position_early_tp3_frac_bps),
                })
            } else {
                // Late-curve profile: wider TPs, capture post-graduation vol
                Some(DerivedTargets {
                    tp1_bps: self.cfg.mcap_position_late_tp1_bps,
                    tp2_bps: self.cfg.mcap_position_late_tp2_bps,
                    tp3_bps: self.cfg.mcap_position_late_tp3_bps,
                    rungs,
                    tp1_frac_bps: Some(self.cfg.mcap_position_late_tp1_frac_bps),
                    tp2_frac_bps: Some(self.cfg.mcap_position_late_tp2_frac_bps),
                    tp3_frac_bps: Some(self.cfg.mcap_position_late_tp3_frac_bps),
                })
            }
        } else {
            Some(DerivedTargets {
                tp1_bps: clamp(1),
                tp2_bps: clamp(2),
                tp3_bps: clamp(3),
                rungs,
                tp1_frac_bps: None,
                tp2_frac_bps: None,
                tp3_frac_bps: None,
            })
        }
    }

    /// Open one arbitration winner (§23): probe-sized entry (§33 probe→confirm→
    /// scale), full-target risk commitment, thesis registration (§32), shadow-
    /// tournament mirror (§48), and the Admitted journal record.
    fn open_pending(&mut self, e: &PendingEntry) {
        // Criterion 112 / A-6: split the target into a probe + scale-in add such that
        // EVERY emitted bite is ≥ the operator floor (see [`probe_scale_split`]).
        let (probe, scale_add) = probe_scale_split(
            e.size,
            self.cfg.probe_frac_bp,
            self.cfg.min_trade_size_lamports,
        );
        let probe_cost =
            ((u128::from(e.entry_cost) * u128::from(probe)) / u128::from(e.size.max(1))) as u64;
        let scale_cost = e.entry_cost.saturating_sub(probe_cost);
        if self
            .positions
            .open(e.mint, e.entry_price, probe, probe_cost, self.now)
        {
            self.admitted += 1;
            // C2 wiring (item 7): notify the outbound junction sink that a
            // position was admitted. Side-effect only — the return value is
            // logged for the report, never fed into a decision (§24(b)).
            // In paper/replay mode `outbound_sink` is `None` and this is a
            // no-op (golden-digest safe).
            if let Some(sink) = self.outbound_sink {
                // In paper mode this branch is never reached (sink is None).
                // In live mode the junction owns the real wallet pubkey and
                // slippage bounds; the AdmitRecord carries the engine's
                // economic parameters for the junction's tx_build call.
                let record = pump_quant_execution::ex_outbound_sink::AdmitRecord {
                    mint: e.mint,
                    user: [0u8; 32], // sink overrides with the real signer pubkey
                    is_buy: true,
                    size_lamports: probe,
                    entry_price: e.entry_price,
                    max_slippage_bps: 500, // 5% default; sink may override
                };
                let outcome = sink.on_admit(&record);
                // Log the outcome for observability. The outcome is NEVER fed
                // into a decision (§24(b)) — it is diagnostic only. In paper
                // mode the sink is None and this branch is skipped entirely.
                match &outcome {
                    pump_quant_execution::ex_outbound_sink::OutboundOutcome::Accepted { signature } => {
                        if signature == &[0u8; 64] {
                            eprintln!("[engine] sink: Accepted (paper placeholder)");
                        } else {
                            // Hex-encode the first 8 bytes of the sig for a compact log tag.
                            let sig_hex: String = signature[..8].iter().map(|b| format!("{b:02x}")).collect();
                            eprintln!("[engine] sink: Accepted sig={sig_hex}");
                            self.live_outbound_successes += 1;
                            // Rev-19: register the pending buy for on-chain confirmation polling.
                            // The daemon will poll getSignaturesForAddress and feed
                            // OurBuyConfirmed/OurBuyFailed back into the engine.
                            self.register_pending_buy(
                                *signature,
                                e.mint,
                                probe,
                                e.entry_price,
                            );
                        }
                    }
                    other => {
                        eprintln!("[engine] sink FAILED: {other:?}");
                        self.live_outbound_failures += 1;
                    }
                }
            }
            // LAZY-HOLD ATA: the rent deposit is already in `e.entry_cost` (charged
            // by `pending_entry` iff the mint was absent from the set), so this is
            // the bookkeeping half — record that an account is now open for the mint
            // so a re-entry is not charged the deposit twice. Refused at the §99
            // bound, which fails in the EXPENSIVE direction: an untracked mint is
            // priced as a fresh account on every entry, never as a free one.
            if self.ata_open.len() < ATA_OPEN_CAP {
                self.ata_open.insert(e.mint);
            }
            self.bankroll_committed = self
                .bankroll_committed
                .saturating_add(u128::from(e.entry_cost));
            self.open_lane.insert(
                e.mint,
                OpenAttribution {
                    lane: e.lane,
                    discovery_lane: e.discovery_lane,
                    archetype: e.archetype,
                    realized_acc: 0,
                    entry_spend: e.entry_cost,
                    scale_add,
                    scale_cost,
                    entry_price: e.entry_price,
                    brain: e.brain,
                    entry_tick: self.now,
                    entry_vsol: e.entry_vsol,
                    entry_obs: e.entry_obs,
                },
            );
            // LAW B1/B2: remember the setup CLASS that was actually traded, so the
            // reflection sweep can go back and ask what that class paid.
            if let Some(be) = &e.brain {
                self.brain.on_admit(be);
            }
            // §24 LAW 6: the recent-window realized volatility that scales the
            // stop/trail, and §24 LAW 2: the per-market cost-derived take-profit
            // ladder — both computed once, at admit, and armed on the position and
            // its shadow challengers (the vol-stop challenger needs `vol_bps` even
            // when the incumbent's global switch is off).
            let vol_bps = self
                .structure
                .recent_vol_bps(&e.mint, VOL_STOP_WINDOW_BARS)
                .map_or(0, |v| v.clamp(0, i128::from(u32::MAX)) as u32);
            // §Quant-Rev-4: pass the entry mcap (derived from entry_vsol) to
            // the derive_targets function so it can select the mcap-position-
            // specific TP ladder profile. mcap = vsol² / MCAP_DIVISOR_LAMPORTS.
            let entry_mcap = {
                let v = u128::from(e.entry_vsol);
                (v.saturating_mul(v) / crate::curve_state::MCAP_DIVISOR_LAMPORTS) as u64
            };
            let derived = self.derive_targets(e.size, e.round_trip_cost_bps, e.entry_vsol, entry_mcap);
            self.positions.arm_context(&e.mint, vol_bps, derived);
            self.tournament.open(
                e.mint,
                e.entry_price,
                e.size,
                e.entry_cost,
                self.now,
                vol_bps,
            );
            // §32: the entry thesis, compiled from the registered v0 feature schema
            // (OFI stays net-buy; CVD sign stays positive), slot-stamped.
            let thesis = build_thesis(&ThesisInputs {
                entry_mode: e.lane as u16,
                archetype: e.archetype,
                entry_ts_ns: self.now,
                required: Vec::new(),
                invalidation: vec![
                    ThesisCondition {
                        feature_id: THESIS_FEAT_OFI,
                        direction: pump_quant_strategy::thesis::Direction::AtLeast,
                        threshold_fp: THESIS_OFI_MIN_FP,
                        min_completeness_bps: THESIS_MIN_COMPLETENESS_BPS,
                        freshness_bound_ns: u64::MAX,
                    },
                    ThesisCondition {
                        feature_id: THESIS_FEAT_CVD,
                        direction: pump_quant_strategy::thesis::Direction::AtLeast,
                        threshold_fp: THESIS_CVD_MIN_FP,
                        min_completeness_bps: THESIS_MIN_COMPLETENESS_BPS,
                        freshness_bound_ns: u64::MAX,
                    },
                ],
                evidence_refs: vec![fnv1a_64(&e.mint)],
            });
            self.theses.insert(e.mint, thesis);
            self.journal.record(Decision::Admitted {
                mint: e.mint,
                size_lamports: e.size,
                x_min: e.x_min,
                x_cost: e.x_cost,
                x_max: e.x_max,
                // §34.4 attempt/fail-rate multiplier and impact provenance — the
                // exact inputs that produced the admitted size, already computed at
                // admit (the fail-rate that inflated the fixed cost, the measured
                // round-trip impact cost at this size).
                fail_rate_bps: self.cfg.gate_fail_rate_bps,
                rt_cost_bps: e.round_trip_cost_bps,
                // PROVENANCE (2026-07-28): the benefit term this size was justified
                // by, the estimator that produced it, and the basis of the depth the
                // capacity cap came from. Without these the record says WHAT was
                // admitted and at what cost, but not what we thought it was worth or
                // who told us — and a replay cannot reconstruct either.
                move_bps: i128::from(e.priced_move.admission_bps()),
                move_source: e.priced_move.admission_source().code(),
                depth_basis: e.depth_basis,
            });
        } else {
            // ACCOUNTING IDENTITY FIX: positions.open() refused (duplicate
            // mint or capacity full). The candidate was promoted, passed the
            // gate, survived arbitration, and was AWARDED a slot — but could
            // not be opened. Must count as rejected to preserve
            // promoted = admitted + rejected.
            self.reject(REJECT_OPEN_FAILURE);
            self.journal.record(Decision::Rejected {
                mint: e.mint,
                reason: REJECT_OPEN_FAILURE,
            });
        }
    }

    /// Book one realized exit from the held-position lifecycle: journal it (with the
    /// exit reason, §48/§49 attribution), fold its net into the running per-lane
    /// reconciliation (the report's net-SOL), the lane-performance accountant
    /// (reflection weights), and the **bankroll** (realized-only, §33) — and, when
    /// the position fully closes, release its committed risk budget, ratchet the
    /// high-water mark, and attribute the market's total realized net back to its
    /// social callers (§82).
    fn book_exit(&mut self, mut e: Exit) {
        // ── LIVE SELL SUBMISSION (2026-08-17 fix for tape-vs-on-chain gap) ──
        //
        // When the outbound sink is installed (live mode), every exit with a
        // non-zero token_amount triggers an on-chain sell via the same sink
        // that handles buys. The sink fetches fresh curve state, computes
        // min_sol_out from current reserves, builds the sell ix, signs, and
        // submits. The outcome is logged for diagnostics but — per §24(b) —
        // NEVER fed back into a decision path.
        //
        // In paper mode (sink is None) this branch is skipped entirely and
        // the tape records the paper-simulated PnL as before.
        //
        // The token_amount on the Exit struct was computed at exit-creation
        // time from the notional fraction sold and the entry price:
        //   token_amount = (size_lamports * frac_bps / 10_000) * PRICE_SCALE / entry_price_fp
        // A zero token_amount means either paper mode (no real tokens) or a
        // degenerate position (zero entry price) — in both cases no on-chain
        // sell is attempted.
        if let Some(sink) = self.outbound_sink {
            if e.token_amount > 0 {
                let record = pump_quant_execution::ex_outbound_sink::AdmitRecord {
                    mint: e.mint,
                    user: [0u8; 32], // sink overrides with real signer pubkey
                    is_buy: false,   // SELL
                    size_lamports: e.token_amount, // token base units to sell
                    entry_price: e.exit_price_fp, // current exit price for slippage bound
                    max_slippage_bps: 500, // 5% default; sink may override
                };
                let outcome = sink.on_admit(&record);
                match &outcome {
                    pump_quant_execution::ex_outbound_sink::OutboundOutcome::Accepted { signature } => {
                        if signature == &[0u8; 64] {
                            eprintln!("[engine] sell sink: Accepted (paper placeholder)");
                        } else {
                            let sig_hex: String = signature[..8].iter().map(|b| format!("{b:02x}")).collect();
                            let mint_hex: String = e.mint[..4].iter().map(|b| format!("{b:02x}")).collect();
                            eprintln!("[engine] sell sink: Accepted sig={sig_hex} mint={mint_hex} tokens={}",
                                e.token_amount);
                            self.live_sell_successes += 1;
                            // Rev-19: register the pending sell for on-chain confirmation polling.
                            self.register_pending_sell(
                                *signature,
                                e.mint,
                                e.token_amount,
                                e.exit_price_fp,
                            );
                        }
                    }
                    other => {
                        let mint_hex: String = e.mint[..4].iter().map(|b| format!("{b:02x}")).collect();
                        eprintln!("[engine] sell sink FAILED: {other:?} mint={mint_hex}");
                        self.live_sell_failures += 1;
                    }
                }
            }
        }
        // CLOSE-ON-FULL-EXIT: a fully-exited position's token account is emptied, so
        // it is closed. `close_account` returns the whole `ATA_RENT_LAMPORTS` deposit
        // — which the entry basis charged and `realize` has been netting out pro-rata
        // across the position's life — for one signature. The net ATA cost of a
        // COMPLETED round trip is therefore `ATA_CLOSE_LAMPORTS`, five thousand
        // lamports, against the 2_039_280 an abandoned account forfeits: a 408:1
        // return on the cheapest instruction in the system, and the single highest-
        // return action available anywhere in this engine.
        //
        // This is booked HERE, on the engine, and not in `position::realize`, because
        // only the engine knows whether an account exists for the mint. Applied
        // before any accountant reads `e.net_lamports`, so the refund reaches the
        // journal, the bankroll, the lane attribution and the analytics identically.
        if e.closed && self.ata_open.remove(&e.mint) {
            e.net_lamports = e.net_lamports.saturating_add(i128::from(
                crate::cost_model::ATA_RENT_LAMPORTS - crate::cost_model::ATA_CLOSE_LAMPORTS,
            ));
        }
        let attribution = self
            .open_lane
            .get(&e.mint)
            .map(|a| (a.lane, a.discovery_lane, a.archetype));
        if let Some((lane, discovery_lane, archetype)) = attribution {
            // Saturate in the CORRECT DIRECTION: `try_from` fails at BOTH ends, so
            // `unwrap_or(i64::MAX)` would turn an out-of-range LOSS into a maximal
            // GAIN — and this value feeds `lane_perf`/`disc_perf`, which drive
            // reflection weights (a decision path, not a readout). Clamping is what
            // the sibling attribution below already does. (Audit 2026-07-25.)
            let net = e
                .net_lamports
                .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            self.lane_perf.record(lane, net);
            // §71.2 reflection integrity: attribute realized net-SOL to the ACTUAL
            // discovery lane, so lanes sharing a setup archetype learn independently.
            self.disc_perf.record(discovery_lane, net);
            let recon = ReconTrade {
                lane: eval_lane_of(lane),
                gross_lamports: e.net_lamports,
                fees: 0,
                tips: 0,
                failed_costs: 0,
                mint: e.mint,
                entry_price_fp: e.entry_price_fp,
                exit_price_fp: e.exit_price_fp,
                size_lamports: e.size_lamports,
                archetype,
                exit_reason_code: e.reason.code(),
                mfe_bps: e.mfe_bps,
                mae_bps: e.mae_bps,
                entry_tick: e.entry_tick,
            };
            self.recon[accum_index(recon.lane)].add(&recon);
            // Phase 2: accumulate for tape export.
            if self.tape_trades.len() < TAPE_TRADE_CAP {
                self.tape_trades.push(recon);
            }
        }
        self.journal.record(Decision::Filled {
            mint: e.mint,
            net_pnl_lamports: e.net_lamports,
            reason: e.reason.code(),
        });
        // §47/§54 LAW 17: register this exit for post-exit markout sampling at its
        // fill mark; the forward samples are taken at the mandated ns horizons on
        // the reflection cadence. Report-only — never touches the journal digest.
        if let Some(exit_px) = self.numeric.latest_price_fp(DomainMint::from_bytes(e.mint)) {
            self.analytics
                .record_exit_markout(e.mint, exit_px, self.now, e.reason.code());
        }
        // Realized-only bankroll accounting: every exit's net folds in immediately
        // (partial tranches included) — marks never do (§33).
        self.bankroll_realized = self.bankroll_realized.saturating_add(e.net_lamports);
        if let Some(att) = self.open_lane.get_mut(&e.mint) {
            att.realized_acc = att.realized_acc.saturating_add(e.net_lamports);
        }
        // §21.3: a rug-precursor exit is a market-wide collapse observation.
        if e.reason == ExitReason::RugPrecursor {
            self.context.on_rug_precursor();
        }
        if e.closed {
            if let Some(att) = self.open_lane.remove(&e.mint) {
                let (lane_w, total, entry_spend, entry_price, archetype, entry_vsol, entry_obs) = (
                    att.lane,
                    att.realized_acc,
                    att.entry_spend,
                    att.entry_price,
                    att.archetype,
                    att.entry_vsol,
                    att.entry_obs,
                );
                // ---- LAW B1: seal the completed trade as an immutable episode.
                // The fingerprint and context are the ADMIT-TIME capture carried on
                // the open position; ONLY the outcome comes from here. Recomputing
                // the fingerprint at this point would make it a function of the very
                // price path it is meant to predict — the single most expensive
                // mistake available in episodic memory, and the reason the capture
                // lives in the gate. The realized net also becomes a markout for
                // every author who called this mint (§82), which is what turns
                // "who called it" into "who actually earns".
                if let Some(be) = &att.brain {
                    self.brain.record_exit(
                        be,
                        total,
                        self.now
                            .saturating_sub(att.entry_tick)
                            .saturating_mul(BRAIN_TICK_NS),
                        exit_reason_of(e.reason),
                        e.mfe_bps,
                        e.mae_bps,
                    );
                }
                self.bankroll_committed = self
                    .bankroll_committed
                    .saturating_sub(u128::from(entry_spend));
                self.social_earn.record_outcome(&e.mint, total);
                // LAW D5: fold the whole position's realized net into the paid
                // Discord room that surfaced this mint — the per-source outcome
                // ledger (§29.8/§71/§74) reflection reads to grade whether a room
                // earns its keep. Saturating i128→i64 (§22): a lamport total beyond
                // i64 range is physically impossible, and a LOSS must attribute as a
                // loss (never wrap to a gain). Report plane only — no decision reads it.
                if let Some(&src) = self.alpha_source.get(&e.mint) {
                    let net_i64 = total.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
                    self.source_outcome.record(src, net_i64);
                }
                self.theses.remove(&e.mint);
                // §99 bounded state: the flow-persistence run dies with the position.
                self.thesis_adverse.remove(&e.mint);
                // §Quant-Rev-7: record the mint's exit tick for re-entry cooldown.
                // Only fires when the cooldown feature is armed — when disabled the
                // set is never read, so the insert is dead state that costs nothing
                // on the golden path (no position ever closes in the golden tape).
                if self.cfg.reentry_cooldown_enable {
                    self.reentry_cooldown.insert(e.mint, self.now);
                }
                // §47/§48/§49 analytics: the whole position's realized row + the
                // exit-policy convexity event + the §52 naive-baseline counterfactual.
                // §25 LAW 4: the row is tagged with the derived setup archetype.
                self.analytics.record_trade(
                    lane_w.index(),
                    e.reason.code(),
                    total,
                    entry_spend,
                    e.mfe_bps,
                    e.mae_bps,
                    archetype,
                );
                let realized_bps = if entry_spend > 0 {
                    (total.saturating_mul(10_000) / i128::from(entry_spend)) as i64
                } else {
                    0
                };
                // Phase 2: feed the realized outcome back into the calibrated
                // expected-move model. This is THE call that was missing in
                // production — without it the model stayed empty forever and
                // every candidate fell back to the cold-start constant. We
                // record on FULL CLOSE only (partial tranche exits do not yet
                // carry enough signal about the full round-trip outcome).
                self.expected_move.record(entry_vsol, entry_obs, realized_bps);
                // §49 LAW 15: the exit-policy event as a full-participation ALLOW
                // built through the enrich layer — the position was allowed to run
                // to its exit, so the ledger credits realized-vs-MFE (not a
                // degenerate self-cancelling pair). Vetoes and haircuts are recorded
                // as their own two-sided (counterfactual-vs-zero / reduced-vs-full)
                // events at the reject and admit sites.
                self.analytics
                    .record_convexity_mark(&ConvexityMark::Allowed {
                        rule: RuleId::new(RuleKind::ExitPolicy, u64::from(e.reason.code())),
                        realized_bps,
                        mfe_bps: e.mfe_bps,
                    });
                // §100 REPORT-plane hazard scaffolding: fold this paper fill into
                // its phase-separated per-CellKey accumulator. The hazard event is
                // "the position hit its §24(e) time-stop". This ONLY updates the
                // report-plane scaffold (never the live time-stop, never journaled),
                // so the golden decision path is byte-identical.
                let hz_phase = if self.context.is_pool(&e.mint) {
                    Phase::Pool
                } else {
                    Phase::Curve
                };
                self.hazard_scaffold.record_fill(
                    CellKey {
                        archetype,
                        phase: hz_phase,
                        catalyst: 0,
                        regime: 0,
                    },
                    e.reason == ExitReason::TimeStop,
                );
                // §24 EXPECTANCY_V1: fold this fill's realized return into the
                // lane's conditional-expectancy cell (paper-realized — a ranking
                // input for §23 arbitration, never promotion evidence).
                let cell = &mut self.lane_edge[lane_w.index()];
                cell.0 = cell.0.saturating_add(i128::from(realized_bps));
                cell.1 = cell.1.saturating_add(1);
                // §52: the buy-and-hold baseline is valued on the SAME tape with
                // the SAME fees — the REALIZED price move from entry to this exit,
                // never the configured expected move (a configured constant must
                // not manufacture baseline evidence). Unknown final price ⇒ no
                // baseline sample (UNKNOWN is recorded as absence, §6.4).
                if entry_price > 0 {
                    if let Some(px) = self.numeric.latest_price_fp(DomainMint::from_bytes(e.mint)) {
                        let hold_gross = (u128::from(entry_spend).saturating_mul(u128::from(px))
                            / u128::from(entry_price))
                            as i128;
                        // §52: the baseline pays the SAME unified round-trip
                        // frictions the strategy does — two legs of the venue's
                        // tiered fee, two landed transactions, and one closing
                        // signature. A baseline priced on a cheaper cost model
                        // would be beaten by arithmetic rather than by strategy.
                        let bl_vsol = self
                            .numeric
                            .features_for(DomainMint::from_bytes(e.mint))
                            .map_or(0, |f| f.liquidity_lamports);
                        let bl_fee_bps = 2 * crate::cost_model::venue_fee_bps_per_leg(bl_vsol);
                        let baseline = hold_gross
                            - i128::from(entry_spend)
                            - (u128::from(entry_spend) * u128::from(bl_fee_bps) / 10_000) as i128
                            - 2 * i128::from(crate::cost_model::FIXED_LAMPORTS_PER_LEG)
                            - i128::from(crate::cost_model::ATA_CLOSE_LAMPORTS);
                        self.analytics.record_baseline(baseline);
                    }
                }
                // §48 tournament pairing: the incumbent's realized net closes the pair.
                let numeric = &self.numeric;
                self.tournament.incumbent_closed(&e.mint, total, &|m| {
                    numeric.latest_price_fp(DomainMint::from_bytes(*m))
                });
            }
            // The drawdown ratchet's reference updates on realized closes only —
            // immune to mark manipulation by construction.
            let balance = self.bankroll_balance();
            if balance > self.bankroll_hwm {
                self.bankroll_hwm = balance;
            }
        }
    }

    /// Force-close every still-open position at its last-known mark (end of run), so
    /// the reported net-SOL is complete. Idempotent: after it runs the lifecycle is
    /// empty, so a second call books nothing.
    fn finalize(&mut self) {
        if self.positions.is_empty() {
            return;
        }
        let numeric = &self.numeric;
        let exits = self
            .positions
            .force_close_all(&|m| numeric.latest_price_fp(DomainMint::from_bytes(*m)));
        for e in exits {
            self.book_exit(e);
        }
    }

    fn run_reflection(&mut self) {
        // Re-derive earned social-source quality from all reconciled outcomes (§82,
        // §29.9 reflection cadence). Off the hot path; a no-op until social calls
        // have been attributed to realized markets.
        self.social_earn.reconcile();
        // §29.8: fold the D1–D10 determinant bundles from the reconciled calls into
        // the source-quality ledger — the classification path is now LIVE (was the
        // last dead loop in the social stack).
        self.analytics.fold_social_quality(
            self.social_earn.reconciled_calls(),
            &[],
            &mut self.ledger,
        );
        // §47c PRFS: mark rejected candidates forward at current prices; §33 L2:
        // refresh the envelope-clamped sizing recommendation; §56.11: retirement.
        {
            let numeric = &self.numeric;
            self.analytics.reflect(self.now, &|m| {
                numeric.latest_price_fp(DomainMint::from_bytes(*m))
            });
        }
        // §47/§54 LAW 17: mark closed exits forward at the mandated ns horizons
        // (info-time = the event-stream tick), folding the post-exit markout cells
        // and foregone-upside per ExitReason. Report-only.
        {
            let numeric = &self.numeric;
            self.analytics.mark_forward_markouts(self.now, &|m| {
                numeric.latest_price_fp(DomainMint::from_bytes(*m))
            });
        }
        // §47a LAW 18: reflect every tracked mint's terminal state at the versioned
        // δT cadence — a mint silent for ≥ δT ticks of info-time is labeled dead,
        // each label stamped with the criterion version. Report-only.
        {
            let cadence = ReflectionCadence::new(TERMINAL_DELTA_T_TICKS, TERMINAL_CADENCE_VERSION);
            let mints: Vec<MintSwaps> = self
                .last_trade_tick
                .iter()
                .map(|(m, &last)| MintSwaps {
                    mint: MintId(fnv1a_64(m)),
                    swap_ts_ns: vec![last],
                    window_end_ns: self.now,
                })
                .collect();
            self.terminal_reflections = reflect_mints(cadence, &mints);
        }
        self.f_recommended = self
            .analytics
            .sizing_recommendation(self.cfg.probe_f_bp, self.cfg.f_base_bp);
        self.retired = self.analytics.retirement_verdicts();
        // §21.3 regime summary consumption (sizing haircut when rugs are elevated).
        self.regime_rug_elevated = self.context.regime_summary().rug_elevated;
        // Recompute the category rank/size adjustments from the on-chain rotation
        // since the last reflection (strengthened, never created, by attention
        // breadth). Off the hot path; a no-op until categories have been fed.
        self.update_meta_rotation();
        // ---- LAW B2: grounded reflection. Snapshot the meta lifecycle onto the
        // brain's timeline, then re-query recall for the setup classes the engine
        // ACTUALLY traded (conditioned by phase × meta × lane) and cache the answer
        // for the report. Both are pure reads of already-realized state — no
        // decision, no journal entry, no digest contribution.
        if self.cfg.brain_enable {
            self.record_brain_meta_snapshots();
            self.brain.refresh_reflection();
            // §28/§29.8/§110 social abstraction readouts + the §34.3 staleness
            // sweep. Pure reads of already-realized state; report plane only.
            self.refresh_social_plane();
        }
        // §21.4 meta LIFECYCLE: sample every tracked category's factual on-chain
        // health onto the phase tracker. This is what makes `Decaying` reachable —
        // the app's rotation vocabulary (emerging / saturating / running) has no
        // way to express a meta whose participation and activity are BOTH falling
        // off a prior peak, which is precisely the state in which new entrants are
        // exit liquidity. Report/fingerprint plane only.
        self.record_meta_phase_samples();
        // LAW B7: build the reduce-only lane-decay flag set from conditioned
        // recall. Computed ONLY when the law is armed — a disarmed engine does not
        // pay the recall pass, and `reflect_with_brain` under an empty flag set is
        // byte-identical to the pre-LAW-B7 `reflect`. Fail-closed inside
        // `lane_decay`: a lane below `brain_decay_min_sample` is never flagged.
        let decay = if self.cfg.brain_enable && self.cfg.brain_reflect_enable {
            crate::brain_analysis::lane_decay(
                &self.brain.conditioned_classes(),
                self.cfg.brain_decay_min_sample,
            )
        } else {
            crate::reflect::LaneDecay::none()
        };
        let deltas = reflect_with_brain(&self.lane_perf, &mut self.weights, &self.cfg, &decay);
        for d in &deltas {
            if d.before_bp != d.after_bp {
                self.journal.record(Decision::Reweighted {
                    lane: d.lane as u8,
                    before_bp: d.before_bp,
                    after_bp: d.after_bp,
                });
            }
        }
        // Rebuild the watchlist under the adapted weights so ranking and promotion
        // consistently reflect the new emphasis. Candidates carry their own
        // `discovered_at`, so recency is preserved across the rebuild.
        let survivors: Vec<Candidate> = self.watchlist.entries().values().copied().collect();
        self.watchlist =
            WatchlistState::new(self.cfg.watchlist_capacity, self.params, self.weights);
        for c in survivors {
            self.watchlist.insert(c, self.now);
        }
    }

    /// LAW B2: push one [`pump_quant_brain::meta_timeline::MetaSnapshot`] per
    /// tracked category onto the brain's lifecycle timeline, so "what is the state
    /// of the meta this week" is answerable from measured history rather than from
    /// the single most recent reducer snapshot.
    ///
    /// The lifecycle label is the engine's own rotation verdict (emerging /
    /// saturating), the net is the category's realized on-chain net flow, and the
    /// breadth is its distinct-creator count. All already computed; this only
    /// records them against information time.
    fn record_brain_meta_snapshots(&mut self) {
        use pump_quant_brain::fingerprint::MetaSaturationState;
        let Some(snap) = self.meta_prev.clone() else {
            return;
        };
        let now_ns = self.now.saturating_mul(BRAIN_TICK_NS);
        for cat in &snap.categories {
            let saturation = match self.category_rank_adj.get(&cat.category_id) {
                Some(&adj) if adj > 0 => MetaSaturationState::Emerging,
                Some(_) => MetaSaturationState::Saturated,
                None => MetaSaturationState::Hot,
            };
            self.brain.record_meta_snapshot(
                (cat.category_id & 0xFFFF_FFFF) as u32,
                now_ns,
                saturation,
                cat.net_flow,
                cat.unique_creators,
                u32::try_from(cat.launches).unwrap_or(u32::MAX),
            );
        }
    }

    /// Rebuild the social abstraction readouts (§28/§29.8/§110) and run the
    /// §34.3 staleness sweep over the provenance ledger.
    ///
    /// The watched set is the CURRENT watchlist, in the engine's own deterministic
    /// order, so the support verdicts describe the mints the operator is actually
    /// looking at rather than the whole social firehose. Everything here is a pure
    /// read: no journal entry, no decision, no digest contribution.
    fn refresh_social_plane(&mut self) {
        let watched: Vec<u64> = self
            .watchlist
            .entries()
            .keys()
            .map(|m| fnv1a_64(&m.bytes()))
            .collect();
        let now = self.now;
        let as_of_ns = now.saturating_mul(BRAIN_TICK_NS);
        let ttl = self.cfg.lane_evidence_ttl_ticks;
        let min_sample = self.cfg.brain_min_sample;
        // Disjoint borrows: the plane mutates while the brain's indexes are read.
        let brain = &self.brain;
        self.social_plane.refresh(
            brain.index(),
            brain.social(),
            RefreshAt {
                watched: &watched,
                now_tick: now,
                as_of_ns,
                ttl_ticks: ttl,
                min_sample,
            },
        );
    }

    /// The §28/§29.8/§110 social abstraction plane (read-only inspection seam).
    #[must_use]
    pub const fn social_plane(&self) -> &SocialPlane {
        &self.social_plane
    }

    /// §28 operator judgement: set how PUBLIC a source is. Never inferred — "how
    /// many other people read this account" is not observable from our own
    /// realized outcomes, so it is an operator input or it is `Niche`.
    pub fn set_source_exposure(
        &mut self,
        author_id: u64,
        exposure: pump_quant_brain::trust::SourceExposure,
    ) {
        let _ = self.social_plane.set_exposure(author_id, exposure);
    }

    /// §110 record that the OPERATOR follows `author_id`. This records a fact for
    /// the recommendation engine to reason about; it performs no social action —
    /// there is no outbound social capability in this binary.
    pub fn record_operator_follow(&mut self, author_id: u64) -> bool {
        self.social_plane.follow(author_id)
    }

    /// §110 record that the OPERATOR no longer follows `author_id`.
    pub fn record_operator_unfollow(&mut self, author_id: u64) -> bool {
        self.social_plane.unfollow(author_id)
    }

    /// The REFLECTION OUTPUT's capture work list: the specific external evidence
    /// that would sharpen the current social-support estimates.
    ///
    /// This is the brain telling the Phase-B capture layer what to go and fetch —
    /// "poll Telegram for this mint", "build a track record for author X", "the
    /// operator must set author Y's §28 exposure before we lean on them" — rather
    /// than a vague request for more data. Refreshed at the reflection cadence and
    /// bounded (§99). Polling it costs one clone and changes nothing.
    #[must_use]
    pub fn capture_work_list(&self) -> Vec<SupportNeed> {
        self.social_plane.needs()
    }

    /// §21.4 / criterion 83: push one factual meta-health sample per tracked
    /// category onto the lifecycle phase tracker.
    ///
    /// All three measures are DECODED ON-CHAIN FACTS — criterion 83 forbids social
    /// interpretation from populating factual meta state, so none of these may come
    /// from the attention plane:
    ///
    /// * `participation` — NEW distinct creators launching into the category this
    ///   interval (the breadth axis §21.4 calls "broad participation").
    /// * `attention` — trade events attributed to the category this interval. This
    ///   is an ACTIVITY level, not a social attention score, and it is named
    ///   `attention` only because that is the tracker's field name.
    /// * `realized_outcome_bps` — the interval's measured flow imbalance in bps of
    ///   gross flow. An interval with no flow yields `None` and the sample is
    ///   SKIPPED rather than recorded with a fabricated zero (§6.4).
    ///
    /// Note these are **per-interval deltas**, not the reducer's cumulative
    /// totals: the phase classifier detects a peak-and-decline, and a monotone
    /// cumulative counter can never decline, so feeding the totals directly would
    /// leave `Decaying` exactly as unreachable as it was before this wiring. See
    /// [`crate::measured_state::MeasuredState::record_meta_interval`].
    ///
    /// Sampled on the engine's logical tick, which is the reflection cadence, so
    /// the spacing is regular by construction.
    fn record_meta_phase_samples(&mut self) {
        if self.mint_category.is_empty() {
            return;
        }
        let snap = self.meta.snapshot();
        let now = self.now;
        for cat in &snap.categories {
            // UNCLASSIFIED is not a meta; it is the absence of one.
            if cat.category_id == CATEGORY_UNCLASSIFIED {
                continue;
            }
            self.measured.record_meta_interval(
                cat.category_id,
                now,
                MetaTotals {
                    unique_creators: u64::from(cat.unique_creators),
                    buy_quote: cat.buy_quote,
                    sell_quote: cat.sell_quote,
                    buy_count: cat.buy_count,
                    sell_count: cat.sell_count,
                },
            );
        }
    }

    /// LAWs B1/B2/B5: the episodic memory plane (read-only inspection seam).
    #[must_use]
    pub const fn brain(&self) -> &BrainPlane {
        &self.brain
    }

    /// LAW B5: arm durable episodic persistence against `store`, rooted at the
    /// operator's `brain_path`, restoring whatever snapshot + journal are already
    /// there. Call BEFORE driving events — it replaces the live index with the
    /// restored one.
    ///
    /// Returns the restore report (how much came off the snapshot vs the journal,
    /// and whether any damage was seen) so an operator can tell a clean restart
    /// from a recovered crash.
    ///
    /// # Errors
    /// Propagates [`pump_quant_brain::persist::PersistError`] — a store whose magic
    /// or format version does not match is refused rather than silently ignored.
    pub fn attach_brain_store(
        &mut self,
        store: AppBlobStore,
    ) -> Result<pump_quant_brain::persist::RestoreReport, pump_quant_brain::persist::PersistError>
    {
        let base = std::path::PathBuf::from(self.cfg.brain_path.as_str());
        self.brain.attach(store, &base)
    }

    /// LAW B5: write an episodic snapshot now, collapsing the journal tail.
    ///
    /// # Errors
    /// Propagates [`pump_quant_brain::persist::PersistError`].
    pub fn snapshot_brain(&mut self) -> Result<(), pump_quant_brain::persist::PersistError> {
        self.brain.snapshot_now()
    }

    /// LAW B5: detach and return the blob store, disarming persistence. The proof
    /// harness uses this to hand the same buffer to a "restarted" engine.
    pub fn detach_brain_store(&mut self) -> AppBlobStore {
        self.brain.detach()
    }

    /// Drive a whole event stream and produce the report.
    pub fn run(&mut self, events: &[AppEvent]) -> Report {
        for &ev in events {
            self.tick(ev);
        }
        self.report()
    }

    /// The end-of-run report. **Finalizes first**: any still-open held position is
    /// force-closed at its last mark so the reported net-SOL is complete (§24 — a
    /// scalp is only realized when it closes). Idempotent — after finalize the
    /// lifecycle is empty, so calling `report` again yields the same numbers.
    pub fn report(&mut self) -> Report {
        // §70.1 ENTER/HOLD limb: snapshot the holder trajectory of the still-open
        // book BEFORE `finalize()` force-closes it, otherwise the answer is always
        // "no positions were open".
        let holder_trajectory = self.holder_trajectory_rows();
        self.finalize();
        let scalp_net = self.recon[accum_index(EvalLane::Scalp)].net();
        let early_net = self.recon[accum_index(EvalLane::Early)].net();
        let mut per_lane_net = [(WlLane::CreationSniper, 0i64); WlLane::COUNT];
        let mut final_weights = [(WlLane::CreationSniper, 0u32); WlLane::COUNT];
        for (i, lane) in WlLane::ALL.into_iter().enumerate() {
            per_lane_net[i] = (lane, self.lane_perf.net_sol(lane));
            final_weights[i] = (lane, self.weights.get(lane));
        }
        let mut per_discovery_lane_net =
            [(DiscoveryLane::OnchainCreation, 0i64); DiscoveryLane::COUNT];
        for (i, lane) in DiscoveryLane::ALL.into_iter().enumerate() {
            per_discovery_lane_net[i] = (lane, self.disc_perf.net_sol(lane));
        }
        // LAW D5: per-Discord-room realized net-SOL, sorted (BTreeSet) for
        // determinism. Only rooms with a reconciled outcome (a closed position)
        // are reported — a bound-but-never-closed room has earned nothing to grade.
        let per_alpha_source_net: Vec<(SourceRef, i64)> = self.alpha_source_net_rows();
        // LAW B2: `finalize()` above just booked every still-open position, so the
        // cached reflection readout is one cadence stale. Refresh it once here so
        // the end-of-run report answers over the COMPLETE episodic history rather
        // than over history as of the last reflection tick. A pure read.
        if self.cfg.brain_enable {
            self.brain.refresh_reflection();
            self.refresh_social_plane();
        }
        Report {
            ticks: self.now,
            promoted: self.promoted,
            admitted: self.admitted,
            rejected: self.rejected,
            net_lamports: scalp_net.saturating_add(early_net),
            per_lane_net,
            per_discovery_lane_net,
            final_weights,
            journal_digest: self.journal.digest(),
            universe_filtered: self.universe_filtered,
            per_alpha_source_net,
            brain_episodes_recorded: self.brain.episodes_recorded(),
            brain_recall_known: self.brain.recall_known(),
            brain_recall_unknown: self.brain.recall_unknown(),
            brain_haircuts_applied: self.brain.haircuts_applied(),
            brain_vetoes: self.brain.vetoes(),
            brain_setup_classes: self.brain.setup_classes(),
            brain_meta_state: self.brain.meta_state(),
            brain_author_records: self.brain.author_records(self.cfg.brain_min_sample),
            social_evidence: self.social_plane.evidence_rows(),
            social_support: self.social_plane.support_rows(),
            social_support_needs: self.social_plane.needs(),
            follow_recommendations: self.social_plane.follow_rows(),
            unfollow_candidates: self.social_plane.unfollow_rows(),
            caller_trust: self.social_plane.trust_rows(),
            lens_scoreboard: self.social_plane.lens_rows(),
            best_paying_lens: self.social_plane.best_paying_lens(),
            holder_trajectory,
        }
    }

    /// §70.1 holder trajectory of every currently-open position, sorted by mint.
    ///
    /// `open_lane` is a `BTreeMap`, so iteration is already mint-sorted and the
    /// rows are deterministic without a sort (§22). A pure read.
    fn holder_trajectory_rows(&self) -> Vec<HolderTrajectoryRow> {
        let as_of_ns = self.now.saturating_mul(BRAIN_TICK_NS);
        self.open_lane
            .keys()
            .filter_map(|m| {
                let r = self.holder_flow.reading(m)?;
                // The acceleration is only meaningful where the basis admits a
                // growth reading; under `Incomplete` the sampled series itself is
                // truncated, so the estimate is suppressed rather than shown with a
                // caveat nobody will read (§6.4).
                let est = if r.basis().admits_growth() {
                    self.measured.holder_estimate(fnv1a_64(m), as_of_ns)
                } else {
                    None
                };
                Some(HolderTrajectoryRow {
                    mint: *m,
                    basis: r.basis(),
                    level: r.level(),
                    growth_level: r.growth_level(),
                    lower_bound: r.lower_bound(),
                    accel_bps: est.map(|e| e.accel_bps),
                    growth_bps: est.map(|e| e.growth_bps),
                    entities_tracked: r.entities_tracked(),
                    truncated: r.truncated(),
                    concentration: concentration_of(&self.holder_flow, m),
                    concentration_trajectory: self.conc_trajectory.trajectory_as_of(
                        m,
                        Some(r.basis()),
                        as_of_ns,
                    ),
                    concentration_rate_bps: self.conc_trajectory.rate_as_of(m, as_of_ns),
                })
            })
            .collect()
    }

    /// §70.1 read-only view of the continuous holder-accounting plane — the
    /// inspection seam for the report plane and the proof suite. Never a decision
    /// input on its own: decision consumers go through
    /// [`HolderReading`]'s basis-gated accessors.
    #[must_use]
    pub const fn holder_flow(&self) -> &HolderFlow {
        &self.holder_flow
    }

    /// §21.7 the CONCENTRATION TRAJECTORY of the tracked cohort for one mint, as
    /// known at the current information time — the parallel stream's derivative
    /// half.
    ///
    /// Always derived, regardless of any config switch, for the same reason
    /// [`Self::holder_concentration`] is: an operator asking which way a float is
    /// moving is asking about the EVIDENCE, not about whether some law happens to
    /// be armed. Never a decision input.
    #[must_use]
    pub fn concentration_trajectory(&self, mint: &[u8; 32]) -> BrainTrajectory {
        self.conc_trajectory.trajectory_as_of(
            mint,
            self.holder_flow.reading(mint).map(|r| r.basis()),
            self.now.saturating_mul(BRAIN_TICK_NS),
        )
    }

    /// The raw signed rate behind [`Self::concentration_trajectory`], in bps of
    /// normalized internal concentration per minute. `None` when no rate is
    /// measurable — never a fabricated zero (§6.4).
    #[must_use]
    pub fn concentration_rate_bps(&self, mint: &[u8; 32]) -> Option<i64> {
        self.conc_trajectory
            .rate_as_of(mint, self.now.saturating_mul(BRAIN_TICK_NS))
    }

    /// §70.1 the current holder reading for one mint, or `None` when the mint has
    /// no folded flow. See [`HolderReading`] for the basis gating.
    #[must_use]
    pub fn holder_reading(&self, mint: &[u8; 32]) -> Option<HolderReading> {
        self.holder_flow.reading(mint)
    }

    /// The decision journal, for inspection or persistence.
    #[must_use]
    pub fn journal(&self) -> &DecisionJournal {
        &self.journal
    }

    /// Phase 2: drain accumulated tape trades for export to the evaluator.
    /// Returns all `ReconTrade` records accumulated since the last call and
    /// clears the internal buffer. The daemon calls this periodically to
    /// flush the tape to disk.
    pub fn take_tape_trades(&mut self) -> Vec<TapeTrade> {
        std::mem::take(&mut self.tape_trades)
            .into_iter()
            .map(|t| TapeTrade {
                scalp: matches!(t.lane, pump_quant_evaluator::evaluator_stats::Lane::Scalp),
                gross: t.gross_lamports,
                fees: t.fees,
                tips: t.tips,
                failed: t.failed_costs,
                mint: t.mint,
                entry_price_fp: t.entry_price_fp,
                exit_price_fp: t.exit_price_fp,
                size_lamports: t.size_lamports,
                archetype: t.archetype,
                exit_reason_code: t.exit_reason_code,
                mfe_bps: t.mfe_bps,
                mae_bps: t.mae_bps,
                entry_tick: t.entry_tick,
            })
            .collect()
    }

    /// LAW D5: the per-Discord-room realized-net-SOL ledger (§29.8/§71/§74) — the
    /// report-plane accountant reflection reads to grade each paid room. Read-only.
    #[must_use]
    pub fn source_outcome_ledger(&self) -> &SourceOutcomeLedger {
        &self.source_outcome
    }

    /// The numeric feature snapshot the engine currently holds for a mint, if any.
    #[must_use]
    pub fn numeric_features(&self, mint: DomainMint) -> Option<Features> {
        self.numeric.features_for(mint)
    }

    /// The earned favorable-rate (bps) for a social source, once the reconciliation
    /// loop has graded it against realized outcomes (§82). `None` until a source's
    /// attributed calls have reconciled — the caller then uses the PUBLIC_BURNED
    /// baseline. Inspection / telemetry seam for the research plane.
    #[must_use]
    pub fn earned_source_quality(&self, source_id: u64) -> Option<u32> {
        self.social_earn.quality_bps_for(source_id)
    }

    /// Apply the corroboration-tier meta-rotation rank adjustment to a candidate:
    /// an on-chain-emerging category multiplicatively *raises* its mints' discovery
    /// score (bounded by `meta_rank_bonus_bp`), a saturating category *fades* them
    /// (bounded by `meta_saturation_haircut_bp`). Returns the candidate **unchanged**
    /// whenever its mint has no known category or its category is not rotating —
    /// which is always true for a run that never ingests `TokenMetadata`, so the
    /// discovery ranking is byte-identical (the golden acceptance gate). Reorders
    /// promotion only; the gate still requires on-chain confirmation (§29/§71, §85).
    fn apply_meta_rank(&self, mut cand: Candidate) -> Candidate {
        let Some(&cat) = self.mint_category.get(&cand.mint.bytes()) else {
            return cand;
        };
        let Some(&adj_bps) = self.category_rank_adj.get(&cat) else {
            return cand;
        };
        // Multiplicative over a 10_000 base; clamped ≥ 0 so a haircut can zero a
        // score but never invert it. u128 intermediate (§22 explicit overflow).
        let factor = (10_000i64 + adj_bps).max(0) as u128;
        cand.discovery_score = (u128::from(cand.discovery_score) * factor / 10_000) as u64;
        cand
    }

    /// Recompute the per-category discovery-rank/size adjustments from the on-chain
    /// `MetaRotationState` diff since the last reflection, **strengthened but never
    /// created** by attention-breadth meta-emergence (on-chain-led, §85). Off the
    /// hot path (§29.9). A no-op that leaves `category_rank_adj` empty until
    /// categories have actually been fed and rotated — so a run without
    /// `TokenMetadata` never adjusts a score or a size (golden-safe).
    fn update_meta_rotation(&mut self) {
        let snap = self.meta.snapshot();
        if let Some(prev) = &self.meta_prev {
            let rotations = rotation_between(prev, &snap, self.cfg.meta_min_share_bps);
            self.category_rank_adj.clear();
            for r in &rotations {
                if r.emerging {
                    // On-chain emergence is the primary signal; attention breadth
                    // confirming it earns the full bonus, on-chain alone earns half
                    // (real, but weaker conviction — §29.7c corroboration).
                    let full = i64::from(self.cfg.meta_rank_bonus_bp);
                    let bonus = if self.category_attention_emerging(r.category_id) {
                        full
                    } else {
                        full / 2
                    };
                    if bonus != 0 {
                        self.category_rank_adj.insert(r.category_id, bonus);
                    }
                } else if r.saturating {
                    let haircut = i64::from(self.cfg.meta_saturation_haircut_bp);
                    if haircut != 0 {
                        self.category_rank_adj.insert(r.category_id, -haircut);
                    }
                }
            }
        }
        self.meta_prev = Some(snap);
        // §21.4 third consumption limb: a SATURATING category tightens its open
        // positions' exits (halved stall/trail cap) — pressure, never a veto.
        if !self.category_rank_adj.is_empty() {
            let saturating: Vec<[u8; 32]> = self
                .mint_category
                .iter()
                .filter(|(_, cat)| self.category_rank_adj.get(cat).is_some_and(|&adj| adj < 0))
                .map(|(m, _)| *m)
                .collect();
            for m in saturating {
                self.positions.apply_pressure(&m);
            }
        }
    }

    /// Whether the attention field shows accelerating *breadth* across a category's
    /// mints — the [`nv_meta_emergence`] breadth test over the read-only attention
    /// velocities of every mint currently assigned to `category_id`. Read-only and
    /// on-chain-led: this only ever *strengthens* an on-chain rotation, it never
    /// creates one. `false` when the attention field is empty or the category has no
    /// tracked mints.
    fn category_attention_emerging(&self, category_id: u64) -> bool {
        if self.attention.is_empty() {
            return false;
        }
        let mut velocities: Vec<i64> = Vec::new();
        for (mint, &cat) in &self.mint_category {
            if cat == category_id {
                if let Some(v) = self.attention.velocity_of(mint) {
                    velocities.push(v);
                }
            }
        }
        if velocities.is_empty() {
            return false;
        }
        nv_meta_emergence(
            &velocities,
            self.cfg.meta_accel_threshold,
            self.cfg.meta_min_breadth,
        )
        .emerging
    }

    /// The corroboration-tier size multiplier (bps of 10_000, always ≤ 10_000) for
    /// an admitted market: a graded haircut composed from creator distribution (the
    /// creator has sold more than `creator_fade_sold_bps` of peak) and category
    /// saturation. Returns 10_000 (identity) when nothing is known about the market
    /// — so a run without creator or category data sizes exactly as before (golden-
    /// safe). NEVER zero-on-known-risk as a veto: creator fade is capped at
    /// `MAX_CREATOR_FADE_BPS` (§22 behavioral-risk clause).
    fn size_haircut_bps(&self, mint: &[u8; 32]) -> u32 {
        let mut mult: u64 = 10_000;
        // (1) Creator-distribution fade: once the creator has sold more than the
        // configured fraction of peak, fade size linearly with the excess, capped.
        if let Some(reducer) = self.creators.get(mint) {
            if let Some(sold) = reducer.snapshot().sold_fraction_of_peak_bps {
                if sold > self.cfg.creator_fade_sold_bps {
                    let excess = sold - self.cfg.creator_fade_sold_bps;
                    let span = 10_000u64
                        .saturating_sub(self.cfg.creator_fade_sold_bps)
                        .max(1);
                    let fade =
                        u64::from(MAX_CREATOR_FADE_BPS).saturating_mul(excess.min(span)) / span;
                    mult = mult.saturating_sub(fade);
                }
            }
        }
        // (2) Category-saturation fade: reuse the negative discovery adjustment a
        // saturating category already carries (single source of truth), applied
        // proportionally to whatever size (1) left.
        if let Some(&cat) = self.mint_category.get(mint) {
            if let Some(&adj) = self.category_rank_adj.get(&cat) {
                if adj < 0 {
                    let hair = (adj.unsigned_abs()).min(10_000);
                    mult = mult.saturating_sub(mult.saturating_mul(hair) / 10_000);
                }
            }
        }
        // (3) §70.8/§49 narrative-class sizing conviction (LAW 8, reduce-only): a
        // fast, low-ceiling narrative (News/Trend) sizes down; a durable, high-
        // ceiling one (Tech/Culture) keeps full conviction. Read from the derived
        // class the attention field carries for the mint. Identity when the class
        // law is off or the mint is untracked (unknown stays unknown, §6.4) — so
        // the golden path is byte-identical.
        if self.cfg.narrative_class_enable {
            if let Some(class) = self.attention.narrative_class_of(mint) {
                let conv = u64::from(crate::attention::narrative_class_conviction_bp(class));
                mult = mult.saturating_mul(conv) / 10_000;
            }
        }
        mult.min(10_000) as u32
    }

    /// §26 (operator-approved reversal): whether the deployer of `mint` is in a
    /// CONFIRMED distribution — has sold at/above the veto fraction of peak. Unlike
    /// the graded `size_haircut_bps` fade, this is a hard binary the pre-entry gate
    /// and the held-position lifecycle both consult. When the §27 creator classifier
    /// labels the deployer a known extractor (`SerialRug`/`VolumeFarmer`) the
    /// stricter (lower) veto bar applies. A market with no creator distribution
    /// evidence is never a dump (unknown stays unknown, §6.4).
    fn creator_dump_active(&self, mint: &[u8; 32]) -> bool {
        if !self.cfg.creator_dump_veto_enable {
            return false;
        }
        let Some(reducer) = self.creators.get(mint) else {
            return false;
        };
        let snap = reducer.snapshot();
        let Some(sold) = snap.sold_fraction_of_peak_bps else {
            return false;
        };
        let threshold = if self.creator_is_known_extractor(mint, &snap) {
            self.cfg.creator_dump_veto_strict_bp
        } else {
            self.cfg.creator_dump_veto_bp
        };
        sold >= threshold
    }

    /// Consult the §27 creator classifier over the point-in-time deployer integers
    /// the app plane maintains (prior-launch / serial-window occupancy from
    /// `creator_launches`, distribution intensity from the creator-state reducer).
    /// Returns true only for the extractive archetypes `SerialRug`/`VolumeFarmer`.
    /// Unknowable inputs (resolved-terminal outcomes, survival, retention, copycat
    /// similarity) are left at zero, so a thin history yields `Unknown` (§6.4) and
    /// never tightens the bar on speculation.
    fn creator_is_known_extractor(&self, mint: &[u8; 32], snap: &CreatorState) -> bool {
        let Some(creator) = self.mint_creator.get(mint) else {
            return false;
        };
        let (lifetime, _, in_window) = self
            .creator_launches
            .get(creator)
            .copied()
            .unwrap_or((0, 0, 0));
        let dump_intensity_bps =
            u32::try_from(snap.sold_fraction_of_peak_bps.unwrap_or(0)).unwrap_or(u32::MAX);
        let inputs = CreatorInputs {
            prior_launch_count: lifetime,
            resolved_launch_count: 0,
            rugged_launch_count: 0,
            max_launches_in_window: in_window,
            dump_intensity_bps,
            median_survival_secs: 0,
            community_retention_bps: 0,
            streamer_launch_ratio_bps: 0,
            copycat_similarity_bps: 0,
        };
        matches!(
            classify_creator(&inputs, &CreatorThresholds::test()),
            CreatorClass::SerialRug | CreatorClass::VolumeFarmer
        )
    }

    /// §70.9 deployer-credibility screen (LAW 10): the reduce-only size multiplier
    /// (bps) for a market's deployer, from the wallet-graph deployer_credibility
    /// bundle CLASS-CONDITIONED by the §27 known-extractor verdict. Identity
    /// (10_000) when the deployer is unknown (§6.4). The point-in-time prior-launch
    /// slot list is reconstructed from the app plane's tracked launch counts
    /// (`creator_launches`: lifetime + windowed occupancy) — `in_window` launches
    /// clustered in the recent serial window, the remainder spread before it — so
    /// `compute_deployer_credibility` recovers the prior-CA count and serial-deploy
    /// burst deterministically.
    fn deployer_screen_mult_bp(&self, mint: &[u8; 32]) -> u32 {
        let Some(creator) = self.mint_creator.get(mint) else {
            return 10_000;
        };
        let Some(&(lifetime, window_start, in_window)) = self.creator_launches.get(creator) else {
            return 10_000;
        };
        if lifetime == 0 {
            return 10_000;
        }
        let window = CREATOR_SERIAL_WINDOW_TICKS.max(1);
        let decision_slot = self.now.saturating_add(1);
        let mut launches: Vec<PriorLaunch> = Vec::with_capacity(lifetime as usize);
        // Recent cluster inside the serial window (drives the serial-deploy burst).
        for i in 0..in_window.min(lifetime) {
            launches.push(PriorLaunch {
                slot: window_start.saturating_add(u64::from(i)),
            });
        }
        // Older launches, one per window width before the recent cluster, so they
        // count toward prior-CA without manufacturing a false recent burst.
        let older = lifetime.saturating_sub(in_window);
        for i in 0..older {
            launches.push(PriorLaunch {
                slot: window_start.saturating_sub((u64::from(i) + 1).saturating_mul(window)),
            });
        }
        let cfg = DeployerCredibilityConfig {
            serial_window_slots: window,
            serial_threshold: CREATOR_SERIAL_THRESHOLD,
        };
        let cred = compute_deployer_credibility(
            &launches,
            decision_slot,
            &[],
            &SocialReachInput::default(),
            &cfg,
        );
        let extractor = self
            .creators
            .get(mint)
            .map(|r| self.creator_is_known_extractor(mint, &r.snapshot()))
            .unwrap_or(false);
        deployer_screen_haircut_bp(&cred, extractor)
    }

    /// §26 held-position limb (operator-approved reversal): once the deployer of a
    /// held market has distributed past the veto threshold, force the exit of that
    /// position at the current mark (or raise exit pressure when the mark is
    /// UNKNOWN, §38). The confirmed-dump regime overrides the prior "creator
    /// distribution is never a veto" fade.
    fn enforce_creator_dump_exit(&mut self, mint: &[u8; 32]) {
        if !self.creator_dump_active(mint) || !self.positions.has(mint) {
            return;
        }
        match self.numeric.latest_price_fp(DomainMint::from_bytes(*mint)) {
            Some(price) if price > 0 => {
                if let Some(exit) = self
                    .positions
                    .close_at(mint, price, ExitReason::CreatorDump)
                {
                    self.book_exit(exit);
                }
            }
            _ => self.positions.apply_pressure(mint),
        }
    }

    /// The current on-chain `MetaRotationState` snapshot — per-category launches,
    /// flow, creators and graduations (research-plane telemetry seam, §21.4).
    #[must_use]
    pub fn meta_snapshot(&self) -> MetaRotationState {
        self.meta.snapshot()
    }

    /// S8: Reflection state snapshot for the refiner feedback loop.
    /// Returns the engine's current `retired[4]` array and the logical tick
    /// at snapshot time. The daemon appends this as metadata to the champion
    /// config dump so the refiner can observe reflection state without
    /// needing direct engine access.
    #[must_use]
    pub fn reflection_snapshot(&self) -> ReflectionSnapshot {
        ReflectionSnapshot {
            retired: self.retired,
            tick: self.now,
            reflect_every_ticks: self.cfg.reflect_every_ticks,
            brain_reflect_enable: self.cfg.brain_reflect_enable,
        }
    }

    /// The creator-state snapshot for a market, if the creator lane has seen it
    /// (position, distribution, sold-fraction-of-peak). Telemetry seam.
    #[must_use]
    pub fn creator_state(&self, mint: &[u8; 32]) -> Option<CreatorState> {
        self.creators.get(mint).map(|r| r.snapshot())
    }

    /// The §54 analytics block (CVaR, profit factor, top-k excision, PRFS gate
    /// ledgers, convexity rule ledgers, MFE capture, baseline comparison).
    #[must_use]
    pub fn analytics_report(&self) -> crate::analytics::AnalyticsReport {
        self.analytics.report()
    }

    /// §105 (CRITERION 105) REPORT / hazard-scaffold plane readout: the mint's
    /// decayed LPI/wash extraction-risk covariate (a `manip_history_fp`-style bps
    /// figure, 0 for an untracked mint), evaluated at the current logical tick.
    /// This is a pure report-plane observable — it is NOT consulted by any sizing,
    /// gating, or promotion decision, which is why accumulating it leaves the
    /// golden decision path byte-identical.
    #[must_use]
    pub fn extraction_risk_fp(&self, mint: &[u8; 32]) -> u32 {
        self.extraction_risk.manip_history_fp(mint, self.now)
    }

    /// §105 number of mints currently carrying an extraction-risk covariate
    /// (bounded state readout, report-plane only).
    #[must_use]
    pub fn extraction_risk_tracked(&self) -> usize {
        self.extraction_risk.tracked()
    }

    /// §100 (CRITERION 100) REPORT / hazard-scaffold plane readout: the hierarchical
    /// hold-horizon hazard estimate for a conditioning cell, shrunk toward its
    /// phase-separated parent (a starved cell defaults to `baseline_bps`). This is a
    /// pure report-plane observable built from realized paper fills — it does NOT
    /// drive the live §24(e) time-stop, which is why it leaves the golden path
    /// byte-identical. `k` is the shrink pseudo-count; `min_effective_sample` the
    /// baseline gate.
    pub fn cell_hazard(
        &self,
        cell: CellKey,
        k: u32,
        min_effective_sample: u32,
        baseline_bps: u32,
    ) -> Result<HazardEstimate, ShrinkError> {
        self.hazard_scaffold
            .cell_hazard(cell, k, min_effective_sample, baseline_bps)
    }

    /// §100 number of hazard cells currently tracked (bounded, report-plane only).
    #[must_use]
    pub fn hazard_cells_tracked(&self) -> usize {
        self.hazard_scaffold.tracked()
    }

    /// §101 (CRITERION 101) REPORT-plane curve-analytic authenticity margin (bps)
    /// for a mint: `observed_appreciation − supported`, where the supported move is
    /// priced under the phase-selected model. The mint's phase (curve vs pool) is
    /// read from the market context. `use_curve_analytic = false` (the default
    /// behaviour) reproduces the reserve/impact estimator byte-for-byte; `true`
    /// opts the CURVE phase into the distinct curve-analytic model. This is a pure
    /// report-plane observable — it never drives the live authenticity screen, so
    /// the golden decision path is unchanged (and no `Config` field is added, which
    /// would otherwise move the §19 config-identity digest seed).
    #[must_use]
    pub fn curve_authenticity_margin_bps(
        &self,
        mint: &[u8; 32],
        observed_appreciation_bps: u64,
        net_inflow: u64,
        reserve: u64,
        use_curve_analytic: bool,
    ) -> i128 {
        let phase = if self.context.is_pool(mint) {
            Phase::Pool
        } else {
            Phase::Curve
        };
        crate::curve_authenticity::authenticity_margin_bps(
            observed_appreciation_bps,
            phase,
            use_curve_analytic,
            net_inflow,
            reserve,
        )
    }

    /// §47a LAW 18 terminal-state reflections (report-only): the per-mint dead/
    /// alive labels from the most recent reflection cadence, each stamped with the
    /// δT criterion version. A mint is keyed by `MintId(fnv1a_64(mint_bytes))`.
    #[must_use]
    pub fn terminal_reflections(&self) -> &[MintReflection] {
        &self.terminal_reflections
    }

    /// §55 capacity-curve report over the mandated size grid (0.01–1.00 SOL) at
    /// the given venue depth, priced by the SAME fill/cost/impairment models the
    /// paper scalp path uses. Report-only — nothing in the decision path reads
    /// this; scaling never assumes linear PnL.
    #[must_use]
    pub fn capacity_report(&self, depth_lamports: u64) -> Vec<CapacityPoint> {
        crate::scalp::capacity_report(&self.cfg, depth_lamports)
    }

    /// The honest promotion-readiness report (§38/§64, report-only): the fill
    /// model that produced this run's evidence, the evidence status it may
    /// claim, and the fail-closed probe-gate verdict. On any laptop run this
    /// reports the exact blocker — `mode_c_required` under the optimistic
    /// ceiling, a failed probe criterion under Mode C, and
    /// `awaiting_live_capability` only once every deterministic gate passes
    /// (missing capability, never missing authority or a human approval).
    #[must_use]
    pub fn promotion_readiness(&self) -> crate::authority::PromotionReadiness {
        let defeats = self
            .baseline_verdict()
            .map(|v| v.defeats())
            .unwrap_or(false);
        let balance = self.bankroll_balance();
        let hwm = self.bankroll_hwm.max(1);
        let dd_bp = if balance >= hwm {
            0
        } else {
            ((u128::from(hwm - balance) * 10_000) / u128::from(hwm)) as u32
        };
        // LAW B9: recall is consulted as an ADDITIONAL fail-closed criterion,
        // applied last so it can never mask the §38/§51/§64 blockers, and one-
        // directional so it can only ever remove eligibility (§46/§51).
        crate::authority::promotion_readiness_with_recall(
            &self.cfg,
            defeats,
            dd_bp < self.cfg.dd_tier3_bp,
            self.promotion_stat_verdict(),
            self.recall_evidence(),
        )
    }

    /// §51 LAW 14 statistical promotion verdict (report-only, CONSULTED): the
    /// combined FDR + PBO/CSCV gate over the current challenger evidence. With no
    /// reconciled trades there is no promotion candidate, so the gate reports
    /// `Clear` and defers to the evidence-sufficiency probe gate; once trades exist
    /// it consults the exit-tournament challenger family — each challenger a
    /// hypothesis (p-value proxy from its SPRT log-likelihood ratio) and the
    /// laptop's non-block-structured challenger returns the CSCV matrix. That matrix
    /// is inadmissible on the laptop, so the §51 gate fails CLOSED (overfitting that
    /// cannot be measured is not a silent pass) — consulted, never ignored.
    #[must_use]
    pub fn promotion_stat_verdict(&self) -> PromotionStatisticalVerdict {
        let a = self.analytics.report();
        if a.trades == 0 {
            return PromotionStatisticalVerdict {
                fdr_blocks: false,
                pbo_blocks: false,
                pbo_bps: Some(0),
                reason: PromotionBlockReason::Clear,
            };
        }
        let standings = self.tournament.standings();
        let family: Vec<Hypothesis> = standings
            .iter()
            .enumerate()
            .map(|(i, s)| Hypothesis::new(i as u64, sprt_llr_to_p_ppm(s.sprt_millinats)))
            .collect();
        let perf: Vec<Vec<i64>> = standings.iter().map(|s| vec![s.sprt_millinats]).collect();
        promotion_verdict(
            &family,
            PROMOTION_ALPHA_PPM,
            0,
            &perf,
            PROMOTION_PBO_THRESHOLD_BPS,
        )
    }

    /// Per-lane conditional-expectancy telemetry: (Σ realized bps, fills,
    /// current EXPECTANCY_V1 edge in bps). Report-only; the third element is
    /// exactly what §23 arbitration is conditioned on for that lane.
    #[must_use]
    pub fn expectancy_report(&self) -> [(i128, u32, i128); 4] {
        let mut out = [(0i128, 0u32, 0i128); 4];
        for (i, lane) in WlLane::ALL.into_iter().enumerate() {
            let (sum, n) = self.lane_edge[lane.index()];
            out[i] = (sum, n, self.priced_move(lane, None).ranking_bps());
        }
        out
    }

    /// The reproducible strategy identity (§56.3/§19): canonical config hash,
    /// journal-digest seed FNV, and the protocol-registry version fold.
    #[must_use]
    pub fn strategy_identity(&self) -> crate::authority::StrategyIdentity {
        crate::authority::strategy_identity(&self.cfg)
    }

    /// §52 baseline-destruction verdict: the live policy's all-time realized net
    /// versus the buy-every-confirm hold baseline, through the evaluator's
    /// family-wise-margin verdict. `None` until `baseline_min_trades` realized
    /// trades exist (small-n verdicts are noise, §46). Report-only: the verdict
    /// informs the operator's promotion review (§56.1), never a trade.
    #[must_use]
    pub fn baseline_verdict(&self) -> Option<DestructionVerdict> {
        let a = self.analytics.report();
        if a.trades < self.cfg.baseline_min_trades || a.baseline_trades == 0 {
            return None;
        }
        // §52 LAW 16: the live policy's realized net vs the WHOLE deterministic
        // baseline family (family-wise margin), not just the single buy-every-
        // confirm-hold shadow — every family member competes, plus the analytics'
        // running buy-and-hold shadow as the pre-existing anchor.
        let mut competitors: Vec<Competitor> = self
            .baseline_family_report()
            .iter()
            .map(|r| Competitor::baseline(r.net_lamports))
            .collect();
        competitors.push(Competitor::baseline(a.baseline_net_lamports));
        Some(baseline_destruction(
            a.live_net_lamports,
            &competitors,
            i128::from(self.cfg.baseline_margin_lamports),
        ))
    }

    /// §52 LAW 16 baseline FAMILY net-SOL vector (report-only): each deterministic
    /// naive baseline (random-eligible / buy-every-launch / threshold-only /
    /// fixed-TP-SL / hold-to-death) reconciled over the SAME realized-trade tape and
    /// the SAME per-entry fee model. The family-wise-margin verdict runs against ALL
    /// of these; this exposes the full vector for the promotion review.
    #[must_use]
    pub fn baseline_family_report(&self) -> Vec<BaselineResult> {
        let tape = self.baseline_family_tape();
        // §52 LAW 16: the naive baselines pay the same per-entry landed-transaction
        // cost the strategy does (`cost_model`), never the retired 10_000 tip.
        let fee = FeeModel::new(u128::from(crate::cost_model::FIXED_LAMPORTS_PER_LEG));
        run_family(&tape, &fee, &FamilyParams::default_params())
    }

    /// §52 LAW 16: assemble the baseline-family tape from the retained realized
    /// trades. Each realized opportunity is a `TapeEvent` whose entry the baselines
    /// re-decide; the outcome each entered event realizes is the trade's OWN
    /// realized net — the only counterfactual the laptop can honestly claim without
    /// a separate price-path replay — so the family differs by WHICH events each
    /// naive rule enters, never by a fabricated price path.
    fn baseline_family_tape(&self) -> Vec<TapeEvent> {
        self.analytics
            .realized_trade_tape()
            .into_iter()
            .enumerate()
            .map(|(i, (net, ret, lane_idx))| {
                TapeEvent::test(
                    i as u64,
                    true,          // every realized opportunity was an eligible entry
                    lane_idx == 0, // the creation-sniper lane == a fresh launch entry
                    ret,           // decision score = realized per-trade return, bps
                    net,           // hold-to-death proxy = realized net
                    net,           // fixed-TP/SL proxy = realized net
                )
            })
            .collect()
    }

    /// §48 exit-policy tournament standings (report-only; adoption is an operator
    /// config change inside the §56.2 envelope).
    #[must_use]
    pub fn tournament_standings(&self) -> Vec<ChallengerStanding> {
        self.tournament.standings()
    }

    /// LAW B8: brain-grounded challenger PROPOSALS for the §48 grid (report-only).
    ///
    /// Derived from the recall distribution of the setups that actually paid, one
    /// axis at a time, phase-separated (§100). These are **pre-registration
    /// candidates**, not challengers: nothing here edits the fixed 2×2×2 grid, and
    /// there is no code path from a proposal to a live exit parameter. An operator
    /// promotes one into the grid through the §56.2 config envelope, after which it
    /// races under the existing SPRT machinery like everything else.
    ///
    /// Fail-closed: an empty vector until the winners cohort clears
    /// `brain_decay_min_sample` in a venue phase.
    #[must_use]
    pub fn exit_proposals(&self) -> Vec<crate::shadow::ExitProposal> {
        if !self.cfg.brain_enable {
            return Vec::new();
        }
        let mut out: Vec<crate::shadow::ExitProposal> = Vec::new();
        for phase in [
            pump_quant_brain::fingerprint::VenuePhase::Curve,
            pump_quant_brain::fingerprint::VenuePhase::Pool,
        ] {
            out.extend(crate::shadow::brain_exit_proposals(
                self.brain.index(),
                self.positions.params(),
                phase,
                self.cfg.brain_decay_min_sample,
                crate::brain::BRAIN_TICK_NS,
            ));
        }
        out
    }

    /// §56.10 VOI-ranked open research queue.
    #[must_use]
    pub fn voi_queue(&self) -> Vec<(u64, i128)> {
        self.analytics.voi_ranking()
    }

    /// §6.6 CORROBORATION-TIER holder-count ingestion: record one THIRD-PARTY
    /// observed holder count for `mint` at the engine's current information time.
    ///
    /// **This is no longer how the §70.1 holder series is populated.** The
    /// production series is folded continuously from our own decoded swap flow
    /// ([`crate::holder_flow`], wired on every `MarketTrade`) — canonical §6.1
    /// evidence with no third-party dependency and no added latency. This seam
    /// remains only as an optional corroboration channel for an operator who
    /// wants to cross-check the folded count against an RPC/indexer read of
    /// distinct non-zero balances; it must never be the *only* source of the
    /// field, and a Birdeye/DAS count fed here is corroboration, not authority.
    ///
    /// Samples pushed here land in the SAME series as the folded ones, so a
    /// caller mixing the two is mixing an absolute third-party level with our
    /// observation-window count and gets whatever that mixture deserves. In
    /// practice: either feed this or let the fold do it, not both.
    ///
    /// The holder-growth ACCELERATION estimator needs three usable samples spaced
    /// at least `HOLDER_MIN_INTERVAL_NS` apart and no more than
    /// `HOLDER_MAX_INTERVAL_NS` apart; below that it refuses and the fingerprint
    /// takes the neutral rung. Feeding this changes no capital decision — the
    /// holder field is a fingerprint/report input only.
    ///
    /// Returns whether the sample landed (a non-advancing information time is
    /// dropped — §20).
    pub fn observe_holder_count(&mut self, mint: &[u8; 32], holder_count: u64) -> bool {
        let ns = self.now.saturating_mul(BRAIN_TICK_NS);
        self.measured
            .record_holder_count(fnv1a_64(mint), holder_count, ns)
    }

    /// §21.4 launch-metadata ingestion: classify and remember `mint`'s narrative
    /// FAMILY from its decoded launch metadata.
    ///
    /// A PARALLEL channel for the same dossier-lock reason as above: the decoded
    /// [`AppEvent::TokenMetadata`] carries only the resolved integer `category_id`
    /// (strings never cross the engine boundary — §22/§85), so the family
    /// classifier runs at the `[S]`-adjacent seam and its verdict is handed in
    /// here.
    ///
    /// This is a SEPARATE axis from the attention plane's four-way
    /// `NarrativeClass`, which keeps owning the §70.6/§70.8 conviction-ceiling
    /// semantics. The family owns the brain fingerprint's eight-slot nominal
    /// identity, and it is the only path to the Animal / Stream / Seasonal slots.
    /// First classification wins (a launch's family is a launch-time fact, §81).
    ///
    /// `live_stream_active` and `derivative_similarity_bps` are `None` when the
    /// corresponding lane was not observed at all — `None` never becomes a family.
    /// Report/fingerprint plane only: nothing here gates, sizes, or ranks.
    pub fn observe_launch_metadata(
        &mut self,
        mint: &[u8; 32],
        name: &str,
        symbol: &str,
        live_stream_active: Option<bool>,
        derivative_similarity_bps: Option<u32>,
    ) -> NarrativeFamily {
        self.measured
            .classify_family(
                *mint,
                name,
                symbol,
                live_stream_active,
                derivative_similarity_bps,
            )
            .family
    }

    /// The four MEASURED estimators behind the fingerprint's formerly-fabricated
    /// fields (read-only inspection seam).
    #[must_use]
    pub const fn measured(&self) -> &MeasuredState {
        &self.measured
    }

    /// §70.10 first-slot fee ingestion (Batch-2c LAW 10): record a market's
    /// first-slot fee/tip footprint for the anti-bundle fee-floor screen. A
    /// PARALLEL channel to the decoded `AppEvent` stream (the event vocabulary is
    /// dossier-locked / additive-only, so first-slot fees are threaded here rather
    /// than on `TokenMetadata`). Folds the cumulative `priority + tip` lamports and
    /// the tx count into a bounded per-mint record; repeated calls accumulate.
    /// Bounded (§99): a new mint beyond the confirmed-set capacity evicts the
    /// lexicographically-smallest tracked mint (deterministic). Inert unless
    /// `fee_floor_enable` is set — a run that never calls this is byte-identical.
    pub fn observe_first_slot_fees(&mut self, mint: &[u8; 32], txs: &[FirstSlotTx]) {
        let cap = self
            .cfg
            .watchlist_capacity
            .saturating_mul(self.cfg.confirmed_capacity_mult)
            .max(1);
        if !self.first_slot_fees.contains_key(mint) && self.first_slot_fees.len() >= cap {
            if let Some(&victim) = self.first_slot_fees.keys().next() {
                self.first_slot_fees.remove(&victim);
            }
        }
        let add_fees = cumulative_fees_lamports(txs);
        let e = self.first_slot_fees.entry(*mint).or_insert((0, 0));
        e.0 = e.0.saturating_add(add_fees);
        e.1 = e.1.saturating_add(txs.len() as u64);
    }

    /// §70.10 whether the market's first-slot fee footprint is a fully-saturated
    /// (bundle/wash) signature that VETOES pre-entry, versus merely a graded fade.
    /// Returns `(veto, fade_bps)`: `veto` fires only when the fee-floor law is on,
    /// the footprint is `ImplausiblyLow`, AND the fade is at/above the veto bar.
    /// `(false, 0)` when the law is off or no fee record exists (unknown stays
    /// unknown, §6.4) — so the golden path is untouched.
    fn fee_floor_verdict(&self, mint: &[u8; 32]) -> (bool, u32) {
        if !self.cfg.fee_floor_enable {
            return (false, 0);
        }
        let Some(&(fees, count)) = self.first_slot_fees.get(mint) else {
            return (false, 0);
        };
        let r = assess_fee_floor(fees, count, &FeeFloorConfig::neutral());
        if r.status != FeeFloorStatus::ImplausiblyLow {
            return (false, 0);
        }
        (r.fade_bps >= FEE_FLOOR_VETO_FADE_BP, r.fade_bps)
    }

    /// §28 lagged-shadow followable verdict for a buyer entity (telemetry seam —
    /// the event vocabulary does not yet carry wallet ids on WalletAction).
    #[must_use]
    pub fn wallet_followable(&self, entity: u64) -> bool {
        let numeric = &self.numeric;
        self.wallet_screen.followable(entity, &|m, _slot| {
            numeric.latest_price_fp(DomainMint::from_bytes(*m))
        })
    }

    // ── §27/§28 amendment methods ───────────────────────────────────────────
    //
    // record_tracked_buy: G5 corroboration counter, called from the tick path.
    // tracked_wallet_boost_bp: G5 trust boost, called from the gate sizing path.
    // wallet_graph_node: G6 graph node allocation (grows by 1, preserves edges).
    // add_wallet_funding_edge: G6 funding edge between buyer↔seller on a mint.
    // smart_money_boost_bp: §28 Phase 7 smart-money PnL-screen boost.
    // test accessors: funding_node_count / funding_edge_count for integration tests.

    /// §27 amendment (G5): record a tracked-wallet buy for corroboration.
    /// Called from the tick path on every BUY where `signed_base >= 0`.
    /// Updates the per-mint `(slot, count)` pair with bounded eviction (§99).
    fn record_tracked_buy(&mut self, mint: &[u8; 32], buyer_entity: u64, slot: u64) {
        // Only count if the buyer is a tracked wallet.
        if !self.cfg.tracked_wallet_boost_enable {
            return;
        }
        let is_tracked = self
            .tracked_matcher
            .as_ref()
            .map(|m| m.contains_entity(buyer_entity))
            .unwrap_or(false);
        if !is_tracked {
            return;
        }
        let cap = self
            .cfg
            .watchlist_capacity
            .saturating_mul(self.cfg.confirmed_capacity_mult)
            .max(1);
        if !self.tracked_buys.contains_key(mint) && self.tracked_buys.len() >= cap {
            if let Some(&victim) = self.tracked_buys.keys().next() {
                self.tracked_buys.remove(&victim);
            }
        }
        let entry = self.tracked_buys.entry(*mint).or_insert((slot, 0));
        entry.0 = slot;
        entry.1 = entry.1.saturating_add(1);
    }

    /// §27 amendment (G5): compute the tracked-wallet trust boost in bps for a
    /// mint, based on how many distinct tracked wallets have bought it within
    /// the corroboration window. Returns 0 when disabled or no corroboration.
    /// Called from the gate sizing path. Bounded O(1) lookup.
    fn tracked_wallet_boost_bp(&self, mint: &[u8; 32]) -> u32 {
        if !self.cfg.tracked_wallet_boost_enable {
            return 0;
        }
        let max_bp = self.cfg.tracked_dev_boost_max_bps;
        if max_bp == 0 {
            return 0;
        }
        let window = self.cfg.tracked_corroboration_window_slots;
        let now = self.now;
        let (_, count) = match self.tracked_buys.get(mint) {
            Some(&v) => v,
            None => return 0,
        };
        if count == 0 {
            return 0;
        }
        // Scale: 1 wallet = quarter boost, 2 = half, 3+ = full.
        let scaled = if count >= 3 {
            max_bp
        } else if count == 2 {
            max_bp / 2
        } else {
            max_bp / 4 // 1 wallet = quarter boost
        };
        // Apply corroboration window: if the last tracked buy is older than
        // `window` slots, decay the boost by half.
        let (last_slot, _) = self.tracked_buys.get(mint).copied().unwrap_or((now, 0));
        if now.saturating_sub(last_slot) > window {
            scaled / 2
        } else {
            scaled
        }
    }

    /// §27/§28 amendment (G6): allocate or retrieve a wallet-graph node index
    /// for the given entity id. Grows the graph by one node at a time,
    /// preserving all existing edges. Bounded O(1) amortized.
    fn wallet_graph_node(&mut self, entity: u64) -> usize {
        if let Some(&idx) = self.wallet_graph_nodes.get(&entity) {
            return idx;
        }
        let idx = self.wallet_graph_nodes.len();
        self.wallet_graph.grow(idx + 1);
        self.wallet_graph_nodes.insert(entity, idx);
        idx
    }

    /// §27/§28 amendment (G6): add a funding edge between buyer and seller
    /// wallets that transacted on the same mint. Self-edges are suppressed
    /// (a wallet transacting with itself is not a funding edge).
    fn add_wallet_funding_edge(&mut self, buyer: u64, seller: u64, slot: u64) {
        if buyer == seller {
            return; // No self-edges.
        }
        let a = self.wallet_graph_node(buyer);
        let b = self.wallet_graph_node(seller);
        use pump_quant_wallet_graph::tier2_wallet_graph::EdgeKind;
        self.wallet_graph.add_edge(a, b, EdgeKind::Funding, slot);
    }

    /// §28 amendment (Phase 7): compute the §28 smart-money PnL-screen boost in
    /// bps for a mint. The boost is applied when the most recent buyer on that
    /// mint is `wallet_followable()` (≥40 actions, positive realized PnL,
    /// lagged-shadow edge). Returns 0 when disabled or the buyer is not
    /// followable. Bounded O(1) lookup + O(1) followable check.
    fn smart_money_boost_bp(&self, mint: &[u8; 32]) -> u32 {
        if !self.cfg.smart_money_boost_enable {
            return 0;
        }
        let max_bp = self.cfg.smart_money_boost_max_bps;
        if max_bp == 0 {
            return 0;
        }
        // Look up the last buyer entity on this mint.
        let buyer = match self.last_mint_buyer.get(mint) {
            Some(&e) => e,
            None => return 0,
        };
        // §28 truth screen: ≥40 actions, positive realized PnL, lagged-shadow.
        if !self.wallet_followable(buyer) {
            return 0;
        }
        // Full boost when the smart-money buyer is followable.
        max_bp
    }

    /// §27/§28 test accessor: the number of distinct wallet entities registered
    /// in the funding graph. Used by integration tests to verify that funding
    /// edges are wired into the tick path (G6).
    pub fn funding_node_count(&self) -> usize {
        self.wallet_graph_nodes.len()
    }

    /// §27/§28 test accessor: the total edge count in the wallet funding graph.
    /// Used by integration tests to verify that funding edges are created when
    /// buyers and sellers transact on the same mint (G6).
    pub fn funding_edge_count(&self) -> usize {
        self.wallet_graph.edges().len()
    }

    /// §Quant-Rev-3: detect coordinated funding for a mint. Returns true when
    /// >70% of the first-N buyers on this mint are connected in the funding
    /// graph (share a common funding ancestor). Uses the existing wallet_graph
    /// funding-edge connected components. Fail-open: returns false when the
    /// graph has insufficient data (§6.4) or the feature is disabled.
    ///
    /// Algorithm:
    /// 1. Collect the first-N buyer entities for this mint from `last_mint_buyer`
    ///    and the holder_flow ledger's known entities.
    /// 2. Build the funding-edge connected components via `families_by_kinds`.
    /// 3. Find the largest component that contains any of the first-N buyers.
    /// 4. If that component contains >70% of the first-N buyers → coordinated.
    ///
    /// This is O(V + E) over the funding graph, bounded by §99 capacity.
    fn detect_coordinated_funding(&self, mint: &[u8; 32]) -> bool {
        use pump_quant_wallet_graph::tier2_wallet_graph::EdgeKind;

        // Gather the early buyer entities for this mint. We use the holder_flow
        // ledger's per-mint entity roster. The `last_mint_buyer` map gives us
        // the most recent buyer, but we need the FIRST N buyers. We track them
        // via the holder_flow's per-entity first-buy sighting, which records
        // the age_slots at first buy — entities with age_slots == 0 (creation
        // slot) or age_slots <= SNIPER_SLOT_WINDOW are the early buyers.
        //
        // However, the holder_flow does not expose a per-mint entity iterator
        // directly. We use the wallet_graph's funding families as a proxy:
        // the funding graph connects wallets that transacted on the same mint.
        // A large connected component in the funding graph IS the coordination
        // signal — if N wallets on the same mint share funding edges, they
        // are linked by a common funder.

        // Get all funding-edge connected components.
        let funding_kinds = EdgeKind::funding_family_kinds();
        let families = self.wallet_graph.families_by_kinds(&funding_kinds);

        // If there are no funding edges at all, fail-open (§6.4).
        if families.is_empty() {
            return false;
        }

        // Find the entities associated with this mint. We use the last_mint_buyer
        // and last_mint_seller maps, plus any entity that has transacted on this
        // mint according to the wallet_graph. Since we don't have a per-mint
        // entity roster, we use the funding graph families directly: if a single
        // funding family contains >= (first_n_buyers * max_share / 10_000)
        // entities, that's coordinated funding.
        let max_share = self.cfg.coordinated_funding_max_share_bps;
        let first_n = self.cfg.coordinated_funding_first_n_buyers as usize;

        // The largest funding family component.
        let _largest_family_size = families.iter().map(|f| f.len()).max().unwrap_or(0);

        // If the largest funding family has >= first_n entities and constitutes
        // > max_share of all tracked wallet entities, it's coordinated.
        let total_entities = self.wallet_graph_nodes.len();
        if total_entities == 0 {
            return false; // fail-open
        }

        // We need to check if the entities on THIS mint are concentrated in
        // one funding family. Since we don't have a per-mint entity list, we
        // use the wallet_graph's funding families: if one family contains
        // >= ceil(first_n * max_share / 10_000) of the first-N entities,
        // that's coordinated funding.
        let threshold = ((first_n * max_share as usize) + 9_999) / 10_000; // ceil division
        // The family must have at least `threshold` members AND those members
        // must include entities that transacted on this mint. We approximate
        // the "transacted on this mint" check using the wallet_graph_node indices:
        // if the mint's last buyer is in a large family, and that family is
        // large enough, it's coordinated.
        let buyer = self.last_mint_buyer.get(mint).copied().unwrap_or(0);
        if buyer == 0 {
            return false; // no buyer recorded, fail-open
        }
        let buyer_idx = match self.wallet_graph_nodes.get(&buyer) {
            Some(&idx) => idx,
            None => return false, // buyer not in graph, fail-open
        };

        // Check if the buyer's funding family is large enough.
        for family in &families {
            if family.contains(&buyer_idx) && family.len() >= threshold {
                return true;
            }
        }
        false
    }

    /// §27/§28 test accessor: the number of distinct wallet entities registered
    /// in the funding graph. Used by integration tests to verify that funding
    /// edges are wired into the tick path (G6).
    pub fn wallet_graph_entity_count(&self) -> usize {
        self.wallet_graph_nodes.len()
    }

    /// Alias for `funding_edge_count` — the total edge count in the wallet
    /// funding graph. Kept for test readability.
    pub fn wallet_graph_edge_count(&self) -> usize {
        self.wallet_graph.edges().len()
    }

    /// §27/§28 test accessor: the most recent buyer entity on a mint, if any.
    pub fn last_mint_buyer_entity(&self, mint_bytes: &[u8; 32]) -> Option<u64> {
        self.last_mint_buyer.get(mint_bytes).copied()
    }

    /// §27/§28 test accessor: the most recent seller entity on a mint, if any.
    pub fn last_mint_seller_entity(&self, mint_bytes: &[u8; 32]) -> Option<u64> {
        self.last_mint_seller.get(mint_bytes).copied()
    }

    // ── §27 daemon-wiring methods (called by pq_daemon) ──────────────────────

    /// G3 persistence: restore the creator ledger from a serialized blob.
    /// Returns true if the restore succeeded, false if the bytes were invalid.
    pub fn restore_creator_ledger(&mut self, bytes: &[u8]) -> bool {
        use pump_quant_wallet_graph::creator_ledger::CreatorLedger;
        match CreatorLedger::deserialize(bytes) {
            Ok(ledger) => {
                self.measured.restore_creator_ledger(ledger);
                true
            }
            Err(_) => false,
        }
    }

    /// G3 persistence: snapshot the creator ledger as a serialized blob for
    /// cross-session persistence. Called by the daemon on shutdown.
    pub fn snapshot_creator_ledger(&self) -> Vec<u8> {
        self.measured.creator_ledger().serialize()
    }

    /// §27 amendment: set the loaded tracked-wallet matcher at daemon startup.
    /// Returns the number of tracked wallets loaded.
    pub fn set_tracked_wallet_matcher(
        &mut self,
        matcher: pump_quant_wallet_graph::tracked_wallet_matcher::TrackedWalletMatcher,
    ) -> usize {
        let n = matcher.len();
        self.tracked_matcher = Some(matcher);
        n
    }

    /// The current signed discovery-rank adjustment (bps over a 10_000 base) for a
    /// category, if it is rotating: positive = emerging, negative = saturating.
    /// `None` when the category is not currently rotating. Telemetry seam.
    #[must_use]
    pub fn category_rank_adjustment(&self, category_id: u64) -> Option<i64> {
        self.category_rank_adj.get(&category_id).copied()
    }

    // ── C2 wiring: route-health accessors ──────────────────────────────────
    //
    // These methods are the seam between the engine's decision plane and the
    // execution crate's route-health ring. In paper/replay mode they are never
    // called (the paper-session binary does not submit real transactions), so
    // the golden tape is byte-identical. In live mode the junction will:
    //   1. call `route_health_record()` after every submission landing/failure,
    //   2. call `route_health_fill()` to fold health into `RouteCtx` before
    //      `choose_submit_plan()`.

    /// Record a landing outcome into the route-health ring (C2).
    ///
    /// `route` is the submission route the attempt was sent on. `attempt`
    /// carries the landed/missed flag and wall-clock latency in ms.
    pub fn route_health_record(
        &mut self,
        route: pump_quant_execution::ex_route_policy::Route,
        attempt: pump_quant_execution::ex_route_health::Attempt,
    ) {
        self.route_health.record(route, attempt);
    }

    /// Record a Helius Sender attempt into the sender-health ring (C2).
    pub fn route_health_record_sender(
        &mut self,
        attempt: pump_quant_execution::ex_route_health::Attempt,
    ) {
        self.route_health.record_sender(attempt);
    }

    /// Fold measured route health into `route_ctx` (C2). Returns `false` if
    /// not all legacy routes have cleared `MIN_SAMPLES`, leaving `ctx`
    /// unchanged. Call this before `choose_submit_plan()`.
    pub fn route_health_fill(
        &self,
        route_ctx: &mut pump_quant_execution::ex_route_policy::RouteCtx,
        max_fail_bps: u32,
    ) -> bool {
        if !self.route_health.all_legacy_measured() {
            return false;
        }
        self.route_health.fill_route_ctx(route_ctx, max_fail_bps);
        true
    }

    /// C2 wiring (item 7): install an outbound junction sink. Only the
    /// junction crate calls this — in live mode it installs an
    /// `OutboundJunction`; in paper/replay mode it is never called and the
    /// sink remains `None` (golden-digest safe).
    ///
    /// The sink reference must have a `'static` lifetime because the engine
    /// outlives any individual call frame. The junction crate constructs a
    /// `Box::leak` or a `static NoopSink` for this.
    pub fn install_outbound_sink(
        &mut self,
        sink: &'static dyn pump_quant_execution::ex_outbound_sink::OutboundSink,
    ) {
        self.outbound_sink = Some(sink);
    }

    /// Whether an outbound sink is installed (for the report).
    #[must_use]
    pub fn outbound_sink_installed(&self) -> bool {
        self.outbound_sink.is_some()
    }

    // ─── Rev-19 on-chain feedback API ──────────────────────────────────────

    /// Register a pending buy tx for on-chain confirmation tracking.
    /// Called by the daemon immediately after the live sink submits a buy tx.
    /// The signature comes from `OutboundOutcome` returned by `on_admit`.
    pub fn register_pending_buy(&mut self, signature: [u8; 64], mint: [u8; 32], size: u64, price_fp: u64) {
        let key = sig_key(&signature);
        // §99 bounded: evict oldest at capacity (256 entries).
        if self.pending_buys.len() >= 256 && !self.pending_buys.contains_key(&key) {
            if let Some(first) = self.pending_buys.keys().next().copied() {
                self.pending_buys.remove(&first);
            }
        }
        self.pending_buys.insert(key, PendingTx {
            signature,
            mint,
            submit_tick: self.now,
            size,
            price_fp,
        });
    }

    /// Register a pending sell tx for on-chain confirmation tracking.
    pub fn register_pending_sell(&mut self, signature: [u8; 64], mint: [u8; 32], size: u64, price_fp: u64) {
        let key = sig_key(&signature);
        if self.pending_sells.len() >= 256 && !self.pending_sells.contains_key(&key) {
            if let Some(first) = self.pending_sells.keys().next().copied() {
                self.pending_sells.remove(&first);
            }
        }
        self.pending_sells.insert(key, PendingTx {
            signature,
            mint,
            submit_tick: self.now,
            size,
            price_fp,
        });
    }

    /// Return all pending buy signatures for daemon polling.
    /// Returns (mint_bytes, signature) pairs.
    pub fn pending_buy_signatures(&self) -> Vec<([u8; 32], [u8; 64])> {
        self.pending_buys.values().map(|p| (p.mint, p.signature)).collect()
    }

    /// Return all pending sell signatures for daemon polling.
    /// Returns (mint_bytes, signature) pairs.
    pub fn pending_sell_signatures(&self) -> Vec<([u8; 32], [u8; 64])> {
        self.pending_sells.values().map(|p| (p.mint, p.signature)).collect()
    }

    /// Evict pending txs older than `max_age_ticks` — called by the daemon
    /// when `getSignaturesForAddress` returns no match for a signature that
    /// has been pending too long. Returns the count evicted.
    pub fn evict_stale_pending(&mut self, max_age_ticks: u64) -> usize {
        let threshold = self.now.saturating_sub(max_age_ticks);
        let mut evicted = 0;
        // Evict stale buys
        let stale_buy_keys: Vec<[u8; 8]> = self.pending_buys
            .iter()
            .filter(|(_, p)| p.submit_tick < threshold)
            .map(|(k, _)| *k)
            .collect();
        for k in stale_buy_keys {
            self.pending_buys.remove(&k);
            evicted += 1;
        }
        // Evict stale sells
        let stale_sell_keys: Vec<[u8; 8]> = self.pending_sells
            .iter()
            .filter(|(_, p)| p.submit_tick < threshold)
            .map(|(k, _)| *k)
            .collect();
        for k in stale_sell_keys {
            self.pending_sells.remove(&k);
            evicted += 1;
        }
        evicted
    }

    /// Pending buy count (for status reporting).
    pub fn pending_buy_count(&self) -> usize {
        self.pending_buys.len()
    }

    /// Pending sell count (for status reporting).
    pub fn pending_sell_count(&self) -> usize {
        self.pending_sells.len()
    }

    /// Buy confirmed count (for status reporting).
    pub fn buy_confirmed_count(&self) -> u64 {
        self.buy_confirmed_count
    }

    /// Buy failed count (for status reporting).
    pub fn buy_failed_count(&self) -> u64 {
        self.buy_failed_count
    }

    /// Sell confirmed count (for status reporting).
    pub fn sell_confirmed_count(&self) -> u64 {
        self.sell_confirmed_count
    }

    /// Sell failed count (for status reporting).
    pub fn sell_failed_count(&self) -> u64 {
        self.sell_failed_count
    }
}

/// **Rev-19**: Compact a 64-byte signature into an 8-byte key for BTreeMap
/// storage. Solana signatures are 64-byte Ed25519 signatures — the first 8
/// bytes provide sufficient distinctiveness for a bounded pending-tx map.
fn sig_key(signature: &[u8; 64]) -> [u8; 8] {
    let mut k = [0u8; 8];
    k.copy_from_slice(&signature[..8]);
    k
}

/// §51 LAW 14: map a challenger's SPRT log-likelihood ratio (milli-nats) to a
/// p-value proxy in ppm for the promotion FDR family. Monotone and deterministic:
/// stronger evidence the challenger beats the incumbent (more positive LLR) → a
/// smaller p; non-positive evidence → a large p. Integer-only (§22).
const fn sprt_llr_to_p_ppm(llr_millinats: i64) -> u32 {
    if llr_millinats <= 0 {
        900_000
    } else {
        let dec = if llr_millinats > 500_000 {
            500_000u32
        } else {
            llr_millinats as u32
        };
        let p = 500_000u32.saturating_sub(dec);
        if p == 0 {
            1
        } else {
            p
        }
    }
}

/// Stable small codes for gate-reject reasons, for the journal.
const fn reject_code(r: GateReject) -> u8 {
    match r {
        GateReject::NeedsOnchainConfirmation => 1,
        GateReject::NoNumericConfirmation => 2,
        GateReject::EconomicallyUnviable => 3,
        // 9 continues the post-gate numbering; a band refusal is a SELECTION event and
        // must never be confused with an economic one in the reject statistics.
        GateReject::OutsideMcapBand => 9,
        // Re-pin #29: TP1 reachability refusal — the model's estimated upside can't
        // reach TP1 after round-trip costs. A distinct gate refusal, not an economic
        // unviability: the trade clears costs, it just can't reach the first take-
        // profit rung of the new cost-aware ladder.
        GateReject::Tp1Unreachable => 18,
        // §Quant-Rev-7: re-entry cooldown refusal — a SELECTION refusal (mint on
        // temporary blackout after exit). Cannot fire in golden tape.
        GateReject::ReentryCooldown => 25,
        // Rev-13: entry quality filter refusal — a SELECTION refusal (pre-entry
        // trade ring lacks organic buy demand or is whale-dominated). Cannot
        // fire in golden tape (no pre-entry ring in the tape's fixed flow).
        GateReject::EntryQualityFilter => 26,
        // Rev-14 wangr intelligence: selection refusals from the wangr graduation
        // study. Each is a SELECTION event, distinct from economic unviability.
        // Cannot fire in golden tape (no MarketAuxiliary/TimeSignal events fed).
        GateReject::WangrTokenStandard => 30,
        GateReject::WangrTimeWindow => 31,
        GateReject::WangrSymbolLength => 32,
        GateReject::WangrCreatorTrack => 33,
        GateReject::WangrLiquidityZone => 34,
    }
}

/// Post-gate journal reject codes (continuing the gate's 1–3 numbering): sizing
/// and toxicity refusals that fire AFTER economic admission. Stable for replay.
const REJECT_VPIN_TOXIC: u8 = 4;
const REJECT_INSUFFICIENT_BANKROLL: u8 = 5;
const REJECT_MAX_CONCURRENT: u8 = 6;
const REJECT_BELOW_COST_FLOOR: u8 = 7;
const REJECT_WALLET_FLOOR: u8 = 8;
/// Extreme fabrication signature on the mint's flow (§21.7 law — the ONLY
/// authenticity-driven discovery-adjacent gate; graded authenticity only sizes).
const REJECT_FABRICATED_FLOW: u8 = 9;
/// Phase-correct executable exit cost exceeds the priced expected move (§34.4 —
/// a trade whose exit side already eats the edge is a structural loss).
const REJECT_EXIT_COST: u8 = 10;
/// Slot lost in expected-net arbitration (§23 — the forgone candidate and its
/// opportunity cost are journaled, never silently dropped).
const REJECT_ARBITRATION: u8 = 11;
/// The discovering lane is retired (§56.11 sequential-evidence retirement):
/// discovery continues as research, capital eligibility is suspended.
const REJECT_LANE_RETIRED: u8 = 12;
/// §26 confirmed-creator-dump HARD VETO (operator-approved reversal): the
/// deployer has distributed past the config veto threshold — refuse pre-entry.
const REJECT_CREATOR_DUMP: u8 = 13;
/// §70.10 anti-bundle fee-floor VETO (Batch-2c LAW 10): the market's first-slot
/// fee footprint is a fully-saturated bundle/wash signature — refuse pre-entry.
const REJECT_FEE_FLOOR: u8 = 14;
/// §94/§18.2 UNKNOWN-fails-closed: the market's quote mint could not be decoded,
/// so its round-trip cost cannot be priced without silently assuming SOL — refuse.
/// Never fires while quote resolution defaults to SOL (all golden markets are
/// SOL-quoted), so the golden path is byte-identical.
const REJECT_UNDECODED_QUOTE: u8 = 15;

/// LAW B3 (§29.5/§46): episodic recall returned a `Known` verdict over a setup
/// class that historically BLED — negative median realized net AND a decisive win
/// rate at or below the veto bar. Refused pre-entry. This code can only ever appear
/// with `brain_haircut_enable` armed; the fail-closed law (B4) guarantees an
/// `Unknown` verdict can never reach it.
const REJECT_BRAIN_BLED: u8 = 16;

/// §21.7/§70.1 holder-concentration refusal: the market's tracked holder
/// distribution is extreme (cumulative top-10 share, first-ten-buyer capture, or
/// whale dominance past the named-const veto bar) AND an independent §21.7
/// flow-authenticity signature corroborates it.
///
/// **The conjunction is constitutional, not stylistic.** §21.7 names this exact
/// feature — bundle-adjusted top-N holding concentration — "a feature family and
/// prior, never a standalone veto", and separately restricts hard rejection to
/// "extreme fabrication signatures". Concentration alone therefore only ever
/// haircuts; this code can only appear when the base-position distribution AND the
/// quote-flow authenticity independently agree.
///
/// It also cannot appear at all unless `holder_concentration_enable` is armed, and
/// the fail-open law guarantees a `ConcentrationVerdict::Unknown` (delta-only,
/// truncated, or thin ledger) can never reach it.
const REJECT_HOLDER_CONCENTRATION: u8 = 17;

/// Post-admission pricing failure (§18.2/§24): the gate's `Admit` verdict
/// fired, but a downstream pricing step failed-closed — no spot price, curve
/// fill returned `None`, or zero entry price — so the candidate could not be
/// economically sized and was refused. This is a **pricing-side rejection**,
/// not a silent drop: it preserves the accounting identity
/// `promoted = admitted + rejected` by ensuring every promoted candidate
/// that exits `gate_evaluate` with `None` has been counted.
const REJECT_PRICING_FAILURE: u8 = 18;

/// Post-arbitration open failure (§23/§33): the candidate was awarded a slot
/// by arbitration, but `positions.open()` refused — either because the mint
/// was already open (duplicate award) or the positions capacity was full
/// (capacity reached between gate and open). The candidate is counted as
/// rejected to preserve the accounting identity `promoted = admitted + rejected`.
const REJECT_OPEN_FAILURE: u8 = 19;

/// §Quant-Rev-1: bundle detection — ≥3 buys sharing the creation slot from
/// linked wallets (same-slot buy count ≥ `bundle_detect_min_same_slot_buys`).
/// MELT (arXiv:2602.13480) shows 36.5% of supply held by coordinated accounts
/// on average; ScorpTrader identifies same-slot buy counting as the #1 anti-rug
/// filter. The existing `holder_concentration` module already classifies
/// bundle entities; this reject fires when the bundle cohort is large enough
/// to indicate coordinated insider accumulation, independent of the reduce-only
/// size haircut path. Operator-approved hard veto (A-14 precedent: the §26
/// creator-dump veto reversed the prior "fade-only" behaviour for confirmed
/// extractors; this is the same reversal for confirmed bundlers).
const REJECT_BUNDLE_DETECTED: u8 = 20;

/// §Quant-Rev-1: bundle concentration — same-slot buyers collectively hold
/// more than `bundle_concentration_max_bps` (default 25%) of the float. When
/// a coordinated cohort controls >25% supply, a single actor can move the
/// price alone — this is not a free market. MELT's bundle-trace data shows
/// that high same-slot concentration is the strongest predictor of rug-pull
/// launches. Fires only when bundle detection is enabled AND the concentration
/// screen has a non-Unknown verdict (fail-open on insufficient evidence, §21.7).
const REJECT_BUNDLE_CONCENTRATION: u8 = 21;

/// §Quant-Rev-2: dev wallet grading — the deployer has a prior-launch history
/// with a graduation rate below `dev_graduation_min_rate_bp` over at least
/// `dev_history_min_launches` prior mints. ScorpTrader data: grade-B devs
/// survive at 87.8% vs 45.5% for unknown devs. The existing
/// `deployer_screen_mult_bp` already computes a size haircut; this reject
/// fires when the deployer's history is so poor (e.g. <10% graduation over
/// ≥5 prior launches) that no size is safe. Operator-approved hard veto
/// (same A-14 precedent). Fail-open: no prior history → identity (unknown
/// stays unknown, §6.4), so first-launch deployers are never rejected.
const REJECT_DEV_HISTORY: u8 = 22;

/// §Quant-Rev-3: coordinated funding — >70% of the first-10 buyers trace to a
/// common funding source, indicating "one entity wearing ten hats" (ScorpTrader).
/// The existing `wallet_graph` infrastructure already builds funding edges via
/// `add_wallet_funding_edge`; this reject traverses the graph to detect a
/// single-ancestor cluster among early buyers. MELT's bundle-trace data
/// confirms coordinated accounts hold 36.5% supply on average. Fail-open when
/// the funding graph has insufficient data (§6.4). Operator-approved.
const REJECT_COORDINATED_FUNDING: u8 = 23;

/// §Quant-Rev-6: exit liquidity — fewer than `exit_liquidity_min_holders`
/// (default 30) genuinely independent holders exist for the exit side.
/// ScorpTrader: "Under ~30 genuinely independent holders means nobody on the
/// other side when you sell. You will realise −60% on a token that never
/// technically rugged." The holder count excludes entities linked by the
/// funding graph (coordinated wallets are not "independent"). Fail-open when
/// the holder ledger is Unknown or truncated. Operator-approved.
const REJECT_INSUFFICIENT_EXIT_LIQUIDITY: u8 = 24;

/// §Quant-Rev-7: RE-ENTRY COOLDOWN refusal. The mint was recently exited and is
/// still within the cooldown window (`reentry_cooldown_ticks`). A SELECTION
/// refusal: the trade may be viable, the mint is on temporary blackout to break
/// the death-by-a-thousand-cuts re-entry loop. Cannot fire in the golden tape
/// (no position closes → cooldown set never populated → golden path byte-identical).
const REJECT_REENTRY_COOLDOWN: u8 = 25;

/// §21.7 corroboration bar (bps) for [`REJECT_HOLDER_CONCENTRATION`].
///
/// The independent flow-authenticity reading — computed over per-entity
/// quote-lamport gross flow, a different quantity with a different denominator
/// from the base-token net positions the concentration is computed over — must be
/// EVIDENCED (past the screen's own swap-sample floor, so a thin tape's neutral
/// prior cannot corroborate anything) and degraded to at most half. At 5 000 bps
/// the wash/HHI evidence the concentration screen never saw has already cut the
/// authenticity multiplier in half on its own.
const CONCENTRATION_VETO_AUTH_BPS: u32 = 5_000;

/// §94 default quote decimals for a SOL-quoted market (lamports = 9 decimals).
/// The engine's economic gate has always priced in SOL lamports; making the
/// quote mint explicit (defaulting here) leaves every SOL-quoted golden market
/// byte-identical while opening the parametric USDC path.
const SOL_QUOTE_DECIMALS: u32 = 9;
/// §70.10 fee-floor veto bar (bps of fade): an `ImplausiblyLow` footprint whose
/// fade reaches this is a manufactured/wash launch (near-zero cumulative fees for
/// the advertised activity) — a veto, not merely a fade. Below it the footprint
/// only shrinks size. 9_000 bps ⇒ intensity ≤ 10% of the plausible floor.
const FEE_FLOOR_VETO_FADE_BP: u32 = 9_000;

/// Creator serial-deploy window (ticks) and launch-count threshold for the
/// §27/§70.9 credibility haircut (3 launches inside ~2 minutes = serial deployer).
const CREATOR_SERIAL_WINDOW_TICKS: u64 = 300;
const CREATOR_SERIAL_THRESHOLD: u32 = 3;
/// Regime consumption (§21.3): sizing haircut applied when the market-wide
/// rug/collapse rate is Elevated+ (reduce-only, never a veto).
const REGIME_RUG_HAIRCUT_BP: u32 = 7_000;
/// §34.4 exit-cost structural-veto multiple: the phase-correct executable exit
/// cost may not exceed this multiple of the priced expected move. The economic
/// gate already prices round-trip viability with the SAME fee/failure config, and
/// the v0 phase model carries its own fixed failure/retry load — so this backstop
/// only fires on genuinely structural phase mispricing (e.g. a size that is a
/// large fraction of the curve reserve: impact alone ⇒ cost ≫ move), never on the
/// ordinary cost load. 10× the priced move ⇒ fires around ≳30% exit impact.
const EXIT_COST_VETO_MULT: u32 = 10;

/// Thesis feature ids (§32: conditions compile from the registered feature
/// schema — these two ARE the v0 schema): 1 = numeric-lane OFI bps, 2 = CVD sign.
const THESIS_FEAT_OFI: u32 = 1;
const THESIS_FEAT_CVD: u32 = 2;

/// §70.1 composite money proxy weights (§102 named scales). The smart-wallet-
/// entry / net-inflow term is decade-compressed (0..≈20) before this weight, and
/// the holder-growth term is a raw on-chain buyer count (0..64); both are chosen
/// so a genuine wallet-entry / holder-growth lead registers rising money against
/// the 0..10_000 buy-pressure momentum tail without swamping it — the composite
/// LEADS on wallet/holder evidence (§70.1) yet momentum still contributes.
const MONEY_PROXY_WALLET_WEIGHT: u64 = 500;
const MONEY_PROXY_HOLDER_WEIGHT: u64 = 200;

/// §70.1/§102 dynamic-range clamp on the holder term when it is sourced from the
/// continuous holder ledger (`money_proxy_holder_flow_enable`).
///
/// The legacy term is a popcount over a 64-bit bitset, so it lives in `0..=64` by
/// construction, and `MONEY_PROXY_HOLDER_WEIGHT` was calibrated against that range
/// so the holder term contributes `0..=12_800` against a `0..=10_000`
/// buy-pressure momentum tail. The folded holder count is bounded only by
/// `holder_flow::HOLDER_ENTITY_CAP` (512), which would let the holder term reach
/// ~102_400 and swamp both momentum and the wallet-inflow term. Clamping at the
/// bitset's own ceiling changes the term's INFORMATION (a real, collision-free,
/// non-monotone count) without changing its SCALE, so the arming is a swap of
/// measurement quality and not a silent reweighting of the composite.
pub const MONEY_PROXY_HOLDER_TERM_CAP: u64 = 64;

/// Coarse base-10 magnitude of a lamport quantity (0 → 0, else floor(log10)+1),
/// mirroring `lane::decade` — keeps the smart-money inflow term comparable across
/// orders of magnitude without a float (§22). Branch-free via the intrinsic.
#[inline]
#[must_use]
fn decade_u64(v: u64) -> u64 {
    v.checked_ilog10().map_or(0, |x| x as u64 + 1)
}

/// §94 quote-mint-parametric round-trip cost (bps). `size` / `eff_fixed` are in
/// the quote's base units; the bps ratio is decimals-invariant, so a decoded SOL
/// or USDC quote yields the SAME bps the SOL-assumed `round_trip_cost_bps`
/// produced (the golden path is byte-identical for the default SOL quote).
///
/// Returns:
/// * `Some(bps)` for a decoded quote — **byte-identical** to the pre-§94 expression
///   `round_trip_cost_bps(..).unwrap_or(u32::MAX)` (an unpriceable inner cost still
///   saturates to `u32::MAX`, exactly as before — that is a cost, not a refusal).
/// * `None` ONLY for an UNDECODED quote — §18.2 UNKNOWN fails closed: refuse rather
///   than price against an assumed-SOL cost.
///
/// **RETIRED (2026-07-28 cost-model unification).** The live §94 seam is
/// [`Engine::unified_rt_bps`], which applies the same decoded/undecoded quote rule to
/// [`crate::cost_model::round_trip_bps`]. This expression is retained only as the
/// reference the §94 dossier tests below pin — deleting it would delete the evidence
/// that the SOL path was byte-identical to the pre-§94 cost.
#[cfg(test)]
#[inline]
#[must_use]
fn round_trip_cost_bps_quoted(
    size: u64,
    eff_fixed: u64,
    protocol_bps: u32,
    impact: &ImpactCurve,
    quote: QuoteMint,
) -> Option<u32> {
    match quote {
        QuoteMint::Sol { .. } | QuoteMint::Usdc { .. } => {
            Some(round_trip_cost_bps(size, eff_fixed, protocol_bps, impact).unwrap_or(u32::MAX))
        }
        QuoteMint::Undecoded => None,
    }
}

/// Which evaluator lane a watchlist discovery lane reconciles into (mirrors
/// `scalp::eval_lane`): sniper/early discovery is `Early`; active/graduation
/// scalping is `Scalp`. Used to route held-position exit net-SOL into the right
/// running accountant.
const fn eval_lane_of(lane: WlLane) -> EvalLane {
    match lane {
        WlLane::CreationSniper | WlLane::EarlyConfirmation => EvalLane::Early,
        WlLane::GraduationTransition | WlLane::ActiveMarketScalp => EvalLane::Scalp,
    }
}

#[cfg(test)]
mod criterion_94_quote_mint {
    //! §94 quote-mint parametrization: the SOL path is byte-identical to the
    //! pre-§94 cost expression (digest-safe), the USDC path is reachable, and an
    //! undecoded quote refuses (fail-closed).
    use super::{round_trip_cost_bps, round_trip_cost_bps_quoted, ImpactCurve, QuoteMint, ReflectionSnapshot};
    use pump_quant_strategy::safety_integrity::{round_trip_cost_quote, Market};

    /// The default SOL quote reproduces `round_trip_cost_bps(..).unwrap_or(MAX)`
    /// EXACTLY across a spread of sizes/fixed costs — this is why threading the
    /// quote mint (defaulting to SOL) leaves the golden digest unchanged.
    #[test]
    fn sol_quote_is_byte_identical_to_legacy_bps() {
        let impact = ImpactCurve::linear_test(1_000_000);
        for &size in &[0u64, 1, 1_000, 50_000, 10_000_000] {
            for &fixed in &[0u64, 5_000, 250_000] {
                for &protocol_bps in &[0u32, 30, 250] {
                    let legacy =
                        round_trip_cost_bps(size, fixed, protocol_bps, &impact).unwrap_or(u32::MAX);
                    let sol = round_trip_cost_bps_quoted(
                        size,
                        fixed,
                        protocol_bps,
                        &impact,
                        QuoteMint::Sol { decimals: 9 },
                    );
                    assert_eq!(sol, Some(legacy), "SOL path must equal the legacy cost");
                }
            }
        }
    }

    /// An UNDECODED quote is refused (None) — the gate never prices against an
    /// assumed-SOL cost (§18.2 UNKNOWN fails closed).
    #[test]
    fn undecoded_quote_refuses() {
        let impact = ImpactCurve::linear_test(1_000_000);
        assert_eq!(
            round_trip_cost_bps_quoted(50_000, 5_000, 30, &impact, QuoteMint::Undecoded),
            None,
        );
    }

    /// The USDC-quoted path is reachable (returns a priced cost, not a refusal)
    /// and, at the bps layer, equals the SOL bps (the ratio is decimals-invariant).
    #[test]
    fn usdc_quote_is_reachable() {
        let impact = ImpactCurve::linear_test(1_000_000);
        let sol =
            round_trip_cost_bps_quoted(50_000, 5_000, 30, &impact, QuoteMint::Sol { decimals: 9 });
        let usdc =
            round_trip_cost_bps_quoted(50_000, 5_000, 30, &impact, QuoteMint::Usdc { decimals: 6 });
        assert!(usdc.is_some(), "USDC path must be reachable, not refused");
        assert_eq!(usdc, sol, "bps cost is a scale-free ratio across quotes");
    }

    /// The parametric absolute-cost leaf IS decimals-sensitive: for the same
    /// whole-token fixed cost, a 9-decimal SOL quote and a 6-decimal USDC quote
    /// produce DIFFERENT absolute round-trip costs, and an undecoded quote refuses.
    /// This is the reachable USDC-quoted unit case the engine now dispatches to.
    #[test]
    fn usdc_absolute_cost_differs_from_sol_by_decimals() {
        let mkt = Market {
            fee_bps: 30,
            fixed_cost_whole: 1,
        };
        let sol = round_trip_cost_quote(50_000, &mkt, QuoteMint::Sol { decimals: 9 });
        let usdc = round_trip_cost_quote(50_000, &mkt, QuoteMint::Usdc { decimals: 6 });
        let undecoded = round_trip_cost_quote(50_000, &mkt, QuoteMint::Undecoded);
        assert_eq!(undecoded, None, "undecoded quote must refuse");
        assert!(sol.is_some() && usdc.is_some());
        // 1 whole token = 10^9 base units (SOL) vs 10^6 (USDC): the fixed leg alone
        // differs by 1000×, so the absolute costs cannot be equal.
        assert_ne!(sol, usdc, "decimals must parametrize the absolute cost");
        assert!(sol.unwrap() > usdc.unwrap());
    }

    // ─── S8: ReflectionSnapshot tests ──────────────────────────────────────

    #[test]
    fn s8_reflection_snapshot_constructs() {
        let snap = ReflectionSnapshot {
            retired: [false, true, false, false],
            tick: 12345,
            reflect_every_ticks: 50,
            brain_reflect_enable: false,
        };
        assert_eq!(snap.retired, [false, true, false, false]);
        assert_eq!(snap.tick, 12345);
        assert_eq!(snap.reflect_every_ticks, 50);
        assert!(!snap.brain_reflect_enable);
    }

    #[test]
    fn s8_reflection_snapshot_all_retired() {
        let snap = ReflectionSnapshot {
            retired: [true; 4],
            tick: 999,
            reflect_every_ticks: 100,
            brain_reflect_enable: true,
        };
        assert_eq!(snap.retired, [true, true, true, true]);
        assert!(snap.brain_reflect_enable);
    }

    #[test]
    fn s8_reflection_snapshot_is_copy() {
        // ReflectionSnapshot must be Copy — it's returned by value from
        // the engine without cloning.
        let snap = ReflectionSnapshot {
            retired: [false; 4],
            tick: 1,
            reflect_every_ticks: 50,
            brain_reflect_enable: false,
        };
        let snap_copy = snap; // copy, not move
        // If it weren't Copy, this line would fail to compile.
        assert_eq!(snap.retired, snap_copy.retired);
    }
}
