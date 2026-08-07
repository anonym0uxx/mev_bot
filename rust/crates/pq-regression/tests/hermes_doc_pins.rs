//! REGRESSION CLASS 7 — **the Hermes-facing documents never drift from the code.**
//!
//! # Why this file exists
//!
//! `regression_manifest.rs` already proves `REGRESSION_MANIFEST.md` mirrors
//! [`pq_regression::baselines`]. That covers the manifest and nothing else. The documents an
//! autonomous builder is actually handed — the constitution, the Phase-B activation directive,
//! the README — quote the same numbers in prose, and until this file existed nothing checked them.
//!
//! **They drifted, and the drift landed on the worst possible line.** The activation directive's
//! §4 hands the builder a five-value decision vector for the single most consequential Phase-B
//! judgment: whether a moved golden digest is a legitimate seed-only re-pin (adding a `Config`
//! field re-seeds the §19 journal with zero decision change, so this WILL fire during Phase B) or
//! a real determinism break that must halt the build. The fifth entry, `AlphaCall net`, was quoted
//! as `−2,721,835` for two re-pins after the live constant became `+593,348`. A builder running
//! that checklist would find a mismatch on a healthy tree and conclude "a decision number moved →
//! determinism break". The good outcome is a halted healthy build. The bad outcome is a builder
//! who "fixes" the code to match the document.
//!
//! # The defect class, and why a test is the right shape for it
//!
//! This is the third time this repository has been bitten by the same thing: two files, each
//! locally coherent, wrong only in relation to each other. `one_authority_laws.rs` guards it for
//! quantities inside the decision path; `docs/SILO_AUDIT_2026-07-28.md` swept for more. This file
//! extends the guard across the code/prose boundary, which is where the constitution's own
//! Amendment A-13(5) obligation lives — *"when a fixture correction falsifies a claim already
//! written into a document, locate every place that claim was repeated and correct it in the SAME
//! commit."* That obligation was, until this file, enforced entirely by the diligence of whoever
//! did the re-pin. A-13(5) was violated by A-13 itself.
//!
//! It matters most for a builder that cannot cross-check. A model strong enough to notice that the
//! re-pin ledger in `golden_digest.rs` contradicts a checklist in a 65 KB directive does not need
//! this test. A weaker one does, and the whole point of turning the obligation into a red test is
//! that it no longer depends on which model is reading.
//!
//! # What it asserts, and deliberately does not
//!
//! **Presence, not absence.** Every live pin must appear, in some canonical form, in each document
//! that is supposed to carry it. It does NOT assert that retired values are absent, because these
//! documents legitimately quote their own history — the AlphaCall arc `+447,700 → −2,721,835 →
//! +593,348` is exactly the evidence that the number cannot settle the question, and an
//! absence-assertion would forbid telling that story. Presence alone is sufficient for the defect
//! this guards: the stale directive did not contain `593,348` anywhere at all.
//!
//! Number forms are checked as plain (`30889282`), comma-grouped (`30,889,282`) and
//! underscore-grouped (`30_889_282`), because prose and Rust disagree about digit separators and
//! neither convention should be legislated by a test.

use pq_regression::baselines::*;

// The documents, embedded at compile time — no runtime I/O (§22), and a moved or renamed file is
// a compile error rather than a skipped test.
const ACTIVATION: &str = include_str!("../../../../docs/HERMES_PHASE_B_ACTIVATION_ONESHOT.md");
const README: &str = include_str!("../../../../README.md");
const BASELINES_MD: &str = include_str!("../../../../REGRESSION_BASELINES.md");
const CONSTITUTION: &str = include_str!("../../../../docs/HERMES_ONE_SHOT_PROMPT.md");

