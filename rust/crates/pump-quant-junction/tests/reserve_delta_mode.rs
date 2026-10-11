//! Mode-aware reserve-delta derivation (offset slice, proc/OFFSET_v4_REPORT.md §1) on REAL captured accounts.
//!
//! The virtual-delta derivation assumes `vsol - real_sol` is constant (observed only on Ordinary curves). On a
//! Mayhem curve the virtual offset moves between trades, so a virtual delta overstates (or invents) the print.
//! * Ordinary -> exactly the legacy derivation (control).
//! * Mayhem   -> NO trade; real reserves moved = `ModeUnsupported` (recorded fail-closed as a possible missing
//!   trade); an offset-only reset (real reserves unchanged) = `NoPrint` (NOT a possible missing trade).
//! * Unknown (unsupported layout) -> NO trade; anything moved = `ModeUnsupported`.
use pump_quant_app::decision_join::MissingKind;
use pump_quant_app::event::AppEvent;
use pump_quant_junction::decode::decode_onchain_confirm_with_curve;
use pump_quant_junction::reserve_delta::{
    classify_delta_miss_for_mode, derive_market_trade_from_delta,
    derive_market_trade_from_delta_for_mode, DeltaMiss, DeltaMode, ReserveSnapshot,
};
use pump_quant_protocol::decode::PumpCurve;

include!("fixtures/offset_mode_accounts.rs");

const MB: [u8; 32] = [9u8; 32];

fn curve_of(bytes: &[u8]) -> PumpCurve {
    decode_onchain_confirm_with_curve(&MB, bytes, 1)
        .expect("real account decodes")
        .1
}

fn quote_of(pe: &pump_quant_junction::ProvenancedEvent) -> u64 {
    match pe.event {
        AppEvent::MarketTrade { quote_lamports, .. } => quote_lamports,
        _ => panic!("not a trade"),
    }
}

#[test]
fn mode_of_real_accounts_follows_the_verified_classifier() {
    assert_eq!(
        DeltaMode::of_account(&hex_bytes(CANON_60)),
        DeltaMode::Ordinary
    );
    assert_eq!(
        DeltaMode::of_account(&hex_bytes(CASHBACK_130)),
        DeltaMode::Ordinary
    );
    for h in [
        FX804_32067,
        FX804_32626,
        MAYHEM_HI_640,
        MAYHEM_COINCIDE_130489,
    ] {
        assert_eq!(DeltaMode::of_account(&hex_bytes(h)), DeltaMode::Mayhem);
    }
    assert_eq!(
        DeltaMode::of_account(&hex_bytes(CANON_60)[..115]),
        DeltaMode::Unknown
    );
}

/// 804a86fe: the two REAL states either side of the Mayhem reset (lines 32067 -> 32626). Real SOL moved
/// 179,532,847 -> 134,865,076 (a 44.7 mSOL sell) but virtual SOL moved 2.37 SOL. The legacy derivation
/// emits a 2.37 SOL "trade" -- a 53x overstatement. The mode-aware path emits none and names it.
#[test]
fn the_804a86fe_mayhem_pair_derives_no_trade_and_is_named_mode_unsupported() {
    let a = hex_bytes(FX804_32067);
    let b = hex_bytes(FX804_32626);
    let (ca, cb) = (curve_of(&a), curve_of(&b));
    let prev = ReserveSnapshot::of(&ca, 445_637_772);
    let mode = DeltaMode::of_account(&b);
    assert_eq!(mode, DeltaMode::Mayhem);

    // The old mode-blind evidence is kept: the legacy derivation overstates the print.
    let legacy = derive_market_trade_from_delta(&MB, Some(prev), &cb, 445_637_775, true, Some(1))
        .expect("legacy mode-blind derivation emits a trade");
    let real_moved = ca.real_sol - cb.real_sol;
    assert_eq!(real_moved, 44_667_771);
    assert!(
        quote_of(&legacy) > 50 * real_moved,
        "legacy quote {}",
        quote_of(&legacy)
    );

    assert!(derive_market_trade_from_delta_for_mode(
        &MB,
        Some(prev),
        &cb,
        445_637_775,
        true,
        Some(1),
        mode
    )
    .is_none());
    let miss = classify_delta_miss_for_mode(Some(&prev), &cb, 445_637_775, mode);
    assert_eq!(miss, DeltaMiss::ModeUnsupported);
    assert_eq!(miss.missing_kind(), Some(MissingKind::PossibleTrade));
    assert_eq!(mode.refusal(), "delta_unsupported:mayhem_mode");
}

