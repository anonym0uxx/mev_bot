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
use pump_quant_features::micro::{
    classify_divergence, cumulative_volume_delta, order_flow_imbalance_bps, CvdDivergence,
};
use pump_quant_features::types::{Side, TradeEvent};
use pump_quant_narrative::narrative::{
    nv_candidate_score, nv_virality_coeff, AttentionMoneyDivergence, LifecycleStage,
};
use pump_quant_watchlist::candidate::{
    Candidate, DiscoveryLane, Features, Lane as WlLane, Mint as WlMint,
};
use std::collections::{BTreeMap, VecDeque};

/// Bound on how many distinct mints a single lane tracks before it evicts the
/// weakest (§99 bounded state). Set high enough that laptop replays never hit it.
const LANE_TRACK_CAP: usize = 4_096;

/// Max recent trades retained per mint for microstructure (§57/§99). A small ring:
/// CVD/OFI/VWAP over the last N swaps is the scalping horizon, and a bounded ring
/// keeps per-mint state O(1) and the emit hot path cache-friendly.
const NUMERIC_RING_CAP: usize = 64;

/// Minimum trades before the numeric microstructure is trusted. With fewer than
/// this, CVD/OFI/divergence are pure noise on a sparse tape (§21.7 thin-market
/// degradation clause), so the lane emits nothing rather than a spurious score.
const NUMERIC_MIN_TRADES: usize = 3;

/// Map order-flow imbalance (bps, −10_000..=10_000, or `None` on empty flow) onto
/// the shared 0..=10_000 buy-pressure scale (5_000 = balanced) so the `Features`
/// contract keeps meaning while being sourced from wash-robust *signed* flow rather
/// than the old buy/(buy+sell) share that a manipulator can inflate (§21.7).
#[inline]
#[must_use]
fn ofi_to_pressure_bp(ofi_bps: Option<i32>) -> u32 {
    match ofi_bps {
        Some(b) => ((i64::from(b) + 10_000) / 2) as u32,
        None => 0,
    }
}

/// The tape's momentum/reversion regime from the Roll-measure serial-covariance
/// sign — the **#1 crypto short-horizon predictor** (Easley et al., SSRN 4814346;
/// Roll 1984). On a bonding curve the sign of `cov(Δp_t, Δp_{t−1})` is an
/// impact-weighted run/alternation statistic: positive = one-sided flow (TREND),
/// negative = side alternation / churn (REVERT — also the matched-wash signature,
/// which usefully forces the strict playbook).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Regime {
    /// Positive serial covariance: momentum playbook (current gates unchanged).
    Trend,
    /// Deadband / insufficient tape: default playbook.
    Neutral,
    /// Negative serial covariance: raised entry bar + reduced size (Kaminski–Lo —
    /// momentum-following components only earn their keep under positive ρ).
    Revert,
}

/// Minimum nonzero price deltas before the Roll estimator is trusted; below this
/// the tape is noise and the regime stays [`Regime::Neutral`] (fail-safe).
const ROLL_MIN_NONZERO_DELTAS: usize = 16;

/// Lag-1 (uncentered) autocorrelation of trade-price changes over the ring, in bps
/// of [−10_000, 10_000]: `10_000 × Σ Δᵢ·Δᵢ₋₁ / Σ Δᵢ²` (i128; truncation toward
/// zero biases toward Neutral — fail-safe). `None` on insufficient nonzero deltas.
#[must_use]
fn roll_rho_bp(trades: &[TradeEvent]) -> Option<i64> {
    if trades.len() < 3 {
        return None;
    }
    let mut cov_num: i128 = 0;
    let mut den: i128 = 0;
    let mut prev_delta: Option<i128> = None;
    let mut nonzero = 0usize;
    for w in trades.windows(2) {
        let d = w[1].price_fp - w[0].price_fp;
        if d != 0 {
            nonzero += 1;
        }
        den = den.saturating_add(d.saturating_mul(d));
        if let Some(pd) = prev_delta {
            cov_num = cov_num.saturating_add(d.saturating_mul(pd));
        }
        prev_delta = Some(d);
    }
    if nonzero < ROLL_MIN_NONZERO_DELTAS || den == 0 {
        return None;
    }
    Some(((cov_num.saturating_mul(10_000)) / den) as i64)
}

