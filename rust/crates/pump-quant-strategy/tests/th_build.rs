//! Leaf th_build: deterministic per-entry thesis construction (criterion 43).

use pump_quant_strategy::strategy_id::fnv1a_64;
use pump_quant_strategy::thesis::{build_thesis, Direction, ThesisCondition, ThesisInputs};

fn sample_inputs() -> ThesisInputs {
    ThesisInputs {
        entry_mode: 2,
        archetype: 5,
        entry_ts_ns: 1_000,
        required: vec![
            ThesisCondition {
                feature_id: 10,
                direction: Direction::AtLeast,
                threshold_fp: 100,
                min_completeness_bps: 8_000,
                freshness_bound_ns: 500,
            },
            ThesisCondition {
                feature_id: 11,
                direction: Direction::AtMost,
                threshold_fp: 50,
                min_completeness_bps: 5_000,
                freshness_bound_ns: 1_000,
            },
        ],
        invalidation: vec![ThesisCondition {
            feature_id: 20,
            direction: Direction::AtLeast,
            threshold_fp: 9_000,
            min_completeness_bps: 6_000,
            freshness_bound_ns: 2_000,
        }],
        evidence_refs: vec![7, 8, 9],
    }
}

#[test]
fn identical_inputs_yield_identical_thesis() {
    let t1 = build_thesis(&sample_inputs());
    let t2 = build_thesis(&sample_inputs());
    assert_eq!(t1, t2);
    assert_eq!(t1.thesis_id, t2.thesis_id);
    assert_eq!(t1.canonical_bytes(), t2.canonical_bytes());
}

#[test]
fn thesis_id_is_hash_of_canonical_bytes() {
    let t = build_thesis(&sample_inputs());
    // The id is fnv1a over the canonical bytes (which exclude the id field).
    assert_eq!(t.thesis_id, fnv1a_64(&t.canonical_bytes()));
}

#[test]
fn any_input_change_changes_id() {
    let base = build_thesis(&sample_inputs());

    let mut i = sample_inputs();
    i.entry_mode = 3;
    assert_ne!(build_thesis(&i).thesis_id, base.thesis_id);

    let mut i = sample_inputs();
    i.entry_ts_ns = 2_000;
    assert_ne!(build_thesis(&i).thesis_id, base.thesis_id);

    let mut i = sample_inputs();
    i.required[0].threshold_fp = 101;
    assert_ne!(build_thesis(&i).thesis_id, base.thesis_id);

    let mut i = sample_inputs();
    i.invalidation[0].feature_id = 21;
    assert_ne!(build_thesis(&i).thesis_id, base.thesis_id);

    let mut i = sample_inputs();
    i.evidence_refs.push(10);
    assert_ne!(build_thesis(&i).thesis_id, base.thesis_id);
}

#[test]
fn fields_copied_faithfully() {
    let i = sample_inputs();
    let t = build_thesis(&i);
    assert_eq!(t.entry_mode, i.entry_mode);
    assert_eq!(t.archetype, i.archetype);
    assert_eq!(t.created_at_ns, i.entry_ts_ns);
    assert_eq!(t.required, i.required);
    assert_eq!(t.invalidation, i.invalidation);
    assert_eq!(t.evidence_refs, i.evidence_refs);
}
