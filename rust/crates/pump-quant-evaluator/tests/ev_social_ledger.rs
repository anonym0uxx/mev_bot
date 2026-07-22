use pump_quant_evaluator::social_ledger::*;

fn call(source: u64, call_ts: u64, feat_ts: u64, net: i128, fav: bool) -> SocialCall {
    SocialCall {
        source_id: SourceId(source),
        call_ts_ns: call_ts,
        feature_ts_ns: feat_ts,
        realized_net_lamports: net,
        realized_favorable: fav,
    }
}

#[test]
fn time_safe_flag_matches_d3_rule() {
    assert!(call(1, 100, 100, 0, true).is_time_safe()); // equal ok
    assert!(call(1, 100, 50, 0, true).is_time_safe()); // before ok
    assert!(!call(1, 100, 101, 0, true).is_time_safe()); // after = look-ahead
}

#[test]
fn lookahead_calls_excluded_from_quality() {
    // Source 1: three admissible (2 favorable), one look-ahead (favorable but
    // rejected). Admissible net = 100 + 100 + (-40) = 160. quality = 2/3 in bps
    // = 2*10000/3 = 6666 (integer floor). The look-ahead's +999 net and its
    // favorability must NOT count.
    let calls = [
        call(1, 100, 90, 100, true),
        call(1, 200, 200, 100, true),
        call(1, 300, 250, -40, false),
        call(1, 400, 500, 999, true), // look-ahead: excluded
    ];
    let out = reconcile_social_quality(&calls);
    assert_eq!(out.len(), 1);
    let sc = out[0];
    assert_eq!(sc.source_id, SourceId(1));
    assert_eq!(sc.n_total, 4);
    assert_eq!(sc.n_admissible, 3);
    assert_eq!(sc.n_lookahead_rejected, 1);
    assert_eq!(sc.n_favorable, 2);
    assert_eq!(sc.net_lamports, 160);
    assert_eq!(sc.quality_bps, QualityBps::Bps(6666));
}

#[test]
fn source_with_only_lookahead_is_missing_quality() {
    let calls = [call(7, 100, 150, 500, true)];
    let out = reconcile_social_quality(&calls);
    assert_eq!(out.len(), 1);
    let sc = out[0];
    assert_eq!(sc.n_admissible, 0);
    assert_eq!(sc.n_lookahead_rejected, 1);
    assert_eq!(sc.net_lamports, 0);
    assert!(sc.quality_bps.is_missing());
}

#[test]
fn multiple_sources_grouped_ascending() {
    let calls = [
        call(5, 10, 10, 50, true),
        call(2, 10, 10, -10, false),
        call(5, 20, 20, 30, true),
    ];
    let out = reconcile_social_quality(&calls);
    // Deterministic ascending SourceId order: 2 then 5.
    assert_eq!(out[0].source_id, SourceId(2));
    assert_eq!(out[1].source_id, SourceId(5));
    // Source 5: 2 admissible, both favorable -> 10000 bps, net 80.
    assert_eq!(out[1].n_admissible, 2);
    assert_eq!(out[1].net_lamports, 80);
    assert_eq!(out[1].quality_bps, QualityBps::Bps(10_000));
    // Source 2: 1 admissible, 0 favorable -> 0 bps.
    assert_eq!(out[0].quality_bps, QualityBps::Bps(0));
}

#[test]
fn empty_input_yields_no_scorecards() {
    assert!(reconcile_social_quality(&[]).is_empty());
}