/// Classify a rho reading against operator deadband edges into a [`Regime`].
#[inline]
#[must_use]
fn classify_regime(rho_bp: Option<i64>, trend_bp: i64, revert_bp: i64) -> Regime {
    match rho_bp {
        Some(r) if r >= trend_bp => Regime::Trend,
        Some(r) if r <= revert_bp => Regime::Revert,
        _ => Regime::Neutral,
    }
}

/// The operator gate the numeric lane's emission consults (§102 named, config-fed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumericEmitGate {
    /// Baseline minimum OFI (bps) to emit a self-authorizing candidate.
    pub ofi_min_bp: u32,
    /// Raised OFI bar under [`Regime::Revert`] (the regime is breaking — only a
    /// violent imbalance qualifies; Kaminski–Lo value-destruction case otherwise).
    pub revert_ofi_min_bp: u32,
    /// Regime deadband edge for TREND (rho bps ≥ this).
    pub roll_trend_bp: i64,
    /// Regime deadband edge for REVERT (rho bps ≤ this; negative).
    pub roll_revert_bp: i64,
    /// Evidence freshness TTL (ticks): a mint whose last decoded trade is older
    /// emits nothing — dead tapes must not keep ranking (§29.6/§34.3 staleness law).
    pub evidence_ttl_ticks: u64,
}

/// The documented source→ranking-lane bijection (see module docs).
#[inline]
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
#[inline]
#[must_use]
pub fn to_wl_mint(m: DomainMint) -> WlMint {
    WlMint::new(*m.as_bytes())
}

/// Per-mint numeric microstructure accumulator: a bounded ring of recent decoded
/// swaps plus the O(1) scalar context the discovery score needs. The ring is what
/// the real `features::micro` CVD / OFI / VWAP-divergence functions fold over.
#[derive(Clone, Debug, Default)]
struct NumericObs {
    liquidity_lamports: u64,
    buyer_bitset: u64,
    age_slots: u32,
    last_tick: u64,
    /// Bounded ring of recent trades (oldest→newest), cap = [`NUMERIC_RING_CAP`].
    /// A [`VecDeque`] so `observe()` — the single hottest path (once per decoded
    /// swap) — evicts the oldest in O(1) (`pop_front`) instead of the O(n) memmove
    /// a `Vec::remove(0)` incurred (O1 hot-path fix). Readers that need a single
    /// contiguous ordered slice for the `features::micro` folds obtain one via
    /// [`NumericObs::with_ordered`] — at most one bounded copy, once per emit,
    /// paid on the cold(er) read path rather than the hot decode path.
    trades: VecDeque<TradeEvent>,
}

impl NumericObs {
    /// Call `f` with the trade ring as a single contiguous ordered (oldest→newest)
    /// `&[TradeEvent]` slice. When the deque is already contiguous this is
    /// zero-copy (`as_slices().0`); when it has wrapped, the (bounded, ≤
    /// [`NUMERIC_RING_CAP`]) contents are copied once into a stack buffer. The
    /// resulting slice is byte-identical to what the old `Vec` presented, so every
    /// derived quantity — and the golden digest — is unchanged.
    #[inline]
    fn with_ordered<R>(&self, f: impl FnOnce(&[TradeEvent]) -> R) -> R {
        let (a, b) = self.trades.as_slices();
        if b.is_empty() {
            // Contiguous (includes the empty deque): hand the fold the slice directly.
            f(a)
        } else {
            // Wrapped ⇒ `a` (the front run) is non-empty, so `a[0]` is a valid seed
            // for the fixed stack scratch; every used cell is overwritten below.
            let mut buf = [a[0]; NUMERIC_RING_CAP];
            let split = a.len();
            let n = split + b.len();
            buf[..split].copy_from_slice(a);
            buf[split..n].copy_from_slice(b);
            f(&buf[..n])
        }
    }
}

