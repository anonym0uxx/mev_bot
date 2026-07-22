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
use crate::position::{Exit, LifecycleParams, ScalpLifecycle};
use crate::reflect::reflect;

use crate::attention::{AttentionField, AttentionParams};
use crate::event::CreatorActionKind;
use crate::social_earn::{SocialEarn, SocialEarnParams};
use crate::social_ingest::{ledger_quality, to_mention, SourceQualityPolicy};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_evaluator::evaluator_stats::{Lane as EvalLane, ReconTrade};
use pump_quant_ingest::social_parse::parse_social_event;
use pump_quant_ingest::social_source::SocialSource;
use pump_quant_market_state::creator::{CreatorEvent, CreatorState, CreatorStateReducer};
use pump_quant_market_state::meta::{
    rotation_between, CategoryEvent, CategoryEventKind, MetaRotationReducer, MetaRotationState,
};
use pump_quant_narrative::narrative::nv_meta_emergence;
use pump_quant_social::ledger::SourceQualityLedger;
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

/// Per-creator linked-cluster tracking bound (§99/§102). Small by design: a
/// creator funding more than this many distinct sybil clusters is already
/// maximally suspect, so the count saturates as a lower bound past here rather
/// than growing an unbounded set.
const CREATOR_LINKED_CLUSTER_CAP: usize = 64;

