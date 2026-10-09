//! Current pump-amm Buy/Sell event layouts (2026-10 mainnet captures) and the four-component fee stack
//! (lp + protocol + creator + CASHBACK) on cashback coins. Fixture:
//! `fixtures/pumpswap_event_layouts_2026_10.json`, 27 successful events, expected fields decoded by the
//! official `@pump-fun/pump-sdk` 2.0.0 (independent decoder). Each test can fail: the old 3-entry offset
//! table returns `None` on 16 of the 27 payloads, and dropping cashback breaks the 6 cashback sells and 4 cashback exact-in buys.

use pump_quant_protocol::pumpswap_event::{
    buy_exact_quote_in_cb, buy_layout_ix_name, cashback_field, cashback_field_of_event,
    cashback_fields, pre_cashback_tail_end, swap_event_payload, swap_layout_semantics_ok,
    virtual_quote_offset, CashbackField, BUY_IX_NAME_OFFSET, CASHBACK_BPS_MAX, SWAP_EVENT_FIXED_LEN,
};
use pump_quant_protocol::pumpswap_fees::{sell_net_quote_cb, Fees};
use serde_json::Value;

fn events() -> Vec<Value> {
    let f: Value =
        serde_json::from_str(include_str!("fixtures/pumpswap_event_layouts_2026_10.json")).unwrap();
    f["events"].as_array().unwrap().clone()
}
fn n(v: &Value, k: &str) -> u128 {
    v[k].as_str().unwrap().parse().unwrap()
}
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn every_current_layout_yields_the_virtual_quote_and_cashback_the_sdk_decodes() {
    let ev = events();
    assert_eq!(ev.len(), 27);
    for v in &ev {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let p = swap_event_payload(&data, buy).expect("event identity");
        let o = virtual_quote_offset(buy, p.len()).unwrap_or_else(|| panic!("{}", v["key"]));
        let vq = u64::from_le_bytes(p[o..o + 8].try_into().unwrap());
        assert_eq!(u128::from(vq), n(v, "vq"), "{}", v["key"]);
        // High 8 bytes of the i128 are zero on every captured pool (non-negative, < 2^64).
        assert_eq!(&p[o + 8..o + 16], &[0u8; 8]);
        let cb = cashback_fields(buy, p).expect("every known layout carries cashback");
        assert_eq!(
            (u128::from(cb.0), u128::from(cb.1)),
            (n(v, "cb_bps"), n(v, "cb")),
            "{}",
            v["key"]
        );
    }
}

#[test]
fn sells_net_exact_with_cashback_withheld() {
    let mut ok = 0;
    for v in events().iter().filter(|v| !v["buy"].as_bool().unwrap()) {
        let f = Fees {
            lp_bps: n(v, "lp_bps") as u64,
            protocol_bps: n(v, "pr_bps") as u64,
            creator_bps: n(v, "cr_bps") as u64,
        };
        let (q, cb) = sell_net_quote_cb(
            n(v, "pb"),
            n(v, "pq"),
            n(v, "vq"),
            n(v, "amt_base"),
            f,
            n(v, "cb_bps") as u64,
        )
        .expect("payable");
        assert_eq!(q.gross, n(v, "quote_core"), "gross {}", v["sig"]);
        assert_eq!(
            (q.lp_fee, q.protocol_fee, q.creator_fee, cb),
            (n(v, "lp"), n(v, "pr"), n(v, "cr"), n(v, "cb"))
        );
        assert_eq!(q.net, n(v, "user_quote"), "net {}", v["sig"]);
        ok += 1;
    }
    assert_eq!(ok, 8);
}

#[test]
fn exact_quote_in_buys_with_cashback_and_the_fee_identity_exception_is_refused() {
    let (mut ok, mut refused) = (0, Vec::new());
    for v in events()
        .iter()
        .filter(|v| v["buy"].as_bool().unwrap() && v["key"].as_str().unwrap().contains("exact"))
    {
        let parts = [
            n(v, "lp_bps"),
            n(v, "pr_bps"),
            n(v, "cr_bps"),
            n(v, "cb_bps"),
        ];
        let fees = [n(v, "lp"), n(v, "pr"), n(v, "cr"), n(v, "cb")];
        // The event's own identity: every reported fee is ceil(net * bps) of the net it reports.
        let net = n(v, "user_quote");
        let consistent = parts
            .iter()
            .zip(fees)
            .all(|(b, f)| (net * b).div_ceil(10_000) == f);
        if !consistent {
            refused.push(v["sig"].as_str().unwrap().to_string());
            continue;
        }
        // Gross spend = what the trader paid: the event's `quote_amount_in` here (net + all fees).
        let fill = buy_exact_quote_in_cb(
            n(v, "pb"),
            n(v, "pq"),
            n(v, "vq"),
            n(v, "quote_core"),
            (parts[0], parts[1], parts[2]),
            parts[3],
        )
        .unwrap();
        assert_eq!(fill.net_quote_in, net, "{}", v["sig"]);
        assert_eq!(fill.base_out, n(v, "amt_base"), "{}", v["sig"]);
        ok += 1;
    }
    assert_eq!(ok, 10);
    assert_eq!(refused.len(), 1, "{refused:?}");
}

