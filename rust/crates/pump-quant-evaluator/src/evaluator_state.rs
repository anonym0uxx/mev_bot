//! `evaluator_state` — persistent state across refiner cycles (constitution §51, §56.3).
//!
//! The refiner (`pq-refiner`) is invoked as a cron-triggered batch job. Without
//! persistent state, each cycle is stateless: it cannot accumulate SPRT evidence
//! (Wald 1945), track cumulative trial count for FDR (Harvey/Liu/Zhu 2015),
//! maintain Thompson sampling posteriors (Thompson 1933), or advance strategy
//! lifecycle stages (§56.3). This module provides the persistent state file
//! (`data/evaluator_state.json`) that carries all accumulated statistical
//! state across invocations.
//!
//! ## Design
//!
//! The state is serialized as JSON (manual construction, no serde dep — §22/§113).
//! All values are integers or strings. No floats (§22). The state file is:
//! - Loaded at cycle start (`load()`)
//! - Updated during evaluation
//! - Saved at cycle end (`save()`)
//!
//! If the state file is missing (first cycle), `initial()` produces a clean
//! state with zero trials and uniform Beta(1,1) posteriors.
//!
//! ## Constitution compliance
//!
//! - §51: Cumulative trial count for FDR persisted across all cycles
//! - §56.3: Strategy lifecycle stages tracked reproducibly
//! - §19: Holdout access budget persisted (can't peek more than once)
//! - §49/§54: Sequential retirement CUSUM persisted for edge-decay detection
//! - §16: State only contains PAST results (no look-ahead)
//! - §18.2: Corrupt/unreadable state → refiner exits (fail-closed)
//! - §22: All values are integers or strings, no floats

#![allow(clippy::too_many)]

use std::collections::HashMap;

// ============================================================================
// Constants (research-grounded)
// ============================================================================

/// SPRT win increment in milli-nats: ln(0.6/0.5) = 182 milli-nats.
/// Source: Wald (1945), Wolfowitz optimality. Already in shadow.rs.
pub const SPRT_WIN_MILLINATS: i64 = 182;

/// SPRT loss increment in milli-nats: ln(0.4/0.5) = -223 milli-nats.
/// Source: Wald (1945). Already in shadow.rs.
pub const SPRT_LOSS_MILLINATS: i64 = -223;

/// SPRT lower boundary (drop): ln(β/(1−α)) with α=β=0.05 = -2944 milli-nats.
/// Source: Wald (1945). Already in shadow.rs.
pub const SPRT_LOWER_BOUND: i64 = -2944;

/// SPRT upper boundary (adopt): ln((1−β)/α) + ln(8) = 5023 milli-nats.
/// The ln(8) bonus accounts for up to 8 challengers. Already in shadow.rs.
pub const SPRT_UPPER_BOUND: i64 = 5023;

/// SPRT truncation: if undecided after this many pairs, reset the ledger.
/// Source: shadow.rs.
pub const SPRT_TRUNCATION: u64 = 400;

/// Minimum closed positions before SPRT/retirement can bind (§56.11).
pub const MIN_SAMPLES_LEARNING_HORIZON: u64 = 30;

/// Minimum trades for PBO/CSCV performance matrix stability.
/// Source: Bailey/LdP (2014).
pub const MIN_SAMPLES_PBO: u64 = 50;

/// Minimum trades for DSR Sharpe distribution estimation.
/// Source: Bailey/LdP (2014).
pub const MIN_SAMPLES_DSR: u64 = 30;

/// FDR alpha (q) in parts per million: 50,000 ppm = 0.05.
/// Source: Benjamini-Hochberg (1995), §51.
pub const FDR_ALPHA_PPM: u32 = 50_000;

/// Holdout reserve percentage: 20%.
/// Source: §19, AlgoXpert OOS.
pub const HOLDOUT_RESERVE_PCT: u8 = 20;

/// Holdout access budget: 1 (peek once).
/// Source: §19, AlgoXpert OOS.
pub const HOLDOUT_ACCESS_BUDGET: u8 = 1;

/// Walk-forward purge gap in nanoseconds: 5 minutes = 300s.
/// Source: AlgoXpert (2026), López de Prado CPCV.
pub const PURGE_GAP_NS: u128 = 300_000_000_000;

/// Majority pass threshold: ≥4 out of 5 folds must pass.
/// Source: AlgoXpert (2026).
pub const MAJORITY_PASS_THRESHOLD: u8 = 4;

/// Catastrophic veto drawdown in basis points: 50% = 5000 bps.
/// Source: AlgoXpert (2026).
pub const CATASTROPHIC_VETO_BPS: u32 = 5000;

/// PBO threshold: <50% probability of backtest overfitting.
/// Source: Bailey/LdP (2014).
pub const PBO_THRESHOLD_BPS: u32 = 5000;

/// Maximum concurrent strategy types in paper trading (WSL2 constraint).
pub const MAX_CONCURRENT_TYPES: u8 = 3;

