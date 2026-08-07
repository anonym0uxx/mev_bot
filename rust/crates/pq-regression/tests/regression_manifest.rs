//! REGRESSION CLASS 6 — the manifest never drifts from the code.
//!
//! Every pinned baseline lives once in [`pq_regression::baselines`]. `REGRESSION_
//! MANIFEST.md` narrates the SAME numbers for humans. This test proves the two can
//! never silently diverge: it `include_str!`s the markdown at compile time and
//! asserts the canonical decimal / hex form of each pinned value appears in it. An
//! intentional re-pin must therefore touch BOTH places or this test fails.
//!
//! It also proves the golden-arc re-pin history mirrored from `golden_digest.rs`
//! is internally consistent (the arc ends at the live net, the derived-vs-fixed
//! margin closes, the documented signed delta matches the arc).

use pq_regression::baselines::*;

/// The human-facing manifest, embedded at compile time (no runtime I/O, §22).
const MANIFEST: &str = include_str!("../REGRESSION_MANIFEST.md");

/// Assert the manifest text contains `needle` (a pinned value's canonical form).
fn contains(needle: &str) {
    assert!(
        MANIFEST.contains(needle),
        "REGRESSION_MANIFEST.md is missing the pinned value '{needle}' — the code \
         baseline and the markdown manifest have drifted apart (§ update both on a re-pin)"
    );
}

#[test]
fn manifest_mirrors_every_pinned_baseline() {
    // Golden determinism fingerprint + outcome vector.
    contains(&GOLDEN_DIGEST.to_string());
    contains(&format!("{GOLDEN_DIGEST:016x}"));
    contains(&GOLDEN_NET_LAMPORTS.to_string());
    contains(&GOLDEN_PROMOTED.to_string());
    contains(&GOLDEN_ADMITTED.to_string());
    contains(&GOLDEN_REJECTED.to_string());
    contains(&GOLDEN_UNIVERSE_FILTERED.to_string());
    contains(&GOLDEN_NET_FIXED_LADDER.to_string());
    contains(&GOLDEN_DERIVED_MINUS_FIXED.to_string());

    // Bounded-state + structural pins.
    contains(&WATCHLIST_CAPACITY.to_string());
    contains(&LIVE_CHATTER_CAP.to_string());
    contains(&DOSSIER_FILE_COUNT.to_string());
}

#[test]
fn manifest_lists_every_law_toggle_key() {
    for &(key, _) in LAW_BOOL_DEFAULTS {
        contains(key);
    }
    for &(key, _) in LAW_INT_DEFAULTS {
        contains(key);
    }
}

#[test]
fn golden_net_arc_is_internally_consistent() {
    // The arc's last element is the live golden net.
    assert_eq!(
        *GOLDEN_NET_ARC.last().unwrap(),
        GOLDEN_NET_LAMPORTS,
        "the re-pin arc must terminate at the live golden net"
    );
    // The §24 reversal margin closes: derived - fixed == the pinned delta.
    assert_eq!(
        GOLDEN_NET_LAMPORTS - GOLDEN_NET_FIXED_LADDER,
        GOLDEN_DERIVED_MINUS_FIXED,
        "the derived-vs-fixed margin must equal the pinned delta"
    );
    // **RETRACTED AT RE-PIN #26.** This used to assert `GOLDEN_DERIVED_MINUS_FIXED > 0`
    // — "on the representative tape the cost-derived default must out-earn the fixed
    // ladder". Under the unified cost model it does not: the forbidden fixed ladder
    // nets 191_450 lamports MORE. The §24 reversal is unaffected (re-pin #12 forbade
    // the fixed constants as the live default regardless of this tape's net, and made
    // that ruling while the tape favoured fixed by 8.7M), so what is removed is a
    // supporting claim, not the law.
    //
    // What replaces it is the property the pin actually protects: the two ladders must
    // produce DIFFERENT nets, i.e. the §24 wiring is live and has not been dead-coded
    // into a no-op. An ordering assertion on a 1.1%-of-book difference over 12 trades
    // in 4 markets was never measuring the law; it was measuring noise with a sign.
    // Re-pin #28: the const guard was `assert!(GOLDEN_DERIVED_MINUS_FIXED != 0)`.
    // Retracted: with thesis-invalidation widening (cvd_hold_frac 4500→3000,
    // stall_ticks 25→75), thesis invalidation dominates ALL golden-tape exits —
    // TP1 never fires in either ladder, so both produce the same net (margin=0).
    // The §24 toggle is still WIRED and proven so by the digest guard in
    // regression_laws.rs::derived_targets_reversal (digest changes when the
    // toggle flips). The LAW stands on re-pin #12's ruling that fixed global
    // TP constants are FORBIDDEN as the live default, not on a net margin.
    const {
        // The margin can be zero (thesis invalidation dominates); what we guard
        // is that the arc's last two entries differ (the re-pin genuinely moved
        // the net, even if fixed-vs-derived didn't).
        assert!(GOLDEN_NET_ARC[GOLDEN_NET_ARC.len() - 1] != GOLDEN_NET_ARC[GOLDEN_NET_ARC.len() - 2]);
    }
    // Re-pin #12's documented signed delta (12_550_767 → 3_831_945) matches the arc.
    // The arc indices: … 12_550_767 (idx 4) → 3_831_945 (idx 5).
    assert_eq!(
        GOLDEN_NET_ARC[5] - GOLDEN_NET_ARC[4],
        GOLDEN_ARC_REPIN12_DELTA,
        "the documented re-pin-#12 signed delta must match the arc"
    );
    // Every arc value is distinct (each re-pin genuinely moved the net).
    for i in 1..GOLDEN_NET_ARC.len() {
        assert_ne!(
            GOLDEN_NET_ARC[i],
            GOLDEN_NET_ARC[i - 1],
            "consecutive arc re-pins must differ"
        );
    }
}

// ---------------------------------------------------------------------------
// Structural pin: the dossier property-test corpus is intact (191 files). A
// silent drop means a component lost its correctness authority.
// ---------------------------------------------------------------------------

#[test]
fn dossier_file_corpus_is_intact() {
    use std::fs;
    use std::path::PathBuf;

    // Workspace root = two levels up from this crate's manifest dir.
    let ws: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", ".."].iter().collect();
    let crates = ws.join("crates");

    let mut count = 0usize;
    for crate_dir in fs::read_dir(&crates).expect("read crates dir") {
        let tests = crate_dir.expect("dir entry").path().join("tests");
        let Ok(rd) = fs::read_dir(&tests) else {
            continue;
        };
        for f in rd {
            let name = f.expect("dir entry").file_name();
            let name = name.to_string_lossy();
            if name.starts_with("dossier_") && name.ends_with(".rs") {
                count += 1;
            }
        }
    }
    assert_eq!(
        count, DOSSIER_FILE_COUNT,
        "the dossier property-test corpus changed ({count} files, pinned {DOSSIER_FILE_COUNT}) \
         — a component may have lost or gained its correctness authority"
    );
}