// ---------------------------------------------------------------------------------------------------------
// Cashback field STATE with layout provenance (KNOWN / MISSING / UNSUPPORTED), on the real captured events.
// ---------------------------------------------------------------------------------------------------------

/// Every captured current-layout event is KNOWN with the SDK's values and its own payload length as provenance;
/// the 17 non-cashback-coin events are KNOWN ZERO (field present, value 0), never "missing".
#[test]
fn every_captured_event_is_known_and_non_cashback_coins_are_known_zero() {
    let (mut known_zero, mut known_nonzero) = (0, 0);
    for v in &events() {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let p = swap_event_payload(&data, buy).unwrap();
        let c = cashback_field_of_event(&data, buy);
        assert_eq!(c, cashback_field(buy, p));
        let CashbackField::Known {
            bps,
            lamports,
            layout_len,
        } = c
        else {
            panic!("{} -> {c:?}", v["key"]);
        };
        assert_eq!(
            usize::from(layout_len),
            p.len(),
            "provenance = payload length"
        );
        assert_eq!(
            (u128::from(bps), u128::from(lamports)),
            (n(v, "cb_bps"), n(v, "cb"))
        );
        assert_eq!(c.state(), "known");
        if bps == 0 {
            assert_eq!(lamports, 0, "{}", v["key"]);
            assert!(
                v["key"].as_str().unwrap().ends_with("false"),
                "{}",
                v["key"]
            );
            known_zero += 1;
        } else {
            known_nonzero += 1;
        }
    }
    assert_eq!((known_zero, known_nonzero), (13, 14));
}

/// MISSING: the same real events cut back to the creator-fee-era tail (sell 352 / buy 353 bytes, the layout
/// before any cashback field was appended) and to the 336-byte pre-creator-fee prefix. DERIVED from the real
/// captures (truncation; no pre-cashback event is in the capture set): the decoder never invents a pair.
#[test]
fn older_layouts_are_missing_never_known_zero() {
    for v in &events() {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let p = swap_event_payload(&data, buy).unwrap();
        for len in [SWAP_EVENT_FIXED_LEN, pre_cashback_tail_end(buy)] {
            let c = cashback_field(buy, &p[..len]);
            assert_eq!(
                c,
                CashbackField::Missing {
                    layout_len: len as u16
                },
                "{} @{len}",
                v["key"]
            );
            assert_eq!(c.known(), None);
        }
    }
}