/// The on-chain numeric lane: real trade-flow microstructure (CVD, order-flow
/// imbalance, CVD/price divergence) over a bounded per-mint trade ring. This is the
/// only self-authorizing lane, and it now discovers on *signed* flow — wash-robust,
/// exhaustion-aware — instead of the old buy/(buy+sell) share a manipulator could
/// fake (§21.7). Every derived quantity is integer/fixed-point (§22).
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

    /// Ingest a decoded swap: append it to the mint's bounded trade ring and refresh
    /// the scalar context. `price_fp` is fixed-point (PRICE_SCALE), `quote_lamports`
    /// the swap's quote volume (signed into CVD by `signed_base`'s sign).
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        mint: DomainMint,
        price_fp: i128,
        quote_lamports: u64,
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
        // Cheap deterministic buyer-breadth proxy: fold the entity id into a 64-bit
        // set so `unique_buyers` grows without an unbounded per-mint collection.
        e.buyer_bitset |= 1u64 << (buyer_entity % 64);
        let side = if signed_base >= 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        // Bounded ring (§57/§99): drop the oldest when full. `pop_front` is O(1)
        // on the `VecDeque` (vs the old `Vec::remove(0)` O(n) memmove) — this is the
        // single hottest path, run once per decoded swap (O1).
        if e.trades.len() >= NUMERIC_RING_CAP {
            e.trades.pop_front();
        }
        e.trades.push_back(TradeEvent {
            event_id: now,
            ts_ns: now,
            price_fp,
            base_qty: signed_base.unsigned_abs(),
            quote_qty: quote_lamports,
            side,
        });
    }

    fn entry(&mut self, key: [u8; 32]) -> &mut NumericObs {
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            evict_weakest_numeric(&mut self.obs);
        }
        self.obs.entry(key).or_default()
    }

    /// The most recent trade price (fixed-point, PRICE_SCALE) the lane holds for a
    /// mint, if any — the entry / mark price the held-position lifecycle tracks
    /// against. Narrowed to `u64` (saturating) for the strategy protection leaf;
    /// realistic AMM prices in PRICE_SCALE units are well within `u64`.
    #[must_use]
    pub fn latest_price_fp(&self, mint: DomainMint) -> Option<u64> {
        self.obs
            .get(mint.as_bytes())
            .and_then(|o| o.trades.back())
            .map(|t| u64::try_from(t.price_fp.max(0)).unwrap_or(u64::MAX))
    }

    /// Age (ticks) of the mint's most recent numeric evidence, or `None` when
    /// untracked. The gate consults this so a FRESH on-chain confirm can never
    /// borrow a STALE numeric snapshot (§34.3: freshness is earned per proof —
    /// depth observed long ago is not depth now).
    #[must_use]
    pub fn evidence_age(&self, mint: DomainMint, now: u64) -> Option<u64> {
        self.obs
            .get(mint.as_bytes())
            .map(|o| now.saturating_sub(o.last_tick))
    }

    /// The numeric feature snapshot for a mint, if the lane has seen it. Buy-pressure
    /// is now the OFI-derived (signed, wash-robust) value on the shared scale.
    #[must_use]
    pub fn features_for(&self, mint: DomainMint) -> Option<Features> {
        self.obs.get(mint.as_bytes()).map(|o| Features {
            liquidity_lamports: o.liquidity_lamports,
            buy_pressure_bp: ofi_to_pressure_bp(o.with_ordered(order_flow_imbalance_bps)),
            unique_buyers: o.buyer_bitset.count_ones(),
            age_slots: o.age_slots,
        })
    }

    /// The tape regime for a mint (Roll-sign over its trade ring). [`Regime::Neutral`]
    /// for an untracked mint or an insufficient tape.
    #[must_use]
    pub fn regime_of(&self, mint: DomainMint, trend_bp: i64, revert_bp: i64) -> Regime {
        match self.obs.get(mint.as_bytes()) {
            Some(o) => classify_regime(o.with_ordered(roll_rho_bp), trend_bp, revert_bp),
            None => Regime::Neutral,
        }
    }

    /// Emit one candidate per bullish-flow mint with an integer discovery score.
    #[must_use]
    pub fn emit(&self, now: u64, gate: &NumericEmitGate) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, gate);
        out
    }

    /// Append one self-authorizing candidate per mint whose real trade flow is
    /// genuinely bullish into `buf`.
    ///
    /// **Sign-agreement gate (§21.7 CVD/OFI/VWAP combined, wash-robust).** A numeric
    /// candidate is emitted only when order-flow imbalance clears the regime's OFI
    /// bar (baseline in TREND/NEUTRAL, raised under REVERT — a mean-reverting tape
    /// only qualifies on a violent imbalance, Kaminski–Lo), the cumulative volume
    /// delta is net-buy, price is NOT diverging bearishly from flow (exhaustion),
    /// **and the evidence is fresh** — a mint whose last decoded trade is older than
    /// `evidence_ttl_ticks` emits nothing (dead tapes must not keep ranking, §34.3).
    /// Sub-[`NUMERIC_MIN_TRADES`] sparse tapes emit nothing. Score is monotone in
    /// imbalance strength × liquidity magnitude × buyer breadth; integer, saturating
    /// (§22). Reused buffer ⇒ alloc-free steady state.
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64, gate: &NumericEmitGate) {
        buf.reserve(self.obs.len());
        for (k, o) in &self.obs {
            if o.trades.len() < NUMERIC_MIN_TRADES {
                continue;
            }
            // Staleness law: dead evidence never ranks.
            if now.saturating_sub(o.last_tick) > gate.evidence_ttl_ticks {
                continue;
            }
            // One contiguous materialization per mint per emit (O1): all four
            // `micro`/Roll folds read the same ordered slice, so the numbers — and
            // the digest — are byte-identical to the old `Vec`-slice reads.
            let (ofi_bp, cvd, divergence, regime) = o.with_ordered(|trades| {
                let ofi_bp = order_flow_imbalance_bps(trades).unwrap_or(0);
                let cvd = cumulative_volume_delta(trades);
                let first = trades.first().map_or(0, |t| t.price_fp);
                let last = trades.last().map_or(0, |t| t.price_fp);
                let divergence = classify_divergence(last - first, cvd);
                let regime =
                    classify_regime(roll_rho_bp(trades), gate.roll_trend_bp, gate.roll_revert_bp);
                (ofi_bp, cvd, divergence, regime)
            });
            let required_ofi = if regime == Regime::Revert {
                gate.revert_ofi_min_bp
            } else {
                gate.ofi_min_bp
            };
            let bullish = ofi_bp >= 0
                && (ofi_bp as u32) >= required_ofi
                && cvd > 0
                && divergence != CvdDivergence::Bearish;
            if !bullish {
                continue;
            }
            let breadth = u64::from(o.buyer_bitset.count_ones()).max(1);
            let liq_decade = decade(o.liquidity_lamports);
            let score = (ofi_bp as u64)
                .saturating_mul(liq_decade)
                .saturating_mul(breadth);
            buf.push(
                Candidate::new(
                    WlMint::new(*k),
                    WlLane::ActiveMarketScalp,
                    score,
                    now,
                    Features {
                        liquidity_lamports: o.liquidity_lamports,
                        buy_pressure_bp: ofi_to_pressure_bp(Some(ofi_bp)),
                        unique_buyers: o.buyer_bitset.count_ones(),
                        age_slots: o.age_slots,
                    },
                )
                .with_discovery_lane(DiscoveryLane::ActiveMarket),
            );
        }
    }
}