/// The three canonical written forms of an integer: plain, comma-grouped, underscore-grouped.
fn forms(n: i128) -> [String; 3] {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut comma = String::new();
    let mut under = String::new();
    for (i, c) in digits.chars().enumerate() {
        let left = digits.len() - i;
        if i > 0 && left % 3 == 0 {
            comma.push(',');
            under.push('_');
        }
        comma.push(c);
        under.push(c);
    }
    let sign = if neg { "-" } else { "" };
    [
        format!("{sign}{digits}"),
        format!("{sign}{comma}"),
        format!("{sign}{under}"),
    ]
}

/// Assert `doc` quotes `value` in at least one canonical form.
fn quotes(doc: &str, doc_name: &str, label: &str, value: i128) {
    let f = forms(value);
    assert!(
        f.iter().any(|form| doc.contains(form.as_str())),
        "{doc_name} does not quote the live pin {label} = {value} in ANY form ({f:?}).\n\
         \n\
         The code and the document an autonomous builder reads have drifted apart. The code is \
         the authority: fix the DOCUMENT, never the constant. If the value genuinely moved, this \
         is a re-pin and Amendment A-13(5) requires correcting every place it was repeated in the \
         SAME commit — which is what this test exists to force."
    );
}

/// **The decision vector.** These five values are what `§4` of the activation directive tells a
/// Phase-B builder to check when the golden digest moves, to distinguish a seed-only re-pin from
/// a determinism break. Getting any of them wrong in the prose misdirects the halt decision, so
/// they are pinned here first and most loudly.
#[test]
fn the_activation_directive_quotes_the_live_decision_vector() {
    let d = ACTIVATION;
    let n = "docs/HERMES_PHASE_B_ACTIVATION_ONESHOT.md";
    quotes(d, n, "GOLDEN_NET_LAMPORTS", GOLDEN_NET_LAMPORTS);
    quotes(d, n, "GOLDEN_PROMOTED", i128::from(GOLDEN_PROMOTED));
    quotes(d, n, "GOLDEN_ADMITTED", i128::from(GOLDEN_ADMITTED));
    quotes(d, n, "GOLDEN_REJECTED", i128::from(GOLDEN_REJECTED));
    quotes(
        d,
        n,
        "GOLDEN_UNIVERSE_FILTERED",
        i128::from(GOLDEN_UNIVERSE_FILTERED),
    );
    // The entry that was stale for two re-pins, on the line that decides whether to halt.
    quotes(
        d,
        n,
        "GOLDEN_ALPHACALL_NET",
        i128::from(GOLDEN_ALPHACALL_NET),
    );
    // And the digest itself, which the same section instructs the builder to re-pin in two places.
    quotes(d, n, "GOLDEN_DIGEST", i128::from(GOLDEN_DIGEST));
}

/// The README is the first thing any reader — human or agent — opens, and it states the shipped
/// position. A README quoting a retired book is how a stale number acquires authority.
#[test]
fn the_readme_quotes_the_live_outcome_vector() {
    let n = "README.md";
    quotes(README, n, "GOLDEN_DIGEST", i128::from(GOLDEN_DIGEST));
    quotes(README, n, "GOLDEN_NET_LAMPORTS", GOLDEN_NET_LAMPORTS);
    quotes(README, n, "GOLDEN_PROMOTED", i128::from(GOLDEN_PROMOTED));
    quotes(README, n, "GOLDEN_ADMITTED", i128::from(GOLDEN_ADMITTED));
    quotes(README, n, "GOLDEN_REJECTED", i128::from(GOLDEN_REJECTED));
    quotes(
        README,
        n,
        "GOLDEN_UNIVERSE_FILTERED",
        i128::from(GOLDEN_UNIVERSE_FILTERED),
    );
}

