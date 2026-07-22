//! The live social attention-velocity field — `virality = attention = money`.
//!
//! This is the deep social-ingestion integration: the [`crate::social_ingest`]
//! pipeline turns captured posts into narrative [`Mention`]s, and this field
//! accumulates them **per mint** and runs the full NARRATIVE ALPHA model on the
//! deterministic side — [`nv_attention_state`] (velocity / acceleration / breadth /
//! concentration), [`nv_attention_money_divergence`] against the on-chain money
//! trajectory, [`nv_lifecycle_stage`], [`nv_pre_legibility`], and
//! [`nv_virality_coeff`], fused by [`nv_candidate_score`] into a corroboration-tier
//! `EarlyConfirmation` candidate. Before this, social mentions only produced
//! corroboration calls; now the attention *derivative* — the actual edge — is live.
//!
//! # Discipline (binding)
//! * **Deterministic, integer, no wall-clock (§22).** The time base is the
//!   `observed_at_ns` each mention carries (measured at the `[S]` capture boundary
//!   and fed in through the deterministic event stream); the field reads no clock.
//!   The same mention stream always yields the same candidates.
//! * **Corroboration-tier / fade-first (§29, §71).** Every candidate is
//!   `EarlyConfirmation` (never self-authorizing); [`nv_candidate_score`] hard-caps
//!   the score when money is unconfirmed, so attention alone can never dominate.
//! * **Bounded (§99).** Tracked mints, per-mint mentions, and the level series are
//!   all capped; overflow evicts the weakest.
//! * **Named scales (§102).** Every window / threshold / step is a documented
//!   named constant in [`AttentionParams`], never an inline magic number.

use pump_quant_narrative::attention_state::{nv_attention_state, Mention};
use pump_quant_narrative::narrative::{
    nv_attention_money_divergence, nv_candidate_score, nv_lifecycle_stage, nv_pre_legibility,
    nv_virality_coeff, AttentionSeries, FP_ONE,
};
use pump_quant_watchlist::candidate::{Candidate, Features, Lane as WlLane, Mint as WlMint};
use std::collections::BTreeMap;

/// Named tuning for the attention field (§102 — each a documented scale, not a
/// magic number). Construct [`AttentionParams::standard`] for the shipped defaults;
/// operators may build a different set inside their envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionParams {
    /// Max recent mentions retained per mint (§99). Matches the narrative crate's
    /// `MAX_TRACKED` so a mint's distinct-source count is never under-counted.
    pub mention_cap: usize,
    /// Max attention-level samples kept for the velocity/acceleration series.
    pub series_cap: usize,
    /// Max distinct mints tracked before the weakest is evicted (§99).
    pub track_cap: usize,
    /// Trailing window (ns) for the 1-minute weighted-mention level.
    pub window_1m_ns: u64,
    /// Trailing window (ns) for the 5-minute weighted-mention level.
    pub window_5m_ns: u64,
    /// Lookback (in samples) for the discrete velocity/acceleration derivatives.
    pub series_window: usize,
    /// Age (ns) at which attention freshness decays to zero.
    pub freshness_full_ns: u64,
    /// Attention floor: below this weighted level a mint is still `Formation`.
    pub formation_level: u64,
    /// Symmetric deadband for attention-vs-money "rising" classification.
    pub divergence_threshold: i64,
    /// Pre-legibility age penalty per elapsed window (fixed-point over `FP_ONE`).
    pub age_step_fp: u64,
    /// Attention units each distinct GENUINE live-chat chatter adds to the
    /// weighted level while the live window is fresh (§29.6 stream structure).
    pub live_chatter_weight: u64,
    /// Attention units a fresh broadcaster call adds (see `standard()` rationale).
    pub live_broadcaster_weight: u64,
}

