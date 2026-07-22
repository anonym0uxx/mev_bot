//! Social-attention payload parsing (leaf `in_social_parse`).
//!
//! Responsibility: turn a **vendor-agnostic normalized** social payload (one
//! post/tweet/message/video, as minimal JSON) into a deterministic, bounded
//! [`SocialEvent`] — the attention-layer analogue of what [`crate::helius_parse`]
//! and [`crate::pumpportal_parse`] do for on-chain trades. The live capture that
//! produces the normalized JSON (twitterapi.io stream, Telegram MTProto, a TikTok
//! scraper, Firecrawl) is OUT OF SCOPE here — it is `[S]` live-I/O behind the
//! [`crate::social_source`] trait. This module is the pure decoder only.
//!
//! # Constitution discipline (binding)
//! * **§22 determinism / integer.** No floating point, no wall-clock, no RNG, no
//!   network. The observation instant is supplied by the caller as an
//!   already-measured `u64` nanosecond value (`observed_at_ns`); this module never
//!   reads a clock. Every derived field is an integer or a fixed-width hash.
//! * **§29 provenance + horizon.** Every event carries its [`SocialPlatform`]
//!   provenance and its measured `observed_at_ns`; timing is never equated across
//!   platforms by this layer (a Telegram call and an X post keep their own source
//!   and instant). Echo is separated from origination (`is_echo`) so reach is
//!   never mistaken for alpha (fade-first).
//! * **§99 bounded.** Extracted cashtags and mints are capped at
//!   [`MAX_CASHTAGS`] / [`MAX_MINTS`]; a post naming more is truncated, never
//!   allowed to grow an unbounded allocation.
//!
//! The tradeable signal downstream is deterministic attention *velocity /
//! breadth / authenticity*, never an LLM sentiment label — any such label is a
//! research artifact only (§83) and is intentionally absent from this type.

use crate::base58;
use crate::json;

/// FNV-1a 64-bit offset basis (matches `pump_quant_memory::hashing`).
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Deterministic FNV-1a 64-bit hash. Byte-identical on every platform; used here
/// to fold variable-length handles / community ids / post text into fixed-width
/// `u64` identity so [`SocialEvent`] is `Copy`, bounded, and replay-stable (§22).
///
/// Overflow contract: FNV is defined modulo 2^64, so the wrapping multiply/xor is
/// the intended arithmetic, not an overflow bug (§22 explicit-overflow discipline).
#[must_use]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Maximum distinct cashtags kept from one post (§99). A post naming more is
/// truncated at the cap — memecoin spam posts sometimes list dozens of tickers.
pub const MAX_CASHTAGS: usize = 8;
/// Maximum distinct Solana mint addresses kept from one post (§99).
pub const MAX_MINTS: usize = 4;
/// Minimum cashtag body length (after `$`), e.g. `$OK`.
const CASHTAG_MIN: usize = 2;
/// Maximum cashtag body length (after `$`); longer runs are not tickers.
const CASHTAG_MAX: usize = 10;
/// Minimum base58 length of a Solana address (32-byte key, base58-encoded).
const MINT_B58_MIN: usize = 32;
/// Maximum base58 length of a Solana address.
const MINT_B58_MAX: usize = 44;

/// The originating platform of a social attention event (§29 provenance).
///
/// Kept distinct so downstream never equates timing/authority across sources: a
/// Telegram call and an X-KOL post carry different measured latency (the Signal-
/// Horizon Law), and `Web` (a general-web scrape, e.g. Firecrawl) is broad
/// context, not a real-time call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocialPlatform {
    /// Twitter / X (crypto-Twitter).
    X,
    /// TikTok (slower, broad meta emergence).
    TikTok,
    /// Telegram call channels (lowest latency).
    Telegram,
    /// General web (news / aggregators / project pages), e.g. Firecrawl.
    Web,
    /// Twitch live-stream chat (real-time viewing; §29.6 stream/comment events).
    /// Captured by the dependency-free Rust IRC lane (`tools/social-ingest-rs`).
    Twitch,
    /// Pump.fun-native social surface (per-coin replies / communities): venue-
    /// native commentary, structurally the EARLIEST off-chain signal — degens
    /// comment on the coin page before X/Telegram pick it up. Captured by the
    /// tier-3 frontend lane (`pq-social-capture pump`) behind a degradation
    /// sentinel (undocumented endpoint, §18.8).
    Pump,
    /// Aggregator surfaces (CoinGecko trending/listings/categories): the
    /// LEGIBILITY tier (§783) — a coin the aggregator lists is already
    /// surfaced to the whole market. Never earliness; feeds the pre-legibility
    /// clock and aggregator-sentiment corroboration.
    Aggregator,
}

