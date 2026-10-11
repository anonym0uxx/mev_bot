//! SINGLE BOOKING PATH (source half). The engine's books (`bankroll_realized` / `bankroll_committed`) may be read or
//! written ONLY in `src/engine/books.rs`, which under `paper_fill_v2_shadow` returns the SettlementLedger's figures
//! and turns every booking outside a declared settlement scope into the named fault `books_bypass:<site>`.
//! Any other file that names those fields (outside comments, the struct declaration and the constructor) is a
//! booking that could bypass the ledger, and this test fails on it.
//!
//! The runtime half (a v2 BUY -> ADD/REDUCE -> EXIT run with zero bypasses, and a probe that an unscoped booking
//! latches `books_bypass:*`) lives in `tests/paper_fill_v2_wiring.rs`.

use std::path::{Path, PathBuf};

const FIELDS: [&str; 2] = ["bankroll_realized", "bankroll_committed"];

/// The only non-comment lines outside books.rs allowed to name the fields: declaration + constructor in engine.rs.
const ALLOWED_ENGINE_RS: [&str; 4] = [
    "bankroll_realized: i128,",
    "bankroll_committed: u128,",
    "bankroll_realized: 0,",
    "bankroll_committed: 0,",
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn names_field(line: &str, f: &str) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find(f) {
        let s = from + i;
        let e = s + f.len();
        let before = line[..s].chars().next_back();
        let after = line[e..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !ident(before) && !ident(after) {
            return true;
        }
        from = e;
    }
    false
}

#[test]
fn no_file_but_books_rs_touches_the_engine_book_fields() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(files.len() > 20, "scanned the crate sources");
    let mut hits = Vec::new();
    let mut allowed_seen = 0;
    for p in &files {
        let rel = p
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "engine/books.rs" {
            continue;
        }
        let text = std::fs::read_to_string(p).unwrap();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if !FIELDS.iter().any(|f| names_field(t, f)) {
                continue;
            }
            if rel == "engine.rs" && ALLOWED_ENGINE_RS.contains(&t) {
                allowed_seen += 1;
                continue;
            }
            hits.push(format!("src/{rel}:{}: {t}", n + 1));
        }
    }
    assert!(
        hits.is_empty(),
        "bookings outside engine/books.rs (route them through books_* inside a settlement scope):\n{}",
        hits.join("\n")
    );
    assert_eq!(
        allowed_seen, 4,
        "declaration + constructor found exactly once each"
    );
}

#[test]
fn books_rs_writes_the_v1_fields_only_behind_the_v2_guard() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine/books.rs");
    let text = std::fs::read_to_string(p).unwrap();
    // Every assignment to a v1 field sits in a function that first consults the v2 guard (or the ledger's absence).
    let mut fn_body = String::new();
    let mut bad = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("pub") && line.contains(" fn ")
            || line.trim_start().starts_with("fn ")
        {
            fn_body.clear();
        }
        fn_body.push_str(line);
        fn_body.push('\n');
        let t = line.trim();
        let assigns = FIELDS.iter().any(|f| {
            t.starts_with(&format!("self.{f} =")) && !t.starts_with(&format!("self.{f} =="))
        });
        if assigns
            && !fn_body.contains("books_v1_or_check")
            && !fn_body.contains("settle.is_none()")
            && !fn_body.contains("for_test")
        {
            bad.push(t.to_string());
        }
    }
    assert!(bad.is_empty(), "unguarded v1 writes in books.rs: {bad:?}");
}
