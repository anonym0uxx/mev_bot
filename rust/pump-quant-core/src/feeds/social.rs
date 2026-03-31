//! Social signal aggregation — Phase 0 logging infrastructure.
//!
//! Defines types for tracking social media signals (Twitter, Telegram, Discord, etc.)
//! per token mint. Phase 0 provides the data structures and logging fields;
//! actual feed wiring comes in Phase 1.

use hashbrown::HashMap;

// ─── Enums ─────────────────────────────────────────────────────────

/// Source platform for a social signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SocialSource {
    Twitter   = 0,
    Telegram  = 1,
    Discord   = 2,
    Website   = 3,
    Unknown   = 4,
}

/// Type of social signal observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SocialSignalType {
    Mention           = 0,
    CashtTag          = 1,
    TokenLink         = 2,
    CreatorPromo      = 3,
    InfluencerMention = 4,
    BotPromo          = 5,
}

// ─── Signal Struct ─────────────────────────────────────────────────

/// A single social signal event for a token mint.
pub struct SocialSignal {
    /// The token mint (32-byte Solana public key).
    pub mint: [u8; 32],
    /// Which platform this came from.
    pub source: SocialSource,
    /// What kind of signal.
    pub signal_type: SocialSignalType,
    /// When this signal was observed (epoch ms).
    pub timestamp_ms: u64,
    /// Follower count of the account (if from Twitter/social).
    pub follower_count: Option<u32>,
    /// Normalized engagement score (0–10000).
    pub engagement_score: u16,
    /// Whether the source account is likely a bot.
    pub is_bot_likely: bool,
    /// URL of the source post/page, if available.
    pub source_url: Option<String>,
    /// Hash of the raw text content (for dedup; we don't store full text).
    pub raw_text_hash: u64,
}

// ─── Per-Mint Social Profile ───────────────────────────────────────

/// Aggregated social profile for a single token mint.
#[derive(Debug, Clone)]
pub struct MintSocialProfile {
    pub total_mentions: u16,
    pub unique_sources: u8,
    pub max_follower_count: u32,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub bot_mention_count: u16,
    pub organic_mention_count: u16,
    pub has_twitter: bool,
    pub has_telegram: bool,
    pub has_website: bool,
    /// Bitmask of SocialSource variants seen (for unique_sources counting).
    sources_seen: u8,
}

impl MintSocialProfile {
    fn new(ts_ms: u64) -> Self {
        Self {
            total_mentions: 0,
            unique_sources: 0,
            max_follower_count: 0,
            first_seen_ms: ts_ms,
            last_seen_ms: ts_ms,
            bot_mention_count: 0,
            organic_mention_count: 0,
            has_twitter: false,
            has_telegram: false,
            has_website: false,
            sources_seen: 0,
        }
    }

    /// Record a signal into this profile.
    fn record(&mut self, signal: &SocialSignal) {
        self.total_mentions = self.total_mentions.saturating_add(1);
        self.last_seen_ms = self.last_seen_ms.max(signal.timestamp_ms);

        // Track unique sources via bitmask
        let bit = 1u8 << (signal.source as u8);
        if self.sources_seen & bit == 0 {
            self.sources_seen |= bit;
            self.unique_sources = self.sources_seen.count_ones() as u8;
        }

        // Platform flags
        match signal.source {
            SocialSource::Twitter  => self.has_twitter = true,
            SocialSource::Telegram => self.has_telegram = true,
            SocialSource::Website  => self.has_website = true,
            _ => {}
        }

        // Follower tracking
        if let Some(fc) = signal.follower_count {
            self.max_follower_count = self.max_follower_count.max(fc);
        }

        // Bot vs organic
        if signal.is_bot_likely {
            self.bot_mention_count = self.bot_mention_count.saturating_add(1);
        } else {
            self.organic_mention_count = self.organic_mention_count.saturating_add(1);
        }
    }
}

// ─── Social Aggregator ────────────────────────────────────────────

/// Tracks social activity per token mint.
pub struct SocialAggregator {
    profiles: HashMap<[u8; 32], MintSocialProfile>,
}

impl SocialAggregator {
    /// Create a new empty aggregator.
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    /// Record a social signal, creating a profile if needed.
    pub fn record_signal(&mut self, signal: &SocialSignal) {
        let profile = self
            .profiles
            .entry(signal.mint)
            .or_insert_with(|| MintSocialProfile::new(signal.timestamp_ms));
        profile.record(signal);
    }

    /// Get the social profile for a mint, if any signals have been recorded.
    pub fn get_profile(&self, mint: &[u8; 32]) -> Option<&MintSocialProfile> {
        self.profiles.get(mint)
    }

