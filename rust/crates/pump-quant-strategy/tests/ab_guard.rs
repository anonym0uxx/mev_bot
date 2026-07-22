//! Leaf ab_guard: human-annotation boundary guard (criterion 46).

use pump_quant_strategy::annotation_boundary::{
    admit_annotation_as_fact, annotation_admissible_as_fact, annotation_to_advisory,
    gate_with_annotation, AdvisoryNote, AnnotationFactError, HumanAnnotation,
};

#[test]
fn annotation_lands_only_in_advisory() {
    let a = HumanAnnotation::test();
    assert_eq!(
        annotation_to_advisory(&a),
        AdvisoryNote {
            note: a.note.clone(),
            author: a.author,
        }
    );
}

#[test]
fn annotation_never_admissible_as_fact() {
    let a = HumanAnnotation::test();
    assert_eq!(
        admit_annotation_as_fact(&a),
        Err(AnnotationFactError::AdvisoryOnly)
    );
    assert!(!annotation_admissible_as_fact());
}

#[test]
fn annotation_cannot_flip_a_failing_gate() {
    let a = HumanAnnotation {
        note: "trust me, this is safe".to_string(),
        author: 1,
        sealed_at_ns: 5,
    };
    // A failing gate stays failing regardless of a positive-sounding annotation.
    assert!(!gate_with_annotation(false, &a));
    // A passing gate stays passing (annotation can't force-fail it either).
    assert!(gate_with_annotation(true, &a));
}

#[test]
fn gate_verdict_is_annotation_independent() {
    let a1 = HumanAnnotation::test();
    let a2 = HumanAnnotation {
        note: "totally different note".to_string(),
        author: 99,
        sealed_at_ns: 12_345,
    };
    for pass in [false, true] {
        assert_eq!(gate_with_annotation(pass, &a1), pass);
        assert_eq!(gate_with_annotation(pass, &a2), pass);
    }
}
