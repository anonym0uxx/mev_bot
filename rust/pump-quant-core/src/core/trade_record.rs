/// TradeRecord: 128 bytes, 2 cache lines.
/// Cache line 1 is touched by every gate check.
/// Cache line 2 is touched by scorer + position manager.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TradeRecord {
    // ── Cache line 1 (64 bytes) ──────────────────────────────────────
    pub timestamp_ms: u64,       // epoch ms via quanta
    pub sol_amount: u64,         // lamports (NOT float)
    pub token_amount: u64,       // raw token units
    pub is_buy: bool,
    pub _pad0: [u8; 7],
    pub trader: [u8; 32],        // decoded pubkey, NOT base58 string

    // ── Cache line 2 (64 bytes) ──────────────────────────────────────
    pub vsol_reserves: u64,      // bonding curve vSol in lamports
    pub vtoken_reserves: u64,    // bonding curve vTokens
    pub market_cap_sol: u64,     // lamports
    pub slot: u64,
    pub sig_prefix: [u8; 8],     // first 8 bytes of tx sig for fast dedup check
    pub _pad1: [u8; 24],
}

// Compile-time size verification — exactly 2 cache lines.
const _: () = assert!(std::mem::size_of::<TradeRecord>() == 128);

impl TradeRecord {
    pub const ZERO: Self = Self {
        timestamp_ms: 0,
        sol_amount: 0,
        token_amount: 0,
        is_buy: false,
        _pad0: [0; 7],
        trader: [0; 32],
        vsol_reserves: 0,
        vtoken_reserves: 0,
        market_cap_sol: 0,
        slot: 0,
        sig_prefix: [0; 8],
        _pad1: [0; 24],
    };
}
