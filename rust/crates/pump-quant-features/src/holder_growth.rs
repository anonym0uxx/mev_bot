//! §70.1 **holder-growth acceleration** — the second derivative of a mint's
//! holder count, in basis points, computed point-in-time-safely.
//!
//! ## Why this exists
//! Holder growth is one of the §70.1 "money" leading indicators: accumulation
//! broadens (more distinct holders) *before* price confirms it. The first
//! difference of the holder series is a growth **rate**; the second difference
//! is a growth **acceleration** — the quantity that turns positive while the
//! rate is still small, which is the whole point of a leading indicator.
//!
//! ## §6.4 unknown-stays-unknown
//! Every estimator here returns [`Option`]. A `0` returned as "no data" is
//! indistinguishable from a `0` measured as "genuinely flat", and that
//! ambiguity is exactly what the UNKNOWN discipline exists to prevent. This
//! module therefore refuses — with `None` — whenever:
//!
//! * fewer than [`HOLDER_MIN_SAMPLES_FOR_ACCEL`] usable samples exist at or
//!   before the decision cutoff;
//! * the two comparison intervals cannot be spaced at least
//!   [`HolderGrowthConfig::min_interval_ns`] apart (sub-interval sampling
//!   amplifies quantization noise into a fabricated acceleration);
//! * either interval is longer than [`HolderGrowthConfig::max_interval_ns`]
//!   (the series is stale — a gap that long is not a measurement);
//! * a base holder count is zero (a relative growth rate off a zero base is
//!   undefined, not infinite).
//!
//! ## §20 point-in-time safety
//! [`HolderSeries::estimate_as_of`] selects samples strictly by
//! `ts_ns <= as_of_ns`. A sample that arrives later can never change an earlier
//! estimate, and pushes are rejected if they would move information time
//! backwards ([`crate::types::FeatureError::NonMonotonicTimestamp`]).
//!
//! ## §22 / §99 arithmetic and memory law
//! All math is integer; intermediates widen to `i128` and narrow by explicit
//! clamping. State is a fixed-size ring per mint
//! ([`HOLDER_SERIES_CAP`] samples, oldest-evicted) inside a capacity-bounded
//! tracker ([`HolderGrowthTracker`]) with documented least-recently-updated
//! eviction. Nothing here allocates on the estimate path, reads a clock, or
//! touches floating point.

use crate::types::FeatureError;

/// Stable upstream-assigned mint handle. This module only ever compares
/// handles, so the mapping never affects an outcome (§22 determinism).
pub type MintKey = u64;

/// Basis-point denominator (100% = 10 000 bps) (§22).
const BPS_DENOM: i128 = 10_000;

/// Ring capacity of one mint's holder-count series (§99/§57 memory law).
///
/// Thirty-two samples is enough to reach back over a full decimation ladder at
/// the default one-minute normalization while keeping the per-mint footprint at
/// a fixed 512 bytes. The oldest sample is evicted on overflow; the count of
/// evictions is retained ([`HolderSeries::dropped`]) so a consumer can tell a
/// short series from a truncated one.
pub const HOLDER_SERIES_CAP: usize = 32;

/// Minimum usable samples before an acceleration may be reported (§6.4).
///
/// A second difference needs three points by construction: two first
/// differences, one difference between them. Below this the estimator returns
/// `None` rather than a fabricated neutral value.
pub const HOLDER_MIN_SAMPLES_FOR_ACCEL: usize = 3;

/// Default minimum spacing between the three comparison points (§20/§70.1).
///
/// One second. Holder counts are integers; two samples taken microseconds apart
/// differ by at most a handful of holders, so the implied per-minute rate is
/// dominated by quantization. Refusing sub-interval spacing keeps the reported
/// acceleration a measurement rather than an artifact of the sampling cadence.
pub const HOLDER_MIN_INTERVAL_NS: u64 = 1_000_000_000;

