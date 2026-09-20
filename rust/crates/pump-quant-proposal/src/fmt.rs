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

/// `round(x, nd)` as Python does it, for the non-negative `nd` this corpus uses.
pub fn round_half_even_nd(x: f64, nd: usize) -> f64 {
    if nd == 0 {
        return round_half_even(x);
    }
    let scale = 10f64.powi(nd as i32);
    round_half_even(x * scale) / scale
}

#[cfg(test)]
mod tests {
    use super::*;

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
