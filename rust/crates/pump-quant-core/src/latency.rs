//! latency — deterministic fixed-bucket latency percentile estimator (§22, criterion 20).
//!
//! Responsibility: accumulate observed latencies (in nanoseconds) into a
//! fixed-size log-linear histogram and answer nearest-rank quantile queries
//! (p50 / p95 / p99 / p99.9 and any point in between) without ever touching a
//! clock, a random source, a float, or the allocator. This is the pure
//! primitive that criterion 20 ("p50/p95/p99/p99.9 latency measured") requires
//! and that the criterion-103/109 latency budgets consume. Live capture of the
//! nanosecond samples is a server concern and is out of scope here — this module
//! only *stores* and *reads* samples that are handed to [`LatencyHistogram::record`].
//!
//! Constitutional discipline observed here (§22):
//! * No `f32`/`f64` anywhere — every value is an integer; quantile fractions are
//!   expressed as integers scaled by [`QUANTILE_SCALE`].
//! * No allocation after construction — the backing store is a fixed
//!   `[u64; NUM_BUCKETS]` array, so a histogram is a plain value type.
//! * Explicit overflow — every counter update is `saturating_add`; every bound
//!   computation is proven to fit `u64` and documented where it approaches the
//!   top of the range.
//! * Deterministic — identical `record` sequences always yield identical
//!   quantiles; there is no wall-clock, RNG, or platform dependence.
//!
//! # Bucket layout
//!
//! The layout is HDR-histogram style: a linear low region followed by
//! per-octave sub-buckets. With [`SUB_BITS`] = 6 there are [`SUB_COUNT`] = 64
//! sub-buckets per octave.
//!
//! * Values `0 .. SUB_COUNT` each occupy their own bucket (exact, unit width).
//! * Larger values are split by their most-significant-bit octave; within an
//!   octave the next [`SUB_BITS`] bits select the sub-bucket, so relative
//!   resolution is constant (~1.5%) across the whole `u64` range.
//!
//! A bucket's reported quantile value is its **inclusive upper bound** (the
//! largest latency the bucket can hold). Reporting the upper bound makes the
//! estimate a conservative over-approximation of the true percentile, which is
//! the safe direction for a latency budget gate.

/// Number of sub-bucket bits per octave. Larger ⇒ finer relative resolution and
/// more buckets. Six bits give 64 sub-buckets (~1.5% relative error). §22.
pub const SUB_BITS: u32 = 6;

/// Sub-buckets per octave, `2^SUB_BITS`. Also the size of the exact linear low
/// region: values `0 .. SUB_COUNT` are stored losslessly. §22.
pub const SUB_COUNT: u64 = 1 << SUB_BITS;

/// Total fixed bucket count. The linear region contributes `SUB_COUNT` buckets;
/// octave regions run for exponents `1 ..= (64 - SUB_BITS)`, each contributing
/// `SUB_COUNT` buckets. `(64 - SUB_BITS + 1) * SUB_COUNT` is the tight upper
/// bound on `bucket_index` + 1 and is the length of the backing array. §22.
pub const NUM_BUCKETS: usize = (64 - SUB_BITS as usize + 1) * SUB_COUNT as usize;

/// Denominator for quantile fractions. A probability `p` is passed as the
/// integer `round(p * QUANTILE_SCALE)`, e.g. p99.9 is [`P999`] = 99_900. Using a
/// fixed integer scale keeps the whole quantile path float-free (§22).
pub const QUANTILE_SCALE: u64 = 100_000;

/// The 50th percentile (median), scaled by [`QUANTILE_SCALE`]. Criterion 20.
pub const P50: u64 = 50_000;
/// The 95th percentile, scaled by [`QUANTILE_SCALE`]. Criterion 20.
pub const P95: u64 = 95_000;
/// The 99th percentile, scaled by [`QUANTILE_SCALE`]. Criterion 20.
pub const P99: u64 = 99_000;
/// The 99.9th percentile, scaled by [`QUANTILE_SCALE`]. Criterion 20.
pub const P999: u64 = 99_900;