/// Default maximum spacing between comparison points (§20 staleness).
///
/// One hour. A longer gap means the series was not being observed, and a
/// "growth rate" spanning an unobserved hour is not point-in-time information
/// about the decision instant. Refused with `None`.
pub const HOLDER_MAX_INTERVAL_NS: u64 = 3_600_000_000_000;

/// Time basis the growth rates are normalized to (§70.1).
///
/// Sixty seconds. Both first differences are expressed as "bps of holder growth
/// per minute" so that irregularly spaced samples remain comparable and their
/// difference (the acceleration) is meaningful.
pub const HOLDER_GROWTH_NORM_NS: u64 = 60_000_000_000;

/// Default number of distinct mints the tracker follows (§99/§57).
///
/// Five hundred and twelve live mints at [`HOLDER_SERIES_CAP`] samples each is
/// a fixed 256 KiB ceiling. A new mint arriving at capacity evicts the
/// least-recently-updated series (see [`HolderGrowthTracker::push`]).
pub const HOLDER_TRACKER_CAP: usize = 512;

/// One observed holder-count reading at a known information time (§20).
///
/// `holder_count` is the number of distinct addresses holding a non-zero
/// balance of the mint as observed at `ts_ns`. `ts_ns` is *information* time —
/// when the fact became knowable — never a wall-clock read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HolderSample {
    /// Information time of the observation, nanoseconds.
    pub ts_ns: u64,
    /// Distinct holders observed at `ts_ns`.
    pub holder_count: u64,
}

/// Named, versioned gates for the acceleration estimator (§102: no magic
/// numbers in a decision path — every comparison reads a field here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HolderGrowthConfig {
    /// Minimum spacing between consecutive comparison points, nanoseconds.
    pub min_interval_ns: u64,
    /// Maximum spacing between consecutive comparison points, nanoseconds.
    pub max_interval_ns: u64,
    /// Time basis both growth rates are normalized to, nanoseconds.
    pub norm_ns: u64,
}

impl HolderGrowthConfig {
    /// The named-const default: 1 s minimum spacing, 1 h staleness ceiling,
    /// per-minute normalization.
    pub const DEFAULT: Self = HolderGrowthConfig {
        min_interval_ns: HOLDER_MIN_INTERVAL_NS,
        max_interval_ns: HOLDER_MAX_INTERVAL_NS,
        norm_ns: HOLDER_GROWTH_NORM_NS,
    };

    /// Whether this configuration has well-defined semantics: a positive
    /// normalization basis and a maximum spacing not below the minimum. An
    /// invalid configuration makes every estimate `None` rather than producing
    /// a value from nonsense bounds (§6.4).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.norm_ns > 0 && self.max_interval_ns >= self.min_interval_ns
    }
}

impl Default for HolderGrowthConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A point-in-time holder-growth acceleration estimate (§70.1).
///
/// `accel_bps = growth_bps - prior_growth_bps`, where each growth term is the
/// relative change in holder count over its interval, normalized to
/// [`HolderGrowthConfig::norm_ns`]. Positive acceleration means accumulation is
/// *broadening faster* than it was over the preceding interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HolderGrowthEstimate {
    /// Second difference: `growth_bps - prior_growth_bps`, signed bps per
    /// normalization window. The §70.1 leading indicator.
    pub accel_bps: i64,
    /// First difference over the most recent interval (`mid` → `newest`),
    /// signed bps per normalization window.
    pub growth_bps: i64,
    /// First difference over the preceding interval (`oldest` → `mid`), signed
    /// bps per normalization window.
    pub prior_growth_bps: i64,
    /// Oldest of the three comparison points actually used.
    pub oldest: HolderSample,
    /// Middle comparison point actually used.
    pub mid: HolderSample,
    /// Newest comparison point actually used (its `ts_ns <= as_of_ns`).
    pub newest: HolderSample,
}

impl HolderGrowthEstimate {
    /// Total information-time span the estimate was computed over,
    /// `newest.ts_ns - oldest.ts_ns`. Always positive by construction.
    #[must_use]
    pub fn span_ns(&self) -> u64 {
        self.newest.ts_ns.saturating_sub(self.oldest.ts_ns)
    }
}

