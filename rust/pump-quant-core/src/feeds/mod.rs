pub mod pumpportal;
pub mod helius;
pub mod shredstream;
pub mod corecast;
pub mod event_joiner;

// ── Feed event types ────────────────────────────────────────────────

/// Source of a graduation/migration detection event.
/// Used by GraduationArbEngine for dedup and latency tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MigrationSource {
    /// Detected via Helius logsSubscribe (primary, fastest ~50ms)
    HeliusLogs = 0,
    /// Detected via CoreCast/Bitquery stream 2 Raydium AMM trades (fallback, ~80ms)
    CoreCastStream2 = 1,
}

impl MigrationSource {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeliusLogs => "helius",
            Self::CoreCastStream2 => "corecast",
        }
    }
}

#[derive(Debug)]
pub enum FeedEvent {
    Trade(TradeEvent),
    PreWarm(PreWarmEvent), // Helius-only, no vSol
    CreatorSell { mint: [u8; 32], ts_ms: u64 },
    /// Token migrated to Raydium AMM — force-exit any open position.
    Migration {
        mint: [u8; 32],
        ts_ms: u64,
        /// Source feed that detected this graduation event
        source: MigrationSource,
        /// Full 64-byte Solana transaction signature (for getTransaction RPC calls + dedup)
        sig: [u8; 64],
    },
    /// LP removal / rug detection — force-exit any open position.
    LpRemoval { mint: [u8; 32], ts_ms: u64 },
    /// New token created — carries regime exclusion flags.
    TokenCreated(TokenCreatedEvent),
    Tick { ts_ms: u64 }, // 50ms timer tick for dead-token decay check
    Shutdown,
}

/// Token creation event from PumpPortal.
/// Carries metadata needed for regime classification (mayhem/agent detection).
#[derive(Debug, Clone)]
pub struct TokenCreatedEvent {
    pub mint: [u8; 32],
    pub is_mayhem: bool,
    pub is_tokenized_agent: bool,
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    pub mint: [u8; 32],
    pub trader: [u8; 32],
    pub sig: [u8; 64],
    pub sig_prefix: [u8; 8],
    pub sol_amount: u64,     // lamports
    pub token_amount: u64,
    pub vsol_reserves: u64,  // lamports
    pub vtoken_reserves: u64,
    pub market_cap_sol: u64,
    pub slot: u64,
    pub timestamp_ms: u64,
    pub is_buy: bool,
    pub source: FeedSource,
    pub bonding_curve: [u8; 32],
    pub assoc_bonding_curve: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeedSource {
    PumpPortal,
    Helius,
    ShredStream,
    CoreCast,
}

#[derive(Debug, Clone)]
pub struct PreWarmEvent {
    pub mint: [u8; 32],
    pub trader: [u8; 32],
    pub sig: [u8; 64],
    pub sol_amount: u64,
    pub is_buy: bool,
    pub timestamp_ms: u64,
    pub source: FeedSource,
}

pub use event_joiner::EventJoiner;

// ── Layout assertions: keep FeedEvent cache-friendly ──────────────
// TradeEvent dominates FeedEvent size (~256 bytes). Boxing Trade would add
// heap allocation on every trade (the critical hot path) — worse than the
// extra enum padding. Migration.sig [u8;64] does NOT dominate; Trade does.
// If FeedEvent ever grows beyond 264 bytes, investigate boxing cold variants.
#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn feed_event_size_audit() {
        let size = std::mem::size_of::<FeedEvent>();
        // Document current size for regression detection.
        // TradeEvent is ~256 bytes; FeedEvent adds discriminant + alignment.
        assert!(
            size <= 264,
            "FeedEvent grew to {} bytes — audit for cache line pollution",
            size
        );
        // Ensure TradeEvent is the dominant variant (not Migration.sig)
        assert!(
            std::mem::size_of::<TradeEvent>() > std::mem::size_of::<[u8; 64]>(),
            "TradeEvent should be larger than Migration.sig"
        );
    }

    #[test]
    fn trade_event_size_audit() {
        let size = std::mem::size_of::<TradeEvent>();
        // 250 bytes payload + padding
        assert!(
            size <= 264,
            "TradeEvent grew to {} bytes — check for unnecessary field additions",
            size
        );
    }
}
