//! Python-compatible number formatting.
//!
//! The corpus was rendered by Python, so every number in the live proposal has to be
//! formatted the way Python formats it, not the way Rust does by default. Three
//! differences matter and each one is a silent out-of-distribution risk:
//!
//! * Python `str(float)` keeps a trailing `.0` on whole numbers; Rust's `{}` drops it.
//! * Python `%.Ng` is C's `%g` (significant digits, `e`-notation outside `[-4, N)`,
//!   exponent padded to two digits); Rust has no `%g` at all.
//! * Python `round()` is half-to-even; Rust's `f64::round()` is half-away-from-zero.
//!
//! Everything here is pure and branch-predictable: no allocation in the hot path except
//! the output string itself.

#![forbid(unsafe_code)]

/// Render an `f64` exactly as Python's `str(float)` would: the shortest representation
/// that round-trips, with a trailing `.0` kept on whole numbers (`1.0`, not `1`).
pub fn py_float(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if v == 0.0 {
        // Python keeps the sign of a negative zero.
        return if v.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    // Python switches to scientific notation outside [-4, 16); Rust's Display never does.
    let e = format!("{:e}", v);
    let (mant, exp) = e.split_once('e').expect("LowerExp carries an exponent");
    let x: i32 = exp.parse().expect("exponent is an integer");
    if x < -4 || x >= 16 {
        return format!("{}e{}{:02}", mant, if x < 0 { '-' } else { '+' }, x.abs());
    }
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Render an `f64` exactly as Python's `%.*f` would (fixed point, `dp` decimals).
pub fn py_fixed(v: f64, dp: usize) -> String {
    format!("{v:.dp$}")
}

/// Render an `f64` exactly as Python's `%.*g` would (C `%g` with `p` significant
/// digits, trailing zeros stripped, exponent `e±dd`).
pub fn py_g(v: f64, p: usize) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v < 0.0 {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }
    let p = p.max(1);
    let e = format!("{v:.*e}", p - 1);
    let (mant, exp) = e.split_once('e').expect("`{:e}` always emits an exponent");
    let x: i32 = exp.parse().expect("`{:e}` exponent is an integer");
    if x < -4 || x >= p as i32 {
        let mut m = mant.to_string();
        strip_fraction_zeros(&mut m);
        format!("{m}e{}{:02}", if x < 0 { '-' } else { '+' }, x.abs())
    } else {
        let prec = (p as i32 - 1 - x).max(0) as usize;
        let mut s = format!("{v:.prec$}");
        strip_fraction_zeros(&mut s);
        s
    }
}

/// The corpus's `fnum` ladder (`serialize_families.py`), used for the STATE line's return,
/// volatility and share fields.
///
/// WHY NOT `str(float)`. Those fields are DERIVED, so a live value carries full precision.
/// The corpus printed them through this ladder: `e9`/`e6` shorthands at `1e9`/`1e6`, `%.0f`
/// at `1e3`, else `%.6g`. `py_float(509.0)` emits `509.0` where the corpus wrote `509`, and
/// `py_float(304.32986)` emits nine digits where the corpus wrote `304.33` — the same number
/// as a DIFFERENT token sequence, i.e. out-of-distribution input from correct arithmetic.
/// The C5 parity harness caught exactly this on real c12 rows; the P1 fixtures could not,
/// because they round-trip the corpus's already-shortened text (`str(float)` of a value the
/// corpus had already put through `%g` reproduces it byte for byte).
#[must_use]
pub fn py_fnum(x: f64) -> String {
    let a = x.abs();
    if a >= 1e9 {
        let mut s = format!("{:.2}", x / 1e9);
        s.push_str("e9");
        s
    } else if a >= 1e6 {
        let mut s = format!("{:.2}", x / 1e6);
        s.push_str("e6");
        s
    } else if a >= 1e3 {
        format!("{x:.0}")
    } else {
        py_g(x, 6)
    }
}

fn strip_fraction_zeros(s: &mut String) {
    if !s.contains('.') {
        return;
    }
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
}

/// Python's `round(x)` for one argument: nearest integer, ties to even.
pub fn round_half_even(x: f64) -> f64 {
    let f = x.floor();
    let d = x - f;
    if d > 0.5 {
        f + 1.0
    } else if d < 0.5 {
        f
    } else if (f / 2.0).fract() == 0.0 {
        f
    } else {
        f + 1.0
    }
}

/// `round(x, nd)` as Python does it: round the double's EXACT value to `nd` decimals, ties to even.
///
/// WHY THE OBVIOUS IMPLEMENTATION IS WRONG. `(x * 10^nd).round_ties_even() / 10^nd` rounds a
/// product that has ALREADY been rounded to a double. `3.865 * 100` is exactly `386.5`, so that
/// reading sees a tie and goes to even — `3.86` — while Python's `round(3.865, 2)` is `3.87`,
/// because the exact value of the double `3.865` is `3.865000000000000213…`, ABOVE the tie. The
/// corpus renders `last_trade_age_s` and the other decimals this way, so the two disagree on real
/// prompt LINES (the C5 harness caught it on one of twelve rows).
///
/// The fix carries the multiplication's exact residual: `mul_add` gives it (`x·scale − fl(x·scale)`
/// with no rounding left over), and a two-sum comparison of `frac + err` against one half decides
/// the direction exactly. No wider arithmetic is needed, and `err == 0` with `frac == 0.5` is the
/// only true tie.
pub fn round_half_even_nd(x: f64, nd: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let scale = 10f64.powi(nd as i32);
    if nd == 0 || !scale.is_finite() || scale == 0.0 {
        return round_half_even(x);
    }
    let scaled = x * scale;
    if !scaled.is_finite() {
        return x;
    }
    let base = scaled.floor();
    let frac = scaled - base;
    let (sum, tail) = two_sum(frac, x.mul_add(scale, -scaled));
    let up = if sum > 0.5 {
        true
    } else if sum < 0.5 {
        false
    } else if tail != 0.0 {
        tail > 0.0
    } else {
        // An exact tie: half-to-even, on the scaled integer.
        (base / 2.0).fract() != 0.0
    };
    (if up { base + 1.0 } else { base }) / scale
}

/// Knuth's two-sum: `(s, e)` with `s + e == a + b` exactly and `s == fl(a + b)`.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bv = s - a;
    let e = (a - (s - bv)) + (b - bv);
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_even_nd_matches_python_where_the_TIE_IS_AN_ARTEFACT() {
        // Every expectation here is CPython's own `round(x, nd)` output. The first three are the
        // values that separate the correct implementation from the scaled one: `3.865 * 100` is
        // exactly 386.5 (looks like a tie, is not — the double is above it), `2.675` and `1.005`
        // land just below theirs.
        assert_eq!(round_half_even_nd(3.865, 2), 3.87);
        assert_eq!(round_half_even_nd(2.675, 2), 2.67);
        assert_eq!(round_half_even_nd(1.005, 2), 1.0);
        assert_eq!(round_half_even_nd(-3.865, 2), -3.87);
        // Exact ties go to even.
        assert_eq!(round_half_even_nd(0.125, 2), 0.12);
        assert_eq!(round_half_even_nd(0.135, 2), 0.14);
        assert_eq!(round_half_even_nd(2.5, 0), 2.0);
        assert_eq!(round_half_even_nd(3.5, 0), 4.0);
        assert_eq!(round_half_even_nd(0.5, 0), 0.0);
        assert_eq!(round_half_even_nd(2.675, 0), 3.0);
        // Non-ties, and the values the corpus actually stores.
        assert_eq!(round_half_even_nd(3.865, 1), 3.9);
        assert_eq!(round_half_even_nd(0.15, 2), 0.15);
        assert_eq!(round_half_even_nd(664.291, 1), 664.3);
        assert_eq!(round_half_even_nd(3.87, 2), 3.87);
    }

    #[test]
    fn py_float_keeps_the_python_whole_number_dot() {
        assert_eq!(py_float(1.0), "1.0");
        assert_eq!(py_float(0.0), "0.0");
        assert_eq!(py_float(4800.0), "4800.0");
        assert_eq!(py_float(-6.39046), "-6.39046");
    }

    #[test]
    fn py_g_matches_c_percent_g() {
        assert_eq!(py_g(1.0499954004771828, 10), "1.0499954");
        assert_eq!(py_g(1.0499954004771828e-9, 10), "1.0499954e-09");
        assert_eq!(py_g(0.0010499954004771828, 10), "0.0010499954");
        assert_eq!(py_g(1865.3, 4), "1865");
        assert_eq!(py_g(4277.289364175, 6), "4277.29");
        assert_eq!(py_g(0.0245422000546, 12), "0.0245422000546");
        assert_eq!(py_g(2.45422000546e-11, 12), "2.45422000546e-11");
        assert_eq!(py_g(0.0, 4), "0");
        assert_eq!(py_g(100.0, 6), "100");
        assert_eq!(py_g(9.9999999999e-5, 4), "0.0001");
    }

    #[test]
    fn probe_marks() {
        assert_eq!(py_float(7.921156319656042e-5), "7.921156319656042e-05");
        assert_eq!(py_float(1.0e16), "1e+16");
        assert_eq!(py_float(1.0e-5), "1e-05");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(-0.0), "-0.0");
        assert_eq!(py_float(4800.0), "4800.0");
    }

    #[test]
    fn rounding_is_half_to_even() {
        assert_eq!(round_half_even(0.5), 0.0);
        assert_eq!(round_half_even(1.5), 2.0);
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(-0.5), 0.0);
        assert_eq!(round_half_even(-1.5), -2.0);
        assert_eq!(round_half_even(544.5), 544.0);
        assert_eq!(round_half_even(2.8247), 3.0);
    }
}