/// Per-mint narrative accumulator.
#[derive(Clone, Copy, Debug, Default)]
struct NarrativeObs {
    prior_active: u64,
    new_mentions: u64,
    samples: u32,
    last_tick: u64,
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

    /// Ingest a narrative sample at logical `now` (freshness stamp).
    pub fn observe(&mut self, mint: DomainMint, prior_active: u64, new_mentions: u64, now: u64) {
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            evict_weakest_narrative(&mut self.obs);
        }
        let e = self.obs.entry(key).or_default();
        e.prior_active = prior_active;
        e.new_mentions = e.new_mentions.saturating_add(new_mentions);
        e.samples = e.samples.saturating_add(1);
        e.last_tick = now;
    }

    /// Emit one candidate per tracked mint. Score comes from `nv_candidate_score`
    /// with the lifecycle stage inferred from the virality coefficient against the
    /// operator-supplied band edges (`stage_hi_fp` ≥ `stage_lo_fp`, both in the
    /// narrative crate's fixed-point unit) — no band edge is baked in. Evidence
    /// older than `ttl_ticks` emits nothing (staleness law, §29.6/§34.3), and
    /// evidence ages CONTINUOUSLY before that cliff: every `decay.step_ticks` of
    /// age multiplies the score by `decay.rate_bp` (§29.6 decay-after-peak —
    /// memecoin attention decays in minutes; a stale mention must not rank like
    /// a fresh one). Decayed scores below `decay.floor` emit nothing at all.
    #[must_use]
    pub fn emit(
        &self,
        now: u64,
        stage_hi_fp: u64,
        stage_lo_fp: u64,
        ttl_ticks: u64,
        decay: &AttentionDecayParams,
    ) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, stage_hi_fp, stage_lo_fp, ttl_ticks, decay);
        out
    }

    /// Append one candidate per tracked mint into `buf` (see [`Self::emit`]).
    pub fn emit_into(
        &self,
        buf: &mut Vec<Candidate>,
        now: u64,
        stage_hi_fp: u64,
        stage_lo_fp: u64,
        ttl_ticks: u64,
        decay: &AttentionDecayParams,
    ) {
        buf.reserve(self.obs.len());
        for (k, o) in &self.obs {
            let age = now.saturating_sub(o.last_tick);
            if age > ttl_ticks {
                continue;
            }
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
            let score = decay.apply(score, age);
            if score < decay.floor {
                continue;
            }
            buf.push(
                Candidate::new(
                    WlMint::new(*k),
                    WlLane::EarlyConfirmation,
                    score,
                    now,
                    Features::default(),
                )
                .with_discovery_lane(DiscoveryLane::NarrativeAttentionVelocity),
            );
        }
    }
}

