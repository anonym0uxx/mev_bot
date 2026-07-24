//! §21.4/§29.6 narrative-family classification: happy path per class,
//! no-evidence refusal, precision boundaries, determinism, and bounded input.

use pump_quant_narrative::narrative_family::matches_needle;
use pump_quant_narrative::{
    nv_family_classify, nv_family_classify_default, FamilyEvidence, FamilyEvidenceLane,
    NarrativeFamily, FAMILY_DERIVATIVE_SIMILARITY_BPS, FAMILY_LEXICON_V1, FAMILY_LEXICON_VERSION,
};

fn ev<'a>(name: &'a str, symbol: &'a str) -> FamilyEvidence<'a> {
    FamilyEvidence {
        name,
        symbol,
        live_stream_active: None,
        derivative_similarity_bps: None,
    }
}

fn family(name: &str, symbol: &str) -> NarrativeFamily {
    nv_family_classify_default(&ev(name, symbol)).family
}

// ---------------------------------------------------------------------------
// Happy path — the previously unreachable classes
// ---------------------------------------------------------------------------

#[test]
fn animal_family_is_reachable_from_lexical_evidence() {
    assert_eq!(family("Doge Killer", "DOGEK"), NarrativeFamily::Animal);
    assert_eq!(family("dogwifhat", "WIF"), NarrativeFamily::Animal);
    assert_eq!(family("Pepe Classic", "PEPEC"), NarrativeFamily::Animal);
    assert_eq!(family("Grumpy Cat", "GCAT"), NarrativeFamily::Animal);
    assert_eq!(family("Capybara Coin", "CAPY"), NarrativeFamily::Animal);
}

#[test]
fn seasonal_family_is_reachable_from_lexical_evidence() {
    assert_eq!(family("Santa Rally", "SANTA"), NarrativeFamily::Seasonal);
    assert_eq!(family("Halloween Pump", "SPOOK"), NarrativeFamily::Seasonal);
    assert_eq!(family("Pumpkin Spice", "PSPICE"), NarrativeFamily::Seasonal);
    // Symbol-only evidence counts too.
    assert_eq!(family("Untitled", "XMAS"), NarrativeFamily::Seasonal);
}

#[test]
fn stream_family_comes_from_metadata_only() {
    let e = FamilyEvidence {
        live_stream_active: Some(true),
        ..ev("Just Some Token", "JST")
    };
    let c = nv_family_classify_default(&e);
    assert_eq!(c.family, NarrativeFamily::Stream);
    assert_eq!(c.lane, FamilyEvidenceLane::LiveStream);
    assert_eq!(c.matched_needle, None);

    // There is deliberately no lexical detector for Stream: a name that merely
    // says "stream" is not evidence a stream is running.
    assert_eq!(
        family("Livestream Token", "LIVE"),
        NarrativeFamily::Unclassified,
        "no lexical Stream detector may be invented"
    );
}

#[test]
fn derivative_family_comes_from_measured_similarity_only() {
    let e = FamilyEvidence {
        derivative_similarity_bps: Some(FAMILY_DERIVATIVE_SIMILARITY_BPS),
        ..ev("Totally Original", "ORIG")
    };
    let c = nv_family_classify_default(&e);
    assert_eq!(c.family, NarrativeFamily::Derivative);
    assert_eq!(c.lane, FamilyEvidenceLane::MetadataSimilarity);
}

#[test]
fn political_celebrity_and_tech_families_classify() {
    assert_eq!(family("Trump Coin", "TRUMP"), NarrativeFamily::Political);
    assert_eq!(family("MAGA hat", "MAGA"), NarrativeFamily::Political);
    assert_eq!(family("Elon Rocket", "ELON"), NarrativeFamily::Celebrity);
    assert_eq!(family("Neural Net Coin", "NEURO"), NarrativeFamily::Tech);
    assert_eq!(family("AI agent", "AGENT"), NarrativeFamily::Tech);
}

