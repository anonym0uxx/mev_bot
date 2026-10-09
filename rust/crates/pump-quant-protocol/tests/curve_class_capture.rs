//! `classify_pump_curve` on REAL captured account bytes: only non-Mayhem, SOL-quoted, 151-byte accounts are
//! `Ordinary` (the population where `vsol - real_sol = 30 SOL` was observed); every other class is refused.
use pump_quant_protocol::decode::{
    classify_pump_curve, decode_pump_curve, CurveClassRefusal, PumpCurveClass,
};

include!("fixtures/offset_mode_accounts.rs");

fn class(h: &str) -> PumpCurveClass {
    classify_pump_curve(&hex_bytes(h))
}

fn offset(h: &str) -> u64 {
    let c = decode_pump_curve(&hex_bytes(h)).unwrap();
    c.virtual_sol - c.real_sol
}

#[test]
fn ordinary_is_exactly_the_tested_non_mayhem_sol_quoted_population() {
    assert_eq!(
        class(CANON_60),
        PumpCurveClass::Ordinary { cashback: false }
    );
    assert_eq!(
        class(CASHBACK_130),
        PumpCurveClass::Ordinary { cashback: true }
    );
    // A canonical curve with the non-standard 211.8e12 token offset is still Ordinary (SOL offset = 30 SOL).
    assert_eq!(
        class(CANON_TOK2118_99943),
        PumpCurveClass::Ordinary { cashback: false }
    );
    for h in [CANON_60, CASHBACK_130, CANON_TOK2118_99943] {
        assert_eq!(offset(h), 30_000_000_000);
    }
}

#[test]
fn every_mayhem_variant_is_mayhem_never_ordinary() {
    let want = [
        (MAYHEM_CB_5158, true, true),
        (MAYHEM_NONSOL_50841, false, false),
        (MAYHEM_CB_NONSOL_719, true, false),
        (MAYHEM_HI_640, false, true),
        (MAYHEM_COINCIDE_130489, false, true),
        (FX804_32067, false, true),
        (FX804_32626, false, true),
    ];
    for (h, cashback, sol_quote) in want {
        let c = class(h);
        assert_eq!(
            c,
            PumpCurveClass::Mayhem {
                cashback,
                sol_quote
            }
        );
        assert!(!c.is_ordinary());
    }
    // The coincidence account satisfies the canonical equation numerically, yet is NOT Ordinary.
    assert_eq!(offset(MAYHEM_COINCIDE_130489), 30_000_000_000);
    // 804a86fe: the offset moved across the reset (9,465,440,184 -> 7,143,521,795).
    assert_eq!(offset(FX804_32067), 9_465_440_184);
    assert_eq!(offset(FX804_32626), 7_143_521_795);
}

#[test]
fn unverified_layouts_flags_and_quotes_are_refused_by_name() {
    let b = hex_bytes(CANON_60);
    // Older 49-byte layout and any other length: not a supported layout (never read as Ordinary).
    for n in [49usize, 82, 83, 114, 115, 150] {
        assert_eq!(
            classify_pump_curve(&b[..n]),
            PumpCurveClass::Unsupported(CurveClassRefusal::UnsupportedLayout),
            "len {n}"
        );
    }
    let mut long = b.clone();
    long.push(0);
    assert_eq!(
        classify_pump_curve(&long),
        PumpCurveClass::Unsupported(CurveClassRefusal::UnsupportedLayout)
    );
    let mut foreign = b.clone();
    foreign[0] ^= 1;
    assert_eq!(
        classify_pump_curve(&foreign),
        PumpCurveClass::Unsupported(CurveClassRefusal::NotPumpCurve)
    );
    for at in [81usize, 82] {
        let mut junk = b.clone();
        junk[at] = 2;
        assert_eq!(
            classify_pump_curve(&junk),
            PumpCurveClass::Unsupported(CurveClassRefusal::NoncanonicalFlag)
        );
    }
    // A non-Mayhem curve with a non-SOL quote: 0 observations -> refused, not Ordinary.
    let mut q = b;
    q[100] = 1;
    assert_eq!(
        classify_pump_curve(&q),
        PumpCurveClass::Unsupported(CurveClassRefusal::QuoteNotObservedSol)
    );
}
