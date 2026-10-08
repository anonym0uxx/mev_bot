//! PRE-TRADE fee source acceptance. The rates come from the mainnet `FeeConfig` account (snapshot shipped
//! in the official pump-swap-sdk 1.20.0, slot 445186127), selected by the pool's PRE-TRADE reserves; the
//! trade's realised fee fields are used only as the thing being predicted, never as an input.
//!
//! ASSUMPTIONS the vectors cannot check (they carry no mint/pool account): base mint supply = 1e15 raw
//! (pump.fun mint, no burns), pool is canonical, quote is WSOL, not mayhem, coin_creator set, no per-pool
//! creator rate; and the 445186127 snapshot was in force at each vector's slot (441.34M..445.78M).
//! Validation counts only the `untouched` group.

use pump_quant_protocol::pumpswap_event::buy_exact_quote_in;
use pump_quant_protocol::pumpswap_fees::{
    decode_fee_config, fees_for, sell_net_quote, tier_for, FeeRefusal, Fees, PoolFeeInputs,
};
use serde_json::Value;

const SUPPLY: u128 = 1_000_000_000_000_000;

fn cfg() -> pump_quant_protocol::pumpswap_fees::FeeConfig {
    let f: Value =
        serde_json::from_str(include_str!("fixtures/pumpswap_fee_config_mainnet.json")).unwrap();
    let raw = base64_decode(f["data_base64"].as_str().unwrap());
    assert_eq!(raw.len(), 4073);
    decode_fee_config(&raw).expect("mainnet FeeConfig decodes")
}
fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = T.iter().position(|t| *t == c).unwrap() as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}
fn vectors() -> Vec<Value> {
    serde_json::from_str(include_str!("pumpswap_quote_vectors.json")).unwrap()
}
fn u(v: &Value, k: &str) -> u128 {
    u128::from(v[k].as_u64().unwrap_or_else(|| panic!("{k}")))
}
fn inputs(v: &Value) -> PoolFeeInputs {
    PoolFeeInputs {
        is_canonical_pump_pool: true,
        quote_is_sol_like: true,
        is_mayhem_mode: false,
        pool_creator_fee_bps: 0,
        coin_creator_set: true,
        base_mint_supply: SUPPLY,
        base_reserve: u(v, "pb"),
        effective_quote_reserve: u(v, "pq") + u(v, "V"),
    }
}

#[test]
fn the_mainnet_fee_config_decodes_to_the_published_schedule() {
    let c = cfg();
    assert_eq!(c.fee_tiers.len(), 25);
    assert_eq!(c.stable_fee_tiers.len(), 25);
    assert_eq!(c.exotic_flat_fees, None);
    assert_eq!(
        c.flat_fees,
        Fees {
            lp_bps: 25,
            protocol_bps: 5,
            creator_bps: 0
        }
    );
    // First tier (0 SOL mcap): 2 + 93 + 30 = 125 bp; second from 420 SOL: 20 + 5 + 95.
    assert_eq!(
        c.fee_tiers[0].fees,
        Fees {
            lp_bps: 2,
            protocol_bps: 93,
            creator_bps: 30
        }
    );
    assert_eq!(c.fee_tiers[1].threshold, 420_000_000_000);
    assert_eq!(
        c.fee_tiers[1].fees,
        Fees {
            lp_bps: 20,
            protocol_bps: 5,
            creator_bps: 95
        }
    );
    assert_eq!(
        tier_for(&c.fee_tiers, 419_999_999_999),
        Some(c.fee_tiers[0].fees)
    );
    assert_eq!(
        tier_for(&c.fee_tiers, 420_000_000_000),
        Some(c.fee_tiers[1].fees)
    );
    // Truncated / wrong-discriminator accounts refuse.
    let f: Value =
        serde_json::from_str(include_str!("fixtures/pumpswap_fee_config_mainnet.json")).unwrap();
    let raw = base64_decode(f["data_base64"].as_str().unwrap());
    assert!(decode_fee_config(&raw[..200]).is_none());
    let mut bad = raw.clone();
    bad[0] ^= 1;
    assert!(decode_fee_config(&bad).is_none());
}