#[test]
fn family_ordinals_are_stable_and_round_trip() {
    for (f, o) in [
        (NarrativeFamily::Unclassified, 0u8),
        (NarrativeFamily::Animal, 1),
        (NarrativeFamily::Political, 2),
        (NarrativeFamily::Celebrity, 3),
        (NarrativeFamily::Tech, 4),
        (NarrativeFamily::Derivative, 5),
        (NarrativeFamily::Stream, 6),
        (NarrativeFamily::Seasonal, 7),
    ] {
        assert_eq!(f.ordinal(), o);
        assert_eq!(NarrativeFamily::from_ordinal(o), Some(f));
    }
    assert_eq!(NarrativeFamily::from_ordinal(8), None);
}

// ---------------------------------------------------------------------------
// No-evidence refusal (§6.4)
// ---------------------------------------------------------------------------

#[test]
fn no_evidence_stays_unclassified() {
    let c = nv_family_classify_default(&ev("Zorbulon Prime", "ZRB"));
    assert_eq!(c.family, NarrativeFamily::Unclassified);
    assert_eq!(c.lane, FamilyEvidenceLane::NoEvidence);
    assert_eq!(c.matched_needle, None);
    assert_eq!(c.lexicon_version, FAMILY_LEXICON_VERSION);
}

#[test]
fn empty_and_absent_fields_never_fabricate_a_family() {
    assert_eq!(family("", ""), NarrativeFamily::Unclassified);
    // An unobserved metadata lane is not an observation of absence, and neither
    // is an observation of absence a family.
    let unobserved = FamilyEvidence {
        live_stream_active: None,
        derivative_similarity_bps: None,
        ..ev("", "")
    };
    let observed_absent = FamilyEvidence {
        live_stream_active: Some(false),
        derivative_similarity_bps: Some(0),
        ..ev("", "")
    };
    assert_eq!(
        nv_family_classify_default(&unobserved).family,
        NarrativeFamily::Unclassified
    );
    assert_eq!(
        nv_family_classify_default(&observed_absent).family,
        NarrativeFamily::Unclassified
    );
}

#[test]
fn similarity_below_the_gate_is_not_derivative() {
    let below = FamilyEvidence {
        derivative_similarity_bps: Some(FAMILY_DERIVATIVE_SIMILARITY_BPS - 1),
        ..ev("Nondescript", "NDS")
    };
    assert_eq!(
        nv_family_classify_default(&below).family,
        NarrativeFamily::Unclassified,
        "sharing a theme is not being a clone"
    );
    let at = FamilyEvidence {
        derivative_similarity_bps: Some(FAMILY_DERIVATIVE_SIMILARITY_BPS),
        ..ev("Nondescript", "NDS")
    };
    assert_eq!(
        nv_family_classify_default(&at).family,
        NarrativeFamily::Derivative,
        "the boundary is inclusive"
    );
}

// ---------------------------------------------------------------------------
// Precision — word-boundary needles must not fabricate
// ---------------------------------------------------------------------------

#[test]
fn short_needles_do_not_fire_inside_longer_words() {
    // The classic false positives a naive substring lexicon produces.
    assert_eq!(
        family("Catalyst Protocol", "CTLY"),
        NarrativeFamily::Unclassified
    );
    assert_eq!(
        family("Airdrop Season", "AIRD"),
        NarrativeFamily::Unclassified
    );
    assert_eq!(
        family("Bottom Signal", "BTM"),
        NarrativeFamily::Unclassified
    );
    assert_eq!(family("Dogma", "DGMA"), NarrativeFamily::Unclassified);
    assert_eq!(
        family("Frogger Arcade", "FRGR"),
        NarrativeFamily::Unclassified
    );
}

#[test]
fn word_needles_fire_at_real_boundaries() {
    for (name, symbol) in [
        ("cat", "CAT"),
        ("Space Cat", "SCAT"),
        ("cat-in-hat", "CIH"),
        ("MOON CAT 9000", "MC9"),
    ] {
        assert_eq!(
            family(name, symbol),
            NarrativeFamily::Animal,
            "expected Animal for {name}/{symbol}"
        );
    }
}

#[test]
fn substring_needles_still_match_inside_compounds() {
    // Long, distinctive needles are allowed to match anywhere.
    assert_eq!(family("Superdogecoin", "SDOGE"), NarrativeFamily::Animal);
    assert_eq!(
        family("ultrahalloweenmax", "UHM"),
        NarrativeFamily::Seasonal
    );
}

