//! Runtime-loaded ("dynamic") narrative lexicon.
//!
//! The pinned [`crate::narrative_family::FAMILY_LEXICON_V1`] is a compile-time
//! category taxonomy (63 needles, 8 families). It cannot track a meta that
//! rotates in hours, so this module adds the second layer: a lexicon that is
//! **caller-supplied data**, versioned and timestamped, matched with the exact
//! semantics of the pinned one via
//! [`crate::narrative_family::matches_needle_text`].
//!
//! Invariants (operator §22/§29/§6.4, and the crate's own):
//! * No `f32`/`f64`. Integers only; every subtraction is checked or saturating.
//! * No wall clock, no RNG, no network, no allocation. `now_ms` is SUPPLIED.
//! * The crate holds no growing state — the lexicon is a borrowed slice.
//! * **Pinned first, dynamic second.** A dynamic entry may EXTEND coverage; it
//!   may never override a pinned hit, so the specificity cascade keeps meaning.
//! * **Causality is an integer compare.** An entry is usable at `now_ms` iff
//!   `first_seen_ms <= as_of_ms <= now_ms`. Look-ahead is not representable.
//! * **Stale is its own state.** A stale lexicon in this environment is a WRONG
//!   lexicon, not a conservative one, so expiry is reported, never folded into
//!   `Unclassified` (which means "no evidence", a different fact).
//! * `ModelProposal` entries are QUARANTINE: measurement-only unless the caller
//!   explicitly opts in. Auto-trusting a model-proposed meta lets a hallucinated
//!   narrative create a self-fulfilling filter, and spinners learn to trigger it.

use crate::narrative_family::{matches_needle_text, MatchMode, NarrativeFamily};

/// Schema version of the on-disk dynamic lexicon artifact.
pub const DYNAMIC_LEXICON_SCHEMA_VERSION: u32 = 1;

/// Where an entry came from. Never collapsed: a model proposal is not evidence
/// of the same standing as a measured pipeline observation (authority hierarchy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Produced by the offline pipeline from observed data (mint stream, social).
    Pipeline,
    /// Proposed by the model. Quarantine unless the caller opts in.
    ModelProposal,
    /// Promoted by the operator.
    Operator,
}

impl Provenance {
    /// Whether this provenance may bind an entry decision without an explicit
    /// opt-in. Only measured/operator provenance qualifies.
    #[must_use]
    pub const fn is_trusted(self) -> bool {
        matches!(self, Provenance::Pipeline | Provenance::Operator)
    }
}

/// One needle in a runtime-loaded lexicon. Mirrors [`crate::narrative_family::Needle`]
/// but borrows its text, so the table can come from a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynNeedle<'a> {
    /// Needle text. Matched ASCII-case-insensitively.
    pub text: &'a str,
    /// How it is allowed to match.
    pub mode: MatchMode,
}

/// A family and its runtime-loaded needles, plus the entry's own validity window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynFamilyEntry<'a> {
    /// The family these needles evidence.
    pub family: NarrativeFamily,
    /// Needles, scanned in order (specificity cascade within the entry).
    pub needles: &'a [DynNeedle<'a>],
    /// When OUR pipeline first observed this alias in the wild.
    pub first_seen_ms: u64,
    /// When this entry became usable. Must be `>= first_seen_ms`.
    pub as_of_ms: u64,
    /// Shelf life. This environment's meta rotates in hours.
    pub ttl_ms: u64,
    /// Where the entry came from.
    pub provenance: Provenance,
    /// Confidence in bps, for ranking only. Never a gate on its own.
    pub confidence_bps: u16,
}

/// A versioned set of runtime-loaded entries.
///
/// The sealed journal digest must seed on `version`, NOT on `entries`, so a
/// fixed version stays byte-stable while the contents may be re-derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicLexicon<'a> {
    /// Monotonic version. Digest-relevant.
    pub version: u32,
    /// Entries, scanned in order after the pinned lexicon.
    pub entries: &'a [DynFamilyEntry<'a>],
}

