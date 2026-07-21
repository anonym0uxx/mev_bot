//! Leaf fa_admit: causal-hypothesis feature-admission guard (criterion 41).

use pump_quant_strategy::feature_admission::{
    admit_feature, AdmissionReject, AdmittedFeature, FeatureAdmissionRequest,
};

fn req(ch: Option<u64>, ex: Option<u64>, defeated: bool) -> FeatureAdmissionRequest {
    FeatureAdmissionRequest {
        feature_id: 42,
        causal_hypothesis_id: ch,
        experiment_id: ex,
        defeated_baseline: defeated,
    }
}

#[test]
fn admits_complete_request() {
    let r = req(Some(7), Some(13), true);
    assert_eq!(
        admit_feature(&r),
        Ok(AdmittedFeature {
            feature_id: 42,
            causal_hypothesis_id: 7,
            experiment_id: 13,
        })
    );
}

#[test]
fn rejects_missing_causal_hypothesis_first() {
    // Even with everything else missing, hypothesis is reported first.
    assert_eq!(
        admit_feature(&req(None, None, false)),
        Err(AdmissionReject::MissingCausalHypothesis)
    );
}

#[test]
fn rejects_missing_experiment() {
    assert_eq!(
        admit_feature(&req(Some(7), None, true)),
        Err(AdmissionReject::MissingExperiment)
    );
}

#[test]
fn rejects_undefeated_baseline() {
    assert_eq!(
        admit_feature(&req(Some(7), Some(13), false)),
        Err(AdmissionReject::BaselineNotDefeated)
    );
}

#[test]
fn all_four_combinations_of_ids() {
    for ch in [None, Some(1)] {
        for ex in [None, Some(2)] {
            for defeated in [false, true] {
                let got = admit_feature(&req(ch, ex, defeated));
                let expected = match (ch, ex, defeated) {
                    (None, _, _) => Err(AdmissionReject::MissingCausalHypothesis),
                    (Some(_), None, _) => Err(AdmissionReject::MissingExperiment),
                    (Some(_), Some(_), false) => Err(AdmissionReject::BaselineNotDefeated),
                    (Some(c), Some(e), true) => Ok(AdmittedFeature {
                        feature_id: 42,
                        causal_hypothesis_id: c,
                        experiment_id: e,
                    }),
                };
                assert_eq!(got, expected, "ch={ch:?} ex={ex:?} defeated={defeated}");
            }
        }
    }
}
