//! PumpSwap Anchor **CPI event** decoders + normalized swap summary.
//!
//! # Responsibility
//! Decode the self-CPI event instructions the PumpSwap program emits on
//! `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` — the canonical per-trade
//! truth for reserves and fees. An event arrives as an *inner instruction*
//! whose data is: the 8-byte Anchor event-ix tag [`ANCHOR_EVENT_IX_TAG`], then
//! an 8-byte event discriminator (`sha256("event:<Name>")[..8]`), then the
//! borsh payload. Payload fields are only ever appended, so decode is
//! length-tolerant: the known prefix decodes by offset and the appended tail
//! surfaces as `Option` fields (`None` on historical short payloads; unknown
//! further bytes — incentive/cashback additions — are ignored).
//!
//! # Reserve semantics
//! `pool_base_token_reserves` / `pool_quote_token_reserves` in Buy/Sell events
//! are **PRE-trade snapshots** of the pool's two token-account balances, taken
//! before the swap moves funds. They are synchronous with the trade — prefer
//! them over separately-fetched vault balances, which race the stream.
//!
//! # Fee semantics
//! The per-trade `*_fee_basis_points` fields are ground truth: pump fees are
//! dynamic and market-cap-tiered since 2025-09-01, routed through
//! [`crate::pumpswap::PUMP_FEES_PROGRAM`]. Never hardcode a schedule.
//!
//! # Constitution
//! * §22 — integer-only; the fixed-point price helpers use
//!   [`PUMPSWAP_PRICE_SCALE`] with `u128` widening, never a float.
//! * §99 — fixed-size structs from borrowed slices; zero allocation.
//! * §102 — tags, discriminators and scales are named constants.
//! * §18.2 — fail closed: wrong tag/discriminator or a truncated required
//!   field yields `None`; every access is bounds-checked, never a panic.

use crate::curve::pumpswap_amount_out;
use crate::pumpswap::{read_bool, read_i64_le, read_pubkey, read_u16_le, read_u64_le};

/// Anchor self-CPI event instruction tag: the first 8 bytes of every event
/// inner-instruction's data (`sha256("anchor:event")[..8]` in little-endian
/// wire order: `e4 45 a5 2e 51 cb 9a 1d`).
pub const ANCHOR_EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// `BuyEvent` discriminator (`sha256("event:BuyEvent")[..8]`).
pub const BUY_EVENT_DISCRIMINATOR: [u8; 8] = [103, 244, 82, 31, 44, 245, 119, 119];

/// `SellEvent` discriminator (`sha256("event:SellEvent")[..8]`).
pub const SELL_EVENT_DISCRIMINATOR: [u8; 8] = [62, 47, 55, 10, 165, 3, 220, 42];

/// `CreatePoolEvent` discriminator (`sha256("event:CreatePoolEvent")[..8]`).
pub const CREATE_POOL_EVENT_DISCRIMINATOR: [u8; 8] = [177, 49, 12, 210, 160, 118, 167, 116];

/// Byte offset of the event payload inside the inner-instruction data
/// (8-byte tag + 8-byte discriminator).
pub const EVENT_PAYLOAD_OFFSET: usize = 16;

/// Return the borsh payload of an event inner-instruction whose tag and
/// discriminator both match; `None` otherwise (fail closed, §18.2).
fn event_payload<'a>(data: &'a [u8], discriminator: &[u8; 8]) -> Option<&'a [u8]> {
    if data.get(0..8)? != ANCHOR_EVENT_IX_TAG {
        return None;
    }
    if data.get(8..16)? != discriminator {
        return None;
    }
    data.get(EVENT_PAYLOAD_OFFSET..)
}

// ---------------------------------------------------------------------------
// BuyEvent.
// ---------------------------------------------------------------------------

/// Decoded PumpSwap `BuyEvent` (payload offsets in the layout table below).
///
/// ```text
/// offset  size  field                                 (payload-relative)
/// 0       8     timestamp                             (i64 LE)
/// 8       8     base_amount_out                       (u64 LE)
/// 16      8     max_quote_amount_in                   (u64 LE)
/// 24      8     user_base_token_reserves              (u64 LE)
/// 32      8     user_quote_token_reserves             (u64 LE)
/// 40      8     pool_base_token_reserves              (u64 LE)  PRE-trade
/// 48      8     pool_quote_token_reserves             (u64 LE)  PRE-trade
/// 56      8     quote_amount_in                       (u64 LE)
/// 64      8     lp_fee_basis_points                   (u64 LE)
/// 72      8     lp_fee                                (u64 LE)
/// 80      8     protocol_fee_basis_points             (u64 LE)
/// 88      8     protocol_fee                          (u64 LE)
/// 96      8     quote_amount_in_with_lp_fee           (u64 LE)
/// 104     8     user_quote_amount_in                  (u64 LE)
/// 112     32    pool                                  (Pubkey)
/// 144     32    user                                  (Pubkey)
/// 176     32    user_base_token_account               (Pubkey)
/// 208     32    user_quote_token_account              (Pubkey)
/// 240     32    protocol_fee_recipient                (Pubkey)
/// 272     32    protocol_fee_recipient_token_account  (Pubkey)
/// 304     32    coin_creator                          (Pubkey)
/// ---- optional appended tail ----
/// 336     8     coin_creator_fee_basis_points         (u64 LE)
/// 344     8     coin_creator_fee                      (u64 LE)
/// 352     1     track_volume                          (bool)
/// (further incentive/cashback tail bytes are ignored)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyEvent {
    /// Chain unix timestamp (seconds) at execution.
    pub timestamp: i64,
    /// Exact base tokens bought, base units.
    pub base_amount_out: u64,
    /// Trader's quote slippage cap.
    pub max_quote_amount_in: u64,
    /// User base-ATA balance before the trade.
    pub user_base_token_reserves: u64,
    /// User quote-ATA balance before the trade.
    pub user_quote_token_reserves: u64,
    /// Pool base vault balance, PRE-trade snapshot.
    pub pool_base_token_reserves: u64,
    /// Pool quote vault balance, PRE-trade snapshot.
    pub pool_quote_token_reserves: u64,
    /// Constant-product quote input (before fees are stacked on top).
    pub quote_amount_in: u64,
    /// LP fee rate actually applied, basis points (ground truth per trade).
    pub lp_fee_basis_points: u64,
    /// LP fee paid, quote units.
    pub lp_fee: u64,
    /// Protocol fee rate actually applied, basis points (ground truth).
    pub protocol_fee_basis_points: u64,
    /// Protocol fee paid, quote units.
    pub protocol_fee: u64,
    /// `quote_amount_in + lp_fee`.
    pub quote_amount_in_with_lp_fee: u64,
    /// Total quote the user paid (all fees included).
    pub user_quote_amount_in: u64,
    /// Pool account.
    pub pool: [u8; 32],
    /// Trader.
    pub user: [u8; 32],
    /// Trader base ATA.
    pub user_base_token_account: [u8; 32],
    /// Trader quote ATA.
    pub user_quote_token_account: [u8; 32],
    /// Protocol fee recipient.
    pub protocol_fee_recipient: [u8; 32],
    /// Protocol fee recipient's token account.
    pub protocol_fee_recipient_token_account: [u8; 32],
    /// Coin creator credited with the creator fee.
    pub coin_creator: [u8; 32],
    /// Creator fee rate, basis points (appended tail; `None` historically).
    pub coin_creator_fee_basis_points: Option<u64>,
    /// Creator fee paid, quote units (appended tail).
    pub coin_creator_fee: Option<u64>,
    /// Volume-tracking flag (appended tail).
    pub track_volume: Option<bool>,
}