/// Map a latency sample (nanoseconds) to its fixed bucket index.
///
/// Responsibility: the single, deterministic value→bucket mapping shared by
/// [`LatencyHistogram::record`] and every quantile read. Monotonic
/// non-decreasing in `ns`, and the returned index is always `< NUM_BUCKETS`.
/// No float, no clock, no alloc (§22). Constitution criterion 20.
#[inline]
pub fn bucket_index(ns: u64) -> usize {
    if ns < SUB_COUNT {
        // Linear low region: each value is its own bucket (exact).
        return ns as usize;
    }
    // Significant-bit count of `ns`; `ns >= SUB_COUNT` ⇒ `bits >= SUB_BITS + 1`.
    let bits = 64 - ns.leading_zeros();
    // Octave exponent, `>= 1`; `shift` drops the bits below the retained mantissa.
    let exp = bits - SUB_BITS;
    let shift = exp - 1;
    // `sub` lands in `[SUB_COUNT, 2*SUB_COUNT)`; the leading bit is implicit.
    let sub = ns >> shift;
    let mantissa = sub - SUB_COUNT;
    exp as usize * SUB_COUNT as usize + mantissa as usize
}

/// Inclusive `[lo, hi]` nanosecond range covered by bucket `index`.
///
/// Responsibility: invert [`bucket_index`] to a value range so a quantile read
/// can report a concrete nanosecond figure (the inclusive upper bound `hi`).
/// Returns `None` for an out-of-range index. Overflow at the very top octave is
/// handled explicitly: `hi` for the final bucket is exactly `u64::MAX`, computed
/// as `lo + (width - 1)` so the intermediate never wraps (§22). Criterion 20.
#[inline]
pub fn bucket_bounds(index: usize) -> Option<(u64, u64)> {
    if index >= NUM_BUCKETS {
        return None;
    }
    let idx = index as u64;
    if idx < SUB_COUNT {
        // Linear low region: unit-width bucket, lo == hi == index.
        return Some((idx, idx));
    }
    let exp = idx / SUB_COUNT; // octave exponent, >= 1
    let mantissa = idx % SUB_COUNT; // sub-bucket within the octave, [0, SUB_COUNT)
    let shift = (exp - 1) as u32;
    // `(SUB_COUNT + mantissa)` is at most 127; `127 << 57 < 2^64`, so `lo` fits.
    let lo = (SUB_COUNT + mantissa) << shift;
    // Bucket width is `2^shift`; `lo + (width - 1)` fits `u64` even for the final
    // bucket (there it equals `u64::MAX`), whereas `lo + width` would wrap.
    let width_minus_1 = (1u64 << shift) - 1;
    let hi = lo + width_minus_1;
    Some((lo, hi))
}

/// Deterministic fixed-bucket latency histogram (§22, criterion 20).
///
/// Responsibility: hold accumulated nanosecond latency counts in a fixed array
/// and answer nearest-rank quantile queries. Construct with [`LatencyHistogram::new`],
/// feed samples with [`record`](LatencyHistogram::record), read tails with
/// [`quantile`](LatencyHistogram::quantile) / [`p50`](LatencyHistogram::p50) /
/// [`p95`](LatencyHistogram::p95) / [`p99`](LatencyHistogram::p99) /
/// [`p999`](LatencyHistogram::p999). The type is a plain value (no heap), so it
/// is cheap to snapshot and [`merge`](LatencyHistogram::merge).
#[derive(Clone)]
pub struct LatencyHistogram {
    /// Per-bucket sample counts, saturating on overflow.
    counts: [u64; NUM_BUCKETS],
    /// Total samples recorded, saturating on overflow. Equals `counts.sum()`.
    total: u64,
}

impl LatencyHistogram {
    /// Construct an empty histogram. Const so it can back a `static`. §22.
    #[inline]
    pub const fn new() -> Self {
        Self {
            counts: [0; NUM_BUCKETS],
            total: 0,
        }
    }

