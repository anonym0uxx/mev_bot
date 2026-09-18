//! End-to-end tests for the tape sink: real files under `std::env::temp_dir()`, real
//! appends, real bytes read back off disk. Nothing here is mocked, because the properties
//! being checked — append-not-truncate, a byte cap that cannot be crossed, byte-exact
//! escaping, and the exact corpus line shape — only exist on the filesystem.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pump_quant_tape::{read_back, Side, TapeRecord, TapeWriter};

/// A unique path in the temp dir, so parallel test threads never collide.
fn temp_path(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("pq_tape_{tag}_{}_{nanos}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn rec(mint: &str, regime: &str) -> TapeRecord {
    TapeRecord {
        mint: mint.to_string(),
        trader: "GYVnp1k6eavJ1a4mXdmxiwXZpVBYr2mWSsGF4iWSNqmS".to_string(),
        side: Side::Buy,
        venue: "pumpfun".to_string(),
        slot: 445669922,
        recv_unix_ms: 1788975568357,
        signature: "BERxAsFqw1PMp9GKKzYHGFR17SdxoZiYJGyc6SUS8Zh8aVp2v5NtMMfa31c5fyRgMHVK7QTADsChgdrPYkLAVbo".to_string(),
        sol_lamports: -98778145,
        tokens_raw: 1774559103622,
        fee_lamports: 1005000,
        cu_consumed: 111723,
        status: "success".to_string(),
        resolution: "instruction_accounts".to_string(),
        regime: regime.to_string(),
    }
}

#[test]
fn writes_reads_back_and_appends_rather_than_truncating() {
    let path = temp_path("append");

    // Session one: two records, flushed to stable storage.
    let mut w = TapeWriter::open(&path, 1 << 20).expect("open");
    assert_eq!(w.path(), path.as_path());
    let a = rec("Aaaa", "quiet");
    let b = rec("Bbbb", "trending_up");
    assert!(w.append(&a).expect("append a"));
    assert!(w.append(&b).expect("append b"));
    assert_eq!(w.records_written(), 2);
    let after_two = w.bytes_written();
    assert_eq!(after_two, (a.to_jsonl_line().len() + b.to_jsonl_line().len() + 2) as u64);
    assert_eq!(w.flushed_through(), 0, "nothing claimed durable before flush");
    w.flush().expect("flush");
    assert_eq!(w.flushed_through(), after_two);
    let size_on_disk = std::fs::metadata(&path).expect("stat").len();
    assert_eq!(size_on_disk, after_two, "one newline per record, nothing else");
    drop(w);

    assert_eq!(read_back(&path).expect("read back"), vec![a.clone(), b.clone()]);

    // Session two: opening the same tape APPENDS. The first two records must survive.
    let mut w2 = TapeWriter::open(&path, 1 << 20).expect("reopen");
    assert_eq!(w2.bytes_written(), after_two, "existing length counts against the cap");
    assert_eq!(w2.records_written(), 0, "a fresh writer counts only its own records");
    let c = rec("Cccc", "capitulation");
    assert!(w2.append(&c).expect("append c"));
    assert_eq!(w2.records_written(), 1);
    assert_eq!(w2.bytes_written(), after_two + c.to_jsonl_line().len() as u64 + 1);
    w2.flush().expect("flush 2");
    drop(w2);

    let all = read_back(&path).expect("read back all");
    assert_eq!(all, vec![a.clone(), b.clone(), c]);
    assert_eq!(all.len(), 3, "the reopened writer truncated nothing");

    // Every line round-trips through the strict parser.
    for r in &all {
        assert_eq!(&TapeRecord::from_jsonl_line(&r.to_jsonl_line()).expect("round trip"), r);
    }

    // The on-disk bytes are exactly the rendered lines, newline-terminated.
    let raw = std::fs::read_to_string(&path).expect("read raw");
    let expected = format!(
        "{}\n{}\n{}\n",
        a.to_jsonl_line(),
        b.to_jsonl_line(),
        all[2].to_jsonl_line()
    );
    assert_eq!(raw, expected);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn byte_cap_refuses_the_record_that_would_cross_it_and_still_takes_a_smaller_one() {
    let path = temp_path("cap");
    let small = rec("M", "quiet");
    let big = rec(&"X".repeat(600), "quiet");

    let small_line = small.to_jsonl_line().len() as u64 + 1;
    let big_line = big.to_jsonl_line().len() as u64 + 1;
    assert!(big_line >= 2 * small_line, "the test needs a clearly larger record");

    // Room for exactly one small record, not for the big one, not for a second small one.
    let cap = 2 * small_line - 1;
    let mut w = TapeWriter::open(&path, cap).expect("open");

    assert_eq!(w.append(&big).expect("append big"), false, "big record must be refused");
    assert_eq!(w.records_written(), 0);
    assert_eq!(w.bytes_written(), 0);
    assert_eq!(std::fs::metadata(&path).expect("stat").len(), 0, "a refusal writes nothing");

    assert!(w.append(&small).expect("append small"), "small record fits");
    assert_eq!(w.bytes_written(), small_line);

    let before = std::fs::read(&path).expect("bytes before");
    assert_eq!(w.append(&small).expect("append small again"), false);
    assert_eq!(w.append(&big).expect("append big again"), false);
    assert_eq!(w.records_written(), 1);
    let after = std::fs::read(&path).expect("bytes after");
    assert_eq!(before, after, "refused appends change no byte on disk");
    assert_eq!(after, format!("{}\n", small.to_jsonl_line()).as_bytes());

    w.flush().expect("flush");
    assert_eq!(w.flushed_through(), small_line);
    drop(w);
    assert_eq!(read_back(&path).expect("read back"), vec![small.clone()]);

    // A cap of exactly the line length still accepts that line: bounded, not over-strict.
    let path2 = temp_path("cap-exact");
    let mut w2 = TapeWriter::open(&path2, small_line).expect("open exact");
    assert!(w2.append(&small).expect("append exact"));
    assert_eq!(w2.append(&small).expect("append over"), false);
    let _ = std::fs::remove_file(&path2);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn quotes_backslashes_newlines_and_non_ascii_round_trip_byte_exactly() {
    let path = temp_path("escape");
    let mut nasty = rec("mint\"with\\quote", "regime\ttab");
    nasty.trader = "trader\nline\r\nCR".to_string();
    nasty.venue = "\u{1}\u{1f}ctrl".to_string();
    nasty.signature = "sig/Ω𐍈✅日本語".to_string();
    nasty.status = "sta\u{8}tus\u{c}".to_string();
    nasty.resolution = "  spaced  ".to_string();

    let line = nasty.to_jsonl_line();
    assert!(
        !line.contains('\n') && !line.contains('\r') && !line.contains('\t'),
        "no raw control character may survive into a line"
    );
    assert!(line.contains("\\\""), "quotes are escaped");
    assert!(line.contains("\\\\"), "backslashes are escaped");
    assert!(line.contains("\\n"), "newlines are escaped");
    assert!(line.contains("\\u0001"), "low control bytes are \\u-escaped");
    assert!(line.contains("\\b") && line.contains("\\f"), "short forms used where JSON has them");
    assert!(line.contains("Ω𐍈✅日本語"), "non-ASCII passes through unescaped");

    // The line is a single physical line and round-trips byte for byte.
    let mut w = TapeWriter::open(&path, 1 << 20).expect("open");
    assert!(w.append(&nasty).expect("append"));
    w.flush().expect("flush");
    drop(w);

    let raw = std::fs::read(&path).expect("raw");
    assert_eq!(raw, format!("{line}\n").as_bytes());

    let parsed = TapeRecord::from_jsonl_line(&line).expect("parse");
    assert_eq!(parsed, nasty);
    assert_eq!(parsed.mint, "mint\"with\\quote");
    assert_eq!(parsed.venue, "\u{1}\u{1f}ctrl");
    assert_eq!(parsed.signature, "sig/Ω𐍈✅日本語");
    assert_eq!(parsed.trader, "trader\nline\r\nCR");
    assert_eq!(parsed.to_jsonl_line(), line, "re-render is byte-identical");
    assert_eq!(read_back(&path).expect("read back"), vec![nasty]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn shape_matches_the_corpus_line_with_the_regime_appended() {
    // The first line of /training/v2/canonical/renormalized_v7/trades.jsonl, byte for byte,
    // plus the trailing regime key this crate adds.
    let expected = "{\"mint\": \"4FqqLmDcRTeFBJkGUemYwhsUuSPCdUPS6TWqDaLCpump\", \
\"trader\": \"GYVnp1k6eavJ1a4mXdmxiwXZpVBYr2mWSsGF4iWSNqmS\", \"side\": \"sell\", \
\"venue\": \"pumpswap\", \"slot\": 445669922, \"recv_unix_ms\": 1788975568348, \
\"signature\": \"3y6qnRFdyTimEtFeMYMkn2GNQT2mBHgWTHQot4JNGpjDQxn3Nrizg3yTW91L3h3wfNLTbRD8WL8QU7NHMnYrV71X\", \
\"sol_lamports\": 47914152, \"tokens_raw\": -69608018393, \"fee_lamports\": 81811, \
\"cu_consumed\": 105584, \"status\": \"success\", \"resolution\": \"instruction_accounts\", \
\"regime\": \"trending_up\"}";

    let r = TapeRecord {
        mint: "4FqqLmDcRTeFBJkGUemYwhsUuSPCdUPS6TWqDaLCpump".to_string(),
        trader: "GYVnp1k6eavJ1a4mXdmxiwXZpVBYr2mWSsGF4iWSNqmS".to_string(),
        side: Side::Sell,
        venue: "pumpswap".to_string(),
        slot: 445669922,
        recv_unix_ms: 1788975568348,
        signature: "3y6qnRFdyTimEtFeMYMkn2GNQT2mBHgWTHQot4JNGpjDQxn3Nrizg3yTW91L3h3wfNLTbRD8WL8QU7NHMnYrV71X".to_string(),
        sol_lamports: 47914152,
        tokens_raw: -69608018393,
        fee_lamports: 81811,
        cu_consumed: 105584,
        status: "success".to_string(),
        resolution: "instruction_accounts".to_string(),
        regime: "trending_up".to_string(),
    };
    assert_eq!(r.to_jsonl_line(), expected);

    // The same line with the regime key removed is exactly what the corpus holds, so the
    // regime tag is a pure append to the trained shape: it disturbs no other byte.
    let corpus_line = expected.replace(", \"regime\": \"trending_up\"}", "}");
    let head = &corpus_line[..corpus_line.len() - 1]; // the corpus line, minus its closing brace
    let rendered = r.to_jsonl_line();
    assert!(rendered.starts_with(head));
    assert_eq!(rendered, format!("{head}, \"regime\": \"trending_up\"}}"));
    assert_eq!(
        corpus_line.len() + ", \"regime\": \"trending_up\"".len(),
        rendered.len()
    );
}

#[test]
fn side_renders_and_parses_both_ways() {
    assert_eq!(Side::Buy.as_str(), "buy");
    assert_eq!(Side::Sell.as_str(), "sell");
    assert_eq!(Side::Buy.to_string(), "buy");
    assert_eq!(Side::parse("buy").expect("buy"), Side::Buy);
    assert_eq!(Side::parse("sell").expect("sell"), Side::Sell);
    assert!(Side::parse("BUY").is_err());
}
