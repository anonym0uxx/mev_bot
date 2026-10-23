//! 2026-10 PumpSwap event layouts (grown by an appended 40-byte tail). Live mainnet fixtures captured
//! 2026-10-23; `vq_at_offset` was established by the constant-product identity (sell 449, buy 497)
//! and by cross-pool constancy at the pre-existing offset (buy 512). See `virtual_quote_offset`.
use pump_quant_protocol::pumpswap_event::{
    buy_exact_quote_in, decode_pumpswap_event, virtual_quote_offset, virtual_quote_offset_idl_2026_10, PumpSwapEvent,
};

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
        let is_buy = name.starts_with("buy");
        let (want_vq, want_quote) = match fx.get("decoded") {
            Some(d) if d.get("vq").is_some() => (d["vq"].as_i64().unwrap() as i128, d["pool_quote_pre"].as_u64().unwrap()),
            _ => (fx["vq_at_offset"].as_u64().unwrap() as i128, fx["quote_res"].as_u64().unwrap()),
        };
        let payload_len = data.len() - 16;
        let payload = &data[16..];
        // Every captured layout is located structurally by the pinned IDL (variable `ix_name`);
        // the legacy fixed table agrees wherever it has an entry.
        let idl_off = virtual_quote_offset_idl_2026_10(is_buy, payload).unwrap_or_else(|| panic!("{name}: len {payload_len} not an IDL shape"));
        if let Some(fixed) = virtual_quote_offset(is_buy, payload_len) {
            assert_eq!(fixed, idl_off, "{name}: table and IDL disagree");
        }
        let v2 = fx["decoded"]["ix_name"].as_str().is_some_and(|n| n.ends_with("_v2"));
        // Balance-change verification (jsonParsed meta of the same transaction, recorded in the
        // fixture): the LP fee is RETAINED IN THE POOL VAULT; protocol + creator fees leave it.
        let pool_delta = fx["balance_deltas"]["pool_quote_vault"].as_i64().unwrap();
        let user_delta = fx["balance_deltas"]["user_quote"].as_i64();
        match decode_pumpswap_event(&data).unwrap_or_else(|| panic!("{name}: no decode")) {
            PumpSwapEvent::Buy(b) => {
                assert!(is_buy, "{name}");
                assert_eq!(b.pool_quote_token_reserves, want_quote, "{name}");
                assert_eq!(b.virtual_quote_reserves, Some(want_vq), "{name}");
                if v2 {
                    // v2 (buy_exact_quote_in_v2): field roles are inverted — `quote_amount_in` is the
                    // user's GROSS, `user_quote_amount_in` the NET reaching the curve leg. Protocol +
                    // creator fees are RETAINED in the vault (Pump docs: fee adjustment); only the
                    // buyback fee leaves: vault delta = gross - buyback_fee.
                    let gross = b.quote_amount_in;
                    let net = b.user_quote_amount_in;
                    assert_eq!(gross, net + b.lp_fee + b.protocol_fee + b.coin_creator_fee.unwrap_or(0), "{name}: gross = net + lp + protocol + creator");
                    let buyback = fx["decoded"]["buyback_fee"].as_u64().unwrap();
                    assert_eq!(pool_delta, (gross - buyback) as i64, "{name}: vault retains everything but the buyback fee");
                    // Constant-product identity with the SIGNED effective quote: exact.
                    let fill = buy_exact_quote_in(
                        b.pool_base_token_reserves as u128,
                        b.pool_quote_token_reserves as u128,
                        want_vq,
                        gross as u128,
                        b.lp_fee_basis_points as u128,
                        b.protocol_fee_basis_points as u128,
                        b.coin_creator_fee_basis_points.unwrap_or(0) as u128,
                    )
                    .unwrap();
                    assert_eq!(fill.base_out, u128::from(b.base_amount_out), "{name}: base_out identity");
                } else {
                    assert_eq!(pool_delta, (b.quote_amount_in + b.lp_fee) as i64, "{name}: vault gains quote_in + lp fee");
                    assert_eq!(b.user_quote_amount_in, b.quote_amount_in + b.lp_fee + b.protocol_fee + b.coin_creator_fee.unwrap_or(0), "{name}: gross = net + all fees");
                    assert_eq!(user_delta, Some(-(b.user_quote_amount_in as i64)), "{name}: user pays gross");
                }
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
    assert_eq!(seen, 4);
    // The fixed table still does not list 507: it is covered structurally (ix_name 21 bytes).
    assert!(virtual_quote_offset(true, 507).is_none());
    // Unknown shapes stay unsupported: a payload one byte off the IDL shape.
    let mut p = vec![0u8; 441];
    assert!(virtual_quote_offset_idl_2026_10(false, &p).is_some());
    p.push(0);
    assert!(virtual_quote_offset_idl_2026_10(false, &p).is_none());
}

#[test]
fn virtual_quote_is_signed_and_only_a_nonpositive_effective_quote_is_refused() {
    use pump_quant_protocol::pumpswap_event::{effective_quote, read_virtual_quote_i128, sell_gross_quote_out};
    let mut p = vec![0u8; 32];
    p[..16].copy_from_slice(&(17_223_035_079i128).to_le_bytes());
    assert_eq!(read_virtual_quote_i128(&p, 0), Some(17_223_035_079));
    p[..16].copy_from_slice(&(-30_000_000i128).to_le_bytes());
    assert_eq!(read_virtual_quote_i128(&p, 0), Some(-30_000_000), "a negative adjustment is a valid value (fees kept in the vault)");
    assert_eq!(read_virtual_quote_i128(&p[..10], 0), None, "short payload");
    // Pump docs example: vault 110.00, fees kept 0.03 => vq = -0.03, effective 109.97.
    assert_eq!(effective_quote(110_000_000_000, -30_000_000), Some(109_970_000_000));
    // Only an invalid EFFECTIVE result is refused.
    assert_eq!(effective_quote(1_000, -1_000), None);
    assert_eq!(effective_quote(1_000, -2_000), None);
    // Pricing uses the effective quote: a negative vq lowers the sell proceeds versus the raw vault.
    let raw = sell_gross_quote_out(1_000_000_000, 110_000_000_000, 0, 1_000_000).unwrap();
    let adj = sell_gross_quote_out(1_000_000_000, 110_000_000_000, -30_000_000, 1_000_000).unwrap();
    assert!(adj < raw, "the raw vault balance is not liquidity");
}
