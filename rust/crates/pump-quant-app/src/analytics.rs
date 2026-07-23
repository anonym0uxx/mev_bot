//! Reflection & report analytics — the evaluator surface the app never consumed.
//!
//! The evaluator/memory/social crates ship the §47–§56 research instruments
//! (CVaR, profit factor, top-k excision, PRFS gate ledgers, convexity rule
//! ledgers, edge decomposition, sequential retirement, sizing validation, VOI
//! ranking, D1–D10 source quality), but until now the engine computed none of
//! them: the loop graded itself on raw net-SOL alone. This module folds all of
//! it behind ONE bounded struct — [`ReflectionAnalytics`] — that the engine
//! feeds from its exit/reject/haircut seams and drains at reflection cadence
//! ([`ReflectionAnalytics::reflect`]) and at report time
//! ([`ReflectionAnalytics::report`]).
//!
//! Coverage by section:
//! * **§47 PRFS** — every gate rejection is recorded and marked forward against
//!   the live price feed; `prfs_fold` scores each gate on BOTH sides of the
//!   over-rejection law (loss avoided AND upside foregone).
//! * **§48/§56.11 retirement** — per-lane CUSUM edge-decay verdicts over that
//!   lane's realized outcomes ([`ReflectionAnalytics::retirement_verdicts`]).
//! * **§49 convexity** — every haircut/veto/exit event feeds the two-sided
//!   rule ledger (`build_ledger`): no rule is scored on avoided losses alone.
//! * **§50 edge decomposition** — an honest-UNKNOWN decomposition: components
//!   are unattributed until the engine captures them, so the residual IS the
//!   unexplained edge, never a fabricated attribution.
//! * **§51/§33 sizing** — log-utility fit + bootstrap band + Monte-Carlo
//!   survival, folded into one clamped recommendation
//!   ([`ReflectionAnalytics::sizing_recommendation`]).
//! * **§52 baseline book** — a naive buy-every-confirm shadow total the live
//!   policy must beat ([`ReflectionAnalytics::record_baseline`]).
//! * **§54 report** — CVaR(5%), profit factor, median, excision flips, MFE
//!   capture, all in one [`AnalyticsReport`].
//! * **§29.8 source quality** — reconciled social calls fold into D1/D4/D8
//!   determinant bundles (everything else honest-UNKNOWN) and the bounded
//!   [`SourceQualityLedger`] ([`ReflectionAnalytics::fold_social_quality`]).
//! * **§56.10 VOI** — the standing research backlog ranked by value of
//!   information ([`ReflectionAnalytics::voi_ranking`]).
//!
//! # Discipline (binding)
//! Deterministic, integer-only, no wall-clock, no RNG in decision paths — the
//! bootstrap/Monte-Carlo seeds are fixed named constants (§22). All state is
//! bounded (§99): every ring below has a named cap and evicts its OLDEST entry.
//! Everything here runs at reflection cadence or report time — off the hot path.

use pump_quant_evaluator::convexity_enrich::{event_from_mark, ConvexityMark};
use pump_quant_evaluator::convexity_ledger::{
    build_ledger, ConvexityEvent, RuleId, RuleKind, RuleLedger,
};
use pump_quant_evaluator::edge_decomposition::{aggregate_edge, ComponentValue, PerTradeEdge};
use pump_quant_evaluator::evaluator_stats::{
    mfe_capture, prfs_fold, topk_excision, ArchetypeKey, Excision, ExcursionRow, GateId,
    GateLedger, Lane, MfeReport, PrfsSample, Side, TradeId,
};
use pump_quant_evaluator::exit_markout::{
    exit_markout_cells, foregone_upside, ExitMarkoutRow, ExitReason as MarkoutReason,
    ForegoneUpside, MarkoutCellNs, MANDATED_HORIZONS_NS,
};
use pump_quant_evaluator::metrics::{
    cvar, median_return_bps, profit_factor, CvarReport, ProfitFactor,
};
use pump_quant_evaluator::sequential_retirement::{
    sequential_retirement, RetirementConfig, RetirementVerdict,
};
use pump_quant_evaluator::sizing_validator::{
    bootstrap_fraction_band, monte_carlo_survival, optimal_log_utility,
};
use pump_quant_evaluator::social_ledger::SocialCall;
use pump_quant_memory::rows::{Hypothesis, HypothesisId, InferenceState};
use pump_quant_memory::voi::rank_open;
use pump_quant_social::classification::{ClassificationConfig, DeterminantBundle};
use pump_quant_social::determinants::{
    d1_reconciled_markouts, d4_selectivity, d8_originality, MarkoutSample,
};
use pump_quant_social::ledger::SourceQualityLedger;
use pump_quant_social::types::DeterminantScore;
use std::collections::{BTreeMap, VecDeque};

// ============================================================================
// Named constants (§102 — each with its rationale).
// ============================================================================

/// Number of watchlist discovery lanes (§71: on-chain flow, narrative, social,
/// smart-money). Per-lane statistics are arrays of this length.
const LANES: usize = 4;

/// Trade-record ring capacity (§99). 4096 realized exits is weeks of paper
/// volume — enough for every distributional statistic here while keeping worst
/// case memory ~a few hundred KB.
const TRADE_RING_CAP: usize = 4096;

/// Pending gate-rejection ring capacity (§99). Rejections are marked forward
/// once and retired, so 512 covers the between-reflection backlog; overflow
/// evicts the OLDEST pending mark (the least-current evidence).
const REJECT_RING_CAP: usize = 512;

/// Folded PRFS sample ring capacity (§99): the marked-forward rejection
/// history the per-gate ledgers are computed over.
const PRFS_SAMPLE_CAP: usize = 4096;

/// Convexity event ring capacity (§99): haircut/veto/exit observations kept
/// for the §49 rule ledgers.
const CONVEXITY_RING_CAP: usize = 1024;

/// §47/§54 LAW 17 pending post-exit markout ring capacity (§99): exits awaiting
/// their forward price samples. Evicts the oldest pending exit past the cap.
const MARKOUT_PENDING_CAP: usize = 1024;
/// §47/§54 LAW 17 folded markout-sample ring capacity (§99): the post-exit price
/// observations the markout-cell / foregone-upside tables are computed over.
const MARKOUT_ROW_CAP: usize = 4096;
/// §47 LAW 17 tick→ns quantum: each logical replay tick is one 250ms step, so the
/// mandated markout horizons (250ms/1s/5s/30s/5m) land on integer tick offsets
/// `[1, 4, 20, 120, 1200]`. Deterministic, integer (§22) — a replay carries no
/// wall-clock, so the info-time horizon is derived from the event-stream tick.
const MARKOUT_TICK_NS: u64 = 250_000_000;
/// The mandated markout horizons expressed in ticks (`MANDATED_HORIZONS_NS /
/// MARKOUT_TICK_NS`), in ascending order — the forward-sample checkpoints.
const MARKOUT_HORIZON_TICKS: [u64; 5] = [1, 4, 20, 120, 1200];

/// Logical-tick duration in seconds for PRFS horizon accounting. The app's
/// tick is the ~1 s engine cadence tick, so tick deltas ARE seconds; PRFS
/// horizons (`horizon_s`) are tick deltas under this scale.
const SECONDS_PER_TICK: u64 = 1;

