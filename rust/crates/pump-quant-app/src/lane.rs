//! The independent discovery lanes.
//!
//! The constitution's §71 mandate is *union, not intersection*: each lane scans the
//! world its own way and can surface a mint onto the watchlist on its own, without
//! waiting for any other lane to agree. A loud social call and a quiet on-chain
//! accumulation are both admitted to the watchlist; they are reconciled later, at
//! the gate, not suppressed at discovery.
//!
//! Four source modalities are modelled, bound one-to-one onto the watchlist's four
//! ranking lanes so each source carries its own adaptable weight (which is what the
//! reflection pass tunes from realized net-SOL):
//!
//! | discovery source            | watchlist lane              | self-authorizing |
//! |-----------------------------|-----------------------------|------------------|
//! | Numeric (on-chain flow)     | `ActiveMarketScalp`         | yes              |
//! | Narrative (attention)       | `EarlyConfirmation`         | no (corroborate) |
//! | Social (calls/mentions)     | `CreationSniper`            | no (corroborate) |
//! | Wallet (smart-money)        | `GraduationTransition`      | no (corroborate) |
//!
//! Only the numeric lane's evidence may, on its own, authorise capital. The other
//! three are corroboration that raises rank but never triggers entry alone — that
//! discipline is enforced at the gate (`crate::gate`), and the mapping here records
//! which lane is which. Every score is an integer built from the real leaf crates
//! (`pump_quant_narrative`); no floating point reaches a score (§22).

use crate::event::LaneKind;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_narrative::narrative::{
    nv_candidate_score, nv_virality_coeff, AttentionMoneyDivergence, LifecycleStage,
};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane as WlLane, Mint as WlMint};
use std::collections::BTreeMap;

/// Bound on how many distinct mints a single lane tracks before it evicts the
/// weakest (§99 bounded state). Set high enough that laptop replays never hit it.
const LANE_TRACK_CAP: usize = 4_096;

/// The documented source→ranking-lane bijection (see module docs).
#[must_use]
pub const fn wl_lane_for(kind: LaneKind) -> WlLane {
    match kind {
        LaneKind::Numeric => WlLane::ActiveMarketScalp,
        LaneKind::Narrative => WlLane::EarlyConfirmation,
        LaneKind::Social => WlLane::CreationSniper,
        LaneKind::Wallet => WlLane::GraduationTransition,
    }
}

/// Convert a domain mint (32 bytes) to the watchlist's mint newtype. Both wrap the
/// same 32-byte identity; this is a total, lossless re-tag.
#[must_use]
pub fn to_wl_mint(m: DomainMint) -> WlMint {
    WlMint::new(*m.as_bytes())
}

/// Per-mint numeric microstructure accumulator.
#[derive(Clone, Copy, Debug, Default)]
struct NumericObs {
    liquidity_lamports: u64,
    buy_base: u128,
    sell_base: u128,
    buyer_bitset: u64,
    age_slots: u32,
    last_tick: u64,
}

/// The on-chain numeric lane: signed flow, liquidity and buyer breadth. This is the
/// only self-authorizing lane. Its discovery score is monotonic in net buy pressure
/// and liquidity, both integer.
#[derive(Clone, Debug, Default)]
pub struct NumericLane {
    obs: BTreeMap<[u8; 32], NumericObs>,
}

