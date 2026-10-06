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

/// One live print, as the join needs it. Built from `AppEvent::MarketTrade` plus the venue the
/// provenance names (the event itself carries none).
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
    /// Receive instants of prints the feed derivation dropped before the reducer could
    /// see them. A flow window containing one is incomplete and refused by name.
    flow_upstream_drops: VecDeque<i64>,
    enrich_overflow: bool,
    venue: VenueLabel,
    recent: VecDeque<(Option<u64>, [u8; 32], i64, i64)>,
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
        }
    }

    #[must_use]
    pub fn counters(&self) -> IngestCounters {
        self.counters
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
        let key = (t.slot, t.trader.unwrap_or([0u8; 32]), t.signed_base, recv);
        if mc.recent.iter().any(|k| *k == key) {
            self.counters.duplicate += 1;
            return Ingest::Duplicate;
        }
        if mc.recent.len() >= DEDUPE_LOOKBACK {
            mc.recent.pop_front();
        }
        mc.recent.push_back(key);
        if mc.n_accepted == 0 {
            mc.first_seen_ms = recv;
        }
        mc.last_recv_ms = recv;
        mc.venue = t.venue;
        mc.n_accepted += 1;
        self.counters.accepted += 1;

        if let Some(st) = StateTrade::from_market_trade(
            recv,
            t.price_fp,
            t.quote_lamports,
            t.signed_base,
            t.buyer_entity,
            t.venue,
            Some(t.signed_base),
        ) {
            self.ledger.on_trade(&t.mint, st);
        }

        // Enrichment (holders / bundles): needs the wallet and both legs.
        match t.trader {
            Some(w) if t.signed_base != 0 => {
                if mc.enrich.len() >= MAX_ENRICH_TRADES_PER_MINT {
                    mc.enrich_overflow = true;
                } else {
                    mc.enrich.push(EnrichmentTrade {
                        recv_unix_ms: recv,
                        trader: w,
                        tokens_raw: i128::from(t.signed_base),
                        sol_lamports: t.quote_lamports,
                        slot: t.slot,
                    });
                }
            }
            _ => mc.identity_missing += 1,
        }

        // Flow: wallet + slot + total fee are required by the reducer's event; CU is carried as
        // Option. A print lacking any of them cannot enter the window, and that is counted.
        let flow_ev = match (t.trader, t.slot, t.fee_lamports) {
            (Some(w), Some(slot), Some(fee)) => flow_event_from_market_trade(
                &t.mint,
                slot,
                Some(recv),
                t.quote_lamports,
                t.signed_base,
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

    /// Record that the feed derivation dropped a print for `mint` at `drop_unix_ms` — a
    /// reserve delta the derivation refused before it could reach the flow reducer. Such a
    /// print is invisible to every received-print check, so the cache remembers the instant
    /// and refuses any 300 s flow window that contains it ([`JoinRefusal::FlowUpstreamDrop`])
    /// rather than serving the window as complete or as a quietly idle market.
    ///
    /// The record is bounded and self-healing: once the decision clock has advanced past the
    /// drop by more than [`WINDOW_300_MS`], the drop is outside the served window and normal
    /// serving resumes without any explicit clearing.
    pub fn note_flow_upstream_drop(&mut self, mint: [u8; 32], drop_unix_ms: i64) {
        let mc = self.mints.entry(mint).or_default();
        if mc.flow_upstream_drops.len() >= FLOW_UPSTREAM_DROP_RING {
            mc.flow_upstream_drops.pop_front();
        }
        mc.flow_upstream_drops.push_back(drop_unix_ms);
        self.counters.flow_upstream_drops += 1;
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
    fn prepare(&self, mint: &[u8; 32], t_dec_ms: i64) -> Result<Prepared, JoinRefusal> {
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
        // UPSTREAM DROP — fail-closed, and blamed before any "quiet"/"few trades" verdict.
        // A print the feed derivation refused never reached the reducer, so no received-print
        // check and no reserve age can prove this window complete: it is known-incomplete.
        // Refuse by name so a window driven quiet BY the drop reads as feed loss, never as an
        // idle market. The window recovers once the drop leaves the 300 s horizon.
        if let Some(&drop_ms) = mc
            .flow_upstream_drops
            .iter()
            .find(|&&d| d >= t_dec_ms - WINDOW_300_MS && d < t_dec_ms)
        {
            return Err(JoinRefusal::FlowUpstreamDrop { drop_ms });
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
        } = self.prepare(mint, t_dec_ms)?;
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
        } = self.prepare(mint, t_dec_ms)?;
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
        }
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

        // RECOVERY: extend the history past drop_ms + 300 s. The drop is now outside the
        // served flow window, so the same cache serves normally again with no explicit clear.
        for i in 80..155 {
            assert_eq!(c.observe_trade(&trade(i)), Ingest::Accepted);
        }
        let t_recover = t_dec(155);
        assert!(
            t_recover - drop_ms > WINDOW_300_MS,
            "the drop must have left the 300 s window: {drop_ms} vs {t_recover}"
        );
        assert!(
            c.snapshot(&MINT, t_recover).is_ok(),
            "once the drop ages out of the flow window, serving resumes"
        );
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
}