/// Earliest forward-mark age, ticks (§47): a rejection younger than ~1 minute
/// has no measurable post-rejection fate yet, so it stays pending.
const PRFS_MARK_DELAY_TICKS: u64 = 60;

/// Pending-mark expiry, ticks: past 24 h (the PRFS ledger's counting window) an
/// unmarkable rejection is dropped. A mint whose feed went dark is dropped
/// WITHOUT fabricating a zero price — inventing a total-loss mark would
/// overcount loss-avoided in the gate's favor (§47 honesty).
const PRFS_EXPIRE_TICKS: u64 = 86_400;

/// Horizon ceiling fed to `prfs_fold`, seconds: samples are capped at the 24 h
/// boundary so a late mark still lands inside the ledger's counting window.
const PRFS_HORIZON_CAP_S: u32 = 86_400;

/// CVaR tail level, bps: 500 == the worst 5% of trades (§54's tail-loss lens).
const CVAR_ALPHA_BPS: u32 = 500;

/// Top-k excision depths (§54): does the book survive losing its best 1/3/5
/// trades? Kamat-class fragility shows up as `flipped_negative`.
const EXCISION_KS: [u32; 3] = [1, 3, 5];

/// Convexity "runner" threshold, bps of counterfactual outcome: a suppressed
/// event whose full-position outcome was ≥ 2× counts as a missed runner (the
/// right-tail winners the §48 exit family exists to harvest).
const RUNNER_THRESHOLD_BPS: i64 = 20_000;

/// Default excursion archetype id: the single app-level archetype until the
/// engine tags per-setup archetypes (§54 MFE capture report).
const DEFAULT_ARCHETYPE_ID: u64 = 0;

/// Minimum realized trades before a sizing recommendation exists (§33/§51 —
/// below this the log-utility fit is noise, so the verdict is honest `None`).
const SIZING_MIN_TRADES: usize = 64;

/// Sizing grid ceiling, bps of capital: recommendations never exceed 25% per
/// trade regardless of fit (fractional-Kelly survival discipline, §33).
const SIZING_F_MAX_BPS: u32 = 2_500;

/// Sizing grid step, bps. Must be > 0 (the validator panics on a zero step);
/// 50 bps resolution is finer than any operator envelope here.
const SIZING_STEP_BPS: u32 = 50;

/// Bootstrap resamples for the fraction uncertainty band. 200 keeps the p05
/// stable across runs (it is seeded, so byte-stable anyway) at trivial cost.
const SIZING_BOOTSTRAP_RESAMPLES: u32 = 200;

/// Monte-Carlo survival paths / path length: 128 × 96 compounded trades
/// approximates a quarter of forward volume — enough to expose ruin at a
/// candidate fraction without burdening the reflection pass.
const SIZING_MC_PATHS: u32 = 128;
/// See [`SIZING_MC_PATHS`].
const SIZING_MC_PATH_LEN: u32 = 96;

/// Ruin floor, bps of starting equity (must be < 10_000 — the validator
/// panics otherwise): a path that loses half the stake is counted dead (§33).
const SIZING_RUIN_BPS: u32 = 5_000;

/// Minimum surviving-path share, bps: a fraction is acceptable only when ≥95%
/// of Monte-Carlo paths finish alive (§33 survival-first).
const SIZING_SURVIVAL_MIN_BPS: u64 = 9_500;

/// Fixed deterministic seed for the seeded bootstrap/Monte-Carlo APIs (§22:
/// seeds are pinned constants — the same book always yields the same verdict).
const SIZING_SEED: u64 = 0x5EED_0033_5EED_0033;

/// Retirement null reference, lamports (§56.11): a lane must at least break
/// even net; outcomes below zero erode its standing.
const RETIRE_REFERENCE_LAMPORTS: i128 = 0;

/// Retirement slack, lamports: 0.02 SOL of per-trade noise tolerated before a
/// shortfall counts — keeps the CUSUM from twitching on fee jitter.
const RETIRE_SLACK_LAMPORTS: i128 = 20_000_000;

/// Retirement decision threshold, lamports: 2 SOL of accumulated below-null
/// deficit declares the lane's edge decayed (a full session's worth of bleed).
const RETIRE_THRESHOLD_LAMPORTS: i128 = 2_000_000_000;

/// Retirement learning horizon, samples (§56.11): RETIRE cannot bind before 30
/// reconciled outcomes no matter how bad the early sample looks.
const RETIRE_MIN_SAMPLES: u32 = 30;

/// D1 proxy price at call, fixed-point: the contract anchor 1_000_000; horizon
/// prices are this scaled by the call's realized bps (§29.8 markout proxy).
const D1_PRICE_AT_CALL: u64 = 1_000_000;

/// Notional against which a social call's realized net lamports become bps:
/// 1 SOL — the reference follower size the §29.8 reconciliation assumes.
const SOCIAL_NOTIONAL_LAMPORTS: i128 = 1_000_000_000;

/// Per-call markout cap, bps: one 10× call cannot dominate a source's D1
/// (values above are clamped; the floor is −10_000 == a total loss).
const D1_MARKOUT_CAP_BPS: i64 = 90_000;

/// Markout magnitude, bps, assigned when a call's net rounds to zero but the
/// reconciler flagged it favorable/unfavorable — the sign must survive the
/// integer rounding or thin-size callers would all read as zero evidence.
const D1_NEUTRAL_SIGN_BPS: i64 = 250;

/// D1 horizon blend weights for [+5m, +30m, +2h, +24h]. The proxy carries one
/// realized outcome across all four horizons, so equal weights are exact.
const D1_HORIZON_WEIGHTS: [i64; 4] = [1, 1, 1, 1];

/// D1 age-decay half-life, ns (~14 days — mirrors the classification config's
/// decay so both halves of §29.8 age evidence at the same rate).
const D1_HALF_LIFE_NS: u64 = 14 * 24 * 60 * 60 * 1_000_000_000;

/// D4 call budget per day: a selective caller stays under ~5 calls/day; above
/// it precision is discounted as volume-spam (§29.8 D4).
const D4_BUDGET_CALLS_PER_DAY: u64 = 5;

/// Nanoseconds per day, for D4 calls-per-day estimation.
const DAY_NS: u128 = 86_400_000_000_000;

/// D8 originator-share threshold, bps, below which a source is echo-heavy —
/// mirror of `ClassificationConfig::fade_first_default().echo_originality_bps`.
const D8_ECHO_THRESHOLD_BPS: i64 = 3_000;

/// One standing research-backlog hypothesis for the §56.10 VOI queue.
struct VoiBacklogItem {
    /// Stable hypothesis id (1..=6).
    id: u64,
    /// Expected net-SOL impact if true, lamports.
    impact_lamports: i128,
    /// Prior probability true, bps.
    prob_true_bps: i64,
    /// Cost to run the deciding experiment, lamports.
    cost_lamports: u64,
    /// Edge half-life, seconds.
    half_life_secs: u64,
}

