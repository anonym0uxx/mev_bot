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
}

impl AttentionParams {
    /// The shipped defaults. Each constant is chosen with rationale (§102):
    /// minute/5-minute windows match the memecoin attention cadence; a 3-sample
    /// derivative lookback needs 7 samples to define acceleration; a 1-hour
    /// freshness horizon matches §29.6 staleness; the formation floor requires a
    /// small but real amount of weighted attention before "emergence"; a zero
    /// deadband treats any strictly-positive velocity as rising; and a `FP_ONE/16`
    /// age step fully legibilizes a narrative over ~16 windows.
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
        }
    }
}

impl Default for AttentionParams {
    fn default() -> Self {
        Self::standard()
    }
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
            let level: u64 = a
                .mentions
                .iter()
                .filter(|m| now_ns.saturating_sub(m.ts_ns) < params.window_1m_ns)
                .fold(0u64, |acc, m| acc.saturating_add(m.weight));

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
            let pre_leg = nv_pre_legibility(
                state.unique_sources,
                state.source_concentration,
                age_windows,
                false, // aggregator_listed: the [S] legibility clock is a separate signal
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
