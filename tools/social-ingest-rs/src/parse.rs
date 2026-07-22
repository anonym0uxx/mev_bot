//! Minimal IRC line parsing for anonymous read-only Twitch chat.
//!
//! Responsibility: classify ONE raw IRC protocol line into the tiny event set the
//! capture edge needs — `PING` (keepalive we must answer), `PRIVMSG` (a chat line
//! we normalize and emit), everything else (silently ignored). This is the `[S]`
//! capture side of the social lane; it never makes a decision.
//!
//! # Constitution discipline (binding)
//! * **§22 determinism boundary.** Parsing here is a pure `&str -> IrcEvent`
//!   function: no clock, no network, no RNG, no float. The wall clock is read only
//!   in `main` at the capture edge (the one place it is allowed); the deterministic
//!   core never reads it.
//! * **§29 provenance.** The author (chatter nick) and community (channel) are
//!   extracted verbatim (ASCII-lowercased, Twitch's own canonical form) so the
//!   core's FNV identity hashing sees a stable origin id per chatter/channel.
//!   A chat line is an *origination* — echo/copy detection is downstream via
//!   content hash, never guessed here.
//! * **§99-spirit bounding.** Input lines are truncated at [`MAX_LINE_BYTES`]
//!   (UTF-8-boundary safe) before parsing; malformed lines return
//!   [`IrcEvent::Other`] — this module never panics on hostile input.
//!
//! Twitch specifics tolerated: IRCv3 `@tags` prefixes (we never request the
//! capability, but a server sending them anyway is dropped harmlessly) and
//! `\x01ACTION ...\x01` (`/me`) framing, which is unwrapped to the inner text.

/// Hard cap on one raw IRC line before parsing (bytes). The IRC RFC caps lines at
/// 512 bytes but Twitch tag-lines run longer; 4096 bounds hostile input without
/// clipping any realistic chat message.
pub const MAX_LINE_BYTES: usize = 4096;

/// One parsed chat message (`PRIVMSG`) ready for normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// Channel without the leading `#`, ASCII-lowercased (normalized `community`).
    pub community: String,
    /// Chatter nick from the `:nick!user@host` prefix, ASCII-lowercased
    /// (normalized `author`).
    pub author: String,
    /// Raw message text (ACTION framing unwrapped), cashtags + contract addresses
    /// left intact for the deterministic core to extract.
    pub text: String,
}

/// Classification of one raw IRC line. Total: every input maps to exactly one
/// variant; malformed input collapses into [`IrcEvent::Other`] (skip, never panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcEvent {
    /// `PING :token` — the transport must answer `PONG :token` to stay connected.
    Ping(String),
    /// A `PRIVMSG` chat line to normalize and emit.
    Chat(ChatLine),
    /// Any other verb, or a line too malformed to classify. Ignored silently.
    Other,
}

/// Parse one raw IRC line. Pure function of its input (§22): no I/O, no clock.
#[must_use]
pub fn parse_line(raw: &str) -> IrcEvent {
    let line = truncate_at_boundary(raw, MAX_LINE_BYTES);
    let mut rest = line.trim_end_matches(['\r', '\n']);

    // IRCv3 tags prefix (`@key=val;... `): tolerated and dropped. We never send
    // `CAP REQ`, but a proxy or future server behavior must not break capture.
    if let Some(r) = rest.strip_prefix('@') {
        match r.split_once(' ') {
            Some((_tags, after)) => rest = after,
            None => return IrcEvent::Other,
        }
    }

    // Optional `:prefix ` (server name or `nick!user@host`).
    let mut prefix = "";
    if let Some(r) = rest.strip_prefix(':') {
        match r.split_once(' ') {
            Some((p, after)) => {
                prefix = p;
                rest = after;
            }
            None => return IrcEvent::Other,
        }
    }

    let (verb, params) = match rest.split_once(' ') {
        Some((v, p)) => (v, p),
        None => (rest, ""),
    };

    if verb.eq_ignore_ascii_case("PING") {
        let token = params.strip_prefix(':').unwrap_or(params);
        return IrcEvent::Ping(token.to_string());
    }
    if !verb.eq_ignore_ascii_case("PRIVMSG") {
        return IrcEvent::Other;
    }

    // PRIVMSG needs an origin: an anonymous/serverless PRIVMSG is malformed.
    let author = match prefix.split('!').next() {
        Some(nick) if !nick.is_empty() => nick.to_ascii_lowercase(),
        _ => return IrcEvent::Other,
    };

    // Params: `#channel :message text`.
    let (target, tail) = match params.split_once(' ') {
        Some((t, rest)) => (t, rest),
        None => return IrcEvent::Other,
    };
    let community = target
        .strip_prefix('#')
        .unwrap_or(target)
        .to_ascii_lowercase();
    if community.is_empty() {
        return IrcEvent::Other;
    }
    // Twitch always sends the message as an IRC trailing param (`:...`); tolerate
    // the colon-less single-param form some test rigs produce.
    let text = tail.strip_prefix(':').unwrap_or(tail);
    let text = unwrap_action(text).to_string();

    IrcEvent::Chat(ChatLine {
        community,
        author,
        text,
    })
}