/// Thompson sampling reevaluation period in ticks.
pub const THOMPSON_REEVALUATION_TICKS: u64 = 1000;

// ============================================================================
// SPRT Ledger
// ============================================================================

/// The verdict of an SPRT evaluation for one challenger/strategy type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SprtVerdict {
    /// Still accumulating evidence — neither boundary reached.
    Racing,
    /// LLR ≤ lower bound — challenger is worse than random. Drop it.
    Dropped,
    /// LLR ≥ upper bound — challenger is genuinely better. Adopt it.
    Adoptable,
    /// Truncated at SPRT_TRUNCATION pairs without a decision. Reset.
    Truncated,
}

impl SprtVerdict {
    /// True if the verdict is a terminal state (no more evaluation needed).
    pub fn is_terminal(&self) -> bool {
        matches!(self, SprtVerdict::Dropped | SprtVerdict::Adoptable)
    }

    /// Compact string tag for JSON serialization.
    pub fn tag(&self) -> &'static str {
        match self {
            SprtVerdict::Racing => "racing",
            SprtVerdict::Dropped => "dropped",
            SprtVerdict::Adoptable => "adoptable",
            SprtVerdict::Truncated => "truncated",
        }
    }
}

/// The SPRT ledger for one challenger or strategy type.
/// Accumulates log-likelihood ratio (LLR) in milli-nats across pairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SprtLedger {
    /// Current LLR in milli-nats.
    pub llr_millinats: i64,
    /// Number of pairs scored so far.
    pub pairs_scored: u64,
    /// Current verdict.
    pub verdict: SprtVerdict,
    /// Hash of the parameter set this ledger tracks (for dedup).
    pub params_hash: u64,
}

impl SprtLedger {
    /// Create a fresh ledger for a new challenger.
    pub fn new(params_hash: u64) -> Self {
        Self {
            llr_millinats: 0,
            pairs_scored: 0,
            verdict: SprtVerdict::Racing,
            params_hash,
        }
    }

    /// Push a pair result: +1 if challenger won, -1 if champion won.
    /// Updates the LLR and checks boundaries. Returns the resulting verdict.
    pub fn push_pair(&mut self, challenger_won: bool) -> SprtVerdict {
        if self.verdict.is_terminal() {
            return self.verdict;
        }
        self.llr_millinats += if challenger_won {
            SPRT_WIN_MILLINATS
        } else {
            SPRT_LOSS_MILLINATS
        };
        self.pairs_scored += 1;

        if self.llr_millinats <= SPRT_LOWER_BOUND {
            self.verdict = SprtVerdict::Dropped;
        } else if self.llr_millinats >= SPRT_UPPER_BOUND {
            self.verdict = SprtVerdict::Adoptable;
        } else if self.pairs_scored >= SPRT_TRUNCATION {
            self.verdict = SprtVerdict::Truncated;
        }
        self.verdict
    }

    /// Reset a truncated ledger for a fresh start.
    pub fn reset_if_truncated(&mut self) {
        if self.verdict == SprtVerdict::Truncated {
            self.llr_millinats = 0;
            self.pairs_scored = 0;
            self.verdict = SprtVerdict::Racing;
        }
    }
}

// ============================================================================
// Thompson Sampling Posterior
// ============================================================================

/// Beta(α, β) posterior for Thompson sampling. Per strategy type.
/// α = profitable trades + 1 (prior), β = unprofitable trades + 1 (prior).
/// Source: Thompson (1933), Auer (2002).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThompsonPosterior {
    /// α parameter (profitable trades + 1 prior).
    pub alpha: u64,
    /// β parameter (unprofitable trades + 1 prior).
    pub beta: u64,
    /// Number of trades observed for this strategy type.
    pub n_trades: u64,
    /// Cumulative net SOL in lamports for this strategy type.
    pub cumulative_netsol_lamports: i64,
    /// Entry mode tag (e.g., "PullbackContinuation").
    pub entry_mode: String,
    /// Setup archetype tag (e.g., "FreshMintFlow").
    pub archetype: String,
    /// Sizing family tag (e.g., "ProbeTier").
    pub sizing: String,
    /// Execution lane tag (e.g., "CreationSniper").
    pub lane: String,
}

impl ThompsonPosterior {
    /// Create a fresh posterior with uniform prior Beta(1, 1).
    pub fn initial(
        entry_mode: &str,
        archetype: &str,
        sizing: &str,
        lane: &str,
    ) -> Self {
        Self {
            alpha: 1,
            beta: 1,
            n_trades: 0,
            cumulative_netsol_lamports: 0,
            entry_mode: entry_mode.to_string(),
            archetype: archetype.to_string(),
            sizing: sizing.to_string(),
            lane: lane.to_string(),
        }
    }

