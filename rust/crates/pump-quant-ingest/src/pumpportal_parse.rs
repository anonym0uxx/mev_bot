//! Port of `feeds/pumpportal.rs` decode logic (leaf `in_pumpportal_parse`).
//!
//! Responsibility: turn a PumpPortal trade message into a [`CanonicalTx`].
//! Ports `parse_message`'s trade path: `txType` gating (create/migration →
//! `None`, buy/sell → a trade), base58 decode of signature/mint/trader, and the
//! amount extraction.
//!
//! CRITICAL §22 CHANGE: the legacy code multiplied a `f64` `solAmount` by a
//! `f64` `LAMPORTS_PER_SOL` and cast to `u64` — a float in the money path, and
//! named repository defect (8). Here the decimal SOL text is converted to
//! lamports with pure integer fixed-point arithmetic in
//! [`decimal_sol_to_lamports`]; no float is ever produced.

use crate::base58;
use crate::canonical::{CanonicalTx, SourceKind, TradeDirection, TxKind};
use crate::json::{self};

/// Lamports per SOL (1e9). Integer scale for the fixed-point SOL conversion.
pub const LAMPORTS_PER_SOL: u128 = 1_000_000_000;
/// Number of lamport decimal places (SOL has 9).
pub const SOL_DECIMALS: usize = 9;

/// Convert a non-negative decimal SOL amount (as source text) into integer
/// lamports, fixed-point, no floats (§22).
///
/// Rules: an optional integer part and an optional fractional part separated by
/// `'.'`; fractional digits beyond [`SOL_DECIMALS`] are truncated (matching a
/// lamport-granular chain). Returns `None` for empty/`no-digit` input,
/// non-digit characters (incl. signs / exponents), or overflow.
///
/// Examples: `"1.5" -> 1_500_000_000`, `"0.000000001" -> 1`, `"2" ->
/// 2_000_000_000`, `"1.9999999999" -> 1_999_999_999` (10th digit truncated).
pub fn decimal_sol_to_lamports(s: &str) -> Option<u128> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };

    // Require at least one digit somewhere.
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }

    let mut lamports: u128 = 0;
    for b in int_part.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        lamports = lamports.checked_mul(10)?.checked_add((b - b'0') as u128)?;
    }
    lamports = lamports.checked_mul(LAMPORTS_PER_SOL)?;

    // Fractional part: digit i (0-based) contributes value * 10^(DECIMALS-1-i).
    let mut scale = LAMPORTS_PER_SOL;
    let mut idx = 0usize;
    for b in frac_part.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        if idx < SOL_DECIMALS {
            scale /= 10;
            lamports = lamports.checked_add((b - b'0') as u128 * scale)?;
            idx += 1;
        }
        // Digits past SOL_DECIMALS are validated then truncated.
    }

    Some(lamports)
}

/// Read an object field as a decimal-SOL lamports value, defaulting to 0 when
/// the field is absent, and failing (`None`) only when a present value is
/// malformed.
fn field_lamports(v: &json::JsonValue, key: &str) -> Option<u128> {
    match v.get(key).and_then(|n| n.as_number_str()) {
        Some(raw) => decimal_sol_to_lamports(raw),
        None => Some(0),
    }
}

/// Read an object field as an integer count, defaulting to 0 when absent and
/// failing (`None`) only when a present value is malformed.
fn field_u128(v: &json::JsonValue, key: &str) -> Option<u128> {
    match v.get(key).and_then(|n| n.as_number_str()) {
        Some(raw) => json::number_to_u128_trunc(raw),
        None => Some(0),
    }
}

/// Parse a PumpPortal message into a [`CanonicalTx`].
///
/// Returns `None` for: malformed JSON, control/ack messages (no `signature` or
/// no `mint`), non-trade `txType` (`create`, migration, unknown), and pubkey /
/// signature decode failures. Buy/sell trades produce a `TxKind::Trade` record
/// with signed `sol_delta` / `token_delta` from the trader's perspective.
pub fn parse_pumpportal(payload: &[u8]) -> Option<CanonicalTx> {
    let root = json::parse(payload)?;

    // Control/ack messages lack a signature (legacy: `get_str("signature")`).
    let sig_str = root.get("signature")?.as_str()?;
    let mint_str = root.get("mint")?.as_str()?;
    let tx_type = root.get("txType")?.as_str()?;

    // create / migration / unknown are not trades.
    if tx_type != "buy" && tx_type != "sell" {
        return None;
    }
    let is_buy = tx_type == "buy";

    let sol_lamports = field_lamports(&root, "solAmount")?;
    let token_amount = field_u128(&root, "tokenAmount")?;
    let vsol_reserves = field_lamports(&root, "vSolInBondingCurve")?;
    let vtoken_reserves = field_u128(&root, "vTokensInBondingCurve")?;
    let market_cap_lamports = field_lamports(&root, "marketCapSol")?;

    let signature = base58::decode_signature(sig_str)?;
    let mint = base58::decode_pubkey(mint_str)?;
    let trader = match root.get("traderPublicKey").and_then(|t| t.as_str()) {
        Some(t) if !t.is_empty() => base58::decode_pubkey(t)?,
        _ => [0u8; 32],
    };

    // Trader-perspective signed deltas (see `CanonicalTx` docs).
    let sol_mag = sol_lamports as i128;
    let token_mag = token_amount as i128;
    let (sol_delta, token_delta) = if is_buy {
        (-sol_mag, token_mag)
    } else {
        (sol_mag, -token_mag)
    };

    let timestamp_ms = field_u128(&root, "timestamp")?.min(u64::MAX as u128) as u64;

    Some(CanonicalTx {
        slot: 0, // PumpPortal does not provide a slot (legacy sets slot = 0).
        signature,
        mint,
        trader,
        sol_delta,
        token_delta,
        vsol_reserves,
        vtoken_reserves,
        market_cap_lamports,
        timestamp_ms,
        direction: if is_buy {
            TradeDirection::Buy
        } else {
            TradeDirection::Sell
        },
        kind: TxKind::Trade,
        source: SourceKind::PumpPortal,
    })
}