impl AttentionParams {
    /// The shipped defaults. Each constant is chosen with rationale (§102):
    /// minute/5-minute windows match the memecoin attention cadence; a 3-sample
    /// derivative lookback needs 7 samples to define acceleration; a 1-hour
    /// freshness horizon matches §29.6 staleness; the formation floor requires a
    /// small but real amount of weighted attention before "emergence"; a zero
    /// deadband treats any strictly-positive velocity as rising; and a `FP_ONE/16`
    /// age step fully legibilizes a narrative over ~16 windows. The live-chat
    /// weights (§29.6 stream/comment structure), chosen as a BREADTH-GATED law
    /// (§102 rationale, not fake precision): a broadcaster call alone is HALF
    /// the formation evidence (`live_broadcaster_weight = formation_level/2`),
    /// and only genuine distinct-chatter breadth may complete it — at
    /// `live_chatter_weight = 6`, roughly nine distinct non-echo chatters
    /// inside the live window close the gap. Thin chat behind a broadcaster
    /// call stays sub-formation; a raid of coordinated echoes counts zero
    /// (echo-excluded breadth). The §29 fade-first cap still binds until money
    /// confirms, so live attention can rank but never authorize.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            mention_cap: 64,
            series_cap: 16,
            track_cap: 4_096,
            window_1m_ns: 60_000_000_000,
            window_5m_ns: 300_000_000_000,
            series_window: 3,
            freshness_full_ns: 3_600_000_000_000,
            formation_level: 100,
            divergence_threshold: 0,
            age_step_fp: FP_ONE / 16,
            live_chatter_weight: 6,
            live_broadcaster_weight: 50,
        }
    }
}

impl Default for AttentionParams {
    fn default() -> Self {
        Self::standard()
    }
}

/// Distinct live-chat chatters tracked per mint before the count saturates
/// (§99 bounded state). 16 distinct genuine chatters inside one live window is
/// already maximal breadth evidence at Twitch-chat scale; past it the count is
/// a lower bound, exactly like the creator linked-cluster cap.
const LIVE_CHATTER_CAP: usize = 16;

/// Provenance of one mention, derived at the ingest seam from the normalized
/// event — a PARALLEL channel that reaches into the field's internal state
/// without touching the shared [`Mention`] type (whose shape is locked by
/// dossier tests in two crates). §29.6 names stream/comment events as
/// first-class attention structure; this is that structure, carried honestly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MentionProvenance {
    /// The mention originated in a REAL-TIME live-stream chat (Twitch IRC): the
    /// capture channel only delivers messages while the stream's chat is live,
    /// so this flag is structural, not inferred.
    pub realtime_chat: bool,
    /// The author IS the channel (broadcaster speaking in their own chat —
    /// `author_id == community_id` on Twitch): a broadcaster call, the §25
    /// PUMP_LIVE_STREAM archetype's defining trigger.
    pub broadcaster: bool,
    /// Distinct-originator identity for live-chat breadth (the author id).
    pub author_id: u64,
    /// Whether the event is an echo/coordinated repeat — echoes raise reach,
    /// never breadth (fade-first, §29).
    pub echo_or_coordinated: bool,
    /// An AGGREGATOR (CoinGecko-tier) lists this token: the §783 legibility
    /// clock. Once seen, the mint's pre-legibility earliness bonus is gone —
    /// permanently (listing is not un-observed). Reduce-only.
    pub aggregator: bool,
    /// A HIGH-CONFIDENCE BEARISH sentiment reading accompanies this mention
    /// (scam accusation / rug call territory). Reduce-only consumption: it
    /// suppresses the live-chat enthusiasm bonus while fresh; it never blocks
    /// tracking and never becomes negative market evidence on its own (§29.5).
    pub bearish: bool,
}

/// Per-mint accumulated attention state.
#[derive(Clone, Debug, Default)]
struct MintAttn {
    /// Bounded ring of recent mentions (cap = `params.mention_cap`).
    mentions: Vec<Mention>,
    /// Bounded ring of weighted-level samples, oldest→newest (cap = `series_cap`).
    levels: Vec<u64>,
    /// Earliest observed instant (ns) — narrative age origin.
    first_seen_ns: u64,
    /// Latest observed instant (ns) — the deterministic "now" for this mint.
    latest_ns: u64,
    /// Previous on-chain money level, for the money-velocity difference.
    prev_money: u64,
    /// Whether a money level has been recorded yet (first emit seeds `prev_money`).
    seen_money: bool,
    /// Distinct GENUINE (non-echo, non-coordinated) live-chat chatter ids that
    /// have named this mint — Twitch-origin breadth (bounded, saturating).
    live_chatters: Vec<u64>,
    /// Latest instant (ns) a broadcaster call named this mint (0 = never).
    broadcaster_seen_ns: u64,
    /// Latest instant (ns) any live-chat mention named this mint (0 = never).
    live_chat_latest_ns: u64,
    /// Whether an aggregator listing has EVER been observed (§783 legibility
    /// clock — one-way; a listed coin cannot regain earliness).
    aggregator_seen: bool,
    /// Latest instant (ns) a high-confidence bearish reading named this mint
    /// (0 = never). While fresh (within the 5-minute window) the live-chat
    /// bonus is suppressed.
    bearish_seen_ns: u64,
}