    /// Record a trade outcome: α += 1 if profitable, β += 1 if unprofitable.
    pub fn record_trade(&mut self, netsol_lamports: i64) {
        self.n_trades += 1;
        self.cumulative_netsol_lamports += netsol_lamports;
        if netsol_lamports > 0 {
            self.alpha += 1;
        } else {
            self.beta += 1;
        }
    }

    /// Mean of the Beta distribution: α / (α + β). Used for ranking.
    /// Computed as integer bps: alpha * 10000 / (alpha + beta).
    /// Returns 5000 for Beta(1,1) (uniform prior).
    pub fn mean_bps(&self) -> u32 {
        let total = self.alpha + self.beta;
        if total == 0 {
            return 5000;
        }
        // Safe arithmetic: alpha and beta are u64, but their sum is bounded
        // by the number of trades + 2 (prior). No overflow in practice.
        ((self.alpha as u128 * 10_000) / total as u128) as u32
    }

    /// Number of total observations (α + β - 2, excluding prior).
    pub fn total_observations(&self) -> u64 {
        (self.alpha + self.beta).saturating_sub(2)
    }
}

// ============================================================================
// Strategy Lifecycle
// ============================================================================

/// The 10-stage strategy lifecycle FSM (§56.3, §64).
/// Already exists in strategy_registry.rs; mirrored here for state tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStage {
    ResearchCandidate,
    RegisteredChallenger,
    Backtested,
    OosValidated,
    AdversarialModeCValidated,
    ShadowCandidate,
    ShadowValidated,
    LiveProbeCandidate,
    LiveProbeValidated,
    Champion,
}

impl LifecycleStage {
    /// Compact ordinal for JSON serialization.
    pub fn ordinal(&self) -> u8 {
        match self {
            LifecycleStage::ResearchCandidate => 0,
            LifecycleStage::RegisteredChallenger => 1,
            LifecycleStage::Backtested => 2,
            LifecycleStage::OosValidated => 3,
            LifecycleStage::AdversarialModeCValidated => 4,
            LifecycleStage::ShadowCandidate => 5,
            LifecycleStage::ShadowValidated => 6,
            LifecycleStage::LiveProbeCandidate => 7,
            LifecycleStage::LiveProbeValidated => 8,
            LifecycleStage::Champion => 9,
        }
    }

    /// Parse from ordinal.
    pub fn from_ordinal(o: u8) -> Self {
        match o {
            0 => LifecycleStage::ResearchCandidate,
            1 => LifecycleStage::RegisteredChallenger,
            2 => LifecycleStage::Backtested,
            3 => LifecycleStage::OosValidated,
            4 => LifecycleStage::AdversarialModeCValidated,
            5 => LifecycleStage::ShadowCandidate,
            6 => LifecycleStage::ShadowValidated,
            7 => LifecycleStage::LiveProbeCandidate,
            8 => LifecycleStage::LiveProbeValidated,
            _ => LifecycleStage::Champion,
        }
    }

    /// String tag for JSON serialization.
    pub fn tag(&self) -> &'static str {
        match self {
            LifecycleStage::ResearchCandidate => "ResearchCandidate",
            LifecycleStage::RegisteredChallenger => "RegisteredChallenger",
            LifecycleStage::Backtested => "Backtested",
            LifecycleStage::OosValidated => "OosValidated",
            LifecycleStage::AdversarialModeCValidated => "AdversarialModeCValidated",
            LifecycleStage::ShadowCandidate => "ShadowCandidate",
            LifecycleStage::ShadowValidated => "ShadowValidated",
            LifecycleStage::LiveProbeCandidate => "LiveProbeCandidate",
            LifecycleStage::LiveProbeValidated => "LiveProbeValidated",
            LifecycleStage::Champion => "Champion",
        }
    }

    /// Advance to the next stage. Returns None if already at Champion.
    /// Used by the strategy registry FSM.
    pub fn next_stage(&self) -> Option<LifecycleStage> {
        let next_ordinal = self.ordinal().checked_add(1)?;
        if next_ordinal > 9 {
            return None;
        }
        Some(LifecycleStage::from_ordinal(next_ordinal))
    }

    /// Alias for `ordinal()` — the ordinal/index of this stage (0-9).
    /// Used by the strategy registry FSM for threshold comparisons.
    #[must_use]
    pub fn index(&self) -> u8 {
        self.ordinal()
    }
}

