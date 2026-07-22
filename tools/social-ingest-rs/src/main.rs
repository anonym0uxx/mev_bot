//! `pq-twitch-capture` — anonymous read-only Twitch chat → normalized NDJSON.
//!
//! The `[S]` capture edge for the Twitch lane: connects to Twitch IRC over PLAIN
//! TCP (`irc.chat.twitch.tv:6667`, no TLS, no OAuth — anonymous `justinfan` read
//! access to public chat), joins the requested channels, and emits ONE normalized
//! JSON object per chat line on stdout — the exact schema
//! `tools/social-ingest/normalize.py` defines and
//! `pump_quant_ingest::social_parse::parse_social_event` consumes.
//!
//! # Constitution discipline (binding)
//! * **§22 determinism boundary.** The wall clock ([`SystemTime`]) is read here
//!   and ONLY here, at the capture edge, to stamp `observed_at_ns`. The parse and
//!   emit modules are pure; `--replay` mode is fully deterministic (synthetic
//!   monotone timestamps, zero network) so tests and replays are byte-stable.
//! * **§29 provenance.** Platform `"twitch"`, author = chatter nick, community =
//!   channel: origin identity is carried verbatim so downstream trust is *earned*
//!   per source (D-ledger), never assumed. The anonymous reading identity is
//!   sacrificial (§29.7e) — no credential exists to burn.
//! * **§67 removable adapter.** Zero dependencies, one binary, speaks only the
//!   shared NDJSON contract on stdout; delete the binary and the system loses one
//!   lane, nothing else. Diagnostics go to stderr exclusively — stdout is the
//!   NDJSON stream and nothing but.
//!
//! Usage:
//! ```text
//! pq-twitch-capture chan1 chan2            # live capture ('#' optional)
//! pq-twitch-capture --channels-file f.txt  # one channel per line
//! pq-twitch-capture --replay lines.irc     # deterministic, no network
//! ```

mod emit;
mod parse;

use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Twitch IRC ingress — plain TCP, the reason this lane can be dependency-free.
const IRC_ADDR: &str = "irc.chat.twitch.tv:6667";
/// Fixed anonymous nick. Any `justinfan<digits>` gets read-only access with no
/// credentials; the identity is sacrificial by design (§29.7e).
const ANON_NICK: &str = "justinfan73646";
/// Pause between JOINs — respects Twitch's join rate limits (20 joins / 10 s for
/// unverified connections; 600 ms keeps us comfortably under).
const JOIN_PACE: Duration = Duration::from_millis(600);
/// Reconnect backoff bounds (seconds): exponential 1 → 60, capped.
const BACKOFF_MIN_SECS: u64 = 1;
const BACKOFF_MAX_SECS: u64 = 60;
/// Replay-mode synthetic clock: base + step per emitted event. Arbitrary but
/// FIXED so replay output is byte-identical run-to-run (§22).
const REPLAY_BASE_NS: u64 = 1_000_000_000;
const REPLAY_STEP_NS: u64 = 1_000_000;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cli = match Cli::parse(&args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("[pq-twitch-capture] {msg}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Some(path) = cli.replay {
        return match run_replay(&path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("[pq-twitch-capture] replay failed: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if cli.channels.is_empty() {
        eprintln!("[pq-twitch-capture] no channels given");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    run_live(&cli.channels) // never returns; reconnects forever
}

const USAGE: &str = "usage: pq-twitch-capture [--channels-file <path>] [chan ...]\n\
       pq-twitch-capture --replay <raw-irc-lines-file>\n\
  Channels may be given with or without a leading '#'. NDJSON on stdout,\n\
  diagnostics on stderr.";

/// Parsed command line. `--replay` wins over any channel arguments (zero network).
struct Cli {
    replay: Option<String>,
    channels: Vec<String>,
}

impl Cli {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut replay = None;
        let mut channels = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--replay" => {
                    replay = Some(it.next().ok_or("--replay needs a file path")?.clone());
                }
                "--channels-file" => {
                    let path = it.next().ok_or("--channels-file needs a file path")?;
                    let body = std::fs::read_to_string(path)
                        .map_err(|e| format!("cannot read {path}: {e}"))?;
                    for line in body.lines() {
                        push_channel(&mut channels, line);
                    }
                }
                "-h" | "--help" => return Err("help requested".to_string()),
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown flag {flag}"));
                }
                chan => push_channel(&mut channels, chan),
            }
        }
        Ok(Self { replay, channels })
    }
}