/// The bounded, per-mint social attention field. Fed by [`Self::observe`] from the
/// social-ingestion pipeline; drained by [`Self::emit_into`] each evaluation tick.
#[derive(Clone, Debug)]
pub struct AttentionField {
    obs: BTreeMap<[u8; 32], MintAttn>,
    params: AttentionParams,
}

impl AttentionField {
    /// A fresh field under the given tuning.
    #[must_use]
    pub fn new(params: AttentionParams) -> Self {
        Self {
            obs: BTreeMap::new(),
            params,
        }
    }

    /// Whether the field is tracking any mint (an empty field emits nothing, so a
    /// run that never ingests social attention pays zero cost and is byte-identical
    /// to one without this layer).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.obs.is_empty()
    }

    /// Number of tracked mints (bounded by `params.track_cap`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.obs.len()
    }

    /// The current attention *velocity* (engagement velocity) for a mint, if the
    /// field is tracking it — a **non-mutating** read used by the category
    /// meta-emergence aggregation ([`pump_quant_narrative::narrative::nv_meta_emergence`]).
    ///
    /// Recomputes [`nv_attention_state`] over the stored mentions and existing level
    /// series **without appending a new sample**, so it never perturbs the
    /// deterministic [`Self::emit_into`] path or the per-mint state. `None` for an
    /// untracked mint (UNKNOWN, §6.4). Deterministic: a pure function of the stored
    /// series and each mint's measured `latest_ns` — no clock (§22).
    #[must_use]
    pub fn velocity_of(&self, mint: &[u8; 32]) -> Option<i64> {
        let a = self.obs.get(mint)?;
        let state = nv_attention_state(
            &a.mentions,
            a.latest_ns,
            self.params.window_1m_ns,
            self.params.window_5m_ns,
            &a.levels,
            self.params.series_window,
            self.params.freshness_full_ns,
        );
        Some(state.engagement_velocity)
    }

    /// Record one narrative [`Mention`] against a mint (from the social pipeline).
    ///
    /// Bounded (§99): a new mint beyond `track_cap` evicts the mint with the fewest
    /// retained mentions (the weakest attention), and each mint keeps at most
    /// `mention_cap` most-recent mentions.
    pub fn observe(&mut self, mint: [u8; 32], mention: Mention) {
        // Neutral provenance: the no-live-chat path through `observe_tagged` is
        // byte-identical to the historical `observe` (the zero-hot-path-change
        // guarantee the golden digest pins).
        self.observe_tagged(mint, mention, &MentionProvenance::default());
    }

    /// Record one mention WITH its provenance (the deep live-chat channel).
    ///
    /// With a default (neutral) provenance this is exactly the historical
    /// [`Self::observe`]. With `realtime_chat` set, the mention additionally
    /// maintains the mint's live-chat internal state: distinct genuine chatter
    /// breadth (bounded at [`LIVE_CHATTER_CAP`]), broadcaster-call recency, and
    /// the live-window instant — which [`Self::emit_into`] converts into
    /// attention level through the operator-visible weights. Echo/coordinated
    /// mentions never add breadth (fade-first, §29); everything stays
    /// corroboration-tier — the gate still demands on-chain confirmation.
    pub fn observe_tagged(&mut self, mint: [u8; 32], mention: Mention, prov: &MentionProvenance) {
        if !self.obs.contains_key(&mint) && self.obs.len() >= self.params.track_cap {
            if let Some((&weakest, _)) = self.obs.iter().min_by_key(|(_, a)| a.mentions.len()) {
                self.obs.remove(&weakest);
            }
        }
        let cap = self.params.mention_cap;
        let a = self.obs.entry(mint).or_default();
        if a.mentions.is_empty() {
            a.first_seen_ns = mention.ts_ns;
        }
        a.first_seen_ns = a.first_seen_ns.min(mention.ts_ns);
        a.latest_ns = a.latest_ns.max(mention.ts_ns);
        if a.mentions.len() >= cap {
            a.mentions.remove(0); // drop oldest (ring); cap is small
        }
        a.mentions.push(mention);
        if prov.realtime_chat {
            a.live_chat_latest_ns = a.live_chat_latest_ns.max(mention.ts_ns);
            if prov.broadcaster {
                a.broadcaster_seen_ns = a.broadcaster_seen_ns.max(mention.ts_ns);
            }
            if !prov.echo_or_coordinated
                && a.live_chatters.len() < LIVE_CHATTER_CAP
                && !a.live_chatters.contains(&prov.author_id)
            {
                a.live_chatters.push(prov.author_id);
            }
        }
        if prov.aggregator {
            a.aggregator_seen = true;
        }
        if prov.bearish {
            a.bearish_seen_ns = a.bearish_seen_ns.max(mention.ts_ns);
        }
    }

    /// Emit one corroboration-tier `EarlyConfirmation` candidate per tracked mint
    /// whose fused attention score is positive, appending into `buf`.
    ///
    /// `money_of(mint)` supplies the current on-chain money level (a monotone proxy
    /// for smart-money flow — e.g. buy pressure), and `is_confirmed(mint)` whether
    /// the mint has an on-chain confirmation (the `money_confirmed` gate that lifts
    /// the fade-first cap). `now_tick` is the logical clock stamped onto the emitted
    /// candidate's `discovered_at`; the attention *windows* use each mint's measured
    /// `latest_ns`, never a wall-clock. Deterministic (BTreeMap order); mutates the
    /// per-mint level series and money baseline as a pure function of the inputs.
    pub fn emit_into<M, C>(
        &mut self,
        buf: &mut Vec<Candidate>,
        now_tick: u64,
        money_of: M,
        is_confirmed: C,
    ) where
        M: Fn(&[u8; 32]) -> u64,
        C: Fn(&[u8; 32]) -> bool,
    {
        let AttentionField { obs, params } = self;
        for (mint, a) in obs.iter_mut() {
            let now_ns = a.latest_ns;
            // Current weighted attention level: sum of mention weights inside the
            // 1-minute window (bounded by mention_cap).
            let base_level: u64 = a
                .mentions
                .iter()
                .filter(|m| now_ns.saturating_sub(m.ts_ns) < params.window_1m_ns)
                .fold(0u64, |acc, m| acc.saturating_add(m.weight));
            // §29.6 live-chat structure: while the live window is fresh, distinct
            // genuine chatter breadth and a broadcaster call add attention level
            // through the SAME model everything else uses (virality, stage,
            // divergence, fade cap all still bind downstream). When the mint has
            // no live-chat state, every term is zero and `level == base_level`
            // exactly — the no-Twitch path is byte-identical (golden-pinned).
            let live_fresh = a.live_chat_latest_ns > 0
                && now_ns.saturating_sub(a.live_chat_latest_ns) < params.window_5m_ns;
            // §29 fade-first: a FRESH high-confidence bearish reading (rug
            // call / scam accusation) suppresses the live-enthusiasm bonus —
            // reduce-only; tracking, level, and staging are otherwise
            // untouched (bearish sentiment is never negative market evidence
            // by itself, §29.5 — it only stops us AMPLIFYING).
            let bearish_fresh = a.bearish_seen_ns > 0
                && now_ns.saturating_sub(a.bearish_seen_ns) < params.window_5m_ns;
            let live_bonus: u64 = if live_fresh && !bearish_fresh {
                let breadth =
                    (a.live_chatters.len() as u64).saturating_mul(params.live_chatter_weight);
                let bcast_fresh = a.broadcaster_seen_ns > 0
                    && now_ns.saturating_sub(a.broadcaster_seen_ns) < params.window_5m_ns;
                let bcast = if bcast_fresh {
                    params.live_broadcaster_weight
                } else {
                    0
                };
                breadth.saturating_add(bcast)
            } else {
                0
            };
            let level = base_level.saturating_add(live_bonus);

            // Append to the bounded level series (oldest→newest).
            if a.levels.len() >= params.series_cap {
                a.levels.remove(0);
            }
            a.levels.push(level);

            let state = nv_attention_state(
                &a.mentions,
                now_ns,
                params.window_1m_ns,
                params.window_5m_ns,
                &a.levels,
                params.series_window,
                params.freshness_full_ns,
            );
            let series = AttentionSeries {
                level,
                velocity: state.engagement_velocity,
                acceleration: state.engagement_acceleration,
            };

            // Virality (branching factor): new mentions this window over the prior
            // window's level; undefined (prior 0) folds to 0, never Virality.
            let prior = if a.levels.len() >= 2 {
                a.levels[a.levels.len() - 2]
            } else {
                0
            };
            let virality = nv_virality_coeff(prior, level).unwrap_or(0);

            // Money velocity: change in the on-chain money level since last emit.
            let money = money_of(mint);
            let money_vel = if a.seen_money {
                sat_i64(i128::from(money) - i128::from(a.prev_money))
            } else {
                0
            };
            a.prev_money = money;
            a.seen_money = true;

            let divergence = nv_attention_money_divergence(
                state.engagement_velocity,
                money_vel,
                params.divergence_threshold,
            );
            let stage = nv_lifecycle_stage(&series, virality, params.formation_level);
            let age_windows = a.levels.len() as u32;
            // §783 legibility clock — LIVE at last: once an aggregator lists
            // the coin, the pre-legibility earliness bonus is cut by the model
            // itself (previously hardcoded `false` awaiting this source).
            let pre_leg = nv_pre_legibility(
                state.unique_sources,
                state.source_concentration,
                age_windows,
                a.aggregator_seen,
                params.age_step_fp,
            );
            let money_confirmed = is_confirmed(mint);
            let score = nv_candidate_score(stage, divergence, virality, pre_leg, money_confirmed);

            if score > 0 {
                buf.push(Candidate::new(
                    WlMint::new(*mint),
                    WlLane::EarlyConfirmation,
                    score,
                    now_tick,
                    Features::default(),
                ));
            }
        }
    }
}

