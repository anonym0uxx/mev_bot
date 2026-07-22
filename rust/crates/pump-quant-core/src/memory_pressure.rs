//! Continuous memory-pressure awareness and graceful load-shedding
//! (constitution §57 memory mandate (b); acceptance criterion 99).
//!
//! ## Responsibility
//! §57(a) — bounded growth — is enforced elsewhere (`pump-quant-memory`
//! `bounded_push` + `CapacityExceeded`). This module implements the *distinct*
//! §57(b) half: the process continuously samples its own resident-set size and
//! the system's available memory, classifies that into an ordinal
//! [`PressureLevel`], and derives a [`ShedPlan`] of graceful degradations —
//! shed research/enrichment, narrow best-of-N, compact caches, flush-and-release
//! — so the engine sheds load *before* any hard limit rather than OOM-crashing.
//!
//! ## Determinism & bounds (§22, §99)
//! The load-bearing logic is pure integer threshold code:
//! * [`classify`] — `(sample, thresholds) -> PressureLevel`, no clock/float/RNG/IO
//!   (mirrors `pump-quant-market-state` `regime::classify`).
//! * [`ShedPlan::for_level`] — pure ordinal -> action-set map.
//! * [`PressureReducer`] — O(1) bounded state with anti-flap hysteresis
//!   (escalate instantly to protect against OOM, de-escalate slowly).
//!
//! The actual OS read (`/proc/self/statm`, `sysinfo`, `GlobalMemoryStatusEx`, …)
//! is confined behind the mockable [`MemorySampler`] trait — SERVER-DEFERRED,
//! exactly like the Windows `OsTune` binding in `cpu_numa_tuning`. Only
//! [`MockSampler`] exists here; every decision path is tested with synthetic
//! inputs.

/// Ordinal memory-pressure level (higher = more stressed). Kept ordinal, never
/// collapsed into a number, so escalation/de-escalation compare directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureLevel {
    /// Comfortable headroom — full behaviour, nothing shed.
    Nominal,
    /// Soft threshold crossed — begin shedding optional work early.
    Soft,
    /// Hard threshold crossed — aggressively narrow work.
    Hard,
    /// Critical — last-ditch flush-and-release before a limit is hit.
    Critical,
}

impl PressureLevel {
    /// Ordinal rank (0 = Nominal … 3 = Critical), for stepping.
    #[inline]
    #[must_use]
    fn rank(self) -> u8 {
        match self {
            PressureLevel::Nominal => 0,
            PressureLevel::Soft => 1,
            PressureLevel::Hard => 2,
            PressureLevel::Critical => 3,
        }
    }

    /// The level one step less severe (`Nominal` saturates at itself).
    #[inline]
    #[must_use]
    fn step_down(self) -> PressureLevel {
        match self {
            PressureLevel::Critical => PressureLevel::Hard,
            PressureLevel::Hard => PressureLevel::Soft,
            PressureLevel::Soft | PressureLevel::Nominal => PressureLevel::Nominal,
        }
    }
}

/// A single point-in-time memory observation — the pure input to [`classify`].
///
/// Both figures are raw byte counts produced by a [`MemorySampler`]. No floats
/// (§22). `available_bytes` is `Option` so an unobservable system-memory figure
/// classifies as *unknown* (contributes nothing) rather than a fabricated `0`
/// that would spuriously trip Critical (§6.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemorySample {
    /// Process resident-set size in bytes.
    pub rss_bytes: u64,
    /// System memory currently available in bytes; `None` when unobserved.
    pub available_bytes: Option<u64>,
}

impl MemorySample {
    /// A sample with only RSS observed (system-available unknown).
    #[must_use]
    pub fn rss(rss_bytes: u64) -> Self {
        Self {
            rss_bytes,
            available_bytes: None,
        }
    }

    /// A sample with both RSS and system-available observed.
    #[must_use]
    pub fn new(rss_bytes: u64, available_bytes: u64) -> Self {
        Self {
            rss_bytes,
            available_bytes: Some(available_bytes),
        }
    }
}

