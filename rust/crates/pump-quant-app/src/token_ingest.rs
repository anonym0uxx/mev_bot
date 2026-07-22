//! Token-metadata ingestion wiring: decoded on-chain metadata → engine input.
//!
//! This is the seam that turns a decoded [`RawTokenMetadata`] into the factual
//! [`AppEvent::TokenMetadata`] the nervous system consumes. It is the *only* place
//! the deterministic category classifier runs, and it runs on the token's own
//! decoded on-chain `name`/`symbol` — never on social text — so the engine's
//! factual `MetaRotationState` is on-chain-led by construction (§21.4, §85,
//! criterion 83). Capture/decode itself is `[S]` server I/O upstream; everything
//! here is pure and deterministic (§22).
//!
//! # Discipline (binding)
//! * **On-chain-led factual state (§85).** The category is assigned by
//!   [`classify_category`] from the mint's decoded metadata. Social interpretation
//!   can never reach this path — the app composition root wires it separately from
//!   [`crate::social_ingest`].
//! * **Non-retroactive (criterion 81).** The `taxonomy_version` and `slot` of the
//!   assignment are carried onto the event; a later re-classification is a new
//!   assignment at a new slot, never a rewrite. The engine only merges an
//!   assignment whose `taxonomy_version` matches its reducer's version.
//! * **Integer at the engine boundary (§22).** The engine sees only the resolved
//!   integer `category_id`; the string scan is confined to this `[S]`-adjacent
//!   layer, off the deterministic tick hot path.

use crate::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::token_metadata_parse::RawTokenMetadata;
use pump_quant_market_state::meta::{classify_category, CategoryTaxonomy, TAXONOMY_V0};

/// Classify decoded on-chain token metadata into a factual
/// [`AppEvent::TokenMetadata`] under an explicit taxonomy.
///
/// The classifier runs here on the token's decoded `name`/`symbol`; the engine
/// downstream sees only the integer category id (0 = UNCLASSIFIED). Deterministic,
/// total (§22).
#[must_use]
pub fn to_token_metadata(raw: &RawTokenMetadata, taxonomy: &CategoryTaxonomy) -> AppEvent {
    let a = classify_category(&raw.name, &raw.symbol, taxonomy, raw.slot);
    AppEvent::TokenMetadata {
        mint: Mint::from_bytes(raw.mint),
        category_id: a.category_id,
        taxonomy_version: a.taxonomy_version,
        creator: raw.creator,
        slot: raw.slot,
    }
}

/// Classify under the shipped [`TAXONOMY_V0`] (whose version matches the config
/// default `meta_taxonomy_version = 0`, so the engine merges the assignment).
#[must_use]
pub fn to_token_metadata_v0(raw: &RawTokenMetadata) -> AppEvent {
    to_token_metadata(raw, &TAXONOMY_V0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use pump_quant_ingest::token_metadata_parse::parse_token_metadata;

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    #[test]
    fn animal_name_classifies_to_category_one() {
        let raw =
            format!(r#"{{"mint":"{USDC}","name":"Doge Killer","symbol":"DOGE","creator":"d"}}"#);
        let parsed = parse_token_metadata(raw.as_bytes(), 5).unwrap();
        match to_token_metadata_v0(&parsed) {
            AppEvent::TokenMetadata {
                category_id,
                taxonomy_version,
                slot,
                ..
            } => {
                assert_eq!(category_id, 1, "the 'dog'/'doge' needle → animals (id 1)");
                assert_eq!(taxonomy_version, 0);
                assert_eq!(slot, 5, "slot carried non-retroactively");
            }
            _ => panic!("expected TokenMetadata"),
        }
    }

    #[test]
    fn unmatched_name_is_unclassified() {
        // A name/symbol containing no taxonomy needle (careful: substrings like
        // "ai" in "plain" or "cat" in "location" would match — this one has none).
        let raw = format!(r#"{{"mint":"{USDC}","name":"Quiet Zephyr","symbol":"QZ"}}"#);
        let parsed = parse_token_metadata(raw.as_bytes(), 1).unwrap();
        match to_token_metadata_v0(&parsed) {
            AppEvent::TokenMetadata { category_id, .. } => {
                assert_eq!(category_id, 0, "no needle matched → UNCLASSIFIED (0)");
            }
            _ => panic!("expected TokenMetadata"),
        }
    }
}
