//! Cashback through the paper executor's quotes, on the REAL captured pump-amm events
//! (`pump-quant-protocol/tests/fixtures/pumpswap_event_layouts_2026_10.json`, identical to
//! `proc/m3_amm_event_fixtures.json`: 27 successful mainnet Buy/Sell event CPIs, expected fields decoded by
//! `@pump-fun/pump-sdk` 2.0.0).
//!
//! What is proven here:
//! * the cashback STATE comes from the event's own layout (`cashback_field_of_event`), never assumed;
//! * KNOWN (incl. known zero) replaces the blanket `amm_cashback_unknown` refusal; MISSING / UNSUPPORTED /
//!   NOT_RECORDED with no creator fee stay refused by name;
//! * the cashback is an ENTITLEMENT inside venue fees, separate from immediately received proceeds, counted
//!   exactly once (gross - venue_fees == net, venue_fees == lp + protocol + creator + cashback);
//! * sells reproduce the chain's net and cashback exactly; exact-in buys reproduce net-in and tokens.
use pump_quant_app::exec_quote::{amm_buy, amm_sell, QuoteRefusal};
use pump_quant_protocol::pumpswap_event::{
    cashback_field, cashback_field_of_event, swap_event_payload, CashbackField,
};
use serde_json::Value;

fn events() -> Vec<Value> {
    let f: Value = serde_json::from_str(include_str!(
        "../../pump-quant-protocol/tests/fixtures/pumpswap_event_layouts_2026_10.json"
    ))
    .unwrap();
    f["events"].as_array().unwrap().clone()
}
fn n(v: &Value, k: &str) -> u64 {
    v[k].as_str().unwrap().parse().unwrap()
}
fn parts(v: &Value) -> Option<(u32, u32, u32)> {
    Some((
        n(v, "lp_bps") as u32,
        n(v, "pr_bps") as u32,
        n(v, "cr_bps") as u32,
    ))
}
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn field(v: &Value) -> CashbackField {
    cashback_field_of_event(
        &hex(v["data_hex"].as_str().unwrap()),
        v["buy"].as_bool().unwrap(),
    )
}

/// Every real sell: the quote at the event's own pre-trade state and size equals the chain's settlement. The
/// cashback is withheld from proceeds (entitlement) and counted once inside venue fees.
#[test]
fn real_sells_quote_exactly_and_cashback_is_entitlement_counted_once() {
    let (mut cb_coins, mut known_zero) = (0, 0);
    for v in events().iter().filter(|v| !v["buy"].as_bool().unwrap()) {
        let c = field(v);
        let q = amm_sell(
            n(v, "pb"),
            n(v, "pq"),
            Some(n(v, "vq")),
            parts(v),
            c,
            n(v, "amt_base"),
        )
        .unwrap_or_else(|r| panic!("{} {r:?}", v["sig"]));
        assert_eq!(q.gross, n(v, "quote_core"), "gross {}", v["sig"]);
        assert_eq!(
            q.net,
            n(v, "user_quote"),
            "net = immediately received {}",
            v["sig"]
        );
        assert_eq!(q.cashback_withheld, n(v, "cb"), "entitlement {}", v["sig"]);
        assert_eq!(
            q.venue_fees,
            n(v, "lp") + n(v, "pr") + n(v, "cr") + n(v, "cb"),
            "every component once {}",
            v["sig"]
        );
        assert_eq!(
            q.gross - q.venue_fees,
            q.net,
            "no double count {}",
            v["sig"]
        );
        if n(v, "cb_bps") > 0 {
            assert_eq!(n(v, "cr_bps"), 0);
            assert!(q.cashback_withheld > 0);
            cb_coins += 1;
        } else {
            assert_eq!(q.cashback_withheld, 0);
            known_zero += 1;
        }
    }
    assert_eq!((cb_coins, known_zero), (6, 2));
}

