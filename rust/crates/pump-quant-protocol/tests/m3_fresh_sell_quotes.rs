//! M3 item 1: size-specific SELL quotes for the bF2 held population from a FRESH read-only RPC
//! capture (`fixtures/m3_fresh_sell_state.json`, getMultipleAccounts at one context slot). This is a
//! CURRENT quote, not a replay valuation: it cannot price the bF2 cutoff.
//!
//! Validation is against INDEPENDENT transactions (not ours, not the held positions' own fills):
//! the expected fee amounts come from each tx's emitted event; the RATES come only from the fresh
//! FeeConfig account. Pre-trade reserves are reconstructed from the event's post-trade fields.

use pump_quant_protocol::curve_sell_quote::{
    curve_sell_quote, decode_curve_sell_state, max_sellable_tokens, CurveSellRefusal,
    CurveSellState,
};
use pump_quant_protocol::pda::find_program_address;
use pump_quant_protocol::pumpswap::{decode_pool_account, decode_spl_token_amount};
use pump_quant_protocol::pumpswap_fees::{
    decode_fee_config, fees_for, sell_net_quote, Fees, PoolFeeInputs,
};
use pump_quant_protocol::venue_accounts::{PUMPSWAP_PROGRAM_ID, PUMP_PROGRAM_ID};
use serde_json::Value;

