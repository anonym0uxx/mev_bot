//! `pq-narrative-resolve` — emit the narrative sidecar for a FORWARD corpus.
//!
//! The forward decision-SFT builder (`tools/data-pipeline/src/build_decision_sft_identity.py
//! --narrative`) needs `mint -> family/stage/verdict` for the historical decisions it
//! renders. Those verdicts must come from the SAME resolver the daemon runs, or the
//! corpus and the live path would drift apart — the exact failure this whole change
//! set exists to prevent. So this is a thin CLI over [`NarrativeLexicon`], not a
//! reimplementation.
//!
//! Input  (JSONL, MUST be in ascending `t_ms` order — the alias stage is a ring
//!         buffer over the mint stream, so order is part of the input, not a detail):
//!   {"t_ms":1700000000000,"mint":"<base58 or hex>","name":"mensa","symbol":"MENSA"}
//! Output (JSONL, one line per row, same order):
//!   {"mint":"...","family":"animal","stage":"novel","verdict":"Eligible","lexicon_version":3}
//!
//! Usage:
//!   pq-narrative-resolve --lexicon <dynamic_lexicon_v1.json> < stream.jsonl > sidecar.jsonl
//!   pq-narrative-resolve --lexicon <path> --in stream.jsonl --out sidecar.jsonl
//!
//! Determinism: the resolver is integer-only and takes `t_ms` from the ROW, never from
//! the clock, so the same input always produces the same sidecar. Causality is the
//! crate's own guard (`first_seen_ms <= as_of_ms <= t_ms`), so a row cannot see a
//! lexicon entry that did not exist at its own decision time.

use std::io::{BufRead, BufWriter, Write};

use pump_quant_junction::narrative_lexicon::NarrativeLexicon;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut lexicon_path: Option<String> = None;
    let mut in_path: Option<String> = None;
    let mut out_path: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--lexicon" => lexicon_path = args.next(),
            "--in" => in_path = args.next(),
            "--out" => out_path = args.next(),
            "-h" | "--help" => {
                eprintln!(
                    "pq-narrative-resolve --lexicon <json> [--in <jsonl>] [--out <jsonl>]\n\
                     Reads a time-ordered mint stream and writes the narrative sidecar."
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let lexicon_path = lexicon_path.unwrap_or_else(|| {
        eprintln!("--lexicon is required");
        std::process::exit(2);
    });

    let Some(mut lex) = NarrativeLexicon::load(&lexicon_path) else {
        // FAIL CLOSED BUT HONESTLY: no lexicon means no verdicts. Emitting a
        // sidecar of `Unresolved` for every row would look like a measurement.
        eprintln!("narrative lexicon unavailable at {lexicon_path}: refusing to emit a sidecar");
        std::process::exit(3);
    };

    eprintln!(
        "pq-narrative-resolve: lexicon v{} with {} entries",
        lex.version(),
        lex.len()
    );

    let input: Box<dyn BufRead> = match &in_path {
        Some(p) => match std::fs::File::open(p) {
            Ok(f) => Box::new(std::io::BufReader::new(f)),
            Err(e) => {
                eprintln!("cannot open {p}: {e}");
                std::process::exit(2);
            }
        },
        None => Box::new(std::io::BufReader::new(std::io::stdin())),
    };

    let mut out: Box<dyn Write> = match &out_path {
        Some(p) => match std::fs::File::create(p) {
            Ok(f) => Box::new(BufWriter::new(f)),
            Err(e) => {
                eprintln!("cannot create {p}: {e}");
                std::process::exit(2);
            }
        },
        None => Box::new(BufWriter::new(std::io::stdout())),
    };

    let mut n = 0u64;
    let mut last_t = 0u64;
    let mut out_of_order = 0u64;

    for line in input.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(v) = pump_quant_ingest::json::parse(line.as_bytes()) else {
            eprintln!("skipping unparseable row: {line}");
            continue;
        };
        let geti = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_number_str())
                .and_then(|s| s.parse::<u64>().ok())
        };
        let gets = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);

        let (Some(t_ms), Some(mint), Some(name)) = (geti("t_ms"), gets("mint"), gets("name"))
        else {
            eprintln!("skipping row missing t_ms/mint/name: {line}");
            continue;
        };
        let symbol = gets("symbol").unwrap_or_default();

        // The ring buffer is over the STREAM, so order is an input. Say so rather
        // than silently producing stages that depend on how the file was sorted.
        if t_ms < last_t {
            out_of_order += 1;
        }
        last_t = last_t.max(t_ms);

        let (verdict, stage, family, lexicon_version) = lex.resolve(&name, &symbol, t_ms);
        let _ = writeln!(
            out,
            "{{\"mint\":\"{}\",\"t_ms\":{},\"family\":\"{}\",\"stage\":\"{}\",\
              \"verdict\":\"{}\",\"lexicon_version\":{}}}",
            mint.escape_default(),
            t_ms,
            label_family(family),
            label_stage(stage),
            label_verdict(verdict),
            lexicon_version
        );
        n += 1;
    }

    let _ = out.flush();
    eprintln!("pq-narrative-resolve: wrote {n} rows");
    if out_of_order > 0 {
        eprintln!(
            "WARNING: {out_of_order} rows were out of ascending t_ms order; the alias stage \
             for those rows was computed against a stream that had already seen later mints"
        );
        std::process::exit(4);
    }
}

fn label_verdict(code: u8) -> &'static str {
    match code {
        1 => "Eligible",
        2 => "Saturated",
        3 => "NoAttach",
        4 => "Throwaway",
        5 => "Unresolved",
        _ => "Unobserved",
    }
}

fn label_stage(code: u8) -> &'static str {
    match code {
        1 => "novel",
        2 => "rising",
        3 => "cresting",
        4 => "saturated",
        _ => "unobserved",
    }
}

fn label_family(code: u8) -> &'static str {
    match code {
        1 => "animal",
        2 => "political",
        3 => "celebrity",
        4 => "tech",
        5 => "derivative",
        6 => "stream",
        7 => "seasonal",
        _ => "unclassified",
    }
}
