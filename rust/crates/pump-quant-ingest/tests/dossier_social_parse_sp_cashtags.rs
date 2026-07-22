// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_parse' component (leaf 'sp_cashtags').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_parse::*;

#[test]
fn sp_cashtags_case_fold_dedup_and_bounds() {
    let (h, n) = extract_cashtags("gm $wif and $WIF then $BONK $bonk");
    assert_eq!(n, 2, "case-folded duplicates collapse to one");
    assert_eq!(h[0], fnv1a_64(b"WIF"));
    assert_eq!(h[1], fnv1a_64(b"BONK"));
    // Too short and too long are both rejected.
    let (_, none) = extract_cashtags("$X $TOOOOOOOOOOONG");
    assert_eq!(none, 0);
    // Bound holds even when many tickers appear.
    let (_, capped) = extract_cashtags("$AA $BB $CC $DD $EE $FF $GG $HH $II $JJ");
    assert_eq!(capped as usize, MAX_CASHTAGS);
}
