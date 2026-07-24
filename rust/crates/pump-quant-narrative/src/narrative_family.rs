//! §21.4/§29.6 **narrative family** — the subject-matter taxonomy, classified
//! only from deterministic lexical and launch-metadata evidence.
//!
//! ## What this is, and what it is not
//! [`crate::narrative::NarrativeClass`] answers *how does this narrative
//! behave* — how fast it decays, how high it can reach (trend / news / tech /
//! culture). That axis is unchanged and untouched by this module.
//!
//! [`NarrativeFamily`] answers a different question: *what is this token
//! about* — the animal meme, the political meme, the live-stream, the seasonal
//! calendar meme. Downstream episodic recall keys on this family as a nominal
//! field, and several of its slots were unreachable because nothing in the
//! system attempted the subject-matter read at all. This module supplies it.
//!
//! ## §6.4 under-claiming beats fabricating
//! A family is emitted only where deterministic evidence actually supports it:
//!
//! * **Animal / Political / Celebrity / Tech / Seasonal** — a lexical hit in the
//!   token name or symbol against a versioned needle list
//!   ([`FAMILY_LEXICON_V1`]).
//! * **Stream** — the launch-metadata live-stream flag, and *only* that. There
//!   is deliberately no lexicon for this family: "live"/"stream" appear in far
//!   too many unrelated tickers to be evidence, and a guess dressed as a
//!   detector is worse than an honest `Unclassified`.
//! * **Derivative** — a measured metadata-mimicry similarity to a pre-existing
//!   token at/above a named threshold, and only that.
//!
//! Every metadata input is an [`Option`]: `None` means the lane was never
//! observed, which is not the same as observing its absence, and neither is
//! allowed to manufacture a family. No evidence ⇒
//! [`NarrativeFamily::Unclassified`], which is the honest UNKNOWN carrier of
//! this axis (§29.5 — absence stays absence).
//!
//! ## Precision of lexical matching
//! Naive substring matching over short needles fabricates families: `"cat"`
//! fires on *catalyst*, `"ai"` on *airdrop*, `"bot"` on *bottom*. Each needle
//! therefore declares its own [`MatchMode`]: long, unambiguous needles
//! (`"halloween"`, `"doge"`) match as substrings, while short or
//! English-common needles (`"cat"`, `"ai"`, `"inu"`) must match at a
//! word boundary — start/end of the field, or adjacent to a non-alphanumeric
//! byte. Matching is ASCII-case-insensitive and performed byte-wise with no
//! allocation and no lowercase copy; non-ASCII bytes compare as-is.
//!
//! ## §22 determinism
//! Pure, total function of its inputs: no float, no clock, no RNG, no I/O, no
//! allocation, and no interior state — so there is nothing to bound and nothing
//! to evict. Identical inputs always yield an identical family, and the
//! versioned lexicon is stamped onto every result so a historical
//! classification stays interpretable.

/// Version stamped onto every [`FamilyClassification`] (§21.4 versioned
/// taxonomy; criterion 81 — an assignment keeps the meaning it was made with).
pub const FAMILY_LEXICON_VERSION: u32 = 1;

/// Default metadata-mimicry similarity, in bps, at/above which a launch reads
/// as [`NarrativeFamily::Derivative`] (§29.6).
///
/// 7 000 bps = 70% similarity to a pre-existing token's metadata. Below this a
/// launch shares a theme; at or above it, it is a clone of a specific token,
/// which is the claim the `Derivative` slot actually makes.
pub const FAMILY_DERIVATIVE_SIMILARITY_BPS: u32 = 7_000;

/// Coarse subject-matter family of a token's narrative (§21.4/§29.6). Nominal.
///
/// The discriminants are the dense ordinals the downstream episodic-recall
/// fingerprint uses for its nominal narrative field, so the mapping across the
/// crate boundary is the identity on [`Self::ordinal`] and cannot silently
/// drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum NarrativeFamily {
    /// No deterministic evidence supported any family (§6.4 UNKNOWN carrier).
    Unclassified = 0,
    /// Animal / mascot memes.
    Animal = 1,
    /// Political / current-events memes.
    Political = 2,
    /// Celebrity or influencer tie-in.
    Celebrity = 3,
    /// Technology or AI themed.
    Tech = 4,
    /// Metadata-mimicry clone of an already-running token.
    Derivative = 5,
    /// Live-stream / streamer driven (launch-metadata evidence only).
    Stream = 6,
    /// Recurring seasonal or calendar meme.
    Seasonal = 7,
}

impl NarrativeFamily {
    /// Dense ordinal used for one-hot encoding.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::Unclassified),
            1 => Some(Self::Animal),
            2 => Some(Self::Political),
            3 => Some(Self::Celebrity),
            4 => Some(Self::Tech),
            5 => Some(Self::Derivative),
            6 => Some(Self::Stream),
            7 => Some(Self::Seasonal),
            _ => None,
        }
    }
}