/// Which table produced a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexiconSource {
    /// The pinned compile-time table.
    Pinned,
    /// A runtime-loaded entry.
    Dynamic,
}

/// Freshness of the dynamic layer AT the supplied instant. Reported, never
/// silently collapsed into `Unclassified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexiconFreshness {
    /// A dynamic lexicon was supplied and at least one entry was live at `now_ms`.
    Fresh,
    /// A dynamic lexicon was supplied but every entry had expired at `now_ms`.
    Stale,
    /// No dynamic lexicon was supplied at all (a coverage fact, not a failure).
    Absent,
}

/// The resolution of a token's narrative family across both tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyResolution<'a> {
    /// Resolved family, `Unclassified` when nothing fired.
    pub family: NarrativeFamily,
    /// Which table fired, when one did.
    pub source: Option<LexiconSource>,
    /// The needle text that matched, for auditability.
    pub matched_text: Option<&'a str>,
    /// Lexicon version in force at resolution time.
    pub version: u32,
    /// Freshness of the dynamic layer at `now_ms`.
    pub freshness: LexiconFreshness,
    /// Age of the newest live dynamic entry, when one was live.
    pub age_ms: Option<u64>,
}

impl FamilyResolution<'_> {
    /// Whether a family was resolved by any table.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.source.is_some()
    }
}

/// `now_ms` is inside an entry's validity window, and the entry's own window is
/// not itself malformed (`as_of_ms >= first_seen_ms`).
///
/// Malformed windows are treated as unusable rather than clamped: a
/// `first_seen_ms > as_of_ms` entry is a producer bug, and silently repairing it
/// would admit look-ahead.
#[must_use]
pub const fn entry_usable_at(e: &DynFamilyEntry<'_>, now_ms: u64) -> bool {
    e.first_seen_ms <= e.as_of_ms && e.as_of_ms <= now_ms
}

/// Whether the entry has expired at `now_ms`. Saturating: a `now_ms` before
/// `as_of_ms` cannot expire (it is not usable either — see [`entry_usable_at`]).
#[must_use]
pub const fn entry_expired_at(e: &DynFamilyEntry<'_>, now_ms: u64) -> bool {
    match now_ms.checked_sub(e.as_of_ms) {
        Some(age) => age > e.ttl_ms,
        None => false,
    }
}

/// Resolve a family across the pinned table then the optional dynamic table.
///
/// * `use_quarantine` — when `false` (the gating default), `ModelProposal`
///   entries are skipped entirely. Pass `true` only for measurement/analysis.
///
/// Pure, allocation-free, panic-free on any input.
#[must_use]
pub fn nv_family_resolve<'a>(
    name: &str,
    symbol: &str,
    pinned: &[crate::narrative_family::FamilyLexicon],
    dynamic: Option<&DynamicLexicon<'a>>,
    now_ms: u64,
    use_quarantine: bool,
) -> FamilyResolution<'a> {
    // 1. Pinned table, unchanged semantics. It always wins when it fires.
    for entry in pinned {
        for needle in entry.needles {
            if matches_needle_text(name, needle.text, needle.mode)
                || matches_needle_text(symbol, needle.text, needle.mode)
            {
                return FamilyResolution {
                    family: entry.family,
                    source: Some(LexiconSource::Pinned),
                    matched_text: Some(needle.text),
                    version: crate::narrative_family::FAMILY_LEXICON_VERSION,
                    freshness: freshness_of(dynamic, now_ms),
                    age_ms: None,
                };
            }
        }
    }

    // 2. Dynamic table — extends coverage only.
    if let Some(dl) = dynamic {
        for e in dl.entries {
            if !use_quarantine && !e.provenance.is_trusted() {
                continue;
            }
            if !entry_usable_at(e, now_ms) || entry_expired_at(e, now_ms) {
                continue;
            }
            for needle in e.needles {
                if matches_needle_text(name, needle.text, needle.mode)
                    || matches_needle_text(symbol, needle.text, needle.mode)
                {
                    return FamilyResolution {
                        family: e.family,
                        source: Some(LexiconSource::Dynamic),
                        matched_text: Some(needle.text),
                        version: dl.version,
                        freshness: LexiconFreshness::Fresh,
                        age_ms: Some(now_ms.saturating_sub(e.as_of_ms)),
                    };
                }
            }
        }
    }

    // 3. No evidence. Never a guess (§6.4).
    let freshness = freshness_of(dynamic, now_ms);
    FamilyResolution {
        family: NarrativeFamily::Unclassified,
        source: None,
        matched_text: None,
        version: match dynamic {
            Some(dl) => dl.version,
            None => crate::narrative_family::FAMILY_LEXICON_VERSION,
        },
        freshness,
        age_ms: None,
    }
}

