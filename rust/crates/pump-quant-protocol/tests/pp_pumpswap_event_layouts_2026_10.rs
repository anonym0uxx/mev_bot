//! 2026-10 PumpSwap event layouts (grown by an appended 40-byte tail). Live mainnet fixtures captured
//! 2026-10-23; `vq_at_offset` was established by the constant-product identity (sell 449, buy 497)
//! and by cross-pool constancy at the pre-existing offset (buy 512). See `virtual_quote_offset`.
use pump_quant_protocol::pumpswap_event::{decode_pumpswap_event, virtual_quote_offset, PumpSwapEvent};

#[test]
fn new_layouts_decode_with_virtual_quote() {
    let raw = include_str!("pumpswap_event_layouts_2026_10.json");
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let mut seen = 0;
    for (name, fx) in v.as_object().unwrap() {
        let hx = fx["data_hex"].as_str().unwrap();
        // fixture = discriminator + payload; the decoder takes the full instruction data, which
        // starts with the 8-byte Anchor self-CPI event tag.
        let mut data: Vec<u8> = vec![0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];
        data.extend((0..hx.len()).step_by(2).map(|i| u8::from_str_radix(&hx[i..i + 2], 16).unwrap()));
        let want_vq = fx["vq_at_offset"].as_u64().unwrap();
        let want_quote = fx["quote_res"].as_u64().unwrap();
        let payload_len = data.len() - 16;
        let is_buy = name.starts_with("buy");
        assert!(virtual_quote_offset(is_buy, payload_len).is_some(), "{name}: len {payload_len} unsupported");
        match decode_pumpswap_event(&data).unwrap_or_else(|| panic!("{name}: no decode")) {
            PumpSwapEvent::Buy(b) => {
                assert!(is_buy, "{name}");
                assert_eq!(b.pool_quote_token_reserves, want_quote, "{name}");
                assert_eq!(b.virtual_quote_reserves, Some(want_vq), "{name}");
            }
            PumpSwapEvent::Sell(s) => {
                assert!(!is_buy, "{name}");
                assert_eq!(s.pool_quote_token_reserves, want_quote, "{name}");
                assert_eq!(s.virtual_quote_reserves, Some(want_vq), "{name}");
            }
            PumpSwapEvent::CreatePool(_) => panic!("{name}: create"),
        }
        seen += 1;
    }
    assert_eq!(seen, 3);
    // Unverified length stays unsupported (never guessed).
    assert!(virtual_quote_offset(true, 507).is_none());
}