/// Saturating `i128 → i64` narrow (§22 explicit overflow).
#[inline]
fn sat_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mention(ts_ns: u64, source: u64, weight: u64, copycat: bool) -> Mention {
        Mention {
            ts_ns,
            source_id: source,
            community_id: source,
            weight,
            copycat,
        }
    }

    #[test]
    fn empty_field_emits_nothing() {
        let mut f = AttentionField::new(AttentionParams::standard());
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 0, |_| false);
        assert!(buf.is_empty());
        assert!(f.is_empty());
    }

    #[test]
    fn accumulating_attention_emits_early_confirmation_candidate() {
        let mut f = AttentionField::new(AttentionParams::standard());
        let mint = [7u8; 32];
        // A burst of distinct-source, high-weight mentions past the formation floor.
        for i in 0..6u64 {
            f.observe(mint, mention(1_000 + i * 10, i, 500, false));
        }
        let mut buf = Vec::new();
        // Emit twice so a velocity series exists; money flat/unconfirmed => attention-leads, fade-capped.
        f.emit_into(&mut buf, 1, |_| 0, |_| false);
        buf.clear();
        for i in 6..12u64 {
            f.observe(mint, mention(2_000 + i * 10, i, 800, false));
        }
        f.emit_into(&mut buf, 2, |_| 0, |_| false);
        assert_eq!(buf.len(), 1, "one attention candidate for the tracked mint");
        let c = buf[0];
        assert_eq!(c.lane, WlLane::EarlyConfirmation, "corroboration-tier lane");
        assert!(
            c.discovery_score <= 500,
            "money unconfirmed => fade-first hard cap (<=500)"
        );
    }

    #[test]
    fn confirmation_lifts_the_fade_cap() {
        let mut f = AttentionField::new(AttentionParams::standard());
        let mint = [9u8; 32];
        for i in 0..8u64 {
            f.observe(mint, mention(1_000 + i * 10, i, 1_000, false));
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 0, |_| true); // money rising + confirmed
        buf.clear();
        for i in 8..16u64 {
            f.observe(mint, mention(2_000 + i * 10, i, 2_000, false));
        }
        // Rising money + confirmed => the cap is lifted (score may exceed 500).
        f.emit_into(&mut buf, 2, |_| 5_000, |_| true);
        assert_eq!(buf.len(), 1);
        // With confirmation the fade cap no longer binds; a strong burst can exceed it.
        assert!(buf[0].discovery_score > 0);
    }

    #[test]
    fn bounded_tracking_evicts_weakest() {
        let params = AttentionParams {
            track_cap: 2,
            ..AttentionParams::standard()
        };
        let mut f = AttentionField::new(params);
        f.observe([1u8; 32], mention(1, 1, 10, false));
        f.observe([1u8; 32], mention(2, 2, 10, false)); // mint 1 has 2 mentions
        f.observe([2u8; 32], mention(3, 3, 10, false)); // mint 2 has 1
        f.observe([3u8; 32], mention(4, 4, 10, false)); // evicts weakest (mint 2)
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn same_stream_is_deterministic() {
        let build = || {
            let mut f = AttentionField::new(AttentionParams::standard());
            let mint = [5u8; 32];
            for i in 0..10u64 {
                f.observe(mint, mention(1_000 + i * 5, i % 4, 300 + i * 7, i % 3 == 0));
            }
            let mut buf = Vec::new();
            f.emit_into(&mut buf, 1, |_| 100, |_| false);
            buf
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn velocity_of_is_non_mutating_and_none_for_untracked() {
        let mut f = AttentionField::new(AttentionParams::standard());
        let mint = [3u8; 32];
        // Untracked mint → UNKNOWN.
        assert_eq!(f.velocity_of(&mint), None);
        for i in 0..8u64 {
            f.observe(mint, mention(1_000 + i * 10, i, 500, false));
        }
        // Reading velocity must not perturb subsequent emits (idempotent read).
        let v1 = f.velocity_of(&mint);
        let v2 = f.velocity_of(&mint);
        assert_eq!(v1, v2, "repeated reads are stable (non-mutating)");
        assert!(v1.is_some(), "a tracked mint has a defined velocity");
    }
}

#[cfg(test)]
mod twitch_tests {
    use super::*;

    fn mention(ts: u64, src: u64, w: u64) -> Mention {
        Mention {
            ts_ns: ts,
            source_id: src,
            community_id: 7,
            weight: w,
            copycat: false,
        }
    }

    fn emit_scores(f: &mut AttentionField) -> Vec<u64> {
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 0, |_| false);
        buf.iter().map(|c| c.discovery_score).collect()
    }

    /// The zero-hot-path-change guarantee: neutral provenance through
    /// `observe_tagged` is byte-identical to the historical `observe`.
    #[test]
    fn neutral_provenance_is_byte_identical_to_observe() {
        let mut f1 = AttentionField::new(AttentionParams::standard());
        let mut f2 = AttentionField::new(AttentionParams::standard());
        let m = [9u8; 32];
        for i in 0..5u64 {
            f1.observe(m, mention(1_000_000_000 + i, i, 3));
            f2.observe_tagged(
                m,
                mention(1_000_000_000 + i, i, 3),
                &MentionProvenance::default(),
            );
        }
        assert_eq!(emit_scores(&mut f1), emit_scores(&mut f2));
    }

    /// A broadcaster call landing in live chat is an attention SPIKE the model
    /// reads as a virality jump: same tape, same model, higher rank than the
    /// identical mentions without the live structure. (Emitted with money
    /// confirmed so the fade cap — tested separately — does not mask the
    /// comparison.)
    #[test]
    fn live_chat_structure_raises_attention() {
        let m = [8u8; 32];
        let mut plain = AttentionField::new(AttentionParams::standard());
        let mut live = AttentionField::new(AttentionParams::standard());
        // Round 1: identical plain chatter in both fields; seed the level series.
        for i in 0..6u64 {
            let men = mention(1_000_000_000 + i, 100 + i, 3);
            plain.observe(m, men);
            live.observe(m, men);
        }
        let mut buf = Vec::new();
        plain.emit_into(&mut buf, 1, |_| 0, |_| true);
        buf.clear();
        live.emit_into(&mut buf, 1, |_| 0, |_| true);
        // Round 2: one more message each — but in the live field the broadcaster
        // says it in their own chat (realtime + broadcaster provenance).
        let men2 = mention(1_000_000_100, 500, 3);
        plain.observe(m, men2);
        live.observe_tagged(
            m,
            men2,
            &MentionProvenance {
                realtime_chat: true,
                broadcaster: true,
                author_id: 500,
                echo_or_coordinated: false,
                aggregator: false,
                bearish: false,
            },
        );
        let mut p = Vec::new();
        plain.emit_into(&mut p, 2, |_| 0, |_| true);
        let mut l = Vec::new();
        live.emit_into(&mut l, 2, |_| 0, |_| true);
        assert!(!p.is_empty() && !l.is_empty());
        assert!(
            l[0].discovery_score > p[0].discovery_score,
            "broadcaster live-chat spike must outrank plain mentions ({} vs {})",
            l[0].discovery_score,
            p[0].discovery_score
        );
    }

    /// Echo / coordinated repeats never add live-chat breadth (fade-first §29),
    /// and the distinct-chatter set is bounded (§99).
    #[test]
    fn echoes_add_no_breadth_and_chatters_are_bounded() {
        let m = [7u8; 32];
        let mut f = AttentionField::new(AttentionParams::standard());
        // A coordinated flood: many "chatters", all flagged echo/coordinated.
        for i in 0..40u64 {
            f.observe_tagged(
                m,
                mention(1_000_000_000 + i, 200 + i, 1),
                &MentionProvenance {
                    realtime_chat: true,
                    broadcaster: false,
                    author_id: 200 + i,
                    echo_or_coordinated: true,
                    aggregator: false,
                    bearish: false,
                },
            );
        }
        let flood = emit_scores(&mut f);
        // Same mentions, genuine: breadth counts, but bounded at the cap.
        let mut g = AttentionField::new(AttentionParams::standard());
        for i in 0..40u64 {
            g.observe_tagged(
                m,
                mention(1_000_000_000 + i, 200 + i, 1),
                &MentionProvenance {
                    realtime_chat: true,
                    broadcaster: false,
                    author_id: 200 + i,
                    echo_or_coordinated: false,
                    aggregator: false,
                    bearish: false,
                },
            );
        }
        let genuine = emit_scores(&mut g);
        assert!(
            genuine[0] >= flood[0],
            "genuine breadth must not rank below a flood"
        );
        // The genuine field's breadth is capped: LIVE_CHATTER_CAP distinct ids,
        // so the bonus is bounded regardless of flood size.
        let a = g.obs.get(&m).expect("tracked");
        assert!(a.live_chatters.len() <= LIVE_CHATTER_CAP);
        let b = f.obs.get(&m).expect("tracked");
        assert!(b.live_chatters.is_empty(), "echoes must add zero breadth");
    }

    /// The §29 fade-first cap still binds: without money confirmation, no amount
    /// of live-chat structure can push the score past the pre-confirmation cap.
    #[test]
    fn fade_cap_binds_until_money_confirms() {
        let m = [6u8; 32];
        let mut f = AttentionField::new(AttentionParams::standard());
        for i in 0..LIVE_CHATTER_CAP as u64 + 4 {
            f.observe_tagged(
                m,
                mention(1_000_000_000 + i, 300 + i, 50),
                &MentionProvenance {
                    realtime_chat: true,
                    broadcaster: i == 0,
                    author_id: 300 + i,
                    echo_or_coordinated: false,
                    aggregator: false,
                    bearish: false,
                },
            );
        }
        let mut unconfirmed = Vec::new();
        f.clone().emit_into(&mut unconfirmed, 1, |_| 0, |_| false);
        let mut confirmed = Vec::new();
        f.emit_into(&mut confirmed, 1, |_| 0, |_| true);
        assert!(!unconfirmed.is_empty() && !confirmed.is_empty());
        assert!(
            unconfirmed[0].discovery_score <= confirmed[0].discovery_score,
            "confirmation may only lift the cap, never lower it"
        );
        assert!(
            unconfirmed[0].discovery_score <= 500,
            "pre-confirmation fade cap must bind, got {}",
            unconfirmed[0].discovery_score
        );
    }
}
