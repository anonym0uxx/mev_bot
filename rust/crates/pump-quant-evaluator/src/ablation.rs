//! `ablation` — deterministic feature-family ablation harness (constitution §50).
//!
//! §50 requires that each feature family's marginal contribution be *measured*,
//! not asserted: for every family we re-run the recorded experiment with that
//! family removed / alone / delayed / noised / shuffled and read the change in
//! net-SOL and right-tail. This module provides the harness — an
//! [`AblationVariant`] enum, an [`AblationReplay`] trait the app implements later
//! against its real replay engine, and a [`run_ablation`] runner that folds each
//! variant's outcome into an incremental figure against the all-features-on
//! baseline.
//!
//! The harness is pure over the trait: given a deterministic `impl AblationReplay`
//! it produces byte-for-byte identical [`AblationResult`]s. No RNG lives here —
//! the "noised"/"shuffled" variants are *named* perturbations the replay closure
//! realizes deterministically from a seed it owns; the harness only orchestrates.
//! Money is `i128` lamports, right-tail an `i64` bps figure — no floats (§22).

/// A feature family under test. Opaque id; ordering drives deterministic output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureFamily(pub u32);

/// The set of currently-enabled feature families, as a 64-bit mask keyed by the
/// family id (`id < 64`). A mask is used rather than a set so toggling is a pure
/// bit operation and equality is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FeatureToggleMask(pub u64);

impl FeatureToggleMask {
    /// All-off mask.
    pub fn none() -> Self {
        FeatureToggleMask(0)
    }

    /// Build an all-on mask over the supplied families.
    pub fn all_on(families: &[FeatureFamily]) -> Self {
        let mut m = 0u64;
        for f in families {
            debug_assert!(
                f.0 < 64,
                "FeatureFamily id must be < 64 for the toggle mask"
            );
            m |= 1u64 << (f.0 & 63);
        }
        FeatureToggleMask(m)
    }

    /// True iff `family` is enabled in this mask.
    pub fn contains(&self, family: FeatureFamily) -> bool {
        self.0 & (1u64 << (family.0 & 63)) != 0
    }

    /// Copy with `family` cleared.
    pub fn without(&self, family: FeatureFamily) -> Self {
        FeatureToggleMask(self.0 & !(1u64 << (family.0 & 63)))
    }

    /// Copy with only `family` set.
    pub fn only(family: FeatureFamily) -> Self {
        FeatureToggleMask(1u64 << (family.0 & 63))
    }
}

/// The ablation perturbation applied to a family for one replay (§50).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AblationVariant {
    /// The family is removed; everything else stays on.
    Removed,
    /// The family alone is on; everything else off.
    Alone,
    /// All families on (the reference baseline).
    Combined,
    /// The family's signal is delayed by one decision step.
    Delayed,
    /// The family's signal has deterministic noise injected.
    Noised,
    /// The family's signal is deterministically shuffled across events.
    Shuffled,
}

impl AblationVariant {
    /// The perturbation variants scored *per family* (i.e. excluding the shared
    /// [`AblationVariant::Combined`] baseline), in deterministic order.
    pub const PER_FAMILY: [AblationVariant; 5] = [
        AblationVariant::Removed,
        AblationVariant::Alone,
        AblationVariant::Delayed,
        AblationVariant::Noised,
        AblationVariant::Shuffled,
    ];
}

/// The outcome of one replay: reconciled net-SOL and a right-tail measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ReplayOutcome {
    /// Reconciled net lamports for this replay.
    pub net_lamports: i128,
    /// Right-tail measure (e.g. top-decile net contribution), bps.
    pub right_tail_bps: i64,
}

impl ReplayOutcome {
    /// Constructor.
    pub fn new(net_lamports: i128, right_tail_bps: i64) -> Self {
        ReplayOutcome {
            net_lamports,
            right_tail_bps,
        }
    }
}

