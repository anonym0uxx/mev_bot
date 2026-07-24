//! Tests for the deterministic category classifier v0 and MetaRotationState.
//! Expectations computed by hand against `TAXONOMY_V0`.

use pump_quant_market_state::common::Completeness;
use pump_quant_market_state::meta::{
    category_flow_imbalance_bps, category_needle_matches, classify_category, rotation_between,
    CategoryEvent, CategoryEventKind, CategoryMatchMode, CategoryMeasures, CategoryNeedle,
    MetaRotationReducer, MetaRotationState, CATEGORY_UNCLASSIFIED, TAXONOMY_V0, TAXONOMY_V1,
    TAXONOMY_VERSION_V0, TAXONOMY_VERSION_V1,
};

#[test]
fn classifier_assigns_named_categories_deterministically() {
    let slot = 500;
    let animals = classify_category("DogeToTheMoon", "DOGE", &TAXONOMY_V0, slot);
    assert_eq!(animals.category_id, 1);
    assert_eq!(animals.assigned_at_slot, slot);
    assert_eq!(animals.taxonomy_version, 0);

    assert_eq!(
        classify_category("TrumpWin2024", "MAGA", &TAXONOMY_V0, slot).category_id,
        2
    );
    assert_eq!(
        classify_category("ElonToMars", "MUSK", &TAXONOMY_V0, slot).category_id,
        3
    );
    assert_eq!(
        classify_category("NeuralAgent", "GPT", &TAXONOMY_V0, slot).category_id,
        4
    );
}

#[test]
fn classifier_is_case_insensitive_and_matches_symbol_or_name() {
    // Keyword only in symbol.
    assert_eq!(
        classify_category("ZZZ Token", "pepe", &TAXONOMY_V0, 1).category_id,
        1
    );
    // Uppercase name.
    assert_eq!(
        classify_category("BIDEN COIN", "XYZ", &TAXONOMY_V0, 1).category_id,
        2
    );
}

#[test]
fn classifier_returns_unclassified_when_no_keyword_matches() {
    let a = classify_category("Zorble", "ZRB", &TAXONOMY_V0, 42);
    assert_eq!(a.category_id, CATEGORY_UNCLASSIFIED);
    assert_eq!(a.assigned_at_slot, 42); // still timestamped
}

#[test]
fn classifier_first_match_wins_by_scan_order() {
    // Name contains both an animal keyword ("dog") and an ai keyword ("ai").
    // cat1 (animals) is scanned before cat4 (ai), so animals wins.
    let a = classify_category("dogbrain", "AIDOG", &TAXONOMY_V0, 1);
    assert_eq!(a.category_id, 1);
}

#[test]
fn classifier_is_time_safe_pure_of_slot_value() {
    // The assignment id must not depend on the slot; only the stamp changes.
    let a1 = classify_category("PepeKing", "PEPE", &TAXONOMY_V0, 10);
    let a2 = classify_category("PepeKing", "PEPE", &TAXONOMY_V0, 9_999_999);
    assert_eq!(a1.category_id, a2.category_id);
    assert_eq!(a1.assigned_at_slot, 10);
    assert_eq!(a2.assigned_at_slot, 9_999_999);
}

#[test]
fn reducer_accumulates_on_chain_measures() {
    let mut r = MetaRotationReducer::new(0, 64, 256);
    // Category 1: two launches by two distinct creators, one repeat creator.
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Launch { creator: 100 },
        slot: 1,
    });
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Launch { creator: 100 }, // same creator again
        slot: 2,
    });
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Launch { creator: 101 },
        slot: 3,
    });
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Buy {
            quote_lamports: 3_000_000,
        },
        slot: 4,
    });
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Sell {
            quote_lamports: 1_000_000,
        },
        slot: 5,
    });
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Graduation,
        slot: 6,
    });

    let snap = r.snapshot();
    let c1 = snap.category(1).unwrap();
    assert_eq!(c1.launches, 3);
    assert_eq!(c1.unique_creators, 2); // 100 dedup + 101
    assert_eq!(c1.buy_quote, 3_000_000);
    assert_eq!(c1.sell_quote, 1_000_000);
    assert_eq!(c1.net_flow, 2_000_000); // 3M - 1M
    assert_eq!(c1.graduations, 1);
    assert_eq!(snap.total_launches, 3);
    assert_eq!(snap.completeness, Completeness::Complete);
    // launch share of the only category is 100% = 10_000 bps.
    assert_eq!(snap.launch_share_bps(1), Some(10_000));
    // flow imbalance = (3M-1M)/(3M+1M) = 2M/4M = 5000 bps.
    assert_eq!(category_flow_imbalance_bps(c1), Some(5000));
}