/// Freshness of the dynamic layer, judged by whether ANY entry was live.
#[must_use]
fn freshness_of(dynamic: Option<&DynamicLexicon<'_>>, now_ms: u64) -> LexiconFreshness {
    let Some(dl) = dynamic else {
        return LexiconFreshness::Absent;
    };
    let mut any_usable = false;
    for e in dl.entries {
        if entry_usable_at(e, now_ms) && !entry_expired_at(e, now_ms) {
            any_usable = true;
            break;
        }
    }
    if any_usable {
        LexiconFreshness::Fresh
    } else {
        LexiconFreshness::Stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative_family::FAMILY_LEXICON_V1;

    const fn dsub(text: &str) -> DynNeedle<'_> {
        DynNeedle {
            text,
            mode: MatchMode::Substring,
        }
    }

    fn entry<'a>(
        family: NarrativeFamily,
        needles: &'a [DynNeedle<'a>],
        first_seen_ms: u64,
        as_of_ms: u64,
        ttl_ms: u64,
        provenance: Provenance,
    ) -> DynFamilyEntry<'a> {
        DynFamilyEntry {
            family,
            needles,
            first_seen_ms,
            as_of_ms,
            ttl_ms,
            provenance,
            confidence_bps: 8_000,
        }
    }

    const N_ANSEM: &[DynNeedle<'_>] = &[dsub("ansem")];
    const N_SPACEX: &[DynNeedle<'_>] = &[dsub("spacex"), dsub("spcx")];

    #[test]
    fn pinned_still_wins_and_is_unchanged() {
        let entries = [entry(
            NarrativeFamily::Celebrity,
            N_ANSEM,
            1_000,
            1_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 7,
            entries: &entries,
        };
        // "dog" is a pinned Animal needle; a dynamic entry must not shadow it.
        let r = nv_family_resolve("dog", "DOG", FAMILY_LEXICON_V1, Some(&dl), 2_000, false);
        assert_eq!(r.family, NarrativeFamily::Animal);
        assert_eq!(r.source, Some(LexiconSource::Pinned));
    }

    #[test]
    fn dynamic_extends_coverage_where_pinned_is_silent() {
        let entries = [entry(
            NarrativeFamily::Celebrity,
            N_ANSEM,
            1_000,
            1_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 7,
            entries: &entries,
        };
        let r = nv_family_resolve(
            "SAYING ANSEM 1 MILL TIMES",
            "ANSEM",
            FAMILY_LEXICON_V1,
            Some(&dl),
            2_000,
            false,
        );
        assert_eq!(r.family, NarrativeFamily::Celebrity);
        assert_eq!(r.source, Some(LexiconSource::Dynamic));
        assert_eq!(r.matched_text, Some("ansem"));
        assert_eq!(r.version, 7);
        assert_eq!(r.freshness, LexiconFreshness::Fresh);
    }

    #[test]
    fn causality_is_an_integer_compare_no_lookahead() {
        let entries = [entry(
            NarrativeFamily::Tech,
            N_SPACEX,
            5_000,
            9_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 3,
            entries: &entries,
        };
        // Before as_of_ms: not usable, even though first_seen already happened.
        let r = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dl), 8_999, false);
        assert_eq!(r.family, NarrativeFamily::Unclassified);
        assert_eq!(r.source, None);
        // At/after as_of_ms: usable.
        let r2 = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dl), 9_000, false);
        assert_eq!(r2.family, NarrativeFamily::Tech);
        // Malformed window (first_seen > as_of) is never usable.
        let bad = [entry(
            NarrativeFamily::Tech,
            N_SPACEX,
            9_000,
            5_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dlb = DynamicLexicon {
            version: 4,
            entries: &bad,
        };
        let r3 = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dlb), 10_000, false);
        assert_eq!(r3.source, None);
    }

    #[test]
    fn stale_is_its_own_state_not_unclassified() {
        let entries = [entry(
            NarrativeFamily::Tech,
            N_SPACEX,
            1_000,
            1_000,
            60_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 3,
            entries: &entries,
        };
        let r = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dl), 1_000_001, false);
        assert_eq!(r.family, NarrativeFamily::Unclassified);
        assert_eq!(r.freshness, LexiconFreshness::Stale);
        assert!(r.age_ms.is_none());
        // Absent is distinct from Stale.
        let r2 = nv_family_resolve("SPACEX", "SPCX", &[], None, 1_000_001, false);
        assert_eq!(r2.freshness, LexiconFreshness::Absent);
    }

    #[test]
    fn model_proposals_are_quarantined_unless_opted_in() {
        let entries = [entry(
            NarrativeFamily::Tech,
            N_SPACEX,
            1_000,
            1_000,
            86_400_000,
            Provenance::ModelProposal,
        )];
        let dl = DynamicLexicon {
            version: 9,
            entries: &entries,
        };
        let gated = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dl), 2_000, false);
        assert_eq!(gated.source, None, "a model proposal must not bind a gate");
        let measured = nv_family_resolve("SPACEX", "SPCX", &[], Some(&dl), 2_000, true);
        assert_eq!(measured.family, NarrativeFamily::Tech);
    }

    #[test]
    fn word_mode_still_requires_boundaries() {
        let needles = [DynNeedle {
            text: "bull",
            mode: MatchMode::Word,
        }];
        let entries = [entry(
            NarrativeFamily::Animal,
            &needles,
            1_000,
            1_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 1,
            entries: &entries,
        };
        // "bullish" must NOT match a Word needle.
        let r = nv_family_resolve("bullish", "BULLISH", &[], Some(&dl), 2_000, false);
        assert_eq!(r.source, None);
        let r2 = nv_family_resolve("the bull", "BULL", &[], Some(&dl), 2_000, false);
        assert_eq!(r2.family, NarrativeFamily::Animal);
    }

    #[test]
    fn empty_needle_never_matches() {
        let needles = [dsub("")];
        let entries = [entry(
            NarrativeFamily::Tech,
            &needles,
            1_000,
            1_000,
            86_400_000,
            Provenance::Pipeline,
        )];
        let dl = DynamicLexicon {
            version: 1,
            entries: &entries,
        };
        let r = nv_family_resolve("anything", "ANY", &[], Some(&dl), 2_000, false);
        assert_eq!(r.source, None);
    }

    #[test]
    fn pinned_table_alone_has_no_regression() {
        // The dynamic path must not change what the pinned path already decided.
        let with_none =
            nv_family_resolve("christmas dog", "XMAS", FAMILY_LEXICON_V1, None, 0, false);
        let pinned_only = crate::narrative_family::nv_family_classify_default(
            &crate::narrative_family::FamilyEvidence {
                name: "christmas dog",
                symbol: "XMAS",
                live_stream_active: None,
                derivative_similarity_bps: None,
            },
        );
        assert_eq!(with_none.family, pinned_only.family);
        assert_eq!(with_none.matched_text, pinned_only.matched_needle);
    }
}