/// The SAME cashback-coin sells with the cashback NOT known (older layout / unknown layout / older stream
/// schema): still the named `amm_cashback_unknown`, never priced as zero.
#[test]
fn unknown_cashback_with_zero_creator_fee_stays_a_named_refusal() {
    let mut n_ref = 0;
    for v in events().iter().filter(|v| n(v, "cb_bps") > 0) {
        let buy = v["buy"].as_bool().unwrap();
        let data = hex(v["data_hex"].as_str().unwrap());
        let p = swap_event_payload(&data, buy).unwrap();
        let older = cashback_field(buy, &p[..if buy { 353 } else { 352 }]);
        assert!(matches!(older, CashbackField::Missing { .. }));
        for c in [
            older,
            CashbackField::Unsupported { layout_len: 440 },
            CashbackField::NotRecorded,
        ] {
            let r = if buy {
                amm_buy(
                    n(v, "pb"),
                    n(v, "pq"),
                    Some(n(v, "vq")),
                    parts(v),
                    c,
                    10_000_000,
                )
                .map(|_| ())
            } else {
                amm_sell(
                    n(v, "pb"),
                    n(v, "pq"),
                    Some(n(v, "vq")),
                    parts(v),
                    c,
                    n(v, "amt_base"),
                )
                .map(|_| ())
            };
            assert_eq!(
                r,
                Err(QuoteRefusal::AmmCashbackUnknown),
                "{} {c:?}",
                v["sig"]
            );
            n_ref += 1;
        }
    }
    assert_eq!(n_ref, 14 * 3);
}

/// Non-cashback coins (creator fee > 0): the result with a KNOWN ZERO cashback equals the legacy result (no
/// cashback field at all), so adding the field moves nothing on coins that do not charge it.
#[test]
fn known_zero_prices_exactly_like_the_legacy_rule_on_creator_fee_coins() {
    let mut k = 0;
    for v in events().iter().filter(|v| n(v, "cr_bps") > 0) {
        let c = field(v);
        assert_eq!(c.known().map(|x| x.0), Some(0), "{}", v["sig"]);
        let (pb, pq, vq) = (n(v, "pb"), n(v, "pq"), Some(n(v, "vq")));
        for size in [1_000_000u64, 50_000_000, 2_000_000_000] {
            assert_eq!(
                amm_buy(pb, pq, vq, parts(v), c, size),
                amm_buy(pb, pq, vq, parts(v), CashbackField::NotRecorded, size)
            );
            assert_eq!(
                amm_sell(pb, pq, vq, parts(v), c, size * 1_000),
                amm_sell(
                    pb,
                    pq,
                    vq,
                    parts(v),
                    CashbackField::NotRecorded,
                    size * 1_000
                )
            );
        }
        k += 1;
    }
    assert_eq!(k, 13);
}

/// Real exact-in buys (buy_exact_quote_in layouts) whose own fee identity holds: the quote at the trader's
/// all-in spend reproduces the chain's net-in and tokens; the buy-side cashback is an entitlement inside the
/// spend, counted once (spend == net_in + venue_fees, venue_fees == lp + protocol + creator + cashback).
#[test]
fn real_exact_in_buys_quote_exactly_with_the_cashback_entitlement_inside_the_spend() {
    let (mut ok, mut cb_coins) = (0, 0);
    for v in events().iter().filter(|v| {
        v["buy"].as_bool().unwrap() && v["key"].as_str().unwrap().contains("exact_quote_in:")
    }) {
        let net = n(v, "user_quote");
        let ps = [
            n(v, "lp_bps"),
            n(v, "pr_bps"),
            n(v, "cr_bps"),
            n(v, "cb_bps"),
        ];
        let fs = [n(v, "lp"), n(v, "pr"), n(v, "cr"), n(v, "cb")];
        if !ps
            .iter()
            .zip(fs)
            .all(|(b, f)| (u128::from(net) * u128::from(*b)).div_ceil(10_000) == u128::from(f))
        {
            continue; // the one fee-identity exception (protocol test pins it)
        }
        let q = amm_buy(
            n(v, "pb"),
            n(v, "pq"),
            Some(n(v, "vq")),
            parts(v),
            field(v),
            n(v, "quote_core"),
        )
        .unwrap_or_else(|r| panic!("{} {r:?}", v["sig"]));
        assert_eq!(q.net_in, net, "{}", v["sig"]);
        assert_eq!(q.tokens, n(v, "amt_base"), "{}", v["sig"]);
        assert_eq!(q.cashback_entitlement, n(v, "cb"), "{}", v["sig"]);
        assert_eq!(q.venue_fees, fs.iter().sum::<u64>(), "{}", v["sig"]);
        assert_eq!(q.spend, q.net_in + q.venue_fees, "{}", v["sig"]);
        assert!(q.spend <= n(v, "quote_core"));
        if ps[3] > 0 {
            cb_coins += 1;
        }
        ok += 1;
    }
    assert_eq!((ok, cb_coins), (8, 4));
}