impl SocialPlatform {
    /// Parse the normalized `"platform"` string. Total; unknown → `None`.
    #[must_use]
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "x" | "twitter" => Some(Self::X),
            "tiktok" => Some(Self::TikTok),
            "telegram" | "tg" => Some(Self::Telegram),
            "web" | "firecrawl" => Some(Self::Web),
            "twitch" => Some(Self::Twitch),
            "pump" => Some(Self::Pump),
            "coingecko" | "aggregator" => Some(Self::Aggregator),
            _ => None,
        }
    }

    /// A stable small code for journalling/registry use.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::X => 1,
            Self::TikTok => 2,
            Self::Telegram => 3,
            Self::Web => 4,
            Self::Twitch => 5,
            Self::Pump => 6,
            Self::Aggregator => 7,
        }
    }

    /// Signal-Horizon provenance rank (§29 Signal-Horizon Law): a *structural*
    /// ordering of how far upstream each source sits in the shill pipeline —
    /// smaller = earlier / less legible. Telegram call-channels lead X-KOL
    /// amplification, which leads slow-meta TikTok and the legibility-tier general
    /// web (a coin on a scraped aggregator page is already surfaced).
    ///
    /// This is **provenance metadata for horizon classification and cross-source
    /// latency comparison, never a tradeable weight**: a lower rank does not score
    /// higher, it only sits earlier, so timing is never equated across ranks. The
    /// ordering is fixed by the pipeline's structure, not a tunable decision
    /// constant (§22/§102).
    #[must_use]
    pub const fn horizon_rank(self) -> u8 {
        match self {
            // Live-stream chat is a real-time push channel: structurally as early
            // as Telegram call channels (both sit at the unlegible front of the
            // shill pipeline). Equal rank = equal tier, never a tradeable weight.
            // Venue-native replies sit with the real-time push tier: the coin
            // page is where the FIRST off-chain reaction lands.
            Self::Telegram | Self::Twitch | Self::Pump => 0,
            Self::X => 1,
            Self::TikTok => 2,
            // Aggregator listings share the legibility tier with the general
            // web: by the time a coin is on the board, earliness is gone.
            Self::Web | Self::Aggregator => 3,
        }
    }
}

/// A deterministic, bounded social attention event: one normalized post.
///
/// `Copy` and fixed-size so a journal of them replays without allocation churn,
/// exactly like [`crate::canonical::CanonicalTx`]. Author/community/content are
/// folded to `u64` identity via [`fnv1a_64`]; cashtags are stored as ticker
/// hashes (uppercased) and mints as raw 32-byte keys so the event both clusters
/// by symbol and names concrete markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocialEvent {
    /// Provenance (§29). Never collapsed across platforms.
    pub platform: SocialPlatform,
    /// Caller-measured observation instant, ns. This module never reads a clock.
    pub observed_at_ns: u64,
    /// Author identity (FNV-1a of the handle) — the distinct-originator unit.
    pub author_id: u64,
    /// Community/channel identity (FNV-1a), or `0` when the platform has none.
    pub community_id: u64,
    /// Content fingerprint (FNV-1a of normalized text) for dedup / copy-echo.
    pub content_hash: u64,
    /// Engagement weight (e.g. likes + reposts + replies), saturating integer.
    pub engagement: u64,
    /// Whether this post is a reply/repost/quote — an *echo*, not origination.
    /// Echoes raise reach but never authenticity/breadth (fade-first, §29).
    pub is_echo: bool,
    /// Distinct cashtag ticker hashes named in the post (uppercased), bounded.
    pub cashtags: [u64; MAX_CASHTAGS],
    /// Count of valid entries in [`Self::cashtags`].
    pub n_cashtags: u8,
    /// Distinct Solana mint keys named in the post, bounded.
    pub mints: [[u8; 32]; MAX_MINTS],
    /// Count of valid entries in [`Self::mints`].
    pub n_mints: u8,
    /// LLM/aggregator-derived sentiment toward the named token, bps of 10_000
    /// (5_000 = neutral, 0 = maximally bearish). [`SENTIMENT_UNKNOWN`] when no
    /// enrichment annotated the event — UNKNOWN is labeled, never defaulted to
    /// neutral (§6.4). Enrichment is a recorded INPUT (the brain seam runs off
    /// the hot path); replay of the same annotated stream is byte-identical.
    pub sentiment_bp: u32,
    /// Confidence of [`Self::sentiment_bp`], bps. [`SENTIMENT_UNKNOWN`] when absent.
    pub sentiment_conf_bp: u32,
    /// Whether an AGGREGATOR (e.g. CoinGecko) lists/surfaces this token — the
    /// §783 legibility clock. `false` means "no aggregator evidence in this
    /// event", never "not listed".
    pub aggregator_listed: bool,
}