#[test]
fn reducer_category_capacity_bounds_and_reports_incomplete() {
    // Only 1 category slot; ingest 2 distinct categories -> overflow.
    let mut r = MetaRotationReducer::new(0, 1, 8);
    r.ingest(&CategoryEvent {
        category_id: 1,
        kind: CategoryEventKind::Launch { creator: 1 },
        slot: 1,
    });
    r.ingest(&CategoryEvent {
        category_id: 2,
        kind: CategoryEventKind::Launch { creator: 2 },
        slot: 2,
    });
    let snap = r.snapshot();
    assert_eq!(snap.completeness, Completeness::Incomplete);
    // total_launches still counts both (market-wide lower bound preserved).
    assert_eq!(snap.total_launches, 2);
    // only category 1 tracked.
    assert!(snap.category(1).is_some());
    assert!(snap.category(2).is_none());
}

/// Build a MetaRotationState directly for controlled rotation math.
fn state(cats: &[(u64, u64, i128)], total_launches: u64) -> MetaRotationState {
    let categories = cats
        .iter()
        .map(|&(id, launches, net_flow)| CategoryMeasures {
            category_id: id,
            launches,
            graduations: 0,
            buy_quote: if net_flow >= 0 { net_flow as u128 } else { 0 },
            sell_quote: if net_flow < 0 { (-net_flow) as u128 } else { 0 },
            net_flow,
            buy_count: 0,
            sell_count: 0,
            unique_creators: 0,
            completeness: Completeness::Complete,
        })
        .collect();
    MetaRotationState {
        taxonomy_version: 0,
        categories,
        total_launches,
        total_buy_quote: 0,
        completeness: Completeness::Complete,
    }
}

#[test]
fn rotation_emergence_and_share_math() {
    // earlier: cat1 10 launches flow 1_000_000, cat2 10 launches flow 200_000; total 20.
    let earlier = state(&[(1, 10, 1_000_000), (2, 10, 200_000)], 20);
    // later: cat1 4 launches flow 100_000, cat2 36 launches flow 6_000_000; total 40.
    let later = state(&[(1, 4, 100_000), (2, 36, 6_000_000)], 40);

    let rot = rotation_between(&earlier, &later, 3000);
    // ordered by id: cat1 then cat2.
    let c1 = rot.iter().find(|r| r.category_id == 1).unwrap();
    let c2 = rot.iter().find(|r| r.category_id == 2).unwrap();

    // cat1 share: earlier 10/20=5000, later 4/40=1000 -> change -4000.
    assert_eq!(c1.launch_share_change_bps, -4000);
    assert_eq!(c1.net_flow_change, 100_000 - 1_000_000);
    assert_eq!(c1.launch_growth_bps, Some(4000)); // 4/10 = 4000 bps
    assert!(!c1.emerging);
    assert!(!c1.saturating); // later share 1000 < 3000

    // cat2 share: earlier 5000, later 36/40=9000 -> change +4000.
    assert_eq!(c2.launch_share_change_bps, 4000);
    assert_eq!(c2.net_flow_change, 6_000_000 - 200_000);
    assert_eq!(c2.launch_growth_bps, Some(36000)); // 36/10
    assert!(c2.emerging); // share up AND flow up
    assert!(!c2.saturating); // flow change positive
}

