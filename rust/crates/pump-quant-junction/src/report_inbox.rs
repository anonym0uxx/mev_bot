//! Management-sell report inbox: the channel through which a REDUCE / EXIT / ADD fill report reaches the engine.
//! One JSON object per line, appended to a file the daemon polls. Fields: `mint` (64 hex), `order_id`, `action`
//! (`reduce`|`exit`|`add`), `intended` (the issued quantity, restated), `cumulative_tokens` (the TOTAL the order has
//! filled), `cumulative_gross` (TOTAL gross proceeds in lamports; for an ADD the TOTAL notional spent) and
//! `cumulative_fees` (TOTAL all-in fees in lamports; 0 for an ADD). There is NO per-fill price field: price is
//! derived by the engine from the increment of the totals, so it can never be ambiguous.
//!
//! AUTHORITY: HARNESS-ONLY. The producer is the offline paper-replay harness (a stand-in executor writing what a real
//! executor would report). The daemon polls this file only under `PQ_OFFLINE_PAPER_REPLAY=1` without `--live`
//! (`pq_daemon.rs`, `replay_harness`); in any other mode the file is never read. A production executor will need its
//! own authenticated channel; this one carries no authentication and must never be pointed at real funds.
//! Reports are CUMULATIVE, so replaying the whole file after a restart is safe: applying one is idempotent.
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

/// The harness-only gate: the inbox is polled ONLY with `PQ_OFFLINE_PAPER_REPLAY=1` and without `--live`. Every
/// other combination never opens the file. (`--live` with a model endpoint is refused earlier still, at startup.)
#[must_use]
pub fn inbox_enabled(live_mode: bool, offline_replay_env: Option<&str>) -> bool {
    !live_mode && offline_replay_env == Some("1")
}

