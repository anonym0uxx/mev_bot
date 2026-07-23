//! `discord-gateway` subcommand — a PASSIVE, READ-ONLY Discord Gateway v10
//! client (JSON encoding) over the suite's hand-rolled rustls WebSocket.
//!
//! It monitors paid alpha servers the operator legitimately subscribes to and
//! captures the live message push — nothing else. The anti-flag posture is to
//! behave EXACTLY like a well-behaved normal client, never like a scraper:
//!
//! * **Strictly passive.** The ONLY Gateway ops we ever send are `IDENTIFY`
//!   (op 2), `RESUME` (op 6) and `HEARTBEAT` (op 1). We never send a message,
//!   typing indicator, reaction, or presence update beyond the single
//!   `invisible` IDENTIFY. We make ZERO REST calls — no message-history fetch
//!   (history scraping is exactly what trips detection); we consume only the
//!   live Gateway push.
//! * **Invisible.** IDENTIFY presence is `{"status":"invisible",...}` so the
//!   account shows offline to the room while still receiving messages — a
//!   first-class Discord feature, the supported "incognito" posture.
//! * **No WS-level keepalive.** We deliberately do NOT drive
//!   [`WsConn::maybe_keepalive`]: a real Discord client keeps the socket warm
//!   with Gateway op-1 heartbeats, not RFC 6455 pings; emitting both would be
//!   an atypical fingerprint. (We still auto-reply to server pings — that is
//!   normal and handled inside [`WsConn::poll_event`].)
//!
//! Out of scope BY DESIGN (never built here): account rotation, proxy cycling,
//! fake activity, or any evasion tooling. Those are what get a legit account
//! flagged; a single quiet passive client is what keeps it safe.
//!
//! Emission per captured message (§6.3 raw-bytes-first — raw first, derived
//! second): the untouched `d` payload as
//! `{"lane":"discord","recv_unix_ms":...,"raw":<d>}`, then a normalized
//! `discord_alpha` projection ([`normalize_message`]) the corroboration engine
//! parses as `platform:"discord"`. Both after an allowlist + dedupe gate.
//!
//! §22 determinism boundary: every parser/normalizer/timing helper here is a
//! pure function, fixture-tested without sockets; `recv_unix_ms` is injected.
//! §18.8 loud degradation: a missing selected token is fail-closed exit
//! [`EXIT_ARMING`]; drift, staleness, zombie connections and reconnects are
//! loud stderr sentinels, never silence.

use std::collections::HashSet;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::dedupe::DedupeRing;
use crate::json::{self, Value};
use crate::ws::{WsConn, WsEvent};
use crate::{backoff, emit};

/// Default Gateway endpoint (v10, JSON encoding). Override with
/// `DISCORD_GATEWAY_URL` for testing against a local echo server only.
pub const DEFAULT_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Gateway API version we speak.
pub const GATEWAY_VERSION: u64 = 10;

/// Discord's snowflake epoch (2015-01-01T00:00:00Z, ms). `unix_ms =
/// (snowflake >> 22) + DISCORD_EPOCH_MS` (Discord ID reference). Pure integer.
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

// Gateway intents (bitwise; Discord Gateway intents reference). The read set
// this passive lane needs — nothing that would require write scopes.
/// `GUILDS` intent, bit `1 << 0` — guild create/lifecycle (channel identity).
pub const INTENT_GUILDS: u64 = 1;
/// `GUILD_MESSAGES` intent, bit `1 << 9` — MESSAGE_CREATE/UPDATE in guilds.
pub const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
/// `MESSAGE_CONTENT` intent, bit `1 << 15` — the `content` field. Privileged
/// for bots; present-by-default for user tokens.
pub const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;

/// Gateway opcode: DISPATCH (server → client event, carries `t`/`s`/`d`).
pub const OP_DISPATCH: u64 = 0;
/// Gateway opcode: HEARTBEAT (both directions; op-1 from server = "beat now").
pub const OP_HEARTBEAT: u64 = 1;
/// Gateway opcode: IDENTIFY (client → server, start a session).
pub const OP_IDENTIFY: u64 = 2;
/// Gateway opcode: RESUME (client → server, replay a dropped session).
pub const OP_RESUME: u64 = 6;
/// Gateway opcode: RECONNECT (server → client, reconnect and resume).
pub const OP_RECONNECT: u64 = 7;
/// Gateway opcode: INVALID_SESSION (`d` = resumable bool).
pub const OP_INVALID_SESSION: u64 = 9;
/// Gateway opcode: HELLO (first frame; carries `heartbeat_interval`).
pub const OP_HELLO: u64 = 10;
/// Gateway opcode: HEARTBEAT_ACK (server ack of our heartbeat).
pub const OP_HEARTBEAT_ACK: u64 = 11;

/// Staleness watchdog (seconds): no Gateway frame at all for this long forces
/// a reconnect. Alpha rooms plus periodic HEARTBEAT_ACKs mean 120 s of total
/// silence is a dead pipe, not a quiet room.
pub const DISCORD_STALE_SECS: u64 = 120;

/// Deadline to receive HELLO after connect (seconds); Discord sends it
/// immediately, so this only guards a broken/hostile endpoint.
pub const DISCORD_HELLO_TIMEOUT_SECS: u64 = 20;

/// Dedupe ring capacity (message snowflakes remembered) — a resumed session
/// replays missed events and can redeliver.
pub const DISCORD_DEDUPE_CAP: usize = 16_384;

/// Fail-closed arming exit code (§18.8), same convention as the other lanes.
pub const EXIT_ARMING: u8 = 3;

/// Heartbeat-ACK grace numerator: the zombie deadline is `interval * 3/2`
/// (interval*1.5) — Discord's own client uses this as the un-ACKed cutoff.
pub const HEARTBEAT_GRACE_NUM: u64 = 3;
/// Heartbeat-ACK grace denominator (see [`HEARTBEAT_GRACE_NUM`]).
pub const HEARTBEAT_GRACE_DEN: u64 = 2;

/// Deterministic first-heartbeat jitter resolution (parts per this many). The
/// Discord docs jitter the FIRST heartbeat by `interval * random[0,1)`; this
/// crate is RNG-free, so with a `--heartbeat-jitter-seed N` the first delay is
/// the exact fraction `(N % JITTER_RESOLUTION) / JITTER_RESOLUTION` of the
/// interval. Without a seed the first heartbeat uses the full interval.
pub const JITTER_RESOLUTION: u64 = 1000;

/// Default IDENTIFY `properties.os` — a plausible normal-client identity, not
/// a spoof of any specific victim. Configurable via `--client-os`.
pub const DEFAULT_CLIENT_OS: &str = "Windows";
/// Default IDENTIFY `properties.browser`. Configurable via `--client-browser`.
pub const DEFAULT_CLIENT_BROWSER: &str = "Discord Client";
/// Default IDENTIFY `properties.device`. Configurable via `--client-device`.
pub const DEFAULT_CLIENT_DEVICE: &str = "desktop";
/// IDENTIFY presence status — `invisible` is the incognito posture (offline to
/// the room, still receiving). Fixed: visibility is the whole point.
pub const DEFAULT_PRESENCE_STATUS: &str = "invisible";