/// The standing ledger backlog (§56.10), pre-registered as static hypotheses.
/// Impacts/costs are operator estimates in lamports; half-lives reflect how
/// perishable each edge is. Ranked by [`ReflectionAnalytics::voi_ranking`].
const VOI_BACKLOG: [VoiBacklogItem; 6] = [
    // 1: capture per-component costs so the §50 residual stops being the
    // whole book (largest standing attribution gap).
    VoiBacklogItem {
        id: 1,
        impact_lamports: 3_000_000_000,
        prob_true_bps: 7_000,
        cost_lamports: 200_000_000,
        half_life_secs: 30 * 86_400,
    },
    // 2: real multi-horizon social markout capture replacing the D1 proxy.
    VoiBacklogItem {
        id: 2,
        impact_lamports: 2_000_000_000,
        prob_true_bps: 6_000,
        cost_lamports: 500_000_000,
        half_life_secs: 14 * 86_400,
    },
    // 3: multi-horizon PRFS marking (5m/30m/2h/24h) instead of one mark.
    VoiBacklogItem {
        id: 3,
        impact_lamports: 1_500_000_000,
        prob_true_bps: 6_500,
        cost_lamports: 300_000_000,
        half_life_secs: 30 * 86_400,
    },
    // 4: adopt a tournament-flagged exit challenger (§48 — operator-gated).
    VoiBacklogItem {
        id: 4,
        impact_lamports: 4_000_000_000,
        prob_true_bps: 5_500,
        cost_lamports: 1_000_000_000,
        half_life_secs: 21 * 86_400,
    },
    // 5: creator-wallet linkage evidence for the D5 skin-in-game determinant.
    VoiBacklogItem {
        id: 5,
        impact_lamports: 2_500_000_000,
        prob_true_bps: 5_000,
        cost_lamports: 800_000_000,
        half_life_secs: 60 * 86_400,
    },
    // 6: §52 baseline-destruction test wired end-to-end on live confirms.
    VoiBacklogItem {
        id: 6,
        impact_lamports: 1_000_000_000,
        prob_true_bps: 8_000,
        cost_lamports: 100_000_000,
        half_life_secs: 90 * 86_400,
    },
];

// ============================================================================
// Internal bounded records.
// ============================================================================

/// One realized exit, as folded by [`ReflectionAnalytics::record_trade`].
#[derive(Clone, Copy, Debug)]
struct TradeRecord {
    /// Monotonic id (assigned at record time; survives ring eviction gaps).
    id: u64,
    /// Watchlist lane index, clamped to `0..LANES`.
    lane_idx: u8,
    /// Realized net, lamports (signed).
    net_lamports: i128,
    /// Per-trade return, bps of deployed size.
    return_bps: i64,
    /// Max favorable excursion, bps.
    mfe_bps: i64,
    /// Max adverse excursion, bps.
    mae_bps: i64,
    /// §25 derived setup archetype (0 = None / classifier off) — the excursion
    /// grouping key (§54 per-archetype MFE capture).
    archetype: u16,
}

/// One pending gate rejection awaiting its PRFS forward mark (§47).
#[derive(Clone, Copy, Debug)]
struct RejectRecord {
    /// Gate code (becomes the [`GateId`] in the ledger).
    gate_code: u8,
    /// Rejected mint.
    mint: [u8; 32],
    /// Fixed-point price at the moment of rejection.
    price_fp: u64,
    /// Logical tick of the rejection.
    tick: u64,
    /// §25 derived setup archetype of the rejected candidate (0 = None).
    archetype: u16,
}

// ============================================================================
// The report struct.
// ============================================================================

/// Everything §54 requires from an end-of-run report, in one plain struct.
///
/// Distributional statistics (CVaR, profit factor, median, excisions, MFE,
/// edge residual) cover the last [`TRADE_RING_CAP`] trades; the counters
/// (`trades`, baseline totals, `live_net_lamports`) are all-time totals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsReport {
    /// CVaR at the 5% tail over per-trade returns, `None` when no trades.
    pub cvar: Option<CvarReport>,
    /// Profit factor in bps (`10_000` = break-even). `0` when there are no
    /// trades; `u64::MAX` when there are wins but zero losses (the no-loss
    /// saturation — an infinite gross factor).
    pub profit_factor_bps: u64,
    /// Median per-trade return, bps; `None` when no trades.
    pub median_return_bps: Option<i64>,
    /// Top-k winner-excision results for k = 1, 3, 5 (§54 fragility probe).
    pub excisions: Vec<Excision>,
    /// Per-gate PRFS ledgers over the marked-forward rejections (§47).
    pub prfs_gates: Vec<GateLedger>,
    /// Per-rule two-sided convexity ledgers (§49).
    pub convexity_rules: Vec<RuleLedger>,
    /// §50 edge-attribution residual, lamports: with components honest-UNKNOWN
    /// this equals the windowed realized net — the unexplained edge.
    pub edge_residual_lamports: i128,
    /// MFE/MAE distribution + capture efficiency for the default archetype.
    pub mfe_capture: MfeReport,
    /// §25 LAW 4: number of DISTINCT setup archetypes tagged across the realized
    /// trade ring. `1` (or `0`, empty ring) when the classifier is off — every
    /// row is the all-0 `None` bucket; `≥2` once the classifier tags distinct
    /// families, the audit evidence that `archetype:0` is no longer a stub.
    pub distinct_archetypes: u32,
    /// §25 LAW 4: the sorted DISTINCT setup-archetype ids present in the realized
    /// trade ring — `[0]` with the classifier off, the real derived families once
    /// on (the direct proof the stub is replaced).
    pub archetypes: Vec<u16>,
    /// §25 LAW 4: number of DISTINCT setup archetypes tagged across the pending
    /// gate-rejection ring (the reject-sample side of the same tagging). `≤1`
    /// with the classifier off.
    pub distinct_reject_archetypes: u32,
    /// All-time realized trade count.
    pub trades: u32,
    /// All-time §52 baseline (buy-every-confirm shadow) trade count.
    pub baseline_trades: u32,
    /// All-time §52 baseline net, lamports.
    pub baseline_net_lamports: i128,
    /// All-time live-policy net, lamports (the figure the baseline must lose to).
    pub live_net_lamports: i128,
    /// §47/§54 LAW 17 post-exit markout cells, one per `(ExitReason, horizon_ns)`
    /// bucket populated over the run — the fine-grained exit ruler at the mandated
    /// 250ms/1s/5s/30s/5m horizons. Empty until exits have been marked forward.
    pub markout_cells: Vec<MarkoutCellNs>,
    /// §47 LAW 17 per-`(ExitReason, horizon_ns)` foregone-upside / loss-avoided
    /// aggregate over the same post-exit samples — the two-sided over-/under-exit
    /// ruler. Empty until exits have been marked forward.
    pub foregone_upside: Vec<ForegoneUpside>,
}

// ============================================================================
// The analytics fold.
// ============================================================================

