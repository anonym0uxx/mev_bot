//! On-chain **token-metadata** parsing (leaf `in_token_metadata_parse`).
//!
//! Responsibility: turn a **vendor-agnostic normalized** token-metadata payload
//! (one mint's decoded create/metadata record, as minimal JSON) into a
//! deterministic [`RawTokenMetadata`] — the factual-category analogue of what
//! [`crate::social_parse`] does for attention. The live decode that produces the
//! normalized JSON (a Helius/pumpportal create event, the mint metadata account)
//! is OUT OF SCOPE here — it is `[S]` live-I/O behind [`crate::social_source`] and
//! the on-chain decoders. This module is the pure decoder only.
//!
//! # Constitution discipline (binding)
//! * **§22 determinism / integer.** No floating point, no wall-clock, no RNG, no
//!   network. The assignment instant is supplied by the caller as an already-
//!   measured `u64` `slot`; this module never reads a clock. The variable-length
//!   creator handle is folded to a fixed-width `u64` via [`fnv1a_64`].
//! * **§85 / criterion 83 on-chain-led.** This decodes the token's *own* on-chain
//!   metadata (`name`, `symbol`, `creator`) — never social text. The classifier
//!   ([`pump_quant_market_state::meta::classify_category`]) that turns
//!   `name`/`symbol` into a category id runs one layer up in the app composition
//!   root (`token_ingest`), so this leaf stays dependency-free; the engine's
//!   factual `MetaRotationState` can only ever be populated from this path.
//! * **§99 bounded.** The parsed `name`/`symbol` are the caller's own strings; the
//!   classifier truncates its scan. Nothing here retains unbounded state.

use crate::base58;
use crate::json;
use crate::social_parse::fnv1a_64;

/// A decoded, normalized on-chain token-metadata record for one mint.
///
/// Owns its `name`/`symbol` strings (the only allocation on this `[S]`-adjacent
/// decode path) so the downstream classifier can scan them; everything else is a
/// fixed-width integer identity. Deterministic given the input bytes + `slot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTokenMetadata {
    /// The 32-byte mint key.
    pub mint: [u8; 32],
    /// Decoded token name (may be empty).
    pub name: String,
    /// Decoded token symbol / ticker (may be empty).
    pub symbol: String,
    /// Creator/deployer entity id: FNV-1a of the decoded creator handle/address, or
    /// `0` when the payload names no creator.
    pub creator: u64,
    /// The raw 32-byte creator/deployer pubkey, when the payload carried a
    /// base58-encoded Solana address (the PumpPortal create event's
    /// `traderPublicKey`). `None` when the creator field is a non-pubkey handle
    /// or absent — the R-3 creator-history veto (daemon-level) uses this to
    /// query `getSignaturesForAddress` on the creator wallet before buying.
    pub creator_pubkey: Option<[u8; 32]>,
    /// Slot at which the metadata was observed (caller-supplied time; time-safe).
    pub slot: u64,
}

/// Parse one **normalized** token-metadata payload into a [`RawTokenMetadata`].
///
/// Expected JSON (produced by the `[S]` on-chain decoder that normalizes each
/// vendor/create event):
/// ```json
/// { "mint": "So1111...", "name": "Doge Killer", "symbol": "DOGE",
///   "creator": "Cre8r..." }
/// ```
/// `slot` is supplied out-of-band (measured at decode). `name`/`symbol`/`creator`
/// are optional and default to empty/`0`. Returns `None` only when the JSON is
/// unparseable or the `mint` is missing or not a valid 32-byte base58 key — a
/// record with no concrete mint has nothing factual to attribute. No float, no
/// clock (§22).
#[must_use]
pub fn parse_token_metadata(raw: &[u8], slot: u64) -> Option<RawTokenMetadata> {
    let v = json::parse(raw)?;
    let mint = base58::decode_pubkey(v.get("mint")?.as_str()?)?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let symbol = v
        .get("symbol")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let creator_str = v
        .get("creator")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty());
    let creator = creator_str.map_or(0, |s| fnv1a_64(s.as_bytes()));
    // R-3: attempt to decode the creator string as a base58 Solana pubkey. When
    // it decodes, the R-3 veto can query getSignaturesForAddress on this wallet.
    // When it does not (social handle or absent), creator_pubkey stays None and
    // the R-3 veto is skipped for this mint (fail-open, §6.4).
    let creator_pubkey = creator_str.and_then(|s| base58::decode_pubkey(s));
    Some(RawTokenMetadata {
        mint,
        name,
        symbol,
        creator,
        creator_pubkey,
        slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    #[test]
    fn parses_full_metadata() {
        let raw =
            format!(r#"{{"mint":"{USDC}","name":"Doge Killer","symbol":"DOGE","creator":"dev1"}}"#);
        let m = parse_token_metadata(raw.as_bytes(), 42).unwrap();
        assert_eq!(m.mint, base58::decode_pubkey(USDC).unwrap());
        assert_eq!(m.name, "Doge Killer");
        assert_eq!(m.symbol, "DOGE");
        assert_eq!(m.creator, fnv1a_64(b"dev1"));
        // "dev1" is not a valid base58 pubkey → None
        assert!(m.creator_pubkey.is_none());
        assert_eq!(m.slot, 42);
    }

    #[test]
    fn missing_optional_fields_default() {
        let raw = format!(r#"{{"mint":"{USDC}"}}"#);
        let m = parse_token_metadata(raw.as_bytes(), 7).unwrap();
        assert!(m.name.is_empty() && m.symbol.is_empty());
        assert_eq!(m.creator, 0, "no creator named → 0");
    }

    #[test]
    fn rejects_missing_or_invalid_mint() {
        assert!(parse_token_metadata(br#"{"name":"x"}"#, 1).is_none());
        assert!(parse_token_metadata(br#"{"mint":"not-a-key"}"#, 1).is_none());
        assert!(parse_token_metadata(b"not json", 1).is_none());
    }

    #[test]
    fn deterministic_same_bytes_same_record() {
        let raw = format!(r#"{{"mint":"{USDC}","name":"AI Agent","symbol":"GPT"}}"#);
        assert_eq!(
            parse_token_metadata(raw.as_bytes(), 9),
            parse_token_metadata(raw.as_bytes(), 9)
        );
    }
}