/// Minimum cashtag body length after `$` (e.g. `$OK`). Mirrors the social
/// lanes' extractor (§99 bounded).
pub const CASHTAG_MIN: usize = 2;
/// Maximum cashtag body length after `$`; a longer alnum run is not a ticker.
pub const CASHTAG_MAX: usize = 10;
/// Max distinct cashtags kept from one message (§99 bounded).
pub const MAX_CASHTAGS: usize = 8;
/// Minimum base58 length of a Solana address (32-byte key).
pub const MINT_B58_MIN: usize = 32;
/// Maximum base58 length of a Solana address.
pub const MINT_B58_MAX: usize = 44;
/// Max distinct mints kept from one message (§99 bounded).
pub const MAX_MINTS: usize = 4;

/// Combined read intents: `GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT`. Pure.
#[must_use]
pub const fn intents() -> u64 {
    INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT
}

// ---------------------------------------------------------- pure builders

/// A plausible normal-client desktop fingerprint for IDENTIFY `properties`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProperties {
    /// `properties.os`.
    pub os: String,
    /// `properties.browser`.
    pub browser: String,
    /// `properties.device`.
    pub device: String,
}

impl Default for ClientProperties {
    fn default() -> Self {
        Self {
            os: DEFAULT_CLIENT_OS.to_string(),
            browser: DEFAULT_CLIENT_BROWSER.to_string(),
            device: DEFAULT_CLIENT_DEVICE.to_string(),
        }
    }
}

/// Build the IDENTIFY (op 2) payload: `{token, intents, properties, presence}`.
/// The token is placed raw in `d.token` (for BOTH user and bot tokens — the
/// `Bot ` prefix is a REST Authorization-header convention we never use). The
/// presence is `invisible`. Pure. The token is never logged by this crate.
#[must_use]
pub fn identify_payload(
    token: &str,
    intents: u64,
    props: &ClientProperties,
    presence_status: &str,
) -> String {
    let mut out = String::with_capacity(256 + token.len());
    out.push_str("{\"op\":2,\"d\":{\"token\":\"");
    emit::escape_json_into(token, &mut out);
    out.push_str("\",\"intents\":");
    out.push_str(&intents.to_string());
    out.push_str(",\"properties\":{\"os\":\"");
    emit::escape_json_into(&props.os, &mut out);
    out.push_str("\",\"browser\":\"");
    emit::escape_json_into(&props.browser, &mut out);
    out.push_str("\",\"device\":\"");
    emit::escape_json_into(&props.device, &mut out);
    out.push_str("\"},\"presence\":{\"status\":\"");
    emit::escape_json_into(presence_status, &mut out);
    out.push_str("\",\"afk\":false,\"activities\":[]}}}");
    out
}

/// Build the RESUME (op 6) payload: `{token, session_id, seq}`. Pure.
#[must_use]
pub fn resume_payload(token: &str, session_id: &str, seq: u64) -> String {
    let mut out = String::with_capacity(96 + token.len() + session_id.len());
    out.push_str("{\"op\":6,\"d\":{\"token\":\"");
    emit::escape_json_into(token, &mut out);
    out.push_str("\",\"session_id\":\"");
    emit::escape_json_into(session_id, &mut out);
    out.push_str("\",\"seq\":");
    out.push_str(&seq.to_string());
    out.push_str("}}");
    out
}

/// Build the HEARTBEAT (op 1) payload: `{"op":1,"d":<last seq | null>}`. Pure.
#[must_use]
pub fn heartbeat_payload(seq: Option<u64>) -> String {
    match seq {
        Some(s) => format!("{{\"op\":1,\"d\":{s}}}"),
        None => "{\"op\":1,\"d\":null}".to_string(),
    }
}

// ------------------------------------------------------ pure classification

/// One classified inbound Gateway frame.
#[derive(Debug, PartialEq)]
pub enum Inbound<'a> {
    /// op 10 HELLO — carries the heartbeat interval (ms).
    Hello {
        /// `d.heartbeat_interval` in milliseconds.
        heartbeat_interval_ms: u64,
    },
    /// op 11 HEARTBEAT_ACK — clears the zombie-connection flag.
    HeartbeatAck,
    /// op 1 HEARTBEAT — the server asks us to send a heartbeat immediately.
    HeartbeatRequest,
    /// op 0 DISPATCH — a named event (`t`) with sequence (`s`) and data (`d`).
    Dispatch {
        /// Event name, e.g. `READY`, `MESSAGE_CREATE`.
        t: &'a str,
        /// Sequence number for heartbeats/resume (absent on some frames).
        seq: Option<u64>,
        /// Event payload.
        d: Option<&'a Value>,
    },
    /// op 7 RECONNECT — server wants us to reconnect and RESUME.
    Reconnect,
    /// op 9 INVALID_SESSION — `d` is the resumable flag.
    InvalidSession {
        /// True → RESUME is allowed; false → drop session and re-IDENTIFY.
        resumable: bool,
    },
    /// A known-shaped frame with an opcode we do not act on.
    Other {
        /// The unhandled opcode.
        op: u64,
    },
    /// Malformed / missing opcode — logged loudly, never silently dropped.
    Drift,
}

/// Classify one parsed inbound frame by its Gateway opcode. Pure; total over
/// adversarial input (a missing/garbage `op` is [`Inbound::Drift`], never a
/// panic).
#[must_use]
pub fn classify(v: &Value) -> Inbound<'_> {
    let Some(op) = v.get("op").and_then(Value::as_u64) else {
        return Inbound::Drift;
    };
    match op {
        OP_DISPATCH => {
            let Some(t) = v.get("t").and_then(Value::as_str) else {
                return Inbound::Drift;
            };
            Inbound::Dispatch {
                t,
                seq: v.get("s").and_then(Value::as_u64),
                d: v.get("d"),
            }
        }
        OP_HEARTBEAT => Inbound::HeartbeatRequest,
        OP_RECONNECT => Inbound::Reconnect,
        OP_INVALID_SESSION => Inbound::InvalidSession {
            // `d` is a bare boolean; anything but `true` is non-resumable.
            resumable: matches!(v.get("d"), Some(Value::Bool(true))),
        },
        OP_HELLO => match v
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(Value::as_u64)
        {
            Some(ms) => Inbound::Hello {
                heartbeat_interval_ms: ms,
            },
            None => Inbound::Drift,
        },
        OP_HEARTBEAT_ACK => Inbound::HeartbeatAck,
        other => Inbound::Other { op: other },
    }
}

/// Extract `(session_id, resume_gateway_url)` from a READY dispatch `d`. Pure.
#[must_use]
pub fn ready_of(d: &Value) -> Option<(String, String)> {
    let session_id = d.get("session_id").and_then(Value::as_str)?;
    let resume_gateway_url = d.get("resume_gateway_url").and_then(Value::as_str)?;
    Some((session_id.to_string(), resume_gateway_url.to_string()))
}

// ------------------------------------------------------- reconnect decision

/// What to do on the next connection after a session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconnect {
    /// Reconnect to the resume URL and send RESUME (op 6).
    Resume,
    /// Drop the session and send a fresh IDENTIFY (op 2).
    Reidentify,
}

/// op 9 INVALID_SESSION decision: resumable → RESUME, else re-IDENTIFY
/// (Discord mandates a 1-5 s wait before the fresh IDENTIFY; the deterministic
/// backoff ladder's ≥1 s step satisfies it — waiting longer is never
/// penalized, the band only de-syncs mass reconnects). Pure.
#[must_use]
pub fn invalid_session_action(resumable: bool) -> Reconnect {
    if resumable {
        Reconnect::Resume
    } else {
        Reconnect::Reidentify
    }
}