/// Clamp an `i128` into `i64` without panicking (§22 explicit narrowing).
fn clamp_i64(v: i128) -> i64 {
    if v > i128::from(i64::MAX) {
        i64::MAX
    } else if v < i128::from(i64::MIN) {
        i64::MIN
    } else {
        v as i64
    }
}

/// Relative growth from `from` to `to`, in signed bps per `norm_ns`.
///
/// `((to.h - from.h) * 10_000 * norm_ns) / (from.h * dt)`, evaluated entirely in
/// `i128` so no intermediate can overflow, then clamped into `i64`. Returns
/// `None` when the base count is zero (an undefined relative rate, §6.4) or the
/// interval is non-positive.
fn rate_bps(from: HolderSample, to: HolderSample, norm_ns: u64) -> Option<i64> {
    let base = i128::from(from.holder_count);
    if base == 0 {
        return None;
    }
    let dt = i128::from(to.ts_ns.checked_sub(from.ts_ns)?);
    if dt == 0 {
        return None;
    }
    let delta = i128::from(to.holder_count).checked_sub(base)?;
    let num = delta
        .checked_mul(BPS_DENOM)?
        .checked_mul(i128::from(norm_ns))?;
    let den = base.checked_mul(dt)?;
    Some(clamp_i64(num / den))
}

/// A fixed-capacity, monotonically-timestamped holder-count series for one mint
/// (§99 bounded state, §20 point-in-time).
///
/// Samples are held in a fixed-size ring; the oldest is evicted on overflow and
/// counted in [`Self::dropped`]. Push order must be non-decreasing in `ts_ns`
/// so that newest-first traversal is also newest-first in information time —
/// this is what makes `as_of` a bounded backward scan rather than a sort.
#[derive(Debug, Clone)]
pub struct HolderSeries {
    /// Ring storage; `start` indexes the oldest live sample.
    buf: [HolderSample; HOLDER_SERIES_CAP],
    start: usize,
    len: usize,
    last_ts_ns: u64,
    dropped: u64,
}

impl Default for HolderSeries {
    fn default() -> Self {
        Self::new()
    }
}

impl HolderSeries {
    /// Create an empty series.
    #[must_use]
    pub const fn new() -> Self {
        HolderSeries {
            buf: [HolderSample {
                ts_ns: 0,
                holder_count: 0,
            }; HOLDER_SERIES_CAP],
            start: 0,
            len: 0,
            last_ts_ns: 0,
            dropped: 0,
        }
    }