/// The one bounded reflection/report analytics fold the engine feeds and
/// drains. See the module docs for the section-by-section coverage map.
#[derive(Debug)]
pub struct ReflectionAnalytics {
    /// Realized-exit ring (cap [`TRADE_RING_CAP`], oldest evicted).
    trades: VecDeque<TradeRecord>,
    /// Monotonic trade-id counter (never reset; ids survive eviction).
    next_trade_id: u64,
    /// All-time realized trade count (saturating).
    total_trades: u32,
    /// All-time live net, lamports (saturating).
    live_net_lamports: i128,
    /// Pending rejections awaiting forward marks (cap [`REJECT_RING_CAP`]).
    rejects: VecDeque<RejectRecord>,
    /// Marked-forward PRFS samples (cap [`PRFS_SAMPLE_CAP`]).
    prfs_samples: VecDeque<PrfsSample>,
    /// Convexity events (cap [`CONVEXITY_RING_CAP`]).
    convexity: VecDeque<ConvexityEvent>,
    /// All-time §52 baseline totals (O(1) state).
    baseline_trades: u32,
    /// All-time §52 baseline net, lamports (saturating).
    baseline_net_lamports: i128,
    /// §47 LAW 17 exits awaiting forward markout samples (cap
    /// [`MARKOUT_PENDING_CAP`], oldest evicted).
    markout_pending: VecDeque<PendingMarkout>,
    /// §47 LAW 17 folded post-exit markout samples (cap [`MARKOUT_ROW_CAP`]).
    markout_rows: VecDeque<ExitMarkoutRow>,
}

/// §47 LAW 17 one exit awaiting its post-exit forward price samples: the fill
/// price it closed at, the tick it closed on, its reason, and which horizon
/// checkpoints have already been sampled.
#[derive(Clone, Copy, Debug)]
struct PendingMarkout {
    mint: [u8; 32],
    exit_price: u64,
    exit_tick: u64,
    reason: MarkoutReason,
    /// Index of the next unsampled checkpoint in [`MARKOUT_HORIZON_TICKS`].
    next_idx: usize,
}

/// §47 LAW 17: map an app [`crate::position::ExitReason`] code onto the evaluator's
/// coarser markout reason bucket. Take-profit/into-strength are harvests;
/// stops/rug/thesis are protective; time is the time-stop; force/creator-dump are
/// discretionary/manual closes.
fn markout_reason_from_code(code: u8) -> MarkoutReason {
    match code {
        4 | 9 => MarkoutReason::TakeProfit, // TakeProfitLadder, IntoStrength
        2 | 3 => MarkoutReason::StopLoss,   // HardStop, ThesisInvalidation
        5 => MarkoutReason::TrailingStop,   // TrailingStop
        6 => MarkoutReason::TimeStop,       // TimeStop
        1 => MarkoutReason::LiquidityAbort, // RugPrecursor
        _ => MarkoutReason::Manual,         // ForceClose, CreatorDump, unknown
    }
}