/// Reconnect intent for a transport-level end (close, error, staleness,
/// zombie): RESUME if we hold a session, else re-IDENTIFY. Pure.
#[must_use]
fn reconnect_reason(has_session: bool) -> Reconnect {
    if has_session {
        Reconnect::Resume
    } else {
        Reconnect::Reidentify
    }
}

// ----------------------------------------------------------- url + timing

/// Compose a connect URL from a resume-gateway base: append the v10/json query
/// unless the base already carries a `?v=`. Pure.
#[must_use]
pub fn resume_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.contains("?v=") {
        return trimmed.to_string();
    }
    format!("{trimmed}/?v={GATEWAY_VERSION}&encoding=json")
}

/// Decode a Discord snowflake id string to Unix milliseconds
/// (`(id >> 22) + DISCORD_EPOCH_MS`). A non-snowflake id yields 0 (fail-open-
/// as-absence, never a panic). Pure integer.
#[must_use]
pub fn snowflake_to_unix_ms(id: &str) -> u64 {
    match id.parse::<u64>() {
        Ok(n) => (n >> 22).saturating_add(DISCORD_EPOCH_MS),
        Err(_) => 0,
    }
}

/// Deterministic first-heartbeat delay (ms). `None` → full interval (the
/// natural cadence boundary; safe for a single client — the doc jitter only
/// de-syncs fleets). `Some(seed)` → the exact fraction
/// `(seed % JITTER_RESOLUTION)/JITTER_RESOLUTION` of the interval. Pure.
#[must_use]
pub fn first_heartbeat_delay_ms(interval_ms: u64, jitter_seed: Option<u64>) -> u64 {
    match jitter_seed {
        None => interval_ms,
        Some(seed) => interval_ms.saturating_mul(seed % JITTER_RESOLUTION) / JITTER_RESOLUTION,
    }
}

/// Zombie-connection deadline (ms): `interval * 1.5`, integer-only. A heartbeat
/// un-ACKed for this long means the socket is dead → reconnect. Pure.
#[must_use]
pub const fn zombie_deadline_ms(interval_ms: u64) -> u64 {
    interval_ms.saturating_mul(HEARTBEAT_GRACE_NUM) / HEARTBEAT_GRACE_DEN
}

// -------------------------------------------------------------- allowlist

/// Guild + channel allowlist — only the operator's paid alpha rooms. An EMPTY
/// dimension imposes no constraint on that dimension (configure at least one).
#[derive(Debug, Default, Clone)]
pub struct Allowlist {
    /// Allowed guild (server) ids; empty = any guild.
    pub guilds: HashSet<String>,
    /// Allowed channel ids; empty = any channel.
    pub channels: HashSet<String>,
}

impl Allowlist {
    /// Does a message in `(guild_id, channel_id)` pass the allowlist? Pure.
    #[must_use]
    pub fn allowed(&self, guild_id: &str, channel_id: &str) -> bool {
        (self.guilds.is_empty() || self.guilds.contains(guild_id))
            && (self.channels.is_empty() || self.channels.contains(channel_id))
    }
}

/// Is `author_id` a designated high-signal alpha caller? Pure.
#[must_use]
pub fn is_designated_caller(callers: &HashSet<String>, author_id: &str) -> bool {
    !author_id.is_empty() && callers.contains(author_id)
}

/// Parse an allowlist file into `allow`/`callers` (merged, so it composes with
/// CLI flags). Lines are `guild:<id>`, `channel:<id>` or `caller:<id>`; `#`
/// comments and blank lines are skipped. Pure over its text; `Err` on a
/// malformed line (fail-closed config).
pub fn parse_allowlist_file(
    text: &str,
    allow: &mut Allowlist,
    callers: &mut HashSet<String>,
) -> Result<(), String> {
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((kind, id)) = line.split_once(':') else {
            return Err(format!(
                "allowlist line {}: expected `kind:id`, got {raw:?}",
                n + 1
            ));
        };
        let id = id.trim();
        if id.is_empty() {
            return Err(format!("allowlist line {}: empty id", n + 1));
        }
        match kind.trim() {
            "guild" => {
                allow.guilds.insert(id.to_string());
            }
            "channel" => {
                allow.channels.insert(id.to_string());
            }
            "caller" => {
                callers.insert(id.to_string());
            }
            other => {
                return Err(format!(
                    "allowlist line {}: unknown kind {other:?} (want guild|channel|caller)",
                    n + 1
                ));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------- extraction

/// A byte in the base58 (Bitcoin) alphabet — `0`, `O`, `I`, `l` excluded.
#[inline]
fn is_base58_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() && b != b'0' && b != b'O' && b != b'I' && b != b'l'
}

/// Extract distinct uppercased `$TICKER` cashtags from free text. A cashtag is
/// `$` immediately followed by [`CASHTAG_MIN`]..=[`CASHTAG_MAX`] ASCII
/// alphanumerics (a longer run is not a ticker). Deterministic, bounded at
/// [`MAX_CASHTAGS`] (§99). Mirrors the other social lanes. Pure.
#[must_use]
pub fn extract_cashtags(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let mut body = String::new();
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_alphanumeric() && body.len() < CASHTAG_MAX {
            body.push(bytes[j].to_ascii_uppercase() as char);
            j += 1;
        }
        let overflowed = j < bytes.len() && bytes[j].is_ascii_alphanumeric();
        if body.len() >= CASHTAG_MIN
            && !overflowed
            && out.len() < MAX_CASHTAGS
            && !out.iter().any(|c| c == &body)
        {
            out.push(body);
        }
        i = j.max(i + 1);
    }
    out
}

/// Extract distinct base58 pubkey-shaped tokens (length
/// [`MINT_B58_MIN`]..=[`MINT_B58_MAX`]) from free text — candidate Solana
/// mints/addresses. Deterministic, bounded at [`MAX_MINTS`] (§99). The exact
/// 32-byte decode/validation is downstream's job (§6.6 corroboration tier);
/// here we carve the address-shaped runs the other social lanes carve. Pure.
#[must_use]
pub fn extract_mints(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if !is_base58_char(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_base58_char(bytes[i]) {
            i += 1;
        }
        let run_len = i - start;
        if (MINT_B58_MIN..=MINT_B58_MAX).contains(&run_len) {
            // The run is ASCII base58 by construction, so this never fails.
            if let Ok(tok) = std::str::from_utf8(&bytes[start..i]) {
                if out.len() < MAX_MINTS && !out.iter().any(|m| m == tok) {
                    out.push(tok.to_string());
                }
            }
        }
    }
    out
}

// ------------------------------------------------------ pure normalization

/// Outcome of feeding one message dispatch to [`process_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgOutcome {
    /// Raw + normalized lines written.
    Emitted,
    /// Dropped before emit: guild/channel not on the allowlist.
    Dropped,
    /// Dropped before emit: message id already seen (resume redelivery).
    Deduped,
}

fn push_quoted_csv(out: &mut String, items: &[String]) {
    for (n, item) in items.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(item, out);
        out.push('"');
    }
}

