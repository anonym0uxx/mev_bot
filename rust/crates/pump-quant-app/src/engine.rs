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

use crate::config::Config;
use crate::event::AppEvent;
use crate::gate::{decide, Confirmation, GateDecision, GateReject};
use crate::journal_log::{Decision, DecisionJournal};
use crate::lane::{NarrativeLane, NumericLane, SocialLane, WalletLane};
use crate::reflect::reflect;
use crate::scalp::scalp;

use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_evaluator::evaluator_stats::{Lane as EvalLane, ReconTrade};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane as WlLane};
use pump_quant_watchlist::lane_ingest::ingest_union;
use pump_quant_watchlist::lane_performance::LanePerformance;
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

/// Index of an evaluator lane into the running-accumulator array.
const fn accum_index(lane: EvalLane) -> usize {
    match lane {
        EvalLane::Scalp => 0,
        EvalLane::Early => 1,
    }
}

/// How the engine is allowed to act. The laptop build supports only paper and
/// replay; live capital is a Tier-0 human-gated world this type cannot express, so
/// no code path in this binary can reach it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Drive the calibrated fill model; no capital moves.
    Paper,
    /// Re-run a recorded event journal for determinism checking.
    Replay,
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
    /// Realized net-SOL per source lane, lamports.
    pub per_lane_net: [(WlLane, i64); WlLane::COUNT],
    /// Final adapted lane weights, bps.
    pub final_weights: [(WlLane, u32); WlLane::COUNT],
    /// Canonical digest of the decision journal (determinism fingerprint).
    pub journal_digest: u64,
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
    confirmed: BTreeMap<[u8; 32], u64>,

    lane_perf: LanePerformance,
    /// Running net-SOL reconciliation per evaluator lane (Scalp=0, Early=1); bounded.
    recon: [ReconAccum; 2],
    journal: DecisionJournal,

    promoted: u64,
    admitted: u64,
    rejected: u64,
}

impl Engine {
    /// Construct an engine under a validated config and a run mode.
    #[must_use]
    pub fn new(cfg: Config, mode: RunMode) -> Self {
        let weights = LaneWeights::from_defaults();
        let params = RankParams::new(cfg.watchlist_ttl_ticks);
        let watchlist = WatchlistState::new(cfg.watchlist_capacity, params, weights);
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
            recon: [ReconAccum::default(); 2],
            journal: DecisionJournal::new(),
            promoted: 0,
            admitted: 0,
            rejected: 0,
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

    /// Feed one event.
    pub fn tick(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::MarketTrade {
                mint,
                liquidity_lamports,
                signed_base,
                buyer_entity,
                age_slots,
            } => {
                self.numeric.observe(
                    mint,
                    liquidity_lamports,
                    signed_base,
                    buyer_entity,
                    age_slots,
                    self.now,
                );
            }
            AppEvent::NarrativeSample {
                mint,
                prior_active,
                new_mentions,
            } => self.narrative.observe(mint, prior_active, new_mentions),
            AppEvent::SocialCall {
                mint,
                source_quality_bp,
            } => self.social.observe(mint, source_quality_bp),
            AppEvent::WalletAction {
                mint,
                followable,
                size_lamports,
            } => self.wallet.observe(mint, followable, size_lamports),
            AppEvent::OnchainConfirm {
                mint,
                sellable_depth_lamports,
            } => self.confirm(*mint.as_bytes(), sellable_depth_lamports),
            AppEvent::Tick => self.evaluate(),
        }
    }

    fn confirm(&mut self, mint: [u8; 32], depth: u64) {
        // Bound the confirmed set alongside the watchlist (§99); the multiple is a
        // config field, not a baked-in constant.
        let cap = self
            .cfg
            .watchlist_capacity
            .saturating_mul(self.cfg.confirmed_capacity_mult)
            .max(1);
        if !self.confirmed.contains_key(&mint) && self.confirmed.len() >= cap {
            if let Some((&weakest, _)) = self.confirmed.iter().min_by_key(|(_, &d)| d) {
                self.confirmed.remove(&weakest);
            }
        }
        self.confirmed.insert(mint, depth);
    }