/// End of the pre-creator-fee `BuyEvent`/`SellEvent` payload (historical
/// minimum length: fixed fields end after `coin_creator` at byte 336).
pub const SWAP_EVENT_FIXED_LEN: usize = 336;

/// Decode a `BuyEvent` inner-instruction (`data` = tag ++ discriminator ++
/// payload). Length-tolerant per the module docs; `None` on wrong identity or
/// a payload shorter than [`SWAP_EVENT_FIXED_LEN`].
pub fn decode_buy_event(data: &[u8]) -> Option<BuyEvent> {
    let p = event_payload(data, &BUY_EVENT_DISCRIMINATOR)?;
    if p.len() < SWAP_EVENT_FIXED_LEN {
        return None;
    }
    // Sequential optional tail: stops at the first absent/malformed field.
    let coin_creator_fee_basis_points = read_u64_le(p, 336);
    let coin_creator_fee = if coin_creator_fee_basis_points.is_some() {
        read_u64_le(p, 344)
    } else {
        None
    };
    let track_volume = if coin_creator_fee.is_some() {
        read_bool(p, 352)
    } else {
        None
    };
    Some(BuyEvent {
        timestamp: read_i64_le(p, 0)?,
        base_amount_out: read_u64_le(p, 8)?,
        max_quote_amount_in: read_u64_le(p, 16)?,
        user_base_token_reserves: read_u64_le(p, 24)?,
        user_quote_token_reserves: read_u64_le(p, 32)?,
        pool_base_token_reserves: read_u64_le(p, 40)?,
        pool_quote_token_reserves: read_u64_le(p, 48)?,
        quote_amount_in: read_u64_le(p, 56)?,
        lp_fee_basis_points: read_u64_le(p, 64)?,
        lp_fee: read_u64_le(p, 72)?,
        protocol_fee_basis_points: read_u64_le(p, 80)?,
        protocol_fee: read_u64_le(p, 88)?,
        quote_amount_in_with_lp_fee: read_u64_le(p, 96)?,
        user_quote_amount_in: read_u64_le(p, 104)?,
        pool: read_pubkey(p, 112)?,
        user: read_pubkey(p, 144)?,
        user_base_token_account: read_pubkey(p, 176)?,
        user_quote_token_account: read_pubkey(p, 208)?,
        protocol_fee_recipient: read_pubkey(p, 240)?,
        protocol_fee_recipient_token_account: read_pubkey(p, 272)?,
        coin_creator: read_pubkey(p, 304)?,
        coin_creator_fee_basis_points,
        coin_creator_fee,
        track_volume,
    })
}

// ---------------------------------------------------------------------------
// SellEvent.
// ---------------------------------------------------------------------------

/// Decoded PumpSwap `SellEvent`.
///
/// Same shape as [`BuyEvent`] with the quote direction reversed: the u64 run
/// at payload offsets 8..112 is `base_amount_in`, `min_quote_amount_out`,
/// `user_base_token_reserves`, `user_quote_token_reserves`,
/// `pool_base_token_reserves`, `pool_quote_token_reserves`,
/// `quote_amount_out`, `lp_fee_basis_points`, `lp_fee`,
/// `protocol_fee_basis_points`, `protocol_fee`,
/// `quote_amount_out_without_lp_fee`, `user_quote_amount_out`; then the same
/// seven pubkeys at 112..336; then the optional appended creator-fee tail
/// (`coin_creator_fee_basis_points` @336, `coin_creator_fee` @344; further
/// cashback tail bytes are ignored). Pool reserves are PRE-trade snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellEvent {
    /// Chain unix timestamp (seconds) at execution.
    pub timestamp: i64,
    /// Exact base tokens sold, base units.
    pub base_amount_in: u64,
    /// Trader's minimum-quote slippage guard.
    pub min_quote_amount_out: u64,
    /// User base-ATA balance before the trade.
    pub user_base_token_reserves: u64,
    /// User quote-ATA balance before the trade.
    pub user_quote_token_reserves: u64,
    /// Pool base vault balance, PRE-trade snapshot.
    pub pool_base_token_reserves: u64,
    /// Pool quote vault balance, PRE-trade snapshot.
    pub pool_quote_token_reserves: u64,
    /// Constant-product quote output (before fees are deducted).
    pub quote_amount_out: u64,
    /// LP fee rate actually applied, basis points (ground truth per trade).
    pub lp_fee_basis_points: u64,
    /// LP fee deducted, quote units.
    pub lp_fee: u64,
    /// Protocol fee rate actually applied, basis points (ground truth).
    pub protocol_fee_basis_points: u64,
    /// Protocol fee deducted, quote units.
    pub protocol_fee: u64,
    /// `quote_amount_out - lp_fee`.
    pub quote_amount_out_without_lp_fee: u64,
    /// Net quote the user received (all fees deducted).
    pub user_quote_amount_out: u64,
    /// Pool account.
    pub pool: [u8; 32],
    /// Trader.
    pub user: [u8; 32],
    /// Trader base ATA.
    pub user_base_token_account: [u8; 32],
    /// Trader quote ATA.
    pub user_quote_token_account: [u8; 32],
    /// Protocol fee recipient.
    pub protocol_fee_recipient: [u8; 32],
    /// Protocol fee recipient's token account.
    pub protocol_fee_recipient_token_account: [u8; 32],
    /// Coin creator credited with the creator fee.
    pub coin_creator: [u8; 32],
    /// Creator fee rate, basis points (appended tail; `None` historically).
    pub coin_creator_fee_basis_points: Option<u64>,
    /// Creator fee deducted, quote units (appended tail).
    pub coin_creator_fee: Option<u64>,
}