/// Sentinel for an ABSENT sentiment annotation (§6.4: unknown stays unknown;
/// no valid reading uses this value — the valid domain is 0..=10_000).
pub const SENTIMENT_UNKNOWN: u32 = u32::MAX;

impl SocialEvent {
    /// The valid cashtag hashes as a slice.
    #[must_use]
    pub fn cashtags(&self) -> &[u64] {
        &self.cashtags[..self.n_cashtags as usize]
    }

    /// The valid mint keys as a slice.
    #[must_use]
    pub fn mints(&self) -> &[[u8; 32]] {
        &self.mints[..self.n_mints as usize]
    }

    /// Whether the post names at least one concrete market or ticker (otherwise it
    /// is untargeted chatter the discovery lanes cannot attach to a mint).
    #[must_use]
    pub fn is_targeted(&self) -> bool {
        self.n_cashtags > 0 || self.n_mints > 0
    }
}

/// Extract distinct uppercased cashtag hashes from free text (`$TICKER`).
///
/// A cashtag is `$` immediately followed by [`CASHTAG_MIN`]..=[`CASHTAG_MAX`]
/// ASCII alphanumerics; the body is uppercased (so `$wif` == `$WIF`) and hashed.
/// Deterministic, bounded (§99): scanning stops recording after [`MAX_CASHTAGS`]
/// distinct tickers. Returns `(hashes, count)`.
#[must_use]
pub fn extract_cashtags(text: &str) -> ([u64; MAX_CASHTAGS], u8) {
    let bytes = text.as_bytes();
    let mut out = [0u64; MAX_CASHTAGS];
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // Collect the alphanumeric body after '$', uppercasing ASCII letters.
        let mut buf = [0u8; CASHTAG_MAX];
        let mut len = 0usize;
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_alphanumeric() && len < CASHTAG_MAX {
            buf[len] = bytes[j].to_ascii_uppercase();
            len += 1;
            j += 1;
        }
        // Reject if the run overflowed the max (a longer alnum run is not a ticker)
        // or is too short.
        let overflowed = j < bytes.len() && bytes[j].is_ascii_alphanumeric();
        if len >= CASHTAG_MIN && !overflowed {
            let h = fnv1a_64(&buf[..len]);
            if !out[..n].contains(&h) && n < MAX_CASHTAGS {
                out[n] = h;
                n += 1;
            }
        }
        i = j.max(i + 1);
    }
    (out, n as u8)
}

/// Whether a byte is in the base58 (Bitcoin) alphabet — the delimiter test used to
/// carve address-shaped tokens out of free text.
#[inline]
fn is_base58_char(b: u8) -> bool {
    // 0..9 A..Z a..z minus 0 O I l (the four base58-excluded glyphs).
    b.is_ascii_alphanumeric() && b != b'0' && b != b'O' && b != b'I' && b != b'l'
}