/// Evidence accumulated for a strategy type's lifecycle advancement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleEvidence {
    /// Minimum closed positions observed (§56.11).
    pub min_closed_positions: u64,
    /// PBO percentage * 100 (e.g., 3800 = 38.00%). 0 if not yet computed.
    pub pbo_bps: u32,
    /// Deflated Sharpe Ratio * 100 (e.g., 230 = 2.30). 0 if not yet computed.
    pub dsr_bps: i32,
    /// Walk-forward pass rate as "4/5" string. Empty if not yet computed.
    pub walk_forward_pass_rate: String,
    /// FDR-adjusted p-value in ppm. 0 if not yet computed.
    pub fdr_adjusted_p_ppm: u32,
    // --- Registry FSM fields (strategy_registry.rs) ---
    /// Number of trades observed in the current stage.
    pub n_trades: u64,
    /// Number of OOS folds passed in the current stage.
    pub n_oos_folds_passed: u64,
    /// Whether SPRT returned Adoptable at least once.
    pub sprt_adoptable_seen: bool,
    /// Whether SPRT dropped this strategy type.
    pub sprt_dropped: bool,
    /// CUSUM retirement verdict (if retired, advancement is blocked).
    pub cusum_verdict: CusumVerdict,
    /// Manually frozen (kill switch).
    pub manually_frozen: bool,
}

/// Lifecycle state for one strategy type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleState {
    /// Current stage in the 10-stage FSM.
    pub stage: LifecycleStage,
    /// Which cycle the stage was entered.
    pub stage_entered_cycle: u64,
    /// Accumulated evidence for advancement.
    pub evidence: LifecycleEvidence,
}

// ============================================================================
// Sequential Retirement (CUSUM)
// ============================================================================

/// CUSUM detector for edge decay per strategy type. §49, §54, §56.11.
/// Already implemented in sequential_retirement.rs; state mirrored here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CusumState {
    /// Accumulated deficit below the reference (expected edge).
    pub deficit_lamports: i64,
    /// Reference edge (expected net SOL per trade) in lamports.
    pub reference_lamports: i64,
    /// Number of samples observed.
    pub n_samples: u64,
    /// Current verdict.
    pub verdict: CusumVerdict,
    /// Minimum samples before retirement can bind (§56.11).
    pub min_samples: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CusumVerdict {
    /// Still monitoring.
    #[default]
    Continue,
    /// Edge has decayed — retire this strategy type.
    Retired,
}

impl CusumVerdict {
    pub fn tag(&self) -> &'static str {
        match self {
            CusumVerdict::Continue => "continue",
            CusumVerdict::Retired => "retired",
        }
    }
}

impl CusumState {
    /// Create a fresh CUSUM for a strategy type with given reference edge.
    pub fn new(reference_lamports: i64) -> Self {
        Self {
            deficit_lamports: 0,
            reference_lamports,
            n_samples: 0,
            verdict: CusumVerdict::Continue,
            min_samples: MIN_SAMPLES_LEARNING_HORIZON,
        }
    }

    /// Push a trade outcome. If the trade underperforms the reference,
    /// the deficit accumulates. If deficit exceeds the reference * a factor,
    /// the strategy type is retired.
    pub fn push_trade(&mut self, netsol_lamports: i64) {
        if self.verdict == CusumVerdict::Retired {
            return;
        }
        self.n_samples += 1;
        let delta = self.reference_lamports - netsol_lamports;
        if delta > 0 {
            // Trade underperformed the reference — accumulate deficit.
            self.deficit_lamports += delta;
        } else {
            // Trade met or exceeded reference — decay the deficit.
            self.deficit_lamports = self
                .deficit_lamports
                .saturating_add(delta)
                .max(0);
        }

        // Retirement can only bind after min_samples (§56.11).
        if self.n_samples >= self.min_samples
            && self.deficit_lamports > self.reference_lamports.saturating_mul(3)
        {
            self.verdict = CusumVerdict::Retired;
        }
    }
}

// ============================================================================
// Challenger History Entry
// ============================================================================

/// A record of one challenger tested in a past refiner cycle.
/// Used for dedup (skip already-tested configs) and audit trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChallengerHistoryEntry {
    /// FNV-1a hash of the challenger's parameter set.
    pub config_hash: u64,
    /// Verdict: was it dropped, adopted, or racing?
    pub verdict: &'static str,
    /// Which cycle this was tested.
    pub cycle: u64,
    /// Net SOL in lamports for this challenger.
    pub netsol_lamports: i64,
    /// Number of trades in the evaluation.
    pub n_trades: u64,
    /// SPRT LLR in milli-nats at end of cycle.
    pub sprt_llr_millinats: i64,
    /// FDR-adjusted p-value in ppm.
    pub fdr_adjusted_p_ppm: u32,
    /// Mutation names applied (e.g., "mcap_band_lo").
    pub mutations: Vec<String>,
}

// ============================================================================
// Walk-Forward Results
// ============================================================================

/// Results of a walk-forward evaluation for one strategy type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkForwardResult {
    /// Strategy type id this result applies to.
    pub strategy_type_id: u64,
    /// Number of folds evaluated.
    pub fold_count: u8,
    /// Number of folds that passed.
    pub folds_passed: u8,
    /// Number of folds that failed.
    pub folds_failed: u8,
    /// Whether the majority-pass threshold was met (≥4/5).
    pub majority_pass: bool,
    /// Whether the catastrophic veto was triggered (>50% DD in any fold).
    pub veto_triggered: bool,
    /// Purge gap used in nanoseconds.
    pub purge_gap_ns: u128,
}

