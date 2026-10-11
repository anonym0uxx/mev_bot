//! WIRE leg of the supported-layout binding: `decode::curve_mode_observed` on REAL captured accounts emits
//! `mayhem:false` ONLY for the tested population (non-Mayhem, SOL quote, 151 bytes). Every Mayhem variant is
//! `mayhem:true`; an unsupported layout or a non-SOL-quoted non-Mayhem account emits NOTHING (mode stays UNKNOWN).
use pump_quant_app::event::AppEvent;
use pump_quant_junction::decode::curve_mode_observed;

include!("fixtures/offset_mode_accounts.rs");

const MB: [u8; 32] = [9u8; 32];

fn mayhem_of(h: &[u8]) -> Option<bool> {
    curve_mode_observed(&MB, h, 1).map(|pe| match pe.event {
        AppEvent::CurveModeObserved { mayhem, .. } => mayhem,
        _ => panic!("wrong kind"),
    })
}

#[test]
fn ordinary_accounts_are_the_only_mayhem_false() {
    for h in [CANON_60, CASHBACK_130, CANON_TOK2118_99943] {
        assert_eq!(mayhem_of(&hex_bytes(h)), Some(false));
    }
    for h in [
        MAYHEM_CB_5158,
        MAYHEM_NONSOL_50841,
        MAYHEM_CB_NONSOL_719,
        MAYHEM_HI_640,
        MAYHEM_COINCIDE_130489,
        FX804_32067,
        FX804_32626,
    ] {
        assert_eq!(mayhem_of(&hex_bytes(h)), Some(true));
    }
}

#[test]
fn unsupported_layout_or_unobserved_quote_emits_nothing() {
    let b = hex_bytes(CANON_60);
    // Any length other than the observed 151 B, including ones long enough to hold bytes 81/82.
    assert_eq!(mayhem_of(&b[..115]), None);
    assert_eq!(mayhem_of(&b[..150]), None);
    let mut long = b.clone();
    long.push(0);
    assert_eq!(mayhem_of(&long), None);
    // Non-Mayhem with a non-SOL quote: never observed, so not read as ordinary.
    let mut q = b;
    q[90] = 7;
    assert_eq!(mayhem_of(&q), None);
}