/// Normalize one channel argument: trim, drop an optional leading `#`, ASCII-
/// lowercase (Twitch channels are lowercase logins). Empty results are skipped.
fn push_channel(channels: &mut Vec<String>, raw: &str) {
    let c = raw.trim();
    let c = c.strip_prefix('#').unwrap_or(c).to_ascii_lowercase();
    if !c.is_empty() && !channels.contains(&c) {
        channels.push(c);
    }
}

/// Capture-boundary clock read — the ONE place wall time enters the pipeline
/// (§22). Matches `tools/social-ingest/probe`'s stamp exactly.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Emit one normalized NDJSON line and flush (real-time friendly: downstream
/// consumers see each chat line the instant it is captured).
fn emit_chat(out: &mut impl Write, chat: &parse::ChatLine, observed_at_ns: u64) -> io::Result<()> {
    let line = emit::event_line(&chat.community, &chat.author, &chat.text, observed_at_ns);
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// `--replay`: read raw IRC protocol lines from a file, emit the same NDJSON with
/// a fixed monotone synthetic clock. Deterministic, zero network (§22) — this is
/// what the integration tests and any offline replay drive.
fn run_replay(path: &str) -> io::Result<()> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut buf = Vec::new();
    let mut emitted: u64 = 0;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        // Lossy decode: a malformed byte sequence must skip a line, never abort.
        let line = String::from_utf8_lossy(&buf);
        if let parse::IrcEvent::Chat(chat) = parse::parse_line(&line) {
            let ts = REPLAY_BASE_NS + emitted * REPLAY_STEP_NS;
            emit_chat(&mut out, &chat, ts)?;
            emitted += 1;
        }
    }
    eprintln!("[pq-twitch-capture] replay: emitted {emitted} events");
    Ok(())
}

/// Live capture: connect, join, stream forever. Every socket error or EOF is
/// answered with bounded exponential backoff and a full re-JOIN — the lane heals
/// itself; it never panics and never exits on transport trouble.
fn run_live(channels: &[String]) -> ExitCode {
    let mut backoff = BACKOFF_MIN_SECS;
    loop {
        match connect_and_stream(channels) {
            Ok(lines_read) => {
                eprintln!(
                    "[pq-twitch-capture] connection closed after {lines_read} lines; reconnecting"
                );
                if lines_read > 0 {
                    backoff = BACKOFF_MIN_SECS; // the link worked; restart fresh
                }
            }
            Err(e) => eprintln!("[pq-twitch-capture] connection error: {e}"),
        }
        eprintln!("[pq-twitch-capture] reconnect in {backoff}s");
        thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
    }
}

/// One connection lifetime: anonymous NICK, paced JOINs, then read lines until
/// EOF/error. Returns the number of raw lines read (backoff-reset signal).
fn connect_and_stream(channels: &[String]) -> io::Result<u64> {
    eprintln!("[pq-twitch-capture] connecting to {IRC_ADDR} as {ANON_NICK}");
    let stream = TcpStream::connect(IRC_ADDR)?;
    let _ = stream.set_nodelay(true);
    let mut writer = stream.try_clone()?;

    // Anonymous registration: NICK only, no PASS/OAuth — read-only public chat.
    writer.write_all(format!("NICK {ANON_NICK}\r\n").as_bytes())?;
    for chan in channels {
        writer.write_all(format!("JOIN #{chan}\r\n").as_bytes())?;
        eprintln!("[pq-twitch-capture] JOIN #{chan}");
        thread::sleep(JOIN_PACE); // Twitch join rate limit (see JOIN_PACE)
    }

    let mut reader = BufReader::new(stream);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut buf = Vec::new();
    let mut lines_read: u64 = 0;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            return Ok(lines_read); // EOF: server closed; caller reconnects
        }
        lines_read += 1;
        let line = String::from_utf8_lossy(&buf);
        match parse::parse_line(&line) {
            parse::IrcEvent::Ping(token) => {
                writer.write_all(format!("PONG :{token}\r\n").as_bytes())?;
                writer.flush()?;
            }
            parse::IrcEvent::Chat(chat) => emit_chat(&mut out, &chat, now_ns())?,
            parse::IrcEvent::Other => {} // every other verb: silently ignored
        }
    }
}