/// Versioned, explicit thresholds for [`classify`] (§102 — no silent magic
/// numbers). Two independent dimensions, each with soft/hard/critical cuts:
///
/// * process RSS as a fraction of a configured `budget_bytes`, in basis points;
/// * absolute system-available floors, in bytes (lower available = more stress).
///
/// The final level is the *more severe* of the two dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressureThresholds {
    /// Taxonomy/threshold version stamped by the caller for provenance.
    pub version: u32,
    /// Process RSS budget in bytes; RSS is scored as a fraction of this.
    pub budget_bytes: u64,
    /// RSS-fraction cut (bps of `budget_bytes`) at/above which level is `Soft`.
    pub rss_soft_bps: u32,
    /// RSS-fraction cut (bps) at/above which level is `Hard`.
    pub rss_hard_bps: u32,
    /// RSS-fraction cut (bps) at/above which level is `Critical`.
    pub rss_critical_bps: u32,
    /// System-available floor (bytes) at/below which level is `Soft`.
    pub avail_soft_bytes: u64,
    /// System-available floor (bytes) at/below which level is `Hard`.
    pub avail_hard_bytes: u64,
    /// System-available floor (bytes) at/below which level is `Critical`.
    pub avail_critical_bytes: u64,
}

impl Default for PressureThresholds {
    /// Illustrative v0 thresholds for tests / bootstrapping over a 1 GiB budget.
    /// Production supplies calibrated, versioned values.
    fn default() -> Self {
        const GIB: u64 = 1 << 30;
        const MIB: u64 = 1 << 20;
        PressureThresholds {
            version: 0,
            budget_bytes: GIB,
            rss_soft_bps: 7_000,     // 70%
            rss_hard_bps: 8_500,     // 85%
            rss_critical_bps: 9_500, // 95%
            avail_soft_bytes: 512 * MIB,
            avail_hard_bytes: 256 * MIB,
            avail_critical_bytes: 128 * MIB,
        }
    }
}

/// Why a [`PressureThresholds`] set is not valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdError {
    /// `budget_bytes` was zero (division base must be positive).
    ZeroBudget,
    /// RSS bps cuts were not strictly increasing (soft < hard < critical).
    RssNotMonotone,
    /// System-available floors were not strictly decreasing (soft > hard > critical).
    AvailNotMonotone,
}

impl PressureThresholds {
    /// Validate the invariants [`classify`] relies on: positive budget, strictly
    /// increasing RSS cuts, strictly decreasing available floors.
    pub fn validate(&self) -> Result<(), ThresholdError> {
        if self.budget_bytes == 0 {
            return Err(ThresholdError::ZeroBudget);
        }
        if !(self.rss_soft_bps < self.rss_hard_bps && self.rss_hard_bps < self.rss_critical_bps) {
            return Err(ThresholdError::RssNotMonotone);
        }
        if !(self.avail_soft_bytes > self.avail_hard_bytes
            && self.avail_hard_bytes > self.avail_critical_bytes)
        {
            return Err(ThresholdError::AvailNotMonotone);
        }
        Ok(())
    }
}

/// RSS as a fraction of budget, in basis points, saturating (never panics).
/// `rss * 10_000 / budget`, computed in u128 to avoid overflow; `budget == 0`
/// yields `u32::MAX` (treated as maximally stressed) so callers that skip
/// validation still fail safe rather than dividing by zero.
#[inline]
#[must_use]
fn rss_fraction_bps(rss_bytes: u64, budget_bytes: u64) -> u32 {
    if budget_bytes == 0 {
        return u32::MAX;
    }
    let bps = (rss_bytes as u128 * 10_000) / budget_bytes as u128;
    bps.min(u32::MAX as u128) as u32
}

/// Bucket an RSS fraction (bps) by increasing cut points.
#[inline]
#[must_use]
fn rss_level(frac_bps: u32, t: &PressureThresholds) -> PressureLevel {
    if frac_bps >= t.rss_critical_bps {
        PressureLevel::Critical
    } else if frac_bps >= t.rss_hard_bps {
        PressureLevel::Hard
    } else if frac_bps >= t.rss_soft_bps {
        PressureLevel::Soft
    } else {
        PressureLevel::Nominal
    }
}

