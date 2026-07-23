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
    // The §24 reversal margin closes: derived - fixed == the pinned +12_620.
    assert_eq!(
        GOLDEN_NET_LAMPORTS - GOLDEN_NET_FIXED_LADDER,
        GOLDEN_DERIVED_MINUS_FIXED,
        "the derived-vs-fixed margin must equal the pinned re-pin-#13 delta"
    );
    // On the representative tape the cost-derived default must out-earn the fixed
    // ladder (a compile-time invariant on the pinned constant).
    const {
        assert!(GOLDEN_DERIVED_MINUS_FIXED > 0);
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