/// How a lexical needle is allowed to match (see the module docs on precision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Matches anywhere inside the field. Reserved for needles long and
    /// distinctive enough that an incidental hit is not a realistic concern.
    Substring,
    /// Must match at a word boundary: start/end of the field, or adjacent to a
    /// non-alphanumeric byte. Required for short or English-common needles.
    Word,
}

/// One lexical needle and its match mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needle {
    /// Lowercase ASCII needle text.
    pub text: &'static str,
    /// How it is allowed to match.
    pub mode: MatchMode,
}

/// Shorthand for a substring needle.
const fn sub(text: &'static str) -> Needle {
    Needle {
        text,
        mode: MatchMode::Substring,
    }
}

/// Shorthand for a word-boundary needle.
const fn word(text: &'static str) -> Needle {
    Needle {
        text,
        mode: MatchMode::Word,
    }
}

/// A family and the needles that evidence it.
#[derive(Debug, Clone, Copy)]
pub struct FamilyLexicon {
    /// The family these needles evidence.
    pub family: NarrativeFamily,
    /// Needles, scanned in order.
    pub needles: &'static [Needle],
}

/// The versioned lexical evidence table (§21.4).
///
/// Scanned in declaration order, first family with a hit wins, so the ordering
/// is the specificity cascade: a needle set that makes a *narrower* claim is
/// tried before a broader one. Seasonal precedes the rest because its needles
/// are calendar-specific; Animal is last because its needles are the most
/// likely to co-occur with another family's (a seasonal dog is a seasonal
/// meme). `Stream` and `Derivative` are absent by design — they are metadata
/// lanes, not lexical ones.
pub const FAMILY_LEXICON_V1: &[FamilyLexicon] = &[
    FamilyLexicon {
        family: NarrativeFamily::Seasonal,
        needles: &[
            sub("christmas"),
            word("xmas"),
            sub("santa"),
            sub("halloween"),
            sub("spooky"),
            sub("pumpkin"),
            sub("thanksgiving"),
            sub("easter"),
            sub("valentine"),
            sub("newyear"),
            sub("mistletoe"),
            sub("reindeer"),
        ],
    },
    FamilyLexicon {
        family: NarrativeFamily::Political,
        needles: &[
            sub("trump"),
            sub("biden"),
            word("maga"),
            word("potus"),
            sub("kamala"),
            sub("obama"),
            sub("putin"),
            sub("zelensky"),
            sub("election"),
            sub("president"),
            sub("politic"),
            sub("senate"),
        ],
    },
    FamilyLexicon {
        family: NarrativeFamily::Celebrity,
        needles: &[
            sub("elon"),
            word("musk"),
            sub("kanye"),
            sub("mrbeast"),
            sub("ronaldo"),
            word("messi"),
            sub("oprah"),
            sub("bezos"),
            word("zuck"),
            sub("swift"),
            sub("celeb"),
        ],
    },
    FamilyLexicon {
        family: NarrativeFamily::Tech,
        needles: &[
            word("ai"),
            word("gpt"),
            word("llm"),
            sub("neural"),
            sub("openai"),
            sub("deepseek"),
            sub("quantum"),
            word("agent"),
            word("robot"),
            word("algo"),
            sub("cyborg"),
        ],
    },
    FamilyLexicon {
        family: NarrativeFamily::Animal,
        needles: &[
            sub("doge"),
            sub("shiba"),
            word("shib"),
            word("inu"),
            sub("pepe"),
            sub("bonk"),
            word("wif"),
            word("cat"),
            word("dog"),
            sub("kitty"),
            sub("puppy"),
            sub("hippo"),
            sub("capybara"),
            sub("penguin"),
            sub("hamster"),
            word("frog"),
            word("wolf"),
        ],
    },
];

/// Deterministic evidence available at classification time.
///
/// Every metadata field is an [`Option`]: `None` means the lane was not
/// observed, which never becomes a family (§29.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FamilyEvidence<'a> {
    /// Token name as observed in launch metadata.
    pub name: &'a str,
    /// Token symbol / ticker as observed in launch metadata.
    pub symbol: &'a str,
    /// Whether a live stream was active for this launch, if that lane was
    /// observed at all.
    pub live_stream_active: Option<bool>,
    /// Measured metadata similarity to a pre-existing token, in bps, if
    /// similarity scoring ran at all.
    pub derivative_similarity_bps: Option<u32>,
}

/// Which evidence lane produced a family (criterion 47 inspectability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyEvidenceLane {
    /// Nothing fired; the family is [`NarrativeFamily::Unclassified`].
    NoEvidence,
    /// Measured metadata-mimicry similarity cleared its threshold.
    MetadataSimilarity,
    /// The launch-metadata live-stream flag was observed active.
    LiveStream,
    /// A lexical needle matched the name or symbol.
    Lexical,
}