/// Build the normalized `discord_alpha` line for a MESSAGE_CREATE/UPDATE `d`.
/// Absent fields degrade to `""`/`0`/`false` (fail-open-as-absence), never
/// error. `ts` is the snowflake-derived Unix ms of the message id. Pure; the
/// downstream engine parses `platform:"discord"` → `SocialPlatform::Discord`.
#[must_use]
pub fn normalize_message(d: &Value, callers: &HashSet<String>) -> String {
    let id = d.get("id").and_then(Value::as_str).unwrap_or("");
    let guild_id = d.get("guild_id").and_then(Value::as_str).unwrap_or("");
    let channel_id = d.get("channel_id").and_then(Value::as_str).unwrap_or("");
    let author = d.get("author");
    let author_id = author
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let username = author
        .and_then(|a| a.get("username"))
        .and_then(Value::as_str)
        .or_else(|| {
            author
                .and_then(|a| a.get("global_name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let content = d.get("content").and_then(Value::as_str).unwrap_or("");
    // community = the specific alpha room (channel), else the guild.
    let community = if channel_id.is_empty() {
        guild_id
    } else {
        channel_id
    };
    let is_caller = is_designated_caller(callers, author_id);
    let ts = snowflake_to_unix_ms(id);
    let cashtags = extract_cashtags(content);
    let mints = extract_mints(content);

    let mut out = String::with_capacity(256 + content.len());
    out.push_str("{\"lane\":\"discord_alpha\",\"platform\":\"discord\",\"guild_id\":\"");
    emit::escape_json_into(guild_id, &mut out);
    out.push_str("\",\"channel_id\":\"");
    emit::escape_json_into(channel_id, &mut out);
    out.push_str("\",\"author_id\":\"");
    emit::escape_json_into(author_id, &mut out);
    out.push_str("\",\"author\":\"");
    emit::escape_json_into(username, &mut out);
    out.push_str("\",\"community\":\"");
    emit::escape_json_into(community, &mut out);
    out.push_str("\",\"content\":\"");
    emit::escape_json_into(content, &mut out);
    out.push_str("\",\"is_designated_caller\":");
    out.push_str(if is_caller { "true" } else { "false" });
    out.push_str(",\"ts\":");
    out.push_str(&ts.to_string());
    out.push_str(",\"cashtags\":[");
    push_quoted_csv(&mut out, &cashtags);
    out.push_str("],\"mints\":[");
    push_quoted_csv(&mut out, &mints);
    out.push_str("]}");
    out
}

/// Allowlist-gate, dedupe, then emit raw + normalized lines for one message
/// dispatch `d`. Order is allowlist → dedupe → emit (a dropped message never
/// consumes a ring slot). Pure over its arguments (§22: the clock is injected).
pub fn process_message(
    d: &Value,
    allow: &Allowlist,
    callers: &HashSet<String>,
    ring: &mut DedupeRing,
    recv_unix_ms: u64,
    out: &mut impl Write,
) -> Result<MsgOutcome, String> {
    let guild_id = d.get("guild_id").and_then(Value::as_str).unwrap_or("");
    let channel_id = d.get("channel_id").and_then(Value::as_str).unwrap_or("");
    if !allow.allowed(guild_id, channel_id) {
        return Ok(MsgOutcome::Dropped);
    }
    let id = d.get("id").and_then(Value::as_str).unwrap_or("");
    if !ring.insert(id) {
        return Ok(MsgOutcome::Deduped);
    }
    let raw = emit::raw_line("discord", recv_unix_ms, None, &json::serialize(d));
    emit::write_line(out, &raw).map_err(|e| format!("stdout write: {e}"))?;
    let norm = normalize_message(d, callers);
    emit::write_line(out, &norm).map_err(|e| format!("stdout write: {e}"))?;
    Ok(MsgOutcome::Emitted)
}

// ----------------------------------------------------------------- runner

/// Which token the operator authenticates with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A user (self-bot) token — `DISCORD_USER_TOKEN`.
    User,
    /// A bot token — `DISCORD_BOT_TOKEN`.
    Bot,
}

impl TokenKind {
    /// Lowercase tag for logs/usage.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TokenKind::User => "user",
            TokenKind::Bot => "bot",
        }
    }

    /// The env var carrying this token kind.
    #[must_use]
    pub const fn env_var(self) -> &'static str {
        match self {
            TokenKind::User => "DISCORD_USER_TOKEN",
            TokenKind::Bot => "DISCORD_BOT_TOKEN",
        }
    }
}

const USAGE: &str = "usage: pq-stream-capture discord-gateway \
[--token-kind user|bot] [--guilds id,id] [--channels id,id] [--callers id,id]\n\
  [--allowlist-file f] [--client-os s] [--client-browser s] [--client-device s]\n\
  [--heartbeat-jitter-seed N]\n\
  env: DISCORD_USER_TOKEN or DISCORD_BOT_TOKEN (per --token-kind; exit 3 if the\n\
       selected one is missing), DISCORD_GATEWAY_URL (optional override, testing)\n\
  PASSIVE read-only: only IDENTIFY/RESUME/HEARTBEAT are ever sent; invisible\n\
  presence; ZERO REST calls; no rotation/proxy/fake-activity. Allowlist file\n\
  lines are `guild:id` / `channel:id` / `caller:id`.";

struct ParsedArgs {
    token_kind: TokenKind,
    allow: Allowlist,
    callers: HashSet<String>,
    props: ClientProperties,
    jitter_seed: Option<u64>,
}

fn extend_csv(set: &mut HashSet<String>, csv: &str) {
    for item in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        set.insert(item.to_string());
    }
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut token_kind = TokenKind::User;
    let mut allow = Allowlist::default();
    let mut callers: HashSet<String> = HashSet::new();
    let mut props = ClientProperties::default();
    let mut jitter_seed: Option<u64> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--token-kind" => {
                let v = it.next().ok_or("--token-kind needs a value")?;
                token_kind = match v.as_str() {
                    "user" => TokenKind::User,
                    "bot" => TokenKind::Bot,
                    other => return Err(format!("bad --token-kind {other:?} (want user|bot)")),
                };
            }
            "--guilds" => extend_csv(
                &mut allow.guilds,
                it.next().ok_or("--guilds needs a value")?,
            ),
            "--channels" => extend_csv(
                &mut allow.channels,
                it.next().ok_or("--channels needs a value")?,
            ),
            "--callers" => extend_csv(&mut callers, it.next().ok_or("--callers needs a value")?),
            "--allowlist-file" => {
                let path = it.next().ok_or("--allowlist-file needs a value")?;
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read allowlist file {path}: {e}"))?;
                parse_allowlist_file(&text, &mut allow, &mut callers)?;
            }
            "--client-os" => props.os = it.next().ok_or("--client-os needs a value")?.clone(),
            "--client-browser" => {
                props.browser = it.next().ok_or("--client-browser needs a value")?.clone();
            }
            "--client-device" => {
                props.device = it.next().ok_or("--client-device needs a value")?.clone();
            }
            "--heartbeat-jitter-seed" => {
                let v = it.next().ok_or("--heartbeat-jitter-seed needs a value")?;
                jitter_seed = Some(
                    v.parse::<u64>()
                        .map_err(|e| format!("bad --heartbeat-jitter-seed {v:?}: {e}"))?,
                );
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    Ok(ParsedArgs {
        token_kind,
        allow,
        callers,
        props,
        jitter_seed,
    })
}

struct Config {
    gateway_url: String,
    identify_json: String,
    token: String,
    allow: Allowlist,
    callers: HashSet<String>,
    jitter_seed: Option<u64>,
}

struct Session {
    session_id: String,
    resume_gateway_url: String,
    last_seq: Option<u64>,
}

struct Live<'a, W: Write> {
    cfg: &'a Config,
    out: &'a mut W,
    now_ms: fn() -> u64,
    ring: DedupeRing,
    session: Option<Session>,
    attempt: u32,
}