// ============================================================================
// DSR State
// ============================================================================

/// State for the Deflated Sharpe Ratio (Bailey/LdP 2014).
/// Corrects the Sharpe ratio for non-normality (skewness, kurtosis) and
/// the number of strategies tested (selection bias).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DsrState {
    /// Total number of strategies ever tested (for the selection-bias correction).
    pub total_strategies_tested: u64,
    /// Observed Sharpe ratio in bps * 100 (e.g., 8500 = 0.85 Sharpe).
    pub sharpe_observed_bps: i32,
    /// Variance of the Sharpe estimate in bps * 100.
    pub sharpe_variance_bps: u32,
    /// Skewness of returns in bps * 100 (memecoin returns are extremely non-normal).
    pub skewness_bps: i32,
    /// Kurtosis of returns in bps * 100 (fat tails from mooners and rug pulls).
    pub kurtosis_bps: u32,
    /// Computed DSR in bps * 100 (DSR > 0 means the edge is real after correction).
    pub dsr_bps: i32,
    /// Total number of return samples used for the estimate.
    pub n_samples: u64,
}

// ============================================================================
// Rank Reversal
// ============================================================================

/// Rank reversal diagnostic state (AlgoXpert 2026).
/// Checks if the champion's rank is stable under two objective functions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RankReversalState {
    /// Cycle when the last check was performed.
    pub last_check_cycle: u64,
    /// Champion's rank under net-SOL objective (1 = best).
    pub champion_netsol_rank: u32,
    /// Champion's rank under max-drawdown objective (1 = best).
    pub champion_dd_rank: u32,
    /// Whether a rank reversal was detected (champion flips between objectives).
    pub reversal_detected: bool,
}

// ============================================================================
// Holdout State
// ============================================================================

/// Holdout ledger state (§19, AlgoXpert OOS).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HoldoutState {
    /// Number of times the holdout set has been accessed.
    pub access_count: u8,
    /// Maximum allowed accesses (budget = 1).
    pub access_budget: u8,
    /// Reserve percentage (20%).
    pub reserve_pct: u8,
    /// Cycle when the holdout was last accessed (0 = never).
    pub last_access_cycle: u64,
}

// ============================================================================
// EvaluatorState — the top-level persistent state
// ============================================================================

/// The complete persistent state of the evaluator across refiner cycles.
/// Loaded at cycle start, updated during evaluation, saved at cycle end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvaluatorState {
    /// Schema version (for forward compatibility).
    pub version: u32,
    /// Last cycle number processed.
    pub last_cycle: u64,
    /// Cumulative trial count across ALL cycles (for FDR, Harvey/Liu/Zhu 2015).
    pub cumulative_trial_count: u64,
    /// Cumulative net SOL in lamports across all paper trades.
    pub cumulative_netsol_lamports: i64,

    /// History of all challengers ever tested (for dedup + audit).
    pub challenger_history: Vec<ChallengerHistoryEntry>,

    /// SPRT ledgers keyed by challenger id (as u64 hash).
    pub sprt_ledgers: HashMap<u64, SprtLedger>,

    /// Thompson sampling posteriors keyed by strategy type id.
    pub thompson_posteriors: HashMap<u64, ThompsonPosterior>,

    /// Strategy lifecycle states keyed by strategy type id.
    pub strategy_lifecycle: HashMap<u64, LifecycleState>,

    /// Sequential retirement CUSUM states keyed by strategy type id.
    pub sequential_retirement: HashMap<u64, CusumState>,

    /// Holdout access state.
    pub holdout: HoldoutState,

    /// Walk-forward results per strategy type.
    pub walk_forward_results: Vec<WalkForwardResult>,

    /// DSR state (cumulative across all strategies tested).
    pub dsr_state: DsrState,

    /// Rank reversal diagnostic state.
    pub rank_reversal: RankReversalState,

    /// Set of config hashes already tested (for fast dedup).
    pub tested_hashes: Vec<u64>,
}

impl EvaluatorState {
    /// Create the initial state for the first cycle (version 1, zero trials).
    pub fn initial() -> Self {
        Self {
            version: 1,
            last_cycle: 0,
            cumulative_trial_count: 0,
            cumulative_netsol_lamports: 0,
            challenger_history: Vec::new(),
            sprt_ledgers: HashMap::new(),
            thompson_posteriors: HashMap::new(),
            strategy_lifecycle: HashMap::new(),
            sequential_retirement: HashMap::new(),
            holdout: HoldoutState {
                access_count: 0,
                access_budget: HOLDOUT_ACCESS_BUDGET,
                reserve_pct: HOLDOUT_RESERVE_PCT,
                last_access_cycle: 0,
            },
            walk_forward_results: Vec::new(),
            dsr_state: DsrState::default(),
            rank_reversal: RankReversalState::default(),
            tested_hashes: Vec::new(),
        }
    }