/// Bucket a system-available figure by *decreasing* floors (less = worse).
#[inline]
#[must_use]
fn avail_level(available_bytes: u64, t: &PressureThresholds) -> PressureLevel {
    if available_bytes <= t.avail_critical_bytes {
        PressureLevel::Critical
    } else if available_bytes <= t.avail_hard_bytes {
        PressureLevel::Hard
    } else if available_bytes <= t.avail_soft_bytes {
        PressureLevel::Soft
    } else {
        PressureLevel::Nominal
    }
}

/// Classify a [`MemorySample`] into a [`PressureLevel`] — the pure, deterministic
/// core. The result is the more severe of the RSS-fraction dimension and the
/// system-available dimension; an unobserved `available_bytes` contributes
/// nothing (stays `Nominal` for that dimension), never a fabricated Critical.
#[must_use]
pub fn classify(sample: &MemorySample, thresholds: &PressureThresholds) -> PressureLevel {
    let rss = rss_level(
        rss_fraction_bps(sample.rss_bytes, thresholds.budget_bytes),
        thresholds,
    );
    let avail = match sample.available_bytes {
        Some(a) => avail_level(a, thresholds),
        None => PressureLevel::Nominal,
    };
    rss.max(avail)
}

/// The set of graceful degradations to apply at a given [`PressureLevel`].
///
/// Cumulative and monotone: each escalating level is a superset of the previous
/// one, so the engine can act on flags directly. Order of severity follows the
/// §57(b) enumeration (shed research → narrow best-of-N → compact caches →
/// flush-and-release).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShedPlan {
    /// Drop optional research / enrichment work (cheapest, earliest shed).
    pub shed_research: bool,
    /// Compact / trim caches to release retained-but-idle memory.
    pub compact_caches: bool,
    /// Narrow best-of-N candidate generation to the essential minimum.
    pub narrow_best_of_n: bool,
    /// Flush buffered state and release its backing memory (last resort).
    pub flush_and_release: bool,
}

impl ShedPlan {
    /// The no-op plan (full behaviour retained).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether any degradation is requested.
    #[must_use]
    pub fn is_shedding(&self) -> bool {
        self.shed_research || self.compact_caches || self.narrow_best_of_n || self.flush_and_release
    }

    /// The cumulative degradation plan for `level`.
    #[must_use]
    pub fn for_level(level: PressureLevel) -> Self {
        match level {
            PressureLevel::Nominal => ShedPlan::none(),
            PressureLevel::Soft => ShedPlan {
                shed_research: true,
                compact_caches: true,
                narrow_best_of_n: false,
                flush_and_release: false,
            },
            PressureLevel::Hard => ShedPlan {
                shed_research: true,
                compact_caches: true,
                narrow_best_of_n: true,
                flush_and_release: false,
            },
            PressureLevel::Critical => ShedPlan {
                shed_research: true,
                compact_caches: true,
                narrow_best_of_n: true,
                flush_and_release: true,
            },
        }
    }
}

/// The mockable memory-sampling surface. The server implements this over the OS
/// (`/proc/self/statm` + `MemAvailable`, `sysinfo`, `GlobalMemoryStatusEx`);
/// tests implement it with [`MockSampler`]. No live read appears in this crate.
///
/// SERVER (Phase-B) TODO: the real `impl MemorySampler` reading process RSS and
/// system-available memory is a deployment-box deliverable, intentionally absent
/// on the laptop — exactly like the Windows `OsTune` binding in
/// `cpu_numa_tuning`. The portable classification/degradation logic above is
/// complete and independently tested.
pub trait MemorySampler {
    /// Read the current process RSS and system-available memory.
    fn sample(&mut self) -> MemorySample;
}

/// A deterministic test sampler that replays a fixed script of samples, holding
/// the final one once exhausted (so a reducer can be driven indefinitely).
#[derive(Clone, Debug)]
pub struct MockSampler {
    script: Vec<MemorySample>,
    idx: usize,
}

impl MockSampler {
    /// A sampler that always returns `s`.
    #[must_use]
    pub fn fixed(s: MemorySample) -> Self {
        Self {
            script: vec![s],
            idx: 0,
        }
    }

    /// A sampler that replays `script` in order, then repeats the last element.
    /// An empty script yields a zeroed [`MemorySample`] forever.
    #[must_use]
    pub fn scripted(script: Vec<MemorySample>) -> Self {
        Self { script, idx: 0 }
    }
}