/// Why one inbox line was not turned into an event (counted by the caller, never guessed at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRefusal {
    NotJson,
    BadMint,
    MissingField(&'static str),
    BadAction,
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Parse one inbox line into a management report event.
///
/// # Errors
/// [`LineRefusal`] naming what was wrong; nothing is applied.
pub fn parse_line(line: &str) -> Result<AppEvent, LineRefusal> {
    let v: serde_json::Value = serde_json::from_str(line).map_err(|_| LineRefusal::NotJson)?;
    let mint = v["mint"]
        .as_str()
        .and_then(unhex)
        .ok_or(LineRefusal::BadMint)?;
    let u = |k: &'static str| v[k].as_u64().ok_or(LineRefusal::MissingField(k));
    // The action is a NAME, never a number a typo could turn into another action.
    let action = match v["action"].as_str() {
        Some("reduce") => 0u8,
        Some("exit") => 1,
        Some("add") => 2,
        _ => return Err(LineRefusal::BadAction),
    };
    Ok(AppEvent::ModelMgmtReport {
        mint: Mint::from_bytes(mint),
        order_id: u("order_id")?,
        action,
        intended: u("intended")?,
        cumulative_tokens: u("cumulative_tokens")?,
        cumulative_gross: u("cumulative_gross")?,
        cumulative_fees: u("cumulative_fees")?,
    })
}

/// Polls the inbox file and hands back the lines not yet seen. Tracks a BYTE offset, so a poll reads only new
/// data; a file that shrank (rotated or truncated) is re-read from the start, which is safe because reports are
/// cumulative and idempotent.
#[derive(Debug, Default)]
pub struct InboxReader {
    offset: u64,
    /// Lines turned into events.
    pub accepted: u64,
    /// Lines refused, by reason.
    pub refused: std::collections::BTreeMap<&'static str, u64>,
    /// Identity of the file the offset belongs to: (inode, first bytes already consumed). A file whose identity
    /// changed (replaced, rotated, rewritten) is re-read from the START, so no unapplied report is skipped.
    ident: Option<(u64, Vec<u8>)>,
    /// How many times a changed file identity forced a re-read from the start.
    pub replaced: u64,
    /// Bytes of an incomplete trailing line currently held back (reported, never dropped).
    pub partial_bytes: u64,
}

const IDENT_PREFIX: usize = 256;

impl InboxReader {
    /// Lines refused so far, all reasons.
    #[must_use]
    pub fn refused_total(&self) -> u64 {
        self.refused.values().sum()
    }

    /// New lines since the last poll, parsed. A trailing partial line (no newline yet) is left for the next poll.
    pub fn poll(&mut self, path: &std::path::Path) -> Vec<AppEvent> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(path) else {
            return Vec::new();
        };
        let Ok(meta) = f.metadata() else {
            return Vec::new();
        };
        let len = meta.len();
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let ino = 0u64;
        // Is this still the file the offset belongs to? Same inode, not shorter, and the bytes already consumed
        // (first IDENT_PREFIX of them) unchanged. Anything else: start over (reports are idempotent, so re-reading
        // is safe; skipping would lose an unapplied report).
        let mut head =
            vec![0u8; usize::try_from(self.offset.min(IDENT_PREFIX as u64)).unwrap_or(0)];
        let same = match &self.ident {
            None => self.offset == 0,
            Some((i, h)) => {
                *i == ino && len >= self.offset && f.read_exact(&mut head).is_ok() && head == *h
            }
        };
        if !same {
            if self.offset > 0 {
                self.replaced += 1;
            }
            self.offset = 0;
            self.ident = None;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            self.partial_bytes = 0;
            return Vec::new();
        }
        let mut raw = Vec::new();
        if f.read_to_end(&mut raw).is_err() {
            return Vec::new();
        }
        let buf = String::from_utf8_lossy(&raw).into_owned();
        let complete = buf.rfind('\n').map_or(0, |i| i + 1);
        self.partial_bytes = (buf.len() - complete) as u64;
        self.offset += complete as u64;
        if self.offset > 0 {
            // Remember the identity prefix of what has been consumed (read the file's own first bytes).
            if let Ok(mut g) = std::fs::File::open(path) {
                let mut h =
                    vec![0u8; usize::try_from(self.offset.min(IDENT_PREFIX as u64)).unwrap_or(0)];
                if g.read_exact(&mut h).is_ok() {
                    self.ident = Some((ino, h));
                }
            }
        }
        let mut out = Vec::new();
        for line in buf[..complete].lines().filter(|l| !l.trim().is_empty()) {
            match parse_line(line) {
                Ok(ev) => {
                    self.accepted += 1;
                    out.push(ev);
                }
                Err(e) => {
                    let k = match e {
                        LineRefusal::NotJson => "not_json",
                        LineRefusal::BadMint => "bad_mint",
                        LineRefusal::MissingField(_) => "missing_field",
                        LineRefusal::BadAction => "bad_action",
                    };
                    *self.refused.entry(k).or_insert(0) += 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl(order: u64, cum: u64) -> String {
        format!(
            r#"{{"mint":"{}","order_id":{order},"action":"exit","intended":900,"cumulative_tokens":{cum},"cumulative_gross":{cum}000,"cumulative_fees":{cum}}}"#,
            "ab".repeat(32)
        )
    }

    #[test]
    fn a_replaced_longer_file_is_reread_from_the_start_not_skipped_from_the_old_offset() {
        let d = std::env::temp_dir().join(format!("inbox_repl_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("in.ndjson");
        std::fs::write(&p, format!("{}\n", rl(1, 100))).unwrap();
        let mut r = InboxReader::default();
        assert_eq!(r.poll(&p).len(), 1);
        // Replaced by DIFFERENT, LONGER content: the old byte offset points into the middle of the new first line.
        std::fs::write(
            &p,
            format!("{}\n{}\n{}\n", rl(2, 200), rl(2, 300), rl(2, 400)),
        )
        .unwrap();
        let evs = r.poll(&p);
        assert_eq!(
            evs.len(),
            3,
            "every line of the replacement is offered, none skipped"
        );
        assert_eq!(r.replaced, 1);
        // Appending after a replacement works as usual; an unchanged file offers nothing.
        assert_eq!(r.poll(&p).len(), 0);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "{}", rl(2, 500)).unwrap();
        assert_eq!(r.poll(&p).len(), 1);
    }

    #[test]
    fn a_partial_trailing_line_is_held_visibly_until_it_completes() {
        let d = std::env::temp_dir().join(format!("inbox_part_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("in.ndjson");
        let full = rl(1, 100);
        let (a, b) = full.split_at(full.len() / 2);
        std::fs::write(&p, format!("{}\n{a}", rl(1, 50))).unwrap();
        let mut r = InboxReader::default();
        assert_eq!(r.poll(&p).len(), 1, "only the complete line is offered");
        assert_eq!(
            r.partial_bytes,
            a.len() as u64,
            "the unapplied tail is reported, not silently dropped"
        );
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "{b}").unwrap();
        assert_eq!(
            r.poll(&p).len(),
            1,
            "completing the line offers it exactly once"
        );
        assert_eq!(r.partial_bytes, 0);
    }

    #[test]
    fn the_inbox_gate_opens_only_for_offline_replay_without_live() {
        assert!(inbox_enabled(false, Some("1")));
        for (live, env) in [
            (true, Some("1")),
            (false, None),
            (false, Some("0")),
            (false, Some("true")),
            (false, Some("")),
            (true, None),
        ] {
            assert!(!inbox_enabled(live, env), "{live} {env:?}");
        }
    }

    fn line(order: u64, cum: u64) -> String {
        format!(
            r#"{{"mint":"{}","order_id":{order},"action":"exit","intended":900,"cumulative_tokens":{cum},"cumulative_gross":{cum}000,"cumulative_fees":{cum}}}"#,
            "ab".repeat(32)
        )
    }

    #[test]
    fn a_report_line_parses_to_one_event_and_bad_lines_are_named() {
        assert!(matches!(
            parse_line(&line(3, 500)),
            Ok(AppEvent::ModelMgmtReport {
                order_id: 3,
                action: 1,
                intended: 900,
                cumulative_tokens: 500,
                cumulative_gross: 500_000,
                cumulative_fees: 500,
                ..
            })
        ));
        assert_eq!(parse_line("{ no"), Err(LineRefusal::NotJson));
        assert_eq!(
            parse_line(r#"{"mint":"zz","order_id":1}"#),
            Err(LineRefusal::BadMint)
        );
        let no_cum = format!(
            r#"{{"mint":"{}","order_id":1,"action":"exit","intended":9,"cumulative_gross":1,"cumulative_fees":0}}"#,
            "ab".repeat(32)
        );
        assert_eq!(
            parse_line(&no_cum),
            Err(LineRefusal::MissingField("cumulative_tokens"))
        );
    }

    #[test]
    fn polling_reads_only_new_complete_lines_and_tolerates_rotation() {
        let d = std::env::temp_dir().join(format!("pq_inbox_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("inbox.ndjson");
        let _ = std::fs::remove_file(&p);
        let mut r = InboxReader::default();
        assert!(r.poll(&p).is_empty(), "no file yet");
        std::fs::write(&p, format!("{}\n{}", line(1, 10), "{\"mint\":")).unwrap();
        assert_eq!(r.poll(&p).len(), 1, "the partial trailing line waits");
        assert!(r.poll(&p).is_empty(), "nothing new");
        // The offset sits after the first complete line. Append the rest of that file: the old partial text is
        // now followed by garbage that completes it into a bad line, then one more good report.
        let first = format!("{}\n", line(1, 10));
        let bad_then_good = format!("{}\n{}\n", "{\"mint\":broken", line(1, 20));
        std::fs::write(&p, format!("{first}{bad_then_good}")).unwrap();
        let evs = r.poll(&p);
        assert_eq!(evs.len(), 1, "only the good report after the old offset");
        assert_eq!(r.refused.get("not_json").copied(), Some(1));
        assert_eq!(r.accepted, 2);
        // Truncation: re-read from the start (cumulative reports make this safe).
        std::fs::write(&p, format!("{}\n", line(2, 5))).unwrap();
        assert_eq!(r.poll(&p).len(), 1);
    }
}