impl NumericLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a decoded swap.
    pub fn observe(
        &mut self,
        mint: DomainMint,
        liquidity_lamports: u64,
        signed_base: i64,
        buyer_entity: u64,
        age_slots: u32,
        now: u64,
    ) {
        let e = self.entry(*mint.as_bytes());
        e.liquidity_lamports = liquidity_lamports;
        e.age_slots = age_slots;
        e.last_tick = now;
        if signed_base >= 0 {
            e.buy_base = e
                .buy_base
                .saturating_add(signed_base.unsigned_abs() as u128);
        } else {
            e.sell_base = e
                .sell_base
                .saturating_add(signed_base.unsigned_abs() as u128);
        }
        // Cheap deterministic buyer-breadth proxy: fold the entity id into a 64-bit
        // set so `unique_buyers` grows without an unbounded per-mint collection.
        e.buyer_bitset |= 1u64 << (buyer_entity % 64);
    }

    fn entry(&mut self, key: [u8; 32]) -> &mut NumericObs {
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            evict_weakest_numeric(&mut self.obs);
        }
        self.obs.entry(key).or_default()
    }

    /// The numeric feature snapshot for a mint, if the lane has seen it.
    #[must_use]
    pub fn features_for(&self, mint: DomainMint) -> Option<Features> {
        self.obs.get(mint.as_bytes()).map(|o| Features {
            liquidity_lamports: o.liquidity_lamports,
            buy_pressure_bp: buy_pressure_bp(o.buy_base, o.sell_base),
            unique_buyers: o.buyer_bitset.count_ones(),
            age_slots: o.age_slots,
        })
    }

    /// Emit one candidate per tracked mint with an integer discovery score.
    #[must_use]
    pub fn emit(&self, now: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now);
        out
    }

    /// Append one candidate per tracked mint into `buf` (see [`Self::emit`]).
    ///
    /// The engine drives this every tick over a reused buffer, so steady-state
    /// discovery allocates nothing here; `emit` is the owning convenience wrapper.
    /// The emitted candidates are byte-identical to `emit`'s.
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64) {
        buf.reserve(self.obs.len());
        for (k, o) in &self.obs {
            let feats = Features {
                liquidity_lamports: o.liquidity_lamports,
                buy_pressure_bp: buy_pressure_bp(o.buy_base, o.sell_base),
                unique_buyers: o.buyer_bitset.count_ones(),
                age_slots: o.age_slots,
            };
            // Score = buy-pressure(bps) × liquidity-decade × buyer breadth.
            // Monotone in each input, saturating, integer-only.
            let liq_decade = decade(o.liquidity_lamports);
            let score = (feats.buy_pressure_bp as u64)
                .saturating_mul(liq_decade)
                .saturating_mul((feats.unique_buyers as u64).max(1));
            buf.push(Candidate::new(
                WlMint::new(*k),
                WlLane::ActiveMarketScalp,
                score,
                now,
                feats,
            ));
        }
    }
}

/// Per-mint narrative accumulator.
#[derive(Clone, Copy, Debug, Default)]
struct NarrativeObs {
    prior_active: u64,
    new_mentions: u64,
    samples: u32,
}

/// The narrative / attention-velocity lane. Uses the real `pump_quant_narrative`
/// leaves for the virality coefficient and candidate score, and applies the fade-
/// first cap by passing `money_confirmed = false` here — only the gate, after an
/// on-chain confirm, lets a narrative-driven candidate exceed the cap.
#[derive(Clone, Debug, Default)]
pub struct NarrativeLane {
    obs: BTreeMap<[u8; 32], NarrativeObs>,
}

impl NarrativeLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a narrative sample.
    pub fn observe(&mut self, mint: DomainMint, prior_active: u64, new_mentions: u64) {
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            evict_weakest_narrative(&mut self.obs);
        }
        let e = self.obs.entry(key).or_default();
        e.prior_active = prior_active;
        e.new_mentions = e.new_mentions.saturating_add(new_mentions);
        e.samples = e.samples.saturating_add(1);
    }

    /// Emit one candidate per tracked mint. Score comes from `nv_candidate_score`
    /// with the lifecycle stage inferred from the virality coefficient against the
    /// operator-supplied band edges (`stage_hi_fp` ≥ `stage_lo_fp`, both in the
    /// narrative crate's fixed-point unit) — no band edge is baked in.
    #[must_use]
    pub fn emit(&self, now: u64, stage_hi_fp: u64, stage_lo_fp: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, stage_hi_fp, stage_lo_fp);
        out
    }

    /// Append one candidate per tracked mint into `buf` (see [`Self::emit`]).
    pub fn emit_into(
        &self,
        buf: &mut Vec<Candidate>,
        now: u64,
        stage_hi_fp: u64,
        stage_lo_fp: u64,
    ) {
        buf.reserve(self.obs.len());
        for (k, o) in &self.obs {
            let virality = nv_virality_coeff(o.prior_active, o.new_mentions).unwrap_or(0);
            // Stage/divergence inferred deterministically from the configured
            // virality bands (in the narrative leaf's fixed-point unit).
            let stage = if virality >= stage_hi_fp {
                LifecycleStage::Virality
            } else if virality >= stage_lo_fp {
                LifecycleStage::Emergence
            } else {
                LifecycleStage::Formation
            };
            let score = nv_candidate_score(
                stage,
                AttentionMoneyDivergence::AttentionLeads,
                virality,
                0,
                // fade-first: pre-confirmation the narrative score is capped.
                false,
            );
            buf.push(Candidate::new(
                WlMint::new(*k),
                WlLane::EarlyConfirmation,
                score,
                now,
                Features::default(),
            ));
        }
    }
}