#[test]
fn rotation_saturation_and_emergence_from_zero() {
    // Saturating: high later share, non-positive flow change.
    let earlier = state(&[(5, 5, 5_000_000)], 10);
    let later = state(&[(5, 8, 4_000_000)], 10);
    let rot = rotation_between(&earlier, &later, 3000);
    let c5 = rot.iter().find(|r| r.category_id == 5).unwrap();
    // later share 8/10 = 8000 >= 3000, flow change = 4M-5M = -1M <= 0 -> saturating.
    assert!(c5.saturating);
    assert!(!c5.emerging);

    // Emergence from zero: category absent earlier, present later.
    let earlier2 = state(&[(1, 10, 0)], 10);
    let later2 = state(&[(1, 10, 0), (9, 7, 3_000_000)], 17);
    let rot2 = rotation_between(&earlier2, &later2, 3000);
    let c9 = rot2.iter().find(|r| r.category_id == 9).unwrap();
    assert!(c9.emerging_from_zero);
    assert_eq!(c9.launch_growth_bps, None); // grew from zero -> undefined ratio
}

#[test]
fn property_shares_sum_bounded_and_rotation_covers_union() {
    // Over several constructed pairs, every rotation entry's later launch share
    // must be <= 10_000 bps and the output must cover the union of ids.
    for seed in 0..30u64 {
        let a = 1 + seed % 5;
        let b = 6 + seed % 5;
        let earlier = state(
            &[(a, 1 + seed % 4, 1000), (b, 2 + seed % 3, 500)],
            10 + seed,
        );
        let later = state(&[(a, seed % 6, 2000), (b, 1 + seed % 7, 100)], 12 + seed);
        let rot = rotation_between(&earlier, &later, 2500);
        // union coverage
        assert!(rot.iter().any(|r| r.category_id == a));
        assert!(rot.iter().any(|r| r.category_id == b));
        // ordered ascending by id
        for w in rot.windows(2) {
            assert!(w[0].category_id <= w[1].category_id);
        }
        // each later share bounded
        for r in &rot {
            if let Some(sh) = later.launch_share_bps(r.category_id) {
                assert!(sh <= 10_000);
            }
        }
    }
}

// ===========================================================================
// TAXONOMY v0 → v1: the word-boundary fix, applied FORWARD under a version bump
// (criterion 81 — assignments are timestamped and never retroactive).
//
// `category_id` is a brain RECALL FILTER KEY: a mis-assignment pools a token with
// the wrong meta's episodes and silently corrupts every conditioned estimate keyed
// on it. v0's naive substring matching mis-categorizes ordinary English. v0 is
// pinned here AS THE HISTORICAL RECORD (so a replay of a v0-stamped assignment
// still reproduces it), and v1 is pinned as the fix.
// ===========================================================================

/// The six proven v0 mis-classifications: `(name, symbol, wrong v0 category)`.
/// Every one is an incidental English substring hit — `fair`→`ai`, `catalyst`→
/// `cat`, `bottom`→`bot`, `bullish`→`bull`, `starter`→`star`, `magazine`→`maga`.
const V0_MISCLASSIFICATIONS: &[(&str, &str, u64)] = &[
    ("Fair Launch", "FAIR", 4),   // "ai" inside "fair"      → AI
    ("Catalyst", "CTLY", 1),      // "cat" inside "catalyst" → Animal
    ("Bottom Signal", "BTM", 4),  // "bot" inside "bottom"   → AI
    ("Bullish Chain", "BLCH", 1), // "bull" inside "bullish" → Animal
    ("Starter Pack", "STRP", 3),  // "star" inside "starter" → Celebrity
    ("Magazine Coin", "MAGZ", 2), // "maga" inside "magazine"→ Political
];

#[test]
fn v0_substring_misclassifications_are_pinned_as_the_historical_record() {
    // v0 is FROZEN. These are wrong, they are known to be wrong, and they must
    // keep reproducing exactly — a v0-stamped assignment on disk means this.
    for &(name, symbol, wrong) in V0_MISCLASSIFICATIONS {
        let a = classify_category(name, symbol, &TAXONOMY_V0, 7);
        assert_eq!(
            a.category_id, wrong,
            "v0 must stay bit-identical: {name} historically assigned {wrong}"
        );
        assert_eq!(a.taxonomy_version, TAXONOMY_VERSION_V0);
    }
}

