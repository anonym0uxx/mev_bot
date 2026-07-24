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
use pump_quant_market_state::meta::{
    classify_category, CategoryTaxonomy, TAXONOMY_V0, TAXONOMY_V1,
};

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

/// Classify under the FROZEN [`TAXONOMY_V0`].
///
/// Retained only to reproduce assignments already stamped `taxonomy_version = 0`
/// (criterion 81 — the fix is forward, never a rewrite of history). v0's naive
/// substring matching mis-categorizes ordinary English ("Fair Launch" → AI via
/// "ai" in "fair"), so it no longer matches the config default and the engine
/// will NOT merge a v0 assignment into the live reducer. New assignments use
/// [`to_token_metadata_v1`].
#[must_use]
pub fn to_token_metadata_v0(raw: &RawTokenMetadata) -> AppEvent {
    to_token_metadata(raw, &TAXONOMY_V0)
}

/// Classify under the shipped [`TAXONOMY_V1`] — the word-boundary-disciplined
/// lexicon whose version matches the config default `meta_taxonomy_version = 1`,
/// so the engine merges the assignment.
///
/// This is the app's default classification entry point. `category_id` is a brain
/// RECALL FILTER KEY: a substring mis-assignment pools a token with the wrong
/// meta's episodes and silently corrupts every conditioned estimate keyed on it,
/// which is why the corrected lexicon — not the historical one — is what new
/// launches are stamped with.
#[must_use]
pub fn to_token_metadata_v1(raw: &RawTokenMetadata) -> AppEvent {
    to_token_metadata(raw, &TAXONOMY_V1)
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
        match to_token_metadata_v1(&parsed) {
            AppEvent::TokenMetadata {
                category_id,
                taxonomy_version,
                slot,
                ..
            } => {
                assert_eq!(category_id, 1, "the 'dog'/'doge' needle → animals (id 1)");
                assert_eq!(taxonomy_version, 1);
                assert_eq!(slot, 5, "slot carried non-retroactively");
            }
            _ => panic!("expected TokenMetadata"),
        }
        // The frozen v0 path still reproduces its own historical stamp.
        match to_token_metadata_v0(&parsed) {
            AppEvent::TokenMetadata {
                taxonomy_version, ..
            } => assert_eq!(taxonomy_version, 0),
            _ => panic!("expected TokenMetadata"),
        }
    }

    #[test]
    fn unmatched_name_is_unclassified() {
        // A name/symbol containing no taxonomy needle.
        let raw = format!(r#"{{"mint":"{USDC}","name":"Quiet Zephyr","symbol":"QZ"}}"#);
        let parsed = parse_token_metadata(raw.as_bytes(), 1).unwrap();
        match to_token_metadata_v1(&parsed) {
            AppEvent::TokenMetadata { category_id, .. } => {
                assert_eq!(category_id, 0, "no needle matched → UNCLASSIFIED (0)");
            }
            _ => panic!("expected TokenMetadata"),
        }
    }

    #[test]
    fn v1_default_path_no_longer_mis_assigns_ordinary_english() {
        // "Fair Launch" hit the AI category under v0 via the "ai" inside "fair".
        // Under the shipped v1 default it is honestly UNCLASSIFIED (§6.4), and the
        // frozen v0 path still reproduces the historical (wrong) assignment.
        let raw = format!(r#"{{"mint":"{USDC}","name":"Fair Launch","symbol":"FAIR"}}"#);
        let parsed = parse_token_metadata(raw.as_bytes(), 9).unwrap();
        match to_token_metadata_v1(&parsed) {
            AppEvent::TokenMetadata { category_id, .. } => assert_eq!(category_id, 0),
            _ => panic!("expected TokenMetadata"),
        }
        match to_token_metadata_v0(&parsed) {
            AppEvent::TokenMetadata { category_id, .. } => assert_eq!(category_id, 4),
            _ => panic!("expected TokenMetadata"),
        }
    }
}