impl ReflectionAnalytics {
    /// A fresh, empty analytics fold with all rings at their named caps.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trades: VecDeque::with_capacity(TRADE_RING_CAP),
            next_trade_id: 0,
            total_trades: 0,
            live_net_lamports: 0,
            rejects: VecDeque::with_capacity(REJECT_RING_CAP),
            prfs_samples: VecDeque::with_capacity(PRFS_SAMPLE_CAP),
            convexity: VecDeque::with_capacity(CONVEXITY_RING_CAP),
            baseline_trades: 0,
            baseline_net_lamports: 0,
            markout_pending: VecDeque::with_capacity(MARKOUT_PENDING_CAP),
            markout_rows: VecDeque::with_capacity(MARKOUT_ROW_CAP),
        }
    }

    /// Fold one realized exit: lane index (`0..4`, clamped), exit-reason code,
    /// net lamports, deployed size, and the excursion extremes in bps. The
    /// per-trade return is derived as `net · 10_000 / size` (bps of deployed
    /// size, `size` floored at 1). Oldest record evicted past the ring cap.
    #[allow(clippy::too_many_arguments)]
    pub fn record_trade(
        &mut self,
        lane_idx: usize,
        reason_code: u8,
        net_lamports: i128,
        size_lamports: u64,
        mfe_bps: i64,
        mae_bps: i64,
        archetype: u16,
    ) {
        // The reason code is journal metadata for future per-reason splits; it
        // does not alter any statistic here, so it is folded and dropped.
        let _ = reason_code;
        let size = i128::from(size_lamports.max(1));
        let ret = net_lamports.saturating_mul(10_000) / size;
        let return_bps = ret.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        if self.trades.len() >= TRADE_RING_CAP {
            self.trades.pop_front();
        }
        self.trades.push_back(TradeRecord {
            id: self.next_trade_id,
            lane_idx: lane_idx.min(LANES - 1) as u8,
            net_lamports,
            return_bps,
            mfe_bps,
            mae_bps,
            archetype,
        });
        self.next_trade_id = self.next_trade_id.saturating_add(1);
        self.total_trades = self.total_trades.saturating_add(1);
        self.live_net_lamports = self.live_net_lamports.saturating_add(net_lamports);
    }

    /// §52 LAW 16: the retained realized trades as `(net_lamports, return_bps,
    /// lane_idx)` triples, oldest first — the tape the deterministic baseline
    /// FAMILY replays over. Bounded by the trade ring cap. Report-only.
    #[must_use]
    pub fn realized_trade_tape(&self) -> Vec<(i128, i64, u8)> {
        self.trades
            .iter()
            .map(|t| (t.net_lamports, t.return_bps, t.lane_idx))
            .collect()
    }

    /// Fold one §52 baseline-book outcome (the engine's naive buy-every-confirm
    /// fixed-TP/SL shadow figure). Totals only — O(1) state.
    pub fn record_baseline(&mut self, net_lamports: i128) {
        self.baseline_trades = self.baseline_trades.saturating_add(1);
        self.baseline_net_lamports = self.baseline_net_lamports.saturating_add(net_lamports);
    }

    /// Fold one gate rejection for §47 PRFS forward marking: the gate code,
    /// the mint, its fixed-point price at rejection, and the tick. Oldest
    /// pending mark evicted past the ring cap.
    pub fn record_reject(
        &mut self,
        gate_code: u8,
        mint: [u8; 32],
        price_fp: u64,
        tick: u64,
        archetype: u16,
    ) {
        if self.rejects.len() >= REJECT_RING_CAP {
            self.rejects.pop_front();
        }
        self.rejects.push_back(RejectRecord {
            gate_code,
            mint,
            price_fp,
            tick,
            archetype,
        });
    }

    /// Fold one §49 convexity event: a haircut/veto/exit that fired (or was
    /// allowed), with the counterfactual full-position outcome. `rule_kind`
    /// codes follow [`RuleKind`] declaration order (0 = `Veto` … 12 =
    /// `Moonbag`); unknown codes fold into the hard-veto family so they are
    /// still scored two-sidedly rather than dropped.
    pub fn record_convexity(
        &mut self,
        rule_kind: u8,
        rule_id: u64,
        suppressed: bool,
        counterfactual_bps: i64,
        realized_bps: i64,
        mfe_bps: i64,
    ) {
        if self.convexity.len() >= CONVEXITY_RING_CAP {
            self.convexity.pop_front();
        }
        self.convexity.push_back(ConvexityEvent {
            rule: RuleId::new(rule_kind_from_code(rule_kind), rule_id),
            suppressed,
            counterfactual_bps,
            realized_bps,
            mfe_bps,
        });
    }

    /// §49 LAW 15: fold one REAL (non-degenerate) convexity event built through the
    /// [`pump_quant_evaluator::convexity_enrich`] layer from an observed mark — a
    /// veto (counterfactual-vs-zero), a haircut (reduced-vs-full size), or a
    /// full-participation allow. Unlike [`Self::record_convexity`], the caller
    /// supplies the semantic mark and the enrich layer constructs the correctly-
    /// signed two-sided event, so a suppression is never recorded degenerate
    /// (counterfactual == realized). Oldest event evicted past the ring cap.
    pub fn record_convexity_mark(&mut self, mark: &ConvexityMark) {
        if self.convexity.len() >= CONVEXITY_RING_CAP {
            self.convexity.pop_front();
        }
        self.convexity.push_back(event_from_mark(mark));
    }

    /// §47 LAW 17: register a closed exit for post-exit markout sampling. The
    /// `reason_code` is the app exit-reason journal code; `exit_price` the fill
    /// price it closed at (0 ⇒ unpriceable, skipped — never a fabricated mark).
    /// Oldest pending exit evicted past the ring cap.
    pub fn record_exit_markout(
        &mut self,
        mint: [u8; 32],
        exit_price: u64,
        tick: u64,
        reason_code: u8,
    ) {
        if exit_price == 0 {
            return;
        }
        if self.markout_pending.len() >= MARKOUT_PENDING_CAP {
            self.markout_pending.pop_front();
        }
        self.markout_pending.push_back(PendingMarkout {
            mint,
            exit_price,
            exit_tick: tick,
            reason: markout_reason_from_code(reason_code),
            next_idx: 0,
        });
    }

    /// §47 LAW 17 forward-marking pass: for every pending exit, sample the current
    /// price at each mandated horizon checkpoint whose tick offset has elapsed,
    /// emitting one [`ExitMarkoutRow`] (a closed long is a `Side::Sell`) per newly
    /// crossed horizon. A pending exit whose feed is dark at a due checkpoint is
    /// skipped for that horizon (no fabricated mark) but stays until its last
    /// horizon; once every checkpoint is sampled or passed it is retired. Info-time
    /// is the event-stream tick (`now_tick`), never wall-clock. Deterministic.
    pub fn mark_forward_markouts(
        &mut self,
        now_tick: u64,
        latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>,
    ) {
        let mut kept: VecDeque<PendingMarkout> =
            VecDeque::with_capacity(self.markout_pending.len());
        while let Some(mut p) = self.markout_pending.pop_front() {
            let elapsed = now_tick.saturating_sub(p.exit_tick);
            while p.next_idx < MARKOUT_HORIZON_TICKS.len()
                && elapsed >= MARKOUT_HORIZON_TICKS[p.next_idx]
            {
                let horizon_ns = MARKOUT_HORIZON_TICKS[p.next_idx] * MARKOUT_TICK_NS;
                if let Some(later) = latest_price_fp(&p.mint) {
                    if self.markout_rows.len() >= MARKOUT_ROW_CAP {
                        self.markout_rows.pop_front();
                    }
                    self.markout_rows.push_back(ExitMarkoutRow {
                        reason: p.reason,
                        side: Side::Sell,
                        exit_price: p.exit_price,
                        horizon_ns,
                        later_price: later,
                    });
                }
                p.next_idx += 1;
            }
            if p.next_idx < MARKOUT_HORIZON_TICKS.len() {
                kept.push_back(p);
            }
        }
        self.markout_pending = kept;
    }

    /// Reflection-cadence pass (§47): mark pending rejections forward against
    /// current prices. A rejection older than [`PRFS_MARK_DELAY_TICKS`] whose
    /// mint still prints is sampled once (horizon = its age in tick-seconds,
    /// capped at 24 h) and retired; one whose feed stays dark past
    /// [`PRFS_EXPIRE_TICKS`] is dropped without a fabricated mark.
    pub fn reflect(&mut self, now_tick: u64, latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>) {
        let mut kept: VecDeque<RejectRecord> = VecDeque::with_capacity(self.rejects.len());
        while let Some(r) = self.rejects.pop_front() {
            let age_ticks = now_tick.saturating_sub(r.tick);
            if age_ticks < PRFS_MARK_DELAY_TICKS {
                kept.push_back(r);
                continue;
            }
            match latest_price_fp(&r.mint) {
                Some(px) => {
                    let horizon_s = u32::try_from(age_ticks.saturating_mul(SECONDS_PER_TICK))
                        .unwrap_or(u32::MAX)
                        .min(PRFS_HORIZON_CAP_S);
                    if self.prfs_samples.len() >= PRFS_SAMPLE_CAP {
                        self.prfs_samples.pop_front();
                    }
                    self.prfs_samples.push_back(PrfsSample {
                        gate: GateId(u64::from(r.gate_code)),
                        ref_price_fp: r.price_fp,
                        sampled_price_fp: px,
                        horizon_s,
                    });
                }
                None if age_ticks >= PRFS_EXPIRE_TICKS => {
                    // Feed went dark within the whole window: drop honestly.
                }
                None => kept.push_back(r),
            }
        }
        self.rejects = kept;
    }

    /// The Layer-2 sizing verdict (§33/§51): fit realized per-trade returns
    /// with the log-utility grid, take the pessimistic edge of the bootstrap
    /// band, then walk the fraction DOWN until ≥95% of seeded Monte-Carlo
    /// paths survive the half-stake ruin floor. The result is clamped into
    /// `[f_floor_bp, f_ceil_bp]` (bounds swapped if passed inverted — no
    /// panicking clamp). `None` below [`SIZING_MIN_TRADES`] realized trades.
    #[must_use]
    pub fn sizing_recommendation(&self, f_floor_bp: u32, f_ceil_bp: u32) -> Option<u32> {
        if self.trades.len() < SIZING_MIN_TRADES {
            return None;
        }
        let returns: Vec<i64> = self.trades.iter().map(|t| t.return_bps).collect();
        let fit = optimal_log_utility(&returns, SIZING_F_MAX_BPS, SIZING_STEP_BPS);
        let band = bootstrap_fraction_band(
            &returns,
            SIZING_F_MAX_BPS,
            SIZING_STEP_BPS,
            SIZING_BOOTSTRAP_RESAMPLES,
            SIZING_SEED,
        );
        // Survival-constrained: start at the more pessimistic of (fit, p05
        // band edge) and step down until the ruin test passes. Bounded by
        // F_MAX/STEP iterations.
        let mut f = fit.optimal_f_bps.min(band.p05);
        while f > 0 {
            let s = monte_carlo_survival(
                &returns,
                f,
                SIZING_MC_PATHS,
                SIZING_MC_PATH_LEN,
                SIZING_RUIN_BPS,
                SIZING_SEED,
            );
            let survived_bps = if s.n_paths == 0 {
                10_000
            } else {
                u64::from(s.survived).saturating_mul(10_000) / u64::from(s.n_paths)
            };
            if survived_bps >= SIZING_SURVIVAL_MIN_BPS {
                break;
            }
            f = f.saturating_sub(SIZING_STEP_BPS);
        }
        let lo = f_floor_bp.min(f_ceil_bp);
        let hi = f_floor_bp.max(f_ceil_bp);
        Some(f.clamp(lo, hi))
    }

    /// Per-lane §56.11 retirement verdicts over each lane's realized outcomes
    /// (chronological, from the trade ring): `true` = retire, the lane is
    /// capital-ineligible. Lane 0 (numeric flow) is the scalp fast-path; lanes
    /// 1..3 (narrative/social/wallet) are early-discovery flavors — the
    /// [`Lane`] tag scopes the config, the verdict is per watchlist lane.
    #[must_use]
    pub fn retirement_verdicts(&self) -> [bool; 4] {
        let mut out = [false; LANES];
        for (idx, slot) in out.iter_mut().enumerate() {
            let outcomes: Vec<i128> = self
                .trades
                .iter()
                .filter(|t| usize::from(t.lane_idx) == idx)
                .map(|t| t.net_lamports)
                .collect();
            let cfg = RetirementConfig {
                lane: if idx == 0 { Lane::Scalp } else { Lane::Early },
                reference_lamports: RETIRE_REFERENCE_LAMPORTS,
                slack_lamports: RETIRE_SLACK_LAMPORTS,
                threshold_lamports: RETIRE_THRESHOLD_LAMPORTS,
                min_samples: RETIRE_MIN_SAMPLES,
            };
            let decision = sequential_retirement(&outcomes, &cfg);
            *slot = decision.verdict == RetirementVerdict::Retire;
        }
        out
    }

    /// The end-of-run/report block (§54): everything computed over the current
    /// rings plus the all-time totals. See [`AnalyticsReport`] field docs.
    #[must_use]
    pub fn report(&self) -> AnalyticsReport {
        let returns: Vec<i64> = self.trades.iter().map(|t| t.return_bps).collect();
        let nets: Vec<(TradeId, i128)> = self
            .trades
            .iter()
            .map(|t| (TradeId(t.id), t.net_lamports))
            .collect();
        let profit_factor_bps = match profit_factor(&returns) {
            ProfitFactor::Bps(v) => v,
            ProfitFactor::NoLosses => u64::MAX,
            ProfitFactor::Empty => 0,
        };
        let prfs_vec: Vec<PrfsSample> = self.prfs_samples.iter().copied().collect();
        let conv_vec: Vec<ConvexityEvent> = self.convexity.iter().copied().collect();
        // §50 honest-UNKNOWN decomposition: no component is attributed until
        // the engine captures it, so the residual is the whole realized net.
        let edges: Vec<PerTradeEdge> = self
            .trades
            .iter()
            .map(|t| PerTradeEdge::new(t.net_lamports, [ComponentValue::unknown(); 9]))
            .collect();
        let excursions: Vec<ExcursionRow> = self
            .trades
            .iter()
            .map(|t| ExcursionRow {
                key: ArchetypeKey {
                    id: u64::from(t.archetype),
                },
                mfe_bps: t.mfe_bps,
                mae_bps: t.mae_bps,
                realized_bps: t.return_bps,
                // Trades folded here already passed the engine's authenticity
                // screens at admit (§21.5) — phantom excursions never enter.
                authenticity_screened: true,
            })
            .collect();
        AnalyticsReport {
            cvar: cvar(&returns, CVAR_ALPHA_BPS),
            profit_factor_bps,
            median_return_bps: median_return_bps(&returns),
            excisions: topk_excision(&nets, &EXCISION_KS),
            prfs_gates: prfs_fold(&prfs_vec),
            convexity_rules: build_ledger(&conv_vec, RUNNER_THRESHOLD_BPS),
            edge_residual_lamports: aggregate_edge(&edges).residual_lamports,
            mfe_capture: mfe_capture(
                &excursions,
                ArchetypeKey {
                    id: DEFAULT_ARCHETYPE_ID,
                },
            ),
            distinct_archetypes: {
                let mut seen: Vec<u16> = self.trades.iter().map(|t| t.archetype).collect();
                seen.sort_unstable();
                seen.dedup();
                seen.len() as u32
            },
            archetypes: {
                let mut seen: Vec<u16> = self.trades.iter().map(|t| t.archetype).collect();
                seen.sort_unstable();
                seen.dedup();
                seen
            },
            distinct_reject_archetypes: {
                let mut seen: Vec<u16> = Vec::new();
                for r in &self.rejects {
                    if !seen.contains(&r.archetype) {
                        seen.push(r.archetype);
                    }
                }
                seen.len() as u32
            },
            trades: self.total_trades,
            baseline_trades: self.baseline_trades,
            baseline_net_lamports: self.baseline_net_lamports,
            live_net_lamports: self.live_net_lamports,
            markout_cells: {
                let rows: Vec<ExitMarkoutRow> = self.markout_rows.iter().copied().collect();
                exit_markout_cells(&rows, &MANDATED_HORIZONS_NS)
            },
            foregone_upside: {
                let rows: Vec<ExitMarkoutRow> = self.markout_rows.iter().copied().collect();
                foregone_upside(&rows, &MANDATED_HORIZONS_NS)
            },
        }
    }

    /// D1–D10 fold (§29.8): group reconciled calls by source, build each
    /// source's determinant bundle, and fold it into `ledger`. Evidence policy:
    /// * only D3-admissible (time-safe) calls count as evidence; a source with
    ///   zero admissible calls still folds (as `InsufficientSample`).
    /// * **D1** — each call becomes one markout proxy: all four horizon prices
    ///   are [`D1_PRICE_AT_CALL`] scaled by the call's realized bps (net over
    ///   [`SOCIAL_NOTIONAL_LAMPORTS`], clamped, sign rescued from the
    ///   `realized_favorable` flag when the net rounds to zero).
    /// * **D4** — calls-per-day over the source's observed call-timestamp span
    ///   against the [`D4_BUDGET_CALLS_PER_DAY`] budget; hit-rate from the
    ///   favorable flags.
    /// * **D8** — sources listed in `coordinated_echo_sources` count all their
    ///   calls as echoes; everyone else as originations.
    /// * **D2/D3/D5/D6/D7/D9/D10** — [`DeterminantScore::empty`]: honest
    ///   UNKNOWN until richer capture exists (§29.8 fade-first).
    ///
    /// Returns the number of distinct sources folded.
    pub fn fold_social_quality(
        &mut self,
        calls: &[SocialCall],
        coordinated_echo_sources: &[u64],
        ledger: &mut SourceQualityLedger,
    ) -> usize {
        let cfg = ClassificationConfig::fade_first_default();
        let mut by_source: BTreeMap<u64, Vec<&SocialCall>> = BTreeMap::new();
        for c in calls {
            let bucket = by_source.entry(c.source_id.0).or_default();
            if c.is_time_safe() {
                bucket.push(c);
            }
        }
        let folded = by_source.len();
        for (source_id, admissible) in &by_source {
            let n = admissible.len();
            let n_u32 = u32::try_from(n).unwrap_or(u32::MAX);

            // D1: realized outcomes as markout proxies.
            let samples: Vec<MarkoutSample> = admissible
                .iter()
                .map(|c| {
                    let px = proxy_horizon_price(call_markout_bps(c));
                    MarkoutSample {
                        price_at_call: D1_PRICE_AT_CALL,
                        price_5m: px,
                        price_30m: px,
                        price_2h: px,
                        price_24h: px,
                        // No 'now' reaches this fold, so calls are decay-neutral
                        // (age 0) — deterministic and unbiased across sources.
                        age_ns: 0,
                    }
                })
                .collect();
            let d1 = d1_reconciled_markouts(&samples, D1_HORIZON_WEIGHTS, D1_HALF_LIFE_NS);

            // D4: selectivity over the observed call window.
            let favorable = admissible.iter().filter(|c| c.realized_favorable).count();
            let hit_rate_bps = if n == 0 {
                0
            } else {
                (favorable as i64).saturating_mul(10_000) / n as i64
            };
            let d4 = d4_selectivity(
                calls_per_day_milli(admissible),
                hit_rate_bps,
                D4_BUDGET_CALLS_PER_DAY,
                n_u32,
            );

            // D8: originator vs coordinated-echo counts, as received.
            let is_echo = coordinated_echo_sources.contains(source_id);
            let d8res = if is_echo {
                d8_originality(0, n_u32, D8_ECHO_THRESHOLD_BPS)
            } else {
                d8_originality(n_u32, 0, D8_ECHO_THRESHOLD_BPS)
            };

            let bundle = DeterminantBundle {
                d1,
                d2: DeterminantScore::empty(),
                d3: DeterminantScore::empty(),
                d4,
                d5: DeterminantScore::empty(),
                d6: DeterminantScore::empty(),
                d7: DeterminantScore::empty(),
                d8: d8res.score,
                d9: DeterminantScore::empty(),
                d10: DeterminantScore::empty(),
                shill_suspect: false,
                post_peak_persistent: false,
                bot_farm: false,
                echo_heavy: d8res.echo_heavy,
                total_sample: n_u32,
            };
            ledger.fold(*source_id, &bundle, &cfg);
        }
        folded
    }

    /// Rank the standing open research backlog by value of information
    /// (§56.10). The six pre-registered [`VOI_BACKLOG`] hypotheses are ranked
    /// by `pump_quant_memory::voi::rank_open`; returns `(hypothesis id, VOI
    /// score in lamports)` pairs, best first. Deterministic total order.
    #[must_use]
    pub fn voi_ranking(&self) -> Vec<(u64, i128)> {
        let hyps: Vec<Hypothesis> = VOI_BACKLOG
            .iter()
            .map(|item| {
                let mut statement_hash = [0u8; 32];
                statement_hash[..8].copy_from_slice(&item.id.to_le_bytes());
                Hypothesis {
                    id: HypothesisId(item.id),
                    schema_version: pump_quant_memory::schema::SCHEMA_VERSION,
                    statement_hash,
                    expected_impact_lamports: item.impact_lamports,
                    prob_true_bps: item.prob_true_bps,
                    cost_to_test_lamports: item.cost_lamports,
                    edge_half_life_secs: item.half_life_secs,
                    status: InferenceState::Hypothesis,
                }
            })
            .collect();
        rank_open(&hyps).iter().map(|r| (r.id.0, r.score)).collect()
    }
}

