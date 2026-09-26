//! Escapes the JSON parser must survive — because token NAMES flow through it.
//!
//! The name is now a first-class model input (the narrative is inferred from it),
//! so a name the parser silently drops is a narrative silently missing. Memecoin
//! names are full of emoji, and emoji are written as `\uXXXX` SURROGATE PAIRS in
//! transports that escape non-ASCII.

use pump_quant_ingest::json::parse;

#[test]
fn a_surrogate_pair_decodes_to_one_scalar() {
    // "🍔" written the way a non-ASCII-escaping transport writes it. Before the
    // surrogate-pair fix this returned None: `char::from_u32(0xD83C)` is `None`,
    // the `?` failed the whole string, and the caller dropped the ENTIRE row.
    let v = parse(br#"{"name":"\ud83c\udf55 borgar"}"#).expect("an emoji name must parse");
    assert_eq!(
        v.get("name").and_then(|x| x.as_str()),
        Some("\u{1F355} borgar")
    );
}

#[test]
fn a_name_that_is_only_emoji_parses() {
    let v = parse(br#"{"name":"\ud83c\udf55\ud83c\udf54"}"#).expect("parses");
    assert_eq!(
        v.get("name").and_then(|x| x.as_str()),
        Some("\u{1F355}\u{1F354}")
    );
}

#[test]
fn a_bmp_escape_still_decodes() {
    // A single (non-surrogate) escape must keep working — the surrogate branch
    // must not have swallowed the ordinary one.
    let v = parse(br#"{"name":"An\u00e9mal"}"#).expect("parses");
    assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("Anémal"));
}

#[test]
fn a_lone_surrogate_is_rejected_rather_than_half_decoded() {
    // Half of a pair is invalid JSON. Rejecting is correct; emitting a replacement
    // char would silently invent a name the chain never carried.
    assert!(
        parse(br#"{"name":"\ud83c"}"#).is_none(),
        "lone high surrogate"
    );
    assert!(
        parse(br#"{"name":"\udf55"}"#).is_none(),
        "lone low surrogate"
    );
    // A high surrogate followed by a non-low one is equally invalid.
    assert!(parse(br#"{"name":"\ud83cA"}"#).is_none());
}

#[test]
fn plain_ascii_and_literal_utf8_are_unaffected() {
    let v = parse(br#"{"name":"burger","symbol":"BGR"}"#).expect("parses");
    assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("burger"));
    // Literal (unescaped) UTF-8 must keep working too.
    let v = parse("{\"name\":\"🍔 literal\"}".as_bytes()).expect("parses");
    assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("🍔 literal"));
}