/// §29.6 attention-decay law applied to narrative evidence at emission time:
/// multiplicative shrinkage per age step, with an absolute floor below which the
/// evidence is treated as gone. All parameters are operator config (§102); the
/// TTL cliff (§34.3) remains the hard cutoff behind this continuous ramp.
#[derive(Clone, Copy, Debug)]
pub struct AttentionDecayParams {
    /// Per-step multiplicative survival rate, bps of 10_000 (≤ 10_000).
    pub rate_bp: u32,
    /// Logical ticks per decay step (clamped ≥ 1 by config).
    pub step_ticks: u64,
    /// Absolute decayed-score floor: below this the mint emits nothing.
    pub floor: u64,
}

impl AttentionDecayParams {
    /// Bound on decay iterations: after this many steps at any realistic rate
    /// the score is far below any meaningful floor (0.933^32 ≈ 0.11 of peak,
    /// compounding on already-small integer scores).
    const MAX_STEPS: u64 = 32;

    /// `score × rate^(age/step)`, integer, saturating, bounded iterations.
    #[must_use]
    pub fn apply(&self, score: u64, age_ticks: u64) -> u64 {
        let steps = (age_ticks / self.step_ticks.max(1)).min(Self::MAX_STEPS);
        let mut s = u128::from(score);
        for _ in 0..steps {
            s = s * u128::from(self.rate_bp) / 10_000;
        }
        s as u64
    }
}

/// Per-mint social accumulator: summed quality weight, last observation tick, and
/// whether the mint was surfaced by a DESIGNATED-caller ALPHA room (LAW D1). The
/// alpha flag is STICKY — once a paid Discord room calls a mint, that mint's
/// realized net SOL is attributed to the independent `AlphaCall` discovery lane so
/// the room earns its keep separately from the open social-caller firehose (§71
/// reflection integrity). It never touches ranking (both lanes present as the
/// `CreationSniper` archetype); it only tags the net-SOL attribution key.
#[derive(Clone, Copy, Debug, Default)]
struct SocialObs {
    quality: u64,
    last: u64,
    alpha: bool,
}

/// The social lane: quality-weighted call accumulation. Corroboration-tier.
#[derive(Clone, Debug, Default)]
pub struct SocialLane {
    obs: BTreeMap<[u8; 32], SocialObs>,
}

impl SocialLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a scored social call at logical `now`. Weak sources contribute
    /// proportionally less. Presents as the open `SocialCaller` discovery lane.
    pub fn observe(&mut self, mint: DomainMint, source_quality_bp: u32, now: u64) {
        self.observe_lane(mint, source_quality_bp, now, false);
    }

    /// LAW D1: ingest a scored call from a DESIGNATED-caller Discord ALPHA room —
    /// identical accumulation, but the mint is marked alpha-sourced so it emits on
    /// the independent `AlphaCall` discovery lane (sticky; §71 reflection integrity).
    pub fn observe_alpha(&mut self, mint: DomainMint, source_quality_bp: u32, now: u64) {
        self.observe_lane(mint, source_quality_bp, now, true);
    }

    fn observe_lane(&mut self, mint: DomainMint, source_quality_bp: u32, now: u64, alpha: bool) {
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            if let Some((&weakest, _)) = self.obs.iter().min_by_key(|(_, o)| o.quality) {
                self.obs.remove(&weakest);
            }
        }
        let e = self.obs.entry(key).or_insert(SocialObs {
            quality: 0,
            last: now,
            alpha: false,
        });
        e.quality = e.quality.saturating_add(source_quality_bp as u64);
        e.last = now;
        e.alpha |= alpha;
    }

    /// Emit one candidate per FRESH tracked mint. Score is the summed quality
    /// weight; evidence older than `ttl_ticks` emits nothing — one call long ago
    /// must not rank a mint forever (staleness law, §29.6/§34.3).
    #[must_use]
    pub fn emit(&self, now: u64, ttl_ticks: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, ttl_ticks);
        out
    }

    /// Append one candidate per fresh tracked mint into `buf` (see [`Self::emit`]).
    /// An alpha-sourced mint (LAW D1) carries the `AlphaCall` discovery lane; every
    /// other social call carries `SocialCaller`. Both present as `CreationSniper`,
    /// so ranking is unchanged — only the net-SOL attribution key differs.
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64, ttl_ticks: u64) {
        buf.reserve(self.obs.len());
        for (k, o) in &self.obs {
            if now.saturating_sub(o.last) > ttl_ticks {
                continue;
            }
            let discovery_lane = if o.alpha {
                DiscoveryLane::AlphaCall
            } else {
                DiscoveryLane::SocialCaller
            };
            buf.push(
                Candidate::new(
                    WlMint::new(*k),
                    WlLane::CreationSniper,
                    o.quality,
                    now,
                    Features::default(),
                )
                .with_discovery_lane(discovery_lane),
            );
        }
    }
}

