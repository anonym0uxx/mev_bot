//! Entry randomizer — ported from TypeScript `src/mev/entry-randomizer.ts`.
//!
//! Adds jitter to entry timing and position size to avoid MEV fingerprinting.
//! Bot detection algorithms on-chain look for:
//!   - Fixed entry sizes (e.g., always 0.12 SOL)
//!   - Fixed timing patterns (e.g., always ~50ms after trigger)
//!   - Consistent tip amounts
//!
//! This module randomizes delay and size within configurable bounds.
//! Uses a simple xorshift64 PRNG (no heap allocation, no syscall).

/// Configuration for entry randomization.
#[derive(Debug, Clone)]
pub struct RandomizerConfig {
    /// Minimum jitter delay in ms before entry (default 50).
    pub jitter_ms_min: u32,
    /// Maximum jitter delay in ms before entry (default 200).
    pub jitter_ms_max: u32,
    /// Size variance as fraction (default 0.20 = ±20%).
    pub size_variance_pct: f64,
    /// Base entry size in lamports.
    pub base_entry_lamports: u64,
}

impl Default for RandomizerConfig {
    fn default() -> Self {
        Self {
            jitter_ms_min: 50,
            jitter_ms_max: 200,
            size_variance_pct: 0.20,
            base_entry_lamports: 120_000_000, // 0.12 SOL
        }
    }
}

/// Randomized entry parameters.
pub struct RandomizedEntry {
    /// Delay in milliseconds before executing the entry.
    pub delay_ms: u32,
    /// Position size in lamports (randomized around base ± variance).
    pub size_lamports: u64,
}

/// Fast, allocation-free entry randomizer using xorshift64 PRNG.
///
/// The PRNG is seeded from the monotonic clock at construction time.
/// It's NOT cryptographically secure — we only need uniform distribution
/// for anti-fingerprinting, not unpredictability.
pub struct EntryRandomizer {
    config: RandomizerConfig,
    state: u64,
}

impl EntryRandomizer {
    /// Create a new randomizer with the given config.
    /// Seeds the PRNG from the current time.
    pub fn new(config: RandomizerConfig) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // Ensure non-zero seed (xorshift requires it)
        let state = if seed == 0 { 0xDEAD_BEEF_CAFE_BABE } else { seed };
        Self { config, state }
    }

    /// Generate randomized entry parameters.
    ///
    /// Delay: uniform random in [jitter_ms_min, jitter_ms_max]
    /// Size: uniform random in [base * (1 - variance), base * (1 + variance)]
    #[inline]
    pub fn randomize(&mut self) -> RandomizedEntry {
        let r1 = self.next_u64();
        let r2 = self.next_u64();

        // Delay: map r1 to [min, max]
        let range = self.config.jitter_ms_max.saturating_sub(self.config.jitter_ms_min) + 1;
        let delay_ms = self.config.jitter_ms_min + (r1 as u32 % range);

        // Size: map r2 to [base * (1 - v), base * (1 + v)]
        let variance = self.config.size_variance_pct;
        let base = self.config.base_entry_lamports as f64;
        let low = base * (1.0 - variance);
        let high = base * (1.0 + variance);
        // Convert r2 to [0, 1) range
        let frac = (r2 as f64) / (u64::MAX as f64);
        let size = low + frac * (high - low);
        let size_lamports = size as u64;

        RandomizedEntry {
            delay_ms,
            size_lamports,
        }
    }

    /// xorshift64 PRNG step. Fast, no heap, period 2^64-1.
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Update base entry size (for dynamic sizing via tiered config).
    #[inline]
    pub fn set_base_entry_lamports(&mut self, lamports: u64) {
        self.config.base_entry_lamports = lamports;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_randomize_delay_in_range() {
        let config = RandomizerConfig {
            jitter_ms_min: 50,
            jitter_ms_max: 200,
            ..Default::default()
        };
        let mut rng = EntryRandomizer::new(config);

        for _ in 0..1000 {
            let entry = rng.randomize();
            assert!(entry.delay_ms >= 50, "delay {} < 50", entry.delay_ms);
            assert!(entry.delay_ms <= 200, "delay {} > 200", entry.delay_ms);
        }
    }

    #[test]
    fn test_randomize_size_in_range() {
        let config = RandomizerConfig {
            base_entry_lamports: 120_000_000, // 0.12 SOL
            size_variance_pct: 0.20,
            ..Default::default()
        };
        let mut rng = EntryRandomizer::new(config);

        let low = 96_000_000u64;  // 0.12 * 0.80
        let high = 144_000_000u64; // 0.12 * 1.20

        for _ in 0..1000 {
            let entry = rng.randomize();
            assert!(
                entry.size_lamports >= low && entry.size_lamports <= high,
                "size {} not in [{}, {}]",
                entry.size_lamports, low, high
            );
        }
    }

    #[test]
    fn test_randomize_produces_variety() {
        let mut rng = EntryRandomizer::new(RandomizerConfig::default());
        let entries: Vec<_> = (0..10).map(|_| rng.randomize()).collect();

        // At least some should differ (extremely unlikely all identical)
        let all_same_delay = entries.iter().all(|e| e.delay_ms == entries[0].delay_ms);
        assert!(!all_same_delay, "all delays identical — PRNG broken");

        let all_same_size = entries.iter().all(|e| e.size_lamports == entries[0].size_lamports);
        assert!(!all_same_size, "all sizes identical — PRNG broken");
    }
}
