#![allow(unused_imports)]
use pump_quant_protocol::curve::*;
use pump_quant_protocol::decode::PumpCurve;

/// Independent reference implementation of the legacy `simulateBuy` output,
/// computed with `u128` so the test cannot merely echo the crate's answer.
fn expected(v_sol: u64, v_token: u64, sol_in: u64) -> Option<u64> {
    let v_sol = v_sol as u128;
    let v_token = v_token as u128;
    let sol_in = sol_in as u128;
    let fee = sol_in * 100 / 10_000;
    let net = sol_in - fee;
    let k = v_sol * v_token;
    let new_v_sol = v_sol + net;
    if new_v_sol == 0 {
        return None;
    }
    let new_v_token = k / new_v_sol;
    u64::try_from(v_token - new_v_token).ok()
}

fn curve(v_sol: u64, v_token: u64) -> PumpCurve {
    PumpCurve {
        virtual_sol: v_sol,
        virtual_token: v_token,
        real_sol: 0,
        real_token: 0,
        complete: false,
    }
}

#[test]
fn matches_independent_reference_across_inputs() {
    // Realistic pump.fun launch reserves plus assorted buy sizes.
    let cases = [
        (30_000_000_000u64, 1_072_000_000_000_000u64, 100_000_000u64),
        (30_000_000_000, 1_072_000_000_000_000, 1_000_000_000),
        (30_000_000_000, 1_072_000_000_000_000, 5_000_000_000),
        (45_500_000_000, 800_000_000_000_000, 250_000_000),
        (1_000_000_000, 1_000_000_000_000, 1),
        (1_000_000_000, 1_000_000_000_000, 999_999),
    ];
    for (v_sol, v_token, sol_in) in cases {
        let got = pump_amount_out(&curve(v_sol, v_token), sol_in);
        assert_eq!(
            got,
            expected(v_sol, v_token, sol_in),
            "mismatch for v_sol={v_sol} v_token={v_token} sol_in={sol_in}"
        );
    }
}

#[test]
fn fee_is_one_percent_worth_of_input() {
    // With a 1% fee, spending 1_000_000_000 lamports applies fee = 10_000_000,
    // so the effective input is 990_000_000. Compute tokens out directly.
    let v_sol = 30_000_000_000u128;
    let v_token = 1_072_000_000_000_000u128;
    let net = 990_000_000u128;
    let k = v_sol * v_token;
    let new_v_sol = v_sol + net;
    let want = u64::try_from(v_token - k / new_v_sol).unwrap();
    let got =
        pump_amount_out(&curve(30_000_000_000, 1_072_000_000_000_000), 1_000_000_000).unwrap();
    assert_eq!(got, want);
}

#[test]
fn zero_input_yields_zero_tokens() {
    let got = pump_amount_out(&curve(30_000_000_000, 1_072_000_000_000_000), 0).unwrap();
    assert_eq!(got, 0);
}

#[test]
fn empty_curve_does_not_panic() {
    // vSol == 0 and sol_in == 0 => new_v_sol == 0 => None, not a panic.
    assert_eq!(pump_amount_out(&curve(0, 0), 0), None);
}

#[test]
fn monotonic_in_input_size() {
    // Larger buys must never return fewer tokens on the same curve.
    let c = curve(30_000_000_000, 1_072_000_000_000_000);
    let a = pump_amount_out(&c, 500_000_000).unwrap();
    let b = pump_amount_out(&c, 1_000_000_000).unwrap();
    assert!(b > a, "expected {b} > {a}");
}