/// The social lane: quality-weighted call accumulation. Corroboration-tier.
#[derive(Clone, Debug, Default)]
pub struct SocialLane {
    obs: BTreeMap<[u8; 32], u64>,
}

impl SocialLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a scored social call. Weak sources contribute proportionally less.
    pub fn observe(&mut self, mint: DomainMint, source_quality_bp: u32) {
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            if let Some((&weakest, _)) = self.obs.iter().min_by_key(|(_, &v)| v) {
                self.obs.remove(&weakest);
            }
        }
        let e = self.obs.entry(key).or_insert(0);
        *e = e.saturating_add(source_quality_bp as u64);
    }

    /// Emit one candidate per tracked mint. Score is the summed quality weight.
    #[must_use]
    pub fn emit(&self, now: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now);
        out
    }

    /// Append one candidate per tracked mint into `buf` (see [`Self::emit`]).
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64) {
        buf.reserve(self.obs.len());
        for (k, &w) in &self.obs {
            buf.push(Candidate::new(
                WlMint::new(*k),
                WlLane::CreationSniper,
                w,
                now,
                Features::default(),
            ));
        }
    }
}

/// The wallet / smart-money lane: cumulative followable size. Corroboration-tier.
#[derive(Clone, Debug, Default)]
pub struct WalletLane {
    obs: BTreeMap<[u8; 32], u64>,
}

impl WalletLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a smart-money action; only followable wallets contribute.
    pub fn observe(&mut self, mint: DomainMint, followable: bool, size_lamports: u64) {
        if !followable {
            return;
        }
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            if let Some((&weakest, _)) = self.obs.iter().min_by_key(|(_, &v)| v) {
                self.obs.remove(&weakest);
            }
        }
        let e = self.obs.entry(key).or_insert(0);
        *e = e.saturating_add(size_lamports);
    }

    /// Emit one candidate per tracked mint. Score is cumulative followable size,
    /// compressed to a decade then scaled by the operator-supplied `score_scale` so
    /// it is comparable with the other lanes' score magnitudes — the cross-lane
    /// weight is a config field, not a baked-in constant.
    #[must_use]
    pub fn emit(&self, now: u64, score_scale: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, score_scale);
        out
    }

    /// Append one candidate per tracked mint into `buf` (see [`Self::emit`]).
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64, score_scale: u64) {
        buf.reserve(self.obs.len());
        for (k, &size) in &self.obs {
            buf.push(Candidate::new(
                WlMint::new(*k),
                WlLane::GraduationTransition,
                decade(size).saturating_mul(score_scale),
                now,
                Features::default(),
            ));
        }
    }
}

/// Buy pressure in basis points: `buy / (buy + sell)`, integer, 10_000 = 100%.
#[inline]
#[must_use]
fn buy_pressure_bp(buy: u128, sell: u128) -> u32 {
    let total = buy.saturating_add(sell);
    if total == 0 {
        return 0;
    }
    ((buy.saturating_mul(10_000)) / total) as u32
}

/// A coarse base-10 magnitude of a lamport quantity (0 → 0, 1..9 → 1, 10..99 → 2 …).
/// Keeps liquidity/size comparable across many orders of magnitude without a float.
///
/// Equivalent to the digit-count loop it replaces (`0 → 0`, otherwise
/// `floor(log10 v) + 1`) but branch-free via the intrinsic: `checked_ilog10`
/// returns `None` only for `v == 0`, mapping to `0`, and `Some(floor(log10 v))`
/// otherwise, to which we add one for the digit count. Byte-identical for all
/// `u64` (§22), just without the per-call division loop.
#[inline]
#[must_use]
fn decade(v: u64) -> u64 {
    v.checked_ilog10().map_or(0, |x| x as u64 + 1)
}

fn evict_weakest_numeric(obs: &mut BTreeMap<[u8; 32], NumericObs>) {
    if let Some((&weakest, _)) = obs.iter().min_by_key(|(_, o)| o.buy_base) {
        obs.remove(&weakest);
    }
}

fn evict_weakest_narrative(obs: &mut BTreeMap<[u8; 32], NarrativeObs>) {
    if let Some((&weakest, _)) = obs.iter().min_by_key(|(_, o)| o.new_mentions) {
        obs.remove(&weakest);
    }
}