impl Default for ReflectionAnalytics {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Private helpers.
// ============================================================================

/// Map a journal rule-kind code to the evaluator's [`RuleKind`] (declaration
/// order, 0-based). Unknown codes fold into `Veto` — the most conservative
/// family — rather than being dropped (§49: every haircut is scored).
fn rule_kind_from_code(code: u8) -> RuleKind {
    match code {
        0 => RuleKind::Veto,
        1 => RuleKind::ConfidenceReducer,
        2 => RuleKind::EntryMode,
        3 => RuleKind::EntryZone,
        4 => RuleKind::Setup,
        5 => RuleKind::Social,
        6 => RuleKind::Creator,
        7 => RuleKind::Cluster,
        8 => RuleKind::LateEntryAbort,
        9 => RuleKind::EconomicGate,
        10 => RuleKind::ExitPolicy,
        11 => RuleKind::PartialDeRisk,
        12 => RuleKind::Moonbag,
        _ => RuleKind::Veto,
    }
}

/// A reconciled call's markout proxy in bps: realized net over the reference
/// notional, clamped to `[-10_000, D1_MARKOUT_CAP_BPS]`, with the favorable
/// flag rescuing the sign when integer division rounds the net to zero.
fn call_markout_bps(c: &SocialCall) -> i64 {
    let raw = c
        .realized_net_lamports
        .saturating_mul(10_000)
        .saturating_div(SOCIAL_NOTIONAL_LAMPORTS);
    let mut bps = raw.clamp(-10_000, i128::from(D1_MARKOUT_CAP_BPS)) as i64;
    if bps == 0 {
        bps = if c.realized_favorable {
            D1_NEUTRAL_SIGN_BPS
        } else {
            -D1_NEUTRAL_SIGN_BPS
        };
    }
    bps
}

/// The horizon price implied by a markout of `bps` from [`D1_PRICE_AT_CALL`].
fn proxy_horizon_price(bps: i64) -> u64 {
    let mult = 10_000i128.saturating_add(i128::from(bps)).max(0);
    let px = i128::from(D1_PRICE_AT_CALL).saturating_mul(mult) / 10_000;
    u64::try_from(px).unwrap_or(u64::MAX)
}

/// Calls-per-day × 1000 over the admissible calls' timestamp span (span of
/// zero — a single call or identical stamps — reads as one day's worth).
fn calls_per_day_milli(admissible: &[&SocialCall]) -> u64 {
    let n = admissible.len() as u128;
    if n == 0 {
        return 0;
    }
    let min_ts = admissible.iter().map(|c| c.call_ts_ns).min().unwrap_or(0);
    let max_ts = admissible.iter().map(|c| c.call_ts_ns).max().unwrap_or(0);
    let span = u128::from(max_ts.saturating_sub(min_ts));
    let milli = n
        .saturating_mul(1_000)
        .saturating_mul(DAY_NS)
        .checked_div(span)
        // Span of zero (single call / identical stamps) reads as one day.
        .unwrap_or_else(|| n.saturating_mul(1_000));
    u64::try_from(milli).unwrap_or(u64::MAX)
}

// ============================================================================
// Tests.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_evaluator::social_ledger::SourceId;
    use pump_quant_social::types::SourceState;

