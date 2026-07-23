//! §50 ablation harness — integration coverage with a deterministic mock closure.
use pump_quant_evaluator::ablation::{
    run_ablation, AblationReplay, AblationVariant, FeatureFamily, FeatureToggleMask, ReplayOutcome,
};

struct Mock;

impl AblationReplay for Mock {
    fn replay(
        &self,
        toggles: FeatureToggleMask,
        variant: AblationVariant,
        family: Option<FeatureFamily>,
    ) -> ReplayOutcome {
        let bits = toggles.0.count_ones() as i128;
        let fam = family.map(|f| f.0 as i128).unwrap_or(0);
        let adj = match variant {
            AblationVariant::Combined => 0,
            AblationVariant::Removed => -fam,
            AblationVariant::Alone => fam,
            _ => -1,
        };
        ReplayOutcome::new(bits * 1_000 + adj, bits as i64 * 10)
    }
}

#[test]
fn harness_is_deterministic() {
    let fams = vec![FeatureFamily(0), FeatureFamily(1), FeatureFamily(2)];
    let a = run_ablation(&Mock, &fams, &AblationVariant::PER_FAMILY);
    let b = run_ablation(&Mock, &fams, &AblationVariant::PER_FAMILY);
    assert_eq!(a, b);
    assert_eq!(a.baseline.net_lamports, 3_000);
    // 3 families × 5 per-family variants.
    assert_eq!(a.results.len(), 15);
}

#[test]
fn incremental_measured_against_baseline() {
    let fams = vec![FeatureFamily(1)];
    let rep = run_ablation(&Mock, &fams, &[AblationVariant::Removed]);
    let r = &rep.results[0];
    assert_eq!(
        r.incremental_net_lamports,
        r.net_lamports - rep.baseline.net_lamports
    );
}

#[test]
fn output_sorted_by_family_then_variant() {
    let fams = vec![FeatureFamily(2), FeatureFamily(0)];
    let rep = run_ablation(&Mock, &fams, &AblationVariant::PER_FAMILY);
    let mut prev: Option<(FeatureFamily, AblationVariant)> = None;
    for r in &rep.results {
        if let Some(p) = prev {
            assert!((p.0, p.1) < (r.family, r.variant));
        }
        prev = Some((r.family, r.variant));
    }
}
