//! Tape JSONL parser — integration coverage of the §62 artifact input schema.
use pump_quant_evaluator::ablation::AblationVariant;
use pump_quant_evaluator::evaluator_stats::Lane;
use pump_quant_evaluator::tape::parse_jsonl;

#[test]
fn parses_a_full_mixed_tape() {
    let input = concat!(
        "{\"kind\":\"trade\",\"lane\":\"early\",\"gross\":30000,\"fees\":300,\"tips\":100,\"failed\":0}\n",
        "{\"kind\":\"pvalue\",\"id\":7,\"p_ppm\":1000}\n",
        "{\"kind\":\"perf\",\"row\":[1,-2,3,-4]}\n",
        "{\"kind\":\"baseline_event\",\"index\":2,\"eligible\":true,\"launch\":true,\"score\":5,\"net_hold\":900,\"net_tpsl\":400}\n",
        "{\"kind\":\"ablation\",\"family\":3,\"variant\":\"noised\",\"net\":-50,\"tail\":-1}\n",
        "{\"kind\":\"candidate\",\"id\":7}\n",
    );
    let t = parse_jsonl(input).unwrap();
    assert_eq!(t.trades.len(), 1);
    assert_eq!(t.trades[0].lane, Lane::Early);
    assert_eq!(t.pvalues[0].id, 7);
    assert_eq!(t.perf[0], vec![1, -2, 3, -4]);
    assert_eq!(t.baseline_events[0].index, 2);
    assert_eq!(t.ablation[0].variant, AblationVariant::Noised);
    assert_eq!(t.candidate_id, Some(7));
}

#[test]
fn rejects_floats_and_bad_kinds() {
    assert!(parse_jsonl("{\"kind\":\"perf\",\"row\":[1.5]}").is_err());
    assert!(parse_jsonl("{\"kind\":\"nope\"}").is_err());
    assert!(parse_jsonl(
        "{\"kind\":\"trade\",\"lane\":\"x\",\"gross\":0,\"fees\":0,\"tips\":0,\"failed\":0}"
    )
    .is_err());
}

#[test]
fn comments_and_blank_lines_skipped() {
    let input = "# header\n\n{\"kind\":\"pvalue\",\"id\":1,\"p_ppm\":10}\n";
    let t = parse_jsonl(input).unwrap();
    assert_eq!(t.pvalues.len(), 1);
}