    /// Feed one deterministic mixed history into a fresh fold.
    fn seeded() -> ReflectionAnalytics {
        let mut a = ReflectionAnalytics::new();
        for i in 0..80u64 {
            let net: i128 = if i % 3 == 0 {
                -30_000_000
            } else {
                50_000_000 + i as i128 * 1_000
            };
            a.record_trade(
                (i % 4) as usize,
                5,
                net,
                1_000_000_000,
                12_000 + i as i64,
                -1_500,
                0,
            );
        }
        a.record_baseline(-10_000_000);
        a.record_baseline(4_000_000);
        a.record_reject(9, [7u8; 32], 1_000_000, 0, 0);
        a.record_convexity(0, 1, true, -4_000, 0, 500);
        a.record_convexity(10, 2, false, 9_000, 8_000, 12_000);
        a.reflect(PRFS_MARK_DELAY_TICKS, &|_| Some(2_100_000));
        a
    }

    #[test]
    fn determinism_same_feed_same_report() {
        let a = seeded();
        let b = seeded();
        assert_eq!(a.report(), b.report());
        assert_eq!(a.voi_ranking(), b.voi_ranking());
        assert_eq!(
            a.sizing_recommendation(100, 2_000),
            b.sizing_recommendation(100, 2_000)
        );
        assert_eq!(a.retirement_verdicts(), b.retirement_verdicts());
    }

