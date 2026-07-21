//! Leaf tests: the ten §29.8 determinant scorers. Expectations hand-computed.

use pump_quant_social::determinants::*;
use pump_quant_social::types::LifecyclePhase;

const HL: u64 = 1_000_000_000; // 1s half-life; ages of 0 give full weight.

#[test]
fn d1_single_and_mixed() {
    // Single call that doubled at every horizon → +10_000.
    let one = [MarkoutSample {
        price_at_call: 100,
        price_5m: 200,
        price_30m: 200,
        price_2h: 200,
        price_24h: 200,
        age_ns: 0,
    }];
    let s = d1_reconciled_markouts(&one, [1, 1, 1, 1], HL);
    assert_eq!(s.value_bps, 10_000);
    assert_eq!(s.sample_size, 1);

    // One +100% call and one -50% call, equal age → mean of (10000, -5000) = 2500.
    let two = [
        MarkoutSample {
            price_at_call: 100,
            price_5m: 200,
            price_30m: 200,
            price_2h: 200,
            price_24h: 200,
            age_ns: 0,
        },
        MarkoutSample {
            price_at_call: 100,
            price_5m: 50,
            price_30m: 50,
            price_2h: 50,
            price_24h: 50,
            age_ns: 0,
        },
    ];
    let s2 = d1_reconciled_markouts(&two, [1, 1, 1, 1], HL);
    assert_eq!(s2.value_bps, 2_500);
    assert_eq!(s2.sample_size, 2);

    assert_eq!(d1_reconciled_markouts(&[], [1, 1, 1, 1], HL).value_bps, 0);
}

#[test]
fn d1_decay_downweights_old_calls() {
    // A fresh +100% call and an old -50% call one half-life stale → the fresh call
    // weighs 10_000, the old one weighs 5_000: (10000*10000 + -5000*5000)/15000.
    let samples = [
        MarkoutSample {
            price_at_call: 100,
            price_5m: 200,
            price_30m: 200,
            price_2h: 200,
            price_24h: 200,
            age_ns: 0,
        },
        MarkoutSample {
            price_at_call: 100,
            price_5m: 50,
            price_30m: 50,
            price_2h: 50,
            price_24h: 50,
            age_ns: HL,
        },
    ];
    let s = d1_reconciled_markouts(&samples, [1, 1, 1, 1], HL);
    // (100_000_000 - 25_000_000) / 15_000 = 5_000.
    assert_eq!(s.value_bps, 5_000);
}

#[test]
fn d2_timing_and_persistence() {
    let samples = [
        LifecycleSample {
            phase: LifecyclePhase::PreFlow,
            age_ns: 0,
        },
        LifecycleSample {
            phase: LifecyclePhase::PostPeak,
            age_ns: 0,
        },
        LifecycleSample {
            phase: LifecyclePhase::PostPeak,
            age_ns: 0,
        },
    ];
    let r = d2_lifecycle_timing(&samples, HL, 6_000);
    // (8000 - 8000 - 8000)/3 = -2666.
    assert_eq!(r.score.value_bps, -2_666);
    // Post-peak share = 2/3 = 6666 bps > 6000 → persistent.
    assert!(r.post_peak_persistent);

    let pre = [LifecycleSample {
        phase: LifecyclePhase::PreFlow,
        age_ns: 0,
    }];
    let rp = d2_lifecycle_timing(&pre, HL, 6_000);
    assert_eq!(rp.score.value_bps, 8_000);
    assert!(!rp.post_peak_persistent);
}

#[test]
fn d3_excess_over_control() {
    let samples = [
        SelectionSample {
            call_markout_bps: 5_000,
            control_markout_bps: 1_000,
            age_ns: 0,
        },
        SelectionSample {
            call_markout_bps: 2_000,
            control_markout_bps: 3_000,
            age_ns: 0,
        },
    ];
    // excess = (4000, -1000) → mean 1500.
    assert_eq!(d3_selection_control(&samples, HL).value_bps, 1_500);

    // Property: a pure-selection account (call == control) shows ~zero excess.
    let selection_only: Vec<SelectionSample> = (0..10)
        .map(|i| SelectionSample {
            call_markout_bps: 3_000 + i * 100,
            control_markout_bps: 3_000 + i * 100,
            age_ns: 0,
        })
        .collect();
    assert_eq!(d3_selection_control(&selection_only, HL).value_bps, 0);
}

#[test]
fn d4_selectivity_discounts_spam() {
    // Within budget: no discount.
    let clean = d4_selectivity(3_000, 6_000, 5, 40);
    assert_eq!(clean.value_bps, 6_000);
    // 20 calls/day vs budget 5 → factor = 5/20 = 2500 bps → 6000*2500/10000 = 1500.
    let spam = d4_selectivity(20_000, 6_000, 5, 40);
    assert_eq!(spam.value_bps, 1_500);
}