/// Decode a `SellEvent` inner-instruction (`data` = tag ++ discriminator ++
/// payload). Length-tolerant; `None` on wrong identity or a payload shorter
/// than [`SWAP_EVENT_FIXED_LEN`].
pub fn decode_sell_event(data: &[u8]) -> Option<SellEvent> {
    let p = event_payload(data, &SELL_EVENT_DISCRIMINATOR)?;
    if p.len() < SWAP_EVENT_FIXED_LEN {
        return None;
    }
    let coin_creator_fee_basis_points = read_u64_le(p, 336);
    let coin_creator_fee = if coin_creator_fee_basis_points.is_some() {
        read_u64_le(p, 344)
    } else {
        None
    };
    Some(SellEvent {
        timestamp: read_i64_le(p, 0)?,
        base_amount_in: read_u64_le(p, 8)?,
        min_quote_amount_out: read_u64_le(p, 16)?,
        user_base_token_reserves: read_u64_le(p, 24)?,
        user_quote_token_reserves: read_u64_le(p, 32)?,
        pool_base_token_reserves: read_u64_le(p, 40)?,
        pool_quote_token_reserves: read_u64_le(p, 48)?,
        quote_amount_out: read_u64_le(p, 56)?,
        lp_fee_basis_points: read_u64_le(p, 64)?,
        lp_fee: read_u64_le(p, 72)?,
        protocol_fee_basis_points: read_u64_le(p, 80)?,
        protocol_fee: read_u64_le(p, 88)?,
        quote_amount_out_without_lp_fee: read_u64_le(p, 96)?,
        user_quote_amount_out: read_u64_le(p, 104)?,
        pool: read_pubkey(p, 112)?,
        user: read_pubkey(p, 144)?,
        user_base_token_account: read_pubkey(p, 176)?,
        user_quote_token_account: read_pubkey(p, 208)?,
        protocol_fee_recipient: read_pubkey(p, 240)?,
        protocol_fee_recipient_token_account: read_pubkey(p, 272)?,
        coin_creator: read_pubkey(p, 304)?,
        coin_creator_fee_basis_points,
        coin_creator_fee,
    })
}

// ---------------------------------------------------------------------------
// CreatePoolEvent.
// ---------------------------------------------------------------------------

/// Decoded PumpSwap `CreatePoolEvent`.
///
/// ```text
/// offset  size  field                     (payload-relative)
/// 0       8     timestamp                 (i64 LE)
/// 8       2     index                     (u16 LE)
/// 10      32    creator                   (Pubkey)
/// 42      32    base_mint                 (Pubkey)
/// 74      32    quote_mint                (Pubkey)
/// 106     1     base_mint_decimals        (u8)
/// 107     1     quote_mint_decimals       (u8)
/// 108     8     base_amount_in            (u64 LE)
/// 116     8     quote_amount_in           (u64 LE)
/// 124     8     pool_base_amount          (u64 LE)
/// 132     8     pool_quote_amount         (u64 LE)
/// 140     8     minimum_liquidity         (u64 LE)
/// 148     8     initial_liquidity         (u64 LE)
/// 156     8     lp_token_amount_out       (u64 LE)
/// 164     1     pool_bump                 (u8)
/// 165     32    pool                      (Pubkey)
/// 197     32    lp_mint                   (Pubkey)
/// 229     32    user_base_token_account   (Pubkey)
/// 261     32    user_quote_token_account  (Pubkey)
/// ---- optional appended tail ----
/// 293     32    coin_creator              (Pubkey)
/// 325     1     is_mayhem_mode            (bool)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatePoolEvent {
    /// Chain unix timestamp (seconds) at creation.
    pub timestamp: i64,
    /// Pool index within the creator's namespace.
    pub index: u16,
    /// Pool creator.
    pub creator: [u8; 32],
    /// Base-token mint.
    pub base_mint: [u8; 32],
    /// Quote-token mint (key off this; USDC-quoted pools exist since 2026-05).
    pub quote_mint: [u8; 32],
    /// Base mint decimals.
    pub base_mint_decimals: u8,
    /// Quote mint decimals.
    pub quote_mint_decimals: u8,
    /// Base deposited by the creator.
    pub base_amount_in: u64,
    /// Quote deposited by the creator.
    pub quote_amount_in: u64,
    /// Pool base vault balance after creation.
    pub pool_base_amount: u64,
    /// Pool quote vault balance after creation.
    pub pool_quote_amount: u64,
    /// Minimum liquidity locked forever.
    pub minimum_liquidity: u64,
    /// Initial liquidity minted.
    pub initial_liquidity: u64,
    /// LP tokens issued to the creator.
    pub lp_token_amount_out: u64,
    /// Pool PDA bump.
    pub pool_bump: u8,
    /// Pool account.
    pub pool: [u8; 32],
    /// LP mint.
    pub lp_mint: [u8; 32],
    /// Creator's base ATA.
    pub user_base_token_account: [u8; 32],
    /// Creator's quote ATA.
    pub user_quote_token_account: [u8; 32],
    /// Coin creator (appended tail; `None` historically).
    pub coin_creator: Option<[u8; 32]>,
    /// Mayhem-mode flag (appended tail).
    pub is_mayhem_mode: Option<bool>,
}