/// The classification plus the evidence that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyClassification {
    /// The assigned family.
    pub family: NarrativeFamily,
    /// Which lane fired.
    pub lane: FamilyEvidenceLane,
    /// The needle that matched, when [`FamilyEvidenceLane::Lexical`] fired.
    /// Retained so an assignment is auditable against the lexicon version.
    pub matched_needle: Option<&'static str>,
    /// Lexicon version in force at assignment time (criterion 81).
    pub lexicon_version: u32,
}

impl FamilyClassification {
    /// The no-evidence result (§6.4).
    #[must_use]
    pub const fn unclassified() -> Self {
        FamilyClassification {
            family: NarrativeFamily::Unclassified,
            lane: FamilyEvidenceLane::NoEvidence,
            matched_needle: None,
            lexicon_version: FAMILY_LEXICON_VERSION,
        }
    }
}

/// Whether `b` is an ASCII alphanumeric byte (a "word" byte for boundary
/// purposes). Non-ASCII bytes are treated as non-word, so a needle adjacent to
/// multi-byte text still matches at a boundary.
const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// ASCII-case-insensitive test of whether `hay[at..]` starts with `needle`.
fn starts_with_ci(hay: &[u8], at: usize, needle: &[u8]) -> bool {
    let Some(slice) = hay.get(at..at + needle.len()) else {
        return false;
    };
    for (h, n) in slice.iter().zip(needle.iter()) {
        if !h.eq_ignore_ascii_case(n) {
            return false;
        }
    }
    true
}

/// Whether `needle` occurs in `hay` under `mode`, ASCII-case-insensitively.
///
/// Allocation-free: the haystack is scanned in place with byte-wise case
/// folding rather than being lowercased into a new buffer. An empty needle
/// never matches (it would otherwise fire on every token).
#[must_use]
pub fn matches_needle(hay: &str, needle: &Needle) -> bool {
    let n = needle.text.as_bytes();
    let h = hay.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return false;
    }
    let last_start = h.len() - n.len();
    let mut i = 0usize;
    while i <= last_start {
        if starts_with_ci(h, i, n) {
            match needle.mode {
                MatchMode::Substring => return true,
                MatchMode::Word => {
                    let before_ok = match i.checked_sub(1).and_then(|p| h.get(p)) {
                        Some(b) => !is_word_byte(*b),
                        None => true,
                    };
                    let after_ok = match h.get(i + n.len()) {
                        Some(b) => !is_word_byte(*b),
                        None => true,
                    };
                    if before_ok && after_ok {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Classify a token's narrative family from deterministic evidence
/// (§21.4/§29.6).
///
/// Priority cascade, first match wins — a total, order-independent function of
/// the evidence:
///
/// 1. **Derivative** — measured metadata similarity at/above
///    `derivative_threshold_bps`. A clone is a statement about *this* launch and
///    outranks whatever theme it copied.
/// 2. **Stream** — the launch-metadata live-stream flag observed active. A live
///    stream is the token's actual driver regardless of its name.
/// 3. **Lexical** — the first family in `lexicon` with a needle hit in the name
///    or the symbol (name scanned before symbol; the lexicon order is the
///    specificity cascade).
/// 4. **Unclassified** — no evidence (§6.4). Never a guess.
///
/// Pure integer/byte logic, allocation-free, panic-free on any input.
#[must_use]
pub fn nv_family_classify(
    ev: &FamilyEvidence<'_>,
    lexicon: &[FamilyLexicon],
    derivative_threshold_bps: u32,
) -> FamilyClassification {
    if let Some(sim) = ev.derivative_similarity_bps {
        if sim >= derivative_threshold_bps {
            return FamilyClassification {
                family: NarrativeFamily::Derivative,
                lane: FamilyEvidenceLane::MetadataSimilarity,
                matched_needle: None,
                lexicon_version: FAMILY_LEXICON_VERSION,
            };
        }
    }

    if ev.live_stream_active == Some(true) {
        return FamilyClassification {
            family: NarrativeFamily::Stream,
            lane: FamilyEvidenceLane::LiveStream,
            matched_needle: None,
            lexicon_version: FAMILY_LEXICON_VERSION,
        };
    }

    for entry in lexicon {
        for needle in entry.needles {
            if matches_needle(ev.name, needle) || matches_needle(ev.symbol, needle) {
                return FamilyClassification {
                    family: entry.family,
                    lane: FamilyEvidenceLane::Lexical,
                    matched_needle: Some(needle.text),
                    lexicon_version: FAMILY_LEXICON_VERSION,
                };
            }
        }
    }

    FamilyClassification::unclassified()
}

/// [`nv_family_classify`] with the versioned defaults ([`FAMILY_LEXICON_V1`],
/// [`FAMILY_DERIVATIVE_SIMILARITY_BPS`]).
#[must_use]
pub fn nv_family_classify_default(ev: &FamilyEvidence<'_>) -> FamilyClassification {
    nv_family_classify(ev, FAMILY_LEXICON_V1, FAMILY_DERIVATIVE_SIMILARITY_BPS)
}