#[test]
fn d5_skin_in_game_and_shill_flag() {
    let aligned = SkinInGameEvidence {
        funding_edges: 3,
        timing_edges: 1,
        metadata_reuse_edges: 0,
        buy_before_call: 8,
        distribute_into_call: 0,
        total_calls: 10,
    };
    let ra = d5_skin_in_game(&aligned, 3_000);
    assert_eq!(ra.score.value_bps, 8_000); // buy_share 8000, no dumping
    assert!(!ra.shill_suspect);

    let dumping = SkinInGameEvidence {
        funding_edges: 0,
        timing_edges: 0,
        metadata_reuse_edges: 0,
        buy_before_call: 2,
        distribute_into_call: 5,
        total_calls: 10,
    };
    let rd = d5_skin_in_game(&dumping, 3_000);
    // dump_share 5000 > 3000 → suspect; value = 2000 - 2*5000 clamped to -8000... wait
    // 2000 - 10000 = -8000 (already within band).
    assert_eq!(rd.score.value_bps, -8_000);
    assert!(rd.shill_suspect);
}

#[test]
fn d6_integrity_penalises_deletion() {
    let clean = IntegrityEvidence {
        deleted_losing_calls: 0,
        total_losing_calls: 4,
        edit_count: 0,
        total_calls: 10,
        disclosure_present: true,
    };
    // 10000 - 0 - 0 + 1500 - 5000 = 6500.
    assert_eq!(d6_integrity(&clean).value_bps, 6_500);

    let scrubber = IntegrityEvidence {
        deleted_losing_calls: 4,
        total_losing_calls: 4,
        edit_count: 0,
        total_calls: 10,
        disclosure_present: false,
    };
    // 10000 - 10000 + 0 - 5000 = -5000.
    assert_eq!(d6_integrity(&scrubber).value_bps, -5_000);
}

#[test]
fn d7_authenticity_and_farm_flag() {
    let organic = AudienceEvidence {
        reply_diversity_bps: 8_000,
        bot_reply_ratio_bps: 1_000,
        raid_pattern_count: 0,
        copy_echo_density_bps: 500,
        engagement_velocity_bps: 100,
        expected_velocity_bps: 100,
        sample_size: 50,
    };
    let ro = d7_audience_authenticity(&organic, 3_000);
    assert_eq!(ro.score.value_bps, 6_500); // 8000-1000-500
    assert!(!ro.bot_farm);

    let farm = AudienceEvidence {
        reply_diversity_bps: 1_000,
        bot_reply_ratio_bps: 5_000,
        raid_pattern_count: 3,
        copy_echo_density_bps: 4_000,
        engagement_velocity_bps: 900,
        expected_velocity_bps: 100,
        sample_size: 50,
    };
    let rf = d7_audience_authenticity(&farm, 3_000);
    assert!(rf.bot_farm); // bot 5000 > 3000 and velocity 900 > 2*100
    assert!(rf.score.value_bps < 0);
}

#[test]
fn d8_originality_and_echo_flag() {
    let orig = d8_originality(8, 2, 3_000);
    assert_eq!(orig.score.value_bps, 8_000);
    assert!(!orig.echo_heavy);

    let echo = d8_originality(1, 9, 3_000);
    assert_eq!(echo.score.value_bps, 1_000);
    assert!(echo.echo_heavy);

    assert_eq!(d8_originality(0, 0, 3_000).score.value_bps, 0);
}

#[test]
fn d9_category_skill_weights_by_sample_and_decay() {
    let perfs = [
        MetaPerf {
            meta_id: 1,
            markout_bps: 3_000,
            sample_size: 10,
            age_ns: 0,
        },
        MetaPerf {
            meta_id: 2,
            markout_bps: -1_000,
            sample_size: 10,
            age_ns: 0,
        },
    ];
    // equal weight 10 each → (3000 - 1000)/2 = 1000.
    let s = d9_category_skill(&perfs, HL);
    assert_eq!(s.value_bps, 1_000);
    assert_eq!(s.sample_size, 20);
}

#[test]
fn d10_clustering_is_fade_by_default() {
    // 3 converging sources, 1000 bps penalty each extra → -2000.
    let d = d10_clustering(3, 1_000, false, 0);
    assert_eq!(d.value_bps, -2_000);
    // A single source is no cluster → 0.
    assert_eq!(d10_clustering(1, 1_000, false, 0).value_bps, 0);
    // Admission overrides the fade default with the proven markout.
    let adm = d10_clustering(4, 1_000, true, 2_500);
    assert_eq!(adm.value_bps, 2_500);

    // Property: without admission, more sources → strictly more negative (until cap).
    let mut prev = 1i64;
    for k in 1u32..8 {
        let v = d10_clustering(k, 800, false, 0).value_bps;
        assert!(v <= prev);
        prev = v;
    }
}