impl<W: Write> Live<'_, W> {
    /// The forever loop: connect, run one session, reconnect with backoff.
    fn serve(&mut self) -> ! {
        let mut want_resume = false;
        loop {
            let do_resume = want_resume && self.session.is_some();
            let url = match (do_resume, self.session.as_ref()) {
                (true, Some(s)) => resume_url(&s.resume_gateway_url),
                _ => self.cfg.gateway_url.clone(),
            };
            match WsConn::connect(&url) {
                Ok(mut conn) => {
                    eprintln!(
                        "[pq-stream-capture] discord-gateway connected ({url}) — sending {}",
                        if do_resume { "RESUME" } else { "IDENTIFY" }
                    );
                    let reason = self.one_session(&mut conn, do_resume);
                    want_resume = match reason {
                        Reconnect::Resume => self.session.is_some(),
                        Reconnect::Reidentify => {
                            self.session = None;
                            false
                        }
                    };
                }
                Err(e) => {
                    eprintln!(
                        "[pq-stream-capture] discord-gateway connect failed ({e}); retry in {}s",
                        backoff::step_secs(self.attempt)
                    );
                }
            }
            let delay = backoff::step_secs(self.attempt);
            self.attempt = self.attempt.saturating_add(1);
            std::thread::sleep(Duration::from_secs(delay));
        }
    }

    /// HELLO → IDENTIFY/RESUME → event loop, returning the reconnect intent.
    fn one_session(&mut self, conn: &mut WsConn, do_resume: bool) -> Reconnect {
        let interval_ms = match self.await_hello(conn) {
            Ok(ms) => ms,
            Err(reason) => return reason,
        };
        eprintln!("[pq-stream-capture] discord-gateway HELLO heartbeat_interval={interval_ms}ms");
        let write = if do_resume {
            match self.session.as_ref() {
                Some(s) => conn.send_text(&resume_payload(
                    &self.cfg.token,
                    &s.session_id,
                    s.last_seq.unwrap_or(0),
                )),
                None => conn.send_text(&self.cfg.identify_json),
            }
        } else {
            conn.send_text(&self.cfg.identify_json)
        };
        if let Err(e) = write {
            eprintln!(
                "[pq-stream-capture] discord-gateway {} write failed: {e}",
                if do_resume { "RESUME" } else { "IDENTIFY" }
            );
            return reconnect_reason(self.session.is_some());
        }
        self.event_loop(conn, interval_ms)
    }

    /// Poll until HELLO (op 10) or a failure. Bounded by
    /// [`DISCORD_HELLO_TIMEOUT_SECS`].
    fn await_hello(&self, conn: &mut WsConn) -> Result<u64, Reconnect> {
        let start = Instant::now();
        loop {
            if start.elapsed() >= Duration::from_secs(DISCORD_HELLO_TIMEOUT_SECS) {
                eprintln!("[pq-stream-capture] discord-gateway HELLO timeout — reconnecting");
                return Err(reconnect_reason(self.session.is_some()));
            }
            match conn.poll_event() {
                Ok(None) | Ok(Some(WsEvent::Pong)) => {}
                Ok(Some(WsEvent::Binary(_))) => {
                    eprintln!(
                        "[pq-stream-capture] discord-gateway DRIFT: binary frame before HELLO"
                    );
                }
                Ok(Some(WsEvent::Closed(r))) => {
                    eprintln!("[pq-stream-capture] discord-gateway closed before HELLO: {r}");
                    return Err(reconnect_reason(self.session.is_some()));
                }
                Ok(Some(WsEvent::Text(t))) => match json::parse(&t) {
                    Ok(v) => match classify(&v) {
                        Inbound::Hello {
                            heartbeat_interval_ms,
                        } => return Ok(heartbeat_interval_ms),
                        other => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway DRIFT: expected HELLO, got {other:?}"
                            );
                            return Err(reconnect_reason(self.session.is_some()));
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "[pq-stream-capture] discord-gateway DRIFT: unparseable pre-HELLO frame: {e}"
                        );
                    }
                },
                Err(e) => {
                    eprintln!(
                        "[pq-stream-capture] discord-gateway transport error before HELLO: {e}"
                    );
                    return Err(reconnect_reason(self.session.is_some()));
                }
            }
        }
    }

    /// The message pump with Gateway heartbeat scheduling, zombie detection and
    /// the staleness watchdog. Returns the reconnect intent.
    fn event_loop(&mut self, conn: &mut WsConn, interval_ms: u64) -> Reconnect {
        let interval = Duration::from_millis(interval_ms);
        let zombie = Duration::from_millis(zombie_deadline_ms(interval_ms));
        let first_delay =
            Duration::from_millis(first_heartbeat_delay_ms(interval_ms, self.cfg.jitter_seed));
        let mut last_seq = self.session.as_ref().and_then(|s| s.last_seq);
        let mut awaiting_ack = false;
        let mut last_hb = Instant::now();
        let mut next_hb = Instant::now() + first_delay;
        let mut last_frame = Instant::now();
        loop {
            // NB: no conn.maybe_keepalive() — Gateway op-1 heartbeats keep the
            // socket warm; WS-level pings would be an atypical client shape.
            if last_frame.elapsed() >= Duration::from_secs(DISCORD_STALE_SECS) {
                eprintln!(
                    "[pq-stream-capture] discord-gateway STALE: no frame for \
                     {DISCORD_STALE_SECS}s — forcing reconnect"
                );
                return reconnect_reason(self.session.is_some());
            }
            if awaiting_ack && last_hb.elapsed() >= zombie {
                eprintln!(
                    "[pq-stream-capture] discord-gateway ZOMBIE: heartbeat un-ACKed for \
                     interval*1.5 — forcing reconnect"
                );
                return reconnect_reason(self.session.is_some());
            }
            if !awaiting_ack && Instant::now() >= next_hb {
                if let Err(e) = conn.send_text(&heartbeat_payload(last_seq)) {
                    eprintln!("[pq-stream-capture] discord-gateway heartbeat write failed: {e}");
                    return reconnect_reason(self.session.is_some());
                }
                awaiting_ack = true;
                last_hb = Instant::now();
                next_hb = last_hb + interval;
            }
            match conn.poll_event() {
                Ok(None) | Ok(Some(WsEvent::Pong)) => {}
                Ok(Some(WsEvent::Binary(_))) => {
                    eprintln!("[pq-stream-capture] discord-gateway DRIFT: unexpected binary frame");
                }
                Ok(Some(WsEvent::Closed(reason))) => {
                    eprintln!("[pq-stream-capture] discord-gateway closed by server: {reason}");
                    return reconnect_reason(self.session.is_some());
                }
                Ok(Some(WsEvent::Text(text))) => {
                    last_frame = Instant::now();
                    let recv = (self.now_ms)();
                    let v = match json::parse(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway DRIFT: unparseable frame: {e}"
                            );
                            continue;
                        }
                    };
                    match classify(&v) {
                        Inbound::HeartbeatAck => awaiting_ack = false,
                        Inbound::HeartbeatRequest => {
                            if let Err(e) = conn.send_text(&heartbeat_payload(last_seq)) {
                                eprintln!(
                                    "[pq-stream-capture] discord-gateway heartbeat(req) write failed: {e}"
                                );
                                return reconnect_reason(self.session.is_some());
                            }
                            awaiting_ack = true;
                            last_hb = Instant::now();
                            next_hb = last_hb + interval;
                        }
                        Inbound::Hello { .. } => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway DRIFT: second HELLO mid-session"
                            );
                        }
                        Inbound::Reconnect => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway op7 RECONNECT — will RESUME"
                            );
                            return Reconnect::Resume;
                        }
                        Inbound::InvalidSession { resumable } => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway op9 INVALID_SESSION resumable={resumable}"
                            );
                            let action = invalid_session_action(resumable);
                            if action == Reconnect::Reidentify {
                                self.session = None;
                            }
                            return action;
                        }
                        Inbound::Dispatch { t, seq, d } => {
                            if let Some(s) = seq {
                                last_seq = Some(s);
                            }
                            if !self.handle_dispatch(t, d, last_seq, recv) {
                                return reconnect_reason(self.session.is_some());
                            }
                        }
                        Inbound::Other { op } => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway DRIFT: unhandled op {op}"
                            );
                        }
                        Inbound::Drift => {
                            eprintln!(
                                "[pq-stream-capture] discord-gateway DRIFT: unrecognized frame shape"
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[pq-stream-capture] discord-gateway transport error: {e}");
                    return reconnect_reason(self.session.is_some());
                }
            }
        }
    }

    /// Route one DISPATCH. Returns `false` only when stdout is gone (end the
    /// session and let the reconnect loop decide).
    fn handle_dispatch(
        &mut self,
        t: &str,
        d: Option<&Value>,
        last_seq: Option<u64>,
        recv: u64,
    ) -> bool {
        let mut alive = true;
        match t {
            "READY" => match d.and_then(ready_of) {
                Some((session_id, resume_gateway_url)) => {
                    eprintln!(
                        "[pq-stream-capture] discord-gateway READY: session established, resume host set"
                    );
                    self.session = Some(Session {
                        session_id,
                        resume_gateway_url,
                        last_seq,
                    });
                    self.attempt = 0;
                }
                None => eprintln!(
                    "[pq-stream-capture] discord-gateway DRIFT: READY without session_id/resume_gateway_url"
                ),
            },
            "RESUMED" => {
                eprintln!("[pq-stream-capture] discord-gateway RESUMED: replayed missed events");
                self.attempt = 0;
            }
            "MESSAGE_CREATE" | "MESSAGE_UPDATE" => match d {
                Some(d) => match process_message(
                    d,
                    &self.cfg.allow,
                    &self.cfg.callers,
                    &mut self.ring,
                    recv,
                    self.out,
                ) {
                    Ok(MsgOutcome::Emitted) => self.attempt = 0,
                    Ok(MsgOutcome::Dropped | MsgOutcome::Deduped) => {}
                    Err(e) => {
                        eprintln!("[pq-stream-capture] discord-gateway stdout write failed: {e}");
                        alive = false;
                    }
                },
                None => eprintln!(
                    "[pq-stream-capture] discord-gateway DRIFT: {t} dispatch without payload"
                ),
            },
            // Other dispatches (GUILD_CREATE flood on connect, TYPING_START,
            // PRESENCE_UPDATE, …) are normal and high-volume — ignored quietly.
            _ => {}
        }
        // Keep the session's resume cursor current.
        if let Some(s) = self.session.as_mut() {
            s.last_seq = last_seq;
        }
        alive
    }
}