/// The replay engine the harness drives. The app implements this later against
/// its real recorded-experiment replayer; the harness only requires that it be a
/// deterministic pure function of `(toggles, variant, family)`.
///
/// `family` is `None` for the shared [`AblationVariant::Combined`] baseline (a
/// whole-system replay) and `Some(f)` for a per-family perturbation.
pub trait AblationReplay {
    /// Replay the sealed experiment under `toggles`, applying `variant` to
    /// `family`. Must be deterministic: identical arguments -> identical outcome.
    fn replay(
        &self,
        toggles: FeatureToggleMask,
        variant: AblationVariant,
        family: Option<FeatureFamily>,
    ) -> ReplayOutcome;
}

/// One family × variant ablation measurement, relative to the combined baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AblationResult {
    /// Family perturbed.
    pub family: FeatureFamily,
    /// Perturbation applied.
    pub variant: AblationVariant,
    /// This variant's absolute net lamports.
    pub net_lamports: i128,
    /// This variant's absolute right-tail, bps.
    pub right_tail_bps: i64,
    /// Incremental net vs the combined baseline (`variant − baseline`).
    pub incremental_net_lamports: i128,
    /// Incremental right-tail vs the combined baseline.
    pub incremental_right_tail_bps: i64,
}

/// The full ablation report: the baseline plus every per-family measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AblationReport {
    /// The all-features-on combined baseline outcome.
    pub baseline: ReplayOutcome,
    /// Per-family × per-variant results, ordered by `(family, variant)`.
    pub results: Vec<AblationResult>,
}