    /// The evaluation half of the loop, run once per `Tick`.
    fn evaluate(&mut self) {
        self.now = self.now.saturating_add(1);

        // 1. Discovery: every lane emits independently; union, not intersection.
        // Lane scoring parameters come from config — no band edge or scale is baked in.
        let mut all: Vec<Candidate> = Vec::new();
        all.extend(self.numeric.emit(self.now));
        all.extend(self.narrative.emit(
            self.now,
            self.cfg.narrative_stage_hi_fp,
            self.cfg.narrative_stage_lo_fp,
        ));
        all.extend(self.social.emit(self.now));
        all.extend(self.wallet.emit(self.now, self.cfg.wallet_score_scale));
        let unioned = ingest_union(all, &self.weights);
        for cand in unioned.values() {
            self.watchlist.insert(*cand, self.now);
        }

        // 2. Recency prune.
        self.watchlist.prune(self.now);

        // 3. Promote the top-ranked survivors to the gate.
        let promoted = promote_top(
            &self.watchlist,
            self.now,
            self.cfg.promote_k,
            self.cfg.promote_min_rank,
        );

        // 4-5. Gate then paper-scalp each promotion.
        for cand in promoted {
            self.promoted += 1;
            let rank = self.watchlist.rank_of(&cand, self.now);
            self.journal.record(Decision::Promoted {
                mint: cand.mint.bytes(),
                lane: cand.lane as u8,
                rank,
            });
            self.gate_and_scalp(cand);
        }

        // 6. Reflection cadence: realized net-SOL reshapes discovery weights.
        // (Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85.)
        #[allow(clippy::manual_is_multiple_of)]
        if self.now % self.cfg.reflect_every_ticks == 0 {
            self.run_reflection();
        }
    }

    fn gate_and_scalp(&mut self, cand: Candidate) {
        let mint_bytes = cand.mint.bytes();
        let domain_mint = DomainMint::from_bytes(mint_bytes);
        let numeric_feats = self.numeric.features_for(domain_mint);
        // Confirmation exists only with an on-chain confirm AND numeric evidence;
        // a confirm with no numeric snapshot degrades to a NoNumericConfirmation
        // reject inside the gate (default features carry zero liquidity).
        let confirmation = self.confirmed.get(&mint_bytes).map(|&depth| Confirmation {
            sellable_depth_lamports: depth,
            numeric: numeric_feats.unwrap_or_default(),
        });

        match decide(&cand, confirmation, &self.cfg) {
            GateDecision::Admit(band) => {
                self.admitted += 1;
                let depth = confirmation.map(|c| c.sellable_depth_lamports).unwrap_or(0);
                let result = scalp(
                    cand.lane,
                    &band,
                    self.cfg.gate_expected_move_bps,
                    depth,
                    &self.cfg,
                );
                self.journal.record(Decision::Admitted {
                    mint: mint_bytes,
                    size_lamports: result.size_lamports,
                });
                self.journal.record(Decision::Filled {
                    mint: mint_bytes,
                    net_pnl_lamports: result.net_pnl_lamports,
                });
                // Attribute realized net-SOL to the discovering lane for reflection.
                self.lane_perf.record(
                    cand.lane,
                    i64::try_from(result.net_pnl_lamports).unwrap_or(i64::MAX),
                );
                // Fold the reconciliation trade into the bounded running accountant.
                self.recon[accum_index(result.recon.lane)].add(&result.recon);
            }
            GateDecision::Reject(reason) => {
                self.rejected += 1;
                self.journal.record(Decision::Rejected {
                    mint: mint_bytes,
                    reason: reject_code(reason),
                });
            }
        }
    }

    fn run_reflection(&mut self) {
        let deltas = reflect(&self.lane_perf, &mut self.weights, &self.cfg);
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

    /// Drive a whole event stream and produce the report.
    pub fn run(&mut self, events: &[AppEvent]) -> Report {
        for &ev in events {
            self.tick(ev);
        }
        self.report()
    }

    /// Snapshot the current report without consuming the engine.
    #[must_use]
    pub fn report(&self) -> Report {
        let scalp_net = self.recon[accum_index(EvalLane::Scalp)].net();
        let early_net = self.recon[accum_index(EvalLane::Early)].net();
        let mut per_lane_net = [(WlLane::CreationSniper, 0i64); WlLane::COUNT];
        let mut final_weights = [(WlLane::CreationSniper, 0u32); WlLane::COUNT];
        for (i, lane) in WlLane::ALL.into_iter().enumerate() {
            per_lane_net[i] = (lane, self.lane_perf.net_sol(lane));
            final_weights[i] = (lane, self.weights.get(lane));
        }
        Report {
            ticks: self.now,
            promoted: self.promoted,
            admitted: self.admitted,
            rejected: self.rejected,
            net_lamports: scalp_net.saturating_add(early_net),
            per_lane_net,
            final_weights,
            journal_digest: self.journal.digest(),
        }
    }

    /// The decision journal, for inspection or persistence.
    #[must_use]
    pub fn journal(&self) -> &DecisionJournal {
        &self.journal
    }

    /// The numeric feature snapshot the engine currently holds for a mint, if any.
    #[must_use]
    pub fn numeric_features(&self, mint: DomainMint) -> Option<Features> {
        self.numeric.features_for(mint)
    }
}

/// Stable small codes for gate-reject reasons, for the journal.
const fn reject_code(r: GateReject) -> u8 {
    match r {
        GateReject::NeedsOnchainConfirmation => 1,
        GateReject::NoNumericConfirmation => 2,
        GateReject::EconomicallyUnviable => 3,
    }
}