/// UNSUPPORTED: any length the layout table does not know (between the old tail and the current layouts, one
/// byte longer/shorter than a known layout, or a future append) is unsupported: not missing, not zero.
#[test]
fn unknown_layouts_are_unsupported_not_missing_not_zero() {
    let mut n_checked = 0;
    for v in &events() {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let p = swap_event_payload(&data, buy).unwrap().to_vec();
        let known_len = p.len();
        let mut lens = vec![pre_cashback_tail_end(buy) + 1, known_len - 1, known_len + 1];
        lens.retain(|l| virtual_quote_offset(buy, *l).is_none());
        for len in lens {
            let mut q = p.clone();
            q.resize(len, 0);
            let c = cashback_field(buy, &q);
            assert_eq!(
                c,
                CashbackField::Unsupported {
                    layout_len: len as u16
                },
                "{} @{len}",
                v["key"]
            );
            assert_eq!(c.known(), None);
            n_checked += 1;
        }
    }
    assert!(n_checked >= 27 * 2);
    // A wrong discriminator is not a swap event at all.
    let mut d = hex(events()[0]["data_hex"].as_str().unwrap());
    d[8] ^= 0xFF;
    assert_eq!(
        cashback_field_of_event(&d, false),
        CashbackField::Unsupported { layout_len: 0 }
    );
    // The four states are pairwise distinct (not an Option of zero).
    let s = [
        CashbackField::Known {
            bps: 0,
            lamports: 0,
            layout_len: 433,
        },
        CashbackField::Missing { layout_len: 433 },
        CashbackField::Unsupported { layout_len: 433 },
        CashbackField::NotRecorded,
    ];
    for i in 0..4 {
        for j in 0..4 {
            assert_eq!(s[i] == s[j], i == j);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// LAYOUT SEMANTICS binding: discriminator + a known length is NOT enough for `Known`. The bytes must decode
// as that layout (buy: `ix_name` string = this length's name, ending where the pair starts; Borsh bools at
// `track_volume` / `can_boost`), and the rate must be a rate (<= 10_000 bps). A same-length payload with other
// semantics is `Unsupported`, never a misread pair.
// ---------------------------------------------------------------------------------------------------------

/// Every real captured event passes the semantic check (the check is not vacuous-by-refusal).
#[test]
fn every_captured_event_passes_layout_semantics() {
    for v in &events() {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let p = swap_event_payload(&data, buy).unwrap();
        assert!(swap_layout_semantics_ok(buy, p), "{}", v["key"]);
        if buy {
            assert_eq!(
                &p[BUY_IX_NAME_OFFSET + 4..BUY_IX_NAME_OFFSET + 4 + buy_layout_ix_name(p.len()).unwrap().len()],
                buy_layout_ix_name(p.len()).unwrap(),
                "{}",
                v["key"]
            );
        }
    }
}

/// NEGATIVE: real events, same discriminator and same (known) length, but different layout semantics.
/// Each mutation must turn `Known` into `Unsupported { layout_len }` with the unchanged length.
#[test]
fn same_length_payload_with_other_layout_semantics_is_unsupported_not_known() {
    let unsupported = |buy: bool, q: &[u8], what: &str, key: &str| {
        let c = cashback_field(buy, q);
        assert_eq!(
            c,
            CashbackField::Unsupported {
                layout_len: q.len() as u16
            },
            "{key}: {what}"
        );
        assert_eq!(c.known(), None, "{key}: {what}");
        assert_eq!(cashback_fields(buy, q), None, "{key}: {what}");
    };
    let (mut n_buy, mut n_sell) = (0, 0);
    for v in &events() {
        let data = hex(v["data_hex"].as_str().unwrap());
        let buy = v["buy"].as_bool().unwrap();
        let key = v["key"].as_str().unwrap();
        let p = swap_event_payload(&data, buy).unwrap().to_vec();
        assert!(matches!(cashback_field(buy, &p), CashbackField::Known { .. }));
        let vo = virtual_quote_offset(buy, p.len()).unwrap();
        let cb_at = if buy { vo - 32 } else { 352 };
        // (a) rate that is not a rate (> 100%).
        let mut q = p.clone();
        q[cb_at..cb_at + 8].copy_from_slice(&(CASHBACK_BPS_MAX + 1).to_le_bytes());
        unsupported(buy, &q, "bps > 10000", key);
        // (b) `can_boost` (vo + 16) not a Borsh bool.
        let mut q = p.clone();
        q[vo + 16] = 2;
        unsupported(buy, &q, "can_boost not bool", key);
        if buy {
            // (c) ix_name of ANOTHER layout at this length: swap the bytes, keep the length prefix.
            let name = buy_layout_ix_name(p.len()).unwrap();
            let mut q = p.clone();
            q[BUY_IX_NAME_OFFSET + 4] ^= 0x20; // 'b' -> 'B'
            unsupported(buy, &q, "ix_name bytes differ", key);
            // (d) declared ix_name length differs (string would end elsewhere -> pair shifted).
            let mut q = p.clone();
            q[BUY_IX_NAME_OFFSET..BUY_IX_NAME_OFFSET + 4]
                .copy_from_slice(&(name.len() as u32 + 3).to_le_bytes());
            unsupported(buy, &q, "ix_name length differs", key);
            // (e) `track_volume` (352) not a Borsh bool.
            let mut q = p.clone();
            q[352] = 7;
            unsupported(buy, &q, "track_volume not bool", key);
            n_buy += 1;
        } else {
            n_sell += 1;
        }
    }
    assert_eq!((n_buy, n_sell), (19, 8));
}

/// NEGATIVE (cross-layout): a real `buy` event (ix_name "buy") re-sized to an exact-in length is read
/// under the exact-in table: the name does not match the length, so it is refused, not read at offset 415.
#[test]
fn a_buy_payload_padded_to_an_exact_in_length_is_unsupported() {
    let v = events()
        .into_iter()
        .find(|v| v["key"].as_str().unwrap() == "buy:481:buy:false")
        .unwrap();
    let data = hex(v["data_hex"].as_str().unwrap());
    let mut q = swap_event_payload(&data, true).unwrap().to_vec();
    q.resize(496, 0);
    assert!(virtual_quote_offset(true, q.len()).is_some(), "496 is a known length");
    assert_eq!(
        cashback_field(true, &q),
        CashbackField::Unsupported { layout_len: 496 }
    );
}
