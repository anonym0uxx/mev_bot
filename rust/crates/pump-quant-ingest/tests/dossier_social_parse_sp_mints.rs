// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'social_parse' component (leaf 'sp_mints').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_ingest::social_parse::*;

#[test]
fn sp_mints_valid_only_and_dedup() {
    let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let text = format!("ape {usdc} now, again {usdc}, ticker $USDC not-an-addr");
    let (m, n) = extract_solana_mints(&text);
    assert_eq!(n, 1, "same mint twice de-duplicates");
    assert_eq!(
        m[0],
        pump_quant_ingest::base58::decode_pubkey(usdc).unwrap()
    );
    let (_, none) = extract_solana_mints("just some normal english words here");
    assert_eq!(none, 0, "plain words never decode to a 32-byte mint");
}