impl MemorySampler for MockSampler {
    fn sample(&mut self) -> MemorySample {
        if self.script.is_empty() {
            return MemorySample::default();
        }
        let i = self.idx.min(self.script.len() - 1);
        let out = self.script[i];
        if self.idx + 1 < self.script.len() {
            self.idx += 1;
        }
        out
    }
}

/// Continuous, anti-flap pressure tracker with O(1) bounded state (§99).
///
/// Policy: **escalate instantly** (the moment a sample is more severe, jump
/// straight to it — never let load build toward an OOM while debouncing) but
/// **de-escalate slowly** (only step down one level after `calm_required`
/// consecutive strictly-lower samples), so the shed plan doesn't oscillate on a
/// noisy RSS reading.
#[derive(Clone, Debug)]
pub struct PressureReducer {
    thresholds: PressureThresholds,
    level: PressureLevel,
    calm_required: u32,
    calm_streak: u32,
    samples_seen: u64,
}

impl PressureReducer {
    /// A reducer starting at `Nominal`. `calm_required` is the number of
    /// consecutive lower-classified samples needed to relax one level; it is
    /// clamped to at least 1 so relaxation always requires confirmation.
    #[must_use]
    pub fn new(thresholds: PressureThresholds, calm_required: u32) -> Self {
        Self {
            thresholds,
            level: PressureLevel::Nominal,
            calm_required: calm_required.max(1),
            calm_streak: 0,
            samples_seen: 0,
        }
    }

    /// Current debounced pressure level.
    #[must_use]
    pub fn level(&self) -> PressureLevel {
        self.level
    }

    /// The cumulative shed plan for the current level.
    #[must_use]
    pub fn shed_plan(&self) -> ShedPlan {
        ShedPlan::for_level(self.level)
    }

    /// Total samples observed (saturating).
    #[must_use]
    pub fn samples_seen(&self) -> u64 {
        self.samples_seen
    }

    /// Observe one sample, update the debounced level, and return it.
    pub fn observe(&mut self, sample: &MemorySample) -> PressureLevel {
        self.samples_seen = self.samples_seen.saturating_add(1);
        let raw = classify(sample, &self.thresholds);
        match raw.rank().cmp(&self.level.rank()) {
            std::cmp::Ordering::Greater => {
                // Escalate immediately, straight to the raw severity.
                self.level = raw;
                self.calm_streak = 0;
            }
            std::cmp::Ordering::Equal => {
                self.calm_streak = 0;
            }
            std::cmp::Ordering::Less => {
                self.calm_streak = self.calm_streak.saturating_add(1);
                if self.calm_streak >= self.calm_required {
                    self.level = self.level.step_down();
                    self.calm_streak = 0;
                }
            }
        }
        self.level
    }