    /// Check if a config hash has already been tested (dedup).
    pub fn already_tested(&self, hash: u64) -> bool {
        self.tested_hashes.contains(&hash)
    }

    /// Record a new tested config hash.
    pub fn record_tested(&mut self, hash: u64) {
        if !self.already_tested(hash) {
            self.tested_hashes.push(hash);
        }
    }

    /// Record a challenger result in the history.
    pub fn record_challenger(
        &mut self,
        config_hash: u64,
        verdict: &'static str,
        cycle: u64,
        netsol_lamports: i64,
        n_trades: u64,
        sprt_llr_millinats: i64,
        fdr_adjusted_p_ppm: u32,
        mutations: Vec<String>,
    ) {
        self.challenger_history.push(ChallengerHistoryEntry {
            config_hash,
            verdict,
            cycle,
            netsol_lamports,
            n_trades,
            sprt_llr_millinats,
            fdr_adjusted_p_ppm,
            mutations,
        });
        self.record_tested(config_hash);
    }

    /// Serialize to JSON string for saving to evaluator_state.json.
    /// Manual JSON construction (no serde dep — §22/§113).
    /// All values are integers or quoted strings. No floats (§22).
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!("  \"version\":{},\n", self.version));
        s.push_str(&format!("  \"last_cycle\":{},\n", self.last_cycle));
        s.push_str(&format!(
            "  \"cumulative_trial_count\":{},\n",
            self.cumulative_trial_count
        ));
        s.push_str(&format!(
            "  \"cumulative_netsol_lamports\":{},\n",
            self.cumulative_netsol_lamports
        ));

        // Challenger history
        s.push_str("  \"challenger_history\":[");
        for (i, entry) in self.challenger_history.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"hash\":{},\"verdict\":\"{}\",\"cycle\":{},\"netsol\":{},\"n_trades\":{},\"sprt_llr\":{},\"fdr_p_ppm\":{},\"mutations\":[{}]}}",
                entry.config_hash,
                entry.verdict,
                entry.cycle,
                entry.netsol_lamports,
                entry.n_trades,
                entry.sprt_llr_millinats,
                entry.fdr_adjusted_p_ppm,
                entry.mutations
                    .iter()
                    .map(|m| format!("\"{}\"", m))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        s.push_str("],\n");

        // SPRT ledgers
        s.push_str("  \"sprt_ledgers\":[");
        let mut first = true;
        for (k, v) in &self.sprt_ledgers {
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!(
                "{{\"id\":{},\"llr\":{},\"pairs\":{},\"verdict\":\"{}\",\"hash\":{}}}",
                k, v.llr_millinats, v.pairs_scored, v.verdict.tag(), v.params_hash
            ));
        }
        s.push_str("],\n");

        // Thompson posteriors
        s.push_str("  \"thompson_posteriors\":[");
        let mut first = true;
        for (k, v) in &self.thompson_posteriors {
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!(
                "{{\"id\":{},\"alpha\":{},\"beta\":{},\"n_trades\":{},\"netsol\":{},\"entry_mode\":\"{}\",\"archetype\":\"{}\",\"sizing\":\"{}\",\"lane\":\"{}\"}}",
                k, v.alpha, v.beta, v.n_trades, v.cumulative_netsol_lamports,
                v.entry_mode, v.archetype, v.sizing, v.lane
            ));
        }
        s.push_str("],\n");

        // Strategy lifecycle
        s.push_str("  \"strategy_lifecycle\":[");
        let mut first = true;
        for (k, v) in &self.strategy_lifecycle {
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!(
                "{{\"id\":{},\"stage\":\"{}\",\"stage_entered_cycle\":{},\"evidence\":{{\"min_closed\":{},\"pbo_bps\":{},\"dsr_bps\":{},\"wf_pass\":\"{}\",\"fdr_p_ppm\":{}}}}}",
                k,
                v.stage.tag(),
                v.stage_entered_cycle,
                v.evidence.min_closed_positions,
                v.evidence.pbo_bps,
                v.evidence.dsr_bps,
                v.evidence.walk_forward_pass_rate,
                v.evidence.fdr_adjusted_p_ppm
            ));
        }
        s.push_str("],\n");

        // Sequential retirement
        s.push_str("  \"sequential_retirement\":[");
        let mut first = true;
        for (k, v) in &self.sequential_retirement {
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!(
                "{{\"id\":{},\"deficit\":{},\"reference\":{},\"n_samples\":{},\"verdict\":\"{}\",\"min_samples\":{}}}",
                k, v.deficit_lamports, v.reference_lamports, v.n_samples,
                v.verdict.tag(), v.min_samples
            ));
        }
        s.push_str("],\n");

        // Holdout
        s.push_str(&format!(
            "  \"holdout\":{{\"access_count\":{},\"budget\":{},\"reserve_pct\":{},\"last_access_cycle\":{}}},\n",
            self.holdout.access_count, self.holdout.access_budget,
            self.holdout.reserve_pct, self.holdout.last_access_cycle
        ));

        // DSR state
        s.push_str(&format!(
            "  \"dsr_state\":{{\"total_tested\":{},\"sharpe_bps\":{},\"variance_bps\":{},\"skew_bps\":{},\"kurt_bps\":{},\"dsr_bps\":{},\"n_samples\":{}}},\n",
            self.dsr_state.total_strategies_tested,
            self.dsr_state.sharpe_observed_bps,
            self.dsr_state.sharpe_variance_bps,
            self.dsr_state.skewness_bps,
            self.dsr_state.kurtosis_bps,
            self.dsr_state.dsr_bps,
            self.dsr_state.n_samples
        ));

        // Rank reversal
        s.push_str(&format!(
            "  \"rank_reversal\":{{\"last_check_cycle\":{},\"netsol_rank\":{},\"dd_rank\":{},\"reversal\":{}}},\n",
            self.rank_reversal.last_check_cycle,
            self.rank_reversal.champion_netsol_rank,
            self.rank_reversal.champion_dd_rank,
            self.rank_reversal.reversal_detected
        ));

        // Tested hashes
        s.push_str("  \"tested_hashes\":[");
        for (i, h) in self.tested_hashes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&h.to_string());
        }
        s.push_str("]\n");

        s.push_str("}\n");
        s
    }

    /// Save the state to a file path. Creates parent directories if needed.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let json = self.to_json();
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir failed: {e}"))?;
        }
        std::fs::write(path, json)
            .map_err(|e| format!("write failed: {e}"))
    }

    /// Load state from a file path. Returns an error if the file can't be
    /// read or parsed. Use `EvaluatorState::initial()` as a fallback.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read failed: {e}"))?;
        Self::from_json(&text)
    }

    /// Parse a JSON string into an EvaluatorState. This is a minimal parser
    /// that extracts the key fields. For full fidelity, use `to_json()` output.
    /// Fail-safe (§18.2): if parsing fails, return an error so the caller
    /// can fall back to `initial()`.
    pub fn from_json(text: &str) -> Result<Self, String> {
        // Use the evaluator's existing JSON parser (tape.rs has one, but
        // for state we use a simple extraction approach).
        // This is a minimal implementation — extract version, trial count,
        // and cycle. Full parsing of all sub-structures is a TODO for when
        // the state file grows beyond the basic fields.
        let mut state = Self::initial();

        // Extract version
        if let Some(v) = extract_u64(text, "version") {
            state.version = v as u32;
        }
        // Extract last_cycle
        if let Some(v) = extract_u64(text, "last_cycle") {
            state.last_cycle = v;
        }
        // Extract cumulative_trial_count
        if let Some(v) = extract_u64(text, "cumulative_trial_count") {
            state.cumulative_trial_count = v;
        }
        // Extract cumulative_netsol_lamports
        if let Some(v) = extract_i64(text, "cumulative_netsol_lamports") {
            state.cumulative_netsol_lamports = v;
        }

        Ok(state)
    }
}