/// End of the pre-`coin_creator` `CreatePoolEvent` payload (historical
/// minimum: fixed fields end after `user_quote_token_account` at byte 293).
pub const CREATE_POOL_EVENT_FIXED_LEN: usize = 293;

/// Decode a `CreatePoolEvent` inner-instruction (`data` = tag ++
/// discriminator ++ payload). Length-tolerant; `None` on wrong identity or a
/// payload shorter than [`CREATE_POOL_EVENT_FIXED_LEN`].
pub fn decode_create_pool_event(data: &[u8]) -> Option<CreatePoolEvent> {
    let p = event_payload(data, &CREATE_POOL_EVENT_DISCRIMINATOR)?;
    if p.len() < CREATE_POOL_EVENT_FIXED_LEN {
        return None;
    }
    let coin_creator = read_pubkey(p, 293);
    let is_mayhem_mode = if coin_creator.is_some() {
        read_bool(p, 325)
    } else {
        None
    };
    Some(CreatePoolEvent {
        timestamp: read_i64_le(p, 0)?,
        index: read_u16_le(p, 8)?,
        creator: read_pubkey(p, 10)?,
        base_mint: read_pubkey(p, 42)?,
        quote_mint: read_pubkey(p, 74)?,
        base_mint_decimals: *p.get(106)?,
        quote_mint_decimals: *p.get(107)?,
        base_amount_in: read_u64_le(p, 108)?,
        quote_amount_in: read_u64_le(p, 116)?,
        pool_base_amount: read_u64_le(p, 124)?,
        pool_quote_amount: read_u64_le(p, 132)?,
        minimum_liquidity: read_u64_le(p, 140)?,
        initial_liquidity: read_u64_le(p, 148)?,
        lp_token_amount_out: read_u64_le(p, 156)?,
        pool_bump: *p.get(164)?,
        pool: read_pubkey(p, 165)?,
        lp_mint: read_pubkey(p, 197)?,
        user_base_token_account: read_pubkey(p, 229)?,
        user_quote_token_account: read_pubkey(p, 261)?,
        coin_creator,
        is_mayhem_mode,
    })
}

/// Any decoded PumpSwap CPI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpSwapEvent {
    /// A buy trade.
    Buy(BuyEvent),
    /// A sell trade.
    Sell(SellEvent),
    /// A pool creation (incl. graduation).
    CreatePool(CreatePoolEvent),
}