    /// Number of retained samples (at most [`HOLDER_SERIES_CAP`]).
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the series holds no samples.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fixed ring capacity (§99).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        HOLDER_SERIES_CAP
    }

    /// Count of samples evicted by the capacity bound. Non-zero means an
    /// `as_of` query far enough in the past can no longer be answered from this
    /// series — it will return `None`, never a value reconstructed from the
    /// samples that happen to remain.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Information time of the newest retained sample (`0` when empty).
    #[must_use]
    pub const fn last_ts_ns(&self) -> u64 {
        self.last_ts_ns
    }

    /// Append one observation.
    ///
    /// Rejects a sample whose `ts_ns` is strictly before the previous sample's
    /// (§20: information time never moves backwards). Equal timestamps are
    /// accepted — two observations can share an instant — and are simply never
    /// selected as a comparison pair, because the minimum-spacing gate excludes
    /// a zero interval.
    pub fn push(&mut self, sample: HolderSample) -> Result<(), FeatureError> {
        if self.len > 0 && sample.ts_ns < self.last_ts_ns {
            return Err(FeatureError::NonMonotonicTimestamp {
                previous_ns: self.last_ts_ns,
                offending_ns: sample.ts_ns,
            });
        }
        if self.len < HOLDER_SERIES_CAP {
            let idx = (self.start + self.len) % HOLDER_SERIES_CAP;
            if let Some(slot) = self.buf.get_mut(idx) {
                *slot = sample;
                self.len += 1;
            }
        } else {
            let idx = self.start;
            if let Some(slot) = self.buf.get_mut(idx) {
                *slot = sample;
                self.start = (self.start + 1) % HOLDER_SERIES_CAP;
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.last_ts_ns = sample.ts_ns;
        Ok(())
    }

    /// Sample at newest-first offset `i` (`0` == newest). `None` past the end.
    #[must_use]
    pub fn at_rev(&self, i: usize) -> Option<HolderSample> {
        if i >= self.len {
            return None;
        }
        // start + len - 1 - i, kept non-negative because i < len.
        let idx = (self.start + self.len - 1 - i) % HOLDER_SERIES_CAP;
        self.buf.get(idx).copied()
    }

    /// Newest sample at or before `as_of_ns` and its newest-first offset.
    fn newest_at_or_before(&self, as_of_ns: u64, from_rev: usize) -> Option<(usize, HolderSample)> {
        let mut i = from_rev;
        while i < self.len {
            let s = self.at_rev(i)?;
            if s.ts_ns <= as_of_ns {
                return Some((i, s));
            }
            i += 1;
        }
        None
    }

    /// The §70.1 holder-growth acceleration as known at `as_of_ns` (§20).
    ///
    /// Selection is a decimation, newest-first, strictly over samples with
    /// `ts_ns <= as_of_ns`:
    ///
    /// 1. `newest` = newest sample at or before the cutoff;
    /// 2. `mid` = newest sample at least `min_interval_ns` older than `newest`;
    /// 3. `oldest` = newest sample at least `min_interval_ns` older than `mid`.
    ///
    /// Both resulting intervals are then checked against `max_interval_ns`.
    /// The two first differences are normalized to `norm_ns` and subtracted.
    ///
    /// Returns `None` — never a fabricated `0` — when the configuration is
    /// invalid, fewer than [`HOLDER_MIN_SAMPLES_FOR_ACCEL`] samples are usable,
    /// the decimation cannot be satisfied, an interval exceeds the staleness
    /// ceiling, or a base holder count is zero (§6.4).
    #[must_use]
    pub fn estimate_as_of(
        &self,
        as_of_ns: u64,
        cfg: &HolderGrowthConfig,
    ) -> Option<HolderGrowthEstimate> {
        if !cfg.is_valid() || self.len < HOLDER_MIN_SAMPLES_FOR_ACCEL {
            return None;
        }

        let (i2, newest) = self.newest_at_or_before(as_of_ns, 0)?;
        let mid_cutoff = newest.ts_ns.checked_sub(cfg.min_interval_ns)?;
        let (i1, mid) = self.newest_at_or_before(mid_cutoff, i2 + 1)?;
        let old_cutoff = mid.ts_ns.checked_sub(cfg.min_interval_ns)?;
        let (_i0, oldest) = self.newest_at_or_before(old_cutoff, i1 + 1)?;

        // §20 staleness: an interval longer than the ceiling is an unobserved
        // gap, not a measured rate.
        let recent_dt = newest.ts_ns.checked_sub(mid.ts_ns)?;
        let prior_dt = mid.ts_ns.checked_sub(oldest.ts_ns)?;
        if recent_dt > cfg.max_interval_ns || prior_dt > cfg.max_interval_ns {
            return None;
        }

        let prior_growth_bps = rate_bps(oldest, mid, cfg.norm_ns)?;
        let growth_bps = rate_bps(mid, newest, cfg.norm_ns)?;

        Some(HolderGrowthEstimate {
            accel_bps: growth_bps.saturating_sub(prior_growth_bps),
            growth_bps,
            prior_growth_bps,
            oldest,
            mid,
            newest,
        })
    }
}

/// A capacity-bounded, per-mint holder-growth tracker (§99/§57).
///
/// Entries are kept sorted by [`MintKey`] so lookup is a binary search and
/// iteration is deterministic. When a *new* mint arrives while the tracker is
/// at capacity, the series with the oldest [`HolderSeries::last_ts_ns`] is
/// evicted (ties broken by the smaller key, so eviction is a pure function of
/// state — no clock, no insertion-order dependence); the eviction is counted in
/// [`Self::evictions`]. An already-tracked mint is never evicted by its own
/// push, so a live mint never loses fidelity.
#[derive(Debug, Clone)]
pub struct HolderGrowthTracker {
    entries: Vec<(MintKey, HolderSeries)>,
    capacity: usize,
    evictions: u64,
}

impl HolderGrowthTracker {
    /// Create a tracker following at most `capacity` mints. `capacity` is
    /// clamped to at least 1 so the structure can always hold the live mint.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        HolderGrowthTracker {
            entries: Vec::new(),
            capacity: capacity.max(1),
            evictions: 0,
        }
    }

    /// Number of tracked mints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no mint is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Configured mint capacity (§99).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Count of series evicted by the capacity bound.
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Immutable view of one mint's series, if tracked.
    #[must_use]
    pub fn series(&self, mint: MintKey) -> Option<&HolderSeries> {
        match self.entries.binary_search_by_key(&mint, |(k, _)| *k) {
            Ok(pos) => self.entries.get(pos).map(|(_, s)| s),
            Err(_) => None,
        }
    }

    /// Record one holder observation for `mint`.
    ///
    /// Returns the same [`FeatureError::NonMonotonicTimestamp`] as
    /// [`HolderSeries::push`] when information time would move backwards for
    /// that mint. Admitting a new mint may evict the least-recently-updated
    /// series (see the type docs).
    pub fn push(&mut self, mint: MintKey, sample: HolderSample) -> Result<(), FeatureError> {
        match self.entries.binary_search_by_key(&mint, |(k, _)| *k) {
            Ok(pos) => match self.entries.get_mut(pos) {
                Some((_, series)) => series.push(sample),
                None => Ok(()),
            },
            Err(pos) => {
                let mut pos = pos;
                if self.entries.len() >= self.capacity {
                    if let Some(victim) = self.evict_index() {
                        self.entries.remove(victim);
                        self.evictions = self.evictions.saturating_add(1);
                        if victim < pos {
                            pos -= 1;
                        }
                    }
                }
                if self.entries.len() >= self.capacity {
                    // Capacity is >= 1 and a victim is always available above,
                    // so this is unreachable in practice; refusing the insert
                    // keeps the bound absolute rather than best-effort.
                    return Ok(());
                }
                let mut series = HolderSeries::new();
                let r = series.push(sample);
                self.entries.insert(pos, (mint, series));
                r
            }
        }
    }

    /// Index of the eviction victim: oldest `last_ts_ns`, ties by smaller key.
    fn evict_index(&self) -> Option<usize> {
        let mut best: Option<(usize, u64, MintKey)> = None;
        for (i, (key, series)) in self.entries.iter().enumerate() {
            let ts = series.last_ts_ns();
            let replace = match best {
                None => true,
                Some((_, best_ts, best_key)) => ts < best_ts || (ts == best_ts && *key < best_key),
            };
            if replace {
                best = Some((i, ts, *key));
            }
        }
        best.map(|(i, _, _)| i)
    }

    /// Point-in-time holder-growth acceleration for `mint` at `as_of_ns`.
    ///
    /// `None` for an untracked mint or whenever
    /// [`HolderSeries::estimate_as_of`] refuses (§6.4 — absence is reported,
    /// never neutralized to zero).
    #[must_use]
    pub fn estimate_as_of(
        &self,
        mint: MintKey,
        as_of_ns: u64,
        cfg: &HolderGrowthConfig,
    ) -> Option<HolderGrowthEstimate> {
        self.series(mint)?.estimate_as_of(as_of_ns, cfg)
    }
}

impl Default for HolderGrowthTracker {
    fn default() -> Self {
        Self::with_capacity(HOLDER_TRACKER_CAP)
    }
}