/// Run the ablation harness over `families` and `variants` (§50).
///
/// First establishes the combined (all-on) baseline via one
/// [`AblationVariant::Combined`] replay, then for each family × variant re-runs
/// the closure with the appropriate toggle mask and records the incremental
/// net-SOL and right-tail against that baseline. Output is ordered by
/// `(family, variant)` and is a deterministic function of the closure. Pure
/// aside from the trait's own behaviour.
pub fn run_ablation<R: AblationReplay>(
    replay: &R,
    families: &[FeatureFamily],
    variants: &[AblationVariant],
) -> AblationReport {
    let all_on = FeatureToggleMask::all_on(families);
    let baseline = replay.replay(all_on, AblationVariant::Combined, None);

    let mut results: Vec<AblationResult> = Vec::new();
    // Deterministic order: families are already the caller's order; sort a copy
    // so output is stable regardless of caller order.
    let mut fam_sorted: Vec<FeatureFamily> = families.to_vec();
    fam_sorted.sort_unstable();
    fam_sorted.dedup();

    for &family in &fam_sorted {
        // Variants applied in AblationVariant enum order for determinism.
        let mut var_sorted: Vec<AblationVariant> = variants.to_vec();
        var_sorted.sort_unstable();
        var_sorted.dedup();
        for &variant in &var_sorted {
            // Combined is the baseline, not a per-family perturbation; skip it.
            if variant == AblationVariant::Combined {
                continue;
            }
            let toggles = match variant {
                AblationVariant::Removed => all_on.without(family),
                AblationVariant::Alone => FeatureToggleMask::only(family),
                // Delayed/Noised/Shuffled keep the full toggle set; the
                // perturbation is realized inside the replay closure.
                _ => all_on,
            };
            let out = replay.replay(toggles, variant, Some(family));
            results.push(AblationResult {
                family,
                variant,
                net_lamports: out.net_lamports,
                right_tail_bps: out.right_tail_bps,
                incremental_net_lamports: out.net_lamports - baseline.net_lamports,
                incremental_right_tail_bps: out.right_tail_bps - baseline.right_tail_bps,
            });
        }
    }

    AblationReport { baseline, results }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic mock replay: outcome is a pure function of the toggle mask,
    /// variant, and family — no RNG, no state. Models "each family contributes a
    /// fixed net, perturbations scale it".
    struct MockReplay;

    impl AblationReplay for MockReplay {
        fn replay(
            &self,
            toggles: FeatureToggleMask,
            variant: AblationVariant,
            family: Option<FeatureFamily>,
        ) -> ReplayOutcome {
            // Base contribution: 1000 lamports per enabled family bit.
            let bits = toggles.0.count_ones() as i128;
            let base_net = bits * 1_000;
            let base_tail = bits as i64 * 10;
            // Deterministic per-variant/family adjustment (pure integer arithmetic).
            let fam_id = family.map(|f| f.0 as i128).unwrap_or(0);
            let adj = match variant {
                AblationVariant::Combined => 0,
                AblationVariant::Removed => -fam_id,
                AblationVariant::Alone => fam_id,
                AblationVariant::Delayed => -1,
                AblationVariant::Noised => -2,
                AblationVariant::Shuffled => -3,
            };
            ReplayOutcome::new(base_net + adj, base_tail + adj as i64)
        }
    }

    fn families() -> Vec<FeatureFamily> {
        vec![FeatureFamily(0), FeatureFamily(1), FeatureFamily(2)]
    }

    #[test]
    fn baseline_is_all_on() {
        let rep = run_ablation(&MockReplay, &families(), &AblationVariant::PER_FAMILY);
        // 3 families on -> 3*1000 net.
        assert_eq!(rep.baseline.net_lamports, 3_000);
        assert_eq!(rep.baseline.right_tail_bps, 30);
    }

    #[test]
    fn removed_variant_drops_a_bit() {
        let rep = run_ablation(&MockReplay, &families(), &[AblationVariant::Removed]);
        // 3 families -> 3 results.
        assert_eq!(rep.results.len(), 3);
        // Removing family 0: mask has 2 bits -> net 2000, adj -0 -> 2000.
        let r0 = rep
            .results
            .iter()
            .find(|r| r.family == FeatureFamily(0))
            .unwrap();
        assert_eq!(r0.net_lamports, 2_000);
        assert_eq!(r0.incremental_net_lamports, 2_000 - 3_000);
    }

    #[test]
    fn alone_variant_single_bit() {
        let rep = run_ablation(&MockReplay, &families(), &[AblationVariant::Alone]);
        let r2 = rep
            .results
            .iter()
            .find(|r| r.family == FeatureFamily(2))
            .unwrap();
        // only family 2 on -> 1 bit -> 1000 + adj(fam_id=2) = 1002.
        assert_eq!(r2.net_lamports, 1_002);
    }

    #[test]
    fn combined_variant_skipped_in_per_family() {
        // Passing Combined among variants must not create per-family rows for it.
        let rep = run_ablation(
            &MockReplay,
            &families(),
            &[AblationVariant::Combined, AblationVariant::Removed],
        );
        assert!(rep
            .results
            .iter()
            .all(|r| r.variant != AblationVariant::Combined));
        assert_eq!(rep.results.len(), 3); // only Removed × 3 families
    }

    #[test]
    fn determinism_repeat_identical() {
        let a = run_ablation(&MockReplay, &families(), &AblationVariant::PER_FAMILY);
        let b = run_ablation(&MockReplay, &families(), &AblationVariant::PER_FAMILY);
        assert_eq!(a, b);
    }

    #[test]
    fn output_ordered_by_family_then_variant() {
        // Deliberately unsorted input; output must be sorted.
        let fams = vec![FeatureFamily(2), FeatureFamily(0), FeatureFamily(1)];
        let rep = run_ablation(&MockReplay, &fams, &AblationVariant::PER_FAMILY);
        let mut prev: Option<(FeatureFamily, AblationVariant)> = None;
        for r in &rep.results {
            if let Some(p) = prev {
                assert!((p.0, p.1) < (r.family, r.variant));
            }
            prev = Some((r.family, r.variant));
        }
        // 3 families × 5 per-family variants = 15 rows.
        assert_eq!(rep.results.len(), 15);
    }

    #[test]
    fn duplicate_families_deduped() {
        let fams = vec![FeatureFamily(0), FeatureFamily(0), FeatureFamily(1)];
        let rep = run_ablation(&MockReplay, &fams, &[AblationVariant::Removed]);
        assert_eq!(rep.results.len(), 2);
    }

    #[test]
    fn toggle_mask_ops() {
        let m = FeatureToggleMask::all_on(&families());
        assert!(m.contains(FeatureFamily(1)));
        assert!(!m.without(FeatureFamily(1)).contains(FeatureFamily(1)));
        assert_eq!(FeatureToggleMask::only(FeatureFamily(3)).0, 1 << 3);
        assert_eq!(FeatureToggleMask::none().0, 0);
    }
}