fn fx() -> Value {
    serde_json::from_str(include_str!("fixtures/m3_fresh_sell_state.json")).unwrap()
}
fn b64(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = (acc << 6) | T.iter().position(|t| *t == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}
fn b58(s: &str) -> [u8; 32] {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut n = [0u8; 32];
    for c in s.bytes() {
        let mut carry = A.iter().position(|a| *a == c).unwrap() as u32;
        for b in n.iter_mut().rev() {
            carry += u32::from(*b) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        assert_eq!(carry, 0);
    }
    n
}
fn acct(f: &Value, k: &str) -> Vec<u8> {
    b64(f["accounts"][k]["data_base64"].as_str().unwrap())
}
fn n(v: &Value, k: &str) -> u128 {
    v[k].as_str()
        .map(|s| s.parse().unwrap())
        .or_else(|| v[k].as_u64().map(u128::from))
        .unwrap_or_else(|| panic!("{k}"))
}
fn inv(f: &Value, k: &str) -> u64 {
    f["inventory"][k].as_u64().unwrap()
}

#[test]
fn fresh_fee_configs_decode_and_select_the_first_tier() {
    let f = fx();
    // Curve FeeConfig (["fee_config", pump] under pfee): one tier, 0 / 95 / 30.
    let c = decode_fee_config(&acct(&f, "curve_fee_config")).expect("curve FeeConfig");
    assert_eq!(c.fee_tiers.len(), 1);
    assert_eq!(
        c.fee_tiers[0].fees,
        Fees {
            lp_bps: 0,
            protocol_bps: 95,
            creator_bps: 30
        }
    );
    // AMM FeeConfig: first tier 2 / 93 / 30 below 420 SOL mcap (same as the sealed 445186127 snapshot).
    let a = decode_fee_config(&acct(&f, "amm_fee_config")).expect("amm FeeConfig");
    assert_eq!(
        a.fee_tiers[0].fees,
        Fees {
            lp_bps: 2,
            protocol_bps: 93,
            creator_bps: 30
        }
    );
    assert_eq!(a.fee_tiers[1].threshold, 420_000_000_000);
}

#[test]
fn independent_curve_sells_are_exact_from_the_fee_config_rates() {
    let f = fx();
    let cfg = decode_fee_config(&acct(&f, "curve_fee_config")).unwrap();
    let mut exact = 0;
    for v in f["curve_vectors"].as_array().unwrap() {
        if v["mayhem"].as_bool().unwrap() {
            continue; // mayhem curves are refused by the quote; not a validation input
        }
        let s = CurveSellState {
            virtual_sol: n(v, "vsol_pre") as u64,
            virtual_token: n(v, "vtok_pre") as u64,
            real_sol: u64::MAX, // reconstructed vectors carry no real reserve; the tx landed
            complete: false,
            creator_set: v["creator_set"].as_bool().unwrap(),
            mayhem: false,
            cashback: false,
            quote_is_sol: true,
            creator_fee_bps: 0,
            holder_reward: false,
        };
        let q = curve_sell_quote(&s, &cfg, n(v, "tokens") as u64).unwrap();
        let sig = v["sig"].as_str().unwrap();
        assert_eq!(q.gross, n(v, "sol_out"), "gross {sig}");
        assert_eq!(q.protocol_fee, n(v, "fee"), "protocol {sig}");
        assert_eq!(q.creator_fee, n(v, "creator_fee"), "creator {sig}");
        assert_eq!(
            q.net,
            n(v, "sol_out") - n(v, "fee") - n(v, "creator_fee"),
            "{sig}"
        );
        exact += 1;
    }
    assert_eq!(exact, 3);
}

fn pool_inputs(f: &Value) -> (PoolFeeInputs, u128, u128, u128) {
    let p = decode_pool_account(&acct(f, "pool")).unwrap();
    let mint = b58(f["accounts"]["grad_mint"]["address"].as_str().unwrap());
    assert_eq!(p.base_mint, mint);
    let (auth, _) = find_program_address(&[b"pool-authority", &mint], &PUMP_PROGRAM_ID).unwrap();
    let wsol = b58("So11111111111111111111111111111111111111112");
    let (canon, _) = find_program_address(
        &[b"pool", &0u16.to_le_bytes(), &auth, &mint, &wsol],
        &PUMPSWAP_PROGRAM_ID,
    )
    .unwrap();
    assert_eq!(
        canon,
        b58(f["accounts"]["pool"]["address"].as_str().unwrap()),
        "canonical pool binding"
    );
    let raw = acct(f, "pool");
    let vq = i128::from_le_bytes(raw[245..261].try_into().unwrap());
    let pool_creator_fee_bps = u64::from_le_bytes(raw[261..269].try_into().unwrap());
    let base = decode_spl_token_amount(&acct(f, "pool_base_vault")).unwrap();
    let quote = decode_spl_token_amount(&acct(f, "pool_quote_vault")).unwrap();
    let mint_acct = acct(f, "grad_mint");
    let supply = u64::from_le_bytes(mint_acct[36..44].try_into().unwrap());
    let inputs = PoolFeeInputs {
        is_canonical_pump_pool: p.creator == auth,
        quote_is_sol_like: p.quote_mint == wsol,
        is_mayhem_mode: p.is_mayhem_mode.unwrap(),
        pool_creator_fee_bps,
        coin_creator_set: p.coin_creator.is_some_and(|c| c != [0u8; 32]),
        base_mint_supply: u128::from(supply),
        base_reserve: u128::from(base),
        effective_quote_reserve: u128::from(quote) + u128::try_from(vq).unwrap(),
    };
    (
        inputs,
        u128::from(base),
        u128::from(quote),
        u128::try_from(vq).unwrap(),
    )
}

#[test]
fn independent_amm_sells_on_the_graduated_pool_are_net_exact() {
    let f = fx();
    let cfg = decode_fee_config(&acct(&f, "amm_fee_config")).unwrap();
    let (cur, _, _, _) = pool_inputs(&f);
    let mut exact = 0;
    for v in f["amm_vectors"].as_array().unwrap() {
        let i = PoolFeeInputs {
            base_reserve: n(v, "pb"),
            effective_quote_reserve: n(v, "pq") + n(v, "vq"),
            ..cur
        };
        let fees = fees_for(&cfg, &i).unwrap();
        let q = sell_net_quote(n(v, "pb"), n(v, "pq"), n(v, "vq"), n(v, "base_in"), fees).unwrap();
        let sig = v["sig"].as_str().unwrap();
        assert_eq!(q.gross, n(v, "gross"), "gross {sig}");
        assert_eq!(
            (q.lp_fee, q.protocol_fee),
            (n(v, "lp"), n(v, "protocol")),
            "{sig}"
        );
        // Cashback coin: the 30 bp creator share is emitted as `cashback`, withheld from user_out.
        assert_eq!(n(v, "creator"), 0);
        assert_eq!(q.creator_fee, n(v, "cashback"), "creator share {sig}");
        assert_eq!(q.net, n(v, "user_out"), "net {sig}");
        exact += 1;
    }
    assert_eq!(exact, 3);
}

#[test]
fn held_population_quotes_at_the_capture_slot() {
    let f = fx();
    // 3ba1f2 (51nHMc): curve complete -> AMM canonical pool, full inventory.
    let cs = decode_curve_sell_state(&acct(&f, "curve_3ba1f2")).unwrap();
    let ccfg = decode_fee_config(&acct(&f, "curve_fee_config")).unwrap();
    assert_eq!(
        curve_sell_quote(&cs, &ccfg, inv(&f, "3ba1f2")),
        Err(CurveSellRefusal::CurveComplete)
    );
    let acfg = decode_fee_config(&acct(&f, "amm_fee_config")).unwrap();
    let (i, base, quote, vq) = pool_inputs(&f);
    let fees = fees_for(&acfg, &i).unwrap();
    let q = sell_net_quote(base, quote, vq, u128::from(inv(&f, "3ba1f2")), fees).unwrap();
    assert_eq!(
        (q.gross, q.lp_fee, q.protocol_fee, q.creator_fee, q.net),
        (45_404_041, 9_081, 422_258, 136_213, 44_836_489)
    );
    // 74721d / 820af1: live curves drained to dust real SOL; full inventory refused, max size named.
    for (k, max) in [("74721d", 23_412_456_533u64), ("820af1", 1_108_766)] {
        let s = decode_curve_sell_state(&acct(&f, &format!("curve_{k}"))).unwrap();
        assert!(!s.complete && !s.mayhem && s.quote_is_sol && s.creator_fee_bps == 0);
        assert_eq!(
            curve_sell_quote(&s, &ccfg, inv(&f, k)),
            Err(CurveSellRefusal::RealSolInsufficient { max_tokens: max }),
            "{k}"
        );
        assert_eq!(max_sellable_tokens(&s), Some(max));
        let at_max = curve_sell_quote(&s, &ccfg, max).unwrap();
        assert!(at_max.gross <= u128::from(s.real_sol));
        assert!(curve_sell_quote(&s, &ccfg, max + 1).is_err());
    }
}