/// An offset-only Mayhem reset (virtual reserves move, real reserves do not): nothing traded, so it is NOT
/// recorded as a possible missing trade. Built from the real Mayhem account with only the virtual SOL moved.
#[test]
fn an_offset_only_mayhem_reset_is_no_print_not_a_possible_trade() {
    let b = hex_bytes(FX804_32626);
    let cb = curve_of(&b);
    let mut before = cb;
    before.virtual_sol += 2_000_000_000;
    let prev = ReserveSnapshot::of(&before, 10);
    let miss = classify_delta_miss_for_mode(Some(&prev), &cb, 11, DeltaMode::Mayhem);
    assert_eq!(miss, DeltaMiss::NoPrint);
    assert_eq!(miss.missing_kind(), None);
    // Measured, not assumed: a PURE offset reset moves only virtual SOL (the token offset is trade-invariant),
    // so the legacy path also sees a zero token delta -> NoPrint. The legacy harm is the reset COMBINED with a
    // trade (the 804a86fe pair above), where the virtual delta is read as the print.
    assert!(derive_market_trade_from_delta(&MB, Some(prev), &cb, 11, true, Some(1)).is_none());
    assert_eq!(
        pump_quant_junction::reserve_delta::classify_delta_miss(Some(&prev), &cb, 11),
        DeltaMiss::NoPrint
    );
    // A reset that ALSO moved virtual tokens (any combined state) is still NoPrint under Mayhem while the real
    // reserves are unchanged; the legacy path would derive/flag it.
    let mut both = before;
    both.virtual_token += 1_000_000_000;
    let prev2 = ReserveSnapshot::of(&both, 10);
    assert_eq!(
        classify_delta_miss_for_mode(Some(&prev2), &cb, 11, DeltaMode::Mayhem),
        DeltaMiss::NoPrint
    );
    assert_ne!(
        pump_quant_junction::reserve_delta::classify_delta_miss(Some(&prev2), &cb, 11),
        DeltaMiss::NoPrint
    );
}

/// Control: an Ordinary curve keeps the legacy derivation exactly (same event, same miss classification).
#[test]
fn ordinary_curves_keep_the_legacy_derivation_exactly() {
    let c1 = curve_of(&hex_bytes(CANON_60));
    let c0 = PumpCurve {
        virtual_sol: c1.virtual_sol - 500_000_000,
        virtual_token: c1.virtual_token + 7_000_000_000_000,
        real_sol: c1.real_sol - 500_000_000,
        real_token: c1.real_token + 7_000_000_000_000,
        complete: false,
    };
    let prev = ReserveSnapshot::of(&c0, 5);
    let legacy = derive_market_trade_from_delta(&MB, Some(prev), &c1, 6, true, Some(7)).unwrap();
    let moded = derive_market_trade_from_delta_for_mode(
        &MB,
        Some(prev),
        &c1,
        6,
        true,
        Some(7),
        DeltaMode::Ordinary,
    )
    .expect("ordinary derives");
    assert_eq!(format!("{:?}", legacy.event), format!("{:?}", moded.event));
    assert_eq!(quote_of(&moded), 500_000_000);
    // Same miss classification as the legacy classifier on a same-sign move.
    let mut odd = c1;
    odd.virtual_token = c0.virtual_token + 1;
    assert_eq!(
        classify_delta_miss_for_mode(Some(&prev), &odd, 6, DeltaMode::Ordinary),
        pump_quant_junction::reserve_delta::classify_delta_miss(Some(&prev), &odd, 6)
    );
}

/// Unknown mode is fail-closed: never derived, a moved curve is named; a unchanged one is `NoPrint`.
#[test]
fn unknown_mode_never_derives_and_fails_closed() {
    let c1 = curve_of(&hex_bytes(CANON_60));
    let mut c0 = c1;
    c0.virtual_sol -= 500_000_000;
    c0.real_sol -= 500_000_000;
    c0.virtual_token += 7_000_000_000_000;
    c0.real_token += 7_000_000_000_000;
    let prev = ReserveSnapshot::of(&c0, 5);
    assert!(derive_market_trade_from_delta_for_mode(
        &MB,
        Some(prev),
        &c1,
        6,
        true,
        Some(7),
        DeltaMode::Unknown
    )
    .is_none());
    assert_eq!(
        classify_delta_miss_for_mode(Some(&prev), &c1, 6, DeltaMode::Unknown),
        DeltaMiss::ModeUnsupported
    );
    let same = ReserveSnapshot::of(&c1, 5);
    assert_eq!(
        classify_delta_miss_for_mode(Some(&same), &c1, 6, DeltaMode::Unknown),
        DeltaMiss::NoPrint
    );
}