#[test]
fn untouched_sells_net_proceeds_from_the_pre_trade_schedule_are_exact() {
    let c = cfg();
    let mut ok = 0;
    for v in vectors().iter().filter(|v| v["kind"] == "sell") {
        let f = fees_for(&c, &inputs(v)).unwrap();
        let q = sell_net_quote(u(v, "pb"), u(v, "pq"), u(v, "V"), u(v, "bin"), f).unwrap();
        assert_eq!(q.gross, u(v, "qout"), "gross {}", v["sig"]);
        if v["group"] == "untouched" {
            assert_eq!(q.net, u(v, "uq"), "net {}", v["sig"]);
            assert_eq!(
                (q.lp_fee, q.protocol_fee, q.creator_fee),
                (u(v, "lpf"), u(v, "prf"), u(v, "crf")),
                "fees {}",
                v["sig"]
            );
            ok += 1;
        }
    }
    assert_eq!(ok, 7, "untouched sells");
}

#[test]
fn untouched_exact_in_buys_from_the_pre_trade_schedule() {
    // `buy_exact_quote_in` remains an EMPIRICALLY INFERRED rule (see pumpswap_event); what is new here is
    // that its fee rates now come from FeeConfig, not from the trade.
    let c = cfg();
    let (mut ok, mut wrong_rate) = (0, Vec::new());
    for v in vectors()
        .iter()
        .filter(|v| v["ix"] == "buy_exact_quote_in" && v["group"] == "untouched")
    {
        let f = fees_for(&c, &inputs(v)).unwrap();
        let got = buy_exact_quote_in(
            u(v, "pb"),
            u(v, "pq"),
            u(v, "V"),
            u(v, "X"),
            u128::from(f.lp_bps),
            u128::from(f.protocol_bps),
            u128::from(f.creator_bps),
        )
        .unwrap();
        assert_eq!(got.base_out, u(v, "out"), "{}", v["sig"]);
        if (f.lp_bps, f.protocol_bps, f.creator_bps)
            != (
                v["lp"].as_u64().unwrap(),
                v["pr"].as_u64().unwrap(),
                v["cr"].as_u64().unwrap(),
            )
        {
            wrong_rate.push(v["sig"].as_str().unwrap().to_string());
        }
        ok += 1;
    }
    assert_eq!(ok, 7);
    // 59R2N3EL8tv4: the event REPORTS creator 0 bp, but the output is reproduced exactly only with the
    // schedule's 95 bp creator rate (fee identity in the event fails; see validation ledger). Named, not hidden.
    assert_eq!(wrong_rate, vec!["59R2N3EL8tv4".to_string()]);
}

#[test]
fn unsupported_pool_shapes_refuse_instead_of_pricing() {
    let c = cfg();
    let v = &vectors()[0];
    let base = inputs(v);
    for (i, want) in [
        (
            PoolFeeInputs {
                is_canonical_pump_pool: false,
                ..base
            },
            FeeRefusal::NonCanonicalPool,
        ),
        (
            PoolFeeInputs {
                quote_is_sol_like: false,
                ..base
            },
            FeeRefusal::NonSolQuote,
        ),
        (
            PoolFeeInputs {
                is_mayhem_mode: true,
                ..base
            },
            FeeRefusal::MayhemMode,
        ),
        (
            PoolFeeInputs {
                pool_creator_fee_bps: 7,
                ..base
            },
            FeeRefusal::PoolCreatorFeeConfigured,
        ),
        (
            PoolFeeInputs {
                base_reserve: 0,
                ..base
            },
            FeeRefusal::ZeroBaseReserve,
        ),
    ] {
        assert_eq!(fees_for(&c, &i), Err(want));
    }
    let f = fees_for(
        &c,
        &PoolFeeInputs {
            coin_creator_set: false,
            ..base
        },
    )
    .unwrap();
    assert_eq!(f.creator_bps, 0);
    // A sell the real vault cannot cover refuses.
    assert!(sell_net_quote(1_000, 10, 1_000_000, 1_000_000, f).is_none());
}
