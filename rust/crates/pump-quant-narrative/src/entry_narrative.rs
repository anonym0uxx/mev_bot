//! The entry precondition: is there positive evidence of a RISING narrative?
//!
//! Operator ruling (2026-09-26): narrative is a **binding precondition on the
//! entry path**, not an enrichment. A real memecoin trader only enters a coin
//! they are SURE matches the attention narrative, so an unidentifiable name is a
//! REFUSAL, not a haircut.
//!
//! The predicate is `positive evidence of a RISING attention narrative`,
//! satisfied by EITHER lane:
//! * **Lane L — lexical attachment**: the name attaches to a live alias whose
//!   stage is [`AliasStage::Novel`] or [`AliasStage::Rising`].
//! * **Lane A — attention emergence**: the attention plane shows emergence
//!   independently of the name (`nv_attention_state` / `nv_meta_emergence` /
//!   `nv_lifecycle_stage`).
//!
//! It must NOT be satisfied by "the name is in my vocabulary". Measured: names
//! attaching to a currently-hot alias graduate 2.00% vs 2.89% for names
//! attaching to no hot alias — vocabulary presence selects the SATURATED cohort
//! and refuses the not-yet-hot cohort that outperforms.
//!
//! Every refusal carries a NAMED cause for the journal. No float, no clock, no
//! I/O; `now_ms` is supplied.

use crate::alias_stage::AliasStage;
use crate::dynamic_lexicon::{FamilyResolution, LexiconFreshness};
use crate::narrative_family::{matches_needle_text, MatchMode, NarrativeFamily};

/// Which lane satisfied the precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionLane {
    /// Name attaches to a live alias with a positive stage.
    Lexical,
    /// The attention plane shows emergence independently of the name.
    Attention,
}

/// The entry precondition's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrativeVerdict {
    /// Positive evidence of a rising narrative. The rest of the EV gate decides.
    Eligible,
    /// Attached, but cresting or crowded: the crowd is already there.
    Saturated,
    /// Resolved and attaches to nothing live (model verdict, positive evidence).
    NoAttach,
    /// Throwaway/scatology name. Cheap pre-filter, not the signal.
    Throwaway,
    /// We could not determine. Uncertainty is a REFUSAL.
    Unresolved,
}

impl NarrativeVerdict {
    /// Whether the precondition admits the entry.
    #[must_use]
    pub const fn admits(self) -> bool {
        matches!(self, NarrativeVerdict::Eligible)
    }
}

/// One needle and its mode, pinned as a table.
pub struct ThrowawayNeedle {
    /// Needle text.
    pub text: &'static str,
    /// Match mode.
    pub mode: MatchMode,
}

const fn tw(text: &'static str) -> ThrowawayNeedle {
    ThrowawayNeedle {
        text,
        mode: MatchMode::Word,
    }
}

/// Throwaway-name needles v1.
///
/// Scatology/sexual terms only. This is a cheap VETO with no measured edge of
/// its own (8,661 of 798,430 tokens, 1.08%, graduating 0.9% vs 0.7% for the
/// rest), which is why it is a pre-filter and not the signal. It exists to keep
/// obvious junk out of the model's attention budget, never to substitute for the
/// narrative inference.
pub const THROWAWAY_NEEDLES_V1: &[ThrowawayNeedle] = &[
    tw("poop"),
    tw("shit"),
    tw("shid"),
    tw("fart"),
    tw("penis"),
    tw("dick"),
    tw("cock"),
    tw("cum"),
    tw("sex"),
    tw("porn"),
    tw("tits"),
    tw("boobs"),
    tw("anus"),
    tw("butthole"),
    tw("rape"),
    tw("nazi"),
    tw("hitler"),
];

/// Whether the name or symbol carries a throwaway needle.
#[must_use]
pub fn nv_is_throwaway(name: &str, symbol: &str) -> bool {
    for n in THROWAWAY_NEEDLES_V1 {
        if matches_needle_text(name, n.text, n.mode) || matches_needle_text(symbol, n.text, n.mode)
        {
            return true;
        }
    }
    false
}

