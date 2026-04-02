pub mod pumpportal;
pub mod helius;
pub mod shredstream;
pub mod corecast;
pub mod event_joiner;
pub mod social;

// ── Feed event types ────────────────────────────────────────────────

/// Source of a graduation/migration detection event.
/// Used by GraduationArbEngine for dedup and latency tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MigrationSource {
    /// Detected via Helius logsSubscribe (primary, ~50ms)
    HeliusLogs = 0,
    /// Detected via CoreCast/Bitquery stream 2 Raydium AMM trades (fallback, ~80ms)
    CoreCastStream2 = 1,
    /// Detected via Jito ShredStream gRPC — FASTEST (~0ms from shred decode)
    ShredStream = 2,
    /// Helius Enhanced transactionSubscribe (full tx with account keys)
    HeliusEnhanced = 3,
    /// PumpPortal subscribeMigration feed
    PumpPortalMigration = 4,
}

impl MigrationSource {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeliusLogs => "helius",
            Self::CoreCastStream2 => "corecast",
            Self::ShredStream => "shredstream",
            Self::HeliusEnhanced => "helius_enhanced",
            Self::PumpPortalMigration => "pumpportal_migration",
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
    /// PumpSwap graduation with pre-extracted pool data from Helius Enhanced WS.
    /// Skips getTransaction — vaults already extracted from transactionNotification.
    PumpSwapGraduationDirect {
        mint: [u8; 32],
        sig: [u8; 64],
        ts_ms: u64,
        coin_vault: [u8; 32],
        pc_vault: [u8; 32],
        source: MigrationSource,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedSource {
    PumpPortal,
    Helius,
    ShredStream,
    CoreCast,
}

impl FeedSource {
    /// Convert to u8 index for evidence weight LUT lookup.
    /// PumpPortal=0, Helius=1, CoreCast=2, ShredStream=3.
    #[inline(always)]
    pub fn as_u8(self) -> u8 {
        match self {
            FeedSource::PumpPortal  => 0,
            FeedSource::Helius      => 1,
            FeedSource::CoreCast    => 2,
            FeedSource::ShredStream => 3,
        }
    }

    /// Convert to usize index (alias for array indexing).
    #[inline(always)]
    pub fn as_index(self) -> usize {
        self.as_u8() as usize
    }
}

// FeedSource must be u8-sized for efficient LUT indexing
const _: () = assert!(core::mem::size_of::<FeedSource>() == 1);

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