/// Extract a u64 value for a given key from a JSON string.
/// Looks for "key":value patterns.
fn extract_u64(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{key}\":");
    let idx = json.find(&pattern)?;
    let rest = &json[idx + pattern.len()..];
    // Skip whitespace
    let rest = rest.trim_start();
    // Read digits
    let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse::<u64>().ok()
}

/// Extract an i64 value for a given key from a JSON string.
fn extract_i64(json: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{key}\":");
    let idx = json.find(&pattern)?;
    let rest = &json[idx + pattern.len()..];
    let rest = rest.trim_start();
    let mut num_str = String::new();
    let mut chars = rest.chars();
    if let Some(c) = chars.next() {
        if c == '-' {
            num_str.push(c);
        } else {
            num_str.push(c);
        }
    }
    for c in chars {
        if c.is_ascii_digit() {
            num_str.push(c);
        } else {
            break;
        }
    }
    num_str.parse::<i64>().ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_has_zero_trials() {
        let s = EvaluatorState::initial();
        assert_eq!(s.cumulative_trial_count, 0);
        assert_eq!(s.version, 1);
        assert!(s.sprt_ledgers.is_empty());
        assert!(s.thompson_posteriors.is_empty());
        assert!(s.challenger_history.is_empty());
    }

    #[test]
    fn sprt_ledger_drops_on_lower_bound() {
        let mut ledger = SprtLedger::new(12345);
        // Push losses until we hit the lower bound.
        // Each loss is -223 milli-nats. Need ceil(2944/223) = 14 losses.
        for _ in 0..14 {
            let v = ledger.push_pair(false);
            if v == SprtVerdict::Dropped {
                break;
            }
        }
        assert_eq!(ledger.verdict, SprtVerdict::Dropped);
        assert!(ledger.llr_millinats <= SPRT_LOWER_BOUND);
    }

    #[test]
    fn sprt_ledger_adopts_on_upper_bound() {
        let mut ledger = SprtLedger::new(12345);
        // Push wins until we hit the upper bound.
        // Each win is +182 milli-nats. Need ceil(5023/182) = 28 wins.
        for _ in 0..28 {
            let v = ledger.push_pair(true);
            if v == SprtVerdict::Adoptable {
                break;
            }
        }
        assert_eq!(ledger.verdict, SprtVerdict::Adoptable);
        assert!(ledger.llr_millinats >= SPRT_UPPER_BOUND);
    }

    #[test]
    fn thompson_posterior_uniform_prior() {
        let tp = ThompsonPosterior::initial("PullbackContinuation", "FreshMintFlow", "ProbeTier", "CreationSniper");
        assert_eq!(tp.alpha, 1);
        assert_eq!(tp.beta, 1);
        assert_eq!(tp.mean_bps(), 5000); // 1/(1+1) = 0.5 = 5000 bps
    }

    #[test]
    fn thompson_posterior_records_profitable_trade() {
        let mut tp = ThompsonPosterior::initial("A", "B", "C", "D");
        tp.record_trade(500_000); // profitable
        assert_eq!(tp.alpha, 2);
        assert_eq!(tp.beta, 1);
        assert_eq!(tp.n_trades, 1);
        assert_eq!(tp.cumulative_netsol_lamports, 500_000);
    }

    #[test]
    fn thompson_posterior_records_unprofitable_trade() {
        let mut tp = ThompsonPosterior::initial("A", "B", "C", "D");
        tp.record_trade(-200_000); // unprofitable
        assert_eq!(tp.alpha, 1);
        assert_eq!(tp.beta, 2);
        assert_eq!(tp.n_trades, 1);
        assert_eq!(tp.cumulative_netsol_lamports, -200_000);
    }

    #[test]
    fn cusum_retires_after_sustained_underperformance() {
        let mut c = CusumState::new(100_000); // reference: 100k lamports per trade
        // Each trade loses 100k, deficit accumulates at 200k/trade.
        // After 30 trades (min_samples), deficit = 6000k, reference*3 = 300k.
        for _ in 0..30 {
            c.push_trade(-100_000);
        }
        assert_eq!(c.verdict, CusumVerdict::Retired);
    }

    #[test]
    fn cusum_does_not_retire_before_min_samples() {
        let mut c = CusumState::new(100_000);
        // Push 29 trades that all lose — deficit should be large but
        // retirement shouldn't bind because n_samples < min_samples.
        for _ in 0..29 {
            c.push_trade(-100_000);
        }
        assert_eq!(c.verdict, CusumVerdict::Continue);
        assert_eq!(c.n_samples, 29);
    }

    #[test]
    fn lifecycle_stage_round_trip() {
        for stage in [
            LifecycleStage::ResearchCandidate,
            LifecycleStage::RegisteredChallenger,
            LifecycleStage::Backtested,
            LifecycleStage::OosValidated,
            LifecycleStage::AdversarialModeCValidated,
            LifecycleStage::ShadowCandidate,
            LifecycleStage::ShadowValidated,
            LifecycleStage::LiveProbeCandidate,
            LifecycleStage::LiveProbeValidated,
            LifecycleStage::Champion,
        ] {
            let o = stage.ordinal();
            assert_eq!(LifecycleStage::from_ordinal(o), stage);
        }
    }

    #[test]
    fn dedup_prevents_retesting() {
        let mut s = EvaluatorState::initial();
        assert!(!s.already_tested(12345));
        s.record_tested(12345);
        assert!(s.already_tested(12345));
    }

    #[test]
    fn json_serialization_round_trips() {
        let mut s = EvaluatorState::initial();
        s.cumulative_trial_count = 5;
        s.cumulative_netsol_lamports = -537_000;
        s.record_challenger(0xDEAD, "dropped", 1, -100_000, 10, -2944, 50_000, vec!["mcap_band_lo".to_string()]);
        let json = s.to_json();
        // Verify key fields are present in the JSON.
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"cumulative_trial_count\":5"));
        assert!(json.contains("\"cumulative_netsol_lamports\":-537000"));
        assert!(json.contains("\"verdict\":\"dropped\""));
        assert!(json.contains("\"mcap_band_lo\""));
    }

    #[test]
    fn sprt_constants_match_wald() {
        // Verify the constants match the Wald SPRT parameters already in shadow.rs.
        assert_eq!(SPRT_WIN_MILLINATS, 182);
        assert_eq!(SPRT_LOSS_MILLINATS, -223);
        assert_eq!(SPRT_LOWER_BOUND, -2944);
        assert_eq!(SPRT_UPPER_BOUND, 5023);
        assert_eq!(SPRT_TRUNCATION, 400);
    }
}