/// `REGRESSION_BASELINES.md` is the human-facing mirror of the gate. It contradicted
/// `baselines.rs` on the promoted/admitted/rejected row (`504 / 12 / 447` against the shipped
/// `504 / 11 / 448`) for a full re-pin, because the re-pin updated the digest and net rows of the
/// same table and missed the counts row. A bringup step trusting the doc over the manifest would
/// have reported a false gate failure.
#[test]
fn the_baselines_narrative_quotes_the_live_outcome_vector() {
    let n = "REGRESSION_BASELINES.md";
    quotes(BASELINES_MD, n, "GOLDEN_DIGEST", i128::from(GOLDEN_DIGEST));
    quotes(BASELINES_MD, n, "GOLDEN_NET_LAMPORTS", GOLDEN_NET_LAMPORTS);
    quotes(
        BASELINES_MD,
        n,
        "GOLDEN_PROMOTED",
        i128::from(GOLDEN_PROMOTED),
    );
    quotes(
        BASELINES_MD,
        n,
        "GOLDEN_ADMITTED",
        i128::from(GOLDEN_ADMITTED),
    );
    quotes(
        BASELINES_MD,
        n,
        "GOLDEN_REJECTED",
        i128::from(GOLDEN_REJECTED),
    );
    quotes(
        BASELINES_MD,
        n,
        "GOLDEN_UNIVERSE_FILTERED",
        i128::from(GOLDEN_UNIVERSE_FILTERED),
    );
}

/// Amendment A-13 narrates the depth-realism correction using the golden net and the AlphaCall
/// lane as its worked example. Its clause (5) is the chase-the-falsification obligation; the
/// amendment carried a stale AlphaCall reading for two re-pins, i.e. A-13(5) was violated by A-13.
/// This pins the two figures it must carry currently.
#[test]
fn the_constitution_amendment_quotes_the_live_figures() {
    let n = "docs/HERMES_ONE_SHOT_PROMPT.md";
    quotes(CONSTITUTION, n, "GOLDEN_NET_LAMPORTS", GOLDEN_NET_LAMPORTS);
    quotes(
        CONSTITUTION,
        n,
        "GOLDEN_ALPHACALL_NET",
        i128::from(GOLDEN_ALPHACALL_NET),
    );
}

/// The mirror between this crate and `pump-quant-app/tests/golden_digest.rs` is by hand-copy.
/// That is how the AlphaCall drift survived: two constants, no compiler relationship. This asserts
/// the app-side test file literally contains the underscore form of the value pinned here, so a
/// one-sided edit fails rather than silently diverging.
#[test]
fn the_app_side_golden_test_agrees_with_this_crates_mirror() {
    const APP_GOLDEN: &str = include_str!("../../pump-quant-app/tests/golden_digest.rs");
    for (label, v) in [
        ("GOLDEN_ALPHACALL_NET", i128::from(GOLDEN_ALPHACALL_NET)),
        ("GOLDEN_NET_LAMPORTS", GOLDEN_NET_LAMPORTS),
        ("GOLDEN_DIGEST", i128::from(GOLDEN_DIGEST)),
    ] {
        let f = forms(v);
        assert!(
            f.iter().any(|form| APP_GOLDEN.contains(form.as_str())),
            "pump-quant-app/tests/golden_digest.rs does not contain {label} = {v}. The two \
             pinned mirrors have diverged; they are copied by hand and nothing else relates them."
        );
    }
}

/// Sanity on the helper itself — a formatter bug here would make every assertion above vacuous.
#[test]
fn canonical_forms_are_what_they_claim() {
    assert_eq!(
        forms(30_889_282),
        [
            "30889282".to_string(),
            "30,889,282".to_string(),
            "30_889_282".to_string()
        ]
    );
    assert_eq!(
        forms(593_348),
        [
            "593348".to_string(),
            "593,348".to_string(),
            "593_348".to_string()
        ]
    );
    assert_eq!(
        forms(-2_721_835),
        [
            "-2721835".to_string(),
            "-2,721,835".to_string(),
            "-2_721_835".to_string()
        ]
    );
    assert_eq!(
        forms(72),
        ["72".to_string(), "72".to_string(), "72".to_string()]
    );
}