/// Extract distinct Solana mint keys named in free text.
///
/// Scans maximal base58-alphabet runs; a run of [`MINT_B58_MIN`]..=[`MINT_B58_MAX`]
/// chars that base58-decodes to exactly 32 bytes is a mint. Deterministic, bounded
/// at [`MAX_MINTS`] (§99). Returns `(mints, count)`.
#[must_use]
pub fn extract_solana_mints(text: &str) -> ([[u8; 32]; MAX_MINTS], u8) {
    let bytes = text.as_bytes();
    let mut out = [[0u8; 32]; MAX_MINTS];
    let mut n = 0usize;
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
            // Safe: the run is ASCII base58 by construction.
            if let Ok(tok) = core::str::from_utf8(&bytes[start..i]) {
                if let Some(key) = base58::decode_pubkey(tok) {
                    if !out[..n].contains(&key) && n < MAX_MINTS {
                        out[n] = key;
                        n += 1;
                    }
                }
            }
        }
    }
    (out, n as u8)
}

/// Parse one **normalized** social payload into a [`SocialEvent`].
///
/// Expected JSON (produced by the `[S]` live adapter that normalizes each vendor):
/// ```json
/// { "platform": "x", "author": "someHandle", "community": "chan-or-empty",
///   "text": "gm $WIF ...", "likes": 12, "reposts": 3, "replies": 1,
///   "echo": false }
/// ```
/// `observed_at_ns` is supplied out-of-band (measured at capture). Missing
/// engagement fields default to 0; missing `community` → `0`; `echo` defaults to
/// `false`. Returns `None` only when the JSON is unparseable or lacks a known
/// `platform`/`author`/`text`. No float, no clock (§22).
#[must_use]
pub fn parse_social_event(raw: &[u8], observed_at_ns: u64) -> Option<SocialEvent> {
    let v = json::parse(raw)?;
    let platform = SocialPlatform::from_tag(v.get("platform")?.as_str()?)?;
    let author = v.get("author")?.as_str()?;
    let text = v.get("text")?.as_str()?;

    let community = v.get("community").and_then(|c| c.as_str()).unwrap_or("");
    let echo = v.get("echo").and_then(|e| e.as_bool()).unwrap_or(false);

    let eng_field = |k: &str| -> u64 {
        v.get(k)
            .and_then(|n| n.as_number_str())
            .and_then(json::number_to_u128_trunc)
            .map(|x| x.min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    };
    let engagement = eng_field("likes")
        .saturating_add(eng_field("reposts"))
        .saturating_add(eng_field("replies"));

    let (cashtags, n_cashtags) = extract_cashtags(text);
    let (mut mints, mut n_mints) = extract_solana_mints(text);
    // Optional EXPLICIT mint reference (§29 provenance): a capture lane with
    // thread context (e.g. a pump.fun coin page's replies) names the coin at
    // MINT GRADE — stronger than any ticker or in-text match. It is prepended
    // (deduplicated) so canonical identity outranks text extraction; an
    // invalid value is ignored, never guessed (§6.4 fail-closed resolution).
    if let Some(m58) = v.get("mint").and_then(|m| m.as_str()) {
        if let Some(key) = base58::decode_pubkey(m58) {
            let already = mints[..n_mints as usize].contains(&key);
            if !already {
                let mut shifted = [[0u8; 32]; MAX_MINTS];
                shifted[0] = key;
                let keep = (n_mints as usize).min(MAX_MINTS - 1);
                shifted[1..=keep].copy_from_slice(&mints[..keep]);
                mints = shifted;
                n_mints = (keep + 1) as u8;
            } else {
                // Promote the explicit reference to slot 0 (identity-first).
                let pos = mints[..n_mints as usize]
                    .iter()
                    .position(|k| *k == key)
                    .unwrap_or(0);
                mints.swap(0, pos);
            }
        }
    }

    // Optional brain-seam / aggregator annotations (§6.4: absent = UNKNOWN,
    // out-of-range = UNKNOWN — a malformed annotation is no annotation).
    let sent_field = |k: &str| -> u32 {
        v.get(k)
            .and_then(|n| n.as_number_str())
            .and_then(json::number_to_u128_trunc)
            .filter(|&x| x <= 10_000)
            .map(|x| x as u32)
            .unwrap_or(SENTIMENT_UNKNOWN)
    };
    let sentiment_bp = sent_field("sentiment_bp");
    let sentiment_conf_bp = sent_field("sentiment_conf_bp");
    let aggregator_listed = v
        .get("aggregator_listed")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    Some(SocialEvent {
        platform,
        observed_at_ns,
        author_id: fnv1a_64(author.as_bytes()),
        community_id: if community.is_empty() {
            0
        } else {
            fnv1a_64(community.as_bytes())
        },
        content_hash: fnv1a_64(text.as_bytes()),
        engagement,
        is_echo: echo,
        cashtags,
        n_cashtags,
        mints,
        n_mints,
        sentiment_bp,
        sentiment_conf_bp,
        aggregator_listed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_tags_roundtrip() {
        assert_eq!(SocialPlatform::from_tag("x"), Some(SocialPlatform::X));
        assert_eq!(SocialPlatform::from_tag("twitter"), Some(SocialPlatform::X));
        assert_eq!(
            SocialPlatform::from_tag("tg"),
            Some(SocialPlatform::Telegram)
        );
        assert_eq!(
            SocialPlatform::from_tag("firecrawl"),
            Some(SocialPlatform::Web)
        );
        assert_eq!(SocialPlatform::from_tag("myspace"), None);
    }

    #[test]
    fn cashtags_uppercase_dedup_and_bound() {
        let (h, n) = extract_cashtags("gm $wif and $WIF and $BONK $bonk");
        assert_eq!(n, 2, "case-folded duplicates collapse");
        assert_eq!(h[0], fnv1a_64(b"WIF"));
        assert_eq!(h[1], fnv1a_64(b"BONK"));
        // Too short / too long are rejected.
        let (_, n2) = extract_cashtags("$X $TOOOOOOOOOOONG");
        assert_eq!(n2, 0);
        // Bound holds.
        let many = "$AA $BB $CC $DD $EE $FF $GG $HH $II $JJ";
        let (_, n3) = extract_cashtags(many);
        assert_eq!(n3 as usize, MAX_CASHTAGS);
    }

    #[test]
    fn mint_extraction_valid_only() {
        // A real 32-byte base58 key (USDC mint) embedded in text.
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let text = format!("aping {usdc} rn, ticker $USDC not an addr word");
        let (m, n) = extract_solana_mints(&text);
        assert_eq!(n, 1);
        assert_eq!(m[0], base58::decode_pubkey(usdc).unwrap());
        // No false positive from ordinary words.
        let (_, n2) = extract_solana_mints("just some normal english words here");
        assert_eq!(n2, 0);
    }

    #[test]
    fn parse_full_payload() {
        let raw = br#"{"platform":"x","author":"kolguy","community":"",
            "text":"send it $WIF EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "likes":100,"reposts":20,"replies":5,"echo":false}"#;
        let ev = parse_social_event(raw, 1_000_000_000).unwrap();
        assert_eq!(ev.platform, SocialPlatform::X);
        assert_eq!(ev.observed_at_ns, 1_000_000_000);
        assert_eq!(ev.author_id, fnv1a_64(b"kolguy"));
        assert_eq!(ev.community_id, 0);
        assert_eq!(ev.engagement, 125);
        assert!(!ev.is_echo);
        assert_eq!(ev.n_cashtags, 1);
        assert_eq!(ev.n_mints, 1);
        assert!(ev.is_targeted());
    }

    #[test]
    fn parse_rejects_unknown_platform_and_missing_fields() {
        assert!(parse_social_event(br#"{"platform":"foo","author":"a","text":"t"}"#, 0).is_none());
        assert!(parse_social_event(br#"{"platform":"x","text":"t"}"#, 0).is_none());
        assert!(parse_social_event(br#"not json"#, 0).is_none());
    }

    #[test]
    fn same_payload_same_event_determinism() {
        let raw = br#"{"platform":"telegram","author":"caller","community":"alpha-chan",
            "text":"$PEPE breaking out","likes":3,"echo":true}"#;
        let a = parse_social_event(raw, 42).unwrap();
        let b = parse_social_event(raw, 42).unwrap();
        assert_eq!(a, b);
        assert!(a.is_echo);
        assert_eq!(a.community_id, fnv1a_64(b"alpha-chan"));
    }
}
