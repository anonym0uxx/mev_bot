//! Tests for integer money / fixed-point rate arithmetic. Expectations are
//! computed independently (primitive `u64`/`u128`/`i128` reference math) across
//! multiple inputs including overflow/underflow edge cases.

use pump_quant_domain::money::{BasisPoints, Lamports, SignedLamports, TokenAmount};

#[test]
fn lamports_checked_add_matches_primitive_reference() {
    let cases: [(u64, u64); 6] = [
        (0, 0),
        (1, 2),
        (1_000_000_000, 500_000_000),
        (u64::MAX, 0),
        (u64::MAX, 1),      // overflow
        (u64::MAX - 3, 10), // overflow
    ];
    for (a, b) in cases {
        let expected = a.checked_add(b).map(Lamports);
        assert_eq!(
            Lamports(a).checked_add(Lamports(b)),
            expected,
            "add {a}+{b}"
        );
        // Saturating reference.
        assert_eq!(
            Lamports(a).saturating_add(Lamports(b)),
            Lamports(a.saturating_add(b))
        );
    }
}

#[test]
fn lamports_checked_sub_matches_primitive_reference() {
    let cases: [(u64, u64); 6] = [
        (0, 0),
        (5, 3),
        (3, 5), // underflow
        (u64::MAX, 1),
        (0, 1), // underflow
        (1_000, 1_000),
    ];
    for (a, b) in cases {
        let expected = a.checked_sub(b).map(Lamports);
        assert_eq!(
            Lamports(a).checked_sub(Lamports(b)),
            expected,
            "sub {a}-{b}"
        );
        assert_eq!(
            Lamports(a).saturating_sub(Lamports(b)),
            Lamports(a.saturating_sub(b))
        );
    }
}

#[test]
fn lamports_checked_mul_scalar() {
    let cases: [(u64, u64); 5] = [
        (0, 100),
        (7, 6),
        (1_000_000, 1_000),
        (u64::MAX, 1),
        (u64::MAX, 2), // overflow
    ];
    for (a, s) in cases {
        assert_eq!(
            Lamports(a).checked_mul(s),
            a.checked_mul(s).map(Lamports),
            "mul {a}*{s}"
        );
    }
}

#[test]
fn lamports_apply_bps_independent_reference() {
    // Independent reference: floor(value * bps / 10_000) via u128.
    fn reference(value: u64, bps: u32) -> u128 {
        (value as u128) * (bps as u128) / 10_000u128
    }
    let value_cases = [0u64, 1, 9_999, 10_000, 1_000_000_000, u64::MAX];
    let bps_cases = [0u32, 1, 25, 100, 500, 10_000, 30_000, u32::MAX];

    for &v in &value_cases {
        for &bps in &bps_cases {
            let want = reference(v, bps);
            let want_sat = if want > u64::MAX as u128 {
                Lamports::MAX
            } else {
                Lamports(want as u64)
            };
            assert_eq!(
                Lamports(v).apply_bps_saturating(BasisPoints(bps)),
                want_sat,
                "apply_bps_saturating {v} @ {bps}bp"
            );
            let want_checked = if want > u64::MAX as u128 {
                None
            } else {
                Some(Lamports(want as u64))
            };
            assert_eq!(
                Lamports(v).checked_apply_bps(BasisPoints(bps)),
                want_checked,
                "checked_apply_bps {v} @ {bps}bp"
            );
        }
    }
}

#[test]
fn lamports_apply_bps_known_vectors() {
    // 1 SOL, 5% fee (500bp) = 0.05 SOL = 50_000_000 lamports. Hand-computed.
    assert_eq!(
        Lamports(1_000_000_000).apply_bps_saturating(BasisPoints(500)),
        Lamports(50_000_000)
    );
    // 100% of any value is itself.
    assert_eq!(
        Lamports(123_456).apply_bps_saturating(BasisPoints::FULL),
        Lamports(123_456)
    );
    // Truncation: 3 * 3333bp = 9999/10000 -> floor 0.
    assert_eq!(
        Lamports(3).apply_bps_saturating(BasisPoints(3333)),
        Lamports(0)
    );
    // 3× move = 30_000 bp on 10 lamports = 30.
    assert_eq!(
        Lamports(10).apply_bps_saturating(BasisPoints(30_000)),
        Lamports(30)
    );
}

