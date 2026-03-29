pub mod pumpportal;
pub mod helius;
pub mod shredstream;
pub mod corecast;
pub mod event_joiner;

// ── Feed event types ────────────────────────────────────────────────

#[derive(Debug)]
pub enum FeedEvent {
    Trade(TradeEvent),
    PreWarm(PreWarmEvent), // Helius-only, no vSol
    CreatorSell { mint: [u8; 32], ts_ms: u64 },
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
