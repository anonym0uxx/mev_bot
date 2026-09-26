//! The narrative WIRE CODES are mirrored in two places: the enums in
//! `pump-quant-narrative` and the label table in `TokenIdentity::from_codes`.
//!
//! A mirror is a drift hazard, so this file is the guard: it reads the ACTUAL
//! discriminant values off the enums and asserts the proposal crate renders the
//! matching label for each. Change one side without the other and this fails.
//!
//! It also pins the reserved sentinel: code `0` means "never observed" for both
//! verdict and stage and must NEVER collide with a real variant. That invariant is
//! what lets an unobserved mint stay unobserved instead of being rendered as a
//! verdict nobody produced.

use pump_quant_narrative::alias_stage::AliasStage;
use pump_quant_narrative::entry_narrative::NarrativeVerdict;
use pump_quant_narrative::narrative_family::NarrativeFamily;
use pump_quant_proposal::TokenIdentity;

fn id(verdict: u8, stage: u8, family: u8) -> TokenIdentity {
    TokenIdentity::from_codes("name", "SYM", verdict, stage, family, 3, None)
}

#[test]
fn verdict_codes_render_their_own_labels() {
    for (v, label) in [
        (NarrativeVerdict::Eligible, "Eligible"),
        (NarrativeVerdict::Saturated, "Saturated"),
        (NarrativeVerdict::NoAttach, "NoAttach"),
        (NarrativeVerdict::Throwaway, "Throwaway"),
        (NarrativeVerdict::Unresolved, "Unresolved"),
    ] {
        assert_eq!(
            id(v as u8, 1, 1).verdict,
            label,
            "verdict code {} no longer renders {label} — the two tables drifted",
            v as u8
        );
        assert_ne!(
            v as u8, 0,
            "code 0 is the unobserved sentinel and cannot be a verdict"
        );
    }
}

#[test]
fn stage_codes_render_their_own_labels() {
    for (s, label) in [
        (AliasStage::Novel, "novel"),
        (AliasStage::Rising, "rising"),
        (AliasStage::Cresting, "cresting"),
        (AliasStage::Saturated, "saturated"),
    ] {
        assert_eq!(
            id(1, s as u8, 1).stage,
            label,
            "stage code {} no longer renders {label} — the two tables drifted",
            s as u8
        );
        assert_ne!(
            s as u8, 0,
            "code 0 is the unobserved sentinel and cannot be a stage"
        );
    }
}

#[test]
fn family_codes_render_their_own_labels() {
    for (f, label) in [
        (NarrativeFamily::Unclassified, "unclassified"),
        (NarrativeFamily::Animal, "animal"),
        (NarrativeFamily::Political, "political"),
        (NarrativeFamily::Celebrity, "celebrity"),
        (NarrativeFamily::Tech, "tech"),
        (NarrativeFamily::Derivative, "derivative"),
        (NarrativeFamily::Stream, "stream"),
        (NarrativeFamily::Seasonal, "seasonal"),
    ] {
        assert_eq!(
            id(1, 1, f as u8).family,
            label,
            "family code {} no longer renders {label} — the two tables drifted",
            f as u8
        );
    }
}

/// The sentinel: a mint that was never resolved, and a code we simply do not
/// understand, must both land on the honest "nothing observed" labels — never
/// coerced onto a real class that no producer ever emitted.
#[test]
fn zero_and_unknown_codes_are_not_coerced_onto_a_real_class() {
    let z = id(0, 0, 0);
    assert_eq!(z.verdict, "Unobserved");
    assert_eq!(z.stage, "unobserved");
    assert_eq!(z.family, "unclassified");

    let junk = id(200, 200, 200);
    assert_eq!(junk.verdict, "Unobserved");
    assert_eq!(junk.stage, "unobserved");
    assert_eq!(junk.family, "unclassified");
}

/// The identity reaches the rendered prompt, which is the whole point of the
/// wiring: the model must have the name to infer from.
#[test]
fn the_identity_reaches_the_rendered_prompt() {
    let rendered = pump_quant_proposal::render_identity(&Some(id(1, 2, 3)));
    assert!(rendered.contains("token_name=name"), "{rendered}");
    assert!(rendered.contains("token_symbol=SYM"), "{rendered}");
    assert!(
        rendered.contains("narrative_verdict=Eligible"),
        "{rendered}"
    );
    assert!(rendered.contains("narrative_stage=rising"), "{rendered}");
    assert!(
        rendered.contains("narrative_family=celebrity"),
        "{rendered}"
    );
}