/// Maximum size fade (bps of 10_000) that creator *distribution* alone may apply.
/// A fully-distributed creator caps the haircut here — it can shrink size, never
/// veto a trade the on-chain gate already admitted (§22 behavioral-risk clause:
/// creator ownership is never an automatic binary reject).
const MAX_CREATOR_FADE_BPS: u32 = 5_000;

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
    /// Open-position attribution: mint → (discovering lane, net realized so far).
    /// Read when an exit books, removed when the position fully closes; carries the
    /// lane so realized net-SOL reflects to the right discovery weight (§29.9) and
    /// the total attributes back to the market's social callers (§82) on close.
    open_lane: BTreeMap<[u8; 32], (WlLane, i128)>,

    /// Reused per-tick discovery scratch: the union of all four lanes' emissions.
    /// Cleared (not freed) each tick so steady-state discovery does not re-allocate
    /// (§99: its capacity is bounded by the number of tracked mints, which the lanes
    /// already cap). Holds no state between ticks — purely a scratch buffer.
    scratch: Vec<Candidate>,

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
        // Build the meta reducer from config BEFORE `cfg` is moved into the struct.
        let meta = MetaRotationReducer::new(
            cfg.meta_taxonomy_version,
            cfg.meta_max_categories,
            cfg.meta_max_creators_per_cat,
        );
        // The exit-lifecycle cost model is tied to the operator's fee/tip config so
        // paper net-SOL is consistent with the gate's economics; the trigger scales
        // are §102 named constants (project design doc). Concurrency is bounded to
        // the confirmed-set size (§99) — no more open scalps than confirmed markets.
        let lifecycle_params = LifecycleParams {
            fee_bps: cfg.exit_fee_bps,
            tip_lamports: cfg.exit_tip_lamports,
            ..LifecycleParams::standard()
        };
        let position_cap = cfg
            .watchlist_capacity
            .saturating_mul(cfg.confirmed_capacity_mult)
            .max(1);
        let positions = ScalpLifecycle::new(lifecycle_params, position_cap);
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
            attention: AttentionField::new(AttentionParams::standard()),
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
            scratch: Vec::new(),
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
                    if let Some(exit) =
                        self.positions
                            .on_trade(mint.as_bytes(), price_u, signed_quote, self.now)
                    {
                        self.book_exit(exit);
                    }
                }
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
                self.observe_creator_action(*mint.as_bytes(), kind, slot)
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
        let mut applied = 0usize;
        for payload in &batch {
            if let Some(ev) = parse_social_event(&payload.json, payload.observed_at_ns) {
                // Earned favorable-rate from the reconciliation loop supersedes the
                // PUBLIC_BURNED baseline; an unproven source falls back to the ledger.
                let q = self
                    .social_earn
                    .quality_bps_for(ev.author_id)
                    .unwrap_or_else(|| {
                        ledger_quality(&self.ledger, ev.author_id, &self.quality_policy)
                    });
                let mention = to_mention(&ev);
                for m in ev.mints() {
                    self.social.observe(DomainMint::from_bytes(*m), q);
                    self.attention.observe(*m, mention);
                    self.social_earn
                        .record_call(ev.author_id, *m, payload.observed_at_ns);
                    applied += 1;
                }
            }
        }
        applied
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
        // Each lane appends into the reused `scratch` buffer (cleared, not freed) so
        // steady state allocates no per-tick lane vectors; the union then dedups by
        // mint. Disjoint field borrows keep this a single pass with no clones.
        self.scratch.clear();
        self.numeric
            .emit_into(&mut self.scratch, self.now, self.cfg.numeric_ofi_min_bp);
        self.narrative.emit_into(
            &mut self.scratch,
            self.now,
            self.cfg.narrative_stage_hi_fp,
            self.cfg.narrative_stage_lo_fp,
        );
        self.social.emit_into(&mut self.scratch, self.now);
        self.wallet
            .emit_into(&mut self.scratch, self.now, self.cfg.wallet_score_scale);
        // Social attention-velocity candidates (the full `virality = attention =
        // money` model) join the same union. The field is empty until social
        // attention is ingested, so this is a zero-cost no-op — and byte-identical —
        // for any run that never feeds it. Disjoint field borrows: the money proxy
        // reads `numeric`, confirmation reads `confirmed`, both immutable, while
        // `attention` and `scratch` are mutable.
        if !self.attention.is_empty() {
            let numeric = &self.numeric;
            let confirmed = &self.confirmed;
            self.attention.emit_into(
                &mut self.scratch,
                self.now,
                |m| {
                    numeric
                        .features_for(DomainMint::from_bytes(*m))
                        .map_or(0, |f| u64::from(f.buy_pressure_bp))
                },
                |m| confirmed.contains_key(m),
            );
        }
        let unioned = ingest_union(self.scratch.iter().copied(), &self.weights);
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
                // Size within the admitted band, then apply the corroboration-tier
                // creator/category haircut (10_000 = identity; only ever reduces).
                let size_mult_bps = self.size_haircut_bps(&mint_bytes);
                let size = (u128::from(band.x_cost) * u128::from(size_mult_bps) / 10_000) as u64;
                let size = size.clamp(band.x_min, band.x_max);
                // Entry price is the market's latest decoded print; the position is
                // then managed FORWARD per-swap by the exit lifecycle (§24) — no
                // one-shot fixed-move fill. Entry cost (principal + entry fee + tip)
                // is what the principal-recovery tranche must clear.
                let entry_price = self.numeric.latest_price_fp(domain_mint).unwrap_or(0);
                let entry_fee =
                    (u128::from(size) * u128::from(self.cfg.entry_fee_bps) / 10_000) as u64;
                let entry_cost = size
                    .saturating_add(entry_fee)
                    .saturating_add(self.cfg.entry_tip_lamports);
                // Open the managed position. A market already holding an open scalp
                // (or the concurrency cap) refuses a second — no double-admit.
                if entry_price > 0
                    && self
                        .positions
                        .open(mint_bytes, entry_price, size, entry_cost, self.now)
                {
                    self.admitted += 1;
                    self.open_lane.insert(mint_bytes, (cand.lane, 0));
                    self.journal.record(Decision::Admitted {
                        mint: mint_bytes,
                        size_lamports: size,
                    });
                }
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

    /// Book one realized exit from the held-position lifecycle: journal it, fold its
    /// net into the running per-lane reconciliation (the report's net-SOL) and the
    /// lane-performance accountant (reflection weights), and — when the position
    /// fully closes — attribute the market's total realized net back to its social
    /// callers (§82) and drop its attribution entry.
    fn book_exit(&mut self, e: Exit) {
        let lane = self.open_lane.get(&e.mint).map(|(l, _)| *l);
        if let Some(lane) = lane {
            self.lane_perf
                .record(lane, i64::try_from(e.net_lamports).unwrap_or(i64::MAX));
            let recon = ReconTrade {
                lane: eval_lane_of(lane),
                gross_lamports: e.net_lamports,
                fees: 0,
                tips: 0,
                failed_costs: 0,
            };
            self.recon[accum_index(recon.lane)].add(&recon);
        }
        self.journal.record(Decision::Filled {
            mint: e.mint,
            net_pnl_lamports: e.net_lamports,
        });
        if let Some((_, acc)) = self.open_lane.get_mut(&e.mint) {
            *acc = acc.saturating_add(e.net_lamports);
        }
        if e.closed {
            if let Some((_, total)) = self.open_lane.remove(&e.mint) {
                self.social_earn.record_outcome(&e.mint, total);
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
        // Recompute the category rank/size adjustments from the on-chain rotation
        // since the last reflection (strengthened, never created, by attention
        // breadth). Off the hot path; a no-op until categories have been fed.
        self.update_meta_rotation();
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

    /// The end-of-run report. **Finalizes first**: any still-open held position is
    /// force-closed at its last mark so the reported net-SOL is complete (§24 — a
    /// scalp is only realized when it closes). Idempotent — after finalize the
    /// lifecycle is empty, so calling `report` again yields the same numbers.
    pub fn report(&mut self) -> Report {
        self.finalize();
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
        mult.min(10_000) as u32
    }

    /// The current on-chain `MetaRotationState` snapshot — per-category launches,
    /// flow, creators and graduations (research-plane telemetry seam, §21.4).
    #[must_use]
    pub fn meta_snapshot(&self) -> MetaRotationState {
        self.meta.snapshot()
    }

    /// The creator-state snapshot for a market, if the creator lane has seen it
    /// (position, distribution, sold-fraction-of-peak). Telemetry seam.
    #[must_use]
    pub fn creator_state(&self, mint: &[u8; 32]) -> Option<CreatorState> {
        self.creators.get(mint).map(|r| r.snapshot())
    }

    /// The current signed discovery-rank adjustment (bps over a 10_000 base) for a
    /// category, if it is rotating: positive = emerging, negative = saturating.
    /// `None` when the category is not currently rotating. Telemetry seam.
    #[must_use]
    pub fn category_rank_adjustment(&self, category_id: u64) -> Option<i64> {
        self.category_rank_adj.get(&category_id).copied()
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