#[test]
fn matching_is_ascii_case_insensitive() {
    assert_eq!(family("DOGE", "doge"), NarrativeFamily::Animal);
    assert_eq!(family("DoGe", "DoGe"), NarrativeFamily::Animal);
    assert_eq!(family("SaNtA", "sAnTa"), NarrativeFamily::Seasonal);
}

#[test]
fn non_ascii_text_is_safe_and_counts_as_a_boundary() {
    // Multi-byte neighbours are non-word bytes, so a word needle still matches.
    assert_eq!(family("🐱cat🐱", "EMOJI"), NarrativeFamily::Animal);
    // And unrelated non-ASCII text classifies as nothing, without panicking.
    assert_eq!(
        family("日本語トークン", "JPY"),
        NarrativeFamily::Unclassified
    );
}

// ---------------------------------------------------------------------------
// Cascade / determinism / bounded input
// ---------------------------------------------------------------------------

#[test]
fn metadata_lanes_outrank_lexical_evidence() {
    // A dog token that is also a measured clone reads as Derivative.
    let clone = FamilyEvidence {
        derivative_similarity_bps: Some(9_000),
        live_stream_active: Some(true),
        ..ev("Doge Two", "DOGE2")
    };
    assert_eq!(
        nv_family_classify_default(&clone).family,
        NarrativeFamily::Derivative
    );
    // Without the similarity, the live stream wins over the lexicon.
    let streamed = FamilyEvidence {
        derivative_similarity_bps: None,
        ..clone
    };
    assert_eq!(
        nv_family_classify_default(&streamed).family,
        NarrativeFamily::Stream
    );
}

#[test]
fn lexicon_order_is_the_specificity_cascade() {
    // A seasonal dog is a seasonal meme: Seasonal precedes Animal.
    assert_eq!(family("Santa Doge", "SDOGE"), NarrativeFamily::Seasonal);
    // A political celebrity reads Political.
    assert_eq!(
        family("Trump Elon Combo", "TEC"),
        NarrativeFamily::Political
    );
}

#[test]
fn classification_is_deterministic_and_records_its_needle() {
    let e = ev("Halloween Doge", "HDOGE");
    let a = nv_family_classify_default(&e);
    let b = nv_family_classify_default(&e);
    assert_eq!(a, b);
    assert_eq!(a.matched_needle, Some("halloween"));
    assert_eq!(a.lexicon_version, FAMILY_LEXICON_VERSION);
}

#[test]
fn an_empty_lexicon_classifies_nothing() {
    let c = nv_family_classify(&ev("Doge", "DOGE"), &[], FAMILY_DERIVATIVE_SIMILARITY_BPS);
    assert_eq!(c.family, NarrativeFamily::Unclassified);
    // ...but the metadata lanes are lexicon-independent.
    let e = FamilyEvidence {
        live_stream_active: Some(true),
        ..ev("Doge", "DOGE")
    };
    assert_eq!(
        nv_family_classify(&e, &[], FAMILY_DERIVATIVE_SIMILARITY_BPS).family,
        NarrativeFamily::Stream
    );
}

#[test]
fn long_and_adversarial_inputs_do_not_panic() {
    let long = "a".repeat(100_000);
    assert_eq!(family(&long, &long), NarrativeFamily::Unclassified);
    let long_with_hit = format!("{long}doge{long}");
    assert_eq!(family(&long_with_hit, "X"), NarrativeFamily::Animal);
    // Needle longer than the haystack must not index out of bounds.
    assert_eq!(family("d", "d"), NarrativeFamily::Unclassified);
    // A zero threshold makes any measured similarity a clone — including zero.
    let c = nv_family_classify(
        &FamilyEvidence {
            derivative_similarity_bps: Some(0),
            ..ev("x", "x")
        },
        FAMILY_LEXICON_V1,
        0,
    );
    assert_eq!(c.family, NarrativeFamily::Derivative);
}

#[test]
fn empty_needle_never_matches() {
    use pump_quant_narrative::{MatchMode, Needle};
    for mode in [MatchMode::Substring, MatchMode::Word] {
        assert!(!matches_needle("anything", &Needle { text: "", mode }));
        assert!(!matches_needle("", &Needle { text: "", mode }));
    }
}