    /// Compute a composite social score (0–10000) for a mint.
    ///
    /// Formula (weighted sum):
    ///   - unique_sources:          30%  (max 5 sources → 10000 at 5)
    ///   - organic_mention_count:   25%  (saturates at 50 mentions → 10000)
    ///   - max_follower_reach:      20%  (log-scaled, saturates at 1M followers)
    ///   - recency:                 15%  (decays over 1 hour)
    ///   - diversity_bonus:         10%  (has_twitter + has_telegram + has_website)
    pub fn social_score(&self, mint: &[u8; 32]) -> u16 {
        let profile = match self.profiles.get(mint) {
            Some(p) => p,
            None => return 0,
        };

        // 1. Unique sources (30%) — max 5 sources, linear scale
        let sources_score = ((profile.unique_sources as u32).min(5) * 10000 / 5) as u16;

        // 2. Organic mention count (25%) — saturates at 50
        let organic_score = ((profile.organic_mention_count as u32).min(50) * 10000 / 50) as u16;

        // 3. Max follower reach (20%) — log-scaled, saturates at 1M
        let follower_score = if profile.max_follower_count == 0 {
            0u16
        } else {
            // log2(followers) / log2(1_000_000) * 10000
            let log_followers = (profile.max_follower_count as f64).log2();
            let log_max = 1_000_000f64.log2(); // ~19.93
            let ratio = (log_followers / log_max).min(1.0).max(0.0);
            (ratio * 10000.0) as u16
        };

        // 4. Recency (15%) — decays over 1 hour from last_seen
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age_ms = now_ms.saturating_sub(profile.last_seen_ms);
        let one_hour_ms = 3_600_000u64;
        let recency_score = if age_ms >= one_hour_ms {
            0u16
        } else {
            ((one_hour_ms - age_ms) * 10000 / one_hour_ms) as u16
        };

        // 5. Diversity bonus (10%) — has_twitter + has_telegram + has_website
        let diversity_count = profile.has_twitter as u32
            + profile.has_telegram as u32
            + profile.has_website as u32;
        let diversity_score = (diversity_count * 10000 / 3).min(10000) as u16;

        // Weighted sum
        let composite = (sources_score as u32) * 30
            + (organic_score as u32) * 25
            + (follower_score as u32) * 20
            + (recency_score as u32) * 15
            + (diversity_score as u32) * 10;

        (composite / 100).min(10000) as u16
    }
}

impl Default for SocialAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_aggregator_returns_zero_score() {
        let agg = SocialAggregator::new();
        let mint = [0u8; 32];
        assert_eq!(agg.social_score(&mint), 0);
        assert!(agg.get_profile(&mint).is_none());
    }

    #[test]
    fn single_signal_records_correctly() {
        let mut agg = SocialAggregator::new();
        let mint = [1u8; 32];
        let signal = SocialSignal {
            mint,
            source: SocialSource::Twitter,
            signal_type: SocialSignalType::Mention,
            timestamp_ms: 1_700_000_000_000,
            follower_count: Some(10_000),
            engagement_score: 5000,
            is_bot_likely: false,
            source_url: None,
            raw_text_hash: 12345,
        };
        agg.record_signal(&signal);

        let profile = agg.get_profile(&mint).unwrap();
        assert_eq!(profile.total_mentions, 1);
        assert_eq!(profile.unique_sources, 1);
        assert_eq!(profile.organic_mention_count, 1);
        assert_eq!(profile.bot_mention_count, 0);
        assert!(profile.has_twitter);
        assert!(!profile.has_telegram);
        assert_eq!(profile.max_follower_count, 10_000);

        // Score should be > 0
        assert!(agg.social_score(&mint) > 0);
    }

    #[test]
    fn bot_signals_tracked_separately() {
        let mut agg = SocialAggregator::new();
        let mint = [2u8; 32];

        // Organic signal
        agg.record_signal(&SocialSignal {
            mint,
            source: SocialSource::Twitter,
            signal_type: SocialSignalType::Mention,
            timestamp_ms: 1_700_000_000_000,
            follower_count: None,
            engagement_score: 100,
            is_bot_likely: false,
            source_url: None,
            raw_text_hash: 1,
        });

        // Bot signal
        agg.record_signal(&SocialSignal {
            mint,
            source: SocialSource::Telegram,
            signal_type: SocialSignalType::BotPromo,
            timestamp_ms: 1_700_000_000_100,
            follower_count: None,
            engagement_score: 50,
            is_bot_likely: true,
            source_url: None,
            raw_text_hash: 2,
        });

        let profile = agg.get_profile(&mint).unwrap();
        assert_eq!(profile.total_mentions, 2);
        assert_eq!(profile.organic_mention_count, 1);
        assert_eq!(profile.bot_mention_count, 1);
        assert_eq!(profile.unique_sources, 2);
        assert!(profile.has_twitter);
        assert!(profile.has_telegram);
    }
}