/// Unwrap CTCP `/me` framing: `\x01ACTION <text>\x01` → `<text>`. Anything not
/// exactly framed is returned untouched.
fn unwrap_action(text: &str) -> &str {
    text.strip_prefix("\u{1}ACTION ")
        .and_then(|t| t.strip_suffix('\u{1}'))
        .unwrap_or(text)
}

/// Truncate `s` to at most `max` bytes, backing up to a UTF-8 char boundary so
/// the result is always valid `&str` (never panics, never splits a code point).
fn truncate_at_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(raw: &str) -> ChatLine {
        match parse_line(raw) {
            IrcEvent::Chat(c) => c,
            other => panic!("expected Chat, got {other:?} for {raw:?}"),
        }
    }

    #[test]
    fn privmsg_basic() {
        let c = chat(":degen!degen@degen.tmi.twitch.tv PRIVMSG #pumpwatch :$WIF to a billion\r\n");
        assert_eq!(c.author, "degen");
        assert_eq!(c.community, "pumpwatch");
        assert_eq!(c.text, "$WIF to a billion");
    }

    #[test]
    fn privmsg_with_ircv3_tags_prefix() {
        let raw = "@badge-info=;badges=;color=#FF0000;display-name=TagGuy \
                   :tagguy!tagguy@tagguy.tmi.twitch.tv PRIVMSG #pumpwatch :tags tolerated";
        let c = chat(raw);
        assert_eq!(c.author, "tagguy");
        assert_eq!(c.text, "tags tolerated");
    }

    #[test]
    fn privmsg_author_and_channel_lowercased() {
        let c = chat(":CoinCaller!x@x.tmi.twitch.tv PRIVMSG #PumpWatch :hi");
        assert_eq!(c.author, "coincaller");
        assert_eq!(c.community, "pumpwatch");
    }

    #[test]
    fn ping_with_colon_token() {
        assert_eq!(
            parse_line("PING :tmi.twitch.tv\r\n"),
            IrcEvent::Ping("tmi.twitch.tv".to_string())
        );
    }

    #[test]
    fn ping_without_colon_token() {
        assert_eq!(parse_line("PING abc"), IrcEvent::Ping("abc".to_string()));
    }

    #[test]
    fn action_message_unwrapped() {
        let c = chat(":m!m@m.tmi.twitch.tv PRIVMSG #c :\u{1}ACTION slurps the dip\u{1}");
        assert_eq!(c.text, "slurps the dip");
    }

    #[test]
    fn unterminated_action_left_verbatim() {
        let c = chat(":m!m@m.tmi.twitch.tv PRIVMSG #c :\u{1}ACTION half-framed");
        assert_eq!(c.text, "\u{1}ACTION half-framed");
    }

    #[test]
    fn junk_line_skipped() {
        assert_eq!(parse_line("this is not irc at all"), IrcEvent::Other);
        assert_eq!(parse_line(""), IrcEvent::Other);
    }

    #[test]
    fn privmsg_missing_message_skipped() {
        assert_eq!(parse_line(":x!x@x PRIVMSG #chan"), IrcEvent::Other);
    }

    #[test]
    fn privmsg_without_prefix_skipped() {
        assert_eq!(parse_line("PRIVMSG #chan :orphan"), IrcEvent::Other);
    }

    #[test]
    fn other_verbs_ignored() {
        assert_eq!(
            parse_line(":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!"),
            IrcEvent::Other
        );
        assert_eq!(parse_line(":n!u@h JOIN #chan"), IrcEvent::Other);
        assert_eq!(parse_line(":tmi.twitch.tv RECONNECT"), IrcEvent::Other);
    }

    #[test]
    fn dangling_tags_only_line_skipped() {
        assert_eq!(parse_line("@badges=;color=#FFF"), IrcEvent::Other);
        assert_eq!(parse_line(":prefixonly"), IrcEvent::Other);
    }

    #[test]
    fn host_only_prefix_uses_whole_prefix_as_author() {
        // Not a shape Twitch sends for chat, but tolerated rather than dropped.
        let c = chat(":somehost.example PRIVMSG #c :hello");
        assert_eq!(c.author, "somehost.example");
    }

    #[test]
    fn long_line_truncated_at_4096_bytes() {
        let mut raw = String::from(":spam!s@s.tmi.twitch.tv PRIVMSG #c :");
        raw.push_str(&"A".repeat(10_000));
        let c = chat(&raw);
        assert_eq!(
            c.text.len(),
            MAX_LINE_BYTES - ":spam!s@s.tmi.twitch.tv PRIVMSG #c :".len()
        );
    }

    #[test]
    fn truncation_respects_utf8_char_boundary() {
        // Fill so a 4-byte emoji straddles the 4096-byte cap; must not panic and
        // must stay valid UTF-8 (the straddling char is dropped whole).
        let head = ":u!u@u.tmi.twitch.tv PRIVMSG #c :";
        let pad = MAX_LINE_BYTES - head.len() - 2; // leave 2 bytes, emoji needs 4
        let mut raw = String::from(head);
        raw.push_str(&"x".repeat(pad));
        raw.push('\u{1F680}'); // rocket, 4 bytes, straddles the cap
        let c = chat(&raw);
        assert_eq!(c.text.len(), pad, "straddling code point dropped whole");
        assert!(c.text.ends_with('x'));
    }
}