    /// Pull one sample from `sampler`, fold it in, and return the resulting level
    /// and shed plan. This is the seam the engine polls each health tick.
    pub fn poll<S: MemorySampler>(&mut self, sampler: &mut S) -> (PressureLevel, ShedPlan) {
        let s = sampler.sample();
        let level = self.observe(&s);
        (level, ShedPlan::for_level(level))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    fn thr() -> PressureThresholds {
        PressureThresholds::default()
    }

    #[test]
    fn thresholds_validate_ok_and_reject() {
        assert_eq!(thr().validate(), Ok(()));

        let mut z = thr();
        z.budget_bytes = 0;
        assert_eq!(z.validate(), Err(ThresholdError::ZeroBudget));

        let mut r = thr();
        r.rss_hard_bps = r.rss_soft_bps; // not strictly increasing
        assert_eq!(r.validate(), Err(ThresholdError::RssNotMonotone));

        let mut a = thr();
        a.avail_hard_bytes = a.avail_soft_bytes; // not strictly decreasing
        assert_eq!(a.validate(), Err(ThresholdError::AvailNotMonotone));
    }

    #[test]
    fn rss_fraction_is_exact_integer() {
        assert_eq!(rss_fraction_bps(0, GIB), 0);
        assert_eq!(rss_fraction_bps(GIB / 2, GIB), 5_000);
        assert_eq!(rss_fraction_bps(GIB, GIB), 10_000);
        // Over budget saturates the fraction, never overflows.
        assert_eq!(rss_fraction_bps(2 * GIB, GIB), 20_000);
        // Zero budget fails safe to maximal stress.
        assert_eq!(rss_fraction_bps(1, 0), u32::MAX);
    }

    #[test]
    fn classify_rss_dimension_bands() {
        // Exact-percentage budget so band edges are integer-clean.
        let t = PressureThresholds {
            budget_bytes: 1_000_000,
            avail_soft_bytes: 0, // keep the available dimension out of the way
            avail_hard_bytes: 0,
            avail_critical_bytes: 0,
            ..PressureThresholds::default()
        };
        // 60% -> below soft
        assert_eq!(
            classify(&MemorySample::rss(600_000), &t),
            PressureLevel::Nominal
        );
        // exactly 70% -> Soft (inclusive lower edge)
        assert_eq!(
            classify(&MemorySample::rss(700_000), &t),
            PressureLevel::Soft
        );
        // exactly 85% -> Hard (inclusive lower edge)
        assert_eq!(
            classify(&MemorySample::rss(850_000), &t),
            PressureLevel::Hard
        );
        // 96% -> Critical
        assert_eq!(
            classify(&MemorySample::rss(960_000), &t),
            PressureLevel::Critical
        );
    }

    #[test]
    fn classify_available_dimension_bands() {
        let t = thr();
        let low_rss = 100 * MIB; // keeps RSS dimension Nominal
                                 // plenty available
        assert_eq!(
            classify(&MemorySample::new(low_rss, GIB), &t),
            PressureLevel::Nominal
        );
        // exactly the soft floor -> Soft (inclusive)
        assert_eq!(
            classify(&MemorySample::new(low_rss, 512 * MIB), &t),
            PressureLevel::Soft
        );
        // at hard floor
        assert_eq!(
            classify(&MemorySample::new(low_rss, 256 * MIB), &t),
            PressureLevel::Hard
        );
        // below critical floor
        assert_eq!(
            classify(&MemorySample::new(low_rss, 64 * MIB), &t),
            PressureLevel::Critical
        );
    }

    #[test]
    fn classify_takes_more_severe_dimension() {
        let t = thr();
        // RSS says Soft (72%), available says Hard (256MiB) -> Hard wins.
        let s = MemorySample::new(72 * GIB / 100, 256 * MIB);
        assert_eq!(classify(&s, &t), PressureLevel::Hard);
    }

    #[test]
    fn unobserved_available_does_not_fabricate_pressure() {
        let t = thr();
        // available unknown: only RSS scored, stays Nominal despite None.
        assert_eq!(
            classify(&MemorySample::rss(10 * MIB), &t),
            PressureLevel::Nominal
        );
    }

    #[test]
    fn shed_plan_is_cumulative_and_monotone() {
        assert_eq!(
            ShedPlan::for_level(PressureLevel::Nominal),
            ShedPlan::none()
        );
        assert!(!ShedPlan::for_level(PressureLevel::Nominal).is_shedding());

        let soft = ShedPlan::for_level(PressureLevel::Soft);
        assert!(soft.shed_research && soft.compact_caches);
        assert!(!soft.narrow_best_of_n && !soft.flush_and_release);

        let hard = ShedPlan::for_level(PressureLevel::Hard);
        assert!(hard.shed_research && hard.compact_caches && hard.narrow_best_of_n);
        assert!(!hard.flush_and_release);

        let crit = ShedPlan::for_level(PressureLevel::Critical);
        assert!(
            crit.shed_research
                && crit.compact_caches
                && crit.narrow_best_of_n
                && crit.flush_and_release
        );

        // Each escalation is a superset of the previous (monotone).
        for (lo, hi) in [
            (PressureLevel::Nominal, PressureLevel::Soft),
            (PressureLevel::Soft, PressureLevel::Hard),
            (PressureLevel::Hard, PressureLevel::Critical),
        ] {
            let l = ShedPlan::for_level(lo);
            let h = ShedPlan::for_level(hi);
            assert!(!l.shed_research || h.shed_research);
            assert!(!l.compact_caches || h.compact_caches);
            assert!(!l.narrow_best_of_n || h.narrow_best_of_n);
            assert!(!l.flush_and_release || h.flush_and_release);
        }
    }

    #[test]
    fn reducer_escalates_instantly() {
        let mut r = PressureReducer::new(thr(), 3);
        assert_eq!(r.level(), PressureLevel::Nominal);
        // Jump straight from Nominal to Critical on a single severe sample.
        let l = r.observe(&MemorySample::rss(98 * GIB / 100));
        assert_eq!(l, PressureLevel::Critical);
        assert_eq!(r.samples_seen(), 1);
        assert!(r.shed_plan().flush_and_release);
    }

    #[test]
    fn reducer_deescalates_only_after_calm_streak() {
        let mut r = PressureReducer::new(thr(), 3);
        r.observe(&MemorySample::rss(98 * GIB / 100)); // Critical
        assert_eq!(r.level(), PressureLevel::Critical);

        let calm = MemorySample::rss(10 * MIB); // Nominal-classified
                                                // Two calm samples: not yet enough to step down.
        r.observe(&calm);
        r.observe(&calm);
        assert_eq!(r.level(), PressureLevel::Critical);
        // Third calm sample: step down exactly one level (Critical -> Hard).
        r.observe(&calm);
        assert_eq!(r.level(), PressureLevel::Hard);
        // Streak resets; need three more to reach Soft.
        r.observe(&calm);
        r.observe(&calm);
        assert_eq!(r.level(), PressureLevel::Hard);
        r.observe(&calm);
        assert_eq!(r.level(), PressureLevel::Soft);
    }

    #[test]
    fn reducer_calm_streak_resets_on_reescalation() {
        let mut r = PressureReducer::new(thr(), 3);
        r.observe(&MemorySample::rss(98 * GIB / 100)); // Critical
        let calm = MemorySample::rss(10 * MIB);
        r.observe(&calm);
        r.observe(&calm); // streak = 2
                          // A re-escalation to same level resets the streak.
        r.observe(&MemorySample::rss(98 * GIB / 100));
        assert_eq!(r.level(), PressureLevel::Critical);
        r.observe(&calm);
        r.observe(&calm);
        // Only 2 calm since reset -> still Critical.
        assert_eq!(r.level(), PressureLevel::Critical);
    }

    #[test]
    fn reducer_calm_required_clamped_to_one() {
        let mut r = PressureReducer::new(thr(), 0); // clamped to 1
        r.observe(&MemorySample::rss(98 * GIB / 100)); // Critical
        r.observe(&MemorySample::rss(10 * MIB)); // one calm -> step down
        assert_eq!(r.level(), PressureLevel::Hard);
    }

    #[test]
    fn poll_drives_reducer_from_sampler() {
        let script = vec![
            MemorySample::rss(10 * MIB),       // Nominal
            MemorySample::rss(90 * GIB / 100), // Hard
            MemorySample::rss(98 * GIB / 100), // Critical
        ];
        let mut sampler = MockSampler::scripted(script);
        let mut r = PressureReducer::new(thr(), 2);

        let (l0, p0) = r.poll(&mut sampler);
        assert_eq!(l0, PressureLevel::Nominal);
        assert!(!p0.is_shedding());

        let (l1, p1) = r.poll(&mut sampler);
        assert_eq!(l1, PressureLevel::Hard);
        assert!(p1.narrow_best_of_n && !p1.flush_and_release);

        let (l2, p2) = r.poll(&mut sampler);
        assert_eq!(l2, PressureLevel::Critical);
        assert!(p2.flush_and_release);

        // Script exhausted: sampler holds the last (Critical) sample.
        let (l3, _) = r.poll(&mut sampler);
        assert_eq!(l3, PressureLevel::Critical);
        assert_eq!(r.samples_seen(), 4);
    }

    #[test]
    fn mock_sampler_fixed_and_empty() {
        let mut f = MockSampler::fixed(MemorySample::new(1, 2));
        assert_eq!(f.sample(), MemorySample::new(1, 2));
        assert_eq!(f.sample(), MemorySample::new(1, 2));

        let mut e = MockSampler::scripted(vec![]);
        assert_eq!(e.sample(), MemorySample::default());
    }
}
