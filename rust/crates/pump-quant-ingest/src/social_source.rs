//! The social-capture seam (leaf `in_social_source`).
//!
//! Responsibility: define the **trait boundary** between live social capture
//! (`[S]` server: twitterapi.io stream, Telegram MTProto, a TikTok scraper,
//! Firecrawl) and the deterministic decoder [`crate::social_parse`]. The live
//! adapter's only job is to capture posts and normalize each into the
//! vendor-agnostic JSON [`crate::social_parse::parse_social_event`] expects,
//! stamping each with a measured `observed_at_ns`. This crate never performs I/O
//! (§22, ARCHITECTURE rule 4); it owns the trait and a portable in-memory
//! [`MockSocialSource`] for tests, and the pure fan-out that turns a batch of
//! raw payloads into [`SocialEvent`]s.

use crate::social_parse::{parse_social_event, SocialEvent};

/// One captured, already-normalized payload plus its measured capture instant.
///
/// The bytes are the vendor-agnostic JSON documented on
/// [`parse_social_event`]; `observed_at_ns` is measured by the live adapter at
/// capture time (the only place a clock is read — never inside this crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSocialPayload {
    /// Normalized JSON for exactly one post.
    pub json: Vec<u8>,
    /// Capture instant, ns (measured by the `[S]` adapter).
    pub observed_at_ns: u64,
}

impl RawSocialPayload {
    /// Construct a payload from owned JSON bytes and a measured instant.
    #[must_use]
    pub fn new(json: Vec<u8>, observed_at_ns: u64) -> Self {
        Self {
            json,
            observed_at_ns,
        }
    }
}

/// The live-capture seam. A server adapter implements this over its vendor API;
/// the deterministic core only ever sees [`RawSocialPayload`]s through it.
///
/// `next_batch` returns whatever has been captured since the last call (possibly
/// empty). It is intentionally pull-based so the deterministic driver controls
/// cadence and a replay can substitute a recorded batch stream for byte-exact
/// re-execution (§54). Implementations must be non-blocking best-effort: never
/// stall the decision loop waiting on the network.
pub trait SocialSource {
    /// Drain and return the payloads captured since the previous call.
    fn next_batch(&mut self) -> Vec<RawSocialPayload>;
}

/// Parse a batch of raw payloads into [`SocialEvent`]s, dropping any that fail to
/// decode (an unparseable payload is skipped, never panics). Order-preserving and
/// pure — the same batch always yields the same events (§22).
#[must_use]
pub fn parse_batch(batch: &[RawSocialPayload]) -> Vec<SocialEvent> {
    batch
        .iter()
        .filter_map(|p| parse_social_event(&p.json, p.observed_at_ns))
        .collect()
}

/// A portable, deterministic [`SocialSource`] for tests and replay: it hands back
/// pre-loaded payloads in fixed batches, no I/O. This is the seam's stand-in that
/// lets the whole social pipeline be exercised on the laptop with zero network
/// (the same role [`crate::helius_parse`] fixtures play for the on-chain path).
#[derive(Debug, Clone, Default)]
pub struct MockSocialSource {
    batches: Vec<Vec<RawSocialPayload>>,
    cursor: usize,
}

impl MockSocialSource {
    /// A source that yields nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one batch to be returned by a future [`Self::next_batch`] call.
    #[must_use]
    pub fn with_batch(mut self, batch: Vec<RawSocialPayload>) -> Self {
        self.batches.push(batch);
        self
    }

    /// Whether every queued batch has been drained.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.cursor >= self.batches.len()
    }
}

impl SocialSource for MockSocialSource {
    fn next_batch(&mut self) -> Vec<RawSocialPayload> {
        if self.cursor < self.batches.len() {
            let b = self.batches[self.cursor].clone();
            self.cursor += 1;
            b
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::social_parse::SocialPlatform;

    fn payload(platform: &str, text: &str, at: u64) -> RawSocialPayload {
        let json = format!(r#"{{"platform":"{platform}","author":"a","text":"{text}","likes":1}}"#)
            .into_bytes();
        RawSocialPayload::new(json, at)
    }

    #[test]
    fn mock_yields_queued_batches_then_empties() {
        let mut src = MockSocialSource::new()
            .with_batch(vec![payload("x", "$WIF go", 10)])
            .with_batch(vec![
                payload("telegram", "$BONK call", 20),
                payload("tiktok", "meme szn", 21),
            ]);
        let b1 = src.next_batch();
        assert_eq!(b1.len(), 1);
        let b2 = src.next_batch();
        assert_eq!(b2.len(), 2);
        assert!(src.next_batch().is_empty());
        assert!(src.is_drained());
    }

    #[test]
    fn parse_batch_decodes_and_skips_bad() {
        let batch = vec![
            payload("x", "$WIF", 1),
            RawSocialPayload::new(b"garbage".to_vec(), 2),
            payload("telegram", "$PEPE", 3),
        ];
        let events = parse_batch(&batch);
        assert_eq!(events.len(), 2, "bad payload skipped, not fatal");
        assert_eq!(events[0].platform, SocialPlatform::X);
        assert_eq!(events[1].platform, SocialPlatform::Telegram);
        assert_eq!(events[1].observed_at_ns, 3);
    }
}