    #[test]
    fn rings_are_bounded_and_evict_oldest() {
        let mut a = ReflectionAnalytics::new();
        for i in 0..(TRADE_RING_CAP + 5) {
            a.record_trade(0, 1, i as i128, 1_000, 0, 0, 0);
        }
        assert_eq!(a.trades.len(), TRADE_RING_CAP);
        // Oldest ids evicted: the front of the ring is trade id 5.
        assert_eq!(a.trades.front().map(|t| t.id), Some(5));
        // All-time counter keeps counting past the ring.
        assert_eq!(a.report().trades, (TRADE_RING_CAP + 5) as u32);

        for i in 0..(REJECT_RING_CAP + 3) {
            a.record_reject(1, [0u8; 32], 1_000, i as u64, 0);
        }
        assert_eq!(a.rejects.len(), REJECT_RING_CAP);

        for _ in 0..(CONVEXITY_RING_CAP + 7) {
            a.record_convexity(0, 1, true, -100, 0, 0);
        }
        assert_eq!(a.convexity.len(), CONVEXITY_RING_CAP);
    }

    #[test]
    fn prfs_reject_is_marked_forward() {
        let mut a = ReflectionAnalytics::new();
        a.record_reject(9, [7u8; 32], 1_000_000, 0, 0);
        // Too young: nothing marked yet.
        a.reflect(PRFS_MARK_DELAY_TICKS - 1, &|_| Some(2_100_000));
        assert!(a.report().prfs_gates.is_empty());
        // Old enough and the mint doubled: the gate ledger must show the
        // upside it foregone.
        a.reflect(PRFS_MARK_DELAY_TICKS, &|_| Some(2_100_000));
        let report = a.report();
        assert_eq!(report.prfs_gates.len(), 1);
        let g = report.prfs_gates[0];
        assert_eq!(g.gate, GateId(9));
        assert_eq!(g.n, 1);
        assert_eq!(g.doubled_within_24h, 1);
        assert_eq!(g.halved_within_24h, 0);
        assert!(g.upside_foregone_bps_sum > 0);
        assert_eq!(g.loss_avoided_bps_sum, 0);
        // The pending mark was retired: reflecting again adds nothing.
        a.reflect(PRFS_MARK_DELAY_TICKS * 2, &|_| Some(2_100_000));
        assert_eq!(a.report().prfs_gates[0].n, 1);
    }

    #[test]
    fn prfs_dark_feed_expires_without_fabrication() {
        let mut a = ReflectionAnalytics::new();
        a.record_reject(3, [9u8; 32], 500_000, 0, 0);
        a.reflect(PRFS_EXPIRE_TICKS, &|_| None);
        assert!(a.rejects.is_empty(), "dark feed pending mark dropped");
        assert!(
            a.report().prfs_gates.is_empty(),
            "no zero-price fabrication"
        );
    }

    #[test]
    fn sizing_recommendation_clamps_and_gates_on_sample() {
        let mut a = ReflectionAnalytics::new();
        for _ in 0..(SIZING_MIN_TRADES - 1) {
            a.record_trade(0, 1, 50_000_000, 1_000_000_000, 0, 0, 0);
        }
        assert_eq!(a.sizing_recommendation(200, 800), None, "below min sample");
        a.record_trade(0, 1, 50_000_000, 1_000_000_000, 0, 0, 0);
        // All-positive returns: the unconstrained optimum is the grid ceiling,
        // so both clamp edges must bind.
        let low = a.sizing_recommendation(200, 800);
        assert_eq!(low, Some(800), "ceiling clamp binds");
        let high = a.sizing_recommendation(3_000, 4_000);
        assert_eq!(high, Some(3_000), "floor clamp binds");
        // Inverted bounds must not panic — they are swapped.
        let swapped = a.sizing_recommendation(800, 200);
        assert_eq!(swapped, Some(800));
    }

    #[test]
    fn retirement_fires_only_on_the_bleeding_lane() {
        let mut a = ReflectionAnalytics::new();
        // Lane 2 bleeds 0.1 SOL a trade for 40 trades: CUSUM deficit crosses
        // 2 SOL after the §56.11 learning horizon. Lane 0 earns.
        for _ in 0..40 {
            a.record_trade(2, 2, -100_000_000, 1_000_000_000, 0, -3_500, 0);
            a.record_trade(0, 4, 50_000_000, 1_000_000_000, 4_000, -500, 0);
        }
        assert_eq!(a.retirement_verdicts(), [false, false, true, false]);
    }

    #[test]
    fn fold_social_quality_classifies_a_favorable_caller() {
        let mut a = ReflectionAnalytics::new();
        let mut ledger = SourceQualityLedger::with_capacity(16);
        let calls: Vec<SocialCall> = (0..12)
            .map(|i| SocialCall {
                source_id: SourceId(7),
                call_ts_ns: 1_000 + i * DAY_NS as u64 / 4,
                feature_ts_ns: 1_000 + i * DAY_NS as u64 / 4,
                realized_net_lamports: 500_000_000,
                realized_favorable: true,
            })
            .collect();
        let folded = a.fold_social_quality(&calls, &[], &mut ledger);
        assert_eq!(folded, 1);
        let cls = ledger.get(7).expect("favorable caller is classified");
        assert_eq!(cls.state, SourceState::FlowAmplifier);
        // The same caller flagged as a coordinated echo fades to CopyEcho.
        let mut echo_ledger = SourceQualityLedger::with_capacity(16);
        a.fold_social_quality(&calls, &[7], &mut echo_ledger);
        let echo = echo_ledger.get(7).expect("echo source still ledgered");
        assert_eq!(echo.state, SourceState::CopyEchoAccount);
    }

    #[test]
    fn voi_ranking_is_total_and_descending() {
        let a = ReflectionAnalytics::new();
        let ranked = a.voi_ranking();
        assert_eq!(ranked.len(), VOI_BACKLOG.len());
        for pair in ranked.windows(2) {
            assert!(pair[0].1 >= pair[1].1, "scores descend");
        }
        let mut ids: Vec<u64> = ranked.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn report_carries_baseline_and_convexity_sides() {
        let a = seeded();
        let r = a.report();
        assert_eq!(r.baseline_trades, 2);
        assert_eq!(r.baseline_net_lamports, -6_000_000);
        assert_eq!(r.trades, 80);
        assert!(r.cvar.is_some());
        assert!(r.profit_factor_bps > 0);
        assert_eq!(r.excisions.len(), EXCISION_KS.len());
        // Both convexity events grouped under their distinct rules, and the
        // suppressed loss-avoider scores positive net convexity.
        assert_eq!(r.convexity_rules.len(), 2);
        let veto = r
            .convexity_rules
            .iter()
            .find(|l| l.rule.kind == RuleKind::Veto)
            .expect("veto rule ledgered");
        assert!(veto.net_convexity_bps() > 0);
        // Honest-UNKNOWN §50: residual equals the windowed live net.
        let windowed: i128 = a.trades.iter().map(|t| t.net_lamports).sum();
        assert_eq!(r.edge_residual_lamports, windowed);
        assert_eq!(r.mfe_capture.n, 80);
    }
}