/// The wallet / smart-money lane: cumulative followable size. Corroboration-tier.
#[derive(Clone, Debug, Default)]
pub struct WalletLane {
    /// mint → (cumulative followable lamports, last observation tick).
    obs: BTreeMap<[u8; 32], (u64, u64)>,
}

impl WalletLane {
    /// A fresh, empty lane.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a smart-money action at logical `now`; only followable wallets
    /// contribute.
    pub fn observe(&mut self, mint: DomainMint, followable: bool, size_lamports: u64, now: u64) {
        if !followable {
            return;
        }
        let key = *mint.as_bytes();
        if !self.obs.contains_key(&key) && self.obs.len() >= LANE_TRACK_CAP {
            if let Some((&weakest, _)) = self.obs.iter().min_by_key(|(_, &(v, _))| v) {
                self.obs.remove(&weakest);
            }
        }
        let e = self.obs.entry(key).or_insert((0, now));
        e.0 = e.0.saturating_add(size_lamports);
        e.1 = now;
    }

    /// Cumulative followable smart-money inflow (lamports) tracked for a mint, or
    /// `0` when untracked. §70.1: only *followable* wallet entries accumulate here
    /// (the `observe` filter), so this is the monotone smart-wallet-entry / net-
    /// inflow component the composite money proxy folds in ahead of price momentum
    /// (§70.1 M = smart-wallet entry + holder growth + net inflow). Read-only; no
    /// wall-clock (§22).
    #[must_use]
    pub fn inflow_of(&self, mint: DomainMint) -> u64 {
        self.obs.get(mint.as_bytes()).map_or(0, |&(size, _)| size)
    }

    /// Emit one candidate per FRESH tracked mint. Score is cumulative followable
    /// size, compressed to a decade then scaled by the operator-supplied
    /// `score_scale` so it is comparable with the other lanes' score magnitudes —
    /// the cross-lane weight is a config field, not a baked-in constant. Evidence
    /// older than `ttl_ticks` emits nothing (staleness law).
    #[must_use]
    pub fn emit(&self, now: u64, score_scale: u64, ttl_ticks: u64) -> Vec<Candidate> {
        let mut out = Vec::with_capacity(self.obs.len());
        self.emit_into(&mut out, now, score_scale, ttl_ticks);
        out
    }

    /// Append one candidate per fresh tracked mint into `buf` (see [`Self::emit`]).
    pub fn emit_into(&self, buf: &mut Vec<Candidate>, now: u64, score_scale: u64, ttl_ticks: u64) {
        buf.reserve(self.obs.len());
        for (k, &(size, last)) in &self.obs {
            if now.saturating_sub(last) > ttl_ticks {
                continue;
            }
            buf.push(
                Candidate::new(
                    WlMint::new(*k),
                    WlLane::GraduationTransition,
                    decade(size).saturating_mul(score_scale),
                    now,
                    Features::default(),
                )
                .with_discovery_lane(DiscoveryLane::WalletSmartMoney),
            );
        }
    }
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
    // Weakest = fewest observed trades (least microstructure evidence). Deterministic
    // and cheap; the cap is high enough that laptop replays rarely reach here (§99).
    if let Some((&weakest, _)) = obs.iter().min_by_key(|(_, o)| o.trades.len()) {
        obs.remove(&weakest);
    }
}

fn evict_weakest_narrative(obs: &mut BTreeMap<[u8; 32], NarrativeObs>) {
    if let Some((&weakest, _)) = obs.iter().min_by_key(|(_, o)| o.new_mentions) {
        obs.remove(&weakest);
    }
}