/// Decode any PumpSwap CPI event by dispatching on its discriminator.
/// Unknown discriminators (other event kinds this crate does not model)
/// return `None` — never guessed at.
pub fn decode_pumpswap_event(data: &[u8]) -> Option<PumpSwapEvent> {
    let disc: [u8; 8] = data.get(8..16)?.try_into().ok()?;
    match disc {
        BUY_EVENT_DISCRIMINATOR => decode_buy_event(data).map(PumpSwapEvent::Buy),
        SELL_EVENT_DISCRIMINATOR => decode_sell_event(data).map(PumpSwapEvent::Sell),
        CREATE_POOL_EVENT_DISCRIMINATOR => {
            decode_create_pool_event(data).map(PumpSwapEvent::CreatePool)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Normalized swap summary (Phase-B one-liner wiring surface).
// ---------------------------------------------------------------------------

/// Fixed-point price scale for the quote-per-base helpers: 1e9, mirroring
/// `pump_quant_features::types::PRICE_SCALE` (this crate is dependency-free
/// by design, so the value is pinned here and asserted equal in the features
/// crate's docs rather than imported).
pub const PUMPSWAP_PRICE_SCALE: u128 = 1_000_000_000;

/// Trade direction of a normalized PumpSwap swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpSwapSide {
    /// Quote in, base out.
    Buy,
    /// Base in, quote out.
    Sell,
}

/// Normalized per-trade summary extracted from a Buy/Sell CPI event — the
/// single struct Phase-B stream wiring consumes.
///
/// * `pool_*_reserves_pre` are the event's PRE-trade vault snapshots (see the
///   module docs) — post-trade reserves are `pre ± amount` per side.
/// * `quote_amount_gross` is the constant-product leg
///   (`quote_amount_in`/`quote_amount_out`); `quote_amount_user` is what the
///   trader actually paid/received with all fees applied.
/// * `base_mint` is `None` because Buy/Sell events do not carry the mint —
///   join against [`crate::pumpswap::PoolAccount::base_mint`] (or a
///   `CreatePoolEvent`) keyed by `pool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpSwapTrade {
    /// Pool account the trade executed on.
    pub pool: [u8; 32],
    /// Base mint, when known (`None` straight from a Buy/Sell event).
    pub base_mint: Option<[u8; 32]>,
    /// Trader.
    pub user: [u8; 32],
    /// Trade direction.
    pub side: PumpSwapSide,
    /// Base tokens moved, base units.
    pub base_amount: u64,
    /// Constant-product quote leg (pre-fee), quote units.
    pub quote_amount_gross: u64,
    /// Quote the user actually paid (buy) / received (sell), fees applied.
    pub quote_amount_user: u64,
    /// Pool base vault balance, PRE-trade.
    pub pool_base_reserves_pre: u64,
    /// Pool quote vault balance, PRE-trade.
    pub pool_quote_reserves_pre: u64,
    /// LP fee, quote units.
    pub lp_fee: u64,
    /// Protocol fee, quote units.
    pub protocol_fee: u64,
    /// Coin-creator fee, quote units (0 when the event predates the field).
    pub coin_creator_fee: u64,
    /// Chain unix timestamp (seconds).
    pub timestamp: i64,
}

impl PumpSwapTrade {
    /// Pool marginal price PRE-trade, quote-per-base, scaled by
    /// [`PUMPSWAP_PRICE_SCALE`]: `quote_reserves_pre * SCALE /
    /// base_reserves_pre`. `None` on an empty base side (§22: checked, no
    /// float, no panic).
    pub fn pool_price_fp(&self) -> Option<u128> {
        let num = (self.pool_quote_reserves_pre as u128).checked_mul(PUMPSWAP_PRICE_SCALE)?;
        num.checked_div(self.pool_base_reserves_pre as u128)
    }

    /// Realized execution price, quote-per-base, scaled by
    /// [`PUMPSWAP_PRICE_SCALE`]: `quote_amount_gross * SCALE / base_amount`.
    /// `None` on a zero base amount.
    pub fn exec_price_fp(&self) -> Option<u128> {
        let num = (self.quote_amount_gross as u128).checked_mul(PUMPSWAP_PRICE_SCALE)?;
        num.checked_div(self.base_amount as u128)
    }

    /// Raw integer price ratio `(quote_reserves_pre, base_reserves_pre)` for
    /// callers that keep exact rational arithmetic instead of fixed point.
    pub const fn pool_price_ratio(&self) -> (u64, u64) {
        (self.pool_quote_reserves_pre, self.pool_base_reserves_pre)
    }
}

/// Normalize a [`BuyEvent`] into a [`PumpSwapTrade`].
pub fn trade_from_buy_event(ev: &BuyEvent) -> PumpSwapTrade {
    PumpSwapTrade {
        pool: ev.pool,
        base_mint: None,
        user: ev.user,
        side: PumpSwapSide::Buy,
        base_amount: ev.base_amount_out,
        quote_amount_gross: ev.quote_amount_in,
        quote_amount_user: ev.user_quote_amount_in,
        pool_base_reserves_pre: ev.pool_base_token_reserves,
        pool_quote_reserves_pre: ev.pool_quote_token_reserves,
        lp_fee: ev.lp_fee,
        protocol_fee: ev.protocol_fee,
        coin_creator_fee: ev.coin_creator_fee.unwrap_or(0),
        timestamp: ev.timestamp,
    }
}

/// Normalize a [`SellEvent`] into a [`PumpSwapTrade`].
pub fn trade_from_sell_event(ev: &SellEvent) -> PumpSwapTrade {
    PumpSwapTrade {
        pool: ev.pool,
        base_mint: None,
        user: ev.user,
        side: PumpSwapSide::Sell,
        base_amount: ev.base_amount_in,
        quote_amount_gross: ev.quote_amount_out,
        quote_amount_user: ev.user_quote_amount_out,
        pool_base_reserves_pre: ev.pool_base_token_reserves,
        pool_quote_reserves_pre: ev.pool_quote_token_reserves,
        lp_fee: ev.lp_fee,
        protocol_fee: ev.protocol_fee,
        coin_creator_fee: ev.coin_creator_fee.unwrap_or(0),
        timestamp: ev.timestamp,
    }
}

// ---------------------------------------------------------------------------
// Fee/CP cross-check.
// ---------------------------------------------------------------------------

/// Cross-check a [`BuyEvent`] against the constant-product invariant.
///
/// The program computes an exact-out buy as `quote_amount_in =
/// ceil(quote_reserves * base_out / (base_reserves - base_out))`. This check
/// runs the *forward* CP map ([`pumpswap_amount_out`] with `fee_bps = 0`,
/// fees are stacked outside the CP leg on buys) on the event's PRE-trade
/// reserve snapshots and `quote_amount_in`, and requires the implied base out
/// to match `base_amount_out` within integer-rounding tolerance. One quote
/// lamport of ceil-rounding moves the implied base by up to
/// `base_reserves / quote_reserves` base units, so the tolerance is that
/// ratio plus a two-unit floor-rounding guard. Additionally the event's own
/// arithmetic identity `quote_amount_in_with_lp_fee = quote_amount_in +
/// lp_fee` must hold exactly.
///
/// Returns `false` for perturbed/forged reserves or amounts. Used by tests
/// and as a live stream cross-check before an event is trusted.
///
/// # Constitution
/// §22 — pure `u128` integer math; deterministic; no panic on any input.
pub fn verify_buy_event(ev: &BuyEvent) -> bool {
    // Internal identity: with-lp-fee amount is exactly the CP leg plus lp fee.
    let with_lp = (ev.quote_amount_in as u128) + (ev.lp_fee as u128);
    if with_lp != ev.quote_amount_in_with_lp_fee as u128 {
        return false;
    }

    let base_res = ev.pool_base_token_reserves as u128;
    let quote_res = ev.pool_quote_token_reserves as u128;
    if quote_res == 0 {
        // No CP price exists; only a degenerate zero-trade is consistent.
        return ev.quote_amount_in == 0 && ev.base_amount_out == 0;
    }
    let implied_base_out = match pumpswap_amount_out(
        quote_res,
        base_res,
        ev.quote_amount_in as u128,
        0, // fees live outside the CP leg on buys
    ) {
        Some(v) => v,
        None => return false,
    };

    // Rounding tolerance: one ceil'd quote lamport ≈ base_res/quote_res base
    // units, plus 2 for the floor on each side of the round trip.
    let tolerance = base_res / quote_res + 2;
    implied_base_out.abs_diff(ev.base_amount_out as u128) <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    /// Wrap a payload in tag + discriminator.
    fn event_data(disc: [u8; 8], payload: &[u8]) -> Vec<u8> {
        let mut d = ANCHOR_EVENT_IX_TAG.to_vec();
        d.extend_from_slice(&disc);
        d.extend_from_slice(payload);
        d
    }

    /// The reference BuyEvent fixture: internally-consistent CP math.
    /// Reserves 1e12 base / 5e10 quote, exact-out 1e10 base.
    struct BuyFix {
        base_res: u64,
        quote_res: u64,
        base_out: u64,
        quote_in: u64,
        lp_fee: u64,
        protocol_fee: u64,
        creator_fee: u64,
    }

    fn buy_fix() -> BuyFix {
        let base_res: u64 = 1_000_000_000_000;
        let quote_res: u64 = 50_000_000_000;
        let base_out: u64 = 10_000_000_000;
        // quote_in = ceil(quote_res * base_out / (base_res - base_out))
        let num = quote_res as u128 * base_out as u128;
        let den = (base_res - base_out) as u128;
        let quote_in = num.div_ceil(den) as u64;
        let ceil_bps = |amt: u64, bps: u64| ((amt as u128 * bps as u128).div_ceil(10_000)) as u64;
        BuyFix {
            base_res,
            quote_res,
            base_out,
            quote_in,
            lp_fee: ceil_bps(quote_in, 20),
            protocol_fee: ceil_bps(quote_in, 5),
            creator_fee: ceil_bps(quote_in, 5),
        }
    }

    /// Serialize the fixture into a payload of `len` bytes (>= 336 meaningful).
    fn buy_payload(len: usize) -> Vec<u8> {
        let f = buy_fix();
        let mut p = vec![0u8; len];
        let mut put_u64 = |off: usize, v: u64| {
            if len >= off + 8 {
                p[off..off + 8].copy_from_slice(&v.to_le_bytes());
            }
        };
        put_u64(0, 1_753_000_000u64); // timestamp as u64 bits (positive i64)
        put_u64(8, f.base_out);
        put_u64(16, f.quote_in * 2); // max_quote_amount_in (slippage cap)
        put_u64(24, 111);
        put_u64(32, 222);
        put_u64(40, f.base_res);
        put_u64(48, f.quote_res);
        put_u64(56, f.quote_in);
        put_u64(64, 20); // lp bps
        put_u64(72, f.lp_fee);
        put_u64(80, 5); // protocol bps
        put_u64(88, f.protocol_fee);
        put_u64(96, f.quote_in + f.lp_fee);
        put_u64(104, f.quote_in + f.lp_fee + f.protocol_fee + f.creator_fee);
        let mut put_pk = |off: usize, tag: u8| {
            if len >= off + 32 {
                p[off..off + 32].copy_from_slice(&pk(tag));
            }
        };
        put_pk(112, 0xA0); // pool
        put_pk(144, 0xA1); // user
        put_pk(176, 0xA2);
        put_pk(208, 0xA3);
        put_pk(240, 0xA4);
        put_pk(272, 0xA5);
        put_pk(304, 0xA6); // coin_creator
        if len >= 344 {
            p[336..344].copy_from_slice(&5u64.to_le_bytes());
        }
        if len >= 352 {
            p[344..352].copy_from_slice(&buy_fix().creator_fee.to_le_bytes());
        }
        if len >= 353 {
            p[352] = 1;
        }
        p
    }

    #[test]
    fn buy_event_roundtrip_full_length_every_field() {
        let f = buy_fix();
        let data = event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353));
        let e = decode_buy_event(&data).expect("decodes");
        assert_eq!(e.timestamp, 1_753_000_000);
        assert_eq!(e.base_amount_out, f.base_out);
        assert_eq!(e.max_quote_amount_in, f.quote_in * 2);
        assert_eq!(e.user_base_token_reserves, 111);
        assert_eq!(e.user_quote_token_reserves, 222);
        assert_eq!(e.pool_base_token_reserves, f.base_res);
        assert_eq!(e.pool_quote_token_reserves, f.quote_res);
        assert_eq!(e.quote_amount_in, f.quote_in);
        assert_eq!(e.lp_fee_basis_points, 20);
        assert_eq!(e.lp_fee, f.lp_fee);
        assert_eq!(e.protocol_fee_basis_points, 5);
        assert_eq!(e.protocol_fee, f.protocol_fee);
        assert_eq!(e.quote_amount_in_with_lp_fee, f.quote_in + f.lp_fee);
        assert_eq!(
            e.user_quote_amount_in,
            f.quote_in + f.lp_fee + f.protocol_fee + f.creator_fee
        );
        assert_eq!(e.pool, pk(0xA0));
        assert_eq!(e.user, pk(0xA1));
        assert_eq!(e.user_base_token_account, pk(0xA2));
        assert_eq!(e.user_quote_token_account, pk(0xA3));
        assert_eq!(e.protocol_fee_recipient, pk(0xA4));
        assert_eq!(e.protocol_fee_recipient_token_account, pk(0xA5));
        assert_eq!(e.coin_creator, pk(0xA6));
        assert_eq!(e.coin_creator_fee_basis_points, Some(5));
        assert_eq!(e.coin_creator_fee, Some(f.creator_fee));
        assert_eq!(e.track_volume, Some(true));
    }

    #[test]
    fn buy_event_historical_short_payload_tail_is_none() {
        // Pre-creator-fee payload ends exactly at coin_creator (336 bytes).
        let data = event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(SWAP_EVENT_FIXED_LEN));
        let e = decode_buy_event(&data).expect("decodes");
        assert_eq!(e.coin_creator, pk(0xA6));
        assert_eq!(e.coin_creator_fee_basis_points, None);
        assert_eq!(e.coin_creator_fee, None);
        assert_eq!(e.track_volume, None);
        // Partial tail: bps + fee present, track_volume absent.
        let data = event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(352));
        let e = decode_buy_event(&data).expect("decodes");
        assert_eq!(e.coin_creator_fee_basis_points, Some(5));
        assert!(e.coin_creator_fee.is_some());
        assert_eq!(e.track_volume, None);
    }

    #[test]
    fn buy_event_oversized_unknown_tail_ignored() {
        // Future incentive/cashback appends: 80 unknown bytes after track_volume.
        let mut p = buy_payload(353);
        p.extend_from_slice(&[0xEE; 80]);
        let e = decode_buy_event(&event_data(BUY_EVENT_DISCRIMINATOR, &p)).expect("decodes");
        assert_eq!(e.track_volume, Some(true));
        assert_eq!(e.base_amount_out, buy_fix().base_out);
    }

    #[test]
    fn buy_event_rejects_wrong_identity() {
        let p = buy_payload(353);
        // Wrong event-ix tag.
        let mut d = event_data(BUY_EVENT_DISCRIMINATOR, &p);
        d[0] ^= 0x01;
        assert!(decode_buy_event(&d).is_none());
        // Wrong discriminator (a SellEvent is not a BuyEvent).
        let d = event_data(SELL_EVENT_DISCRIMINATOR, &p);
        assert!(decode_buy_event(&d).is_none());
    }

    #[test]
    fn buy_event_rejects_truncated_payload() {
        let d = event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353));
        for len in [0, 8, 15, 16, 100, 16 + 335] {
            assert!(decode_buy_event(&d[..len]).is_none(), "len {len}");
        }
    }

    /// SellEvent payload sharing the buy fixture's reserves.
    fn sell_payload(len: usize) -> Vec<u8> {
        let mut p = buy_payload(if len >= 352 { 352 } else { len });
        p.truncate(len);
        p
    }

    #[test]
    fn sell_event_roundtrip_and_short_tail() {
        let data = event_data(SELL_EVENT_DISCRIMINATOR, &sell_payload(352));
        let e = decode_sell_event(&data).expect("decodes");
        let f = buy_fix();
        assert_eq!(e.base_amount_in, f.base_out);
        assert_eq!(e.min_quote_amount_out, f.quote_in * 2);
        assert_eq!(e.pool_base_token_reserves, f.base_res);
        assert_eq!(e.pool_quote_token_reserves, f.quote_res);
        assert_eq!(e.quote_amount_out, f.quote_in);
        assert_eq!(e.lp_fee, f.lp_fee);
        assert_eq!(e.quote_amount_out_without_lp_fee, f.quote_in + f.lp_fee);
        assert_eq!(
            e.user_quote_amount_out,
            f.quote_in + f.lp_fee + f.protocol_fee + f.creator_fee
        );
        assert_eq!(e.pool, pk(0xA0));
        assert_eq!(e.coin_creator, pk(0xA6));
        assert_eq!(e.coin_creator_fee_basis_points, Some(5));
        assert_eq!(e.coin_creator_fee, Some(f.creator_fee));
        // Historical short payload.
        let data = event_data(
            SELL_EVENT_DISCRIMINATOR,
            &sell_payload(SWAP_EVENT_FIXED_LEN),
        );
        let e = decode_sell_event(&data).expect("decodes");
        assert_eq!(e.coin_creator_fee_basis_points, None);
        assert_eq!(e.coin_creator_fee, None);
        // Truncated / wrong identity rejected.
        let d = event_data(SELL_EVENT_DISCRIMINATOR, &sell_payload(352));
        assert!(decode_sell_event(&d[..200]).is_none());
        assert!(
            decode_sell_event(&event_data(BUY_EVENT_DISCRIMINATOR, &sell_payload(352))).is_none()
        );
    }

    /// CreatePoolEvent payload of `len` bytes.
    fn create_pool_payload(len: usize) -> Vec<u8> {
        let mut p = vec![0u8; len];
        p[0..8].copy_from_slice(&1_753_000_001u64.to_le_bytes());
        p[8..10].copy_from_slice(&4u16.to_le_bytes());
        p[10..42].copy_from_slice(&pk(0xB0));
        p[42..74].copy_from_slice(&pk(0xB1));
        p[74..106].copy_from_slice(&pk(0xB2));
        p[106] = 6;
        p[107] = 9;
        let vals: [u64; 7] = [10, 20, 30, 40, 50, 60, 70];
        for (i, v) in vals.iter().enumerate() {
            let o = 108 + 8 * i;
            p[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
        p[164] = 251;
        p[165..197].copy_from_slice(&pk(0xB3));
        p[197..229].copy_from_slice(&pk(0xB4));
        p[229..261].copy_from_slice(&pk(0xB5));
        p[261..293].copy_from_slice(&pk(0xB6));
        if len >= 325 {
            p[293..325].copy_from_slice(&pk(0xB7));
        }
        if len >= 326 {
            p[325] = 1;
        }
        p
    }

    #[test]
    fn create_pool_event_roundtrip_full() {
        let data = event_data(CREATE_POOL_EVENT_DISCRIMINATOR, &create_pool_payload(326));
        let e = decode_create_pool_event(&data).expect("decodes");
        assert_eq!(e.timestamp, 1_753_000_001);
        assert_eq!(e.index, 4);
        assert_eq!(e.creator, pk(0xB0));
        assert_eq!(e.base_mint, pk(0xB1));
        assert_eq!(e.quote_mint, pk(0xB2));
        assert_eq!(e.base_mint_decimals, 6);
        assert_eq!(e.quote_mint_decimals, 9);
        assert_eq!(e.base_amount_in, 10);
        assert_eq!(e.quote_amount_in, 20);
        assert_eq!(e.pool_base_amount, 30);
        assert_eq!(e.pool_quote_amount, 40);
        assert_eq!(e.minimum_liquidity, 50);
        assert_eq!(e.initial_liquidity, 60);
        assert_eq!(e.lp_token_amount_out, 70);
        assert_eq!(e.pool_bump, 251);
        assert_eq!(e.pool, pk(0xB3));
        assert_eq!(e.lp_mint, pk(0xB4));
        assert_eq!(e.user_base_token_account, pk(0xB5));
        assert_eq!(e.user_quote_token_account, pk(0xB6));
        assert_eq!(e.coin_creator, Some(pk(0xB7)));
        assert_eq!(e.is_mayhem_mode, Some(true));
    }

    #[test]
    fn create_pool_event_historical_and_adversarial() {
        // Historical 293-byte payload: tail absent.
        let data = event_data(
            CREATE_POOL_EVENT_DISCRIMINATOR,
            &create_pool_payload(CREATE_POOL_EVENT_FIXED_LEN),
        );
        let e = decode_create_pool_event(&data).expect("decodes");
        assert_eq!(e.coin_creator, None);
        assert_eq!(e.is_mayhem_mode, None);
        // coin_creator present, mayhem byte absent.
        let data = event_data(CREATE_POOL_EVENT_DISCRIMINATOR, &create_pool_payload(325));
        let e = decode_create_pool_event(&data).expect("decodes");
        assert_eq!(e.coin_creator, Some(pk(0xB7)));
        assert_eq!(e.is_mayhem_mode, None);
        // Truncated mid-field and wrong discriminator rejected.
        let good = event_data(CREATE_POOL_EVENT_DISCRIMINATOR, &create_pool_payload(326));
        for len in [0, 16, 120, 16 + 292] {
            assert!(
                decode_create_pool_event(&good[..len]).is_none(),
                "len {len}"
            );
        }
        assert!(decode_create_pool_event(&event_data(
            BUY_EVENT_DISCRIMINATOR,
            &create_pool_payload(326)
        ))
        .is_none());
    }

    #[test]
    fn dispatch_routes_all_event_kinds() {
        let b = event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353));
        assert!(matches!(
            decode_pumpswap_event(&b),
            Some(PumpSwapEvent::Buy(_))
        ));
        let s = event_data(SELL_EVENT_DISCRIMINATOR, &sell_payload(352));
        assert!(matches!(
            decode_pumpswap_event(&s),
            Some(PumpSwapEvent::Sell(_))
        ));
        let c = event_data(CREATE_POOL_EVENT_DISCRIMINATOR, &create_pool_payload(326));
        assert!(matches!(
            decode_pumpswap_event(&c),
            Some(PumpSwapEvent::CreatePool(_))
        ));
        // Unknown event discriminator → None.
        assert!(decode_pumpswap_event(&event_data([1u8; 8], &buy_payload(353))).is_none());
        assert!(decode_pumpswap_event(&[]).is_none());
    }

    #[test]
    fn trade_from_buy_event_maps_every_field() {
        let f = buy_fix();
        let e = decode_buy_event(&event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353)))
            .expect("decodes");
        let t = trade_from_buy_event(&e);
        assert_eq!(t.pool, pk(0xA0));
        assert_eq!(t.base_mint, None, "buy events carry no mint");
        assert_eq!(t.user, pk(0xA1));
        assert_eq!(t.side, PumpSwapSide::Buy);
        assert_eq!(t.base_amount, f.base_out);
        assert_eq!(t.quote_amount_gross, f.quote_in);
        assert_eq!(
            t.quote_amount_user,
            f.quote_in + f.lp_fee + f.protocol_fee + f.creator_fee
        );
        assert_eq!(t.pool_base_reserves_pre, f.base_res);
        assert_eq!(t.pool_quote_reserves_pre, f.quote_res);
        assert_eq!(t.lp_fee, f.lp_fee);
        assert_eq!(t.protocol_fee, f.protocol_fee);
        assert_eq!(t.coin_creator_fee, f.creator_fee);
        assert_eq!(t.timestamp, 1_753_000_000);
    }

    #[test]
    fn trade_from_sell_event_maps_and_defaults_creator_fee() {
        let e = decode_sell_event(&event_data(
            SELL_EVENT_DISCRIMINATOR,
            &sell_payload(SWAP_EVENT_FIXED_LEN),
        ))
        .expect("decodes");
        let t = trade_from_sell_event(&e);
        assert_eq!(t.side, PumpSwapSide::Sell);
        assert_eq!(t.base_amount, buy_fix().base_out);
        assert_eq!(t.coin_creator_fee, 0, "absent tail defaults to 0");
    }

    #[test]
    fn price_helpers_are_fixed_point_and_safe() {
        let f = buy_fix();
        let e = decode_buy_event(&event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353)))
            .expect("decodes");
        let t = trade_from_buy_event(&e);
        // Pool price: 5e10 * 1e9 / 1e12 = 5e7 (0.05 quote per base, fp 1e9).
        assert_eq!(t.pool_price_fp(), Some(50_000_000));
        let exec = t.exec_price_fp().expect("nonzero base");
        // Exact-out buys execute above the pre-trade marginal price.
        assert!(exec > 50_000_000, "exec {exec}");
        assert_eq!(t.pool_price_ratio(), (f.quote_res, f.base_res));
        // Zero denominators never panic.
        let mut z = t;
        z.pool_base_reserves_pre = 0;
        z.base_amount = 0;
        assert_eq!(z.pool_price_fp(), None);
        assert_eq!(z.exec_price_fp(), None);
    }

    #[test]
    fn verify_buy_event_accepts_consistent_fixture() {
        let e = decode_buy_event(&event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353)))
            .expect("decodes");
        assert!(verify_buy_event(&e));
        // The historical (no creator fee) form is equally consistent.
        let e = decode_buy_event(&event_data(
            BUY_EVENT_DISCRIMINATOR,
            &buy_payload(SWAP_EVENT_FIXED_LEN),
        ))
        .expect("decodes");
        assert!(verify_buy_event(&e));
    }

    #[test]
    fn verify_buy_event_rejects_perturbations() {
        let base = decode_buy_event(&event_data(BUY_EVENT_DISCRIMINATOR, &buy_payload(353)))
            .expect("decodes");
        // Forged quote reserves (+10%).
        let mut e = base;
        e.pool_quote_token_reserves += e.pool_quote_token_reserves / 10;
        assert!(!verify_buy_event(&e));
        // Forged base reserves (-10%).
        let mut e = base;
        e.pool_base_token_reserves -= e.pool_base_token_reserves / 10;
        assert!(!verify_buy_event(&e));
        // Forged output amount (+1%).
        let mut e = base;
        e.base_amount_out += e.base_amount_out / 100;
        assert!(!verify_buy_event(&e));
        // Broken lp-fee identity.
        let mut e = base;
        e.quote_amount_in_with_lp_fee += 1;
        assert!(!verify_buy_event(&e));
        // Degenerate empty pool only admits the zero trade.
        let mut e = base;
        e.pool_quote_token_reserves = 0;
        assert!(!verify_buy_event(&e));
        e.quote_amount_in = 0;
        e.base_amount_out = 0;
        e.lp_fee = 0;
        e.quote_amount_in_with_lp_fee = 0;
        assert!(verify_buy_event(&e));
    }
}