#[test]
fn v1_word_boundary_discipline_fixes_every_named_misclassification() {
    for &(name, symbol, wrong) in V0_MISCLASSIFICATIONS {
        let a = classify_category(name, symbol, &TAXONOMY_V1, 7);
        assert_eq!(
            a.category_id, CATEGORY_UNCLASSIFIED,
            "v1 must not assign {name} (v0 wrongly said {wrong}); no lexical \
             evidence means UNCLASSIFIED (§6.4), never a guess"
        );
        assert_eq!(
            a.taxonomy_version, TAXONOMY_VERSION_V1,
            "the fix ships under a BUMPED version, never as a rewrite of v0"
        );
    }
}

#[test]
fn v1_still_assigns_every_genuine_category_hit() {
    // The fix must not cost recall on real hits: a word-boundary needle still
    // fires at a boundary, and long distinctive needles still match as substrings.
    let cases: &[(&str, &str, u64)] = &[
        ("Bull Run", "BULL", 1),      // word("bull") at a boundary
        ("Cat Coin", "CAT", 1),       // word("cat") at a boundary
        ("DogeToTheMoon", "DOGE", 1), // sub("doge") inside a compound
        ("dogwifhat", "WIF", 1),      // sub("dog") — compound animal ticker
        ("Shiba Inu", "INU", 1),      // word("inu") at a boundary
        ("TrumpWin2024", "MAGA", 2),  // word("maga") == whole symbol
        ("Elon To Mars", "ELON", 3),  // word("elon") at a boundary
        ("Starlord Star", "STAR", 3), // word("star") — symbol boundary hit
        ("AI Agent", "AI", 4),        // word("ai") at a boundary
        ("Neural Net", "NRL", 4),     // sub("neural")
        ("bot army", "BOT", 4),       // word("bot") at a boundary
    ];
    for &(name, symbol, want) in cases {
        assert_eq!(
            classify_category(name, symbol, &TAXONOMY_V1, 1).category_id,
            want,
            "v1 must still assign {name}/{symbol} to {want}"
        );
    }
}

#[test]
fn v1_keeps_stable_ids_and_only_moves_the_version_stamp() {
    // Ids are stable ACROSS versions (criterion 81): a v0 and a v1 assignment of
    // the same unambiguous token are directly comparable; only the stamp differs.
    for (name, symbol) in [("DogeToTheMoon", "DOGE"), ("Neural Net", "NRL")] {
        let v0 = classify_category(name, symbol, &TAXONOMY_V0, 3);
        let v1 = classify_category(name, symbol, &TAXONOMY_V1, 3);
        assert_eq!(v0.category_id, v1.category_id);
        assert_eq!(v0.taxonomy_version, 0);
        assert_eq!(v1.taxonomy_version, 1);
        assert_eq!(v0.assigned_at_slot, v1.assigned_at_slot);
    }
}

#[test]
fn needle_match_modes_are_exactly_substring_vs_word_boundary() {
    let s = CategoryNeedle {
        text: "ai",
        mode: CategoryMatchMode::Substring,
    };
    let w = CategoryNeedle {
        text: "ai",
        mode: CategoryMatchMode::Word,
    };
    assert!(category_needle_matches("fair", &s));
    assert!(!category_needle_matches("fair", &w));
    assert!(category_needle_matches("AI", &w), "case-insensitive");
    assert!(category_needle_matches("ai-agent", &w), "hyphen boundary");
    assert!(category_needle_matches("the ai", &w), "trailing boundary");
    assert!(!category_needle_matches("aid", &w));
    // Non-ASCII bytes are non-word, so a needle adjacent to them still matches.
    assert!(category_needle_matches("\u{2728}ai\u{2728}", &w));
    // Empty needle never fires; over-long needle never fires; no panics.
    let empty = CategoryNeedle {
        text: "",
        mode: CategoryMatchMode::Word,
    };
    assert!(!category_needle_matches("anything", &empty));
    assert!(!category_needle_matches("a", &w));
}
