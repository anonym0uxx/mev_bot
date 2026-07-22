//! `wl_rank` leaf — deterministic fixed-point ranking.
//!
//! Responsibility: turn a [`Candidate`] plus the current logical time into a
//! single comparable `u64` rank, as
//! `discovery_score × recency_factor × per-lane weight`. All integer /
//! fixed-point (§22): intermediate products use `u128` and the result saturates
//! into `u64` (a monotonic, safe-by-contract saturation, documented below).
//! No floats, no clocks, no RNG — pure function of its inputs.
//!
//! Constitution: §22 (fixed-point, deterministic), §102 (every scale is a named
//! const with rationale).

use crate::candidate::{Candidate, Lane};

/// Fixed-point scale for the recency factor: `RECENCY_ONE` represents 1.0×
/// (a brand-new candidate, zero decay). A candidate at or past its TTL decays
/// to 0. Chosen at 1e6 so linear decay has micro-unit resolution. §102.
pub const RECENCY_ONE: u64 = 1_000_000;

/// Fixed-point scale for per-lane weights, in basis points: `WEIGHT_ONE`
/// (10_000) represents a 1.0× weight. §102.
pub const WEIGHT_ONE: u32 = 10_000;

/// Per-lane ranking weights in basis points ([`WEIGHT_ONE`] = 1.0×).
///
/// Responsibility: hold the per-lane multiplier applied by [`score_rank`],
/// seeded from the lane priors ([`Lane::default_weight_bp`]) and overridable so
/// realized lane performance (`wl_lane_performance`) can feed back into ranking.
/// Fixed-size array — bounded (§99).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LaneWeights {
    weight_bp: [u32; Lane::COUNT],
}

impl Default for LaneWeights {
    fn default() -> Self {
        Self::from_defaults()
    }
}

impl LaneWeights {
    /// Seed weights from each lane's static-by-design prior. §102.
    #[must_use]
    pub fn from_defaults() -> Self {
        let mut weight_bp = [WEIGHT_ONE; Lane::COUNT];
        for lane in Lane::ALL {
            weight_bp[lane.index()] = lane.default_weight_bp();
        }
        Self { weight_bp }
    }

    /// The weight for a lane, in basis points.
    #[must_use]
    pub fn get(&self, lane: Lane) -> u32 {
        self.weight_bp[lane.index()]
    }

    /// Override a lane's weight (e.g. from realized net-SOL). Deterministic. §22.
    pub fn set(&mut self, lane: Lane, weight_bp: u32) {
        self.weight_bp[lane.index()] = weight_bp;
    }
}

/// Parameters governing recency decay.
///
/// Responsibility: the single knob ranking needs beyond lane weights — the TTL
/// horizon over which a candidate's recency factor decays linearly to zero.
/// Named, not magic (§102).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RankParams {
    /// Time-to-live in logical ticks. At age 0 the recency factor is
    /// [`RECENCY_ONE`]; at age ≥ `ttl_ticks` it is 0. Must be > 0 for any
    /// candidate to retain positive recency.
    pub ttl_ticks: u64,
}

impl RankParams {
    /// Construct rank parameters with an explicit TTL horizon.
    #[must_use]
    pub const fn new(ttl_ticks: u64) -> Self {
        Self { ttl_ticks }
    }
}

/// Linear recency decay factor in `[0, RECENCY_ONE]`.
///
/// Formula: `RECENCY_ONE × (ttl - age) / ttl`, clamped to 0 at/after TTL.
/// `age = now.saturating_sub(discovered_at)` (a candidate from the future — a
/// caller bug — is treated as brand-new, age 0, never as negative). Monotonic
/// non-increasing in `age`. `ttl == 0` disables the lane (returns 0). §22.
#[must_use]
pub fn recency_factor(discovered_at: u64, now: u64, ttl_ticks: u64) -> u64 {
    if ttl_ticks == 0 {
        return 0;
    }
    let age = now.saturating_sub(discovered_at);
    if age >= ttl_ticks {
        return 0;
    }
    // (ttl - age) is in 1..=ttl, so the product fits u128 and the quotient
    // fits u64 (≤ RECENCY_ONE). Exact integer division.
    let remaining = ttl_ticks - age;
    ((u128::from(RECENCY_ONE) * u128::from(remaining)) / u128::from(ttl_ticks)) as u64
}

/// Compose a candidate's rank: `discovery_score × recency × lane weight`.
///
/// Fixed-point pipeline (§22):
/// `((score × recency / RECENCY_ONE) × weight_bp / WEIGHT_ONE)`, computed in
/// `u128` then saturated into `u64`. Saturation is safe-by-contract: rank is
/// used only for *ordering*, and the saturation ceiling (`u64::MAX`) is
/// monotonic — two candidates that would both exceed it compare equal at the
/// ceiling, which cannot mis-order a smaller candidate above a larger one.
/// A candidate whose recency has decayed to 0 ranks 0.
#[must_use]
pub fn score_rank(
    candidate: &Candidate,
    now: u64,
    params: RankParams,
    weights: &LaneWeights,
) -> u64 {
    let recency = recency_factor(candidate.discovered_at, now, params.ttl_ticks);
    if recency == 0 {
        return 0;
    }
    let weight_bp = weights.get(candidate.lane);
    let after_recency =
        (u128::from(candidate.discovery_score) * u128::from(recency)) / u128::from(RECENCY_ONE);
    let after_weight = (after_recency * u128::from(weight_bp)) / u128::from(WEIGHT_ONE);
    // Saturate into u64 for a compact, comparable rank.
    if after_weight > u128::from(u64::MAX) {
        u64::MAX
    } else {
        after_weight as u64
    }
}