/// Everything the precondition needs, all causal.
#[derive(Debug, Clone, Copy)]
pub struct NarrativeInputs<'a> {
    /// Token name as observed in launch metadata.
    pub name: &'a str,
    /// Token symbol.
    pub symbol: &'a str,
    /// The two-table family resolution.
    pub resolution: &'a FamilyResolution<'a>,
    /// Alias stage, when the alias layer observed this attachment.
    pub stage: Option<AliasStage>,
    /// Attention emergence: `Some(true)` rising, `Some(false)` observed flat or
    /// decaying, `None` not observed. Absence never becomes `false`.
    pub attention_emerging: Option<bool>,
    /// The model's explicit "this name attaches to nothing live" verdict.
    /// `None` means it did not say.
    pub model_no_attach: Option<bool>,
}

/// The precondition's output: verdict, named cause, and the evidence that
/// produced it (criterion 47 inspectability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NarrativeDecision {
    /// The verdict.
    pub verdict: NarrativeVerdict,
    /// Stable, greppable cause token for the journal.
    pub cause: &'static str,
    /// Family in force at the decision.
    pub family: NarrativeFamily,
    /// Stage in force at the decision.
    pub stage: Option<AliasStage>,
    /// Lane that satisfied the precondition, when one did.
    pub lane: Option<ResolutionLane>,
    /// Freshness of the dynamic lexicon at the decision instant.
    pub freshness: LexiconFreshness,
}