    /// Record one latency sample of `ns` nanoseconds.
    ///
    /// Increments the sample's bucket and the running total, both with
    /// `saturating_add` so a counter at `u64::MAX` stays pinned rather than
    /// wrapping (explicit-overflow contract, §22). Criterion 20.
    #[inline]
    pub fn record(&mut self, ns: u64) {
        let i = bucket_index(ns);
        self.counts[i] = self.counts[i].saturating_add(1);
        self.total = self.total.saturating_add(1);
    }

    /// Total number of samples recorded. §22.
    #[inline]
    pub fn count(&self) -> u64 {
        self.total
    }

    /// Whether no samples have been recorded yet. §22.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Nearest-rank quantile in nanoseconds for `p_scaled` (out of
    /// [`QUANTILE_SCALE`]); `None` if empty.
    ///
    /// Uses the nearest-rank definition: with `N` samples the target rank is
    /// `ceil(p * N / QUANTILE_SCALE)`, clamped to `[1, N]`, and the result is the
    /// inclusive upper bound of the bucket that contains the rank-th smallest
    /// sample. `p_scaled` is clamped to `QUANTILE_SCALE`. The rank multiply uses
    /// `u128` to avoid overflow for large sample totals; all other arithmetic is
    /// integer and saturating (§22). Criterion 20.
    pub fn quantile(&self, p_scaled: u64) -> Option<u64> {
        if self.total == 0 {
            return None;
        }
        let p = if p_scaled > QUANTILE_SCALE {
            QUANTILE_SCALE
        } else {
            p_scaled
        };
        // rank = ceil(p * total / SCALE); u128 keeps `p * total` from overflowing.
        let num = p as u128 * self.total as u128;
        let mut rank = num.div_ceil(QUANTILE_SCALE as u128) as u64;
        if rank == 0 {
            rank = 1;
        }
        // p <= SCALE guarantees rank <= total, so the scan always resolves.
        let mut cum: u64 = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            cum = cum.saturating_add(c);
            if cum >= rank {
                // `i` was produced by `bucket_index`, so bounds always exist.
                let (_lo, hi) = bucket_bounds(i)?;
                return Some(hi);
            }
        }
        None
    }

    /// The median (p50) in nanoseconds; `None` if empty. Criterion 20.
    #[inline]
    pub fn p50(&self) -> Option<u64> {
        self.quantile(P50)
    }

    /// The p95 latency in nanoseconds; `None` if empty. Criterion 20.
    #[inline]
    pub fn p95(&self) -> Option<u64> {
        self.quantile(P95)
    }

    /// The p99 latency in nanoseconds; `None` if empty. Criterion 20.
    #[inline]
    pub fn p99(&self) -> Option<u64> {
        self.quantile(P99)
    }

    /// The p99.9 latency in nanoseconds; `None` if empty. Criterion 20.
    #[inline]
    pub fn p999(&self) -> Option<u64> {
        self.quantile(P999)
    }

    /// Inclusive upper bound of the lowest non-empty bucket (a conservative
    /// minimum); `None` if empty. §22.
    pub fn min(&self) -> Option<u64> {
        for (i, &c) in self.counts.iter().enumerate() {
            if c > 0 {
                return bucket_bounds(i).map(|(lo, _hi)| lo);
            }
        }
        None
    }

    /// Inclusive upper bound of the highest non-empty bucket (a conservative
    /// maximum); `None` if empty. §22.
    pub fn max(&self) -> Option<u64> {
        for (i, &c) in self.counts.iter().enumerate().rev() {
            if c > 0 {
                return bucket_bounds(i).map(|(_lo, hi)| hi);
            }
        }
        None
    }

    /// Fold `other` into `self`, bucket by bucket, saturating each counter.
    ///
    /// Enables combining per-thread or per-interval histograms into an aggregate
    /// without touching the raw samples — deterministic and alloc-free (§22).
    /// Criterion 20.
    pub fn merge(&mut self, other: &LatencyHistogram) {
        for (dst, &src) in self.counts.iter_mut().zip(other.counts.iter()) {
            *dst = dst.saturating_add(src);
        }
        self.total = self.total.saturating_add(other.total);
    }
}

impl Default for LatencyHistogram {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}
