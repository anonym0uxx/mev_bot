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
        // Balance-change verification (jsonParsed meta of the same transaction, recorded in the
        // fixture): the LP fee is RETAINED IN THE POOL VAULT; protocol + creator fees leave it.
        let pool_delta = fx["balance_deltas"]["pool_quote_vault"].as_i64().unwrap();
        let user_delta = fx["balance_deltas"]["user_quote"].as_i64();
        match decode_pumpswap_event(&data).unwrap_or_else(|| panic!("{name}: no decode")) {
            PumpSwapEvent::Buy(b) => {
                assert!(is_buy, "{name}");
                assert_eq!(b.pool_quote_token_reserves, want_quote, "{name}");
                assert_eq!(b.virtual_quote_reserves, Some(want_vq), "{name}");
                assert_eq!(pool_delta, (b.quote_amount_in + b.lp_fee) as i64, "{name}: vault gains quote_in + lp fee");
                assert_eq!(b.user_quote_amount_in, b.quote_amount_in + b.lp_fee + b.protocol_fee + b.coin_creator_fee.unwrap_or(0), "{name}: gross = net + all fees");
                assert_eq!(user_delta, Some(-(b.user_quote_amount_in as i64)), "{name}: user pays gross");
            }
            PumpSwapEvent::Sell(s) => {
                assert!(!is_buy, "{name}");
                assert_eq!(s.pool_quote_token_reserves, want_quote, "{name}");
                assert_eq!(s.virtual_quote_reserves, Some(want_vq), "{name}");
                assert_eq!(pool_delta, -((s.quote_amount_out - s.lp_fee) as i64), "{name}: vault loses quote_out minus the retained lp fee");
                assert_eq!(s.user_quote_amount_out, s.quote_amount_out - s.lp_fee - s.protocol_fee - s.coin_creator_fee.unwrap_or(0), "{name}: user receives net of all fees");
            }
            PumpSwapEvent::CreatePool(_) => panic!("{name}: create"),
        }
        seen += 1;
    }
    assert_eq!(seen, 3);
    // Unverified length stays unsupported (never guessed).
    assert!(virtual_quote_offset(true, 507).is_none());
}

#[test]
fn virtual_quote_is_read_as_signed_i128_and_negative_is_unsupported_not_cast() {
    use pump_quant_protocol::pumpswap_event::read_virtual_quote_i128;
    let mut p = vec![0u8; 32];
    p[..16].copy_from_slice(&(17_223_035_079i128).to_le_bytes());
    assert_eq!(read_virtual_quote_i128(&p, 0), Some(17_223_035_079));
    p[..16].copy_from_slice(&(-1i128).to_le_bytes());
    assert_eq!(read_virtual_quote_i128(&p, 0), None, "negative must not become u64::MAX");
    p[..16].copy_from_slice(&((u64::MAX as i128) + 1).to_le_bytes());
    assert_eq!(read_virtual_quote_i128(&p, 0), None);
    assert_eq!(read_virtual_quote_i128(&p[..10], 0), None, "short payload");
}