/// Evaluate the entry precondition.
///
/// Pure, allocation-free, panic-free on any input. Refusal causes, in the order
/// the cascade can produce them:
/// `narrative_throwaway`, `narrative_saturated`, `attached_stage_unobserved`,
/// `narrative_eligible`, `narrative_no_attach`, `narrative_unresolved`.
#[must_use]
pub fn nv_narrative_verdict(i: &NarrativeInputs<'_>) -> NarrativeDecision {
    let freshness = i.resolution.freshness;
    let family = i.resolution.family;

    let mk = |verdict: NarrativeVerdict, cause: &'static str, lane: Option<ResolutionLane>| {
        NarrativeDecision {
            verdict,
            cause,
            family,
            stage: i.stage,
            lane,
            freshness,
        }
    };

    // 1. Throwaway pre-filter. Cheapest check, first.
    if nv_is_throwaway(i.name, i.symbol) {
        return mk(NarrativeVerdict::Throwaway, "narrative_throwaway", None);
    }

    // 2. Attached: the stage decides. Attachment WITHOUT a stage is not positive
    //    evidence — measured attachment-as-hot was worse than no attachment — so
    //    an unobserved stage is Unresolved, not Eligible.
    if i.resolution.is_resolved() {
        return match i.stage {
            Some(s) if s.is_positive_attachment() => mk(
                NarrativeVerdict::Eligible,
                "narrative_eligible",
                Some(ResolutionLane::Lexical),
            ),
            Some(_) => mk(NarrativeVerdict::Saturated, "narrative_saturated", None),
            None => mk(
                NarrativeVerdict::Unresolved,
                "attached_stage_unobserved",
                None,
            ),
        };
    }

    // 3. Unattached: Lane A can still satisfy the precondition.
    if i.attention_emerging == Some(true) {
        return mk(
            NarrativeVerdict::Eligible,
            "narrative_eligible",
            Some(ResolutionLane::Attention),
        );
    }

    // 4. A model verdict of "attaches to nothing live" is positive evidence of a
    //    resolved negative. A lexicon miss alone is NOT (coverage is thin).
    if i.model_no_attach == Some(true) {
        return mk(NarrativeVerdict::NoAttach, "narrative_no_attach", None);
    }

    // 5. Stale vocabulary is not evidence, and it is reported as such rather
    //    than folded into "no attach".
    if freshness == LexiconFreshness::Stale {
        return mk(
            NarrativeVerdict::Unresolved,
            "narrative_lexicon_stale",
            None,
        );
    }

    mk(NarrativeVerdict::Unresolved, "narrative_unresolved", None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_lexicon::{FamilyResolution, LexiconSource};

    fn res(
        family: NarrativeFamily,
        resolved: bool,
        freshness: LexiconFreshness,
    ) -> FamilyResolution<'static> {
        FamilyResolution {
            family,
            source: if resolved {
                Some(LexiconSource::Dynamic)
            } else {
                None
            },
            matched_text: if resolved { Some("ansem") } else { None },
            version: 1,
            freshness,
            age_ms: Some(1_000),
        }
    }

    fn inputs<'a>(
        name: &'a str,
        r: &'a FamilyResolution<'a>,
        stage: Option<AliasStage>,
        attention: Option<bool>,
        no_attach: Option<bool>,
    ) -> NarrativeInputs<'a> {
        NarrativeInputs {
            name,
            symbol: "SYM",
            resolution: r,
            stage,
            attention_emerging: attention,
            model_no_attach: no_attach,
        }
    }

    #[test]
    fn throwaway_is_refused_before_anything_else() {
        let r = res(NarrativeFamily::Celebrity, true, LexiconFreshness::Fresh);
        let i = inputs("poop coin", &r, Some(AliasStage::Novel), Some(true), None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::Throwaway);
        assert_eq!(d.cause, "narrative_throwaway");
        assert!(!d.verdict.admits());
    }

    #[test]
    fn attached_and_novel_is_eligible() {
        let r = res(NarrativeFamily::Celebrity, true, LexiconFreshness::Fresh);
        let i = inputs("saying ansem", &r, Some(AliasStage::Novel), None, None);
        let d = nv_narrative_verdict(&i);
        assert!(d.verdict.admits());
        assert_eq!(d.lane, Some(ResolutionLane::Lexical));
        assert_eq!(d.cause, "narrative_eligible");
    }

    #[test]
    fn attached_but_saturated_is_refused() {
        let r = res(NarrativeFamily::Celebrity, true, LexiconFreshness::Fresh);
        let i = inputs("mayhem", &r, Some(AliasStage::Saturated), None, None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::Saturated);
        assert_eq!(d.cause, "narrative_saturated");
    }

    #[test]
    fn attachment_without_a_stage_is_not_positive_evidence() {
        let r = res(NarrativeFamily::Celebrity, true, LexiconFreshness::Fresh);
        let i = inputs("unknown meta", &r, None, None, None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::Unresolved);
        assert_eq!(d.cause, "attached_stage_unobserved");
    }

    #[test]
    fn lane_a_satisfies_the_precondition_without_a_lexical_hit() {
        let r = res(
            NarrativeFamily::Unclassified,
            false,
            LexiconFreshness::Absent,
        );
        let i = inputs("brand new thing", &r, None, Some(true), None);
        let d = nv_narrative_verdict(&i);
        assert!(d.verdict.admits());
        assert_eq!(d.lane, Some(ResolutionLane::Attention));
    }

    #[test]
    fn unresolved_is_a_refusal() {
        let r = res(
            NarrativeFamily::Unclassified,
            false,
            LexiconFreshness::Absent,
        );
        let i = inputs("brand new thing", &r, None, None, None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::Unresolved);
        assert!(!d.verdict.admits());
        assert_eq!(d.cause, "narrative_unresolved");
    }

    #[test]
    fn model_verdict_of_no_attach_is_a_distinct_cause() {
        let r = res(
            NarrativeFamily::Unclassified,
            false,
            LexiconFreshness::Fresh,
        );
        let i = inputs("nothing at all", &r, None, Some(false), Some(true));
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::NoAttach);
        assert_eq!(d.cause, "narrative_no_attach");
    }

    #[test]
    fn stale_lexicon_is_its_own_refusal_cause() {
        let r = res(
            NarrativeFamily::Unclassified,
            false,
            LexiconFreshness::Stale,
        );
        let i = inputs("brand new thing", &r, None, None, None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.cause, "narrative_lexicon_stale");
    }

    #[test]
    fn attention_false_alone_does_not_assert_no_attach() {
        let r = res(
            NarrativeFamily::Unclassified,
            false,
            LexiconFreshness::Fresh,
        );
        let i = inputs("brand new thing", &r, None, Some(false), None);
        let d = nv_narrative_verdict(&i);
        assert_eq!(d.verdict, NarrativeVerdict::Unresolved);
    }
}