#[test]
fn lamports_signed_diff_is_exact() {
    let cases: [(u64, u64); 5] = [(0, 0), (10, 3), (3, 10), (u64::MAX, 0), (0, u64::MAX)];
    for (a, b) in cases {
        let expected = SignedLamports(a as i128 - b as i128);
        assert_eq!(Lamports(a).signed_diff(Lamports(b)), expected, "{a}-{b}");
    }
    // Concrete: 3 - 10 = -7.
    assert_eq!(Lamports(3).signed_diff(Lamports(10)), SignedLamports(-7));
    assert!(Lamports(3).signed_diff(Lamports(10)).is_loss());
    assert!(!Lamports(10).signed_diff(Lamports(3)).is_loss());
}

#[test]
fn signed_lamports_aggregation_and_magnitude() {
    // Sum a mix of gains and losses; independent i128 reference.
    let deltas = [
        SignedLamports(1_000),
        SignedLamports(-250),
        SignedLamports(-2_000),
        SignedLamports(5_000),
        SignedLamports(-100),
    ];
    let mut acc = SignedLamports::ZERO;
    let mut reference: i128 = 0;
    for d in deltas {
        acc = acc.checked_add(d).expect("no overflow at this scale");
        reference += d.0;
    }
    assert_eq!(acc, SignedLamports(reference));
    assert_eq!(reference, 3_650); // hand-summed
    assert!(!acc.is_loss());

    // magnitude of a negative delta.
    assert_eq!(SignedLamports(-42).magnitude_saturating(), Lamports(42));
    // magnitude saturates when exceeding u64.
    let big = SignedLamports(-(i128::from(u64::MAX) + 5));
    assert_eq!(big.magnitude_saturating(), Lamports::MAX);
    // saturating_neg handles i128::MIN.
    assert_eq!(
        SignedLamports(i128::MIN).saturating_neg(),
        SignedLamports(i128::MAX)
    );
    assert_eq!(SignedLamports(-5).saturating_neg(), SignedLamports(5));
}

#[test]
fn signed_lamports_checked_add_overflow() {
    assert_eq!(
        SignedLamports(i128::MAX).checked_add(SignedLamports(1)),
        None
    );
    assert_eq!(
        SignedLamports(i128::MAX).saturating_add(SignedLamports(1)),
        SignedLamports(i128::MAX)
    );
}

#[test]
fn basis_points_construction_and_addition() {
    // from_percent: 5% -> 500bp, 100% -> 10_000bp.
    assert_eq!(BasisPoints::from_percent(5), BasisPoints(500));
    assert_eq!(BasisPoints::from_percent(100), BasisPoints::FULL);
    assert_eq!(BasisPoints::FULL.0, BasisPoints::ONE_HUNDRED_PERCENT);
    // Summing fee components: 30bp + 100bp + 25bp = 155bp.
    let total = BasisPoints(30)
        .checked_add(BasisPoints(100))
        .and_then(|x| x.checked_add(BasisPoints(25)))
        .unwrap();
    assert_eq!(total, BasisPoints(155));
    // Overflow is checked.
    assert_eq!(BasisPoints(u32::MAX).checked_add(BasisPoints(1)), None);
    assert_eq!(
        BasisPoints(u32::MAX).saturating_add(BasisPoints(1)),
        BasisPoints(u32::MAX)
    );
    // from_percent saturates rather than wrapping.
    assert_eq!(BasisPoints::from_percent(u32::MAX), BasisPoints(u32::MAX));
}

#[test]
fn token_amount_arithmetic() {
    assert_eq!(
        TokenAmount(5).checked_add(TokenAmount(7)),
        Some(TokenAmount(12))
    );
    assert_eq!(TokenAmount(u64::MAX).checked_add(TokenAmount(1)), None);
    assert_eq!(
        TokenAmount(3).saturating_sub(TokenAmount(10)),
        TokenAmount::ZERO
    );
    assert_eq!(
        TokenAmount(10).saturating_sub(TokenAmount(3)),
        TokenAmount(7)
    );
    assert!(TokenAmount(1) < TokenAmount(2));
}
