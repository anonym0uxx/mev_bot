// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_source' component (leaf 'ss_parse_batch').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_source::*;

#[test]
fn ss_parse_batch_order_and_skip_bad() {
    let good_x = RawSocialPayload::new(
        br#"{"platform":"x","author":"a","text":"$WIF","likes":1}"#.to_vec(),
        1,
    );
    let bad = RawSocialPayload::new(b"garbage".to_vec(), 2);
    let good_tg = RawSocialPayload::new(
        br#"{"platform":"telegram","author":"b","text":"$PEPE","likes":1}"#.to_vec(),
        3,
    );
    let events = parse_batch(&[good_x, bad, good_tg]);
    assert_eq!(events.len(), 2, "bad payload skipped, not fatal");
    assert_eq!(
        events[0].platform,
        pump_quant_ingest::social_parse::SocialPlatform::X
    );
    assert_eq!(
        events[1].platform,
        pump_quant_ingest::social_parse::SocialPlatform::Telegram
    );
    assert_eq!(events[1].observed_at_ns, 3, "capture instant preserved");
}