/// Lane entry point. `now_ms` is the injected capture clock (§22).
pub fn run(args: &[String], now_ms: fn() -> u64) -> u8 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[pq-stream-capture] discord-gateway: {e}");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let env_var = parsed.token_kind.env_var();
    let token = match std::env::var(env_var) {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            eprintln!(
                "[pq-stream-capture] discord-gateway ARMING_FAILED: {env_var} is not set — \
                 refusing to start (fail-closed, exit {EXIT_ARMING}; never a silent retry loop)"
            );
            return EXIT_ARMING;
        }
    };
    let gateway_url =
        std::env::var("DISCORD_GATEWAY_URL").unwrap_or_else(|_| DEFAULT_GATEWAY_URL.to_string());
    let identify_json = identify_payload(&token, intents(), &parsed.props, DEFAULT_PRESENCE_STATUS);
    eprintln!(
        "[pq-stream-capture] discord-gateway: PASSIVE read-only client \
         (token-kind={}, intents={}, presence={}); {} guild(s)/{} channel(s) allowlisted, \
         {} designated caller(s). Sends only IDENTIFY/RESUME/HEARTBEAT; zero REST; \
         no rotation/proxy/fake-activity.",
        parsed.token_kind.as_str(),
        intents(),
        DEFAULT_PRESENCE_STATUS,
        parsed.allow.guilds.len(),
        parsed.allow.channels.len(),
        parsed.callers.len()
    );
    let cfg = Config {
        gateway_url,
        identify_json,
        token,
        allow: parsed.allow,
        callers: parsed.callers,
        jitter_seed: parsed.jitter_seed,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut live = Live {
        cfg: &cfg,
        out: &mut out,
        now_ms,
        ring: DedupeRing::new(DISCORD_DEDUPE_CAP),
        session: None,
        attempt: 0,
    };
    live.serve()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    // ------------------------------------------------------- pure builders

    #[test]
    fn intents_bitmask_is_guilds_guild_messages_message_content() {
        // 1 | 512 | 32768 = 33281.
        assert_eq!(INTENT_GUILDS, 1);
        assert_eq!(INTENT_GUILD_MESSAGES, 512);
        assert_eq!(INTENT_MESSAGE_CONTENT, 32768);
        assert_eq!(intents(), 33281);
    }

    #[test]
    fn identify_payload_exact_shape() {
        let payload = identify_payload(
            "TOKENV",
            intents(),
            &ClientProperties::default(),
            "invisible",
        );
        assert_eq!(
            payload,
            "{\"op\":2,\"d\":{\"token\":\"TOKENV\",\"intents\":33281,\
             \"properties\":{\"os\":\"Windows\",\"browser\":\"Discord Client\",\
             \"device\":\"desktop\"},\"presence\":{\"status\":\"invisible\",\
             \"afk\":false,\"activities\":[]}}}"
        );
        assert!(json::parse(&payload).is_ok());
    }

    #[test]
    fn identify_places_token_raw_and_presence_invisible() {
        let payload = identify_payload(
            "Bot xyz.abc",
            intents(),
            &ClientProperties::default(),
            "invisible",
        );
        // Token is raw in d.token (no "Bot " stripping/adding by us).
        assert!(payload.contains("\"token\":\"Bot xyz.abc\""));
        assert!(payload.contains("\"status\":\"invisible\""));
        assert!(payload.contains("\"activities\":[]"));
    }

    #[test]
    fn resume_payload_exact_shape() {
        assert_eq!(
            resume_payload("TOK", "sess-9", 4242),
            "{\"op\":6,\"d\":{\"token\":\"TOK\",\"session_id\":\"sess-9\",\"seq\":4242}}"
        );
    }

    #[test]
    fn heartbeat_payload_seq_and_null() {
        assert_eq!(heartbeat_payload(Some(7)), "{\"op\":1,\"d\":7}");
        assert_eq!(heartbeat_payload(None), "{\"op\":1,\"d\":null}");
        assert!(json::parse(&heartbeat_payload(Some(7))).is_ok());
        assert!(json::parse(&heartbeat_payload(None)).is_ok());
    }

    // -------------------------------------------------------- classification

    #[test]
    fn classify_hello_reads_interval() {
        let v = json::parse(r#"{"op":10,"d":{"heartbeat_interval":41250}}"#).unwrap();
        assert_eq!(
            classify(&v),
            Inbound::Hello {
                heartbeat_interval_ms: 41250
            }
        );
    }

    #[test]
    fn classify_heartbeat_ack_and_request() {
        assert_eq!(
            classify(&json::parse(r#"{"op":11}"#).unwrap()),
            Inbound::HeartbeatAck
        );
        assert_eq!(
            classify(&json::parse(r#"{"op":1,"d":null}"#).unwrap()),
            Inbound::HeartbeatRequest
        );
    }

    #[test]
    fn classify_dispatch_carries_t_s_d() {
        let v = json::parse(r#"{"op":0,"t":"MESSAGE_CREATE","s":55,"d":{"id":"1"}}"#).unwrap();
        match classify(&v) {
            Inbound::Dispatch { t, seq, d } => {
                assert_eq!(t, "MESSAGE_CREATE");
                assert_eq!(seq, Some(55));
                assert_eq!(
                    d.and_then(|d| d.get("id")).and_then(Value::as_str),
                    Some("1")
                );
            }
            other => panic!("misclassified: {other:?}"),
        }
    }

    #[test]
    fn classify_reconnect_and_invalid_session() {
        assert_eq!(
            classify(&json::parse(r#"{"op":7,"d":null}"#).unwrap()),
            Inbound::Reconnect
        );
        assert_eq!(
            classify(&json::parse(r#"{"op":9,"d":true}"#).unwrap()),
            Inbound::InvalidSession { resumable: true }
        );
        assert_eq!(
            classify(&json::parse(r#"{"op":9,"d":false}"#).unwrap()),
            Inbound::InvalidSession { resumable: false }
        );
    }

    #[test]
    fn classify_unknown_op_and_missing_op_are_other_or_drift() {
        assert_eq!(
            classify(&json::parse(r#"{"op":3}"#).unwrap()),
            Inbound::Other { op: 3 }
        );
        assert_eq!(
            classify(&json::parse(r#"{"t":"x"}"#).unwrap()),
            Inbound::Drift
        );
        // op 0 without t is drift, not a half-built Dispatch.
        assert_eq!(
            classify(&json::parse(r#"{"op":0,"d":{}}"#).unwrap()),
            Inbound::Drift
        );
        // Hello without the interval is drift.
        assert_eq!(
            classify(&json::parse(r#"{"op":10,"d":{}}"#).unwrap()),
            Inbound::Drift
        );
    }

    #[test]
    fn classify_never_panics_on_garbage_values() {
        // Whatever parses must classify without panicking (Drift/Other are fine).
        for s in [
            "null",
            "true",
            "123",
            "\"str\"",
            "[]",
            "{}",
            r#"{"op":"notnum"}"#,
            r#"{"op":0}"#,
            r#"{"op":9}"#,
            r#"{"op":0,"t":123}"#,
        ] {
            let v = json::parse(s).unwrap();
            let _ = classify(&v); // must not panic
        }
    }

    #[test]
    fn ready_of_extracts_session_and_resume_url() {
        let d = json::parse(
            r#"{"session_id":"S1","resume_gateway_url":"wss://gw-b.discord.gg","v":10}"#,
        )
        .unwrap();
        assert_eq!(
            ready_of(&d),
            Some(("S1".to_string(), "wss://gw-b.discord.gg".to_string()))
        );
        assert_eq!(
            ready_of(&json::parse(r#"{"session_id":"S1"}"#).unwrap()),
            None
        );
    }

    // ---------------------------------------------------- reconnect decision

    #[test]
    fn op7_resumes_op9_dead_reidentifies() {
        // op 7 → RESUME (the event loop maps Inbound::Reconnect → Resume).
        assert_eq!(
            classify(&json::parse(r#"{"op":7}"#).unwrap()),
            Inbound::Reconnect
        );
        assert_eq!(invalid_session_action(true), Reconnect::Resume);
        assert_eq!(invalid_session_action(false), Reconnect::Reidentify);
    }

    #[test]
    fn reconnect_reason_tracks_session_presence() {
        assert_eq!(reconnect_reason(true), Reconnect::Resume);
        assert_eq!(reconnect_reason(false), Reconnect::Reidentify);
    }

    // ------------------------------------------------------------ timing

    #[test]
    fn resume_url_appends_query_once() {
        assert_eq!(
            resume_url("wss://gw-b.discord.gg"),
            "wss://gw-b.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(
            resume_url("wss://gw-b.discord.gg/"),
            "wss://gw-b.discord.gg/?v=10&encoding=json"
        );
        // Already-parameterized base is used verbatim.
        assert_eq!(
            resume_url("wss://local/echo?v=10&encoding=json"),
            "wss://local/echo?v=10&encoding=json"
        );
    }

    #[test]
    fn snowflake_decodes_to_unix_ms() {
        // Discord's documented worked example: id 175928847299117063 →
        // 2016-04-30T11:18:25.796Z = 1462015105796 ms.
        assert_eq!(
            snowflake_to_unix_ms("175928847299117063"),
            1_462_015_105_796
        );
        assert_eq!(snowflake_to_unix_ms("0"), DISCORD_EPOCH_MS);
        assert_eq!(snowflake_to_unix_ms("not-a-snowflake"), 0);
        assert_eq!(snowflake_to_unix_ms(""), 0);
    }

    #[test]
    fn first_heartbeat_delay_none_is_full_interval_seed_is_fraction() {
        assert_eq!(first_heartbeat_delay_ms(41250, None), 41250);
        // seed 500/1000 → half the interval.
        assert_eq!(first_heartbeat_delay_ms(41250, Some(500)), 20625);
        assert_eq!(first_heartbeat_delay_ms(41250, Some(0)), 0);
        // seeds wrap at JITTER_RESOLUTION.
        assert_eq!(first_heartbeat_delay_ms(1000, Some(1250)), 250);
    }

    #[test]
    fn zombie_deadline_is_interval_times_one_and_a_half() {
        assert_eq!(zombie_deadline_ms(41250), 61875);
        assert_eq!(zombie_deadline_ms(1000), 1500);
        assert_eq!(zombie_deadline_ms(0), 0);
    }

    // ---------------------------------------------------------- allowlist

    #[test]
    fn allowlist_empty_dimension_is_unconstrained() {
        let empty = Allowlist::default();
        assert!(empty.allowed("any-guild", "any-channel"));
    }

    #[test]
    fn allowlist_filters_guild_and_channel() {
        let allow = Allowlist {
            guilds: set(&["G1"]),
            channels: set(&["C1", "C2"]),
        };
        assert!(allow.allowed("G1", "C1"));
        assert!(allow.allowed("G1", "C2"));
        assert!(!allow.allowed("G2", "C1"), "wrong guild");
        assert!(!allow.allowed("G1", "C9"), "wrong channel");
        // A guild-only allowlist still gates the guild dimension.
        let guild_only = Allowlist {
            guilds: set(&["G1"]),
            channels: HashSet::new(),
        };
        assert!(guild_only.allowed("G1", "anything"));
        assert!(!guild_only.allowed("G2", "anything"));
    }

    #[test]
    fn designated_caller_membership() {
        let callers = set(&["111", "222"]);
        assert!(is_designated_caller(&callers, "111"));
        assert!(!is_designated_caller(&callers, "333"));
        assert!(
            !is_designated_caller(&callers, ""),
            "empty id is never a caller"
        );
    }

    #[test]
    fn parse_allowlist_file_populates_all_kinds() {
        let text = "# alpha rooms\nguild:G1\nchannel: C1 \ncaller:U9\n\n# end\n";
        let mut allow = Allowlist::default();
        let mut callers = HashSet::new();
        parse_allowlist_file(text, &mut allow, &mut callers).unwrap();
        assert!(allow.guilds.contains("G1"));
        assert!(allow.channels.contains("C1"));
        assert!(callers.contains("U9"));
    }

    #[test]
    fn parse_allowlist_file_rejects_bad_lines() {
        let mut allow = Allowlist::default();
        let mut callers = HashSet::new();
        assert!(parse_allowlist_file("noseparator", &mut allow, &mut callers).is_err());
        assert!(parse_allowlist_file("guild:", &mut allow, &mut callers).is_err());
        assert!(parse_allowlist_file("bogus:X", &mut allow, &mut callers).is_err());
    }

    // ------------------------------------------------------------ extraction

    #[test]
    fn extract_cashtags_uppercases_dedupes_and_bounds() {
        assert_eq!(extract_cashtags("buy $wif and $WIF now"), vec!["WIF"]);
        assert_eq!(extract_cashtags("$BONK vs $wif"), vec!["BONK", "WIF"]);
        // Too short / too long runs are not tickers.
        assert!(extract_cashtags("$x costs money").is_empty());
        assert!(extract_cashtags("$verylongtickerxx nope").is_empty());
        assert!(extract_cashtags("no cashtags here").is_empty());
        // Bounded at MAX_CASHTAGS.
        let many = "$AA $BB $CC $DD $EE $FF $GG $HH $II $JJ";
        assert_eq!(extract_cashtags(many).len(), MAX_CASHTAGS);
    }

    #[test]
    fn extract_mints_matches_base58_length_window() {
        let mint = "So11111111111111111111111111111111111111112";
        assert!((MINT_B58_MIN..=MINT_B58_MAX).contains(&mint.len()));
        assert_eq!(
            extract_mints(&format!("CA: {mint} ape")),
            vec![mint.to_string()]
        );
        // Duplicates dedupe.
        assert_eq!(
            extract_mints(&format!("{mint} {mint}")),
            vec![mint.to_string()]
        );
        // Too short (a normal word) is ignored; forbidden glyphs break the run.
        assert!(extract_mints("just a normal sentence").is_empty());
        // A run containing '0' (not base58) is split and both halves too short.
        assert!(extract_mints("abc0def").is_empty());
    }

    // ------------------------------------------------------- normalization

    fn message(
        id: &str,
        guild: &str,
        chan: &str,
        author_id: &str,
        name: &str,
        content: &str,
    ) -> Value {
        json::parse(&format!(
            r#"{{"id":"{id}","guild_id":"{guild}","channel_id":"{chan}",
                "author":{{"id":"{author_id}","username":"{name}"}},"content":"{content}"}}"#
        ))
        .unwrap()
    }

    #[test]
    fn normalize_message_exact_key_order_and_values() {
        let d = message(
            "175928847299117063",
            "G1",
            "C1",
            "999",
            "caller1",
            "ape $WIF now",
        );
        let callers = set(&["999"]);
        let line = normalize_message(&d, &callers);
        assert_eq!(
            line,
            "{\"lane\":\"discord_alpha\",\"platform\":\"discord\",\"guild_id\":\"G1\",\
             \"channel_id\":\"C1\",\"author_id\":\"999\",\"author\":\"caller1\",\
             \"community\":\"C1\",\"content\":\"ape $WIF now\",\"is_designated_caller\":true,\
             \"ts\":1462015105796,\"cashtags\":[\"WIF\"],\"mints\":[]}"
        );
        assert!(json::parse(&line).is_ok());
    }

    #[test]
    fn normalize_marks_non_caller_false_and_extracts_mint() {
        let mint = "So11111111111111111111111111111111111111112";
        let d = message("1", "G1", "C1", "222", "rando", &format!("gm CA {mint}"));
        let line = normalize_message(&d, &set(&["999"]));
        assert!(line.contains("\"is_designated_caller\":false"));
        assert!(line.contains(&format!("\"mints\":[\"{mint}\"]")));
    }

    #[test]
    fn normalize_degrades_absent_fields() {
        let d = json::parse("{}").unwrap();
        let line = normalize_message(&d, &HashSet::new());
        assert!(line.contains("\"guild_id\":\"\""));
        assert!(line.contains("\"author\":\"\""));
        assert!(line.contains("\"community\":\"\""));
        assert!(line.contains("\"is_designated_caller\":false"));
        assert!(line.contains("\"ts\":0"));
        assert!(line.contains("\"cashtags\":[]"));
        assert!(json::parse(&line).is_ok());
    }

    #[test]
    fn process_message_emits_drops_and_dedupes() {
        let allow = Allowlist {
            guilds: set(&["G1"]),
            channels: set(&["C1"]),
        };
        let callers = set(&["999"]);
        let mut ring = DedupeRing::new(64);

        // Allowlisted → Emitted, raw + normalized (2 lines).
        let good = message("10", "G1", "C1", "999", "caller1", "$WIF");
        let mut out = Vec::new();
        assert_eq!(
            process_message(&good, &allow, &callers, &mut ring, 7, &mut out).unwrap(),
            MsgOutcome::Emitted
        );
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("{\"lane\":\"discord\",\"recv_unix_ms\":7,"));
        assert!(lines[1].starts_with("{\"lane\":\"discord_alpha\","));

        // Same id again → Deduped, nothing written.
        let mut out2 = Vec::new();
        assert_eq!(
            process_message(&good, &allow, &callers, &mut ring, 8, &mut out2).unwrap(),
            MsgOutcome::Deduped
        );
        assert!(out2.is_empty());

        // Wrong channel → Dropped, nothing written, no ring slot consumed.
        let elsewhere = message("11", "G1", "C9", "999", "caller1", "$WIF");
        let mut out3 = Vec::new();
        assert_eq!(
            process_message(&elsewhere, &allow, &callers, &mut ring, 9, &mut out3).unwrap(),
            MsgOutcome::Dropped
        );
        assert!(out3.is_empty());
    }

    #[test]
    fn token_kind_env_and_tag() {
        assert_eq!(TokenKind::User.env_var(), "DISCORD_USER_TOKEN");
        assert_eq!(TokenKind::Bot.env_var(), "DISCORD_BOT_TOKEN");
        assert_eq!(TokenKind::User.as_str(), "user");
        assert_eq!(TokenKind::Bot.as_str(), "bot");
    }

    #[test]
    fn parse_args_defaults_and_flags() {
        let d = parse_args(&[]).unwrap();
        assert_eq!(d.token_kind, TokenKind::User);
        assert!(d.allow.guilds.is_empty());
        assert_eq!(d.props.os, "Windows");
        assert!(d.jitter_seed.is_none());

        let args: Vec<String> = [
            "--token-kind",
            "bot",
            "--guilds",
            "G1,G2",
            "--channels",
            "C1",
            "--callers",
            "U1,U2",
            "--client-os",
            "Linux",
            "--heartbeat-jitter-seed",
            "500",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.token_kind, TokenKind::Bot);
        assert_eq!(p.allow.guilds, set(&["G1", "G2"]));
        assert_eq!(p.allow.channels, set(&["C1"]));
        assert_eq!(p.callers, set(&["U1", "U2"]));
        assert_eq!(p.props.os, "Linux");
        assert_eq!(p.jitter_seed, Some(500));
    }

    #[test]
    fn parse_args_rejects_bad_flags() {
        assert!(parse_args(&["--token-kind".into(), "admin".into()]).is_err());
        assert!(parse_args(&["--guilds".into()]).is_err(), "missing value");
        assert!(parse_args(&["--nonsense".into()]).is_err());
        assert!(parse_args(&["--heartbeat-jitter-seed".into(), "NaN".into()]).is_err());
    }
}
