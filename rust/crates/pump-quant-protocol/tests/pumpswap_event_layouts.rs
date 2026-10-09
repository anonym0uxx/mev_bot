//! Current pump-amm Buy/Sell event layouts (2026-10 mainnet captures) and the four-component fee stack
//! (lp + protocol + creator + CASHBACK) on cashback coins. Fixture:
//! `fixtures/pumpswap_event_layouts_2026_10.json`, 27 successful events, expected fields decoded by the
//! official `@pump-fun/pump-sdk` 2.0.0 (independent decoder). Each test can fail: the old 3-entry offset
//! table returns `None` on 16 of the 27 payloads, and dropping cashback breaks the 6 cashback sells and 4 cashback exact-in buys.

use pump_quant_protocol::pumpswap_event::{
    buy_exact_quote_in_cb, cashback_fields, swap_event_payload, virtual_quote_offset,
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
